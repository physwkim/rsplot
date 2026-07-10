//! Emit RsDM Rust source from a parsed [`MedmScreen`].
//!
//! This is the analogue of `adl2pydm/output_handler.py`: it walks the widget
//! tree and writes the target display. Where `output_handler` writes PyDM `.ui`
//! XML, this writes a Rust module — a `Screen` struct holding the widgets + an
//! [`Engine`], a `new(cc: &eframe::CreationContext)` builder, and a
//! `ui(&mut self, ui)` draw method that places each widget at its MEDM geometry.
//!
//! Placement is absolute (MEDM screens are absolute `x/y/w/h`) via a small
//! `place` helper that draws each widget in its own `egui::Area` at a fixed
//! position. The Area's `egui::Order` encodes the z-layer, so the user's rule —
//! decoration to the back, controls never occluded or click-stolen — holds by
//! construction: decoration Areas (`Background`) render and receive input below
//! monitors (`Middle`) below controls (`Foreground`). The emitter additionally
//! lays the `place` calls out back-to-front (a stable sort by [`ZLayer`]) so the
//! ordering is also visible in the source.
//!
//! [`Engine`]: https://docs.rs/rsdm
//! [`MedmScreen`]: crate::adl_parser::MedmScreen

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::adl_parser::{Color, Geometry, MedmScreen, MedmWidget, parse_in_dir};
use crate::symbols::{self, ZLayer};

/// Maximum embedded-display nesting depth inlined at code-gen time, a backstop
/// against runaway recursion (cycles are caught separately by [`Builder`]'s
/// `embed_stack`). Beyond it the embedded display falls back to a placeholder.
const MAX_EMBED_DEPTH: usize = 8;

/// Code-generation options (the converter's CLI flags).
#[derive(Clone, Debug)]
pub struct Options {
    /// Channel protocol prefixed onto bare MEDM PV names, e.g. `"ca://"`.
    pub protocol: String,
    /// `$(name)` / `${name}` macro substitutions, baked into every MEDM string
    /// (channels and user-visible text: labels, captions, shell commands,
    /// related-display targets) by `expand_macros`, mirroring MEDM's lexer.
    pub macros: Vec<(String, String)>,
    /// Translate `cartesian plot` as a scatter plot rather than a waveform plot
    /// (mirrors adl2pydm's `--use-scatterplot`).
    pub use_scatterplot: bool,
    /// Directory the source `.adl` lives in, used to resolve an `embedded
    /// display`'s `composite file` so its target can be inlined. `None` (the
    /// default, e.g. converting from stdin or in headless tests) disables
    /// inlining — an embedded display then falls back to a placeholder.
    pub source_dir: Option<PathBuf>,
    /// Emit a responsive layout: scale every widget's MEDM rect proportionally to
    /// fill the available area instead of placing it at fixed absolute pixels.
    /// This is the egui realization of adl2pydm's `grid_layout` (`--use-layout`):
    /// a weighted grid whose stretch factors are the pixel gaps between widget
    /// edges reduces, edge-for-edge, to per-axis proportional reflow — there is no
    /// spanning weighted-grid widget in egui, so the faithful realization places
    /// each widget at its native rect scaled by `available / native` on each axis.
    /// Default `true` (screens reflow with the window); set `false` (CLI
    /// `--absolute`) for fixed absolute MEDM pixels.
    pub use_layout: bool,
    /// Related-display targets converted alongside this screen, keyed by the
    /// target `name` exactly as the emitter sees it (after convert-time macro
    /// baking). Filled by the recursive driver ([`crate::convert`]); when a
    /// target is present a click *opens* its screen in a viewport, when absent
    /// (or for a plain single-file [`generate`]) the click only logs it.
    pub rd_modules: BTreeMap<String, RdModule>,
    /// `true` when this screen is emitted as a child `pub mod __rd_*` inside
    /// the driver's output file: the shared top-level items (`RsdmDisplay`,
    /// `OpenDisplay`, `parse_macro_args`, `next_plot_ids`) are then referenced
    /// through `super::`.
    pub child_module: bool,
}

/// A converted related-display target: the sibling module holding its `Screen`
/// (`None` = the root screen itself, for a cycle back to the root), plus the
/// window title and native size for the child viewport.
#[derive(Clone, Debug)]
pub struct RdModule {
    /// The `pub mod` ident the target's screen lives in; `None` for the root.
    pub ident: Option<String>,
    /// The child window's title (MEDM titles a display with its file name).
    pub title: String,
    /// The target display's native size — the child viewport's inner size.
    pub width: f64,
    pub height: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            protocol: "ca://".to_string(),
            macros: Vec::new(),
            use_scatterplot: false,
            source_dir: None,
            use_layout: true,
            rd_modules: BTreeMap::new(),
            child_module: false,
        }
    }
}

/// The generated source plus any warnings (unsupported widgets, skipped
/// emitters) the caller should surface.
#[derive(Clone, Debug, Default)]
pub struct Generated {
    pub source: String,
    pub warnings: Vec<String>,
    /// Every related-display target `name` seen during emission (after
    /// convert-time macro baking), resolvable or not — the recursive driver's
    /// discovery feed ([`crate::convert`]).
    pub related_targets: Vec<String>,
    /// Whether the screen allocates rsplot `PlotId`s (plots/strip charts), so
    /// the driver knows the output file needs the shared `next_plot_ids`
    /// allocator at its top level.
    pub uses_plot_ids: bool,
}

/// One placed widget: where it goes (`z`, `geom`, a unique Area `id`) and the
/// statement(s) that draw it inside the `place` closure. `gate` is an optional
/// boolean expression: when present, the `place(...)` call is wrapped in `if
/// <gate> { … }` so a MEDM `dynamic attribute` visibility rule can hide it.
struct Placement {
    z: ZLayer,
    id: u64,
    geom: Geometry,
    body: String,
    gate: Option<String>,
}

impl Placement {
    /// A placement with no visibility gate (the common case).
    fn drawn(z: ZLayer, id: u64, geom: Geometry, body: String) -> Self {
        Self {
            z,
            id,
            geom,
            body,
            gate: None,
        }
    }
}

/// Accumulates the pieces of the generated module as the widget tree is walked.
#[derive(Default)]
struct Builder {
    /// `(field_name, field_type)` for each stateful widget (struct + `Self {}`).
    fields: Vec<(String, String)>,
    /// `let <field> = …;` constructor lines for `new()`.
    ctors: Vec<String>,
    /// Absolute placements, drawn back-to-front after a stable sort by `z`.
    placements: Vec<Placement>,
    warnings: Vec<String>,
    /// Running widget index → unique field names and Area ids.
    next_index: u64,
    /// Running plot index → distinct `PlotId`s for GPU plot/image widgets, which
    /// rsplot uses to key their GPU resources (must be unique within a screen).
    next_plot_id: u64,
    /// Running counter for synthetic `loc://` placeholder channels (channel-less
    /// shapes, composite frames, embedded-display frames). Keyed off this rather
    /// than `widget.line` so addresses stay unique across inlined files — two
    /// widgets at the same source line in different `.adl`s must not share a
    /// channel.
    next_synthetic_id: u64,
    /// Whether any emitted code references `Color32` / `rsdm::widgets`.
    needs_color: bool,
    needs_widgets: bool,
    /// Whether a label-less related display / shell command needs its MEDM icon
    /// helper (`related_display_icon` / `shell_command_icon`) appended.
    needs_rd_icon: bool,
    needs_sc_icon: bool,
    /// Whether any emitted code references `rsdm::Channel` (a dynamic visibility
    /// gate field).
    needs_channel: bool,
    /// Whether any emitted string still carries a `$(macro)` reference after the
    /// convert-time `--macro` baking — it then expands at runtime against the
    /// screen instance's macro table (`__m`), so the `MacroTable` helper and the
    /// `__m` field are emitted. Set alongside whichever of the two method flags
    /// below applies (so `needs_macros == needs_macro_expand || needs_macro_args`).
    needs_macros: bool,
    /// Whether the emitted `MacroTable` needs its `expand` method — the child-screen
    /// string path (MEDM `getToken`: an undefined `$(name)` stays literal). Set by
    /// [`medm_str`].
    needs_macro_expand: bool,
    /// Whether the emitted `MacroTable` needs its `expand_args` method — the
    /// related-display `args` path (MEDM `performMacroSubstitutions`: an undefined
    /// `$(name)` is *dropped*). Set by [`rd_click`]. Emitted separately from
    /// `expand` so neither method is dead when a screen uses only one path.
    needs_macro_args: bool,
    /// Whether any emitted ctor needs the wgpu render state (plots), so `new_in`
    /// must unwrap its `render_state` parameter.
    needs_render_state: bool,
    /// The convert-time `--macro` table ([`Options::macros`]); cached here so
    /// `emit_new` can pass it as the root instance's runtime table.
    macros: Vec<(String, String)>,
    /// Converted related-display targets ([`Options::rd_modules`]); cached so
    /// `emit_related_display` can turn a click into an *open* of the sibling
    /// module's screen rather than a log line.
    rd_modules: BTreeMap<String, RdModule>,
    /// Mirrors [`Options::child_module`]: prefix shared top-level items with
    /// `super::` when this screen is emitted as a child `pub mod`.
    child_module: bool,
    /// Whether any related-display click actually opens a converted screen, so
    /// the `__rs`/`__open` fields, the end-of-`ui()` `show_all`, and (at the
    /// top level) the related-display runtime are emitted.
    needs_rd_open: bool,
    /// Every related-display target name seen (the driver's discovery feed).
    related_targets: Vec<String>,
    /// Canonical paths of the `.adl` files currently being inlined (embedded
    /// display recursion), newest last. Guards against include cycles; its length
    /// is the current nesting depth (capped at [`MAX_EMBED_DEPTH`]).
    embed_stack: Vec<PathBuf>,
    /// When `true`, placements scale to fill the available area (the responsive
    /// `--use-layout` mode) rather than using fixed absolute MEDM pixels. Mirrors
    /// [`Options::use_layout`]; cached here so both placement writers (top-level
    /// `emit_ui` and the nested-children path in `emit_frame_container`) can read
    /// it without threading `Options` through every call.
    use_layout: bool,
    /// When `true`, the subtree being emitted is inside a composite-file include
    /// that **replaced** its macro table (a non-empty `;macros` string — MEDM
    /// `compositeFileParse`, `medmComposite.c:659-668`). The parent's macros are
    /// out of scope there, so a `$(name)` that survives the replace table is a
    /// literal dead reference in MEDM (`getToken` passthrough), NOT a runtime
    /// rebind: [`medm_str`] must emit it as a plain literal, not an `__m.expand`
    /// against the parent's runtime table. Set (never cleared) for the duration
    /// of a replace-include subtree, so nested inherit-includes stay sealed too.
    seal_macros: bool,
}

impl Builder {
    /// Allocate the next unique widget index.
    fn index(&mut self) -> u64 {
        let i = self.next_index;
        self.next_index += 1;
        i
    }

    /// Allocate the next distinct `PlotId` for a GPU plot/image widget.
    fn plot_id(&mut self) -> u64 {
        let i = self.next_plot_id;
        self.next_plot_id += 1;
        i
    }

    /// A fresh synthetic `loc://adl2rsdm_<kind>_<n>` placeholder address, unique
    /// across the whole screen (including inlined embedded files). `kind` labels
    /// it (`shape`/`frame`/`embed`); the monotonic `n` guarantees uniqueness even
    /// when two widgets share a source line across different `.adl`s.
    fn synthetic_addr(&mut self, kind: &str) -> String {
        let i = self.next_synthetic_id;
        self.next_synthetic_id += 1;
        format!("loc://adl2rsdm_{kind}_{i}")
    }

    /// The path prefix for the shared top-level items (`RsdmDisplay`,
    /// `OpenDisplay`, `parse_macro_args`, `next_plot_ids`): `super::` inside a
    /// child `pub mod`, empty at the file's top level.
    fn rt_prefix(&self) -> &'static str {
        if self.child_module { "super::" } else { "" }
    }
}

/// Generate the RsDM Rust source for a parsed MEDM screen.
pub fn generate(screen: &MedmScreen, options: &Options) -> Generated {
    // Bake `$(macro)` values into the IR once, before any emitter reads a string:
    // MEDM expands macros for every token at the lexer, so this single pass makes
    // the IR the emitters consume macro-free by construction (channels AND
    // user-visible text). A no-op when no `--macro` was given.
    let mut screen = screen.clone();
    expand_macros(&mut screen.widgets, &options.macros);
    for v in screen.assignments.values_mut() {
        *v = substitute_macros(v, &options.macros);
    }
    let screen = &screen;

    let mut b = Builder {
        use_layout: options.use_layout,
        macros: options.macros.clone(),
        rd_modules: options.rd_modules.clone(),
        child_module: options.child_module,
        ..Default::default()
    };
    for widget in &screen.widgets {
        emit_widget(&mut b, widget, options);
    }
    // An external `cmap` file the parser could not resolve (missing, unparsable,
    // or parsed with no source dir) left `unresolved_cmap` set and fell the table
    // back to MEDM's default 65-colour palette — warn, naming the file, so the
    // colour difference from the real file is visible rather than silent
    // (medmDisplay.c:404-407 "Using the default colormap"). A still-empty table
    // means no colormap at all (degenerately, no display block): every `clr`/`bclr`
    // falls to a rsdm theme default.
    if let Some(file) = &screen.unresolved_cmap {
        b.warnings.push(format!(
            "external colormap file {file:?} could not be read (searched the display's \
             directory and EPICS_DISPLAY_PATH); every fg/bg colour falls to MEDM's default \
             palette instead of the file's colours"
        ));
    } else if screen.color_table.is_empty() {
        b.warnings.push(
            "no color map is defined; every fg/bg colour falls to a rsdm theme default".to_string(),
        );
    }
    // The screen's `bclr` background is painted in `ui()` with `color_expr`, so it
    // needs the `Color32` import even when no widget carries a colour.
    b.needs_color |= screen.background_color.is_some();
    Generated {
        source: assemble(&b, screen),
        warnings: b.warnings,
        related_targets: b.related_targets,
        uses_plot_ids: b.next_plot_id > 0,
    }
}

/// Dispatch one MEDM widget to its emitter. Every MEDM widget symbol has a
/// dedicated emitter; the `_` arm is an unreachable defensive backstop that
/// warns rather than silently dropping a future, not-yet-handled symbol.
fn emit_widget(b: &mut Builder, widget: &MedmWidget, options: &Options) {
    let Some(map) = symbols::lookup(&widget.symbol) else {
        b.warnings.push(format!(
            "line {}: unknown block {:?}",
            widget.line, widget.symbol
        ));
        return;
    };

    let z = map.category.z_layer();
    let start = b.placements.len();
    match widget.symbol.as_str() {
        "text" => emit_static_text(b, widget, options, z),
        "text update" => emit_text_update(b, widget, options, z),
        "text entry" => emit_text_entry(b, widget, options, z),
        "message button" => emit_message_button(b, widget, options, z),
        "menu" => emit_menu(b, widget, options, z),
        "choice button" => emit_choice_button(b, widget, options, z),
        "valuator" => emit_valuator(b, widget, options, z),
        "wheel switch" => emit_wheel_switch(b, widget, options, z),
        "byte" => emit_byte(b, widget, options, z),
        "bar" => emit_scale_indicator(b, widget, options, z, true),
        // `meter` has no dedicated PyDM/RsDM widget; adl2pydm draws it as an
        // indicator (a pointer scale), so it shares the indicator emitter.
        "indicator" | "meter" => emit_scale_indicator(b, widget, options, z, false),
        "rectangle" => emit_drawing(b, widget, options, z, "Rectangle"),
        "oval" => emit_drawing(b, widget, options, z, "Ellipse"),
        "composite" => emit_composite(b, widget, options, z),
        "strip chart" => emit_strip_chart(b, widget, options, z),
        "cartesian plot" => emit_cartesian_plot(b, widget, options, z),
        "arc" => emit_arc(b, widget, options, z),
        "polygon" => emit_polyshape(b, widget, options, z, true),
        "polyline" => emit_polyshape(b, widget, options, z, false),
        "image" => emit_image(b, widget, z),
        "embedded display" => emit_embedded_display(b, widget, options, z),
        "related display" => emit_related_display(b, widget, z),
        "shell command" => emit_shell_command(b, widget, z),
        // Unreachable: every `ADL_WIDGET_SYMBOLS` entry has an arm above. Kept as
        // a defensive backstop so a future symbol can't be silently dropped.
        _ => b.warnings.push(format!(
            "line {}: {:?} -> {} has no emitter (skipped)",
            widget.line, widget.symbol, map.rsdm_widget
        )),
    }

    // A MEDM `dynamic attribute` visibility rule gates every placement this widget
    // produced: build a `calc://` channel that evaluates the rule and wrap the
    // `place(...)` call in `if <gate non-zero> { … }`. A composite's children are
    // already drained into its frame placement above, so by here `placements[start..]`
    // is just this widget's own placement(s) — gating them hides the whole group.
    apply_dynamic_visibility(b, widget, options, start);
}

/// MEDM `dynamic attribute` channel keys → `calc://` variable names (the bound
/// channels A–D).
const VIS_CHANNEL_KEYS: [(&str, &str); 4] = [
    ("chan", "A"),
    ("chanB", "B"),
    ("chanC", "C"),
    ("chanD", "D"),
];

/// Wire a MEDM `dynamic attribute` visibility rule for the placements in
/// `[start..]`: emit a `calc://` gate channel (field + ctor) and tag each of this
/// widget's placements with the boolean that hides it when the rule is false. A
/// widget with no rule (`vis="static"` or no channel) is left ungated.
fn apply_dynamic_visibility(b: &mut Builder, widget: &MedmWidget, options: &Options, start: usize) {
    let Some(gate_addr) = visibility_gate_address(widget, options) else {
        return;
    };
    let id = b.index();
    let field = format!("gate{id}");
    b.needs_channel = true;
    let gate_addr_expr = medm_str(b, &gate_addr);
    b.ctors.push(format!(
        "let {field} = engine\n            .connect({gate_addr_expr})\n            .expect({});",
        rust_str(&format!("adl2rsdm: connect visibility gate {gate_addr}"))
    ));
    b.fields.push((field.clone(), "Channel".to_string()));
    // Read the gate's scalar each frame: shown only when the rule evaluates to a
    // definite non-zero. While the gate has no value yet (an input channel is
    // disconnected, so the calc:// channel has published nothing) the widget is
    // hidden — MEDM never applies a visibility rule to a disconnected
    // dynamic-attribute object (textDraw &c. blank the region with
    // drawWhiteRectangle instead of drawing); treating "unknown" as "visible"
    // made paired vis texts (Collecting/Done) overlap while disconnected.
    let cond = format!(
        "{field}.read(|s| s.value.as_ref().and_then(|v| v.as_f64())).is_some_and(|v| v != 0.0)"
    );
    for placement in &mut b.placements[start..] {
        placement.gate = Some(cond.clone());
    }
    b.warnings.push(format!(
        "line {}: dynamic visibility wired via {gate_addr}",
        widget.line
    ));
}

/// The `calc://` gate address for a widget's `dynamic attribute` visibility rule,
/// or `None` when it has no rule (`vis="static"` or no `vis`/`calc`) or no channel
/// to evaluate. The channels A–D bind `chan`/`chanB`/`chanC`/`chanD`; the
/// expression is the ORIGINAL MEDM CALC text (from the `vis` mode / `calc`
/// field), carried under rsdm's `dialect=medm` so the runtime evaluates it with
/// the EPICS calc engine — MEDM's own grammar and double-typed semantics
/// (`medm/utils.c` `calcVisibility` → `calcPerform`), not a lossy translation
/// into `evalexpr` syntax.
fn visibility_gate_address(widget: &MedmWidget, options: &Options) -> Option<String> {
    let da = widget.attributes.get("dynamic attribute")?;
    // MEDM's `vis` default is V_STATIC (always visible): `dynamicAttributeInit`
    // sets `vis = V_STATIC` (medm/medmCommon.c:805) and `writeDlDynamicAttribute`
    // omits the key when `vis == V_STATIC` (:1518), so a block with a channel but
    // no `vis` is a stock static-visibility object MEDM always draws
    // (`calcVisibility case V_STATIC: return True`, utils.c:4472). Defaulting an
    // absent `vis` to a gating mode fabricates a rule MEDM has none of and hides
    // the widget whenever the channel reads 0 (the common `clr="alarm"` +
    // `chan=…SEVR`, no `vis` pattern) — so absent `vis` resolves to `static`.
    let vis = da.get("vis").map(String::as_str).unwrap_or("static");
    let calc = da.get("calc").map(String::as_str).filter(|c| !c.is_empty());
    if vis == "static" {
        return None; // always visible — no gate
    }

    let mut vars = Vec::new();
    for (key, name) in VIS_CHANNEL_KEYS {
        if let Some(chan) = da.get(key).filter(|c| !c.is_empty()) {
            vars.push((name, apply_protocol(chan, options)));
        }
    }
    if vars.is_empty() {
        return None; // a visibility rule with no channel cannot be evaluated
    }

    let expr = percent_encode_calc(&medm_visibility_expr(vis, calc));
    let mut addr = format!(
        "calc://adl2rsdm_vis_{}?dialect=medm&expr={expr}",
        widget.line
    );
    let mut update = Vec::new();
    for (name, child) in &vars {
        let _ = write!(addr, "&{name}={child}");
        update.push(*name);
    }
    let _ = write!(addr, "&update={}", update.join(","));
    Some(addr)
}

/// The MEDM CALC expression for a visibility rule. `vis="calc"` uses the `calc`
/// field verbatim (default `A`); `if zero` / `if not zero` test channel `A`
/// against zero with MEDM's `=` / `#` operators — and IGNORE the `calc` field,
/// matching MEDM (`calcVisibility`, utils.c:4471-4477, reads `records[0]->value`
/// directly for IF_ZERO/IF_NOT_ZERO; `calc` participates only under V_CALC). A
/// stray `calc="0"` beside `vis="if not zero"` (ADSetup.adl's Connected text)
/// must not poison the gate into a constant. Deliberate deviation from
/// adl2pydm, which wraps the calc in the vis test (output_handler.py
/// `convertDynamicAttribute_to_Rules`) and so hides that text forever.
fn medm_visibility_expr(vis: &str, calc: Option<&str>) -> String {
    match (vis, calc) {
        ("calc", Some(expr)) => expr.to_string(),
        ("calc", None) => "A".to_string(),
        ("if zero", _) => "A=0".to_string(),
        // "if not zero" (MEDM's IF_NOT_ZERO test) and any unknown mode. Absent
        // `vis` never reaches here — it resolves to `static` (no gate) upstream.
        (_, _) => "A#0".to_string(),
    }
}

/// Percent-encode a MEDM CALC expression for the `calc://` query: only `%`
/// (the escape byte itself) and `&` (the query separator) need encoding — the
/// two bytes the raw query cannot carry. rsdm's MEDM dialect percent-decodes
/// the `expr` value on the other end; everything else (`#`, `=`, `?`, `:`,
/// parentheses) rides through the query untouched, keeping the emitted address
/// readable as the original MEDM expression.
fn percent_encode_calc(expr: &str) -> String {
    expr.replace('%', "%25").replace('&', "%26")
}

/// `text` — a static label (a fixed string, no channel). Drawn with a plain
/// `ui.label`, so it needs no struct field.
fn emit_static_text(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        b.warnings.push(format!(
            "line {}: text has no geometry; skipped",
            widget.line
        ));
        return;
    };
    let id = b.index();
    let text = widget.title.clone().unwrap_or_default();
    let color = widget.color.unwrap_or(Color { r: 0, g: 0, b: 0 });
    b.needs_color = true;
    // The text colour is the static `clr` unless the MEDM dynamic attribute sets
    // `clr="alarm"`, which recolours the fixed text by the channel's severity each
    // frame (`alarm_setup` binds a `Channel` field and a `__c` colour local).
    let (alarm_setup, color_token) = static_text_color(b, widget, options, color);
    // MEDM auto-sizes the font to the widget height; render the static text at
    // that size (egui resolves `RichText` without an explicit size against
    // `override_font_id` before `TextStyle::Body`).
    let font_px = font_px_from_height(geom.height);
    let label_call = format!(
        "ui.label(egui::RichText::new({}).color({color_token}));",
        medm_str(b, &text),
    );
    // MEDM `align` positions the text horizontally. Left (the default) keeps the
    // bare `ui.label`; centre/right wrap it in a top-down layout whose cross-axis
    // alignment moves the text without changing its vertical placement.
    let aligned = match text_alignment(widget) {
        Some((_, align)) => format!(
            "ui.with_layout(egui::Layout::top_down(egui::Align::{align}), |ui| {{ {label_call} }});"
        ),
        None => label_call,
    };
    let prelude = style_prelude(b, WidgetColors::default(), Some(font_px));
    // Centre the text row in the MEDM cell — the band every framed widget
    // renders (`RsdmLabel`'s justified path). MEDM draws static text at the top
    // with a font sized to fill the height; the height-derived font is smaller,
    // so centring reproduces MEDM's visual band for group titles too. The
    // spacing runs before the alignment layout, so it serves all three aligns.
    let centring = "    let __font = ui.style().override_font_id.clone().unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()));\n    let __row = ui.fonts_mut(|f| f.row_height(&__font));\n    ui.add_space(((ui.available_height() - __row) / 2.0).max(0.0));\n";
    let body = format!("{{\n{prelude}{alarm_setup}{centring}    {aligned}\n}}");
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// `text update` — a read-only `RsdmLabel` bound to a channel.
fn emit_text_update(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmLabel::new(&engine, {})", medm_str(b, &addr));
    let mut builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    builders.extend(string_format_builder(b, widget, &addr));
    builders.extend(alarm_content_builder(widget));
    if let Some((variant, _)) = text_alignment(widget) {
        builders.push(format!(".with_alignment(TextAlign::{variant})"));
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmLabel",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (text update)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `text entry` — an editable `RsdmLineEdit` bound to a channel.
fn emit_text_entry(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmLineEdit::new(&engine, {})", medm_str(b, &addr));
    let mut builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    builders.extend(string_format_builder(b, widget, &addr));
    // MEDM clrmod="alarm" recolours the field text by severity (medmTextEntry.c:
    // 418-424); wired via RsdmLineEdit's alarm-sensitive content.
    builders.extend(alarm_content_builder(widget));
    // Centre the field text. This is a DELIBERATE DEVIATION from MEDM and PyDM,
    // both of which left-align text entries (MEDM's `XmTextField` has no
    // `XmNalignment`; adl2pydm sets `alignment` only on text/text-update, not on
    // the line edit) — applied uniformly here so the editable control fields
    // match the centred menu/button captions on converted screens (user ask).
    builders.push(".with_alignment(TextAlign::Center)".to_string());
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmLineEdit",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr}"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `message button` — a `RsdmPushButton` that writes `press_msg` (and optionally
/// `release_msg`) to its channel; the MEDM `label` is the caption.
fn emit_message_button(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let label = widget.title.clone().unwrap_or_default();
    let press = widget
        .assignments
        .get("press_msg")
        .cloned()
        .unwrap_or_default();
    let new_call = format!(
        "RsdmPushButton::new(&engine, {}, {}, {})",
        medm_str(b, &addr),
        medm_str(b, &label),
        medm_str(b, &press)
    );
    let mut builders = Vec::new();
    if let Some(release) = widget.assignments.get("release_msg").cloned() {
        builders.push(format!(".with_release_value({})", medm_str(b, &release)));
    }
    // MEDM clrmod="alarm" recolours the caption by severity (medmMessageButton.c:348).
    builders.extend(alarm_content_builder(widget));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmPushButton",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (message button)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `menu` — a `RsdmEnumComboBox` over the channel's enum strings.
fn emit_menu(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmEnumComboBox::new(&engine, {})", medm_str(b, &addr));
    // An MEDM menu is a Motif option menu whose caption is centred (XmLabel's
    // default XmNalignment; medmMenu.c never overrides it), so every converted
    // menu centres — there is no per-widget `align`.
    let mut builders = vec![".with_alignment(TextAlign::Center)".to_string()];
    // MEDM clrmod="alarm" recolours the face caption by severity (medmMenu.c:540).
    builders.extend(alarm_content_builder(widget));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmEnumComboBox",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (menu)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `choice button` — a `RsdmEnumButton` group over the channel's enum strings.
/// MEDM `stacking` maps to orientation as in `adl2pydm`: `row` (default) stacks
/// vertically, `column` lays the buttons out horizontally.
fn emit_choice_button(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmEnumButton::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    let stacking = widget
        .assignments
        .get("stacking")
        .map(String::as_str)
        .unwrap_or("row");
    let vertical = match stacking {
        // `row` -> Vertical, which is `RsdmEnumButton`'s default, so no builder.
        "row" => true,
        "column" => {
            builders.push(".with_orientation(Orientation::Horizontal)".to_string());
            false
        }
        other => {
            b.warnings.push(format!(
                "line {}: choice button stacking {other:?} unsupported, using 'row'",
                widget.line
            ));
            true
        }
    };
    // Font: a `row` (vertical) stack shares the widget height among its
    // buttons, so the font must fit ONE button, not the whole geometry — MEDM
    // sizes each toggle at height/numberOfButtons and picks the font for that
    // cell (medmChoiceButtons.c:131-136). The enum strings are unknown at
    // convert time, so the item count is estimated from the geometry exactly
    // as adl2pydm does: est = max(2, round(h/20)), font from h/est
    // (output_handler.py:650-660). A `column` stack keeps the full height.
    let font_px = if vertical {
        let h = f64::from(geom.height);
        let est_items = (h / 20.0).round().max(2.0);
        font_px_from_fractional_height(h / est_items)
    } else {
        font_px_from_height(geom.height)
    };
    // MEDM clrmod="alarm" recolours the choice captions by severity
    // (medmChoiceButtons.c:375).
    builders.extend(alarm_content_builder(widget));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmEnumButton",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (choice button)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px),
        },
    );
}

/// `valuator` — a `RsdmSlider`. User-defined limits (`*Src == "default"`) and a
/// `dPrecision` map to `.with_limits` / `.with_precision`; `direction`
/// `up`/`down` turn the track vertical (MEDM medmValuator.c:201-225 sets
/// `XmVERTICAL` for both; default RIGHT, :1446).
fn emit_valuator(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmSlider::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    // MEDM clrmod="alarm" recolours the value by severity (medmValuator.c:892-895).
    // RsdmSlider ships alarm-sensitive content ON (PyDM parity), so a valuator
    // without clrmod="alarm" must turn it OFF; one with it takes the MEDM palette.
    builders.extend(valuator_alarm_builder(widget));
    builders.extend(user_defined_limits(widget));
    if let Some(orientation) = direction_orientation(b, widget, false) {
        builders.push(orientation);
    }
    // MEDM additionally REVERSES down/left via XmNprocessingDirection
    // (MAX_ON_BOTTOM / MAX_ON_LEFT, medmValuator.c:203-215). The rsdm slider —
    // like PyDM's, which takes only a Qt orientation (slider.py:35-36) — has
    // no inverted mode, so the axis is kept and the reversal is warned rather
    // than silently dropped.
    if let Some(direction @ ("down" | "left")) =
        widget.assignments.get("direction").map(String::as_str)
    {
        b.warnings.push(format!(
            "line {}: valuator direction {direction:?} keeps its axis, but MEDM's \
             reversed max-end has no rsdm/PyDM slider surface",
            widget.line
        ));
    }
    if let Some(prec) = widget
        .assignments
        .get("dPrecision")
        .and_then(|s| s.parse::<f64>().ok())
    {
        builders.push(format!(".with_precision({})", prec as i32));
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmSlider",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (valuator)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `wheel switch` — a `RsdmSpinbox`. User-defined limits map to `.with_limits`;
/// the MEDM `format` (`integer` or `w.d`) maps to `.with_precision` decimals.
fn emit_wheel_switch(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmSpinbox::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    // MEDM clrmod="alarm" recolours the digits by severity (medmWheelSwitch.c:390).
    builders.extend(alarm_content_builder(widget));
    builders.extend(user_defined_limits(widget));
    // Precision comes from MEDM `format` (what adl2pydm reads), falling back to
    // the `limits` block's `precDefault` (what real wheel-switch screens carry).
    if let Some(fmt) = widget.assignments.get("format") {
        // Xc `compute_format` always yields a precision (DEFAULT 2 for an
        // unparseable format), never leaving it to the channel — see wheel_decimals.
        builders.push(format!(".with_precision({})", wheel_decimals(fmt)));
    } else if let Some(prec) = precision_default_builder(widget) {
        builders.push(prec);
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmSpinbox",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (wheel switch)"),
            builders: &builders,
            // The spinbox renders its value as an (uncoloured-RichText) button,
            // so `clr` reaches the displayed number through `override_text_color`
            // and `bclr` fills behind it — the same text/fill semantics as the
            // other value widgets, unlike the slider whose `clr` is a track colour.
            colors: WidgetColors::from_widget(widget),
            // adl2pydm does not size the wheel-switch font from height; keep the
            // rsdm default so we stay at parity.
            font_px: None,
        },
    );
}

/// `byte` — a `RsdmByteIndicator`. `sbit`/`ebit` give the bit count and shift;
/// `direction` gives the orientation (`right`/`left` -> horizontal). An absent
/// `sbit`/`ebit` means MEDM's defaults 15/0 (medmByte.c:279-280; writeDlByte
/// :366-369 omits exactly those values — adl2pydm's 0/0 fallback is a bug), so
/// a bare `byte` shows 16 bits. `sbit > ebit` displays the high bit first
/// (xc/Byte.c:513-519 with `reverse == False` draws segment `i` as bit
/// `sbit - i`) — rsdm's `with_big_endian(true)`.
fn emit_byte(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let sbit = widget
        .assignments
        .get("sbit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(15);
    let ebit = widget
        .assignments
        .get("ebit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    let num_bits = 1 + (sbit.max(ebit) - sbit.min(ebit));
    let shift = sbit.min(ebit);

    let new_call = format!("RsdmByteIndicator::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    // `RsdmByteIndicator` defaults: 1 bit, no shift, vertical.
    if num_bits != 1 {
        builders.push(format!(".with_num_bits({num_bits})"));
    }
    if shift != 0 {
        builders.push(format!(".with_shift({shift})"));
    }
    // MEDM bytes are bare segments — no per-bit labels (adl2pydm
    // write_block_byte_indicator emits `showLabels = False`); RsdmByteIndicator
    // defaults to PyDM's labels-on. Label-less is also what routes the widget
    // through the exact-share justified division (MEDM xc/Byte.c).
    builders.push(".with_show_labels(false)".to_string());
    // `RsdmByteIndicator` defaults to vertical.
    if let Some(orient) = direction_orientation(b, widget, true) {
        builders.push(orient);
    }
    // MEDM `sbit > ebit` (the 15..0 default) draws the HIGH bit first:
    // xc/Byte.c:513-519 sets `reverse` only when `ebit > sbit`, and the
    // non-reversed segment loop shows bit `sbit - i` (:551-552) — MSB first,
    // rsdm's big-endian display order. RsdmByteIndicator defaults to
    // little-endian, so apply the builder only then. (adl2pydm maps this
    // exactly backwards — `bigEndian` when sbit < ebit; MEDM C wins.)
    if sbit > ebit {
        builders.push(".with_big_endian(true)".to_string());
    }
    // MEDM `clr`/`bclr` are the on/off bit colours (adl2pydm maps them to PyDM's
    // `onColor`/`offColor`). The byte draws its own bits, so these go through the
    // widget's colour builders, not the text/fill `WidgetColors` path.
    if let Some(on) = widget.color {
        builders.push(format!(".with_on_color({})", color_expr(on)));
        b.needs_color = true;
    }
    if let Some(off) = widget.background_color {
        builders.push(format!(".with_off_color({})", color_expr(off)));
        b.needs_color = true;
    }
    // `clrmod="alarm"` recolours lit bits by severity (static on/off colours stay
    // the `NoAlarm` fallback).
    builders.extend(alarm_content_builder(widget));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmByteIndicator",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (byte)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `bar` / `indicator` / `meter` — a `RsdmScaleIndicator`. `bar` draws a filled
/// bar (`with_bar_indicator(true)`); `indicator`/`meter` use the default pointer
/// scale. User-defined limits, `direction`, and `precDefault` map to the
/// matching builders.
fn emit_scale_indicator(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
    z: ZLayer,
    bar: bool,
) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("RsdmScaleIndicator::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    if bar {
        builders.push(".with_bar_indicator(true)".to_string());
    }
    // MEDM's foreground `clr` colours the bar fill / pointer line; rsdm's scale
    // indicator otherwise uses its own default blue. Reproduce the MEDM colour as
    // the static bar colour; `clrmod="alarm"` additionally tracks severity (the
    // alarm builder below), and the static colour is the `NoAlarm` fallback.
    if let Some(c) = widget.color {
        builders.push(format!(".with_bar_color({})", color_expr(c)));
        b.needs_color = true;
    }
    builders.extend(alarm_content_builder(widget));
    builders.extend(user_defined_limits(widget));
    // `RsdmScaleIndicator` defaults to horizontal.
    if let Some(orient) = direction_orientation(b, widget, false) {
        builders.push(orient);
    }
    // A BAR's `direction="down"`/`"left"` grows from the opposite edge: MEDM
    // maps them to the inverted Xc orientations (medmBar.c:154-186 →
    // XcVertDown/XcHorizLeft; xc/BarGraph.c:939-988 fills from the top/right
    // edge). indicator/meter deliberately get NO inversion: MEDM itself
    // rejects down/left there, overriding to up/right with a warning
    // (medmIndicator.c:142-166; medmMeter.c has no direction at all) — which
    // is exactly what the axis-only mapping above already produces.
    if bar
        && matches!(
            widget.assignments.get("direction").map(String::as_str),
            Some("down" | "left")
        )
    {
        builders.push(".with_inverted_appearance(true)".to_string());
    }
    if let Some(prec) = precision_default_builder(widget) {
        builders.push(prec);
    }
    // A bar's `fillmod="from center"` anchors the fill on the scale midpoint
    // (medmBar.c:496-502 parses the token; xc/BarGraph.c fills between
    // mid = len/2 and the value). Bar-only: indicator/meter have no fillmod.
    if bar && widget.assignments.get("fillmod").map(String::as_str) == Some("from center") {
        builders.push(".with_origin_at_center(true)".to_string());
    }
    // The value label follows the MEDM decoration `label` on ALL THREE scale
    // monitors, not just the bar: valueVisible is TRUE only for
    // `limits`/`channel` (bar medmBar.c:132-150, indicator
    // medmIndicator.c:122-140, meter medmMeter.c:134-148; adl2pydm's
    // `showValue`), unlike `RsdmScaleIndicator` which shows it by default.
    let label = widget.assignments.get("label").map(String::as_str);
    let show_value = matches!(label, Some("limits") | Some("channel"));
    if !show_value {
        builders.push(".with_value_label(false)".to_string());
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmScaleIndicator",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (scale indicator)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `rectangle` / `oval` — a `RsdmDrawing` of the given `shape` (`Rectangle` /
/// `Ellipse`). Decorations carry no primary channel, so a `loc://` placeholder
/// is used unless a `dynamic attribute` supplies one. The `basic attribute`
/// block's `fill`/`style`/`width` set the brush and pen: `solid` fills with the
/// widget colour; `outline` (MEDM `NoBrush`) draws only a border, forced to
/// width >= 1 so it shows, as adl2pydm's `write_basic_attribute` does.
fn emit_drawing(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer, shape: &str) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let (addr, placeholder) = dynamic_channel(b, widget, options, "shape");
    let new_call = format!(
        "RsdmDrawing::new(&engine, {}, DrawingShape::{shape})",
        medm_str(b, &addr)
    );
    let mut builders = drawing_brush_builders(b, widget);
    builders.push(drawing_size_builder(geom));
    if placeholder {
        builders.push(".with_placeholder_channel()".to_string());
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (drawing)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// The `.with_fill(...)` / `.with_border(...)` builders for any [`RsdmDrawing`]
/// shape, from the `basic attribute` block (shared by rectangle/oval/arc/
/// polygon/polyline). `solid` fills with the widget colour; `outline` (MEDM
/// `NoBrush`) draws only a border forced to width >= 1, as adl2pydm's
/// `write_basic_attribute` does. A `dash` pen style is flagged (no RsdmDrawing
/// pen-style builder).
fn drawing_brush_builders(b: &mut Builder, widget: &MedmWidget) -> Vec<String> {
    let ba = widget.attributes.get("basic attribute");
    let fill_mode = ba
        .and_then(|a| a.get("fill"))
        .map(String::as_str)
        .unwrap_or("solid");
    let style = ba
        .and_then(|a| a.get("style"))
        .map(String::as_str)
        .unwrap_or("solid");
    let width = ba
        .and_then(|a| a.get("width"))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let color = widget.color.unwrap_or(Color { r: 0, g: 0, b: 0 });
    b.needs_color = true;

    let mut builders = Vec::new();
    if fill_mode == "outline" {
        builders.push(".with_fill(Color32::TRANSPARENT)".to_string());
        builders.push(format!(
            ".with_border({}, {})",
            color_expr(color),
            float_lit(width.max(1.0))
        ));
    } else {
        builders.push(format!(".with_fill({})", color_expr(color)));
        if width > 0.0 {
            builders.push(format!(
                ".with_border({}, {})",
                color_expr(color),
                float_lit(width)
            ));
        }
    }
    if style == "dash" {
        b.warnings.push(format!(
            "line {}: drawing dash border style not applied (RsdmDrawing has no pen-style builder)",
            widget.line
        ));
    }
    // MEDM dynamic-attribute clr="alarm": recolour the colour this shape actually
    // draws with — the border for an `outline` shape, the fill for a solid one.
    builders.extend(drawing_alarm_builder(widget, fill_mode == "outline"));
    builders
}

/// `.with_size(...)` sized from MEDM geometry. Without it `RsdmDrawing::show`
/// allocates its `DEFAULT_SIZE` (40×40), so a large group-box rectangle or a
/// long polyline collapses to a tiny square. Mirrors how `emit_image` sizes
/// `RsdmImage` from the same `object` geometry — the drawing then fills exactly
/// the `place()` rect it is positioned into.
fn drawing_size_builder(geom: Geometry) -> String {
    format!(
        ".with_size(egui::Vec2::new({}, {}))",
        float_lit(f64::from(geom.width)),
        float_lit(f64::from(geom.height))
    )
}

/// `arc` — a `RsdmDrawing(DrawingShape::Arc { begin_deg, span_deg })`. The MEDM
/// `begin`/`path` angles are parsed to degrees (`beginAngle`/`pathAngle`); RsDM's
/// arc keeps MEDM's X11 convention (0° at 3 o'clock, CCW positive), so the
/// parsed values are used directly (no Qt-style negation). An opaque fill paints
/// a pie wedge; `outline` paints an open stroked arc.
///
/// When a key is absent MEDM keeps the value `createDlArc` seeded before
/// parsing: `begin = 0`, `path = 90*64` (`medm/medmArc.c:258-259`, reached via
/// `parseArc`'s `createDlArc(NULL)` at `medmArc.c:277`) — that is 0° and 90°,
/// the angles being stored in 1/64°. PyDM agrees: `adl2pydm` omits `spanAngle`
/// for an absent `path` (`output_handler.py:559`) and `PyDMDrawingArc` seeds
/// `_span_angle = deg_to_qt(90)` (`drawing.py:1271`).
fn emit_arc(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let (addr, placeholder) = dynamic_channel(b, widget, options, "shape");
    let begin = angle_deg(widget, "beginAngle", 0.0);
    let span = angle_deg(widget, "pathAngle", 90.0);
    let new_call = format!(
        "RsdmDrawing::new(&engine, {}, DrawingShape::Arc {{ begin_deg: {}, span_deg: {} }})",
        medm_str(b, &addr),
        float_lit(begin),
        float_lit(span)
    );
    let mut builders = drawing_brush_builders(b, widget);
    builders.push(drawing_size_builder(geom));
    if placeholder {
        builders.push(".with_placeholder_channel()".to_string());
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} (arc)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `polyline` / `polygon` — a `RsdmDrawing(DrawingShape::Polyline|Polygon)` whose
/// vertices come from the MEDM `points` block. MEDM points are absolute screen
/// coordinates; they are normalised to offsets from the widget's `object` origin
/// (matching how `place()` positions the widget's `egui::Area`). A polyline is
/// stroked (no fill); a polygon honours the `basic attribute` brush. With fewer
/// than two points the geometry is degenerate, so a placeholder + warning is
/// emitted instead.
fn emit_polyshape(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
    z: ZLayer,
    polygon: bool,
) {
    let kind = if polygon { "polygon" } else { "polyline" };
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    if widget.points.len() < 2 {
        emit_marker_placeholder(
            b,
            widget,
            z,
            &format!("{kind} unsupported"),
            &format!("{kind} has fewer than 2 points; nothing to draw"),
        );
        return;
    }
    let (addr, placeholder) = dynamic_channel(b, widget, options, "shape");
    let shape = if polygon { "Polygon" } else { "Polyline" };
    let new_call = format!(
        "RsdmDrawing::new(&engine, {}, DrawingShape::{shape})",
        medm_str(b, &addr)
    );
    let mut builders = if polygon {
        drawing_brush_builders(b, widget)
    } else {
        // A polyline is stroked with the line pen only — no fill brush.
        polyline_stroke_builder(b, widget)
    };
    let verts: Vec<String> = widget
        .points
        .iter()
        .map(|p| {
            format!(
                "egui::Vec2::new({}, {})",
                float_lit(f64::from(p.x - geom.x)),
                float_lit(f64::from(p.y - geom.y))
            )
        })
        .collect();
    builders.push(format!(".with_points(vec![{}])", verts.join(", ")));
    builders.push(drawing_size_builder(geom));
    if placeholder {
        builders.push(".with_placeholder_channel()".to_string());
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "RsdmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2rsdm: connect {addr} ({kind})"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// The stroke-only `.with_border(...)` builder for a `polyline` (MEDM line pen):
/// the widget colour at the `basic attribute` width, forced to >= 1 so it shows.
/// A `dash` pen style is flagged (no RsdmDrawing pen-style builder).
fn polyline_stroke_builder(b: &mut Builder, widget: &MedmWidget) -> Vec<String> {
    let ba = widget.attributes.get("basic attribute");
    let width = ba
        .and_then(|a| a.get("width"))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let style = ba
        .and_then(|a| a.get("style"))
        .map(String::as_str)
        .unwrap_or("solid");
    let color = widget.color.unwrap_or(Color { r: 0, g: 0, b: 0 });
    b.needs_color = true;
    if style == "dash" {
        b.warnings.push(format!(
            "line {}: drawing dash border style not applied (RsdmDrawing has no pen-style builder)",
            widget.line
        ));
    }
    let mut builders = vec![format!(
        ".with_border({}, {})",
        color_expr(color),
        float_lit(width.max(1.0))
    )];
    // A polyline paints only its pen, so dynamic-attribute clr="alarm" recolours
    // the border (stroke) by severity.
    builders.extend(drawing_alarm_builder(widget, true));
    builders
}

/// A drawing's angle field (`beginAngle`/`pathAngle`) in degrees, or `default`
/// when absent. The parser already converted MEDM's 1/64° units to degrees.
fn angle_deg(widget: &MedmWidget, key: &str, default: f64) -> f64 {
    widget
        .assignments
        .get(key)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(default)
}

/// `composite` — a `RsdmFrame` grouping its children. MEDM stores children in
/// absolute screen coordinates, so each child is translated into the frame's
/// interior and re-layered back-to-front *inside* the frame's draw closure. The
/// frame paints nothing by default (transparent `egui::Frame::NONE`), so nesting
/// only adds the optional alarm border / enable-gating and the per-container
/// z-order — a control child still layers Foreground (never occluded), a
/// decoration child Background. A composite usually has no channel, so a `loc://`
/// placeholder is used unless its top-level `chan` is set.
fn emit_composite(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    // MEDM writes an embedded display as a *childless* composite carrying a
    // `"composite file"`; adl2pydm rewrites it to an embedded display at output
    // time, and so do we — route it to the inliner instead of an empty frame.
    if widget.children.is_empty() && widget.assignments.contains_key("composite file") {
        emit_embedded_display(b, widget, options, z);
        return;
    }
    let (addr, placeholder) = match widget.assignments.get("chan").filter(|c| !c.is_empty()) {
        Some(chan) => (apply_protocol(chan, options), false),
        None => (b.synthetic_addr("frame"), true),
    };
    // Composite children are in absolute SCREEN coordinates, so they translate
    // into the frame interior by the composite's own origin.
    emit_frame_container(
        b,
        z,
        geom,
        &addr,
        placeholder,
        &format!("adl2rsdm: connect {addr} (composite)"),
        &widget.children,
        (geom.x, geom.y),
        options,
    );
}

/// Emit a `RsdmFrame` at `geom` whose draw closure re-draws `children`
/// back-to-front in the frame interior. `child_origin` is the coordinate the
/// children are measured from: a composite's own screen origin for in-screen
/// children, or `(0, 0)` for an embedded display's children (which carry the
/// target screen's own origin-relative coordinates). The single owner of
/// frame-container emission, shared by `composite` and `embedded display`.
#[allow(clippy::too_many_arguments)]
fn emit_frame_container(
    b: &mut Builder,
    z: ZLayer,
    geom: Geometry,
    addr: &str,
    placeholder: bool,
    connect_desc: &str,
    children: &[MedmWidget],
    child_origin: (i32, i32),
    options: &Options,
) {
    let frame_id = b.index();
    let frame_field = format!("w{frame_id}");
    b.needs_widgets = true;
    let addr_expr = medm_str(b, addr);
    // A synthetic frame address must not surface as a PV (tooltip, Btn2 copy).
    let mark = if placeholder {
        "\n            .with_placeholder_channel()"
    } else {
        ""
    };
    b.ctors.push(format!(
        "let {frame_field} = RsdmFrame::new(&engine, {addr_expr})\n            .expect({}){mark};",
        rust_str(connect_desc)
    ));
    b.fields
        .push((frame_field.clone(), "RsdmFrame".to_string()));

    // Emit the children into the shared builder, then lift their placements out of
    // the top-level list and into this frame's draw closure (coordinate-translated
    // by `child_origin` and re-layered back-to-front). Their struct fields / ctors
    // stay; only the *draw* moves inside the frame.
    let start = b.placements.len();
    for child in children {
        emit_widget(b, child, options);
    }
    let mut child_placements: Vec<Placement> = b.placements.drain(start..).collect();
    child_placements.sort_by_key(|p| p.z);

    let (dx, dy) = child_origin;

    // A childless container is just its (empty) frame shell, drawn at the
    // container's own layer.
    if child_placements.is_empty() {
        let origin = format!("__frame_origin_{frame_id}");
        let mut body = String::new();
        let _ = writeln!(body, "let {origin} = ui.max_rect().min;");
        let _ = write!(body, "let _ = {frame_field}.show(ui, |ui| {{}});");
        b.placements.push(Placement::drawn(z, frame_id, geom, body));
        return;
    }

    // MEDM draws strictly in file order with composites TRANSPARENT (a composite
    // is a group, not a stacking context): a sibling on the same layer that is
    // later in the file must paint over a composite child on that layer. egui
    // stacks same-`Order` Areas by CREATION order, and a single frame closure
    // creates every child's Area at one statement position — so a multi-layer
    // composite could honour file order on at most ONE layer (sorting the frame
    // at its children's lowest layer fixed the ADBuffers title-chip-over-text
    // case, but a control inside the composite then stacked wrong against an
    // earlier same-layer top-level sibling). The structural cure: emit ONE
    // placement PER layer present, each sharing the frame's outer rect/origin so
    // children translate identically, so each child's Area is created at its OWN
    // layer's statement position — file order then holds on every layer at once.
    // The frame shell (border/enable — both no-ops on the children, which are
    // detached Areas) rides the lowest layer, behind everything it groups. A
    // visibility gate the caller sets applies to all of these placements
    // uniformly (apply_dynamic_visibility tags every placement in `[start..]`).
    let mut layers: Vec<ZLayer> = child_placements.iter().map(|p| p.z).collect();
    layers.dedup(); // already sorted ascending by the sort_by_key above
    let home = layers[0];

    for &layer in &layers {
        // Distinct Area id per layer group (reuse the frame's id for the home
        // group); `place()` salts the Area with it, so the groups never collide.
        let pid = if layer == home { frame_id } else { b.index() };
        // Capture each group's OUTER top-left before `show` insets the home
        // group's interior by `BORDER_INSET`; children are positioned relative to
        // this, so the inset never shifts them. Every group shares the frame's
        // rect, so each captures the same origin and children land identically.
        let origin = format!("__frame_origin_{pid}");
        let mut body = String::new();
        let _ = writeln!(body, "let {origin} = ui.max_rect().min;");
        let group = child_placements.iter().filter(|p| p.z == layer);
        if layer == home {
            // The lowest layer hosts the frame shell (and consumes its field).
            let _ = writeln!(body, "let _ = {frame_field}.show(ui, |ui| {{");
            for p in group {
                write_placement(&mut body, p, dx, dy, "    ", options.use_layout, &origin);
            }
            let _ = write!(body, "}});");
        } else {
            for p in group {
                write_placement(&mut body, p, dx, dy, "", options.use_layout, &origin);
            }
            // Drop the trailing newline so bodies are uniformly newline-free.
            while body.ends_with('\n') {
                body.pop();
            }
        }
        b.placements.push(Placement::drawn(layer, pid, geom, body));
    }
}

/// `strip chart` → `RsdmTimePlot`: each MEDM `pen` is a time-series curve. A pen
/// with no `chan` is skipped (nothing to plot); a strip chart with no pens at all
/// is dropped with a warning. MEDM `period` (scaled by `units` to seconds) sets
/// the displayed time span; absent, rsdm's own default span stands.
fn emit_strip_chart(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let pens = widget.records.get("pens").map(Vec::as_slice).unwrap_or(&[]);
    if pens.is_empty() {
        b.warnings.push(format!(
            "line {}: strip chart has no pens; skipped",
            widget.line
        ));
        return;
    }

    // A pen's `limits {}` block (MEDM `parsePen` → `parseLimits`) is retained by
    // the parser's deep pass, so its range keys are visible here. MEDM scales
    // EACH pen to its own `[lopr, hopr]` onto a shared axis (medmStripChart.c
    // :1878-1898 — per-pen normalised traces). When any pen carries an authored
    // range, emit every pen through `add_normalized_channel` so RsdmTimePlot maps
    // it onto the shared [0,1] axis (R3-18); a chart with no authored ranges stays
    // on `add_channel`'s single auto-scaled axis (the common same-range case).
    let normalized = pens.iter().any(|pen| {
        let (lo, hi) = defaulted_limits(|k| pen.get(k));
        lo.is_some() || hi.is_some()
    });

    let mut adds = Vec::new();
    for pen in pens {
        let Some(chan) = pen.get("chan").filter(|c| !c.is_empty()) else {
            b.warnings.push(format!(
                "line {}: strip chart pen has no chan; skipped",
                widget.line
            ));
            continue;
        };
        let addr = apply_protocol(chan, options);
        if normalized {
            let (lo, hi) = defaulted_limits(|k| pen.get(k));
            adds.push(format!(
                "add_normalized_channel(&engine, {}, {}, {}, {}, {}).expect({});",
                medm_str(b, &addr),
                record_color(pen.get("color")),
                medm_str(b, chan),
                opt_float_lit(lo),
                opt_float_lit(hi),
                rust_str(&format!("adl2rsdm: add strip-chart pen {chan}")),
            ));
        } else {
            adds.push(format!(
                "add_channel(&engine, {}, {}, {}).expect({});",
                medm_str(b, &addr),
                record_color(pen.get("color")),
                medm_str(b, chan),
                rust_str(&format!("adl2rsdm: add strip-chart curve {chan}")),
            ));
        }
    }
    if adds.is_empty() {
        return; // every pen lacked a channel; warnings already recorded
    }
    if normalized {
        // The normalization (readability) is now applied; note only the residual
        // fidelity gap so it is not a silent cap: MEDM draws a separate y-axis
        // label column per pen range, whereas rsdm shares one [0,1] axis.
        b.warnings.push(format!(
            "line {}: strip chart normalises each pen to its own [lopr, hopr] onto a shared \
             [0,1] axis (MEDM per-pen normalisation); MEDM's separate per-range y-axis label \
             columns are not reproduced",
            widget.line
        ));
    }

    let mut with = Vec::new();
    let (span, span_warning) = strip_chart_span(widget);
    with.push(format!(".with_time_span({})", float_lit(span)));
    if let Some(w) = span_warning {
        b.warnings.push(w);
    }
    // plotcom styling (title/labels/colours); a strip chart has no axis blocks.
    with.extend(plot_style_builders(b, widget, false));
    b.needs_color = true;
    let plot_id = plot_id_expr(b);
    push_plot_widget(
        b,
        z,
        geom,
        "RsdmTimePlot",
        &format!("RsdmTimePlot::new(rs, {plot_id})"),
        &with,
        &adds,
    );
}

/// The `PlotId` expression for the next plot: an offset into the instance's
/// `__plot_base` block (`new_in` allocates it from the shared counter, so two
/// screen instances never collide on GPU plot resources). The first plot is
/// the bare base — `+ 0` would trip clippy's `identity_op` in the output.
fn plot_id_expr(b: &mut Builder) -> String {
    match b.plot_id() {
        0 => "__plot_base".to_string(),
        n => format!("__plot_base + {n}"),
    }
}

/// `cartesian plot` → `RsdmWaveformPlot` (default) or `RsdmScatterPlot`
/// (`--use-scatterplot`). Each MEDM `trace` is one curve.
///
/// Waveform: a trace needs `ydata` (else it is skipped, as adl2pydm requires a
/// `y_channel`); `xdata` plots Y against an X array, its absence against the
/// array index. Scatter: a trace needs *both* `xdata` and `ydata` (rsdm's
/// scatter pairs two scalar channels); a trace missing either is warned and
/// skipped. MEDM `count` (point budget) maps to the scatter buffer size; the
/// waveform plot has no per-curve budget, so `count` does not apply there.
fn emit_cartesian_plot(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let traces = widget
        .records
        .get("traces")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if traces.is_empty() {
        b.warnings.push(format!(
            "line {}: cartesian plot has no traces; skipped",
            widget.line
        ));
        return;
    }

    let scatter = options.use_scatterplot;
    let mut adds = Vec::new();
    for (i, trace) in traces.iter().enumerate() {
        let legend = format!("curve {}", i + 1);
        let color = record_color(trace.get("color"));
        // MEDM binds each trace to Y1 (yaxis=0) or Y2 (yaxis=1) and to a left/right
        // yside at execute time (medmMonitor.c:346-358; writeDlTrace writes yaxis
        // unconditionally). rsdm's cartesian plot has a single y-axis (its
        // user-specified y2_axis range is already warned unsupported), so a trace
        // asking for Y2 / a non-default side cannot be honoured — warn rather than
        // silently plot it against Y1.
        let yaxis = trace
            .get("yaxis")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let yside = trace
            .get("yside")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if yaxis != 0 || yside != 0 {
            b.warnings.push(format!(
                "line {}: cartesian plot trace {} is assigned to a secondary y-axis \
                 (yaxis={yaxis}, yside={yside}); rsdm has a single y-axis, so it is \
                 plotted against Y1",
                widget.line,
                i + 1
            ));
        }
        let xdata = trace
            .get("xdata")
            .filter(|c| !c.is_empty())
            .map(|c| apply_protocol(c, options));
        let ydata = trace.get("ydata").filter(|c| !c.is_empty());

        if scatter {
            // Scatter pairs two scalar channels — both axes are required.
            let (Some(x), Some(y)) = (&xdata, ydata) else {
                b.warnings.push(format!(
                    "line {}: cartesian plot trace {} needs both xdata and ydata for a scatter plot; skipped",
                    widget.line,
                    i + 1
                ));
                continue;
            };
            let y = apply_protocol(y, options);
            adds.push(format!(
                "add_xy_channel(&engine, {}, {}, {}, {}).expect({});",
                medm_str(b, x),
                medm_str(b, &y),
                color,
                rust_str(&legend),
                rust_str(&format!("adl2rsdm: add scatter {legend}")),
            ));
        } else {
            let Some(y) = ydata else {
                b.warnings.push(format!(
                    "line {}: cartesian plot trace {} has no ydata; skipped",
                    widget.line,
                    i + 1
                ));
                continue;
            };
            let y = apply_protocol(y, options);
            // rsdm waveform `add_xy_channel(y, Option<x>)`: X array optional.
            adds.push(match &xdata {
                Some(x) => format!(
                    "add_xy_channel(&engine, {}, Some({}), {}, {}).expect({});",
                    medm_str(b, &y),
                    medm_str(b, x),
                    color,
                    rust_str(&legend),
                    rust_str(&format!("adl2rsdm: add waveform {legend}")),
                ),
                None => format!(
                    "add_channel(&engine, {}, {}, {}).expect({});",
                    medm_str(b, &y),
                    color,
                    rust_str(&legend),
                    rust_str(&format!("adl2rsdm: add waveform {legend}")),
                ),
            });
        }
    }
    if adds.is_empty() {
        return; // no usable traces; warnings already recorded
    }
    warn_unsupported_cartesian_keys(b, widget);

    let ty = if scatter {
        "RsdmScatterPlot"
    } else {
        "RsdmWaveformPlot"
    };
    // `count` budgets the scatter buffer (PyDM bufferSize); waveform has none.
    let mut with = Vec::new();
    if scatter
        && let Some(count) = widget
            .assignments
            .get("count")
            .and_then(|c| c.parse::<usize>().ok())
    {
        with.push(format!(".with_buffer_size({count})"));
    }
    // plotcom styling (title/labels/colours) + the x/y1/y2 axis ranges.
    with.extend(plot_style_builders(b, widget, true));
    b.needs_color = true;
    let plot_id = plot_id_expr(b);
    push_plot_widget(
        b,
        z,
        geom,
        ty,
        &format!("{ty}::new(rs, {plot_id})"),
        &with,
        &adds,
    );
}

/// Warn on the MEDM cartesian-plot runtime keys that have no rsdm surface, so
/// they are never silently dropped (medmCartesianPlot.c:2957-3070). rsdm's plot
/// is a live, full-array, auto-scaling line plot: no trigger/erase-PV wiring, no
/// per-plot style switch (`line plot` is what it already draws), and no
/// circular/stop point buffer. `line plot`/`line` and a numeric `count` are
/// faithful and stay silent. Because MEDM omits `style`/`erase_oldest` on write
/// when they equal their POINT_PLOT / ERASE_OLDEST_OFF defaults, an ABSENT key
/// *is* that default — so both are resolved to their default before matching,
/// making the on-disk-common point plot / stop-at-n cases warn like the written
/// ones rather than passing silently (R2-68 residual, R3-19).
fn warn_unsupported_cartesian_keys(b: &mut Builder, widget: &MedmWidget) {
    let line = widget.line;
    let a = &widget.assignments;

    if let Some(t) = a.get("trigger").filter(|s| !s.is_empty()) {
        b.warnings.push(format!(
            "line {line}: cartesian plot trigger PV {t:?} not wired; rsdm redraws on \
             every channel update"
        ));
    }
    if let Some(e) = a.get("erase").filter(|s| !s.is_empty()) {
        let mode = a
            .get("eraseMode")
            .map(String::as_str)
            .unwrap_or("if not zero");
        b.warnings.push(format!(
            "line {line}: cartesian plot erase PV {e:?} ({mode}) not wired; rsdm has \
             no plot-clear channel"
        ));
    }
    // `count` (medmCartesianPlot.c:2957-2963) and `countPvName` (:3055-3060) both
    // set the point count; a non-numeric value is a PV-driven buffer size rsdm
    // cannot honour (a numeric `count` is the scatter buffer, handled by caller).
    if let Some(c) = a
        .get("countPvName")
        .or_else(|| a.get("count"))
        .filter(|c| !c.is_empty() && c.parse::<usize>().is_err())
    {
        b.warnings.push(format!(
            "line {line}: cartesian plot count PV {c:?} degrades to the default \
             buffer; rsdm has no PV-driven point count"
        ));
    }
    // MEDM omits `style` on write when it equals its POINT_PLOT default
    // (`createDlCartesianPlot:2904` / `writeDlCartesianPlot:3106`), so an ABSENT
    // key IS a point plot — the most common cartesian style on disk. Resolve the
    // default before matching so present and absent are treated by one uniform
    // rule (the R2-68 fix only gated on the present key, leaving the default —
    // and thus the majority of real plots — silently drawn as connected lines).
    {
        let style = a.get("style").map(String::as_str).unwrap_or("point plot");
        let rendered = match style {
            "point plot" | "point" => Some("point plot"),
            "step" => Some("step"),
            "fill under" | "fill-under" => Some("fill under"),
            _ => None, // "line plot"/"line" is exactly what rsdm draws
        };
        if let Some(name) = rendered {
            b.warnings.push(format!(
                "line {line}: cartesian plot style {name:?} rendered as a connected \
                 line plot; rsdm has no per-plot style switch"
            ));
        }
    }
    // Likewise `erase_oldest` is omitted when it equals its ERASE_OLDEST_OFF
    // default ("plot n pts & stop", :2905 / :3109) — the absent key is that mode.
    {
        let mode = a.get("erase_oldest").map(String::as_str).unwrap_or("off");
        let behaviour = match mode {
            "on" | "plot last n pts" => Some("circular (plot last n pts)"),
            "off" | "plot n pts & stop" => Some("stop-at-n (plot n pts & stop)"),
            _ => None,
        };
        if let Some(behaviour) = behaviour {
            b.warnings.push(format!(
                "line {line}: cartesian plot erase_oldest {behaviour} not reproduced; \
                 rsdm plots the full incoming array"
            ));
        }
    }
}

/// Plot-styling builders shared by `strip chart` and `cartesian plot`: the
/// `plotcom` block's `title`/`xlabel`/`ylabel` (MEDM `parsePlotcom`; its
/// `clr`/`bclr` are hoisted onto the widget colours by the parser, emitted here
/// as the axis/background colour builders), plus — when `axes` — the cartesian
/// `x_axis`/`y1_axis`/`y2_axis` blocks (MEDM `parsePlotAxisDefinition`,
/// medmMonitor.c:193-237): `rangeStyle="user-specified"` pins the axis to
/// `minRange`..`maxRange`, while "auto-scale"/"from channel" keep rsdm's live
/// autoscale (adl2pydm maps the same tokens onto PyDM's autoRange*/min*Range
/// properties). A user-specified y2 range and a log10 `axisStyle` have no rsdm
/// surface — warned, not silently dropped.
fn plot_style_builders(b: &mut Builder, widget: &MedmWidget, axes: bool) -> Vec<String> {
    let mut with = Vec::new();
    if let Some(pc) = widget.attributes.get("plotcom") {
        for (key, method) in [
            ("title", "with_title"),
            ("xlabel", "with_x_label"),
            ("ylabel", "with_y_label"),
        ] {
            if let Some(text) = pc.get(key).filter(|t| !t.is_empty()) {
                let text = medm_str(b, text);
                with.push(format!(".{method}({text})"));
            }
        }
    }
    if axes {
        for (block, method) in [("x_axis", "with_x_range"), ("y1_axis", "with_y_range")] {
            if let Some((min, max)) = user_axis_range(widget, block) {
                with.push(format!(".{method}({}, {})", float_lit(min), float_lit(max)));
            }
        }
        if user_axis_range(widget, "y2_axis").is_some() {
            b.warnings.push(format!(
                "line {}: cartesian plot y2_axis user-specified range has no rsdm \
                 surface (the plots drive the left Y axis); ignored",
                widget.line
            ));
        }
        for block in ["x_axis", "y1_axis", "y2_axis"] {
            if widget
                .attributes
                .get(block)
                .and_then(|a| a.get("axisStyle"))
                .is_some_and(|s| s == "log10")
            {
                b.warnings.push(format!(
                    "line {}: cartesian plot {block} axisStyle=log10 is not supported; \
                     kept linear",
                    widget.line
                ));
            }
        }
    }
    if let Some(c) = widget.color {
        with.push(format!(".with_axis_color({})", color_expr(c)));
    }
    if let Some(c) = widget.background_color {
        with.push(format!(".with_background_color({})", color_expr(c)));
    }
    with
}

/// The `minRange`..`maxRange` of a cartesian axis block when its
/// `rangeStyle="user-specified"`. An absent range field keeps MEDM's default
/// (`plotAxisDefinitionInit`, medmMonitor.c:41-49: minRange 0.0, maxRange 1.0).
fn user_axis_range(widget: &MedmWidget, block: &str) -> Option<(f64, f64)> {
    let axis = widget.attributes.get(block)?;
    if axis.get("rangeStyle").map(String::as_str) != Some("user-specified") {
        return None;
    }
    let range = |key: &str, default: f64| {
        axis.get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    Some((range("minRange", 0.0), range("maxRange", 1.0)))
}

/// The strip chart's displayed time span in seconds, plus an optional converter
/// warning. MEDM's span is `period` scaled by `units` to seconds — the only
/// units are `milli-second`/`milli second` (×0.001), `second` (×1), `minute`
/// (×60) (`medmStripChart.c:586-598`); `"hour"` is not a MEDM unit. `period`
/// and `units` both default to MEDM's stock values when the key is absent
/// (`SC_DEFAULT_PERIOD 60.0`, `SC_DEFAULT_UNITS SECONDS`, `:39-40`, omitted at
/// those defaults, `:2211`), so a strip chart with no `period` is a 60-second
/// window, not rsdm's 5 s. Pre-2.1 files instead carry `delay`, which MEDM
/// converts to a period via a units factor (`-0.060`/`-60`/`-3600` × delay,
/// `:2140-2160`) and a `linear_scale` nice-rounding; the factor is ported here
/// and the nice-rounding is approximated (warned). adl2pydm passes `period`
/// through raw (unscaled) — MEDM C is the contract.
fn strip_chart_span(widget: &MedmWidget) -> (f64, Option<String>) {
    let unit = widget.assignments.get("units").map(String::as_str);
    let period_scale = match unit {
        Some("milli-second") | Some("milli second") => 0.001,
        Some("minute") => 60.0,
        _ => 1.0, // second (SC_DEFAULT_UNITS) or absent
    };
    if let Some(period) = widget
        .assignments
        .get("period")
        .and_then(|p| p.parse::<f64>().ok())
    {
        return (period * period_scale, None);
    }
    // Pre-2.1 `delay` (only consulted when `period` is absent, as in MEDM).
    if let Some(delay) = widget
        .assignments
        .get("delay")
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|&d| d > 0.0)
    {
        let delay_factor = match unit {
            Some("milli-second") | Some("milli second") => 0.060,
            Some("minute") => 3600.0,
            _ => 60.0,
        };
        return (
            delay * delay_factor,
            Some(format!(
                "line {}: legacy strip-chart `delay={delay}` converted to a \
                 {}-second span; MEDM's linear_scale nice-rounding is approximated",
                widget.line,
                delay * delay_factor
            )),
        );
    }
    // Absent period and delay: MEDM's stock 60-second window.
    (60.0, None)
}

/// `Color32::from_rgb(...)` for a trace/pen record's resolved `color` (the
/// `"r,g,b"` the parser stored from `data_clr`/`clr`), white when absent or
/// malformed (so a curve always has a colour).
fn record_color(color: Option<&String>) -> String {
    let (r, g, b) = color.and_then(|s| parse_rgb(s)).unwrap_or((255, 255, 255));
    format!("Color32::from_rgb({r}, {g}, {b})")
}

/// Parse a `"r,g,b"` triple back into bytes.
fn parse_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let mut it = s.split(',');
    let r = it.next()?.trim().parse().ok()?;
    let g = it.next()?.trim().parse().ok()?;
    let b = it.next()?.trim().parse().ok()?;
    Some((r, g, b))
}

/// Emit a GPU plot widget: a `let mut <field> = <new_call><with builders>;`
/// constructor (the plot takes `rs` + a `PlotId`) followed by one
/// `<field>.<add>` statement per curve (each `add` is the method call after the
/// field, e.g. `add_channel(&engine, …).expect(…);`). Stores the field, builds
/// it in `new()`, and draws it back-to-front in `ui()`. Distinct from
/// [`push_channel_widget`]: a plot needs `&mut` plus follow-up `add_*` calls, not
/// a single builder expression.
fn push_plot_widget(
    b: &mut Builder,
    z: ZLayer,
    geom: Geometry,
    ty: &str,
    new_call: &str,
    with_builders: &[String],
    adds: &[String],
) {
    let id = b.index();
    let field = format!("w{id}");
    b.needs_widgets = true;
    // The plot ctor consumes `rs`, so `new_in` must unwrap its render state.
    b.needs_render_state = true;

    let mut ctor = format!("let mut {field} = {new_call}");
    for bld in with_builders {
        let _ = write!(ctor, "{bld}");
    }
    ctor.push(';');
    b.ctors.push(ctor);
    for add in adds {
        b.ctors.push(format!("{field}.{add}"));
    }
    b.fields.push((field.clone(), ty.to_string()));
    // Reference the field's `&mut` local (bound by `ui()`'s `let Self { .. }`
    // destructure), matching every other widget's draw.
    b.placements.push(Placement::drawn(
        z,
        id,
        geom,
        justified_body(&format!("let _ = {field}.show(ui);")),
    ));
}

/// `image` — a MEDM static GIF/TIFF *file* display, emitted as a channel-less
/// `RsdmImage` that decodes the file at run time and draws it scaled to the MEDM
/// geometry. The `image name` is the file path (resolved relative to the running
/// app's working directory / EPICS display path); a missing/undecodable file
/// draws a labelled placeholder at run time, not at build time. With no
/// `image name` there is nothing to load, so a converter placeholder + warning is
/// emitted instead.
fn emit_image(b: &mut Builder, widget: &MedmWidget, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let file = widget
        .assignments
        .get("image name")
        .map(String::as_str)
        .unwrap_or("");
    if file.is_empty() {
        emit_marker_placeholder(
            b,
            widget,
            z,
            "image (no file)",
            "image has no \"image name\"; nothing to load",
        );
        return;
    }
    let new_call = format!("RsdmImage::new({})", medm_str(b, file));
    let builders = vec![format!(
        ".with_size(egui::Vec2::new({}, {}))",
        float_lit(f64::from(geom.width)),
        float_lit(f64::from(geom.height))
    )];
    push_value_widget(b, z, geom, "RsdmImage", &new_call, &builders);
}

/// Emit a fieldless labelled placeholder (a red marker `ui.label`) at the MEDM
/// geometry plus a converter warning — for widgets RsDM cannot represent but
/// whose footprint should still be visible. Never a silent drop.
fn emit_marker_placeholder(
    b: &mut Builder,
    widget: &MedmWidget,
    z: ZLayer,
    label: &str,
    warn: &str,
) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let id = b.index();
    b.needs_color = true;
    b.placements.push(Placement::drawn(
        z,
        id,
        geom,
        format!(
            "ui.label(egui::RichText::new({}).color(Color32::from_rgb(180, 60, 60)));",
            rust_str(&format!("[{label}]"))
        ),
    ));
    b.warnings
        .push(format!("line {}: {warn}; placeholder emitted", widget.line));
}

/// `shell command` — a real control that runs MEDM shell commands. Each MEDM
/// `command[N]` carries a `label`, a `name` (the program), and optional `args`;
/// the executed string is `"<name> <args>"` (adl2pydm's `command_list`), spawned
/// via `sh -c` so shell syntax (pipes, redirection, background `&`) behaves as in
/// MEDM. A single command becomes a plain button; several become an
/// `egui::menu_button` listing each. The widget is channel-less and Engine-less,
/// so it is emitted inline in `ui()` with no struct field. It still layers
/// Foreground (the control z-layer), so the z-order rule holds.
fn emit_shell_command(b: &mut Builder, widget: &MedmWidget, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let entries = shell_command_entries(b, widget);
    if entries.is_empty() {
        emit_marker_placeholder(
            b,
            widget,
            z,
            "shell command (no commands)",
            "shell command has no runnable commands; nothing to spawn",
        );
        return;
    }

    let id = b.index();
    // MEDM captions the button from the widget's own `label` only; a label-less
    // shell command renders just MEDM's exclamation-mark icon
    // (medmShellCommand.c `renderShellCommandPixmap`).
    let caption = medm_button_caption(widget);
    if caption.is_none() {
        b.needs_sc_icon = true;
        b.needs_color = true;
    }
    let (icon_fg, _) = icon_color_exprs(widget);
    let body = if let [(_, command)] = entries.as_slice() {
        // Exactly one command: a plain button.
        match &caption {
            Some(label) => format!(
                "if ui.button({}).clicked() {{\n    {}\n}}",
                medm_str(b, label),
                spawn_command_stmt(b, command),
            ),
            None => format!(
                "let __r = ui.button(\"\");\nshell_command_icon(ui, __r.rect, {icon_fg});\nif __r.clicked() {{\n    {}\n}}",
                spawn_command_stmt(b, command),
            ),
        }
    } else {
        // Several commands: a menu whose items each run one command, then close;
        // the per-command labels caption only these menu items, never the button.
        let mut body = match &caption {
            Some(title) => format!("ui.menu_button({}, |ui| {{", medm_str(b, title)),
            None => "let __m = ui.menu_button(\"\", |ui| {".to_string(),
        };
        for (label, command) in &entries {
            let _ = write!(
                body,
                "\n    if ui.button({}).clicked() {{\n        {}\n        ui.close();\n    }}",
                medm_str(b, label),
                spawn_command_stmt(b, command),
            );
        }
        body.push_str("\n});");
        if caption.is_none() {
            let _ = write!(
                body,
                "\nshell_command_icon(ui, __m.response.rect, {icon_fg});"
            );
        }
        body
    };
    // MEDM draws the button/menu in the widget's `clr`/`bclr` with a height-sized
    // caption, filling its whole rect; the shared prelude applies the colours and
    // font to the scoped egui style and the justified wrap fills the geometry.
    let prelude = style_prelude(
        b,
        WidgetColors::from_widget(widget),
        Some(font_px_from_height(geom.height)),
    );
    let body = justified_body(&body);
    let body = format!("{{\n{prelude}    {}\n}}", body.replace('\n', "\n    "));
    b.placements.push(Placement::drawn(z, id, geom, body));
    b.warnings.push(format!(
        "line {}: shell command emitted as a live button/menu (spawns via `sh -c`)",
        widget.line
    ));
}

/// The `(label, command)` pairs for a shell-command widget: each `command[N]`'s
/// caption (its `label`, else the executed text) and executed string
/// `"<name> <args>"` (adl2pydm's `command_list`). A command with no `name` is
/// dropped with a warning; a command carrying MEDM's `%` argument prompt is kept
/// but warned (RsDM has no run-time argument-substitution dialog).
fn shell_command_entries(b: &mut Builder, widget: &MedmWidget) -> Vec<(String, String)> {
    let commands = widget
        .records
        .get("commands")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut entries = Vec::new();
    for spec in commands {
        let Some(name) = spec.get("name").filter(|s| !s.is_empty()) else {
            b.warnings.push(format!(
                "line {}: shell command entry has no name; skipped",
                widget.line
            ));
            continue;
        };
        let args = spec.get("args").map(String::as_str).unwrap_or("");
        let command = if args.is_empty() {
            name.clone()
        } else {
            format!("{name} {args}")
        };
        if command.contains('%') {
            b.warnings.push(format!(
                "line {}: shell command {command:?} uses MEDM `%` argument prompt; \
                 spawned verbatim (no run-time argument dialog)",
                widget.line
            ));
        }
        let label = spec
            .get("label")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| command.clone());
        entries.push((label, command));
    }
    entries
}

/// The statement that runs one command: `sh -c "<command>"`, detached (`spawn`,
/// not `status`) so the UI thread never blocks, with the child handle discarded
/// — MEDM's fire-and-forget shell execution.
fn spawn_command_stmt(b: &mut Builder, command: &str) -> String {
    format!(
        "let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg({}).spawn();",
        medm_str(b, command)
    )
}

/// The caption MEDM puts on a related-display / shell-command button, from the
/// widget's own `label` (medmRelatedDisplay.c / medmShellCommand.c apply
/// identical rules): an empty label is `None` — MEDM renders only the widget's
/// icon; a leading `-` is stripped — MEDM then suppresses the icon; any other
/// label is used as-is (MEDM draws icon + label; the icon is not reproduced
/// next to a labelled caption here). The per-entry `display[i]`/`command[i]`
/// labels caption menu *items* only, never the button itself.
fn medm_button_caption(widget: &MedmWidget) -> Option<String> {
    let label = widget
        .assignments
        .get("label")
        .map(String::as_str)
        .unwrap_or("");
    if label.is_empty() {
        return None;
    }
    Some(label.strip_prefix('-').unwrap_or(label).to_string())
}

/// The `(fg, bg)` colour expressions for a MEDM button icon: the widget's
/// `clr`/`bclr` when set, else the scoped egui text colour / button face — the
/// same colours MEDM passes its `render*Pixmap` helpers.
fn icon_color_exprs(widget: &MedmWidget) -> (String, String) {
    let colors = WidgetColors::from_widget(widget);
    (
        colors
            .fg
            .map(color_expr)
            .unwrap_or_else(|| "ui.visuals().text_color()".to_string()),
        colors
            .bg
            .map(color_expr)
            .unwrap_or_else(|| "ui.visuals().widgets.inactive.weak_bg_fill".to_string()),
    )
}

/// `embedded display` — inline the referenced screen at code-gen time. MEDM's
/// embedded display names another `.adl` (`"composite file"="file;macros"`) that
/// MEDM/PyDM load at run time; RsDM has no run-time display loader, so the
/// faithful analogue is to read that file *now*, convert it, and emit its widgets
/// into a `RsdmFrame` at the embedded geometry — the same inlining `composite`
/// uses, but sourced from an external file. Per MEDM `compositeFileParse`, a
/// non-empty `macros` string **replaces** the parent's table for the inlined
/// subtree (an empty one inherits it) — see [`merged_macros`] and
/// [`Builder::seal_macros`].
///
/// Inlining needs the source directory ([`Options::source_dir`]); without it, or
/// when the file is missing / forms an include cycle / exceeds
/// [`MAX_EMBED_DEPTH`], the widget falls back to a visible placeholder naming the
/// file (never a silent drop).
fn emit_embedded_display(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let Some((file, macros)) = embedded_file_and_macros(widget) else {
        emit_marker_placeholder(
            b,
            widget,
            z,
            "embedded display (no file)",
            "embedded display has no \"composite file\"; nothing to inline",
        );
        return;
    };

    let Some(dir) = options.source_dir.as_deref() else {
        embed_placeholder(b, widget, z, &file, "no source directory to resolve it");
        return;
    };
    let path = dir.join(&file);
    let Ok(canonical) = path.canonicalize() else {
        embed_placeholder(b, widget, z, &file, "file not found");
        return;
    };
    if b.embed_stack.contains(&canonical) {
        embed_placeholder(b, widget, z, &file, "include cycle");
        return;
    }
    if b.embed_stack.len() >= MAX_EMBED_DEPTH {
        embed_placeholder(b, widget, z, &file, "max embed depth reached");
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            embed_placeholder(b, widget, z, &file, &format!("cannot read: {e}"));
            return;
        }
    };

    let mut target = parse_in_dir(&text, canonical.parent());
    // Resolve the target's channels in the embedded directory (so a nested
    // embedded display resolves relative to *its* file). Per MEDM
    // `compositeFileParse`, a non-empty macro string on the `composite file`
    // replaces the macro table (parent macros are dropped for this subtree);
    // an empty one inherits the parent's (see [`merged_macros`]).
    let child_options = Options {
        macros: merged_macros(&macros, &options.macros),
        source_dir: canonical.parent().map(PathBuf::from),
        ..options.clone()
    };
    // Same parse→emit boundary as `generate`: bake this display's macros into its
    // subtree before inlining (MEDM expands macros per display; the table is the
    // embedded-or-inherited one resolved by `merged_macros`).
    expand_macros(&mut target.widgets, &child_options.macros);
    let addr = b.synthetic_addr("embed");

    b.embed_stack.push(canonical);
    // A non-empty `;macros` string replaced the child's macro table (MEDM
    // `compositeFileParse`): the parent's macros are out of scope for this
    // subtree, so any `$(name)` that survived the replace table must stay
    // literal rather than expand against the runtime `__m` (see
    // [`Builder::seal_macros`]). Sealed for the whole subtree — including nested
    // inherit-includes — and restored to the prior value on the way out (a
    // replace-include nested inside another stays sealed regardless).
    let prev_seal = b.seal_macros;
    b.seal_macros = prev_seal || !macros.trim().is_empty();
    // MEDM `compositeFileParse` (`medmComposite.c:709-736`) refits the composite
    // to its contents after parsing: it takes the children's bounding box
    // (min/max of each element's `x,y,x+w,y+h` — the skipped file/display/colormap
    // blocks are not in the list, exactly as they are absent from `target.widgets`),
    // sets the composite's size to `(maxX-minX, maxY-minY)`, and moves every child
    // by `(oldX-minX, oldY-minY)` so the content's top-left lands at the
    // composite's WRITTEN `x,y`. So children are measured from the bbox min (not
    // the child.adl's file origin) and the frame takes the content size, not the
    // stale `.adl` display geometry. A childless include keeps the written geometry.
    let (frame_geom, child_origin) = match content_bbox(&target.widgets) {
        Some((min_x, min_y, max_x, max_y)) => (
            Geometry {
                x: geom.x,
                y: geom.y,
                width: max_x - min_x,
                height: max_y - min_y,
            },
            (min_x, min_y),
        ),
        None => (geom, (0, 0)),
    };
    emit_frame_container(
        b,
        z,
        frame_geom,
        &addr,
        true,
        &format!("adl2rsdm: connect {addr} (embedded {file})"),
        &target.widgets,
        child_origin,
        &child_options,
    );
    b.seal_macros = prev_seal;
    b.embed_stack.pop();
    b.warnings.push(format!(
        "line {}: embedded display inlined {file} ({} widget(s))",
        widget.line,
        target.widgets.len()
    ));
}

/// The `(file, macros)` of an embedded display's `"composite file"`, which MEDM
/// stores as `file` or `file;macros` (semicolon-delimited, adl2pydm's
/// `split(";")`). `None` when there is no non-empty `composite file`.
fn embedded_file_and_macros(widget: &MedmWidget) -> Option<(String, String)> {
    let spec = widget
        .assignments
        .get("composite file")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    match spec.split_once(';') {
        Some((file, macros)) => Some((file.trim().to_string(), macros.trim().to_string())),
        None => Some((spec.to_string(), String::new())),
    }
}

/// The bounding box `(min_x, min_y, max_x, max_y)` over the widgets that carry a
/// geometry, matching MEDM's composite-refit loop (`medmComposite.c:710-726`:
/// min/max of each element's `x`, `y`, `x+width`, `y+height`). `None` when no
/// widget has a geometry (an empty include — nothing to refit).
fn content_bbox(widgets: &[MedmWidget]) -> Option<(i32, i32, i32, i32)> {
    let mut it = widgets.iter().filter_map(|w| w.geometry);
    let first = it.next()?;
    let (mut min_x, mut min_y) = (first.x, first.y);
    let (mut max_x, mut max_y) = (first.x + first.width, first.y + first.height);
    for g in it {
        min_x = min_x.min(g.x);
        min_y = min_y.min(g.y);
        max_x = max_x.max(g.x + g.width);
        max_y = max_y.max(g.y + g.height);
    }
    Some((min_x, min_y, max_x, max_y))
}

/// Parse an embedded display's macro string (`"A=1,B=2"`) into pairs, dropping
/// entries with no `=` or an empty name.
fn parse_embedded_macros(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|kv| {
            let (name, value) = kv.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The macros for an inlined subtree, matching MEDM `compositeFileParse`
/// (`medmComposite.c:659-668`): a **non-empty** macro string **replaces** the
/// macro table — the parent's macros are *not* consulted while parsing the
/// included file — while an **empty** macro string keeps ("uses the existing")
/// the parent's macros. So `child.adl;M=2` sees only `M=2` (a `$(P)` in the
/// child stays literal, as in MEDM), whereas `child.adl` inherits the parent's.
fn merged_macros(embedded: &str, parent: &[(String, String)]) -> Vec<(String, String)> {
    if embedded.trim().is_empty() {
        parent.to_vec()
    } else {
        parse_embedded_macros(embedded)
    }
}

/// A visible placeholder for an embedded display that could not be inlined (no
/// source dir, missing file, cycle, or depth limit): a red marker naming the
/// file and the reason, plus a warning. Never a silent drop.
fn embed_placeholder(b: &mut Builder, widget: &MedmWidget, z: ZLayer, file: &str, reason: &str) {
    emit_marker_placeholder(
        b,
        widget,
        z,
        &format!("embedded: {file}"),
        &format!("embedded display {file:?} not inlined ({reason})"),
    );
}

/// `related display` — a real control that reports the screen(s) it would open.
/// RsDM has no runtime display loader (a project-level deferral), so the button
/// cannot swap the host app's screen; the faithful in-scope behaviour is a live,
/// enabled control that logs the target on click instead of an inert disabled
/// placeholder. One target becomes a plain button; several become an
/// `egui::menu_button` listing each. Channel-less and Engine-less, so it is
/// emitted inline at the Foreground z-layer (never occluded).
fn emit_related_display(b: &mut Builder, widget: &MedmWidget, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let entries = related_display_entries(b, widget);
    if entries.is_empty() {
        emit_marker_placeholder(
            b,
            widget,
            z,
            "related display (no targets)",
            "related display has no target displays; nothing to open",
        );
        return;
    }

    let id = b.index();
    // MEDM selects the rendering by `iNumberOfDisplays` — the count of entries
    // with a non-empty **label**, not the count of named targets
    // (medmRelatedDisplay.c:235-243). `<= 1` (and not a hidden button) is "Case 1
    // of 4": a single plain button opening the first named target, even when more
    // named targets follow (they are unreachable, exactly as MEDM leaves them).
    // Only `>= 2` reaches a multi-target row/column/menu.
    let labeled_count = related_display_labeled_count(widget);
    match related_display_visual(b, widget) {
        RdVisual::Hidden => {
            emit_hidden_related_display(b, widget, z, geom, id, &entries);
            return;
        }
        v @ (RdVisual::Row | RdVisual::Col) if labeled_count >= 2 => {
            emit_related_display_buttons(b, widget, z, geom, id, &entries, v);
            return;
        }
        _ => {}
    }
    // MEDM captions the button from the widget's own `label` only; a label-less
    // related display renders just MEDM's overlapping-frames icon
    // (medmRelatedDisplay.c `renderRelatedDisplayPixmap`).
    let caption = medm_button_caption(widget);
    if caption.is_none() {
        b.needs_rd_icon = true;
        b.needs_color = true;
    }
    let (icon_fg, icon_bg) = icon_color_exprs(widget);
    let body = if labeled_count <= 1 {
        // MEDM "Case 1 of 4" (medmRelatedDisplay.c:242-243, :302-309): a single
        // plain button opening the *first* named target. Any further named targets
        // are unreachable, exactly as MEDM leaves them. A hover tooltip names the
        // target so it is discoverable in the GUI; adl2pydm likewise gives the
        // button a tooltip. `entries` is non-empty (checked above), so [0] is safe.
        let entry = &entries[0];
        let (hover, click) = rd_click(b, widget.line, entry);
        match &caption {
            Some(label) => format!(
                "if ui.button({}).on_hover_text({hover}).clicked() {{\n{}\n}}",
                medm_str(b, label),
                indent_lines(&click, 4),
            ),
            None => format!(
                "let __r = ui.button(\"\").on_hover_text({hover});\nrelated_display_icon(ui, __r.rect, {icon_fg}, {icon_bg});\nif __r.clicked() {{\n{}\n}}",
                indent_lines(&click, 4),
            ),
        }
    } else {
        // Several targets: a menu whose items each open (or report) one target,
        // then close. Each item carries a hover tooltip naming its target
        // (GUI-discoverable); the per-target labels caption only these menu
        // items, never the button.
        let mut body = match &caption {
            Some(title) => format!("ui.menu_button({}, |ui| {{", medm_str(b, title)),
            None => "let __rd_menu = ui.menu_button(\"\", |ui| {".to_string(),
        };
        for entry in &entries {
            let (hover, click) = rd_click(b, widget.line, entry);
            let _ = write!(
                body,
                "\n    if ui.button({}).on_hover_text({hover}).clicked() {{\n{}\n        ui.close();\n    }}",
                medm_str(b, &entry.caption),
                indent_lines(&click, 8),
            );
        }
        body.push_str("\n});");
        if caption.is_none() {
            let _ = write!(
                body,
                "\nrelated_display_icon(ui, __rd_menu.response.rect, {icon_fg}, {icon_bg});"
            );
        }
        body
    };
    // MEDM draws the button/menu in the widget's `clr`/`bclr` with a height-sized
    // caption, filling its whole rect; the shared prelude applies the colours and
    // font to the scoped egui style and the justified wrap fills the geometry.
    let prelude = style_prelude(
        b,
        WidgetColors::from_widget(widget),
        Some(font_px_from_height(geom.height)),
    );
    let body = justified_body(&body);
    let body = format!("{{\n{prelude}    {}\n}}", body.replace('\n', "\n    "));
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// One `display[N]` entry of a related display.
struct RdEntry {
    /// The button/menu-item caption (the entry's `label`, else its `name`).
    caption: String,
    /// The target `.adl` file name as written (post convert-time baking).
    name: String,
    /// The macro args handed to the child instance (MEDM `args`).
    args: String,
    /// MEDM `mode="replace display"` — MEDM reuses the parent's shell; RsDM
    /// opens a new window instead (deviation warned at emission).
    replace: bool,
}

/// MEDM related-display `visual` (`displayList.h:306-309`, parsed at
/// `medmRelatedDisplay.c:728-739`). `RD_MENU` is the default (any unrecognized
/// token stays menu, like MEDM).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RdVisual {
    /// `"menu"`: one button for a single target, a dropdown menu for many.
    Menu,
    /// `"a row of buttons"`: N side-by-side buttons (each width = geom.width/N).
    Row,
    /// `"a column of buttons"`: N stacked buttons (each height = geom.height/N).
    Col,
    /// `"invisible"`: no widget — a transparent hotspot over the graphic whose
    /// click opens the first target (`RD_HIDDEN_BTN`).
    Hidden,
}

/// Parse a related display's `visual` key (`medmRelatedDisplay.c:728-739`). MEDM
/// leaves the default `RD_MENU` for `"menu"`, an absent key, or anything it does
/// not recognize; adl2rsdm additionally warns on an unrecognized token.
fn related_display_visual(b: &mut Builder, widget: &MedmWidget) -> RdVisual {
    match widget.assignments.get("visual").map(String::as_str) {
        Some("a row of buttons") => RdVisual::Row,
        Some("a column of buttons") => RdVisual::Col,
        Some("invisible") => RdVisual::Hidden,
        Some("menu") | None => RdVisual::Menu,
        Some(other) => {
            b.warnings.push(format!(
                "line {}: related display visual {other:?} unrecognized, using \"menu\"",
                widget.line
            ));
            RdVisual::Menu
        }
    }
}

/// MEDM `RD_HIDDEN_BTN` (`medmRelatedDisplay.c:562-593`): no widget is drawn (MEDM
/// stipples the underlying graphic), and a Button-1 click opens the *first* target
/// directly (`eventHandlers.c:228-251`), never a menu. Emit a transparent
/// clickable area over the geometry — no fill, so the graphic beneath shows
/// through — that opens `entries[0]` on click.
fn emit_hidden_related_display(
    b: &mut Builder,
    widget: &MedmWidget,
    z: ZLayer,
    geom: Geometry,
    id: u64,
    entries: &[RdEntry],
) {
    let (hover, click) = rd_click(b, widget.line, &entries[0]);
    let inner = format!(
        "let __rect = ui.max_rect();\nlet __r = ui.allocate_rect(__rect, egui::Sense::click()).on_hover_text({hover});\nif __r.clicked() {{\n{}\n}}",
        indent_lines(&click, 4),
    );
    let body = format!("{{\n    {}\n}}", inner.replace('\n', "\n    "));
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// MEDM `RD_ROW_OF_BTN` / `RD_COL_OF_BTN` (`medmRelatedDisplay.c:461-561`): a
/// RowColumn of N equal-cell push buttons — a row lays them left-to-right (each
/// width = `width/N`), a column top-to-bottom (each height = `height/N`), with
/// `XmNrecomputeSize FALSE` so the cells are equal. Each button opens its own
/// target and is captioned by that display's `label`. Colours come from the
/// widget's `clr`/`bclr` via the shared prelude, the same as the menu path.
fn emit_related_display_buttons(
    b: &mut Builder,
    widget: &MedmWidget,
    z: ZLayer,
    geom: Geometry,
    id: u64,
    entries: &[RdEntry],
    visual: RdVisual,
) {
    let n = entries.len();
    let row = visual == RdVisual::Row;
    // MEDM sizes each button's font to its cell height: a row keeps the full
    // height, a column splits it among the N cells (medmRelatedDisplay.c:497-508,
    // relatedDisplayFontListIndex).
    let font_px = if row {
        font_px_from_height(geom.height)
    } else {
        font_px_from_fractional_height(f64::from(geom.height) / n as f64)
    };
    let cell = if row {
        "egui::Rect::from_min_size(__rect.min + egui::vec2(__i as f32 * __rect.width() / __n, 0.0), egui::vec2(__rect.width() / __n, __rect.height()))"
    } else {
        "egui::Rect::from_min_size(__rect.min + egui::vec2(0.0, __i as f32 * __rect.height() / __n), egui::vec2(__rect.width(), __rect.height() / __n))"
    };
    let mut inner = String::from(
        "let __rect = ui.max_rect();\nlet __sp = ui.spacing_mut();\n__sp.interact_size = egui::Vec2::ZERO;\n__sp.button_padding = egui::Vec2::ZERO;\n",
    );
    // A plain `{n}f32` float literal — not `{n} as f32`, which trips
    // `clippy::unnecessary_cast` when the generated module is linted.
    let _ = write!(inner, "let __n = {n}f32;");
    for (i, entry) in entries.iter().enumerate() {
        let (hover, click) = rd_click(b, widget.line, entry);
        let _ = write!(
            inner,
            "\n{{\n    let __i = {i};\n    let __cell = {cell};\n    if ui.put(__cell, egui::Button::new({})).on_hover_text({hover}).clicked() {{\n{}\n    }}\n}}",
            medm_str(b, &entry.caption),
            indent_lines(&click, 8),
        );
    }
    let prelude = style_prelude(b, WidgetColors::from_widget(widget), Some(font_px));
    let body = format!("{{\n{prelude}    {}\n}}", inner.replace('\n', "\n    "));
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// The hover-text *expression* and click statements for one related-display
/// target. When the recursive driver converted the target ([`Builder`]'s
/// `rd_modules`), a click opens the sibling module's screen in an immediate
/// viewport — runtime-expanding the entry's `args` against the parent's macro
/// table, focusing an already-open (module, args) window instead of duplicating
/// it (MEDM `relatedDisplayCreateNewDisplay` + `popupExistingDisplay`).
/// Otherwise (single-file convert, missing file, macro-bearing name) the click
/// keeps the report-only behaviour: it logs the target to stderr.
fn rd_click(b: &mut Builder, line: usize, e: &RdEntry) -> (String, String) {
    let Some(m) = b.rd_modules.get(&e.name).cloned() else {
        let report = if e.args.is_empty() {
            format!("related display: open {}", e.name)
        } else {
            format!("related display: open {} (macros: {})", e.name, e.args)
        };
        b.warnings.push(format!(
            "line {line}: related display target {} was not converted alongside this \
             screen (RsDM has no runtime display loader; click logs the target)",
            e.name
        ));
        return (rust_str(&report), eprintln_literal(&report));
    };
    b.needs_rd_open = true;
    if e.replace {
        b.warnings.push(format!(
            "line {line}: related display mode \"replace display\" opens a new window \
             (RsDM keeps the parent open; MEDM would reuse its shell)"
        ));
    }
    let hover = if e.args.is_empty() {
        format!("open {}", e.name)
    } else {
        format!("open {} (macros: {})", e.name, e.args)
    };
    let hover = medm_str(b, &hover);
    // The (module, args) dedup key and the child's macro table both come from
    // the args string; macro references in it resolve against the *parent*
    // instance's table at click time (MEDM `performMacroSubstitutions`,
    // utils.c:3444-3459). Unlike the child-string path (`medm_str`/`expand`), an
    // undefined `$(name)` here is *dropped*, not left literal — hence the separate
    // `expand_args` method, so `args="P=$(X)"` with X unbound yields `P=` (as MEDM
    // does), never `P=$(X)`.
    let args_expr = if has_macro_ref(&e.args) && !b.seal_macros {
        b.needs_macros = true;
        b.needs_macro_args = true;
        format!("__m.expand_args({})", rust_str(&e.args))
    } else if e.args.is_empty() {
        "String::new()".to_string()
    } else {
        // Grounded, or sealed inside a replaced composite-file table (see
        // [`medm_str`]): a surviving `$(name)` stays literal in the click-time
        // args string, matching MEDM's out-of-scope-parent behaviour.
        format!("{}.to_string()", rust_str(&e.args))
    };
    let p = b.rt_prefix();
    let path = match &m.ident {
        Some(ident) => format!("{p}{ident}::Screen"),
        None => format!("{p}Screen"),
    };
    let click = format!(
        "let __rd_ctx = ui.ctx().clone();\nlet __rd_args = {args_expr};\n{p}OpenDisplay::open_or_focus(__open, &__rd_ctx, ({}, __rd_args.clone()), {}, egui::vec2({}, {}), || {{\n    Box::new({path}::new_in(&__rd_ctx, __rs.as_ref(), {p}parse_macro_args(&__rd_args)))\n}});",
        rust_str(m.ident.as_deref().unwrap_or("")),
        rust_str(&m.title),
        float_lit(m.width),
        float_lit(m.height),
    );
    (hover, click)
}

/// Re-indent every line of `s` by `n` spaces (multi-line click bodies nested
/// inside an emitted `if`/menu item).
fn indent_lines(s: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    s.lines()
        .map(|l| format!("{pad}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `display[N]` entries of a related display. The target `name` and `args`
/// already had their `$(P)` macros expanded by the convert-time IR pass
/// ([`expand_macros`]); whatever survives expands at runtime. A target with no
/// `name` is dropped with a warning (nothing to open). Every seen name is also
/// recorded as a discovery target for the recursive driver.
fn related_display_entries(b: &mut Builder, widget: &MedmWidget) -> Vec<RdEntry> {
    let displays = widget
        .records
        .get("displays")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut entries = Vec::new();
    for spec in displays {
        let Some(name) = spec.get("name").filter(|s| !s.is_empty()).cloned() else {
            b.warnings.push(format!(
                "line {}: related display entry has no name; skipped",
                widget.line
            ));
            continue;
        };
        let args = spec.get("args").cloned().unwrap_or_default();
        // MEDM's per-entry replace flag is the `policy` key with value
        // "replace display" (medmRelatedDisplay.c:666-671, stringValueTable
        // [REPLACE_DISPLAY]); the file format has no `mode` key — that is MEDM's
        // internal field name. Reading `mode` here meant `replace` was never
        // detected and the replace-mode deviation warning could never fire.
        let replace = spec.get("policy").is_some_and(|p| p == "replace display");
        let caption = spec
            .get("label")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| name.clone());
        b.related_targets.push(name.clone());
        entries.push(RdEntry {
            caption,
            name,
            args,
            replace,
        });
    }
    entries
}

/// MEDM `iNumberOfDisplays` (`medmRelatedDisplay.c:235-240`): the number of
/// `display[N]` entries with a non-empty **label**. This — not the count of
/// name-bearing targets — is what selects MEDM's rendering: `<= 1` (and not a
/// hidden button) is "Case 1 of 4", a single plain button opening the first
/// name-bearing target (`:242-243`, `:302-309`); `>= 2` is the menu / row /
/// column. It is counted over the raw `display` records (a label-only, name-less
/// entry still counts toward the gate in MEDM), so it can differ from
/// `related_display_entries().len()`, which keeps only the openable name-bearing
/// targets.
fn related_display_labeled_count(widget: &MedmWidget) -> usize {
    widget
        .records
        .get("displays")
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|spec| spec.get("label").is_some_and(|s| !s.is_empty()))
        .count()
}

/// An `eprintln!` statement that prints `msg` verbatim: `msg` is the sole format
/// string with its `{`/`}` doubled, so there are no `{}` placeholders to fill
/// (clippy-clean — a lone literal format string, no trailing args).
fn eprintln_literal(msg: &str) -> String {
    let escaped = msg.replace('{', "{{").replace('}', "}}");
    format!("eprintln!({});", rust_str(&escaped))
}

/// Resolve the geometry and channel address common to every channel-bound
/// widget, recording the matching skip warning and returning `None` if either is
/// absent.
fn resolve_channel(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
) -> Option<(Geometry, String)> {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return None;
    };
    let Some(addr) = channel_address(widget, options) else {
        skip_no_channel(b, widget);
        return None;
    };
    Some((geom, addr))
}

/// A value/control widget's static MEDM colours: `clr` (foreground/text) and
/// `bclr` (background). Applied for the widgets whose `clr`/`bclr` genuinely mean
/// "text colour / fill" (label, line edit, push button, combo box, enum button,
/// spinbox — all render their text through `override_text_color`); NOT for shapes
/// (which colour themselves through drawing builders), the slider (whose `clr` is
/// a track/handle colour `override_text_color` cannot reach), or byte/scale
/// widgets (whose `clr`/`bclr` are on/off and bar/background colours with their
/// own rendering).
#[derive(Clone, Copy, Default)]
struct WidgetColors {
    /// MEDM `clr` — the foreground/text colour.
    fg: Option<Color>,
    /// MEDM `bclr` — the background fill.
    bg: Option<Color>,
}

impl WidgetColors {
    /// The widget's resolved `clr`/`bclr` (the parser folds attribute-block
    /// colours into `widget.color`/`background_color`).
    fn from_widget(widget: &MedmWidget) -> Self {
        Self {
            fg: widget.color,
            bg: widget.background_color,
        }
    }
}

/// MEDM auto-sizes a widget's font to its geometry; adl2pydm's `write_font_size`
/// reproduces it as `pointsize = clamp(round(height * 0.6), 6, 20)`. Returns that
/// size (in egui points, which track the glyph pixel height) for a text-bearing
/// widget of the given `height`.
fn font_px_from_height(height: i32) -> f32 {
    font_px_from_fractional_height(f64::from(height))
}

/// [`font_px_from_height`] over a fractional height — a per-button share of a
/// stacked widget's geometry (adl2pydm `write_font_size(height_override=…)`).
fn font_px_from_fractional_height(height: f64) -> f32 {
    (height * 0.6).round().clamp(6.0, 20.0) as f32
}

/// The scoped style-override lines applying a MEDM-derived font size and static
/// `clr`/`bclr` colours — the single owner of "how MEDM colours reach an egui
/// widget", shared by every emitter that styles a drawn body (channel widgets,
/// static text, related-display/shell-command buttons). Empty when nothing is
/// set; otherwise each line is indented one level for splicing into a `{ ... }`
/// block.
///
/// The font is set as `override_font_id` (egui resolves Label/Button/edit text
/// against it before falling back to `TextStyle::Body`). `bclr` is painted as a
/// filled rect over the widget's full MEDM geometry AND set as the face fill of
/// every self-painting widget — `weak_bg_fill` per interact state (Button /
/// DragValue / ComboBox faces) and `text_edit_bg_color` (TextEdit face) — since
/// those widgets paint their own face over the backing rect (MEDM draws the
/// whole control in `bclr`). `clr` is set as `override_text_color`, which the
/// widget's text honours unless it is alarm-driven (alarm colouring sets the
/// text colour explicitly and so still wins, matching MEDM `clrmod="alarm"`
/// overriding the static `clr`).
fn style_prelude(b: &mut Builder, colors: WidgetColors, font_px: Option<f32>) -> String {
    let mut lines = String::new();
    if let Some(px) = font_px {
        // In responsive layout the rects scale by (sx, sy); the font follows the
        // height scale `sy` — MEDM likewise re-derives fonts from the resized
        // widget height (`font_px_from_height` is a pure height function).
        let scale = if b.use_layout { " * sy" } else { "" };
        let _ = writeln!(
            lines,
            "    ui.style_mut().override_font_id = Some(egui::FontId::proportional({}{scale}));",
            float_lit(f64::from(px))
        );
    }
    if let Some(bg) = colors.bg {
        b.needs_color = true;
        let bg = color_expr(bg);
        let _ = writeln!(lines, "    let __bg = ui.max_rect();");
        let _ = writeln!(
            lines,
            "    ui.painter().rect_filled(__bg, egui::CornerRadius::ZERO, {bg});"
        );
        let _ = writeln!(lines, "    let __v = &mut ui.style_mut().visuals;");
        let _ = writeln!(lines, "    __v.widgets.inactive.weak_bg_fill = {bg};");
        let _ = writeln!(lines, "    __v.widgets.hovered.weak_bg_fill = {bg};");
        let _ = writeln!(lines, "    __v.widgets.active.weak_bg_fill = {bg};");
        let _ = writeln!(lines, "    __v.widgets.open.weak_bg_fill = {bg};");
        let _ = writeln!(lines, "    __v.text_edit_bg_color = Some({bg});");
        if let Some(fg) = colors.fg {
            b.needs_color = true;
            let _ = writeln!(
                lines,
                "    __v.override_text_color = Some({});",
                color_expr(fg)
            );
        }
    } else if let Some(fg) = colors.fg {
        b.needs_color = true;
        let _ = writeln!(
            lines,
            "    ui.style_mut().visuals.override_text_color = Some({});",
            color_expr(fg)
        );
    }
    lines
}

/// Wrap a widget draw in a centered-and-justified layout so it fills the whole
/// `place()` rect — MEDM semantics, where a widget IS its geometry. egui widgets
/// otherwise size to content: a 59x20 message button renders as a small
/// content-sized button inside the rect, leaving dead (unclickable) zones around
/// it and a visible face-vs-background seam. Justified allocation expands every
/// widget — buttons/edits/combos via `allocate_space`, drawings/byte/scale via
/// `allocate_exact_size` — to the full rect, which also makes them track the
/// responsive (`--use-layout`) scale; child uis (e.g. the alarm-border frame
/// inside each rsdm widget) inherit the layout, so the fill reaches the inner
/// widget.
///
/// The wrap also zeroes egui's size floors: a Motif widget has no minimum size
/// or margins, but egui buttons floor their height at `interact_size.y` (18),
/// so a bare `ui.button` in a shorter MEDM cell (related display / shell
/// command) overflowed downward and its caption rode the bottom clip edge.
fn justified_body(stmt: &str) -> String {
    format!(
        "ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {{\n    let spacing = ui.spacing_mut();\n    spacing.interact_size = egui::Vec2::ZERO;\n    spacing.button_padding = egui::Vec2::ZERO;\n    {}\n}});",
        stmt.replace('\n', "\n    ")
    )
}

/// The draw body for a channel widget: the [`style_prelude`] overrides (when any)
/// wrapped around a rect-filling `field.show(ui)`.
fn styled_show_body(
    b: &mut Builder,
    field: &str,
    colors: WidgetColors,
    font_px: Option<f32>,
) -> String {
    let prelude = style_prelude(b, colors, font_px);
    let show = justified_body(&format!("let _ = {field}.show(ui);"));
    if prelude.is_empty() {
        return show;
    }
    format!("{{\n{prelude}    {}\n}}", show.replace('\n', "\n    "))
}

/// The per-widget inputs to [`push_channel_widget`]: how to name, construct,
/// configure, and colour one channel-bound widget. Grouped into one spec so the
/// emitter stays under the argument-count lint while `b`/`z`/`geom` remain the
/// separate placement context.
struct ChannelWidget<'a> {
    /// The rsdm widget type (the `Screen` field's type).
    ty: &'a str,
    /// The `Type::new(...)` constructor call.
    new_call: &'a str,
    /// The `.expect(...)` connection-failure message.
    connect_desc: &'a str,
    /// `.with_*` builder calls applied after construction.
    builders: &'a [String],
    /// Static MEDM `clr`/`bclr` colours; `default()` (none) for widgets that
    /// colour themselves or have no text/fill semantics.
    colors: WidgetColors,
    /// MEDM height-derived font size (`Some` for text-bearing widgets — label,
    /// line edit, push button, combo box, enum button; `None` for the rest).
    font_px: Option<f32>,
}

/// Emit a stateful, channel-bound widget: store it as a `Screen` field, build it
/// in `new()` (`new_call.expect(connect_desc)` then the `.with_*` `builders`),
/// and draw it back-to-front in `ui()`. The single owner of channel-widget
/// emission, so every widget is placed and drawn the same way.
fn push_channel_widget(b: &mut Builder, z: ZLayer, geom: Geometry, w: ChannelWidget) {
    let ChannelWidget {
        ty,
        new_call,
        connect_desc,
        builders,
        colors,
        font_px,
    } = w;
    let id = b.index();
    let field = format!("w{id}");
    b.needs_widgets = true;

    let mut ctor = format!(
        "let {field} = {new_call}\n            .expect({})",
        rust_str(connect_desc)
    );
    // MEDM draws no alarm-severity border, so every framed widget keeps only
    // the RsDM disconnect dash (PyDM's default would ring Minor/Major/Invalid).
    // A RsdmDrawing has no framed border — its border flag recolours the
    // shape's own pen (set by drawing_alarm_builder), so it is left alone.
    if ty != "RsdmDrawing" {
        let _ = write!(
            ctor,
            "\n            .with_border_mode(BorderMode::DisconnectedOnly)"
        );
    }
    for bld in builders {
        let _ = write!(ctor, "\n            {bld}");
    }
    ctor.push(';');

    b.ctors.push(ctor);
    b.fields.push((field.clone(), ty.to_string()));
    // The body references the field's `&mut` local (bound by `ui()`'s `let Self {
    // .. }` destructure), not `self.field`, so a container's draw closure can hold
    // disjoint borrows of the frame and its siblings.
    let body = styled_show_body(b, &field, colors, font_px);
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// Like [`push_channel_widget`] but for a fielded widget whose constructor is
/// infallible and takes no `&engine` — e.g. a channel-less `RsdmImage`. Emits
/// `let wN = <new_call><builders>;` (no `.expect`) plus its `show(ui)` placement.
fn push_value_widget(
    b: &mut Builder,
    z: ZLayer,
    geom: Geometry,
    ty: &str,
    new_call: &str,
    builders: &[String],
) {
    let id = b.index();
    let field = format!("w{id}");
    b.needs_widgets = true;

    let mut ctor = format!("let {field} = {new_call}");
    for bld in builders {
        let _ = write!(ctor, "\n            {bld}");
    }
    ctor.push(';');

    b.ctors.push(ctor);
    b.fields.push((field.clone(), ty.to_string()));
    b.placements.push(Placement::drawn(
        z,
        id,
        geom,
        justified_body(&format!("let _ = {field}.show(ui);")),
    ));
}

/// A `.with_precision(n)` builder from a widget's `limits` block, or `None` when
/// precision is channel-sourced. MEDM applies the limits-block precision only
/// when `precSrc == PV_LIMITS_DEFAULT` (`medmTextUpdate.c:495-497`); otherwise
/// precision tracks the channel's PREC at runtime. `precDefault` defaults to
/// `PREC_DEFAULT` (0) when the key is omitted (`medmWidget.h:57`), and MEDM
/// writes a non-zero `precDefault` even when `precSrc` stays channel
/// (`medmCommon.c:665`), so a bare `precDefault` without `precSrc="default"` is a
/// leftover MEDM ignores — pinning it here would freeze precision where MEDM
/// tracks the PV.
fn precision_default_builder(widget: &MedmWidget) -> Option<String> {
    if widget.assignments.get("precSrc").map(String::as_str) != Some("default") {
        return None; // channel-sourced precision — do not pin
    }
    let n = widget
        .assignments
        .get("precDefault")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0); // PREC_DEFAULT
    Some(format!(".with_precision({n})"))
}

/// A `.with_format(DisplayFormat::…)` builder from a text-update / text-entry
/// widget's MEDM `format` key (and the `$`-suffix long-string convention). MEDM's
/// runtime renders `exponential` and `hexadecimal` in those representations
/// (`medmTextUpdate.c:300-345`), which rsdm exposes as `DisplayFormat::Exponential`
/// / `Hex`; `string` decodes a CHAR waveform. `decimal` and an absent key are the
/// fixed-point default (rsdm's `DisplayFormat::Default`), so they emit nothing.
/// The remaining MEDM formats (`engr. notation`, `compact`, `truncated`, `octal`,
/// `sexagesimal*`) have no rsdm surface and are WARNED, not silently dropped —
/// unlike `adl2pydm`, which maps only `string`.
fn string_format_builder(b: &mut Builder, widget: &MedmWidget, addr: &str) -> Option<String> {
    // A `$`-suffixed PV is MEDM's CHAR-waveform-as-string convention regardless
    // of the `format` key (adl2pydm output_handler.py:266-267).
    if addr.ends_with('$') {
        return Some(".with_format(DisplayFormat::String)".to_string());
    }
    // MEDM `format` tokens (displayList.h stringValueTable[22..32], plus the
    // backward-compat aliases parsed in medmTextUpdate.c:581-600). Absent `format`
    // is the fixed-point default, which rsdm's `DisplayFormat::Default` already
    // renders — so no builder and no warning.
    let variant = match widget.assignments.get("format")?.as_str() {
        "string" => "String",
        "exponential" | "decimal- exponential notation" => "Exponential",
        "hexadecimal" | "hexidecimal" => "Hex",
        // MEDM's plain fixed-point; identical to rsdm's default rendering.
        "decimal" => return None,
        // No rsdm surface: `engr. notation`, `compact`, `truncated`, `octal`,
        // `sexagesimal`/`-hms`/`-dms`. Never a silent drop — warn and fall back to
        // the fixed-point default.
        other => {
            b.warnings.push(format!(
                "line {}: text format {other:?} has no rsdm equivalent; rendered as \
                 the fixed-point default",
                widget.line
            ));
            return None;
        }
    };
    Some(format!(".with_format(DisplayFormat::{variant})"))
}

/// A `.with_alarm_sensitive_content(true)` builder when MEDM `clrmod="alarm"` —
/// the widget's foreground colour follows alarm severity instead of its static
/// `clr`. MEDM's other modes (`static`, the default, and `discrete`) keep the
/// static colour and emit nothing. `adl2pydm` leaves this to PyDM's widget
/// defaults; rsdm defaults `alarm_sensitive_content` off, so reproducing MEDM's
/// alarm colouring needs the builder set explicitly. The MEDM palette rides
/// along: MEDM's `alarmColor` table replaces the foreground for EVERY severity
/// (`NO_ALARM` paints Green3 — medmWidget.c `alarmColorString`, utils.c
/// `alarmColor`), where PyDM keeps the static colour outside an alarm. Every
/// widget this is applied to exposes `with_alarm_sensitive_content` +
/// `with_alarm_palette`: the MONITOR widgets (`RsdmLabel`, `RsdmDrawing`,
/// `RsdmScaleIndicator`, `RsdmByteIndicator`) and, since R2-67, the CONTROLLERS
/// (`RsdmLineEdit`, `RsdmPushButton`, `RsdmEnumComboBox`, `RsdmEnumButton`,
/// `RsdmSlider`, `RsdmSpinbox`) — MEDM alarm-colours the control foreground the
/// same way (medmTextEntry.c:418-424, medmMessageButton.c:348, medmMenu.c:540,
/// medmChoiceButtons.c:375, medmValuator.c:892-895, medmWheelSwitch.c:390). The
/// valuator ships the flag ON, so it uses [`valuator_alarm_builder`] instead.
fn alarm_content_builder(widget: &MedmWidget) -> Option<String> {
    (widget.assignments.get("clrmod").map(String::as_str) == Some("alarm")).then(|| {
        ".with_alarm_sensitive_content(true)\n            \
         .with_alarm_palette(AlarmPalette::Medm)"
            .to_string()
    })
}

/// Alarm-content builder for the valuator (`RsdmSlider`), which unlike the other
/// controllers ships `alarm_sensitive_content` ON by default (PyDM parity,
/// slider.py:264-265). So a MEDM valuator WITHOUT `clrmod="alarm"` must turn it
/// OFF explicitly (else it would alarm-colour with no MEDM basis); one WITH it
/// takes the same MEDM-palette wiring as every other widget
/// ([`alarm_content_builder`]).
fn valuator_alarm_builder(widget: &MedmWidget) -> Option<String> {
    alarm_content_builder(widget).or(Some(".with_alarm_sensitive_content(false)".to_string()))
}

/// The MEDM `dynamic attribute` colour MODE (`clr`: `static`/`alarm`/`discrete`),
/// or `None` when the widget has no dynamic attribute or no `clr` key there. This
/// is the colour-rule mode string and is DISTINCT from the integer `clr`/`bclr`
/// colour indices the parser resolves into `widget.color` (a non-numeric
/// `clr="alarm"` fails to parse as an index, so the parser leaves it here).
fn dynamic_color_mode(widget: &MedmWidget) -> Option<&str> {
    widget
        .attributes
        .get("dynamic attribute")?
        .get("clr")
        .map(String::as_str)
}

/// The `.with_alarm_sensitive_*` builder for a `RsdmDrawing` whose MEDM dynamic
/// attribute sets `clr="alarm"`: the bound channel's alarm severity recolours the
/// object's drawing colour — its border when `draws_border_only` (an `outline`
/// shape or a polyline, which paint only their pen), otherwise its fill. The
/// drawing is already connected to the dynamic-attribute channel by
/// [`dynamic_channel`], so only the sensitivity flag is missing. Returns `None`
/// for every other mode — and that is FAITHFUL, not a dropped feature: MEDM's
/// draw code for dynamic-attribute static graphics treats `discrete` identically
/// to `static` (`case STATIC: case DISCRETE:` both use `colormap[attr.clr]`, the
/// static colour — verified in medm/medm{Rectangle,Oval,Arc,Polygon,Polyline,
/// Text}.c and the rising/falling lines), so the static `clr`/`bclr` already
/// emitted is exactly what MEDM draws.
fn drawing_alarm_builder(widget: &MedmWidget, draws_border_only: bool) -> Option<String> {
    match dynamic_color_mode(widget) {
        // MEDM draws the ALARM arm with `alarmColor(severity)` for every
        // severity (medmRectangle.c etc.), so the MEDM palette rides along —
        // NO_ALARM paints Green3, not the static colour.
        Some("alarm") => Some(format!(
            "{}\n            .with_alarm_palette(AlarmPalette::Medm)",
            if draws_border_only {
                ".with_alarm_sensitive_border(true)"
            } else {
                ".with_alarm_sensitive_content(true)"
            }
        )),
        _ => None,
    }
}

/// The colour token for a static `text` label plus any setup statement to precede
/// it. Normally the static `clr` colour expression with no setup. When the MEDM
/// dynamic attribute sets `clr="alarm"` with a channel, the fixed text is
/// recoloured by that channel's alarm severity each frame: a `Channel` field is
/// emitted (mirroring the visibility-gate pattern) and the setup binds a `__c`
/// local = `severity_color_medm(...)` — MEDM's `alarmColor` table, which is
/// total (`NO_ALARM` paints Green3; medmText.c's ALARM arm never falls back to
/// the static colour). `clr="discrete"` keeps the static colour with no setup —
/// and that is FAITHFUL: medm/medmText.c draws dynamic-attribute text with
/// `case STATIC: case DISCRETE:` sharing `colormap[attr.clr]`, so discrete and
/// static are the same colour for a `text` widget (DISCRETE only differs for
/// monitor `clrmod`, not the dynamic-attribute `clr`).
fn static_text_color(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
    fallback: Color,
) -> (String, String) {
    let fallback_expr = color_expr(fallback);
    match dynamic_color_mode(widget) {
        Some("alarm") => {
            let Some(chan) = widget
                .attributes
                .get("dynamic attribute")
                .and_then(|a| a.get("chan"))
                .filter(|c| !c.is_empty())
            else {
                b.warnings.push(format!(
                    "line {}: static text clr=\"alarm\" has no channel; static colour kept",
                    widget.line
                ));
                return (String::new(), fallback_expr);
            };
            let addr = apply_protocol(chan, options);
            let id = b.index();
            let field = format!("alarm{id}");
            b.needs_channel = true;
            let addr_expr = medm_str(b, &addr);
            b.ctors.push(format!(
                "let {field} = engine\n            .connect({addr_expr})\n            .expect({});",
                rust_str(&format!("adl2rsdm: connect alarm-colour source {addr}"))
            ));
            b.fields.push((field.clone(), "Channel".to_string()));
            let setup = format!(
                "    let __c = {field}.read(|s| severity_color_medm(s.effective_severity()));\n"
            );
            (setup, "__c".to_string())
        }
        // `discrete` (and `static`/unknown) keep the static colour — for a
        // dynamic-attribute text widget MEDM draws discrete identically to static.
        _ => (String::new(), fallback_expr),
    }
}

/// The horizontal text alignment for a `text` / `text update`, as
/// `(rsdm TextAlign variant, egui Align variant)`, for the non-default cases.
/// MEDM `horiz. left`, `justify`, and an absent `align` are the default left
/// alignment and return `None` (so nothing is emitted and existing left-aligned
/// output is unchanged). adl2pydm applies `align` to static text and text updates
/// (`writePropertyTextAlignment`); `justify` collapses to left for single-line
/// value text.
fn text_alignment(widget: &MedmWidget) -> Option<(&'static str, &'static str)> {
    match widget.assignments.get("align").map(String::as_str) {
        Some("horiz. centered") => Some(("Center", "Center")),
        Some("horiz. right") => Some(("Right", "Max")),
        _ => None,
    }
}

/// The limit builder for a control's `limits` block, resolving each end from its
/// OWN MEDM source. An end is user-defined only when its `*Src ==
/// PV_LIMITS_DEFAULT` (`"default"`), and its value falls to `loprDefault`
/// (LOPR_DEFAULT `0.0`) / `hoprDefault` (HOPR_DEFAULT `1.0`) when the `*Default`
/// key is omitted (`medmWidget.h:55-56`; `writeDlLimits` omits each default at
/// that value, `medmCommon.c:653-662`). rsdm resolves limits per bound (R2-66),
/// so each MEDM case maps directly: both ends default → `.with_limits`, lower
/// only → `.with_lower_limit`, upper only → `.with_upper_limit`, neither →
/// nothing (both channel-driven). This closes the former all-or-nothing residual
/// where a single-sided default was dropped to the channel and warned.
fn user_defined_limits(widget: &MedmWidget) -> Option<String> {
    match defaulted_limits(|k| widget.assignments.get(k)) {
        (None, None) => None, // both channel-sourced
        (Some(lo), Some(hi)) => Some(format!(
            ".with_limits({}, {})",
            float_lit(lo),
            float_lit(hi)
        )),
        (Some(lo), None) => Some(format!(".with_lower_limit({})", float_lit(lo))),
        (None, Some(hi)) => Some(format!(".with_upper_limit({})", float_lit(hi))),
    }
}

/// Resolve each limit end from its OWN MEDM source (per-bound, R2-66) out of an
/// arbitrary key block: an end is user-defined only when its `*Src == "default"`
/// (`PV_LIMITS_DEFAULT`), its value falling to LOPR_DEFAULT `0.0` /
/// HOPR_DEFAULT `1.0` when the `*Default` key is omitted (`medmWidget.h:55-56`;
/// `writeDlLimits` omits each default at that value, `medmCommon.c:653-662`).
/// Returns `(lo, hi)` where each end is `Some(value)` when default-sourced and
/// `None` when channel-sourced. `get` looks a key up in the block — a widget's
/// `assignments` for a control's own limits, or a strip-chart pen map for a pen
/// range — so both share one resolver.
fn defaulted_limits<'a>(get: impl Fn(&str) -> Option<&'a String>) -> (Option<f64>, Option<f64>) {
    let end = |src: &str, default_key: &str, fallback: f64| {
        (get(src).map(String::as_str) == Some("default")).then(|| {
            get(default_key)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(fallback)
        })
    };
    (
        end("loprSrc", "loprDefault", 0.0),
        end("hoprSrc", "hoprDefault", 1.0),
    )
}

/// Emit `Some(<float>)` / `None` for an optional normalization bound fed to
/// `RsdmTimePlot::add_normalized_channel`.
fn opt_float_lit(v: Option<f64>) -> String {
    v.map_or_else(|| "None".to_string(), |v| format!("Some({})", float_lit(v)))
}

/// A `.with_orientation(...)` builder from a MEDM `direction`, or `None` when the
/// resolved orientation already equals the widget's own default (so no builder is
/// needed). `default_vertical` is that default (byte = vertical, scale indicator
/// = horizontal). MEDM `up`/`down` are vertical, `right`/`left` horizontal; an
/// unknown direction warns and is treated as `right` (horizontal), as adl2pydm's
/// `write_direction` default does. The single owner of MEDM direction → rsdm
/// orientation, so byte and the scale indicators map it identically.
fn direction_orientation(
    b: &mut Builder,
    widget: &MedmWidget,
    default_vertical: bool,
) -> Option<String> {
    let direction = widget
        .assignments
        .get("direction")
        .map(String::as_str)
        .unwrap_or("right");
    let vertical = match direction {
        "up" | "down" => true,
        "right" | "left" => false,
        other => {
            b.warnings.push(format!(
                "line {}: direction {other:?} unsupported, using 'right'",
                widget.line
            ));
            false
        }
    };
    if vertical == default_vertical {
        None
    } else if vertical {
        Some(".with_orientation(Orientation::Vertical)".to_string())
    } else {
        Some(".with_orientation(Orientation::Horizontal)".to_string())
    }
}

/// Decimals for a wheel-switch `format`, a faithful port of Xc `compute_format`
/// (`WheelSwitch.c:1347-1400`). MEDM hands the raw `format` to the Xc `WheelSwitch`
/// widget, which never leaves the precision to the channel:
///   - it needs a `%` with an `f` conversion after it; without both, the whole
///     format is invalid and it falls back to `DEFAULT_FORMAT` `"% 6.2f"`
///     (precision 2, `:44`);
///   - otherwise it skips printf flags, then `sscanf`s the remainder as `"%d.%d"`
///     (width.precision):
///       * both fields (`n.m`) -> precision, clamped to `[0, width-1]`;
///       * width only (`%6f`)  -> precision 0;
///       * neither  (`%f`, `%.3f`: a leading `.`/no digit gives `nparsed==0`)
///         -> `DEFAULT_FORMAT` precision 2.
///
/// So an unparseable/degenerate format is 2 decimals (Xc's default), never a
/// fall-through to the channel — this always returns a concrete precision. R2-69
/// ported only the happy-path `n.m` clamp; R3-22 adds Xc's default fallback.
/// `"integer"` is an `adl2pydm`/adl2rsdm convenience (not a printf spec) for 0.
fn wheel_decimals(fmt: &str) -> i32 {
    const DEFAULT_PRECISION: i32 = 2; // DEFAULT_FORMAT "% 6.2f" (WheelSwitch.c:44-46)
    if fmt == "integer" {
        return 0;
    }
    // Need a `%` with an `f` conversion after it; else Xc uses DEFAULT_FORMAT.
    let Some(pct) = fmt.find('%') else {
        return DEFAULT_PRECISION;
    };
    let after = &fmt[pct + 1..];
    let Some(fpos) = after.find('f') else {
        return DEFAULT_PRECISION;
    };
    // Skip printf flags, then emulate `sscanf(rest, "%d.%d")` over the pre-`f` span.
    let spec = after[..fpos].trim_start_matches([' ', '+', '#', '0', '-']);
    // Leading width digits are sscanf's first `%d`; none -> nparsed 0 -> DEFAULT
    // (e.g. `%f`, `%.3f`).
    let wend = spec
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(spec.len());
    if wend == 0 {
        return DEFAULT_PRECISION;
    }
    let Ok(width) = spec[..wend].parse::<i32>() else {
        return DEFAULT_PRECISION; // width overflow: treat as unparseable
    };
    // A leading `0` was stripped as a flag, so the first width digit is 1-9 and
    // `width >= 1` here. After the width, `.<digits>` gives the precision
    // (nparsed 2); no `.`, or `.` with no digits, is width-only (nparsed 1) -> 0.
    match spec[wend..].strip_prefix('.') {
        Some(rest) => {
            let pend = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            match rest[..pend].parse::<i32>() {
                Ok(prec) => prec.clamp(0, width - 1), // Xc clamps to [0, width-1]
                Err(_) => 0, // `.` with no digits -> sscanf stops at 1 field
            }
        }
        None => 0, // width only, e.g. `%6f`
    }
}

/// A Rust `f64` literal for `v`, always carrying a decimal point or exponent so
/// it types as `f64` (e.g. `0.0`, `10.5`).
fn float_lit(v: f64) -> String {
    format!("{v:?}")
}

fn skip_no_geometry(b: &mut Builder, widget: &MedmWidget) {
    b.warnings.push(format!(
        "line {}: {:?} has no geometry; skipped",
        widget.line, widget.symbol
    ));
}

fn skip_no_channel(b: &mut Builder, widget: &MedmWidget) {
    b.warnings.push(format!(
        "line {}: {:?} has no channel; skipped",
        widget.line, widget.symbol
    ));
}

/// The channel address for a widget: its `control`/`monitor` block's `chan`,
/// with macros substituted and the protocol prefixed. Pre-2.4 MEDM `.adl` files
/// spell the key `ctrl` (control) / `rdbk` (monitor) instead of `chan` — MEDM
/// parses both (`medm/medmControl.c:36-37` matches "ctrl"/"chan";
/// `medm/medmMonitor.c:77-78` matches "rdbk"/"chan"; adl2pydm
/// `output_handler.py:179-184` get_channel reads all three keys), so each
/// block falls back to its old-format key.
fn channel_address(widget: &MedmWidget, options: &Options) -> Option<String> {
    let chan = widget
        .attributes
        .get("control")
        .and_then(|a| a.get("chan").or_else(|| a.get("ctrl")))
        .or_else(|| {
            widget
                .attributes
                .get("monitor")
                .and_then(|a| a.get("chan").or_else(|| a.get("rdbk")))
        })?;
    Some(apply_protocol(chan, options))
}

/// The channel for a `dynamic attribute` (drawings, composites): its `chan` with
/// macros + protocol when present and non-empty, else a unique local `loc://`
/// placeholder so the channel-less decoration still constructs. `kind` names the
/// placeholder (`shape`, `frame`); a per-screen counter (not the widget line)
/// keeps it unique even across inlined files. The flag reports the placeholder
/// case, which the emitters turn into `.with_placeholder_channel()` so the
/// synthetic address never reaches a PV-facing surface (tooltip, Btn2 copy).
fn dynamic_channel(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
    kind: &str,
) -> (String, bool) {
    if let Some(chan) = widget
        .attributes
        .get("dynamic attribute")
        .and_then(|a| a.get("chan"))
        .filter(|c| !c.is_empty())
    {
        return (apply_protocol(chan, options), false);
    }
    (b.synthetic_addr(kind), true)
}

/// Prefix the protocol onto a MEDM channel name. The channel's `$(macro)`
/// references are already expanded by [`expand_macros`] at the parse→emit
/// boundary, so this only joins the protocol.
fn apply_protocol(chan: &str, options: &Options) -> String {
    format!("{}{}", options.protocol, chan)
}

/// Substitute `$(name)` and `${name}` macros; unmatched references are left in
/// place (the user supplies them via `--macro`), exactly as MEDM's lexer leaves
/// an unknown `$(macro)` literal (`medm/medmCommon.c` `getToken`).
fn substitute_macros(input: &str, macros: &[(String, String)]) -> String {
    let mut out = input.to_string();
    for (name, value) in macros {
        out = out.replace(&format!("$({name})"), value);
        out = out.replace(&format!("${{{name}}}"), value);
    }
    out
}

/// Expand every `$(macro)` in a parsed widget subtree *in place* — channels and
/// user-visible strings (labels, captions, shell commands, related-display
/// targets) alike. MEDM's lexer substitutes macros for *every* token it reads
/// (`medm/medmCommon.c` `getToken`, in both the bare-word and quoted-string
/// states), so the faithful rule is uniform: one pass over the IR, run once at
/// each parse→emit boundary ([`generate`] and embedded-display inlining). RsDM
/// has no runtime macro engine, so values are baked in at convert time; after
/// this pass the IR is macro-free, so no emitter can forget to substitute.
fn expand_macros(widgets: &mut [MedmWidget], macros: &[(String, String)]) {
    if macros.is_empty() {
        return;
    }
    for w in widgets {
        if let Some(title) = &mut w.title {
            *title = substitute_macros(title, macros);
        }
        for v in w.assignments.values_mut() {
            *v = substitute_macros(v, macros);
        }
        for block in w.attributes.values_mut() {
            for v in block.values_mut() {
                *v = substitute_macros(v, macros);
            }
        }
        for recs in w.records.values_mut() {
            for rec in recs.iter_mut() {
                for v in rec.values_mut() {
                    *v = substitute_macros(v, macros);
                }
            }
        }
        expand_macros(&mut w.children, macros);
    }
}

/// A Rust string literal for `s`, with escaping (`{:?}` produces exactly that).
fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

/// Whether `s` still carries a `$(name)` / `${name}` macro reference after the
/// convert-time baking (a stray unclosed `$(` matches too — harmless, since
/// runtime expansion leaves it literal exactly like MEDM's lexer).
pub(crate) fn has_macro_ref(s: &str) -> bool {
    s.contains("$(") || s.contains("${")
}

/// The Rust expression for an MEDM string consumed at runtime: a plain literal
/// when fully grounded, or an expansion against the screen instance's macro
/// table when a `$(macro)` survived convert-time baking (the related-display
/// child path, where macro values only exist at runtime). This is the child
/// screen re-parsing its own file, so it uses MEDM's lexer `getToken`
/// (`medm/medmCommon.c:1455-1462`) semantics — an undefined `$(name)` stays
/// literal — via `MacroTable::expand`, *not* the `args`-only
/// `performMacroSubstitutions` path (see [`rd_click`]). Emitted as `.as_str()` so
/// the expression is a `&str` in every context a literal fits (`&str` params,
/// `Some(...)`, `impl Into<String>`/`Into<WidgetText>`, `AsRef<OsStr>`); the
/// expansion temporary lives to the end of the enclosing statement.
fn medm_str(b: &mut Builder, s: &str) -> String {
    if has_macro_ref(s) && !b.seal_macros {
        b.needs_macros = true;
        b.needs_macro_expand = true;
        format!("__m.expand({}).as_str()", rust_str(s))
    } else {
        // Fully grounded, OR a macro that survived a replaced composite-file
        // table (`b.seal_macros`): MEDM leaves such a `$(name)` literal (the
        // parent table it would have resolved against is out of scope), so emit
        // the string verbatim rather than deferring to the runtime `__m`.
        rust_str(s)
    }
}

/// `Color32::from_rgb(r, g, b)` for a MEDM colour.
fn color_expr(c: Color) -> String {
    format!("Color32::from_rgb({}, {}, {})", c.r, c.g, c.b)
}

/// Assemble the final module source from the accumulated pieces.
fn assemble(b: &Builder, screen: &MedmScreen) -> String {
    let mut s = String::new();

    let title = if screen.adl_filename.is_empty() {
        "an MEDM screen".to_string()
    } else {
        screen.adl_filename.clone()
    };
    let _ = writeln!(
        s,
        "// AUTO-GENERATED from {title} by adl2rsdm -- do not edit by hand.\n"
    );

    // Imports: egis/Engine/rsplot are always used; Color32 and the widget glob
    // only when something references them (keeps the output warning-clean).
    let _ = writeln!(s, "use rsdm::Engine;");
    if b.needs_channel {
        let _ = writeln!(s, "use rsdm::Channel;");
    }
    if b.needs_widgets {
        let _ = writeln!(s, "use rsdm::widgets::*;");
    }
    if b.needs_color {
        let _ = writeln!(s, "use rsplot::egui::{{self, Color32}};");
    } else {
        let _ = writeln!(s, "use rsplot::egui;");
    }
    s.push('\n');

    // Struct.
    let _ = writeln!(s, "/// RsDM screen generated from `{title}`.");
    let _ = writeln!(s, "pub struct Screen {{");
    let _ = writeln!(s, "    _engine: Engine,");
    if b.needs_macros {
        let _ = writeln!(s, "    __m: MacroTable,");
    }
    if b.needs_rd_open {
        let _ = writeln!(
            s,
            "    /// Render state handed on to child screens opened from related displays."
        );
        let _ = writeln!(s, "    __rs: Option<rsplot::egui_wgpu::RenderState>,");
        let _ = writeln!(
            s,
            "    /// The related displays this screen has open (MEDM's display list)."
        );
        let _ = writeln!(s, "    __open: Vec<{}OpenDisplay>,", b.rt_prefix());
    }
    for (name, ty) in &b.fields {
        let _ = writeln!(s, "    {name}: {ty},");
    }
    let _ = writeln!(s, "}}\n");

    // impl: new() + ui().
    let _ = writeln!(s, "impl Screen {{");
    emit_new(&mut s, b);
    s.push('\n');
    emit_ui(&mut s, b, screen);
    let _ = writeln!(s, "}}\n");

    emit_place_helper(&mut s, b.use_layout);
    if b.needs_macros {
        emit_macro_table(&mut s, b);
    }
    // The shared runtime items live once at the output file's top level; child
    // `pub mod`s reference them through `super::` (the recursive driver appends
    // the plot-id allocator itself when only a child needs it).
    if b.next_plot_id > 0 && !b.child_module {
        s.push_str(PLOT_IDS_HELPER);
    }
    if b.needs_rd_open && !b.child_module {
        s.push_str(RD_RUNTIME_HELPER);
    }
    if b.needs_rd_icon {
        s.push_str(RELATED_DISPLAY_ICON_HELPER);
    }
    if b.needs_sc_icon {
        s.push_str(SHELL_COMMAND_ICON_HELPER);
    }
    s
}

/// The runtime macro table emitted into screens whose strings still carry
/// `$(macro)` references after convert-time baking (related-display children,
/// whose macro values only exist at runtime). Assembled from the three parts
/// below by [`emit_macro_table`], so a screen carries only the method(s) it uses.
const MACRO_TABLE_HEAD: &str = r#"
/// A display instance's macro table. `expand` follows MEDM's lexer `getToken`
/// (child screens re-parsing their own file — an unknown `$(name)` stays
/// literal); `expand_args` follows MEDM `performMacroSubstitutions` (the
/// related-display `args` path — an unknown `$(name)` is dropped).
pub struct MacroTable(pub Vec<(String, String)>);

impl MacroTable {"#;

/// `MacroTable::expand` — the child-string path (MEDM `getToken`, unknown refs
/// left literal). Emitted when [`Builder::needs_macro_expand`] is set.
const MACRO_TABLE_EXPAND: &str = r#"
    /// Substitute `$(name)`/`${name}` for a defined macro, leaving an unknown
    /// reference in place exactly as MEDM's lexer does (medm/medmCommon.c
    /// `getToken`).
    fn expand(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (name, value) in &self.0 {
            out = out.replace(&format!("$({name})"), value);
            out = out.replace(&format!("${{{name}}}"), value);
        }
        out
    }
"#;

/// `MacroTable::expand_args` — the related-display `args` path (MEDM
/// `performMacroSubstitutions`, unknown refs dropped). Emitted when
/// [`Builder::needs_macro_args`] is set.
const MACRO_TABLE_EXPAND_ARGS: &str = r#"
    /// Substitute `$(name)` for a defined macro and *drop* an undefined one
    /// (`$(X)` with X unbound becomes empty, not literal), MEDM
    /// `performMacroSubstitutions` (medm/utils.c:3444-3459). Only the `$(...)`
    /// form is a macro here — a `$` not opening `(` is copied verbatim, exactly as
    /// MEDM's byte scanner does.
    fn expand_args(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(d) = rest.find('$') {
            out.push_str(&rest[..d]);
            let after = &rest[d + 1..];
            if let Some(tail) = after.strip_prefix('(') {
                // MEDM reads to the ')' or, if none, to end-of-string, then
                // substitutes the defined value or drops the reference.
                let (name, next) = match tail.find(')') {
                    Some(end) => (&tail[..end], &tail[end + 1..]),
                    None => (tail, ""),
                };
                if let Some((_, value)) = self.0.iter().find(|(n, _)| n == name) {
                    out.push_str(value);
                }
                rest = next;
            } else {
                // `$` not opening `(` -> verbatim (includes `${...}`).
                out.push('$');
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }
"#;

/// The closing brace of the emitted `impl MacroTable`.
const MACRO_TABLE_TAIL: &str = "}\n";

/// Emit the `MacroTable` struct plus only the method(s) this screen actually
/// uses — `expand` for the child-string path, `expand_args` for the
/// related-display `args` path — so neither is dead code. At least one is set
/// whenever `needs_macros` is (both flags are raised alongside it at their use
/// sites), so the `impl` is never empty.
fn emit_macro_table(s: &mut String, b: &Builder) {
    s.push_str(MACRO_TABLE_HEAD);
    if b.needs_macro_expand {
        s.push_str(MACRO_TABLE_EXPAND);
    }
    if b.needs_macro_args {
        s.push_str(MACRO_TABLE_EXPAND_ARGS);
    }
    s.push_str(MACRO_TABLE_TAIL);
}

/// The shared `PlotId` allocator, emitted once at the output file's top level
/// when any screen in it carries plots (strip charts, cartesian plots): rsplot
/// keys per-plot GPU resources by `PlotId` within a shared render state, so
/// every screen *instance* — related-display children included — must draw from
/// one counter.
pub(crate) const PLOT_IDS_HELPER: &str = r#"
/// Allocate a contiguous block of `count` rsplot `PlotId`s, unique across every
/// screen instance built from this generated file (related-display children
/// included) -- rsplot keys per-plot GPU resources by `PlotId`, so two
/// instances must never share one. (Two *separately generated* files compiled
/// into one app each start at 0 and can still collide; convert such screens
/// together through one root instead.)
fn next_plot_ids(count: u64) -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(count, std::sync::atomic::Ordering::Relaxed)
}
"#;

/// The related-display runtime, emitted once at the output file's top level
/// when any screen in it opens converted targets: the display trait the child
/// screens implement, the open-display list managing the child viewports, and
/// MEDM's macro-args parser.
const RD_RUNTIME_HELPER: &str = r#"
/// What a related-display child screen exposes to be hosted in a viewport: its
/// per-frame draw. Implemented by every `Screen` in this generated file.
pub trait RsdmDisplay {
    fn ui(&mut self, ui: &mut egui::Ui);
}

/// One open related display: a child screen shown in its own immediate egui
/// viewport, keyed by (module, macro args) so a second click focuses the
/// existing window instead of duplicating it (MEDM `popupExistingDisplay`;
/// MEDM dedups across *all* displays, this list is per parent instance).
pub struct OpenDisplay {
    key: (&'static str, String),
    viewport: egui::ViewportId,
    title: String,
    size: egui::Vec2,
    screen: Box<dyn RsdmDisplay>,
}

impl OpenDisplay {
    /// Focus the already-open display for `key`, or build one with `make` and
    /// open it (MEDM `relatedDisplayCreateNewDisplay`).
    pub fn open_or_focus(
        open: &mut Vec<OpenDisplay>,
        ctx: &egui::Context,
        key: (&'static str, String),
        title: &str,
        size: egui::Vec2,
        make: impl FnOnce() -> Box<dyn RsdmDisplay>,
    ) {
        if let Some(d) = open.iter().find(|d| d.key == key) {
            if ctx.embed_viewports() {
                // Embedded fallback: there is no native window to focus --
                // the child renders as an `egui::Window` whose area id is its
                // viewport id (egui `Window::from_viewport`), so raise that
                // window instead (MEDM `popupExistingDisplay` raises too).
                ctx.move_to_top(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new(d.viewport),
                ));
            } else {
                ctx.send_viewport_cmd_to(d.viewport, egui::ViewportCommand::Focus);
            }
            return;
        }
        // A process-wide monotonic id keeps every viewport distinct, even
        // across close-and-reopen and across parent instances.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        open.push(OpenDisplay {
            key,
            viewport: egui::ViewportId::from_hash_of(("adl2rsdm related display", n)),
            title: title.to_owned(),
            size,
            screen: make(),
        });
    }

    /// Show every open display as an immediate viewport (a native OS window;
    /// egui falls back to an embedded `egui::Window` when the backend has no
    /// multi-viewport support), dropping each one whose window was closed.
    pub fn show_all(open: &mut Vec<OpenDisplay>, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        open.retain_mut(|d| {
            let mut keep = true;
            ctx.show_viewport_immediate(
                d.viewport,
                egui::ViewportBuilder::default()
                    .with_title(d.title.clone())
                    .with_inner_size(d.size),
                |ui, _class| {
                    d.screen.ui(ui);
                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        keep = false;
                    }
                },
            );
            keep
        });
    }
}

/// Parse MEDM's related-display `args` ("A=1,B=2") into a macro table: names
/// delimited by `=`, values by `,`, every whitespace character stripped from
/// both (medm/utils.c `generateNameValueTable`).
pub fn parse_macro_args(args: &str) -> Vec<(String, String)> {
    args.split(',')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let name: String = name.chars().filter(|c| !c.is_whitespace()).collect();
            if name.is_empty() {
                return None;
            }
            let value: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            Some((name, value))
        })
        .collect()
}
"#;

/// The icon MEDM renders on a label-less related-display button: a front
/// display frame overlapping the corner of a back one. Geometry mirrors
/// `renderRelatedDisplayPixmap` (medmRelatedDisplay.c) — the `relatedDisplay25`
/// bitmap in 25ths of the icon square, with the front rectangle's interior
/// erased to the background before stroking so it hides the back frame.
const RELATED_DISPLAY_ICON_HELPER: &str = r#"
/// Paint MEDM's related-display icon (a front display frame overlapping a back
/// one) centred in `rect` -- what MEDM shows when a related display has no
/// label (medmRelatedDisplay.c `renderRelatedDisplayPixmap`).
fn related_display_icon(ui: &egui::Ui, rect: egui::Rect, fg: egui::Color32, bg: egui::Color32) {
    let side = (rect.height().min(rect.width()) - 8.0).max(4.0);
    let icon = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(side));
    let p = |x: f32, y: f32| icon.min + egui::vec2(x, y) * (side / 25.0);
    let stroke = egui::Stroke::new(1.0, fg);
    let painter = ui.painter();
    painter.line_segment([p(16.0, 9.0), p(22.0, 9.0)], stroke);
    painter.line_segment([p(22.0, 9.0), p(22.0, 22.0)], stroke);
    painter.line_segment([p(22.0, 22.0), p(10.0, 22.0)], stroke);
    painter.line_segment([p(10.0, 22.0), p(10.0, 18.0)], stroke);
    let front = egui::Rect::from_min_size(p(4.0, 4.0), egui::vec2(13.0, 14.0) * (side / 25.0));
    painter.rect_filled(front, egui::CornerRadius::ZERO, bg);
    painter.rect_stroke(front, egui::CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
}
"#;

/// The icon MEDM renders on a label-less shell-command button: an exclamation
/// mark (bar + dot). Geometry mirrors `renderShellCommandPixmap`
/// (medmShellCommand.c) — the `shellCommand25` bitmap in 25ths of the icon
/// square.
const SHELL_COMMAND_ICON_HELPER: &str = r#"
/// Paint MEDM's shell-command icon (an exclamation mark) centred in `rect` --
/// what MEDM shows when a shell command has no label (medmShellCommand.c
/// `renderShellCommandPixmap`).
fn shell_command_icon(ui: &egui::Ui, rect: egui::Rect, fg: egui::Color32) {
    let side = (rect.height().min(rect.width()) - 8.0).max(4.0);
    let icon = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(side));
    let p = |x: f32, y: f32| icon.min + egui::vec2(x, y) * (side / 25.0);
    let painter = ui.painter();
    let unit = side / 25.0;
    painter.rect_filled(
        egui::Rect::from_min_size(p(12.0, 4.0), egui::vec2(3.0, 14.0) * unit),
        egui::CornerRadius::ZERO,
        fg,
    );
    painter.rect_filled(
        egui::Rect::from_min_size(p(12.0, 20.0), egui::vec2(3.0, 3.0) * unit),
        egui::CornerRadius::ZERO,
        fg,
    );
}
"#;

/// Emit the constructors: `new(cc)` — the eframe entry, which installs rsplot
/// and delegates — and `new_in`, which builds the screen on an existing egui
/// context (the path a related-display child takes, where no
/// `CreationContext` exists). `macros` is this display instance's runtime
/// macro table (MEDM `relatedDisplayCreateNewDisplay` `processedArgs`); the
/// root instance gets the convert-time `--macro` table. A child module emits
/// `new_in` only — a child is never an eframe app root, and the dead entry
/// point would warn in every consuming crate.
fn emit_new(s: &mut String, b: &Builder) {
    if !b.child_module {
        let _ = writeln!(
            s,
            "    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {{"
        );
        let _ = writeln!(
            s,
            "        let rs = cc.wgpu_render_state.as_ref().expect(\"adl2rsdm: a wgpu render state is required\");"
        );
        let _ = writeln!(s, "        rsplot::install(rs);");
        let macros_arg = if b.needs_macros && !b.macros.is_empty() {
            let pairs: Vec<String> = b
                .macros
                .iter()
                .map(|(n, v)| format!("({}.to_string(), {}.to_string())", rust_str(n), rust_str(v)))
                .collect();
            format!("vec![{}]", pairs.join(", "))
        } else {
            "Vec::new()".to_string()
        };
        let _ = writeln!(
            s,
            "        Self::new_in(&cc.egui_ctx, Some(rs), {macros_arg})"
        );
        let _ = writeln!(s, "    }}");
        s.push('\n');
    }

    // `render_state` is read when plot ctors unwrap it AND when child screens
    // opened from related displays inherit it (`__rs`).
    let rs_param = if b.needs_render_state || b.needs_rd_open {
        "render_state"
    } else {
        "_render_state"
    };
    let macros_param = if b.needs_macros { "macros" } else { "_macros" };
    let _ = writeln!(
        s,
        "    /// Build the screen on an existing egui context (the related-display child"
    );
    let _ = writeln!(
        s,
        "    /// path). `macros` is this display instance's macro table (MEDM"
    );
    let _ = writeln!(s, "    /// `performMacroSubstitutions`).");
    let _ = writeln!(s, "    pub fn new_in(");
    let _ = writeln!(s, "        ctx: &egui::Context,");
    let _ = writeln!(
        s,
        "        {rs_param}: Option<&rsplot::egui_wgpu::RenderState>,"
    );
    let _ = writeln!(s, "        {macros_param}: Vec<(String, String)>,");
    let _ = writeln!(s, "    ) -> Self {{");
    if b.needs_render_state {
        let _ = writeln!(
            s,
            "        let rs = render_state.expect(\"adl2rsdm: this screen needs a wgpu render state for its plots\");"
        );
    }
    if b.needs_macros {
        let _ = writeln!(s, "        let __m = MacroTable(macros);");
    }
    if b.next_plot_id > 0 {
        let _ = writeln!(
            s,
            "        // This instance's block of rsplot PlotIds (unique per instance, so"
        );
        let _ = writeln!(
            s,
            "        // related-display children never collide on GPU plot resources)."
        );
        let _ = writeln!(
            s,
            "        let __plot_base = {}next_plot_ids({});",
            b.rt_prefix(),
            b.next_plot_id
        );
    }
    let _ = writeln!(s, "        let engine = Engine::new();");
    let _ = writeln!(s, "        engine.attach_repaint(ctx.clone());");
    for ctor in &b.ctors {
        let _ = writeln!(s, "        {ctor}");
    }
    let _ = write!(s, "        Self {{ _engine: engine");
    if b.needs_macros {
        let _ = write!(s, ", __m");
    }
    if b.needs_rd_open {
        let _ = write!(s, ", __rs: render_state.cloned(), __open: Vec::new()");
    }
    for (name, _) in &b.fields {
        let _ = write!(s, ", {name}");
    }
    let _ = writeln!(s, " }}");
    let _ = writeln!(s, "    }}");
}

/// Emit the `ui()` draw method: placements sorted back-to-front. In responsive
/// (`use_layout`) mode it first binds `sx`/`sy` — the per-axis `available /
/// native` scale every `place(...)` multiplies its MEDM rect by, so the screen
/// reflows with the window (adl2pydm `grid_layout` parity, see [`Options::use_layout`]).
fn emit_ui(s: &mut String, b: &Builder, screen: &MedmScreen) {
    let _ = writeln!(s, "    pub fn ui(&mut self, ui: &mut egui::Ui) {{");
    let _ = writeln!(
        s,
        "        // Back-to-front: decoration (Background) -> monitor (Middle) -> control"
    );
    let _ = writeln!(
        s,
        "        // (Foreground), so controls are never occluded or click-stolen."
    );

    // Bind each widget field to a disjoint `&mut` local. A container's draw
    // closure (`RsdmFrame::show(ui, |ui| ...)`) needs to touch sibling fields
    // while the frame itself is borrowed by the `show` receiver; going through
    // `self.field` inside the closure would re-borrow all of `self` and conflict.
    if !b.fields.is_empty() || b.needs_macros || b.needs_rd_open {
        let _ = write!(s, "        let Self {{ _engine: _");
        if b.needs_macros {
            // Bind the macro table only when a draw body expands a string;
            // a table used solely by `new_in` is discarded here so the
            // generated module stays warning-clean. (`__m.` keeps locals
            // like `__rd_menu` from matching.)
            let ui_uses_macros = b.placements.iter().any(|p| {
                p.body.contains("__m.") || p.gate.as_deref().is_some_and(|g| g.contains("__m."))
            });
            let _ = write!(s, ", __m{}", if ui_uses_macros { "" } else { ": _" });
        }
        if b.needs_rd_open {
            let _ = write!(s, ", __rs, __open");
        }
        for (name, _) in &b.fields {
            let _ = write!(s, ", {name}");
        }
        let _ = writeln!(s, " }} = self;");
    }

    let mut order: Vec<&Placement> = b.placements.iter().collect();
    order.sort_by_key(|p| p.z); // stable: preserves MEDM order within a layer

    // The display block's `bclr` fills the whole screen behind every widget.
    let screen_bg = screen.background_color;
    if order.is_empty() && screen_bg.is_none() {
        // No placements and no background: `sx`/`sy` would be unused, so skip them
        // and just consume `ui` so the empty method is still warning-clean.
        let _ = writeln!(s, "        let _ = ui;");
    } else if b.use_layout {
        // Responsive layout: every place() scales its MEDM rect by (sx, sy) to fill
        // the available area. The native size is the `display` block's geometry
        // (the bounding box of placed widgets when a screen carries none).
        let (native_w, native_h) = layout_native_size(b, screen);
        let _ = writeln!(
            s,
            "        // Responsive layout: scale each MEDM rect by (sx, sy) to fill the"
        );
        let _ = writeln!(
            s,
            "        // available area (adl2pydm grid_layout parity -- proportional reflow)."
        );
        let _ = writeln!(s, "        let avail = ui.max_rect();");
        // The screen origin every top-level placement is measured from.
        let _ = writeln!(s, "        let __origin = avail.min;");
        let _ = writeln!(
            s,
            "        let sx = avail.width() / {};",
            float_lit(native_w)
        );
        let _ = writeln!(
            s,
            "        let sy = avail.height() / {};",
            float_lit(native_h)
        );
    } else {
        // The screen origin every top-level placement is measured from.
        let _ = writeln!(s, "        let __origin = ui.max_rect().min;");
    }
    // Paint the screen background first, as the bottom-most Background-order Area,
    // so it sits behind every widget (decoration included). Covers the native
    // screen rect, scaled with the window in responsive mode like any placement.
    if let Some(bg) = screen_bg {
        let (native_w, native_h) = layout_native_size(b, screen);
        let bg_geom = Geometry {
            x: 0,
            y: 0,
            width: native_w as i32,
            height: native_h as i32,
        };
        let body = format!(
            "let __sbg = ui.max_rect();\nui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, {});",
            color_expr(bg)
        );
        // `u64::MAX` is a fixed Area id that no widget index (0..N) can collide with.
        let bg_place = Placement::drawn(ZLayer::Background, u64::MAX, bg_geom, body);
        write_placement(s, &bg_place, 0, 0, "        ", b.use_layout, "__origin");
    }
    for p in order {
        write_placement(s, p, 0, 0, "        ", b.use_layout, "__origin");
    }
    if b.needs_rd_open {
        let _ = writeln!(
            s,
            "        // Child displays opened from related-display buttons (each in its own"
        );
        let _ = writeln!(
            s,
            "        // viewport; a backend without multi-viewport support falls back to"
        );
        let _ = writeln!(s, "        // embedded windows).");
        let _ = writeln!(
            s,
            "        {}OpenDisplay::show_all(__open, ui);",
            b.rt_prefix()
        );
    }
    let _ = writeln!(s, "    }}");
}

/// The native screen size responsive layout scales against: the `display` block's
/// geometry, or — when a screen carries none (headless/malformed input) — the
/// bounding box of the placed widgets so the scale still fills the area. Both
/// dimensions are clamped to at least 1 so the generated divisor is never zero.
fn layout_native_size(b: &Builder, screen: &MedmScreen) -> (f64, f64) {
    if let Some(g) = screen.geometry
        && g.width > 0
        && g.height > 0
    {
        return (f64::from(g.width), f64::from(g.height));
    }
    let max_x = b
        .placements
        .iter()
        .map(|p| p.geom.x + p.geom.width)
        .max()
        .unwrap_or(1)
        .max(1);
    let max_y = b
        .placements
        .iter()
        .map(|p| p.geom.y + p.geom.height)
        .max()
        .unwrap_or(1)
        .max(1);
    (f64::from(max_x), f64::from(max_y))
}

/// Emit one `place(...)` call at `indent`, offsetting the geometry by `(dx, dy)`
/// — `0, 0` at the top level; a composite's origin for its children so they land
/// inside the frame's interior coordinates. `origin` is the expression for the
/// container's *outer* top-left (`__origin` at the top level, a frame's captured
/// pre-inset origin for its children); every child is positioned relative to it
/// so no widget's inner margin (`RsdmFrame`'s `BORDER_INSET`) can shift a child.
/// The `body` may be several lines (a container's nested draws), each re-indented
/// inside the closure. A `gate` wraps the whole call in `if <gate> { … }` for a
/// dynamic visibility rule. In responsive (`use_layout`) mode the call takes the
/// `sx`/`sy` scale bound by `emit_ui`; a frame's children scale by the same
/// factors (the frame's interior already scaled by them), so the single pair
/// threads through every nesting level.
fn write_placement(
    s: &mut String,
    p: &Placement,
    dx: i32,
    dy: i32,
    indent: &str,
    use_layout: bool,
    origin: &str,
) {
    let Geometry {
        x,
        y,
        width,
        height,
    } = p.geom;
    // A visibility gate wraps the placement in an `if`; the `place(...)` call then
    // sits one indent level deeper.
    let inner = match &p.gate {
        Some(cond) => {
            let _ = writeln!(s, "{indent}if {cond} {{");
            format!("{indent}    ")
        }
        None => indent.to_string(),
    };
    // Responsive mode passes the `(sx, sy)` scale after the origin.
    let scale = if use_layout { "sx, sy, " } else { "" };
    let _ = writeln!(
        s,
        "{inner}place(ui, {origin}, {scale}{}, egui::Id::new({}u64), {}.0, {}.0, {}.0, {}.0, |ui| {{",
        p.z.order_ident(),
        p.id,
        x - dx,
        y - dy,
        width,
        height
    );
    for line in p.body.lines() {
        let _ = writeln!(s, "{inner}    {line}");
    }
    let _ = writeln!(s, "{inner}}});");
    if p.gate.is_some() {
        let _ = writeln!(s, "{indent}}}");
    }
}

/// Emit the shared placement helper. The absolute variant places `add` at fixed
/// MEDM pixels; the responsive (`use_layout`) variant scales the position and
/// size by the per-axis `(sx, sy)` factors `emit_ui` binds, so the screen reflows
/// with the window.
fn emit_place_helper(s: &mut String, use_layout: bool) {
    if use_layout {
        s.push_str(
            r#"/// Place `add` at a MEDM position scaled by `(sx, sy)` -- the per-axis
/// `available / native` factors -- inside its own `egui::Area`, so the screen
/// reflows to fill the window. `origin` is the container's outer top-left (the
/// screen origin, or a frame's pre-inset origin), so a frame's `BORDER_INSET`
/// never shifts its children. The Area's `order` is the z-layer, so decoration
/// (`Background`) renders and takes input below controls (`Foreground`) regardless
/// of call order. The Area id is salted with the host `ui.id()` so two screen
/// instances sharing one viewport (related-display children on an embedded
/// fallback backend) keep distinct Area state.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    origin: egui::Pos2,
    sx: f32,
    sy: f32,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let rect =
        egui::Rect::from_min_size(origin + egui::vec2(x * sx, y * sy), egui::vec2(w * sx, h * sy));
    egui::Area::new(ui.id().with(id))
        .order(order)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(rect);
            ui.set_max_size(rect.size());
            add(ui);
        });
}
"#,
        );
        return;
    }
    s.push_str(
        r#"/// Place `add` at an absolute MEDM position inside its own `egui::Area`.
/// `origin` is the container's outer top-left (the screen origin, or a frame's
/// pre-inset origin), so a frame's `BORDER_INSET` never shifts its children. The
/// Area's `order` is the z-layer, so decoration (`Background`) renders and takes
/// input below controls (`Foreground`) regardless of call order. The Area id is
/// salted with the host `ui.id()` so two screen instances sharing one viewport
/// (related-display children on an embedded fallback backend) keep distinct
/// Area state.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    origin: egui::Pos2,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let rect = egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(w, h));
    egui::Area::new(ui.id().with(id))
        .order(order)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(rect);
            ui.set_max_size(rect.size());
            add(ui);
        });
}
"#,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adl_parser::parse;

    /// A screen with a static text decoration that OVERLAPS a text entry
    /// control, plus a text-update monitor — the overlap case the z-order rule
    /// exists for.
    const OVERLAP: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=200
		height=100
	}
	"basic attribute" {
		clr=1
	}
	textix="Background label"
}
"text update" {
	object {
		x=10
		y=10
		width=80
		height=18
	}
	monitor {
		chan="$(P)rbv"
		clr=0
	}
	limits {
		precSrc="default"
		precDefault=2
	}
}
"text entry" {
	object {
		x=10
		y=40
		width=120
		height=20
	}
	control {
		chan="$(P)set"
	}
}
"#;

    fn build(opts: &Options) -> Generated {
        generate(&parse(OVERLAP), opts)
    }

    #[test]
    fn channel_widgets_keep_only_the_disconnect_border() {
        // MEDM draws no alarm-severity border, so every framed channel widget
        // (here: the text update and the text entry) is emitted in
        // disconnect-dash-only mode — no PyDM ring on Minor/Major/Invalid.
        let g = build(&Options::default());
        assert_eq!(
            g.source
                .matches(".with_border_mode(BorderMode::DisconnectedOnly)")
                .count(),
            2,
            "{}",
            g.source
        );
    }

    #[test]
    fn text_entry_centres_its_text() {
        // MEDM (`XmTextField`, no `XmNalignment`) and PyDM (adl2pydm aligns only
        // text/text-update, never the line edit) both LEFT-align text entries;
        // the converter centres them uniformly so the editable control fields
        // match the centred menu/button captions on a converted screen — a
        // deliberate, documented deviation. OVERLAP has one text entry and no
        // menu, so the sole centred caption is the line edit (its text update is
        // an un-aligned RsdmLabel).
        let g = build(&Options::default());
        assert!(
            g.source.contains("RsdmLineEdit::new(&engine,"),
            "{}",
            g.source
        );
        assert_eq!(
            g.source
                .matches(".with_alignment(TextAlign::Center)")
                .count(),
            1,
            "exactly the text entry should carry centre alignment\n{}",
            g.source
        );
    }

    #[test]
    fn emits_struct_new_ui_and_place_helper() {
        let g = build(&Options::default());
        assert!(g.source.contains("pub struct Screen {"));
        assert!(
            g.source
                .contains("pub fn new(cc: &eframe::CreationContext<'_>)")
        );
        assert!(g.source.contains("pub fn ui(&mut self, ui: &mut egui::Ui)"));
        assert!(g.source.contains("fn place("));
        assert!(g.source.contains("rsplot::install(rs);"));
    }

    #[test]
    fn applies_protocol_and_macros_to_channels() {
        let opts = Options {
            protocol: "ca://".to_string(),
            macros: vec![("P".to_string(), "DMM1:".to_string())],
            ..Options::default()
        };
        let g = build(&opts);
        assert!(
            g.source
                .contains("RsdmLineEdit::new(&engine, \"ca://DMM1:set\")"),
            "macro+protocol not applied:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("RsdmLabel::new(&engine, \"ca://DMM1:rbv\")")
        );
        // precDefault -> with_precision.
        assert!(g.source.contains(".with_precision(2)"));
    }

    #[test]
    fn macros_expand_in_user_visible_strings_not_just_channels() {
        // MEDM's lexer substitutes macros for *every* token it reads
        // (`medm/medmCommon.c` getToken), so a `$(P)` in a static-text label or a
        // button caption is baked in just like a channel address — not only the
        // channel path. Regression for the simdetector `$(P)$(R)` labels.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
display {
	object {
		x=0
		y=0
		width=200
		height=120
	}
	clr=1
	bclr=0
}
text {
	object {
		x=0
		y=0
		width=180
		height=20
	}
	"basic attribute" {
		clr=1
	}
	textix="Hello $(P)"
}
"message button" {
	object {
		x=0
		y=30
		width=80
		height=24
	}
	control {
		chan="$(P)go"
	}
	press_msg="1"
	label="$(P) Start"
}
"#;
        let options = Options {
            macros: vec![("P".to_string(), "DEV:".to_string())],
            ..Options::default()
        };
        let g = generate(&parse(adl), &options);
        // Static-text label: macro baked into the visible string.
        assert!(
            g.source.contains("egui::RichText::new(\"Hello DEV:\")"),
            "static-text label macro not expanded:\n{}",
            g.source
        );
        // Channel: substituted + protocol-prefixed (unchanged behaviour).
        assert!(g.source.contains("ca://DEV:go"), "{}", g.source);
        // Message-button caption (a user-visible string, not a channel).
        assert!(
            g.source.contains("\"DEV: Start\""),
            "message-button label macro not expanded:\n{}",
            g.source
        );
        // The IR is macro-free after the pass — no raw `$(P)` leaks anywhere.
        assert!(
            !g.source.contains("$(P)"),
            "raw macro leaked:\n{}",
            g.source
        );
    }

    #[test]
    fn string_format_maps_to_display_format_string() {
        // `adl2pydm`'s write_display_format sets displayFormat=String for text
        // update / text entry on exactly two conditions: an explicit
        // `format="string"`, or a long-string ($-suffixed) PV. Everything else
        // keeps the Default format (no builder emitted).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"text update" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	monitor {
		chan="$(P)desc"
		clr=0
	}
	format="string"
}
"text entry" {
	object {
		x=0
		y=30
		width=120
		height=20
	}
	control {
		chan="$(P)name$"
	}
}
"text update" {
	object {
		x=0
		y=60
		width=80
		height=18
	}
	monitor {
		chan="$(P)rbv"
		clr=0
	}
	format="decimal"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The string-format update and the $-suffixed entry both get the builder.
        assert_eq!(
            g.source
                .matches(".with_format(DisplayFormat::String)")
                .count(),
            2,
            "format=string text update + $-suffixed text entry must both map to \
             DisplayFormat::String:\n{}",
            g.source
        );
        // The `format="decimal"` update must NOT get a string-format builder
        // (Default is the only other format adl2pydm emits for these widgets).
        // The unexpanded `$(P)` expands at runtime against the instance table.
        assert!(
            g.source
                .contains("RsdmLabel::new(&engine, __m.expand(\"ca://$(P)rbv\").as_str())"),
            "decimal text update should still be emitted:\n{}",
            g.source
        );
    }

    #[test]
    fn exponential_and_hex_formats_map_to_rsdm_and_the_rest_warn() {
        // R2-65: `exponential`/`hexadecimal` have exact rsdm surfaces; the
        // formats with no surface warn instead of silently rendering fixed-point.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"text update" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	monitor {
		chan="$(P)e"
		clr=0
	}
	format="exponential"
}
"text update" {
	object {
		x=0
		y=20
		width=80
		height=18
	}
	monitor {
		chan="$(P)h"
		clr=0
	}
	format="hexadecimal"
}
"text entry" {
	object {
		x=0
		y=40
		width=80
		height=18
	}
	control {
		chan="$(P)o"
	}
	format="octal"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert_eq!(
            g.source
                .matches(".with_format(DisplayFormat::Exponential)")
                .count(),
            1,
            "format=exponential must map to DisplayFormat::Exponential:\n{}",
            g.source
        );
        assert_eq!(
            g.source.matches(".with_format(DisplayFormat::Hex)").count(),
            1,
            "format=hexadecimal must map to DisplayFormat::Hex:\n{}",
            g.source
        );
        // `octal` has no rsdm surface: no builder, but a warning (never silent).
        assert!(
            !g.source.contains("DisplayFormat::Octal"),
            "octal has no rsdm surface and must not be emitted:\n{}",
            g.source
        );
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("text format \"octal\"") && w.contains("no rsdm equivalent")),
            "octal must warn, not silently drop: {:?}",
            g.warnings
        );
    }

    #[test]
    fn unbound_macros_expand_at_runtime_against_the_instance_table() {
        // A `$(macro)` that survives convert-time baking (the related-display
        // child path, where values only exist at runtime) is expanded against
        // the instance's `MacroTable`; a fully grounded screen carries none of
        // that machinery.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=60
		height=20
	}
	"basic attribute" {
		clr=0
	}
	textix="Unit $(U)"
}
"text update" {
	object {
		x=0
		y=30
		width=80
		height=18
	}
	monitor {
		chan="$(P)val"
		clr=0
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The unbound channel and caption expand at runtime...
        assert!(
            g.source
                .contains("RsdmLabel::new(&engine, __m.expand(\"ca://$(P)val\").as_str())"),
            "unbound channel must expand at runtime:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("egui::RichText::new(__m.expand(\"Unit $(U)\").as_str())"),
            "unbound caption must expand at runtime:\n{}",
            g.source
        );
        // ...so the table machinery is emitted: the helper, the field, the
        // binding, and the `ui()` destructure (a draw body uses `__m`).
        assert!(g.source.contains("pub struct MacroTable"), "{}", g.source);
        assert!(g.source.contains("    __m: MacroTable,"), "{}", g.source);
        assert!(
            g.source.contains("let __m = MacroTable(macros);"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains("let Self { _engine: _, __m,"),
            "{}",
            g.source
        );

        // A screen whose macros appear only in channel addresses (no draw body
        // expands anything) discards the table in `ui()`'s destructure so the
        // generated module stays warning-clean.
        let ctor_only = generate(
            &parse(
                r#"
"text update" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	monitor {
		chan="$(P)val"
		clr=0
	}
}
"#,
            ),
            &Options::default(),
        );
        assert!(
            ctor_only.source.contains("let Self { _engine: _, __m: _,"),
            "a new()-only macro table must be discarded in ui():\n{}",
            ctor_only.source
        );

        // Partially bound: the `-m` table bakes `$(P)` and is also passed as
        // the root instance's runtime table (it still owes `$(U)`).
        let partial = generate(
            &parse(adl),
            &Options {
                macros: vec![("P".to_string(), "X:".to_string())],
                ..Options::default()
            },
        );
        assert!(
            partial
                .source
                .contains("RsdmLabel::new(&engine, \"ca://X:val\")"),
            "baked channel must stay a literal:\n{}",
            partial.source
        );
        assert!(
            partial.source.contains(
                "Self::new_in(&cc.egui_ctx, Some(rs), vec![(\"P\".to_string(), \"X:\".to_string())])"
            ),
            "the -m table must be the root instance's runtime table:\n{}",
            partial.source
        );

        // Fully grounded: no MacroTable anywhere, and `new_in` ignores its
        // macros parameter.
        let grounded = generate(
            &parse(adl),
            &Options {
                macros: vec![
                    ("P".to_string(), "X:".to_string()),
                    ("U".to_string(), "mV".to_string()),
                ],
                ..Options::default()
            },
        );
        assert!(
            !grounded.source.contains("MacroTable"),
            "a grounded screen must not carry the macro table:\n{}",
            grounded.source
        );
        assert!(
            grounded
                .source
                .contains("Self::new_in(&cc.egui_ctx, Some(rs), Vec::new())"),
            "{}",
            grounded.source
        );
        assert!(
            grounded.source.contains("_macros: Vec<(String, String)>,"),
            "{}",
            grounded.source
        );
    }

    #[test]
    fn display_background_color_is_painted_behind_everything() {
        // The display block's bclr fills the whole screen, painted as the first
        // (bottom-most) Background-order Area, before any widget.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
		0000ff,
	}
}
display {
	object {
		x=0
		y=0
		width=100
		height=80
	}
	clr=0
	bclr=2
}
text {
	object {
		x=10
		y=10
		width=60
		height=18
	}
	"basic attribute" {
		clr=1
	}
	textix="hi"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The screen background fills the native 100x80 with bclr=2 (blue).
        assert!(
            g.source
                .contains("ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(0, 0, 255));"),
            "display bclr must paint the screen background:\n{}",
            g.source
        );
        // It is painted before the static text (the bg place() precedes the label).
        let bg = g.source.find("__sbg").expect("screen bg");
        let label = g.source.find("ui.label(").expect("static text");
        assert!(
            bg < label,
            "the screen background must be painted before any widget:\n{}",
            g.source
        );
        // It is a Background-order Area at the native screen size (scaled by the
        // default responsive layout's sx/sy like every placement).
        assert!(g.source.contains(
            "place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 100.0, 80.0,"
        ));
    }

    #[test]
    fn static_colors_tint_text_widgets_but_not_shapes() {
        // MEDM clr (foreground) -> override_text_color; bclr (background) -> a
        // filled rect behind the widget. Applied to text/control widgets where
        // clr/bclr mean text+fill; NOT to shapes (which colour themselves via
        // drawing builders, not override_text_color).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
	}
}
"text update" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	monitor {
		chan="$(P)rbv"
		clr=2
		bclr=1
	}
}
rectangle {
	object {
		x=0
		y=30
		width=40
		height=40
	}
	"basic attribute" {
		clr=2
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The text update tints its text (clr=2 -> red) and fills its background
        // (bclr=1 -> black).
        assert!(
            g.source
                .contains("__v.override_text_color = Some(Color32::from_rgb(255, 0, 0));"),
            "text update must tint via override_text_color:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("ui.painter().rect_filled(__bg, egui::CornerRadius::ZERO, Color32::from_rgb(0, 0, 0));"),
            "text update must paint its bclr background:\n{}",
            g.source
        );
        // bclr also reaches the faces of self-painting widgets (Button/ComboBox/
        // DragValue via weak_bg_fill, TextEdit via text_edit_bg_color) so a
        // button/edit face shows the MEDM colour, not the egui theme's.
        assert!(
            g.source
                .contains("__v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0, 0, 0);"),
            "bclr must set the widget face fill:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("__v.text_edit_bg_color = Some(Color32::from_rgb(0, 0, 0));"),
            "bclr must set the text-edit face fill:\n{}",
            g.source
        );
        // Exactly one override_text_color — the shape must NOT get one (it colours
        // itself through with_fill/with_border, not text tinting).
        assert_eq!(
            g.source.matches("override_text_color").count(),
            1,
            "only the text widget should tint text; the shape self-colours:\n{}",
            g.source
        );
    }

    #[test]
    fn clrmod_alarm_maps_to_alarm_sensitive_content() {
        // MEDM clrmod="alarm" on a text update colours the foreground by alarm
        // severity; rsdm's alarm_sensitive_content defaults off, so it must be
        // set explicitly. The default (no clrmod / clrmod="static") emits nothing.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"text update" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	monitor {
		chan="$(P)alarmPV"
		clr=0
	}
	clrmod="alarm"
}
"text update" {
	object {
		x=0
		y=30
		width=80
		height=18
	}
	monitor {
		chan="$(P)staticPV"
		clr=0
	}
	clrmod="static"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Exactly one widget (the alarm one) gets the builder.
        assert_eq!(
            g.source
                .matches(".with_alarm_sensitive_content(true)")
                .count(),
            1,
            "only the clrmod=alarm text update should be alarm-sensitive:\n{}",
            g.source
        );
        // The MEDM palette rides along: clrmod=alarm replaces the foreground
        // for every severity (NO_ALARM → Green3), unlike PyDM's tint-in-alarm.
        assert_eq!(
            g.source
                .matches(".with_alarm_palette(AlarmPalette::Medm)")
                .count(),
            1,
            "the clrmod=alarm text update must use the MEDM palette:\n{}",
            g.source
        );
        // Both PVs are still emitted (the static one just keeps its default).
        assert!(g.source.contains("ca://$(P)alarmPV"));
        assert!(g.source.contains("ca://$(P)staticPV"));
    }

    #[test]
    fn clrmod_alarm_on_a_controller_wires_severity_content() {
        // R2-67: clrmod="alarm" on a controller now colours its foreground by
        // severity with MEDM's palette — the same wiring as the monitor widgets,
        // no longer a warned silent drop.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"text entry" {
	object {
		x=0
		y=0
		width=80
		height=18
	}
	control {
		chan="$(P)set"
		clr=0
	}
	clrmod="alarm"
}
valuator {
	object {
		x=0
		y=30
		width=120
		height=20
	}
	control {
		chan="$(P)lvl"
		clr=0
	}
	clrmod="alarm"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Both controllers (text entry + valuator) alarm-wire their content.
        assert_eq!(
            g.source
                .matches(".with_alarm_sensitive_content(true)")
                .count(),
            2,
            "both controllers alarm-wired:\n{}",
            g.source
        );
        assert!(
            g.source.contains(".with_alarm_palette(AlarmPalette::Medm)"),
            "controllers use MEDM's alarm palette:\n{}",
            g.source
        );
        // The old "not wired" warning is gone — the drop is closed, not warned.
        assert!(
            !g.warnings.iter().any(|w| w.contains("not wired")),
            "alarm clrmod is wired, not warned: {:?}",
            g.warnings
        );
    }

    #[test]
    fn valuator_without_alarm_clrmod_turns_off_default_content_sensitivity() {
        // RsdmSlider ships alarm_sensitive_content ON (PyDM parity); a MEDM
        // valuator without clrmod="alarm" must cancel it so it does not
        // alarm-colour with no MEDM basis.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
valuator {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	control {
		chan="$(P)lvl"
		clr=0
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains(".with_alarm_sensitive_content(false)"),
            "valuator default content sensitivity turned off:\n{}",
            g.source
        );
        assert!(
            !g.source.contains(".with_alarm_sensitive_content(true)"),
            "no alarm colouring without clrmod=alarm:\n{}",
            g.source
        );
    }

    #[test]
    fn pre_2_2_rolling_attributes_apply_to_graphics() {
        // R2-63: for versionNumber < 20200 MEDM rolls top-level attribute blocks
        // into each later graphic (display.c:487,507-546): the basic attribute
        // persists, the dynamic attribute is consumed by the first graphic.
        let adl = r#"
file {
	name="old.adl"
	version=020112
}
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=10
		height=10
	}
}
"basic attribute" {
	attr {
		clr=2
		fill="outline"
		width=3
	}
}
"dynamic attribute" {
	attr {
		mod {
			vis="if not zero"
		}
		param {
			chan="GATE:PV"
		}
	}
}
rectangle {
	object {
		x=20
		y=0
		width=10
		height=10
	}
}
rectangle {
	object {
		x=40
		y=0
		width=10
		height=10
	}
}
"#;
        let s = parse(adl);
        // Graphic BEFORE any block: basicAttributeInit — clr=0 (colormap[0]).
        assert_eq!(
            s.widgets[0].color,
            Some(crate::adl_parser::Color {
                r: 255,
                g: 255,
                b: 255
            }),
            "pre-block graphic takes basicAttributeInit clr=0"
        );
        // Both later rectangles inherit the basic attribute (persists) …
        for w in &s.widgets[1..3] {
            assert_eq!(
                w.color,
                Some(crate::adl_parser::Color { r: 255, g: 0, b: 0 }),
                "rolling clr=2 must land on every later graphic"
            );
            let basic = &w.attributes["basic attribute"];
            assert_eq!(basic.get("fill").map(String::as_str), Some("outline"));
            assert_eq!(basic.get("width").map(String::as_str), Some("3"));
        }
        // … but only the FIRST consumes the dynamic attribute (chan cleared).
        let dyn1 = &s.widgets[1].attributes["dynamic attribute"];
        assert_eq!(dyn1.get("chan").map(String::as_str), Some("GATE:PV"));
        assert_eq!(dyn1.get("vis").map(String::as_str), Some("if not zero"));
        assert!(
            !s.widgets[2].attributes.contains_key("dynamic attribute"),
            "dynamic attribute is consumed once, not re-applied"
        );
        // End-to-end: the vis rule wires the calc:// visibility gate.
        let g = generate(&s, &Options::default());
        assert!(
            g.source.contains("expr=A#0&A=ca://GATE:PV"),
            "old-format vis must reach the emitted gate:\n{}",
            g.source
        );

        // A modern file ignores stray top-level attribute blocks (MEDM >= 20200
        // never routes them into rolling state).
        let modern = adl.replace("020112", "030111");
        let s2 = parse(&modern);
        assert!(
            !s2.widgets[1].attributes.contains_key("basic attribute"),
            "modern files must not inherit top-level attribute blocks"
        );
    }

    #[test]
    fn pre_2_2_rolling_state_threads_composites_and_resets_per_block() {
        // R2-63 boundaries: (1) composite children inherit the rolling state —
        // MEDM parses them through the same parseAndAppendDisplayList; (2) each
        // basic-attribute block RESETS to defaults before parsing
        // (parseOldBasicAttribute calls basicAttributeInit first), so keys from
        // the previous block do not leak forward.
        let adl = r#"
file {
	name="old.adl"
	version=020112
}
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
	}
}
"basic attribute" {
	attr {
		clr=2
		width=5
	}
}
composite {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	chan=""
	children {
		rectangle {
			object {
				x=0
				y=0
				width=10
				height=10
			}
		}
	}
}
"basic attribute" {
	attr {
		clr=1
	}
}
rectangle {
	object {
		x=20
		y=0
		width=10
		height=10
	}
}
"#;
        let s = parse(adl);
        // (1) The composite child inherited the first block through the recursion.
        let child = &s.widgets[0].children[0];
        assert_eq!(
            child.color,
            Some(crate::adl_parser::Color { r: 255, g: 0, b: 0 }),
            "composite child must inherit the rolling basic attribute"
        );
        assert_eq!(
            child.attributes["basic attribute"]
                .get("width")
                .map(String::as_str),
            Some("5")
        );
        // (2) The second block reset the state: new clr, and width back to the
        // basicAttributeInit default (absent).
        let last = &s.widgets[1];
        assert_eq!(
            last.color,
            Some(crate::adl_parser::Color { r: 0, g: 0, b: 0 }),
            "the later basic-attribute block must replace the rolling colour"
        );
        assert!(
            !last.attributes["basic attribute"].contains_key("width"),
            "a reset must not leak width=5 from the previous block"
        );
    }

    #[test]
    fn widget_nested_old_attr_wrapper_parses_at_any_version() {
        // R2-63 (nested half): MEDM's key matching ignores brace depth, so the
        // pre-2.2 `attr {}` wrapper inside a widget-nested attribute block parses
        // in every version — including files stamped with a modern version.
        let adl = r#"
file {
	name="mixed.adl"
	version=030111
}
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=10
		height=10
	}
	"basic attribute" {
		attr {
			clr=1
			width=4
		}
	}
	"dynamic attribute" {
		attr {
			mod {
				vis="if zero"
			}
			param {
				chan="NEST:PV"
			}
		}
	}
}
"#;
        let s = parse(adl);
        assert_eq!(
            s.widgets[0].color,
            Some(crate::adl_parser::Color { r: 0, g: 255, b: 0 }),
            "nested attr{{}} clr must resolve into the widget colour"
        );
        assert_eq!(
            s.widgets[0].attributes["basic attribute"]
                .get("width")
                .map(String::as_str),
            Some("4")
        );
        let d = &s.widgets[0].attributes["dynamic attribute"];
        assert_eq!(d.get("vis").map(String::as_str), Some("if zero"));
        assert_eq!(d.get("chan").map(String::as_str), Some("NEST:PV"));
    }

    #[test]
    fn dynamic_attribute_clr_alarm_recolours_shapes_by_fill_mode() {
        // MEDM `dynamic attribute` clr="alarm" colours the object's drawing colour
        // by the channel's alarm severity: the BORDER for an outline shape (paints
        // only its pen), the FILL for a solid shape. The shape is bound to the
        // dynamic-attribute channel, so only the sensitivity flag is added.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
		fill="outline"
		width=2
	}
	"dynamic attribute" {
		clr="alarm"
		chan="$(P)statusA"
	}
}
oval {
	object {
		x=50
		y=0
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
		fill="solid"
	}
	"dynamic attribute" {
		clr="alarm"
		chan="$(P)statusB"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Outline rectangle → severity recolours the border.
        assert!(
            g.source.contains(".with_alarm_sensitive_border(true)"),
            "outline shape clr=alarm must recolour the border:\n{}",
            g.source
        );
        // Solid oval → severity recolours the fill (content).
        assert!(
            g.source.contains(".with_alarm_sensitive_content(true)"),
            "solid shape clr=alarm must recolour the fill:\n{}",
            g.source
        );
        // Both shapes draw from the MEDM palette (alarmColor is total).
        assert_eq!(
            g.source
                .matches(".with_alarm_palette(AlarmPalette::Medm)")
                .count(),
            2,
            "both clr=alarm shapes must use the MEDM palette:\n{}",
            g.source
        );
        // A drawing has no framed border (its border flag recolours the pen),
        // so the disconnect-only border mode is never emitted on shapes.
        assert!(!g.source.contains("with_border_mode"), "{}", g.source);
        // Each shape is bound to its dynamic-attribute channel (not a synthetic
        // loc:// placeholder), so alarm severity has a real source.
        assert!(g.source.contains("ca://$(P)statusA"));
        assert!(g.source.contains("ca://$(P)statusB"));
        // clr="alarm" is handled, so no warning is emitted for it.
        assert!(
            !g.warnings.iter().any(|w| w.contains("clr=\"alarm\"")),
            "clr=alarm on a shape must not warn: {:?}",
            g.warnings
        );
    }

    #[test]
    fn dynamic_attribute_clr_discrete_renders_as_static_no_warning() {
        // MEDM draws dynamic-attribute static graphics with `case STATIC: case
        // DISCRETE:` sharing the static colour (medm/medmRectangle.c et al.), so
        // discrete == static for a shape: keep the static colour, NO alarm builder,
        // and NO warning (it is rendered faithfully, not an unsupported gap).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
		fill="outline"
		width=2
	}
	"dynamic attribute" {
		clr="discrete"
		chan="$(P)mode"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            !g.source.contains("alarm_sensitive"),
            "discrete must not emit an alarm builder:\n{}",
            g.source
        );
        // The static border colour (clr=1 → 00ff00) is what MEDM draws for discrete.
        assert!(
            g.source
                .contains(".with_border(Color32::from_rgb(0, 255, 0)"),
            "discrete keeps the static border colour:\n{}",
            g.source
        );
        assert!(
            !g.warnings.iter().any(|w| w.contains("discrete")),
            "discrete on a shape renders faithfully (== static) and must not warn: {:?}",
            g.warnings
        );
    }

    #[test]
    fn static_text_clr_alarm_recolours_fixed_text_by_severity() {
        // A static `text` shows fixed glyphs, so clr="alarm" cannot reuse a
        // value-display widget. Instead the converter binds a Channel field and
        // recolours the fixed text by the channel's severity each frame, falling
        // back to the static colour (mirroring the visibility-gate pattern).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"basic attribute" {
		clr=1
	}
	"dynamic attribute" {
		clr="alarm"
		chan="$(P)stat"
	}
	textix="STATUS"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The fixed text is still emitted, coloured by the per-frame severity read.
        assert!(
            g.source
                .contains("egui::RichText::new(\"STATUS\").color(__c)")
        );
        // MEDM's alarmColor table is total (NO_ALARM → Green3), so the read
        // never falls back to the static colour.
        assert!(
            g.source
                .contains("read(|s| severity_color_medm(s.effective_severity()));"),
            "static text clr=alarm must read MEDM severity colour each frame:\n{}",
            g.source
        );
        assert!(g.source.contains("ca://$(P)stat"));
        // It is now wired, not a documented gap → no warning.
        assert!(
            !g.warnings.iter().any(|w| w.contains("clr=\"alarm\"")),
            "static text clr=alarm is wired and must not warn: {:?}",
            g.warnings
        );
    }

    #[test]
    fn static_text_clr_discrete_renders_as_static_no_warning() {
        // medm/medmText.c draws dynamic-attribute text with `case STATIC: case
        // DISCRETE:` sharing the static colour, so discrete == static for a `text`
        // widget: keep the static colour, no severity read, and NO warning.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"basic attribute" {
		clr=1
	}
	"dynamic attribute" {
		clr="discrete"
		chan="$(P)mode"
	}
	textix="STATE"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Keeps the static colour (no severity read, no alarm field).
        assert!(!g.source.contains("severity_color"));
        assert!(
            g.source
                .contains("egui::RichText::new(\"STATE\").color(Color32::")
        );
        assert!(
            !g.warnings.iter().any(|w| w.contains("discrete")),
            "static text discrete renders faithfully (== static) and must not warn: {:?}",
            g.warnings
        );
    }

    #[test]
    fn clrmod_alarm_makes_bar_and_byte_alarm_sensitive() {
        // clrmod="alarm" on a bar and a byte must set alarm-sensitivity on their
        // rsdm widgets (severity recolours the bar/lit bits); the static clr/bclr
        // stay as the NoAlarm fallback.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
bar {
	object {
		x=0
		y=0
		width=20
		height=100
	}
	monitor {
		chan="BAR"
		clr=1
	}
	clrmod="alarm"
}
byte {
	object {
		x=30
		y=0
		width=120
		height=20
	}
	monitor {
		chan="BYT"
		clr=1
	}
	clrmod="alarm"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Both widgets get the alarm builder; the static colours are still emitted.
        assert_eq!(
            g.source
                .matches(".with_alarm_sensitive_content(true)")
                .count(),
            2,
            "both the bar and the byte should be alarm-sensitive:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains(".with_bar_color(Color32::from_rgb(0, 255, 0))")
        );
        assert!(
            g.source
                .contains(".with_on_color(Color32::from_rgb(0, 255, 0))")
        );
    }

    #[test]
    fn medm_align_drives_text_and_text_update_alignment() {
        // A centered static text, a right-aligned text update, and a left-aligned
        // (default) static text. MEDM `align` → rsdm alignment; left emits nothing.
        let adl = r#"
"color map" {
	colors {
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	"basic attribute" {
		clr=0
	}
	textix="centered"
	align="horiz. centered"
}
"text update" {
	object {
		x=0
		y=30
		width=120
		height=20
	}
	monitor {
		chan="RB"
		clr=0
	}
	align="horiz. right"
}
text {
	object {
		x=0
		y=60
		width=120
		height=20
	}
	"basic attribute" {
		clr=0
	}
	textix="plain"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Static text centred → a top-down(Center) layout wraps its ui.label.
        assert!(
            g.source.contains(
                "ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| { ui.label(egui::RichText::new(\"centered\")"
            ),
            "centered static text not wrapped in a centering layout:\n{}",
            g.source
        );
        // Text update right-aligned → the RsdmLabel carries with_alignment(Right).
        assert!(
            g.source.contains(".with_alignment(TextAlign::Right)"),
            "right-aligned text update missing with_alignment:\n{}",
            g.source
        );
        // The plain (left) static text stays a bare ui.label — no layout wrapper,
        // and there is exactly one alignment wrapper (the centered one).
        assert!(
            g.source.contains(
                "ui.label(egui::RichText::new(\"plain\").color(Color32::from_rgb(0, 0, 0)));"
            ),
            "left-aligned static text must stay a bare ui.label:\n{}",
            g.source
        );
        assert_eq!(
            g.source.matches("egui::Layout::top_down(").count(),
            1,
            "only the centered static text should wrap in a layout:\n{}",
            g.source
        );
        // Every static text centres its row in the MEDM cell (the framed-widget
        // band) — the add_space runs before the alignment layout, so both the
        // centered and the plain emission carry it. The text update does not
        // (RsdmLabel centres internally), so exactly the two `text` widgets.
        assert_eq!(
            g.source
                .matches("ui.add_space(((ui.available_height() - __row) / 2.0).max(0.0));")
                .count(),
            2,
            "each static text must vertically centre its row:\n{}",
            g.source
        );
    }

    #[test]
    fn font_px_from_height_matches_adl2pydm_clamp() {
        // adl2pydm write_font_size: pointsize = int(max(6, min(20, round(h*0.6)))).
        assert_eq!(font_px_from_height(20), 12.0);
        assert_eq!(font_px_from_height(30), 18.0);
        // Clamp low: round(5*0.6)=3 → 6.0; high: round(100*0.6)=60 → 20.0.
        assert_eq!(font_px_from_height(5), 6.0);
        assert_eq!(font_px_from_height(100), 20.0);
    }

    #[test]
    fn text_widgets_carry_height_derived_font() {
        // A static text (height 20 → 12.0), a text update (height 30 → 18.0), and a
        // rectangle (no text → no font override). MEDM auto-sizes text to height.
        let adl = r#"
"color map" {
	colors {
		000000,
	}
}
text {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	"basic attribute" {
		clr=0
	}
	textix="hi"
}
"text update" {
	object {
		x=0
		y=30
		width=120
		height=30
	}
	monitor {
		chan="RB"
		clr=0
	}
}
rectangle {
	object {
		x=0
		y=70
		width=40
		height=40
	}
	"basic attribute" {
		clr=0
	}
}
"#;
        // Absolute mode keeps the font literals exact (the default responsive
        // layout appends `* sy`, covered by use_layout_scales_fonts_by_the_height_factor).
        let g = generate(
            &parse(adl),
            &Options {
                use_layout: false,
                ..Options::default()
            },
        );
        // Static text at its height-derived size.
        assert!(
            g.source.contains(
                "ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));"
            ),
            "static text missing height-derived font:\n{}",
            g.source
        );
        // Text update (channel widget) at its height-derived size.
        assert!(
            g.source.contains(
                "ui.style_mut().override_font_id = Some(egui::FontId::proportional(18.0));"
            ),
            "text update missing height-derived font:\n{}",
            g.source
        );
        // Exactly the two text-bearing widgets set a font; the rectangle does
        // not. Count the assignments — the static-text row centring also READS
        // `override_font_id` back.
        assert_eq!(
            g.source.matches("override_font_id = Some(").count(),
            2,
            "only text-bearing widgets should set a font override:\n{}",
            g.source
        );
    }

    #[test]
    fn lays_out_decoration_before_control() {
        // The z-order guarantee: the Background (decoration) place() must appear
        // before the Foreground (control) place() in the source, and the static
        // label must use Background while the line edit uses Foreground.
        let g = build(&Options::default());
        let deco = g
            .source
            .find("egui::Order::Background")
            .expect("background");
        let ctrl = g
            .source
            .find("egui::Order::Foreground")
            .expect("foreground");
        assert!(
            deco < ctrl,
            "decoration must be laid out before the control:\n{}",
            g.source
        );
    }

    #[test]
    fn use_layout_scales_placements_to_fill_the_area() {
        // Responsive mode (the default) binds the per-axis scale and threads it
        // into every place() call; `--absolute` (use_layout: false) does neither.
        let absolute = build(&Options {
            use_layout: false,
            ..Options::default()
        });
        assert!(
            !absolute.source.contains("let sx = avail.width()"),
            "absolute mode must not emit a scale:\n{}",
            absolute.source
        );
        assert!(
            !absolute.source.contains("place(ui, __origin, sx, sy,"),
            "absolute mode must not scale placements:\n{}",
            absolute.source
        );
        // Both modes position every placement against an explicitly captured
        // outer origin, so a frame's BORDER_INSET never shifts its children.
        assert!(
            absolute
                .source
                .contains("let __origin = ui.max_rect().min;"),
            "absolute mode must capture the screen origin:\n{}",
            absolute.source
        );

        let layout = build(&Options {
            use_layout: true,
            ..Options::default()
        });
        // The OVERLAP fixture has no `display` block, so the native size is the
        // bounding box of its widgets (max right edge 200, max bottom edge 100).
        assert!(
            layout.source.contains("let sx = avail.width() / 200.0;"),
            "expected width scale against the 200px bounding box:\n{}",
            layout.source
        );
        assert!(
            layout.source.contains("let sy = avail.height() / 100.0;"),
            "expected height scale against the 100px bounding box:\n{}",
            layout.source
        );
        // Every placement scales by (sx, sy), and the place helper takes them
        // (after the explicit origin).
        assert!(
            layout
                .source
                .contains("place(ui, __origin, sx, sy, egui::Order::")
        );
        assert!(layout.source.contains("let __origin = avail.min;"));
        assert!(layout.source.contains(
            "fn place(\n    ui: &mut egui::Ui,\n    origin: egui::Pos2,\n    sx: f32,\n    sy: f32,"
        ));
        assert!(
            layout
                .source
                .contains("egui::vec2(x * sx, y * sy), egui::vec2(w * sx, h * sy)")
        );
    }

    #[test]
    fn use_layout_scales_fonts_by_the_height_factor() {
        // MEDM re-derives a widget's font from its resized height, so responsive
        // mode (the default) scales every emitted font by `sy` (rects already
        // scale by sx/sy); absolute mode keeps the fixed MEDM-height-derived size.
        let absolute = build(&Options {
            use_layout: false,
            ..Options::default()
        });
        assert!(
            absolute.source.contains("egui::FontId::proportional(11.0)"),
            "absolute mode must keep the fixed font size:\n{}",
            absolute.source
        );
        assert!(
            !absolute.source.contains("* sy)"),
            "absolute mode must not scale fonts:\n{}",
            absolute.source
        );

        let layout = build(&Options {
            use_layout: true,
            ..Options::default()
        });
        // text update height 18 -> 11px; static text height 100 -> clamped 20px.
        assert!(
            layout
                .source
                .contains("egui::FontId::proportional(11.0 * sy)"),
            "layout mode must scale the text-update font by sy:\n{}",
            layout.source
        );
        assert!(
            layout
                .source
                .contains("egui::FontId::proportional(20.0 * sy)"),
            "layout mode must scale the static-text font by sy:\n{}",
            layout.source
        );
    }

    #[test]
    fn use_layout_takes_native_size_from_the_display_block() {
        // A screen WITH a `display` block scales against that geometry, not the
        // widget bounding box.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
display {
	object {
		x=0
		y=0
		width=640
		height=480
	}
}
text {
	object {
		x=10
		y=10
		width=80
		height=18
	}
	"basic attribute" {
		clr=1
	}
	textix="hi"
}
"#;
        let g = generate(
            &parse(adl),
            &Options {
                use_layout: true,
                ..Options::default()
            },
        );
        assert!(
            g.source.contains("let sx = avail.width() / 640.0;"),
            "expected the display block width (640):\n{}",
            g.source
        );
        assert!(
            g.source.contains("let sy = avail.height() / 480.0;"),
            "expected the display block height (480):\n{}",
            g.source
        );
    }

    #[test]
    fn unimplemented_widgets_warn_but_do_not_panic() {
        // A `polygon` with no `points` block is degenerate (fewer than 2
        // vertices), so it falls back to a placeholder marker + warning while the
        // screen still assembles — the real polygon path is covered separately.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
polygon {
	object {
		x=0
		y=0
		width=100
		height=20
	}
	"basic attribute" {
		clr=1
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(g.warnings.iter().any(|w| w.contains("polygon")));
        // Nothing emitted for it yet, but the screen still assembles.
        assert!(g.source.contains("pub struct Screen"));
    }

    /// One of each B5 control widget, each with the MEDM fields its emitter
    /// consumes (label/press for the button, stacking, limits, precision,
    /// format, byte bits).
    const CONTROLS: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"message button" {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	control {
		chan="MBB"
	}
	press_msg="1"
	release_msg="0"
	label="Go"
}
menu {
	object {
		x=0
		y=30
		width=80
		height=20
	}
	control {
		chan="MENU"
	}
}
"choice button" {
	object {
		x=0
		y=60
		width=80
		height=40
	}
	control {
		chan="CHO"
	}
	stacking="column"
}
valuator {
	object {
		x=0
		y=110
		width=120
		height=20
	}
	control {
		chan="VAL"
	}
	dPrecision=3
	limits {
		loprSrc="default"
		loprDefault=-5
		hoprSrc="default"
		hoprDefault=5
	}
}
"wheel switch" {
	object {
		x=0
		y=140
		width=120
		height=20
	}
	control {
		chan="WHL"
	}
	format="6.2"
}
byte {
	object {
		x=0
		y=170
		width=120
		height=20
	}
	monitor {
		chan="BYT"
	}
	sbit=3
	ebit=0
	direction="right"
}
"#;

    fn controls() -> Generated {
        generate(&parse(CONTROLS), &Options::default())
    }

    #[test]
    fn message_button_carries_label_and_press_release_values() {
        let g = controls();
        assert!(
            g.source
                .contains("RsdmPushButton::new(&engine, \"ca://MBB\", \"Go\", \"1\")"),
            "{}",
            g.source
        );
        assert!(g.source.contains(".with_release_value(\"0\")"));
    }

    #[test]
    fn menu_and_choice_button_map_to_enum_widgets() {
        let g = controls();
        assert!(
            g.source
                .contains("RsdmEnumComboBox::new(&engine, \"ca://MENU\")")
        );
        assert!(
            g.source
                .contains("RsdmEnumButton::new(&engine, \"ca://CHO\")")
        );
        // stacking="column" -> horizontal layout.
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Horizontal)")
        );
    }

    #[test]
    fn row_choice_button_sizes_its_font_per_button() {
        // R1-40: a row (vertical) stack shares the height among its buttons —
        // MEDM sizes each toggle at height/numberOfButtons
        // (medmChoiceButtons.c:131-136). A 4-item-shaped 80 px widget:
        // est = max(2, round(80/20)) = 4 -> per-button 20 px -> font 12, where
        // the whole-geometry rule gave clamp(48) = 20 (adl2pydm
        // output_handler.py:650-660).
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"choice button" {
	object {
		x=0
		y=0
		width=80
		height=80
	}
	control {
		chan="ROWCB"
	}
	stacking="row"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // (Responsive output scales the size by `sy`, so match the prefix.)
        assert!(
            g.source.contains("egui::FontId::proportional(12.0"),
            "row choice-button font must fit one button (12.0):\n{}",
            g.source
        );
        assert!(
            !g.source.contains("proportional(20.0"),
            "whole-geometry font must not survive on a row stack:\n{}",
            g.source
        );
        // The CONTROLS column stack keeps the full-height rule: its choice
        // button is the only 40 px-tall widget there, so the sole
        // clamp(round(24)) = 20 font in that screen is its.
        let g = controls();
        assert!(
            g.source.contains("proportional(20.0"),
            "column choice-button keeps the whole-height font:\n{}",
            g.source
        );
    }

    #[test]
    fn menu_centres_its_caption() {
        // A Motif option menu centres its caption (XmLabel's default
        // XmNalignment, never overridden by medmMenu.c), so every converted
        // menu carries the Center alignment — MEDM has no per-menu `align`.
        let g = controls();
        let combo = g
            .source
            .find("RsdmEnumComboBox::new(&engine, \"ca://MENU\")")
            .expect("menu ctor");
        assert!(
            g.source[combo..combo + 300].contains(".with_alignment(TextAlign::Center)"),
            "menu must centre its caption:\n{}",
            g.source
        );
    }

    #[test]
    fn valuator_emits_user_limits_and_precision() {
        let g = controls();
        assert!(g.source.contains("RsdmSlider::new(&engine, \"ca://VAL\")"));
        assert!(
            g.source.contains(".with_limits(-5.0, 5.0)"),
            "user-defined limits not emitted:\n{}",
            g.source
        );
        // dPrecision=3 -> with_precision(3).
        assert!(g.source.contains(".with_precision(3)"));
    }

    #[test]
    fn single_sided_valuator_limit_emits_a_per_bound_builder() {
        // R2-66: a valuator whose limits block pins only HOPR (hoprSrc="default")
        // emits .with_upper_limit and leaves LOPR channel-driven — no all-or-nothing
        // .with_limits, no warning.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
valuator {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	control {
		chan="$(P)v"
		clr=0
	}
	limits {
		hoprSrc="default"
		hoprDefault=42
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains(".with_upper_limit(42.0)"),
            "single-sided upper limit not emitted:\n{}",
            g.source
        );
        assert!(
            !g.source.contains(".with_limits("),
            "single-sided limit must not emit an all-or-nothing range:\n{}",
            g.source
        );
    }

    #[test]
    fn wheel_switch_format_sets_decimals() {
        let g = controls();
        assert!(g.source.contains("RsdmSpinbox::new(&engine, \"ca://WHL\")"));
        // format="6.2" -> 2 decimals.
        assert!(g.source.contains(".with_precision(2)"));
    }

    #[test]
    fn wheel_decimals_reads_medm_printf_and_bare_forms() {
        // R2-69: MEDM's real wheel-switch format is a printf spec handed to the Xc
        // widget; the decimals are the `.p` field, clamped to [0, width-1].
        assert_eq!(wheel_decimals("% 6.2f"), 2); // MEDM DEFAULT_FORMAT
        assert_eq!(wheel_decimals("%6.2f"), 2); // no flags
        assert_eq!(wheel_decimals("% 8.0f"), 0); // explicit 0 decimals
        assert_eq!(wheel_decimals("% 6f"), 0); // width-only (nparsed 1) -> 0
        assert_eq!(wheel_decimals("% 3.9f"), 2); // p clamped to width-1
        assert_eq!(wheel_decimals("integer"), 0); // adl2pydm/adl2rsdm convenience
        // R3-22: Xc compute_format never leaves precision to the channel — an
        // unparseable/degenerate format falls back to DEFAULT_FORMAT precision 2.
        assert_eq!(wheel_decimals("%f"), 2); // nparsed 0 (no width digits)
        assert_eq!(wheel_decimals("%.3f"), 2); // nparsed 0 (leading '.')
        assert_eq!(wheel_decimals("%g"), 2); // no `f` conversion -> DEFAULT
        assert_eq!(wheel_decimals("% 6d"), 2); // no `f` conversion -> DEFAULT
        assert_eq!(wheel_decimals("garbage"), 2); // no `%` -> DEFAULT
        assert_eq!(wheel_decimals("6.2"), 2); // no `%` -> DEFAULT (matches user's 2)
    }

    #[test]
    fn wheel_switch_takes_clr_bclr_but_slider_does_not() {
        // The spinbox renders its value as an uncoloured-RichText button, so MEDM
        // `clr` reaches the number via override_text_color and `bclr` fills behind
        // it. The slider's `clr` is a track/handle colour override_text_color can't
        // reach, so it is deliberately excluded (a rsdm-side gap).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
		0000ff,
	}
}
valuator {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	control {
		chan="VAL"
		clr=1
	}
}
"wheel switch" {
	object {
		x=0
		y=30
		width=120
		height=20
	}
	control {
		chan="WHL"
		clr=1
		bclr=2
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains("override_text_color = Some(Color32::from_rgb(255, 0, 0))"),
            "wheel switch clr must drive override_text_color:\n{}",
            g.source
        );
        assert!(
            g.source.contains(
                "rect_filled(__bg, egui::CornerRadius::ZERO, Color32::from_rgb(0, 0, 255))"
            ),
            "wheel switch bclr must fill behind it:\n{}",
            g.source
        );
        // Only the wheel switch contributes an override; the slider (also clr=1) is
        // excluded, so exactly one override_text_color appears.
        assert_eq!(
            g.source.matches("override_text_color").count(),
            1,
            "only the wheel switch (not the slider) may set override_text_color:\n{}",
            g.source
        );
    }

    #[test]
    fn byte_maps_bits_shift_and_orientation() {
        let g = controls();
        assert!(
            g.source
                .contains("RsdmByteIndicator::new(&engine, \"ca://BYT\")")
        );
        // sbit=3,ebit=0 -> num_bits = 4, shift = min = 0 (so no shift builder).
        assert!(g.source.contains(".with_num_bits(4)"), "{}", g.source);
        assert!(
            !g.source.contains(".with_shift("),
            "shift 0 must not emit a builder"
        );
        // direction="right" -> horizontal.
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Horizontal)")
        );
        // sbit=3 > ebit=0 -> MSB first (xc/Byte.c:551-552 draws bit sbit-i):
        // big-endian display order.
        assert!(
            g.source.contains(".with_big_endian(true)"),
            "sbit > ebit must display MSB first:\n{}",
            g.source
        );
        // The CONTROLS byte has no clr/bclr, so no on/off colour builders.
        assert!(
            !g.source.contains(".with_on_color(") && !g.source.contains(".with_off_color("),
            "a colourless byte must not force on/off colours:\n{}",
            g.source
        );
    }

    #[test]
    fn byte_on_off_colors_follow_medm_clr_and_bclr() {
        // MEDM byte `clr`/`bclr` are the on/off bit colours (adl2pydm onColor/
        // offColor). The parser hoists them into widget.color/background_color;
        // codegen emits with_on_color/with_off_color so the bits match MEDM
        // instead of rsdm's default green/grey.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
		0000ff,
	}
}
byte {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	monitor {
		chan="BYT"
		clr=1
		bclr=2
	}
	sbit=3
	ebit=0
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains(".with_on_color(Color32::from_rgb(255, 0, 0))"),
            "byte clr=1 (ff0000) must drive with_on_color:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains(".with_off_color(Color32::from_rgb(0, 0, 255))"),
            "byte bclr=2 (0000ff) must drive with_off_color:\n{}",
            g.source
        );
    }

    #[test]
    fn byte_with_sbit_below_ebit_is_lsb_first() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
byte {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	monitor {
		chan="BE"
	}
	sbit=0
	ebit=3
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // sbit=0,ebit=3 -> num_bits 4, shift 0, and LSB first: xc/Byte.c:513-515
        // sets `reverse` for ebit > sbit and draws segment i as bit sbit+i —
        // rsdm's little-endian default, so NO big-endian builder. (The old
        // mapping emitted with_big_endian here, matching adl2pydm's inverted
        // `bigEndian = sbit < ebit` instead of the C.)
        assert!(g.source.contains(".with_num_bits(4)"));
        assert!(
            !g.source.contains(".with_big_endian"),
            "sbit < ebit displays LSB first — no big-endian builder:\n{}",
            g.source
        );
    }

    #[test]
    fn byte_without_sbit_ebit_shows_sixteen_bits_msb_first() {
        // MEDM defaults sbit=15, ebit=0 (medmByte.c:279-280; writeDlByte
        // :366-369 omits exactly those values from the .adl) -> 16 bits, no
        // shift, MSB first.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
byte {
	object {
		x=0
		y=0
		width=160
		height=20
	}
	monitor {
		chan="DFLT"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains(".with_num_bits(16)"),
            "default byte must show 16 bits:\n{}",
            g.source
        );
        assert!(
            !g.source.contains(".with_shift("),
            "default ebit=0 must not emit a shift:\n{}",
            g.source
        );
        assert!(
            g.source.contains(".with_big_endian(true)"),
            "default 15..0 displays MSB first:\n{}",
            g.source
        );
    }

    #[test]
    fn controls_are_foreground_and_byte_is_middle() {
        // Controls (button/menu/choice/valuator/wheel) layer Foreground; byte is
        // a monitor (Middle). The decoration-behind-controls rule again.
        let g = controls();
        assert!(g.source.contains("egui::Order::Foreground"));
        assert!(g.source.contains("egui::Order::Middle"));
    }

    /// A bar (vertical, user limits, label="limits") plus a meter (default) and
    /// an indicator — the three scale-indicator widgets.
    const SCALES: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
bar {
	object {
		x=0
		y=0
		width=20
		height=100
	}
	monitor {
		chan="BAR"
	}
	label="limits"
	direction="up"
	limits {
		loprSrc="default"
		loprDefault=0
		hoprSrc="default"
		hoprDefault=100
		precSrc="default"
		precDefault=1
	}
}
meter {
	object {
		x=30
		y=0
		width=80
		height=80
	}
	monitor {
		chan="MTR"
	}
}
indicator {
	object {
		x=120
		y=0
		width=100
		height=20
	}
	monitor {
		chan="IND"
	}
}
"#;

    fn scales() -> Generated {
        generate(&parse(SCALES), &Options::default())
    }

    #[test]
    fn bar_is_a_bar_indicator_with_limits_orientation_and_precision() {
        let g = scales();
        assert!(
            g.source
                .contains("RsdmScaleIndicator::new(&engine, \"ca://BAR\")"),
            "{}",
            g.source
        );
        assert!(g.source.contains(".with_bar_indicator(true)"));
        assert!(g.source.contains(".with_limits(0.0, 100.0)"));
        // direction="up" -> vertical (the non-default orientation for a scale).
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Vertical)")
        );
        assert!(g.source.contains(".with_precision(1)"));
        // label="limits" -> value label shown, so the BAR's chain has no
        // with_value_label(false) (the label-less meter/indicator do).
        let bar_ctor = g
            .source
            .split("RsdmScaleIndicator::new(&engine, \"ca://BAR\")")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .expect("bar ctor");
        assert!(
            !bar_ctor.contains(".with_value_label(false)"),
            "label=\"limits\" must keep the value label:\n{bar_ctor}"
        );
    }

    #[test]
    fn label_less_meter_and_indicator_hide_the_value_label() {
        // R1-39: valueVisible is TRUE only for label="limits"/"channel" on ALL
        // three scale monitors (medmIndicator.c:122-140, medmMeter.c:134-148),
        // not just the bar. SCALES' meter and indicator carry no label.
        let g = scales();
        assert_eq!(
            g.source.matches(".with_value_label(false)").count(),
            2,
            "meter + indicator must both hide the value label:\n{}",
            g.source
        );
    }

    #[test]
    fn meter_and_indicator_are_pointer_scales() {
        let g = scales();
        assert!(
            g.source
                .contains("RsdmScaleIndicator::new(&engine, \"ca://MTR\")")
        );
        assert!(
            g.source
                .contains("RsdmScaleIndicator::new(&engine, \"ca://IND\")")
        );
        // Neither is a bar: exactly one `.with_bar_indicator(true)` (the bar).
        assert_eq!(g.source.matches(".with_bar_indicator(true)").count(), 1);
    }

    #[test]
    fn scale_indicator_bar_color_follows_medm_clr() {
        // A bar's `monitor` block `clr` is its bar/pointer colour. The parser
        // hoists it into `widget.color`; codegen must emit `.with_bar_color(...)`
        // so the bar matches MEDM instead of rsdm's default blue.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
bar {
	object {
		x=0
		y=0
		width=20
		height=100
	}
	monitor {
		chan="BAR"
		clr=1
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains(".with_bar_color(Color32::from_rgb(0, 255, 0))"),
            "bar clr=1 (00ff00) must drive with_bar_color:\n{}",
            g.source
        );
        // A scale with no `clr` keeps rsdm's default bar colour (no override).
        assert!(
            !scales().source.contains(".with_bar_color("),
            "a clr-less scale must not force a bar colour:\n{}",
            scales().source
        );
    }

    #[test]
    fn bar_without_value_label_hides_it() {
        // A bar with no `label` decoration hides the value label (PyDM default),
        // unlike the RsDM default which shows it.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
bar {
	object {
		x=0
		y=0
		width=20
		height=100
	}
	monitor {
		chan="B"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains(".with_value_label(false)"),
            "{}",
            g.source
        );
    }

    #[test]
    fn bar_fillmod_from_center_anchors_the_fill_on_the_midpoint() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
bar {
	object {
		x=0
		y=0
		width=100
		height=20
	}
	monitor {
		chan="CTR"
	}
	fillmod="from center"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // fillmod="from center" (medmBar.c:496-502) -> origin-at-center fill
        // (xc/BarGraph.c anchors at mid = len/2).
        assert!(
            g.source.contains(".with_origin_at_center(true)"),
            "{}",
            g.source
        );
        // The default "from edge" (and absence) must not emit the builder:
        // SCALES' bar has no fillmod.
        assert!(
            !scales().source.contains(".with_origin_at_center"),
            "{}",
            scales().source
        );
    }

    #[test]
    fn scale_indicators_are_monitors_in_the_middle_layer() {
        let g = scales();
        assert!(g.source.contains("egui::Order::Middle"));
        assert!(!g.source.contains("egui::Order::Foreground"));
    }

    // R1-37: MEDM `direction` on the valuator (orientation) and the bar
    // (orientation + inverted fill for down/left).
    #[test]
    fn valuator_direction_up_makes_the_slider_vertical() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
valuator {
	object {
		x=0
		y=0
		width=24
		height=180
	}
	control {
		chan="VAL"
	}
	direction="up"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains("RsdmSlider::new(&engine, \"ca://VAL\")\n            .expect"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Vertical)"),
            "valuator direction=up must turn the slider vertical:\n{}",
            g.source
        );
        // up is not reversed — no warning.
        assert!(
            !g.warnings.iter().any(|w| w.contains("reversed max-end")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn valuator_direction_down_is_vertical_with_a_reversal_warning() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
valuator {
	object {
		x=0
		y=0
		width=24
		height=180
	}
	control {
		chan="VAL"
	}
	direction="down"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Vertical)"),
            "{}",
            g.source
        );
        // MEDM's XmMAX_ON_BOTTOM reversal has no rsdm/PyDM slider surface.
        assert!(
            g.warnings.iter().any(|w| w.contains("reversed max-end")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn bar_direction_down_and_left_invert_the_fill() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
bar {
	object {
		x=0
		y=0
		width=20
		height=100
	}
	monitor {
		chan="B1"
	}
	direction="down"
}
bar {
	object {
		x=30
		y=0
		width=100
		height=20
	}
	monitor {
		chan="B2"
	}
	direction="left"
}
indicator {
	object {
		x=0
		y=120
		width=20
		height=100
	}
	monitor {
		chan="I1"
	}
	direction="down"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // bar down: vertical + inverted (medmBar.c:184 XcVertDown).
        assert!(
            g.source.contains(
                ".with_orientation(Orientation::Vertical)\n            .with_inverted_appearance(true)"
            ),
            "bar direction=down must be vertical + inverted:\n{}",
            g.source
        );
        // bar left: horizontal is the scale default (no orientation builder),
        // inverted fill only (medmBar.c:175 XcHorizLeft).
        // indicator down: vertical but NOT inverted — MEDM overrides down to up
        // for the Scale Monitor (medmIndicator.c:142-150).
        assert_eq!(
            g.source.matches(".with_inverted_appearance(true)").count(),
            2,
            "exactly the two bars invert (never the indicator):\n{}",
            g.source
        );
        assert_eq!(
            g.source
                .matches(".with_orientation(Orientation::Vertical)")
                .count(),
            2,
            "bar down + indicator down are vertical:\n{}",
            g.source
        );
    }

    /// A solid filled rectangle, an outline-only oval, and a dynamic-attribute
    /// rectangle bound to a channel — the three drawing shapes.
    const SHAPES: &str = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=40
		height=20
	}
	"basic attribute" {
		clr=1
		style="solid"
		fill="solid"
		width=2
	}
}
oval {
	object {
		x=50
		y=0
		width=30
		height=30
	}
	"basic attribute" {
		clr=1
		fill="outline"
		width=0
	}
}
rectangle {
	object {
		x=90
		y=0
		width=40
		height=20
	}
	"basic attribute" {
		clr=1
		fill="solid"
	}
	"dynamic attribute" {
		chan="$(P)STATE"
	}
}
"#;

    fn shapes() -> Generated {
        generate(&parse(SHAPES), &Options::default())
    }

    #[test]
    fn solid_rectangle_fills_with_color_and_border_from_width() {
        let g = shapes();
        assert!(
            g.source
                .contains("RsdmDrawing::new(&engine, \"loc://adl2rsdm_shape_"),
            "channel-less rectangle should use a loc:// placeholder:\n{}",
            g.source
        );
        assert!(g.source.contains("DrawingShape::Rectangle"));
        // clr=1 -> ff0000 (red); fill=solid -> with_fill(red); width=2 -> border.
        assert!(
            g.source
                .contains(".with_fill(Color32::from_rgb(255, 0, 0))")
        );
        assert!(
            g.source
                .contains(".with_border(Color32::from_rgb(255, 0, 0), 2.0)")
        );
        // Sized to the MEDM 40x20 geometry, not RsdmDrawing's 40x40 default.
        assert!(
            g.source.contains(".with_size(egui::Vec2::new(40.0, 20.0))"),
            "{}",
            g.source
        );
    }

    #[test]
    fn outline_oval_is_transparent_with_a_forced_border() {
        let g = shapes();
        assert!(g.source.contains("DrawingShape::Ellipse"));
        assert!(g.source.contains(".with_fill(Color32::TRANSPARENT)"));
        // width=0 + outline -> forced to 1.0 so the outline shows.
        assert!(
            g.source
                .contains(".with_border(Color32::from_rgb(255, 0, 0), 1.0)"),
            "{}",
            g.source
        );
        // Sized to the MEDM 30x30 geometry.
        assert!(
            g.source.contains(".with_size(egui::Vec2::new(30.0, 30.0))"),
            "{}",
            g.source
        );
    }

    #[test]
    fn dynamic_attribute_rectangle_binds_its_channel() {
        let opts = Options {
            macros: vec![("P".to_string(), "DEV:".to_string())],
            ..Options::default()
        };
        let g = generate(&parse(SHAPES), &opts);
        assert!(
            g.source
                .contains("RsdmDrawing::new(&engine, \"ca://DEV:STATE\", DrawingShape::Rectangle)"),
            "dynamic-attribute channel not bound:\n{}",
            g.source
        );
    }

    #[test]
    fn drawings_are_decoration_in_the_background_layer() {
        let g = shapes();
        assert!(g.source.contains("egui::Order::Background"));
        assert!(!g.source.contains("egui::Order::Foreground"));
    }

    /// A composite at (120, 10) grouping a decoration rectangle and a text-entry
    /// control, both in absolute screen coordinates.
    const COMPOSITE: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
composite {
	object {
		x=120
		y=10
		width=80
		height=40
	}
	"composite name"=""
	vis="static"
	chan=""
	children {
		rectangle {
			object {
				x=120
				y=10
				width=80
				height=40
			}
			"basic attribute" {
				clr=1
				fill="outline"
			}
		}
		"text entry" {
			object {
				x=150
				y=20
				width=40
				height=18
			}
			control {
				chan="SET"
			}
		}
	}
}
"#;

    fn composite() -> Generated {
        generate(&parse(COMPOSITE), &Options::default())
    }

    #[test]
    fn composite_becomes_a_frame_holding_its_children() {
        let g = composite();
        // The frame (loc:// placeholder, no chan) plus both children are fields.
        assert!(
            g.source
                .contains("RsdmFrame::new(&engine, \"loc://adl2rsdm_frame_"),
            "{}",
            g.source
        );
        assert!(g.source.contains(": RsdmFrame,"));
        assert!(g.source.contains(": RsdmDrawing,"));
        assert!(g.source.contains(": RsdmLineEdit,"));
    }

    #[test]
    fn composite_children_draw_inside_the_frame_closure() {
        let g = composite();
        // The frame's show takes a closure; the children's place() calls sit
        // inside it (the `.show(ui, |ui| {` appears before the child draws).
        let frame_show = g
            .source
            .find(".show(ui, |ui| {")
            .expect("frame show closure");
        let child_draw = g.source.find(".show(ui);").expect("a child draw");
        assert!(
            frame_show < child_draw,
            "children must draw inside the frame closure:\n{}",
            g.source
        );
    }

    #[test]
    fn composite_children_are_translated_to_frame_relative_coordinates() {
        let g = composite();
        // text entry at absolute (150, 20), composite origin (120, 10) ->
        // relative (30, 10) inside the frame.
        assert!(
            g.source.contains("30.0, 10.0, 40.0, 18.0"),
            "child not translated to frame-relative coords:\n{}",
            g.source
        );
        // The rectangle child at (120,10) == composite origin -> (0, 0).
        assert!(g.source.contains("0.0, 0.0, 80.0, 40.0"));
    }

    #[test]
    fn composite_children_use_frame_outer_origin_immune_to_inset() {
        // L1: RsdmFrame::show insets its interior by BORDER_INSET; positioning
        // children off the inner ui would shift them. Instead the frame captures
        // its OUTER origin before `show`, and children place against that — so the
        // inset never moves a child, and codegen never hardcodes BORDER_INSET.
        let g = composite();
        let capture = g
            .source
            .find("let __frame_origin_")
            .expect("frame must capture its outer origin");
        let show = g.source.find(".show(ui, |ui| {").expect("frame show");
        assert!(
            capture < show,
            "the outer origin must be captured BEFORE show insets the interior:\n{}",
            g.source
        );
        // Children position against the captured frame origin, not the screen one.
        assert!(
            g.source.contains("place(ui, __frame_origin_"),
            "frame children must place against the captured frame origin:\n{}",
            g.source
        );
    }

    #[test]
    fn composite_splits_children_into_one_placement_per_layer() {
        let g = composite();
        // A composite spanning two layers (a Background rectangle + a Foreground
        // text entry) emits ONE placement per layer, each at the frame's outer
        // rect: the lowest layer (Background) hosts the frame shell and its
        // closure draws the decoration; the control is a SEPARATE Foreground
        // placement on its own layer, NOT nested in the closure -- so each
        // child's Area is created at its own layer's statement position and file
        // order holds per layer (the structural fix for a control inside a
        // composite stacking wrong against an earlier same-layer sibling).
        let frame_place = g
            .source
            .find("egui::Order::Background")
            .expect("frame background place");
        let frame_show = g.source.find(".show(ui, |ui| {").expect("frame show");
        assert!(frame_place < frame_show, "{}", g.source);
        // The decoration (frame-relative 0,0) draws inside the shell closure.
        let deco = g
            .source
            .find("0.0, 0.0, 80.0, 40.0")
            .expect("decoration place");
        assert!(
            frame_show < deco,
            "the decoration must draw inside the frame shell:\n{}",
            g.source
        );
        // The control is its own Foreground placement, after the decoration.
        let control_layer = g
            .source
            .find("egui::Order::Foreground")
            .expect("control layer");
        let control = g
            .source
            .find("30.0, 10.0, 40.0, 18.0")
            .expect("control place");
        assert!(
            deco < control_layer && control_layer < control,
            "the control must be a separate Foreground placement:\n{}",
            g.source
        );
        // Exactly one frame shell (only the lowest layer hosts `show`), and one
        // captured origin per layer group.
        assert_eq!(
            g.source.matches(".show(ui, |ui| {").count(),
            1,
            "the frame shell rides only the lowest layer:\n{}",
            g.source
        );
        assert_eq!(
            g.source.matches("let __frame_origin_").count(),
            2,
            "one captured frame origin per layer group:\n{}",
            g.source
        );
    }

    /// A top-level control that is EARLIER in the file than a composite whose
    /// interior also holds a control: the composite's control must keep file
    /// order on the Foreground layer (draw on top of the earlier sibling),
    /// which the per-layer split guarantees by creating its Area at the
    /// Foreground statement position rather than at the composite's lowest
    /// layer. Pre-fix, the whole composite sorted at its lowest (Background)
    /// layer, creating the interior control's Area before the earlier sibling's
    /// — so the sibling wrongly painted over it.
    const CONTROL_BEFORE_COMPOSITE: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"text entry" {
	object {
		x=10
		y=200
		width=100
		height=20
	}
	control {
		chan="BTOP"
	}
}
composite {
	object {
		x=10
		y=10
		width=150
		height=150
	}
	"composite name"=""
	chan=""
	children {
		rectangle {
			object {
				x=10
				y=10
				width=150
				height=150
			}
			"basic attribute" {
				clr=1
				fill="outline"
			}
		}
		"text entry" {
			object {
				x=20
				y=20
				width=80
				height=18
			}
			control {
				chan="AINNER"
			}
		}
	}
}
"#;

    #[test]
    fn composite_control_keeps_file_order_against_an_earlier_sibling() {
        let g = generate(&parse(CONTROL_BEFORE_COMPOSITE), &Options::default());
        // Both controls are Foreground; the earlier top-level one (BTOP, screen
        // coords 10,200) must be emitted BEFORE the composite's interior control
        // (AINNER, frame-relative 10,10) so the latter's Area stacks on top.
        let btop = g
            .source
            .find("10.0, 200.0, 100.0, 20.0")
            .expect("top-level control place");
        let ainner = g
            .source
            .find("10.0, 10.0, 80.0, 18.0")
            .expect("composite control place");
        assert!(
            btop < ainner,
            "the earlier top-level control must precede the composite's interior \
             control on the Foreground layer (file order):\n{}",
            g.source
        );
        // The composite still draws its decoration behind, on a Background shell.
        assert!(
            g.source.contains("egui::Order::Background"),
            "the composite decoration must keep its Background shell:\n{}",
            g.source
        );
    }

    #[test]
    fn composite_destructures_self_for_disjoint_field_borrows() {
        let g = composite();
        assert!(
            g.source.contains("let Self { _engine: _,"),
            "ui() must destructure self so the frame closure can borrow siblings:\n{}",
            g.source
        );
    }

    /// The ADBuffers.adl group-title pattern: a composite holding only a
    /// decoration rectangle (the title chip) comes BEFORE an overlapping
    /// static text in the file. MEDM paints in strict file order with
    /// composites transparent (a group, not a stacking context), so the text
    /// lands on top of the chip.
    const DECO_COMPOSITE_UNDER_TEXT: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
composite {
	object {
		x=123
		y=2
		width=105
		height=21
	}
	"composite name"=""
	children {
		rectangle {
			object {
				x=123
				y=2
				width=105
				height=21
			}
			"basic attribute" {
				clr=1
			}
		}
	}
}
text {
	object {
		x=97
		y=3
		width=157
		height=20
	}
	"basic attribute" {
		clr=0
	}
	textix="Buffers"
	align="horiz. centered"
}
"#;

    #[test]
    fn decoration_only_composite_sorts_at_its_children_layer() {
        let g = generate(&parse(DECO_COMPOSITE_UNDER_TEXT), &Options::default());
        let ui_start = g.source.find("pub fn ui").expect("ui fn");
        let ui_src = &g.source[ui_start..];
        // The chip frame sorts at Background -- the layer of its only child --
        // so its Area is created at the composite's file position among the
        // sibling decorations, not hoisted after them by the container's own
        // Middle (same-Order Areas stack by creation order: hoisting put the
        // chip's Area after the title text's and blanked the title).
        let frame_geom = ui_src
            .find("123.0, 2.0, 105.0, 21.0")
            .expect("chip frame placement");
        let line_start = ui_src[..frame_geom].rfind('\n').unwrap_or(0);
        assert!(
            ui_src[line_start..frame_geom].contains("egui::Order::Background"),
            "a decoration-only composite must place at Background:\n{}",
            g.source
        );
        let text_geom = ui_src
            .find("97.0, 3.0, 157.0, 20.0")
            .expect("sibling title text placement");
        assert!(
            frame_geom < text_geom,
            "the chip composite must draw BEFORE the overlapping title text \
             (MEDM file order within the Background layer):\n{}",
            g.source
        );
    }

    // A composite nested inside another composite: outer (100,100), inner
    // (120,120) holding a text entry at (140,130), plus a text update at
    // (110,260) directly under the outer frame. Exercises the recursive
    // translate-and-drain path that single-level composites do not.
    const NESTED_COMPOSITE: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
composite {
	object {
		x=100
		y=100
		width=200
		height=200
	}
	chan=""
	children {
		composite {
			object {
				x=120
				y=120
				width=80
				height=40
			}
			chan=""
			children {
				"text entry" {
					object {
						x=140
						y=130
						width=40
						height=18
					}
					control {
						chan="SET"
					}
				}
			}
		}
		"text update" {
			object {
				x=110
				y=260
				width=80
				height=18
			}
			monitor {
				chan="RBV"
			}
		}
	}
}
"#;

    fn nested_composite() -> Generated {
        generate(&parse(NESTED_COMPOSITE), &Options::default())
    }

    #[test]
    fn nested_composite_emits_two_frames() {
        let g = nested_composite();
        let frames = g.source.matches(": RsdmFrame,").count();
        assert_eq!(frames, 2, "outer + inner frame fields:\n{}", g.source);
    }

    #[test]
    fn nested_composite_translates_coordinates_recursively() {
        let g = nested_composite();
        // inner composite abs (120,120), outer origin (100,100) -> rel (20,20).
        assert!(
            g.source.contains("20.0, 20.0, 80.0, 40.0"),
            "inner frame not translated relative to outer:\n{}",
            g.source
        );
        // text entry abs (140,130), inner origin (120,120) -> rel (20,10):
        // a second translation on top of the first, proving recursion.
        assert!(
            g.source.contains("20.0, 10.0, 40.0, 18.0"),
            "deepest child not translated relative to inner frame:\n{}",
            g.source
        );
        // text update abs (110,260), outer origin (100,100) -> rel (10,160).
        assert!(
            g.source.contains("10.0, 160.0, 80.0, 18.0"),
            "outer-frame child not translated relative to outer:\n{}",
            g.source
        );
    }

    #[test]
    fn nested_composite_places_inner_child_inside_both_frame_closures() {
        let g = nested_composite();
        // Two frame show-closures open before the deepest control's place():
        // the control is two levels deep, not a top-level sibling.
        let shows: Vec<usize> = g
            .source
            .match_indices(".show(ui, |ui| {")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(shows.len(), 2, "two frame closures expected:\n{}", g.source);
        // The control's own place(), found by its inner-frame-relative rect
        // (the inner FRAME also places at Foreground -- it sorts at its only
        // child's layer -- so the Order alone no longer identifies the control).
        let control_place = g
            .source
            .find("20.0, 10.0, 40.0, 18.0")
            .expect("control place");
        assert!(
            shows[1] < control_place,
            "deepest control must sit inside the inner frame closure:\n{}",
            g.source
        );
    }

    // A strip chart (two pens) over a cartesian plot whose first trace has both
    // X and Y arrays and whose second has only Y. Colour map: 2 = red, 3 =
    // green, 4 = blue.
    const PLOTS: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
		00ff00,
		0000ff,
	}
}
"strip chart" {
	object {
		x=33
		y=27
		width=309
		height=191
	}
	period=2
	units="minute"
	pen[0] {
		chan="DEV:H1"
		clr=2
	}
	pen[1] {
		chan="DEV:H2"
		clr=3
	}
}
"cartesian plot" {
	object {
		x=9
		y=230
		width=304
		height=159
	}
	count=500
	trace[0] {
		xdata="DEV:X"
		ydata="DEV:Y1"
		data_clr=2
	}
	trace[1] {
		ydata="DEV:Y2"
		data_clr=4
	}
}
"#;

    fn plots(opts: &Options) -> Generated {
        generate(&parse(PLOTS), opts)
    }

    #[test]
    fn strip_chart_becomes_a_time_plot_with_a_curve_per_pen() {
        let g = plots(&Options::default());
        assert!(g.source.contains(": RsdmTimePlot,"), "{}", g.source);
        // period 2 * units "minute" (60) -> 120 s time span.
        assert!(
            g.source
                .contains("RsdmTimePlot::new(rs, __plot_base).with_time_span(120.0)"),
            "strip-chart span not period*units:\n{}",
            g.source
        );
        // The instance allocates its PlotId block from the shared counter, so a
        // second screen instance (a related-display child, or the same screen
        // opened twice) never reuses these GPU resource keys.
        assert!(
            g.source.contains("let __plot_base = next_plot_ids(2);"),
            "missing per-instance plot-id base:\n{}",
            g.source
        );
        assert!(
            g.source.contains("fn next_plot_ids(count: u64) -> u64 {"),
            "missing shared plot-id allocator:\n{}",
            g.source
        );
        // One add_channel per pen, with the pen colour resolved from the table.
        assert!(g.source.contains(
            "add_channel(&engine, \"ca://DEV:H1\", Color32::from_rgb(255, 0, 0), \"DEV:H1\")"
        ));
        assert!(g.source.contains(
            "add_channel(&engine, \"ca://DEV:H2\", Color32::from_rgb(0, 255, 0), \"DEV:H2\")"
        ));
    }

    #[test]
    fn strip_chart_pen_limits_normalize_each_pen_onto_the_shared_axis() {
        // R3-18: a pen's `limits {}` block used to vanish at the parser (level-0
        // assignments only). The deep pass retains it, and now codegen APPLIES it:
        // when any pen carries an authored range, every pen is emitted through
        // `add_normalized_channel` so RsdmTimePlot maps it onto the shared [0,1]
        // axis (MEDM per-pen normalisation, medmStripChart.c:1878-1898). The real
        // MEDM source token for a default-sourced end is "default"
        // (stringValueTable[PV_LIMITS_DEFAULT], displayList.h:464).
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
		00ff00,
	}
}
"strip chart" {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	pen[0] {
		chan="DEV:T"
		clr=2
		limits {
			loprSrc="default"
			loprDefault=0
			hoprSrc="default"
			hoprDefault=300
		}
	}
	pen[1] {
		chan="DEV:P"
		clr=3
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // The authored [0,300] pen normalizes with its fixed bounds; the pen's own
        // chan/clr must still parse through the nested block.
        assert!(
            g.source.contains(
                "add_normalized_channel(&engine, \"ca://DEV:T\", \
                 Color32::from_rgb(255, 0, 0), \"DEV:T\", Some(0.0), Some(300.0))"
            ),
            "authored-range pen must normalize with its fixed bounds:\n{}",
            g.source
        );
        // The limitless pen normalizes too (channel-sourced ends → None), so both
        // pens share the [0,1] axis rather than one staying on a raw auto-scale.
        assert!(
            g.source.contains(
                "add_normalized_channel(&engine, \"ca://DEV:P\", \
                 Color32::from_rgb(0, 255, 0), \"DEV:P\", None, None)"
            ),
            "limitless pen must normalize with channel-sourced bounds:\n{}",
            g.source
        );
        // The residual fidelity gap (MEDM's per-range y-axis label columns) is
        // noted, not silently dropped — but the ranges themselves are now applied.
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("per-pen normalisation")
                    && w.contains("per-range y-axis label columns are not reproduced")),
            "expected the per-pen normalisation fidelity note:\n{:?}",
            g.warnings
        );
        assert!(
            !g.warnings.iter().any(|w| w.contains("not applied")),
            "the per-pen ranges must no longer be reported as unapplied:\n{:?}",
            g.warnings
        );
    }

    #[test]
    fn strip_chart_pen_with_one_authored_end_normalizes_per_bound() {
        // R2-66 boundary on a pen: only the lower end is default-sourced, so it
        // pins `Some(lo)` while the upper stays channel-sourced (`None`) — the
        // single authored end still triggers normalisation for the whole chart.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
		ff0000,
	}
}
"strip chart" {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	pen[0] {
		chan="DEV:T"
		clr=2
		limits {
			loprSrc="default"
			loprDefault=-5
		}
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains(
                "add_normalized_channel(&engine, \"ca://DEV:T\", \
                 Color32::from_rgb(255, 0, 0), \"DEV:T\", Some(-5.0), None)"
            ),
            "lower-only authored pen must pin Some(lo) with a channel-sourced hi:\n{}",
            g.source
        );
    }

    #[test]
    fn external_cmap_warns_and_no_colormap_uses_default_palette() {
        // R3-20: a display referencing an external colormap file the converter
        // could not read (non-blank cmap, no inline color map, no source dir) falls
        // to MEDM's default palette (medmDisplay.c "Using the default colormap") and
        // warns naming the file, so the colour difference from the real file is
        // visible rather than silent.
        let external = r#"
display {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	cmap="site.map"
	clr=14
	bclr=0
}
"text" {
	object {
		x=0
		y=0
		width=50
		height=20
	}
	"basic attribute" {
		clr=15
	}
	textix="hi"
}
"#;
        let g = generate(&parse(external), &Options::default());
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("external colormap file \"site.map\"")
                    && w.contains("default palette")),
            "external cmap must warn naming the file: {:?}",
            g.warnings
        );
        // The unresolved cmap still resolves clr against the default palette
        // (index 15 = (0,216,0)) rather than dropping every colour to a theme
        // default — the warning marks that these are MEDM defaults, not the file's.
        assert!(
            g.source.contains("Color32::from_rgb(0, 216, 0)"),
            "unresolved cmap must still resolve clr against the default palette:\n{}",
            g.source
        );

        // The same screen WITHOUT a cmap resolves against MEDM's default palette
        // (index 15 = (0,216,0)), so it renders that colour and does not warn.
        let defaulted = r#"
display {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	clr=14
	bclr=0
}
"text" {
	object {
		x=0
		y=0
		width=50
		height=20
	}
	"basic attribute" {
		clr=15
	}
	textix="hi"
}
"#;
        let g2 = generate(&parse(defaulted), &Options::default());
        assert!(
            g2.source.contains("Color32::from_rgb(0, 216, 0)"),
            "default-palette colour (index 15) not resolved:\n{}",
            g2.source
        );
        assert!(
            !g2.warnings
                .iter()
                .any(|w| w.contains("colormap") || w.contains("color map")),
            "a default-palette screen must not warn about colours: {:?}",
            g2.warnings
        );
    }

    #[test]
    fn strip_chart_without_pen_limits_stays_on_the_auto_scaled_axis() {
        // R3-18: the stock two-pen chart (no authored `limits`) must NOT normalize
        // — it keeps `add_channel` on the single auto-scaled axis (the common
        // same-range case), and emits no per-pen normalisation note.
        let g = plots(&Options::default());
        assert!(
            !g.warnings
                .iter()
                .any(|w| w.contains("per-pen normalisation")),
            "no-range strip chart must not emit the normalisation note:\n{:?}",
            g.warnings
        );
        assert!(
            !g.source.contains("add_normalized_channel"),
            "no-range strip chart must stay on add_channel:\n{}",
            g.source
        );
    }

    #[test]
    fn strip_chart_span_scales_units_defaults_and_legacy_delay() {
        // R2-62: span = period * unit-scale in seconds; units and period both
        // default to MEDM's stock values when absent; pre-2.1 `delay` converts.
        let span = |pairs: &[(&str, &str)]| {
            let assignments: BTreeMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            strip_chart_span(&MedmWidget {
                assignments,
                line: 1,
                ..MedmWidget::default()
            })
        };
        // milli-second is a real MEDM unit (×0.001) — a 500 ms window, not 500 s.
        assert_eq!(span(&[("period", "500"), ("units", "milli-second")]).0, 0.5);
        assert_eq!(span(&[("period", "500"), ("units", "milli second")]).0, 0.5);
        // minute ×60, second/absent ×1.
        assert_eq!(span(&[("period", "2"), ("units", "minute")]).0, 120.0);
        assert_eq!(span(&[("period", "30"), ("units", "second")]).0, 30.0);
        assert_eq!(span(&[("period", "30")]).0, 30.0);
        // Absent period: MEDM's stock 60-second window (not rsdm's 5 s default).
        assert_eq!(span(&[]).0, 60.0);
        assert_eq!(span(&[("units", "minute")]).0, 60.0);
        // Legacy `delay` (period absent): delay × unit factor, with a warning.
        let (delay_span, warn) = span(&[("delay", "1"), ("units", "second")]);
        assert_eq!(delay_span, 60.0);
        assert!(warn.is_some_and(|w| w.contains("delay=1")));
        assert_eq!(span(&[("delay", "2"), ("units", "minute")]).0, 7200.0);
        // period wins over a stray delay (MEDM consults delay only when absent).
        assert_eq!(span(&[("period", "10"), ("delay", "5")]).0, 10.0);
    }

    #[test]
    fn limits_precision_resolves_each_bound_per_its_own_source() {
        // R2-66: MEDM resolves lopr/hopr/prec from their own *Src keys; a bare
        // `precDefault` (no `precSrc="default"`) is a leftover MEDM ignores, an
        // absent `hoprDefault` is HOPR_DEFAULT 1.0 (not 0.0), and a single-sided
        // default maps to rsdm's per-bound `.with_lower_limit`/`.with_upper_limit`.
        let widget = |pairs: &[(&str, &str)]| MedmWidget {
            assignments: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            line: 1,
            ..MedmWidget::default()
        };

        // precision: pinned only when precSrc="default"; precDefault absent -> 0.
        assert_eq!(
            precision_default_builder(&widget(&[("precDefault", "3")])),
            None
        );
        assert_eq!(
            precision_default_builder(&widget(&[("precSrc", "default"), ("precDefault", "3")])),
            Some(".with_precision(3)".to_string())
        );
        assert_eq!(
            precision_default_builder(&widget(&[("precSrc", "default")])),
            Some(".with_precision(0)".to_string())
        );

        // limits: each end resolves from its own source (R2-66).
        // Both ends default -> fixed range; absent hoprDefault -> 1.0.
        assert_eq!(
            user_defined_limits(&widget(&[
                ("loprSrc", "default"),
                ("loprDefault", "-5"),
                ("hoprSrc", "default"),
                ("hoprDefault", "5"),
            ])),
            Some(".with_limits(-5.0, 5.0)".to_string())
        );
        assert_eq!(
            user_defined_limits(&widget(&[("loprSrc", "default"), ("hoprSrc", "default")])),
            // LOPR_DEFAULT 0.0, HOPR_DEFAULT 1.0
            Some(".with_limits(0.0, 1.0)".to_string())
        );

        // Neither default -> channel-driven, no builder.
        assert_eq!(user_defined_limits(&widget(&[])), None);

        // Single-sided default is now representable per-bound: the pinned end
        // emits, the other stays channel-driven (no warn, no fabricated end).
        assert_eq!(
            user_defined_limits(&widget(&[("hoprSrc", "default"), ("hoprDefault", "9")])),
            Some(".with_upper_limit(9.0)".to_string())
        );
        assert_eq!(
            user_defined_limits(&widget(&[("loprSrc", "default"), ("loprDefault", "-2")])),
            Some(".with_lower_limit(-2.0)".to_string())
        );
        // Lower-only with an absent loprDefault falls to LOPR_DEFAULT 0.0.
        assert_eq!(
            user_defined_limits(&widget(&[("loprSrc", "default")])),
            Some(".with_lower_limit(0.0)".to_string())
        );
    }

    #[test]
    fn cartesian_plot_defaults_to_a_waveform_plot() {
        let g = plots(&Options::default());
        assert!(g.source.contains(": RsdmWaveformPlot,"), "{}", g.source);
        // trace[0] has X and Y -> add_xy_channel(y, Some(x)); blue from data_clr=2
        // is red (255,0,0).
        assert!(
            g.source.contains(
                "add_xy_channel(&engine, \"ca://DEV:Y1\", Some(\"ca://DEV:X\"), Color32::from_rgb(255, 0, 0), \"curve 1\")"
            ),
            "x/y trace not add_xy_channel:\n{}",
            g.source
        );
        // trace[1] has only Y -> add_channel (plotted against index).
        assert!(
            g.source.contains(
                "add_channel(&engine, \"ca://DEV:Y2\", Color32::from_rgb(0, 0, 255), \"curve 2\")"
            ),
            "y-only trace not add_channel:\n{}",
            g.source
        );
        // The waveform plot has no per-curve buffer; `count` must not appear.
        assert!(
            !g.source.contains("with_buffer_size"),
            "count must not map to a waveform buffer:\n{}",
            g.source
        );
    }

    #[test]
    fn cartesian_plot_warns_on_unsupported_runtime_keys() {
        // R2-68: the runtime keys with no rsdm surface must warn, never silently
        // drop. `line plot` + numeric count are faithful and stay silent.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
	}
}
"cartesian plot" {
	object {
		x=0
		y=0
		width=300
		height=150
	}
	trigger="TRIG:PV"
	erase="ERASE:PV"
	eraseMode="if zero"
	countPvName="NPTS:REC"
	style="point plot"
	erase_oldest="plot last n pts"
	trace[0] {
		ydata="DEV:Y"
		data_clr=1
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        let has = |needle: &str| g.warnings.iter().any(|w| w.contains(needle));
        assert!(
            has("trigger PV \"TRIG:PV\""),
            "no trigger warning: {:?}",
            g.warnings
        );
        assert!(
            has("erase PV \"ERASE:PV\"") && has("if zero"),
            "no erase warning: {:?}",
            g.warnings
        );
        assert!(
            has("count PV \"NPTS:REC\""),
            "no count-PV warning: {:?}",
            g.warnings
        );
        assert!(
            has("style \"point plot\""),
            "no style warning: {:?}",
            g.warnings
        );
        assert!(
            has("erase_oldest circular"),
            "no erase_oldest warning: {:?}",
            g.warnings
        );

        // A `line plot` with a numeric count is faithful for the trigger/erase/
        // count/STYLE surface, so none of those warn. (`erase_oldest` is absent
        // here, which MEDM treats as its stop-at-n default — that case is covered
        // by the R3-19 test below, not asserted silent here.)
        let plain = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
	}
}
"cartesian plot" {
	object {
		x=0
		y=0
		width=300
		height=150
	}
	count=500
	style="line plot"
	erase_oldest="plot last n pts"
	trace[0] {
		ydata="DEV:Y"
		data_clr=1
	}
}
"#;
        let g2 = generate(&parse(plain), &Options::default());
        assert!(
            !g2.warnings.iter().any(|w| w.contains("style")),
            "a line plot must not warn about style: {:?}",
            g2.warnings
        );
        assert!(
            !g2.warnings
                .iter()
                .any(|w| w.contains("trigger") || w.contains("count PV")),
            "faithful trigger/count must stay silent: {:?}",
            g2.warnings
        );
    }

    #[test]
    fn cartesian_plot_trace_on_secondary_y_axis_warns() {
        // R3-21: a trace with yaxis=1 (Y2) cannot be honoured on rsdm's single
        // y-axis; it must warn, not silently plot against Y1. A yaxis=0 trace is
        // faithful and stays silent.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
		00ff00,
	}
}
"cartesian plot" {
	object {
		x=0
		y=0
		width=300
		height=150
	}
	style="line plot"
	erase_oldest="plot last n pts"
	trace[0] {
		ydata="DEV:CURRENT"
		data_clr=1
		yaxis=0
	}
	trace[1] {
		ydata="DEV:PRESSURE"
		data_clr=2
		yaxis=1
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.warnings.iter().any(|w| w.contains("trace 2")
                && w.contains("secondary y-axis")
                && w.contains("yaxis=1")),
            "Y2 trace must warn: {:?}",
            g.warnings
        );
        assert!(
            !g.warnings
                .iter()
                .any(|w| w.contains("trace 1") && w.contains("secondary y-axis")),
            "the Y1 trace must not warn about the axis: {:?}",
            g.warnings
        );
    }

    #[test]
    fn cartesian_plot_absent_style_and_erase_oldest_warn_as_medm_defaults() {
        // R3-19 (R2-68 residual): MEDM omits `style`/`erase_oldest` on write when
        // they equal their POINT_PLOT / ERASE_OLDEST_OFF defaults, so a plot with
        // NEITHER key is a point plot that stops at n — both divergent from rsdm's
        // connected-line, full-array rendering. The absent keys must warn exactly
        // like the written ones, not pass silently.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
	}
}
"cartesian plot" {
	object {
		x=0
		y=0
		width=300
		height=150
	}
	trace[0] {
		ydata="DEV:Y"
		data_clr=1
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("style \"point plot\"")),
            "absent style must warn as point plot: {:?}",
            g.warnings
        );
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("erase_oldest stop-at-n")),
            "absent erase_oldest must warn as stop-at-n: {:?}",
            g.warnings
        );
    }

    #[test]
    fn cartesian_plot_uses_scatter_with_use_scatterplot() {
        let opts = Options {
            use_scatterplot: true,
            ..Options::default()
        };
        let g = plots(&opts);
        assert!(g.source.contains(": RsdmScatterPlot,"), "{}", g.source);
        // count -> scatter buffer size.
        assert!(
            g.source
                .contains("RsdmScatterPlot::new(rs, __plot_base + 1).with_buffer_size(500)"),
            "count not mapped to scatter buffer:\n{}",
            g.source
        );
        // Scatter pairs X and Y in (x, y) order.
        assert!(
            g.source.contains(
                "add_xy_channel(&engine, \"ca://DEV:X\", \"ca://DEV:Y1\", Color32::from_rgb(255, 0, 0), \"curve 1\")"
            ),
            "scatter trace not (x, y):\n{}",
            g.source
        );
        // trace[1] lacks xdata, which scatter requires -> warned and skipped.
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("trace 2 needs both xdata and ydata")),
            "missing-xdata scatter trace not warned:\n{:?}",
            g.warnings
        );
        assert!(!g.source.contains("DEV:Y2"), "{}", g.source);
    }

    // R1-36: plotcom (title/xlabel/ylabel/clr/bclr) and the cartesian axis
    // blocks (rangeStyle/minRange/maxRange/axisStyle).
    const STYLED_PLOTS: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
"strip chart" {
	object {
		x=0
		y=0
		width=300
		height=100
	}
	plotcom {
		title="Trend"
		xlabel="Time"
		ylabel="Volts"
		clr=1
		bclr=0
	}
	period=60
	pen[0] {
		chan="DEV:H1"
		clr=1
	}
}
"cartesian plot" {
	object {
		x=0
		y=120
		width=300
		height=100
	}
	plotcom {
		title="Profile"
		xlabel="Position"
		ylabel="Counts"
		clr=0
		bclr=1
	}
	x_axis {
		rangeStyle="user-specified"
		minRange=2.000000
		maxRange=8.000000
	}
	y1_axis {
		axisStyle="log10"
		rangeStyle="user-specified"
		maxRange=50.000000
	}
	y2_axis {
		rangeStyle="user-specified"
		minRange=0.000000
		maxRange=1.000000
	}
	trace[0] {
		ydata="DEV:Y1"
		data_clr=1
	}
}
"#;

    #[test]
    fn strip_chart_plotcom_styles_title_labels_and_colours() {
        let g = generate(&parse(STYLED_PLOTS), &Options::default());
        assert!(
            g.source.contains(
                ".with_title(\"Trend\").with_x_label(\"Time\").with_y_label(\"Volts\")\
                 .with_axis_color(Color32::from_rgb(0, 0, 0))\
                 .with_background_color(Color32::from_rgb(255, 255, 255))"
            ),
            "strip-chart plotcom styling missing:\n{}",
            g.source
        );
    }

    #[test]
    fn cartesian_plot_axis_blocks_pin_user_ranges_and_warn_on_the_rest() {
        let g = generate(&parse(STYLED_PLOTS), &Options::default());
        // x_axis user-specified 2..8; y1_axis user-specified with minRange
        // absent -> MEDM's plotAxisDefinitionInit default 0.0.
        assert!(
            g.source.contains(
                ".with_title(\"Profile\").with_x_label(\"Position\").with_y_label(\"Counts\")\
                 .with_x_range(2.0, 8.0).with_y_range(0.0, 50.0)\
                 .with_axis_color(Color32::from_rgb(255, 255, 255))\
                 .with_background_color(Color32::from_rgb(0, 0, 0))"
            ),
            "cartesian plotcom/axis styling missing:\n{}",
            g.source
        );
        // y2 user range and log10 have no rsdm surface -> warned, not silent.
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("y2_axis user-specified range")),
            "{:?}",
            g.warnings
        );
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("y1_axis axisStyle=log10")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn plots_are_middle_layer_monitors_with_distinct_ids() {
        let g = plots(&Options::default());
        // Both plots are monitors -> Middle layer, never Background/Foreground.
        assert!(
            !g.source.contains("egui::Order::Background"),
            "{}",
            g.source
        );
        assert!(
            !g.source.contains("egui::Order::Foreground"),
            "{}",
            g.source
        );
        let middles = g.source.matches("egui::Order::Middle").count();
        assert_eq!(middles, 2, "two Middle-layer placements:\n{}", g.source);
        // Distinct PlotIds (offsets into the instance's block) keep their GPU
        // resources separate.
        assert!(g.source.contains("RsdmTimePlot::new(rs, __plot_base)"));
        assert!(
            g.source
                .contains("RsdmWaveformPlot::new(rs, __plot_base + 1)")
        );
    }

    // The formerly-deferred widgets, now all implemented for real: the static
    // shapes (arc/polyline → `DrawingShape::Arc`/`Polyline`), the static-file
    // image (`RsdmImage`), the embedded display (inlined into a `RsdmFrame`), and
    // the nav/shell controls (live `egui::Button`s). Each is asserted as its real
    // RsDM target below; degenerate inputs still fall back to a visible marker
    // rather than a silent drop.
    const DEFERRED: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
arc {
	object {
		x=10
		y=10
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
	}
	begin=2880
	path=5760
}
polyline {
	object {
		x=60
		y=10
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
		width=2
	}
	points {
		(60,10)
		(80,30)
		(100,10)
	}
}
image {
	object {
		x=10
		y=60
		width=100
		height=73
	}
	type="gif"
	"image name"="apple.gif"
}
"embedded display" {
	object {
		x=10
		y=140
		width=100
		height=50
	}
}
"related display" {
	object {
		x=10
		y=200
		width=100
		height=20
	}
	display[0] {
		label="Open Detail"
		name="detail.adl"
	}
}
"shell command" {
	object {
		x=10
		y=230
		width=100
		height=20
	}
	command[0] {
		label="Eyes"
		name="xeyes"
	}
	command[1] {
		label="Load"
		name="xload"
	}
}
"#;

    fn deferred() -> Generated {
        generate(&parse(DEFERRED), &Options::default())
    }

    /// An `arc` block with no `begin`/`path` keeps the values `createDlArc`
    /// seeded before parsing: 0° and 90° (`medm/medmArc.c:258-259`). A full
    /// 360° sweep is what MEDM draws only when the file says so.
    #[test]
    fn arc_without_angles_falls_back_to_medms_zero_and_ninety_degrees() {
        const NO_ANGLES: &str = r#"
arc {
	object {
		x=10
		y=10
		width=40
		height=40
	}
	"basic attribute" {
		clr=3
	}
}
"#;
        let g = generate(&parse(NO_ANGLES), &Options::default());
        assert!(
            g.source
                .contains("DrawingShape::Arc { begin_deg: 0.0, span_deg: 90.0 }"),
            "absent angles must not become a 360° sweep:\n{}",
            g.source
        );
    }

    #[test]
    fn arc_and_polyline_emit_real_drawings_at_the_background_layer() {
        let g = deferred();
        // arc -> RsdmDrawing(Arc) with the parsed begin/span degrees (2880/64=45,
        // 5760/64=90), no Qt-style negation, at the Background (decoration) layer.
        assert!(
            g.source
                .contains("DrawingShape::Arc { begin_deg: 45.0, span_deg: 90.0 }"),
            "arc not emitted with parsed angles:\n{}",
            g.source
        );
        // polyline -> RsdmDrawing(Polyline) with its vertices normalised to the
        // widget origin (60,10): (0,0),(20,20),(40,0).
        assert!(g.source.contains("DrawingShape::Polyline"), "{}", g.source);
        assert!(
            g.source.contains(
                ".with_points(vec![egui::Vec2::new(0.0, 0.0), \
                 egui::Vec2::new(20.0, 20.0), egui::Vec2::new(40.0, 0.0)])"
            ),
            "polyline points not normalised to the widget origin:\n{}",
            g.source
        );
        // Both are sized to their MEDM 40x40 geometry (the polyline's vertices
        // are placed relative to that rect), not the 40x40 default by accident.
        assert_eq!(
            g.source
                .matches(".with_size(egui::Vec2::new(40.0, 40.0))")
                .count(),
            2,
            "arc and polyline should each be sized from geometry:\n{}",
            g.source
        );
        // Both are decorations -> Background layer, and both are real fielded
        // widgets (no fieldless placeholder).
        assert!(g.source.contains("egui::Order::Background"), "{}", g.source);
        assert!(g.source.contains(": RsdmDrawing,"), "{}", g.source);
        // Neither warns any longer (they map cleanly now).
        assert!(
            !g.warnings
                .iter()
                .any(|w| w.contains("arc") || w.contains("polyline") && !w.contains("dash")),
            "unexpected shape warnings: {:?}",
            g.warnings
        );
    }

    #[test]
    fn polygon_with_points_fills_and_normalises_to_the_widget_origin() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
		00ff00,
	}
}
polygon {
	object {
		x=100
		y=50
		width=40
		height=30
	}
	"basic attribute" {
		clr=1
	}
	points {
		(100,50)
		(140,50)
		(120,80)
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(g.source.contains("DrawingShape::Polygon"), "{}", g.source);
        // clr=1 -> 00ff00 (green) fill; points normalised to (0,0),(40,0),(20,30).
        assert!(
            g.source
                .contains(".with_fill(Color32::from_rgb(0, 255, 0))"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains(
                ".with_points(vec![egui::Vec2::new(0.0, 0.0), \
                 egui::Vec2::new(40.0, 0.0), egui::Vec2::new(20.0, 30.0)])"
            ),
            "{}",
            g.source
        );
        // Sized to the MEDM 40x30 geometry, so the placed vertices land correctly.
        assert!(
            g.source.contains(".with_size(egui::Vec2::new(40.0, 30.0))"),
            "{}",
            g.source
        );
    }

    #[test]
    fn image_emits_a_channel_less_rsdm_image_sized_to_the_geometry() {
        let g = deferred();
        // The MEDM static file image becomes a channel-less RsdmImage naming the
        // file, sized to the MEDM geometry (100×73) — never a RsdmImageView
        // (which would need an array channel a file image has none of).
        assert!(
            g.source.contains("RsdmImage::new(\"apple.gif\")"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains(".with_size(egui::Vec2::new(100.0, 73.0))"),
            "{}",
            g.source
        );
        assert!(!g.source.contains("RsdmImageView"), "{}", g.source);
        // It converts cleanly now — no image warning.
        assert!(
            !g.warnings.iter().any(|w| w.contains("apple.gif")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn embedded_display_without_a_file_emits_a_no_file_marker() {
        // The DEFERRED embedded display is a literal block with no `composite file`,
        // so there is nothing to inline — a visible marker, not a silent drop.
        let g = deferred();
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("no \"composite file\"")),
            "{:?}",
            g.warnings
        );
        assert!(
            g.source.contains("[embedded display (no file)]"),
            "{}",
            g.source
        );
    }

    #[test]
    fn embedded_display_without_source_dir_emits_a_placeholder() {
        // A childless composite carrying a `composite file` IS an embedded display
        // (adl2pydm's rewrite), but default options have no source directory, so
        // the file can't be resolved — a placeholder naming it.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
composite {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"composite file"="other.adl"
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(g.source.contains("[embedded: other.adl]"), "{}", g.source);
        assert!(
            g.warnings.iter().any(|w| w.contains("no source directory")),
            "{:?}",
            g.warnings
        );
    }

    /// A fresh temp directory for the filesystem-backed embedded-display tests.
    /// nextest runs each test in its own process, so `process::id()` keys it
    /// uniquely; `tag` separates dirs within a process.
    fn embed_tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("adl2rsdm_embed_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn embedded_display_inlines_the_target_with_merged_macros() {
        let dir = embed_tmpdir("inline");
        std::fs::write(
            dir.join("child.adl"),
            r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
display {
	object {
		x=0
		y=0
		width=120
		height=24
	}
	clr=1
	bclr=0
}
"text update" {
	object {
		x=4
		y=2
		width=110
		height=18
	}
	monitor {
		chan="loc://$(EMB)?type=int"
		clr=1
	}
}
"#,
        )
        .unwrap();
        let parent = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
composite {
	object {
		x=30
		y=40
		width=120
		height=24
	}
	"composite file"="child.adl;EMB=count"
}
"#;
        let options = Options {
            protocol: String::new(),
            source_dir: Some(dir.clone()),
            ..Options::default()
        };
        let g = generate(&parse(parent), &options);
        // The childless-composite-with-composite-file is recognised as an embedded
        // display and inlined into a RsdmFrame at the embedded geometry.
        assert!(g.source.contains("RsdmFrame::new"), "{}", g.source);
        // The child's text-update became a RsdmLabel; the embedded macro EMB=count
        // substituted into its channel.
        assert!(
            g.source.contains("loc://count?type=int"),
            "embedded macro not applied:\n{}",
            g.source
        );
        assert!(
            g.warnings.iter().any(|w| w.contains("inlined child.adl")),
            "{:?}",
            g.warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_macro_string_replaces_parent_table() {
        // MEDM compositeFileParse (medmComposite.c:659-668): a NON-EMPTY macro
        // string replaces the table, so the parent's macros are NOT visible in
        // the child. `$(P)` stays literal; only the embedded `M=2` applies.
        let dir = embed_tmpdir("replace");
        std::fs::write(
            dir.join("child.adl"),
            r#"
display {
	object {
		x=0
		y=0
		width=120
		height=24
	}
	clr=1
	bclr=0
}
"text update" {
	object {
		x=4
		y=2
		width=110
		height=18
	}
	monitor {
		chan="loc://$(P)_$(M)?type=int"
		clr=1
	}
}
"#,
        )
        .unwrap();
        let parent = r#"
composite {
	object {
		x=30
		y=40
		width=120
		height=24
	}
	"composite file"="child.adl;M=2"
}
"#;
        let options = Options {
            source_dir: Some(dir.clone()),
            macros: vec![("P".to_string(), "ioc1:".to_string())],
            ..Options::default()
        };
        let g = generate(&parse(parent), &options);
        // Embedded M=2 applied at convert time; parent P dropped from the table
        // so `$(P)` survives — and, because the composite replaced the table, it
        // is emitted as a PLAIN LITERAL, not `__m.expand(...)`. If it were
        // deferred to the runtime `__m` (which carries the top-level P=ioc1:),
        // the source would still read `$(P)_2` here but resolve to `ioc1:_2` at
        // runtime — exactly the leak this finding is about.
        assert!(
            g.source
                .contains(r#"RsdmLabel::new(&engine, "ca://loc://$(P)_2?type=int")"#),
            "replaced-table child channel must be a literal `$(P)`, not an \
             `__m.expand` against the parent's runtime table:\n{}",
            g.source
        );
        assert!(
            !g.source.contains(r#"__m.expand("ca://loc://$(P)"#),
            "the child's surviving `$(P)` must not defer to the runtime table:\n{}",
            g.source
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_empty_macro_string_inherits_parent_table() {
        // Empty macro string ("use the existing macros"): the parent's macros
        // ARE inherited, so `$(P)` expands.
        let dir = embed_tmpdir("inherit");
        std::fs::write(
            dir.join("child.adl"),
            r#"
display {
	object {
		x=0
		y=0
		width=120
		height=24
	}
	clr=1
	bclr=0
}
"text update" {
	object {
		x=4
		y=2
		width=110
		height=18
	}
	monitor {
		chan="loc://$(P)dev?type=int"
		clr=1
	}
}
"#,
        )
        .unwrap();
        let parent = r#"
composite {
	object {
		x=30
		y=40
		width=120
		height=24
	}
	"composite file"="child.adl"
}
"#;
        let options = Options {
            source_dir: Some(dir.clone()),
            macros: vec![("P".to_string(), "ioc1:".to_string())],
            ..Options::default()
        };
        let g = generate(&parse(parent), &options);
        assert!(
            g.source.contains("loc://ioc1:dev?type=int"),
            "empty macro string must inherit the parent's macros:\n{}",
            g.source
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_display_refits_frame_to_content_bbox_and_translates_children() {
        // MEDM compositeFileParse (medmComposite.c:709-736): the composite refits
        // to its children's bounding box. child.adl has two widgets whose bbox min
        // is (15, 30) — NOT the file origin — and whose bbox is 45x38. The parent
        // composite is written at (100, 200) with a deliberately WRONG 999x888
        // size. After refit the frame must sit at (100, 200) sized 45x38, and each
        // child must be measured from the bbox min (translated by -15, -30).
        let dir = embed_tmpdir("refit");
        std::fs::write(
            dir.join("child.adl"),
            r#"
display {
	object {
		x=0
		y=0
		width=300
		height=300
	}
	clr=1
	bclr=0
}
"text update" {
	object {
		x=20
		y=30
		width=40
		height=10
	}
	monitor {
		chan="loc://a?type=int"
		clr=1
	}
}
"text update" {
	object {
		x=15
		y=60
		width=25
		height=8
	}
	monitor {
		chan="loc://b?type=int"
		clr=1
	}
}
"#,
        )
        .unwrap();
        let parent = r#"
composite {
	object {
		x=100
		y=200
		width=999
		height=888
	}
	"composite file"="child.adl"
}
"#;
        let options = Options {
            source_dir: Some(dir.clone()),
            ..Options::default()
        };
        let g = generate(&parse(parent), &options);
        // Frame at written (100,200) but sized to the content bbox (45x38), not
        // the stale composite 999x888.
        assert!(
            g.source.contains("100.0, 200.0, 45.0, 38.0"),
            "frame must refit to the content bbox at the written position:\n{}",
            g.source
        );
        assert!(
            !g.source.contains("999.0, 888.0"),
            "stale composite geometry must be dropped:\n{}",
            g.source
        );
        // Child A (20,30,40,10) measured from bbox min (15,30) → (5.0, 0.0).
        assert!(
            g.source.contains("5.0, 0.0, 40.0, 10.0"),
            "child A must be translated by -bbox-min:\n{}",
            g.source
        );
        // Child B (15,60,25,8) → (0.0, 30.0).
        assert!(
            g.source.contains("0.0, 30.0, 25.0, 8.0"),
            "child B must be translated by -bbox-min:\n{}",
            g.source
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_display_breaks_include_cycles_with_a_placeholder() {
        let dir = embed_tmpdir("cycle");
        std::fs::write(
            dir.join("cyclic.adl"),
            r#"
"color map" {
	colors {
		ffffff,
	}
}
composite {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"composite file"="cyclic.adl"
}
"#,
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.join("cyclic.adl")).unwrap();
        let options = Options {
            protocol: String::new(),
            source_dir: Some(dir.clone()),
            ..Options::default()
        };
        let g = generate(&parse(&text), &options);
        // The outer level inlines once; the self-reference inside is caught and
        // rendered as a placeholder instead of recursing forever.
        assert!(
            g.warnings.iter().any(|w| w.contains("include cycle")),
            "{:?}",
            g.warnings
        );
        assert!(g.source.contains("[embedded: cyclic.adl]"), "{}", g.source);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The synthetic `loc://adl2rsdm_<kind>_<n>` placeholder address that each
    /// widget *constructor* connects to — one entry per channel-less widget
    /// (`RsdmDrawing::new(&engine, "loc://…")`, `RsdmFrame::new(&engine, …)`).
    /// Anchoring on the `(&engine, "` constructor argument skips the same address
    /// re-appearing in connect-description strings, so two equal entries mean two
    /// widgets genuinely share one channel — the E4 collision.
    fn synthetic_ctor_addrs(source: &str) -> Vec<String> {
        const ANCHOR: &str = "(&engine, \"loc://adl2rsdm_";
        let mut out = Vec::new();
        let mut rest = source;
        while let Some(start) = rest.find(ANCHOR) {
            let tail = &rest[start + "(&engine, \"".len()..];
            let end = tail.find('"').unwrap_or(tail.len());
            out.push(tail[..end].to_string());
            rest = &tail[end..];
        }
        out
    }

    #[test]
    fn synthetic_addresses_stay_unique_across_inlined_files() {
        // E4: synthetic placeholder channels were once keyed off `widget.line`, so
        // a channel-less shape at the same source line in two inlined `.adl`s
        // collided onto one `loc://` address — two widgets sharing one Engine
        // channel. Embedding the SAME child file twice reproduces it: both copies'
        // channel-less rectangle sits at the identical line. A monotonic per-screen
        // counter must hand each occurrence a distinct address.
        let dir = embed_tmpdir("unique");
        std::fs::write(
            dir.join("child.adl"),
            r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
display {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	clr=1
	bclr=0
}
rectangle {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"basic attribute" {
		clr=1
		fill="solid"
	}
}
"#,
        )
        .unwrap();
        // Two composites in the parent, each embedding the identical child file.
        let parent = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
composite {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	"composite file"="child.adl"
}
composite {
	object {
		x=0
		y=40
		width=80
		height=20
	}
	"composite file"="child.adl"
}
"#;
        let options = Options {
            protocol: String::new(),
            source_dir: Some(dir.clone()),
            ..Options::default()
        };
        let g = generate(&parse(parent), &options);
        let addrs = synthetic_ctor_addrs(&g.source);
        // Two embed frames + two channel-less rectangle children = four constructor
        // sites needing a synthetic channel.
        assert_eq!(
            addrs.len(),
            4,
            "expected 4 synthetic constructor sites (2 embeds + 2 shapes):\n{addrs:?}\n{}",
            g.source
        );
        // Every one must be distinct (the pre-fix code emitted two identical
        // `..._shape_<line>` for the two rectangles, fusing their channels).
        let mut deduped = addrs.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            addrs.len(),
            "synthetic channel addresses must be unique; got duplicates in {addrs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_command_emits_a_live_menu_spawning_each_command() {
        let g = deferred();
        // Two commands and no widget label -> MEDM renders a caption-less button
        // carrying the exclamation-mark icon (medmShellCommand.c, empty label
        // case), so the menu button is empty and the icon paints over its rect.
        // One item per command; each spawns `sh -c "<name>"` and closes the menu
        // — a live control, not a disabled placeholder.
        assert!(
            g.source.contains("let __m = ui.menu_button(\"\", |ui| {"),
            "label-less shell command not emitted as an icon menu:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("shell_command_icon(ui, __m.response.rect,"),
            "shell-command icon not painted over the menu button:\n{}",
            g.source
        );
        assert!(
            g.source.contains("fn shell_command_icon("),
            "icon helper not emitted:\n{}",
            g.source
        );
        assert!(
            g.source.contains("if ui.button(\"Eyes\").clicked() {"),
            "{}",
            g.source
        );
        for prog in ["xeyes", "xload"] {
            assert!(
                g.source.contains(&format!(
                    "let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg({prog:?}).spawn();"
                )),
                "missing spawn for {prog}:\n{}",
                g.source
            );
        }
        assert!(g.source.contains("ui.close();"), "{}", g.source);
        // Layered Foreground so a decoration can never occlude it.
        let menu = g.source.find("menu_button").expect("menu placement");
        assert!(
            g.source[..menu].rfind("egui::Order::Foreground").is_some(),
            "shell command must be a Foreground placement:\n{}",
            g.source
        );
        assert!(
            g.warnings.iter().any(|w| w.contains("spawns via `sh -c`")),
            "{:?}",
            g.warnings
        );
        // Channel-less: no Engine widget fabricated for it.
        assert!(!g.source.contains("RsdmPushButton"), "{}", g.source);
    }

    #[test]
    fn single_shell_command_emits_a_plain_button() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"shell command" {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	label="Run"
	command[0] {
		name="make"
		args="-j8 all"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // One command -> a plain button captioned by the widget label, spawning
        // the joined `"<name> <args>"` string; no menu. A labelled widget gets
        // no MEDM icon, so the helper must not be emitted either.
        assert!(
            g.source.contains("if ui.button(\"Run\").clicked() {"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains(
                "let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(\"make -j8 all\").spawn();"
            ),
            "{}",
            g.source
        );
        assert!(!g.source.contains("menu_button"), "{}", g.source);
        assert!(!g.source.contains("shell_command_icon"), "{}", g.source);
    }

    #[test]
    fn dash_prefixed_label_strips_the_dash_and_suppresses_the_icon() {
        // MEDM label rule (medmShellCommand.c / medmRelatedDisplay.c): a label
        // starting with '-' means "caption without icon" — the text after the
        // '-' is the caption. Same rule for both widget kinds.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"shell command" {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	label="-Hide"
	command[0] {
		name="make"
	}
}
"related display" {
	object {
		x=0
		y=30
		width=80
		height=20
	}
	label="-Go"
	display[0] {
		label="Detail"
		name="detail.adl"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains("if ui.button(\"Hide\").clicked() {"),
            "dash-prefixed shell-command label not stripped:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("if ui.button(\"Go\").on_hover_text(\"related display: open detail.adl\").clicked() {"),
            "dash-prefixed related-display label not stripped:\n{}",
            g.source
        );
        assert!(!g.source.contains("shell_command_icon"), "{}", g.source);
        assert!(!g.source.contains("related_display_icon"), "{}", g.source);
    }

    #[test]
    fn related_display_emits_a_live_navigation_reporting_button() {
        let g = deferred();
        // No widget label -> MEDM renders a caption-less button carrying the
        // overlapping-frames icon (medmRelatedDisplay.c, empty label case): the
        // button is empty, the icon paints over its rect, the target tooltip is
        // kept, and a click logs the target (RsDM has no runtime loader to
        // actually swap screens) — all at the control (Foreground) layer.
        assert!(
            g.source.contains(
                "let __r = ui.button(\"\").on_hover_text(\"related display: open detail.adl\");"
            ),
            "label-less related display not emitted as an icon button:\n{}",
            g.source
        );
        assert!(
            g.source.contains("related_display_icon(ui, __r.rect,"),
            "related-display icon not painted over the button:\n{}",
            g.source
        );
        assert!(
            g.source.contains("fn related_display_icon("),
            "icon helper not emitted:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("eprintln!(\"related display: open detail.adl\");"),
            "related-display click does not log the target:\n{}",
            g.source
        );
        // No disabled placeholder remains.
        assert!(!g.source.contains("add_enabled(false"), "{}", g.source);
        let rel = g
            .source
            .find("related_display_icon(ui,")
            .expect("related display button");
        assert!(
            g.source[..rel].rfind("egui::Order::Foreground").is_some(),
            "deferred control must be a Foreground placement:\n{}",
            g.source
        );
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("no runtime display loader")),
            "{:?}",
            g.warnings
        );
        // Channel-less: no Engine widget fabricated.
        assert!(!g.source.contains("RsdmPushButton"), "{}", g.source);
    }

    #[test]
    fn related_display_button_takes_its_medm_clr_bclr() {
        // MEDM draws the related-display button in its widget `clr`/`bclr` (the
        // classic grey-on-cyan); the emitted button must set both on the scoped
        // style — the face via weak_bg_fill (a raw bg rect would be painted OVER
        // by the egui button face) and the caption via override_text_color.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		000000,
		73dfff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=100
		height=20
	}
	clr=1
	bclr=2
	display[0] {
		label="Detail"
		name="detail.adl"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains("__v.widgets.inactive.weak_bg_fill = Color32::from_rgb(115, 223, 255);"),
            "related-display bclr must reach the button face:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("__v.override_text_color = Some(Color32::from_rgb(0, 0, 0));"),
            "related-display clr must tint the caption:\n{}",
            g.source
        );
    }

    #[test]
    fn multi_target_related_display_emits_a_menu_logging_each_target() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	label="Screens"
	display[0] {
		label="A"
		name="a.adl"
	}
	display[1] {
		label="B"
		name="b.adl"
		args="P=X:"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // Two targets, a widget label -> a menu titled by the label, one item per
        // target, each logging the target file (and macros where present). A
        // labelled widget gets no MEDM icon.
        assert!(
            g.source.contains("ui.menu_button(\"Screens\", |ui| {"),
            "{}",
            g.source
        );
        assert!(!g.source.contains("related_display_icon"), "{}", g.source);
        assert!(
            g.source.contains(
                "if ui.button(\"A\").on_hover_text(\"related display: open a.adl\").clicked() {"
            ),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains("eprintln!(\"related display: open a.adl\");"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains("eprintln!(\"related display: open b.adl (macros: P=X:)\");"),
            "{}",
            g.source
        );
        assert!(g.source.contains("ui.close();"), "{}", g.source);
    }

    #[test]
    fn related_display_with_named_targets_but_no_labels_is_one_button() {
        // R3-23: MEDM gates single-button vs menu on iNumberOfDisplays — the count
        // of non-empty *labels*, not names (medmRelatedDisplay.c:235-243). Three
        // named targets with no per-entry labels give iNumberOfDisplays == 0, so
        // MEDM renders a single plain button opening the first target (b.adl and
        // c.adl are unreachable), not a menu exposing all three.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	label="Screens"
	display[0] {
		name="a.adl"
	}
	display[1] {
		name="b.adl"
	}
	display[2] {
		name="c.adl"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // No menu — a single plain button captioned by the widget label.
        assert!(
            !g.source.contains("menu_button"),
            "zero labels must not render a menu:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("if ui.button(\"Screens\").on_hover_text(\"related display: open a.adl\").clicked() {"),
            "expected a single button opening the first target:\n{}",
            g.source
        );
        // The first target opens; the later named targets are unreachable, exactly
        // as MEDM's Case 1 leaves them.
        assert!(
            g.source
                .contains("eprintln!(\"related display: open a.adl\");"),
            "{}",
            g.source
        );
        assert!(
            !g.source.contains("b.adl") && !g.source.contains("c.adl"),
            "later targets must be unreachable in Case 1:\n{}",
            g.source
        );
    }

    #[test]
    fn related_display_one_label_among_many_names_is_still_one_button() {
        // R3-23 boundary: iNumberOfDisplays == 1 also fires Case 1 (`<= 1`), even
        // with two named targets — a single button opening the first name.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	label="Screens"
	display[0] {
		label="A"
		name="a.adl"
	}
	display[1] {
		name="b.adl"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            !g.source.contains("menu_button"),
            "one label (count == 1) must not render a menu:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("eprintln!(\"related display: open a.adl\");"),
            "the first target must open:\n{}",
            g.source
        );
        assert!(
            !g.source.contains("b.adl"),
            "the second target must be unreachable:\n{}",
            g.source
        );
    }

    /// A 2-target related display with `visual="a row of buttons"`, used by the
    /// row/column/invisible tests below.
    fn rd_two_targets(visual: &str) -> String {
        format!(
            r#"
"color map" {{
	colors {{
		ffffff,
	}}
}}
"related display" {{
	object {{
		x=0
		y=0
		width=120
		height=20
	}}
	label="Screens"
	visual="{visual}"
	display[0] {{
		label="A"
		name="a.adl"
	}}
	display[1] {{
		label="B"
		name="b.adl"
	}}
}}
"#
        )
    }

    #[test]
    fn related_display_row_of_buttons_emits_n_filled_cells_not_a_menu() {
        // R2-64: `visual="a row of buttons"` -> N side-by-side buttons (MEDM
        // RD_ROW_OF_BTN, medmRelatedDisplay.c:461-561), each opening its target.
        let g = generate(
            &parse(&rd_two_targets("a row of buttons")),
            &Options::default(),
        );
        assert!(!g.source.contains("menu_button"), "{}", g.source);
        // Two per-target buttons placed in equal cells split along the width.
        assert!(
            g.source
                .contains("ui.put(__cell, egui::Button::new(\"A\"))"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains("ui.put(__cell, egui::Button::new(\"B\"))"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains("__rect.width() / __n"),
            "row cells split the width:\n{}",
            g.source
        );
        assert!(
            g.source.contains("related display: open a.adl")
                && g.source.contains("related display: open b.adl"),
            "{}",
            g.source
        );
    }

    #[test]
    fn related_display_column_of_buttons_splits_the_height() {
        // RD_COL_OF_BTN -> vertical stack, each cell height = height/N.
        let g = generate(
            &parse(&rd_two_targets("a column of buttons")),
            &Options::default(),
        );
        assert!(!g.source.contains("menu_button"), "{}", g.source);
        assert!(
            g.source.contains("__rect.height() / __n"),
            "column cells split the height:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("ui.put(__cell, egui::Button::new(\"A\"))"),
            "{}",
            g.source
        );
    }

    #[test]
    fn related_display_invisible_is_a_transparent_hotspot_opening_the_first() {
        // RD_HIDDEN_BTN: no widget/fill, a click opens display[0] (a.adl) —
        // eventHandlers.c:228-251 opens the first target, never a menu.
        let g = generate(&parse(&rd_two_targets("invisible")), &Options::default());
        assert!(
            g.source
                .contains("ui.allocate_rect(__rect, egui::Sense::click())"),
            "invisible RD is a bare clickable rect:\n{}",
            g.source
        );
        assert!(!g.source.contains("menu_button"), "{}", g.source);
        assert!(!g.source.contains("egui::Button::new"), "{}", g.source);
        assert!(
            g.source.contains("related display: open a.adl"),
            "opens the first target:\n{}",
            g.source
        );
        // The second target is unreachable via the hidden hotspot (MEDM opens
        // only display[0]).
        assert!(
            !g.source.contains("related display: open b.adl"),
            "hidden button wires only the first target:\n{}",
            g.source
        );
    }

    #[test]
    fn related_display_single_target_row_is_still_one_button() {
        // MEDM "case 1 of 4": a single target is one button regardless of visual
        // (only >=2 targets use the row/column layout), medmRelatedDisplay.c:243.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	label="One"
	visual="a row of buttons"
	display[0] {
		label="A"
		name="a.adl"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(!g.source.contains("ui.put(__cell"), "{}", g.source);
        assert!(!g.source.contains("menu_button"), "{}", g.source);
        assert!(
            g.source.contains("ui.button(\"One\")"),
            "single-target row is one captioned button:\n{}",
            g.source
        );
    }

    #[test]
    fn related_display_unrecognized_visual_warns_and_uses_menu() {
        let g = generate(&parse(&rd_two_targets("bogus")), &Options::default());
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("visual") && w.contains("bogus")),
            "unrecognized visual warns: {:?}",
            g.warnings
        );
        assert!(g.source.contains("menu_button"), "{}", g.source);
    }

    #[test]
    fn related_display_replace_flag_reads_the_policy_key_not_mode() {
        // R2-64: MEDM's per-entry replace flag is `policy="replace display"`
        // (medmRelatedDisplay.c:666-671). The file format has no `mode` key, so a
        // spec keyed on `mode` must NOT be read as replace.
        let entry = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let widget = |spec: BTreeMap<String, String>| MedmWidget {
            records: [("displays".to_string(), vec![spec])].into_iter().collect(),
            line: 1,
            ..MedmWidget::default()
        };

        let mut b = Builder::default();
        let replaced = related_display_entries(
            &mut b,
            &widget(entry(&[("name", "c.adl"), ("policy", "replace display")])),
        );
        assert_eq!(replaced.len(), 1);
        assert!(
            replaced[0].replace,
            "policy=\"replace display\" must set replace"
        );

        // The old (wrong) `mode` key is not the file format's key -> not replace.
        let via_mode = related_display_entries(
            &mut b,
            &widget(entry(&[("name", "c.adl"), ("mode", "replace display")])),
        );
        assert!(
            !via_mode[0].replace,
            "a `mode` key must not be read as replace"
        );

        // The non-replace policy value stays non-replace.
        let created = related_display_entries(
            &mut b,
            &widget(entry(&[
                ("name", "c.adl"),
                ("policy", "create new display"),
            ])),
        );
        assert!(!created[0].replace);
    }

    #[test]
    fn related_display_target_substitutes_parent_macros() {
        // The logged target name and macro args resolve the parent `-m` macros at
        // convert time (consistent with channel-address resolution; rsdm has no
        // runtime macro engine), so the message shows values, not `$(P)`/`$(R)`.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=20
	}
	display[0] {
		label="Detail"
		name="$(P)detail.adl"
		args="R=$(R)"
	}
}
"#;
        let options = Options {
            macros: vec![
                ("P".to_string(), "13SIM1:".to_string()),
                ("R".to_string(), "cam1:".to_string()),
            ],
            ..Options::default()
        };
        let g = generate(&parse(adl), &options);
        assert!(
            g.source.contains(
                "eprintln!(\"related display: open 13SIM1:detail.adl (macros: R=cam1:)\");"
            ),
            "related-display target macros not substituted:\n{}",
            g.source
        );
    }

    /// One related display whose single target the recursive driver resolved.
    const RESOLVED_RD: &str = r#"
"color map" {
	colors {
		ffffff,
	}
}
"related display" {
	object {
		x=0
		y=0
		width=120
		height=24
	}
	label="Open"
	display[0] {
		label="Child"
		name="child.adl"
		args="P=$(P)"
	}
}
"#;

    #[test]
    fn resolved_related_display_opens_the_converted_module() {
        let options = Options {
            rd_modules: BTreeMap::from([(
                "child.adl".to_string(),
                RdModule {
                    ident: Some("__rd_child".to_string()),
                    title: "child.adl".to_string(),
                    width: 220.0,
                    height: 90.0,
                },
            )]),
            ..Options::default()
        };
        let g = generate(&parse(RESOLVED_RD), &options);
        // R3-24: the entry's args carry an unbound $(P): expanded at click time
        // against the parent instance's table via `expand_args` (MEDM
        // `performMacroSubstitutions` — an undefined macro is *dropped*, not left
        // literal like the child-string `expand` path). Both the dedup key and the
        // child's macro table come from that string.
        assert!(
            g.source
                .contains("let __rd_args = __m.expand_args(\"P=$(P)\");"),
            "args must use the drop-undefined expand_args, not expand:\n{}",
            g.source
        );
        assert!(
            g.source.contains(
                "OpenDisplay::open_or_focus(__open, &__rd_ctx, (\"__rd_child\", __rd_args.clone()), \"child.adl\", egui::vec2(220.0, 90.0)"
            ),
            "{}",
            g.source
        );
        assert!(
            g.source.contains(
                "Box::new(__rd_child::Screen::new_in(&__rd_ctx, __rs.as_ref(), parse_macro_args(&__rd_args)))"
            ),
            "{}",
            g.source
        );
        // The shared runtime and the open-display state are emitted at the top
        // level; the click never falls back to a stderr report.
        assert!(g.source.contains("pub trait RsdmDisplay"), "{}", g.source);
        assert!(g.source.contains("pub fn parse_macro_args"), "{}", g.source);
        assert!(
            g.source
                .contains("__rs: Option<rsplot::egui_wgpu::RenderState>,"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains("OpenDisplay::show_all(__open, ui);"),
            "{}",
            g.source
        );
        assert!(!g.source.contains("eprintln!"), "{}", g.source);
        assert!(
            g.related_targets == vec!["child.adl".to_string()],
            "{:?}",
            g.related_targets
        );
        // The args carry a macro, so the click-time hover ("open ... (macros:
        // P=$(P))") does too — the child-string `expand` is emitted alongside
        // `expand_args`, and both live in the one MacroTable.
        assert!(g.source.contains("fn expand_args("), "{}", g.source);
        assert!(g.source.contains("fn expand("), "{}", g.source);
    }

    #[test]
    fn macro_table_omits_expand_args_when_no_related_display_args_use_macros() {
        // R3-24 dead-code avoidance: `expand` (child strings, getToken) and
        // `expand_args` (related-display args, performMacroSubstitutions) are
        // emitted independently. A screen expanding a runtime macro only in its own
        // strings — here a static-text label carrying an unbaked $(P) — carries
        // `expand` but must NOT carry the unused `expand_args`.
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
text {
	object {
		x=0
		y=0
		width=80
		height=20
	}
	textix="$(P)label"
}
"#;
        // Generate with no `--macro` baking, so $(P) survives to runtime.
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source
                .contains(r#"egui::RichText::new(__m.expand("$(P)label").as_str())"#),
            "the label must expand its runtime macro via expand:\n{}",
            g.source
        );
        assert!(
            g.source.contains("fn expand("),
            "child-string path needs expand:\n{}",
            g.source
        );
        assert!(
            !g.source.contains("fn expand_args("),
            "expand_args must not be emitted (dead) when no args use macros:\n{}",
            g.source
        );
    }

    #[test]
    fn child_module_references_the_shared_runtime_through_super() {
        // The same screen emitted as a child `pub mod`, whose target is the
        // ROOT screen (a cycle): every shared item goes through `super::` and
        // the root's `Screen` is named directly.
        let options = Options {
            child_module: true,
            rd_modules: BTreeMap::from([(
                "child.adl".to_string(),
                RdModule {
                    ident: None,
                    title: "rd_parent.adl".to_string(),
                    width: 300.0,
                    height: 120.0,
                },
            )]),
            ..Options::default()
        };
        let g = generate(&parse(RESOLVED_RD), &options);
        assert!(
            g.source.contains(
                "super::OpenDisplay::open_or_focus(__open, &__rd_ctx, (\"\", __rd_args.clone()), \"rd_parent.adl\""
            ),
            "{}",
            g.source
        );
        assert!(
            g.source.contains(
                "Box::new(super::Screen::new_in(&__rd_ctx, __rs.as_ref(), super::parse_macro_args(&__rd_args)))"
            ),
            "{}",
            g.source
        );
        assert!(
            g.source.contains("__open: Vec<super::OpenDisplay>,"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains("super::OpenDisplay::show_all(__open, ui);"),
            "{}",
            g.source
        );
        // The shared runtime lives once at the file's top level, never inside
        // a child module.
        assert!(!g.source.contains("pub trait RsdmDisplay"), "{}", g.source);
        // A child is never an eframe app root: no `new(cc)` entry point (it
        // would be dead code in every consuming crate).
        assert!(
            !g.source.contains("eframe::CreationContext"),
            "{}",
            g.source
        );
    }

    // A MEDM `dynamic attribute` CALC/visibility rule on otherwise-supported
    // widgets: a rectangle with a real `calc` rule, an oval with only a `static`
    // visibility (no rule), and a composite whose rule should annotate just the
    // frame.
    const CALC: &str = r#"
"color map" {
	colors {
		ffffff,
		000000,
	}
}
rectangle {
	object {
		x=10
		y=10
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
	}
	"dynamic attribute" {
		vis="calc"
		calc="A=3"
		chan="DEV:sample"
	}
}
oval {
	object {
		x=60
		y=10
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
	}
	"dynamic attribute" {
		vis="static"
		chan="DEV:always"
	}
}
composite {
	object {
		x=100
		y=100
		width=80
		height=40
	}
	chan=""
	"dynamic attribute" {
		vis="if zero"
		chan="DEV:hide"
	}
	children {
		"text entry" {
			object {
				x=110
				y=110
				width=40
				height=18
			}
			control {
				chan="SET"
			}
		}
	}
}
"#;

    fn calc() -> Generated {
        generate(&parse(CALC), &Options::default())
    }

    #[test]
    fn dynamic_calc_rule_wraps_the_placement_in_a_visibility_gate() {
        let g = calc();
        // vis="calc" calc="A=3" -> the ORIGINAL MEDM expression under
        // dialect=medm (R1-34), channel A bound to the rule's chan, carried in
        // a calc:// gate address.
        assert!(
            g.source
                .contains("dialect=medm&expr=A=3&A=ca://DEV:sample&update=A"),
            "gate calc:// address missing or wrong:\n{}",
            g.source
        );
        // A gate Channel field is connected and the rectangle's place() is wrapped
        // in the visibility conditional.
        assert!(g.source.contains(": Channel,"), "{}", g.source);
        assert!(g.source.contains("use rsdm::Channel;"), "{}", g.source);
        let gate = g.source.find("if gate").expect("visibility conditional");
        assert!(
            g.source[gate..].contains("place(ui,"),
            "gate must wrap a place() call:\n{}",
            g.source
        );
        // The rectangle itself still emits (gated, not dropped).
        assert!(
            g.source.contains(
                "RsdmDrawing::new(&engine, \"ca://DEV:sample\", DrawingShape::Rectangle)"
            ),
            "{}",
            g.source
        );
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("dynamic visibility wired")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn visibility_gate_hides_while_the_rule_value_is_unknown() {
        let g = calc();
        // The gate shows only on a definite non-zero: `.is_some_and(|v| v != 0.0)`.
        // An unreadable gate (input channel disconnected -> the calc:// channel
        // has no value) hides the widget — MEDM parity: a disconnected
        // dynamic-attribute object is never drawn with its rule applied
        // (drawWhiteRectangle), so paired vis texts (Collecting/Done) must not
        // BOTH appear while disconnected.
        assert!(
            g.source.contains(".is_some_and(|v| v != 0.0) {"),
            "gate must hide on an unknown rule value:\n{}",
            g.source
        );
        assert!(
            !g.source.contains("!= Some(0.0)"),
            "the old unknown-means-visible gate survives:\n{}",
            g.source
        );
    }

    #[test]
    fn static_visibility_is_not_a_rule_so_emits_no_gate() {
        let g = calc();
        // The oval's dynamic attribute is vis="static" with only a channel — no
        // conditional rule — so no gate binds DEV:always, though the drawing still
        // uses that channel.
        assert!(
            !g.source.contains("A=ca://DEV:always"),
            "static visibility must not bind a gate channel:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("RsdmDrawing::new(&engine, \"ca://DEV:always\", DrawingShape::Ellipse)"),
            "{}",
            g.source
        );
    }

    #[test]
    fn absent_vis_defaults_to_static_not_if_not_zero() {
        // R2-61: MEDM omits `vis` at its V_STATIC default (always visible), so a
        // dynamic attribute with a channel but no `vis` — the ubiquitous
        // `clr="alarm"` + `chan=…SEVR` alarm-recolour pattern — must NOT be gated.
        // The old default fabricated "if not zero", hiding the widget whenever the
        // channel read 0 (NO_ALARM) and while disconnected.
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=40
		height=40
	}
	"basic attribute" {
		clr=1
		fill="solid"
	}
	"dynamic attribute" {
		clr="alarm"
		chan="$(P)status.SEVR"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // No visibility gate is wired (no calc:// vis address, no `if gate`).
        assert!(
            !g.source.contains("adl2rsdm_vis_"),
            "absent vis must not fabricate a visibility gate:\n{}",
            g.source
        );
        assert!(
            !g.source.contains("if gate"),
            "absent vis must not wrap place() in a visibility conditional:\n{}",
            g.source
        );
        // …and no visibility warning is emitted (there is no rule to wire).
        assert!(
            !g.warnings
                .iter()
                .any(|w| w.contains("dynamic visibility wired")),
            "absent vis must not warn about a wired rule: {:?}",
            g.warnings
        );
        // The shape still binds its channel and recolours by severity (the alarm
        // rule is unaffected — only the fabricated visibility rule is gone).
        assert!(
            g.source.contains("ca://$(P)status.SEVR")
                && g.source.contains("DrawingShape::Rectangle"),
            "shape must still bind its channel:\n{}",
            g.source
        );
        assert!(
            g.source.contains(".with_alarm_sensitive_content(true)"),
            "{}",
            g.source
        );
    }

    #[test]
    fn composite_dynamic_rule_gates_the_frame_not_its_child() {
        let g = calc();
        // vis="if zero" with no calc -> MEDM "A=0", channel A = the composite's
        // chan.
        assert!(
            g.source
                .contains("dialect=medm&expr=A=0&A=ca://DEV:hide&update=A"),
            "composite gate address missing or wrong:\n{}",
            g.source
        );
        // DEV:hide is the rule's channel, bound ONLY inside the gate's calc://
        // address (`A=ca://DEV:hide`). It must never appear as a widget channel —
        // neither the composite frame (which uses a synthetic `loc://`) nor the
        // inner child — so the rule gates the frame without leaking onto it.
        assert!(
            !g.source.contains("&engine, \"ca://DEV:hide\""),
            "rule channel leaked onto a widget instead of gating the frame:\n{}",
            g.source
        );
        // The gated place() is the frame's placement (the frame sorts at its
        // only child's Foreground layer, and the nested child's own place()
        // comes later inside the closure -- so the first Foreground is it).
        let mid = g
            .source
            .find("egui::Order::Foreground")
            .expect("frame place");
        assert!(
            g.source[mid.saturating_sub(200)..mid].contains("if gate"),
            "composite gate must wrap the frame placement:\n{}",
            g.source
        );
    }

    #[test]
    fn medm_calc_is_carried_verbatim_with_only_percent_and_amp_encoded() {
        // The MEDM expression is NOT translated (dialect=medm evaluates the
        // original grammar); only `%` and `&` — the bytes the calc:// query
        // cannot carry — are percent-encoded for transport.
        assert_eq!(percent_encode_calc("A=3"), "A=3");
        assert_eq!(percent_encode_calc("A#0"), "A#0");
        assert_eq!(
            percent_encode_calc("A>2?SQRT(B):MIN(A,B)"),
            "A>2?SQRT(B):MIN(A,B)"
        );
        assert_eq!(percent_encode_calc("A&&B"), "A%26%26B");
        assert_eq!(percent_encode_calc("A&B"), "A%26B");
        // `%` encodes first so an expression's own `%` never collides with the
        // escape byte.
        assert_eq!(percent_encode_calc("A%2&B"), "A%252%26B");
    }

    #[test]
    fn medm_visibility_expr_uses_calc_only_under_vis_calc() {
        assert_eq!(medm_visibility_expr("if not zero", None), "A#0");
        assert_eq!(medm_visibility_expr("if zero", None), "A=0");
        assert_eq!(medm_visibility_expr("calc", Some("A>5")), "A>5");
        assert_eq!(medm_visibility_expr("calc", None), "A");
        // MEDM ignores `calc` for if-zero/if-not-zero (calcVisibility reads the
        // channel value directly), so calc="0" must not become a constant gate.
        assert_eq!(medm_visibility_expr("if not zero", Some("0")), "A#0");
        assert_eq!(medm_visibility_expr("if zero", Some("0")), "A=0");
        assert_eq!(medm_visibility_expr("if not zero", Some("A+B")), "A#0");
    }

    #[test]
    fn dynamic_visibility_with_logical_and_is_gated_via_percent_encoding() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
rectangle {
	object {
		x=0
		y=0
		width=20
		height=20
	}
	"basic attribute" {
		clr=1
	}
	"dynamic attribute" {
		vis="calc"
		calc="A&&B"
		chan="X"
		chanB="Y"
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        // `A&&B` transports as `A%26%26B` (R1-34) — previously this bailed out
        // with a "contains '&'" warning and left the rectangle always-visible.
        assert!(
            g.source
                .contains("dialect=medm&expr=A%26%26B&A=ca://X&B=ca://Y&update=A,B"),
            "{}",
            g.source
        );
        assert!(g.source.contains("if gate"), "{}", g.source);
        assert!(
            !g.warnings.iter().any(|w| w.contains("contains '&'")),
            "{:?}",
            g.warnings
        );
        assert!(g.source.contains("DrawingShape::Rectangle"), "{}", g.source);
    }

    #[test]
    fn old_format_ctrl_and_rdbk_keys_resolve_the_channel() {
        // Pre-2.4 MEDM .adl files write `ctrl`/`rdbk` instead of `chan`; MEDM
        // still parses both (medmControl.c:36-37, medmMonitor.c:77-78).
        let adl = r#"
"color map" {
	colors {
		ffffff,
	}
}
"text entry" {
	object {
		x=0
		y=0
		width=60
		height=20
	}
	control {
		ctrl="OLD:setpoint"
		clr=0
		bclr=0
	}
}
"text update" {
	object {
		x=0
		y=30
		width=60
		height=20
	}
	monitor {
		rdbk="OLD:readback"
		clr=0
		bclr=0
	}
}
"#;
        let g = generate(&parse(adl), &Options::default());
        assert!(
            g.source.contains("\"ca://OLD:setpoint\""),
            "control ctrl= did not resolve:\n{}",
            g.source
        );
        assert!(
            g.source.contains("\"ca://OLD:readback\""),
            "monitor rdbk= did not resolve:\n{}",
            g.source
        );
        assert!(
            !g.warnings.iter().any(|w| w.contains("has no channel")),
            "{:?}",
            g.warnings
        );
    }
}
