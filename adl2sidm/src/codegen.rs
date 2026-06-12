//! Emit SiDM Rust source from a parsed [`MedmScreen`].
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
//! [`Engine`]: https://docs.rs/sidm
//! [`MedmScreen`]: crate::adl_parser::MedmScreen

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::adl_parser::{Color, Geometry, MedmScreen, MedmWidget, parse};
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
    /// related-display targets) by [`expand_macros`], mirroring MEDM's lexer.
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
    /// the driver's output file: the shared top-level items (`SidmDisplay`,
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
    /// Whether the screen allocates siplot `PlotId`s (plots/strip charts), so
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
    /// siplot uses to key their GPU resources (must be unique within a screen).
    next_plot_id: u64,
    /// Running counter for synthetic `loc://` placeholder channels (channel-less
    /// shapes, composite frames, embedded-display frames). Keyed off this rather
    /// than `widget.line` so addresses stay unique across inlined files — two
    /// widgets at the same source line in different `.adl`s must not share a
    /// channel.
    next_synthetic_id: u64,
    /// Whether any emitted code references `Color32` / `sidm::widgets`.
    needs_color: bool,
    needs_widgets: bool,
    /// Whether a label-less related display / shell command needs its MEDM icon
    /// helper (`related_display_icon` / `shell_command_icon`) appended.
    needs_rd_icon: bool,
    needs_sc_icon: bool,
    /// Whether any emitted code references `sidm::Channel` (a dynamic visibility
    /// gate field).
    needs_channel: bool,
    /// Whether any emitted string still carries a `$(macro)` reference after the
    /// convert-time `--macro` baking — it then expands at runtime against the
    /// screen instance's macro table (`__m`), so the `MacroTable` helper and the
    /// `__m` field are emitted.
    needs_macros: bool,
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

    /// A fresh synthetic `loc://adl2sidm_<kind>_<n>` placeholder address, unique
    /// across the whole screen (including inlined embedded files). `kind` labels
    /// it (`shape`/`frame`/`embed`); the monotonic `n` guarantees uniqueness even
    /// when two widgets share a source line across different `.adl`s.
    fn synthetic_addr(&mut self, kind: &str) -> String {
        let i = self.next_synthetic_id;
        self.next_synthetic_id += 1;
        format!("loc://adl2sidm_{kind}_{i}")
    }

    /// The path prefix for the shared top-level items (`SidmDisplay`,
    /// `OpenDisplay`, `parse_macro_args`, `next_plot_ids`): `super::` inside a
    /// child `pub mod`, empty at the file's top level.
    fn rt_prefix(&self) -> &'static str {
        if self.child_module { "super::" } else { "" }
    }
}

/// Generate the SiDM Rust source for a parsed MEDM screen.
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
        // `meter` has no dedicated PyDM/SiDM widget; adl2pydm draws it as an
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
            widget.line, widget.symbol, map.sidm_widget
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
/// widget with no rule (or whose expression the `calc://` address cannot carry)
/// is left ungated.
fn apply_dynamic_visibility(b: &mut Builder, widget: &MedmWidget, options: &Options, start: usize) {
    let Some(gate_addr) = visibility_gate_address(b, widget, options) else {
        return;
    };
    let id = b.index();
    let field = format!("gate{id}");
    b.needs_channel = true;
    let gate_addr_expr = medm_str(b, &gate_addr);
    b.ctors.push(format!(
        "let {field} = engine\n            .connect({gate_addr_expr})\n            .expect({});",
        rust_str(&format!("adl2sidm: connect visibility gate {gate_addr}"))
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
/// or `None` when it has no rule (`vis="static"` or no `vis`/`calc`), no channel
/// to evaluate, or an expression the `calc://` query cannot carry. The channels
/// A–D bind `chan`/`chanB`/`chanC`/`chanD`; the expression combines the `vis`
/// mode with the optional `calc` field and is translated MEDM-CALC → `evalexpr`.
fn visibility_gate_address(
    b: &mut Builder,
    widget: &MedmWidget,
    options: &Options,
) -> Option<String> {
    let da = widget.attributes.get("dynamic attribute")?;
    let vis = da.get("vis").map(String::as_str).unwrap_or("if not zero");
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

    let expr = translate_calc_to_evalexpr(&medm_visibility_expr(vis, calc));
    if expr.contains('&') {
        // The `calc://` query splits on `&`, so an expression with logical/bitwise
        // AND cannot be transported. Leave the widget always-visible and say so
        // rather than emit a silently-wrong gate.
        b.warnings.push(format!(
            "line {}: dynamic visibility expr {expr:?} contains '&' (logical/bitwise \
             AND) which a calc:// address cannot carry; left always-visible",
            widget.line
        ));
        return None;
    }

    let mut addr = format!("calc://adl2sidm_vis_{}?expr={expr}", widget.line);
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
        // "if not zero" (the MEDM default) and any unknown mode.
        (_, _) => "A#0".to_string(),
    }
}

/// Translate a MEDM CALC expression to `evalexpr` syntax. Only two operators
/// differ: `#` (not-equal) → `!=`, and `=` (equal) → `==`. MEDM's `&&`, `||`,
/// `!`, the relational operators, and arithmetic already match `evalexpr`, and
/// the channel refs `A`–`D` are bound directly as `evalexpr` variables.
fn translate_calc_to_evalexpr(medm: &str) -> String {
    replace_standalone_eq(&medm.replace('#', "!="))
}

/// Replace MEDM's `=` (equality) with `evalexpr`'s `==`, leaving the compound
/// operators `>=`, `<=`, `!=`, `==` untouched.
fn replace_standalone_eq(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '=' {
            if chars.get(i + 1) == Some(&'=') {
                out.push_str("=="); // already `==` — copy whole, skip the pair
                i += 2;
                continue;
            }
            if matches!(out.chars().last(), Some('>' | '<' | '!')) {
                out.push('='); // part of `>=`, `<=`, `!=`
            } else {
                out.push_str("==");
            }
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
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
    // renders (`SidmLabel`'s justified path). MEDM draws static text at the top
    // with a font sized to fill the height; the height-derived font is smaller,
    // so centring reproduces MEDM's visual band for group titles too. The
    // spacing runs before the alignment layout, so it serves all three aligns.
    let centring = "    let __font = ui.style().override_font_id.clone().unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()));\n    let __row = ui.fonts_mut(|f| f.row_height(&__font));\n    ui.add_space(((ui.available_height() - __row) / 2.0).max(0.0));\n";
    let body = format!("{{\n{prelude}{alarm_setup}{centring}    {aligned}\n}}");
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// `text update` — a read-only `SidmLabel` bound to a channel.
fn emit_text_update(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmLabel::new(&engine, {})", medm_str(b, &addr));
    let mut builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    builders.extend(string_format_builder(widget, &addr));
    builders.extend(alarm_content_builder(widget));
    if let Some((variant, _)) = text_alignment(widget) {
        builders.push(format!(".with_alignment(TextAlign::{variant})"));
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmLabel",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (text update)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `text entry` — an editable `SidmLineEdit` bound to a channel.
fn emit_text_entry(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmLineEdit::new(&engine, {})", medm_str(b, &addr));
    let mut builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    builders.extend(string_format_builder(widget, &addr));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmLineEdit",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr}"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `message button` — a `SidmPushButton` that writes `press_msg` (and optionally
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
        "SidmPushButton::new(&engine, {}, {}, {})",
        medm_str(b, &addr),
        medm_str(b, &label),
        medm_str(b, &press)
    );
    let mut builders = Vec::new();
    if let Some(release) = widget.assignments.get("release_msg").cloned() {
        builders.push(format!(".with_release_value({})", medm_str(b, &release)));
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmPushButton",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (message button)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `menu` — a `SidmEnumComboBox` over the channel's enum strings.
fn emit_menu(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmEnumComboBox::new(&engine, {})", medm_str(b, &addr));
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmEnumComboBox",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (menu)"),
            builders: &[],
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `choice button` — a `SidmEnumButton` group over the channel's enum strings.
/// MEDM `stacking` maps to orientation as in `adl2pydm`: `row` (default) stacks
/// vertically, `column` lays the buttons out horizontally.
fn emit_choice_button(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmEnumButton::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    let stacking = widget
        .assignments
        .get("stacking")
        .map(String::as_str)
        .unwrap_or("row");
    match stacking {
        // `row` -> Vertical, which is `SidmEnumButton`'s default, so no builder.
        "row" => {}
        "column" => builders.push(".with_orientation(Orientation::Horizontal)".to_string()),
        other => b.warnings.push(format!(
            "line {}: choice button stacking {other:?} unsupported, using 'row'",
            widget.line
        )),
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmEnumButton",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (choice button)"),
            builders: &builders,
            colors: WidgetColors::from_widget(widget),
            font_px: Some(font_px_from_height(geom.height)),
        },
    );
}

/// `valuator` — a `SidmSlider`. User-defined limits (`*Src == "default"`) and a
/// `dPrecision` map to `.with_limits` / `.with_precision`.
fn emit_valuator(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmSlider::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    if let Some((lo, hi)) = user_defined_limits(widget) {
        builders.push(format!(
            ".with_limits({}, {})",
            float_lit(lo),
            float_lit(hi)
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
            ty: "SidmSlider",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (valuator)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `wheel switch` — a `SidmSpinbox`. User-defined limits map to `.with_limits`;
/// the MEDM `format` (`integer` or `w.d`) maps to `.with_precision` decimals.
fn emit_wheel_switch(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmSpinbox::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    if let Some((lo, hi)) = user_defined_limits(widget) {
        builders.push(format!(
            ".with_limits({}, {})",
            float_lit(lo),
            float_lit(hi)
        ));
    }
    // Precision comes from MEDM `format` (what adl2pydm reads), falling back to
    // the `limits` block's `precDefault` (what real wheel-switch screens carry).
    if let Some(fmt) = widget.assignments.get("format") {
        match wheel_decimals(fmt) {
            Some(decimals) => builders.push(format!(".with_precision({decimals})")),
            None => b.warnings.push(format!(
                "line {}: wheel switch format {fmt:?} not parseable; precision left to channel",
                widget.line
            )),
        }
    } else if let Some(prec) = precision_default_builder(widget) {
        builders.push(prec);
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmSpinbox",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (wheel switch)"),
            builders: &builders,
            // The spinbox renders its value as an (uncoloured-RichText) button,
            // so `clr` reaches the displayed number through `override_text_color`
            // and `bclr` fills behind it — the same text/fill semantics as the
            // other value widgets, unlike the slider whose `clr` is a track colour.
            colors: WidgetColors::from_widget(widget),
            // adl2pydm does not size the wheel-switch font from height; keep the
            // sidm default so we stay at parity.
            font_px: None,
        },
    );
}

/// `byte` — a `SidmByteIndicator`. `sbit`/`ebit` give the bit count and shift;
/// `direction` gives the orientation (`right`/`left` -> horizontal). MEDM
/// `sbit < ebit` is big-endian (MSB first; adl2pydm's `bigEndian`), applied via
/// `with_big_endian(true)`.
fn emit_byte(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let sbit = widget
        .assignments
        .get("sbit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    let ebit = widget
        .assignments
        .get("ebit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    let num_bits = 1 + (sbit.max(ebit) - sbit.min(ebit));
    let shift = sbit.min(ebit);

    let new_call = format!("SidmByteIndicator::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    // `SidmByteIndicator` defaults: 1 bit, no shift, vertical.
    if num_bits != 1 {
        builders.push(format!(".with_num_bits({num_bits})"));
    }
    if shift != 0 {
        builders.push(format!(".with_shift({shift})"));
    }
    // MEDM bytes are bare segments — no per-bit labels (adl2pydm
    // write_block_byte_indicator emits `showLabels = False`); SidmByteIndicator
    // defaults to PyDM's labels-on. Label-less is also what routes the widget
    // through the exact-share justified division (MEDM xc/Byte.c).
    builders.push(".with_show_labels(false)".to_string());
    // `SidmByteIndicator` defaults to vertical.
    if let Some(orient) = direction_orientation(b, widget, true) {
        builders.push(orient);
    }
    // MEDM `sbit < ebit` is big-endian (MSB first), as adl2pydm maps to PyDM's
    // `bigEndian`. SidmByteIndicator defaults to little-endian, so apply the
    // builder only for the big-endian case.
    if sbit < ebit {
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
            ty: "SidmByteIndicator",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (byte)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `bar` / `indicator` / `meter` — a `SidmScaleIndicator`. `bar` draws a filled
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
    let new_call = format!("SidmScaleIndicator::new(&engine, {})", medm_str(b, &addr));
    let mut builders = Vec::new();
    if bar {
        builders.push(".with_bar_indicator(true)".to_string());
    }
    // MEDM's foreground `clr` colours the bar fill / pointer line; sidm's scale
    // indicator otherwise uses its own default blue. Reproduce the MEDM colour as
    // the static bar colour; `clrmod="alarm"` additionally tracks severity (the
    // alarm builder below), and the static colour is the `NoAlarm` fallback.
    if let Some(c) = widget.color {
        builders.push(format!(".with_bar_color({})", color_expr(c)));
        b.needs_color = true;
    }
    builders.extend(alarm_content_builder(widget));
    if let Some((lo, hi)) = user_defined_limits(widget) {
        builders.push(format!(
            ".with_limits({}, {})",
            float_lit(lo),
            float_lit(hi)
        ));
    }
    // `SidmScaleIndicator` defaults to horizontal.
    if let Some(orient) = direction_orientation(b, widget, false) {
        builders.push(orient);
    }
    if let Some(prec) = precision_default_builder(widget) {
        builders.push(prec);
    }
    // A `bar`'s value label follows the MEDM decoration `label`: it shows only
    // for `limits`/`channel` (adl2pydm's `showValue`), unlike `SidmScaleIndicator`
    // which shows it by default. `indicator`/`meter` keep the default.
    if bar {
        let label = widget.assignments.get("label").map(String::as_str);
        let show_value = matches!(label, Some("limits") | Some("channel"));
        if !show_value {
            builders.push(".with_value_label(false)".to_string());
        }
    }
    push_channel_widget(
        b,
        z,
        geom,
        ChannelWidget {
            ty: "SidmScaleIndicator",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (scale indicator)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `rectangle` / `oval` — a `SidmDrawing` of the given `shape` (`Rectangle` /
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
        "SidmDrawing::new(&engine, {}, DrawingShape::{shape})",
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
            ty: "SidmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (drawing)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// The `.with_fill(...)` / `.with_border(...)` builders for any [`SidmDrawing`]
/// shape, from the `basic attribute` block (shared by rectangle/oval/arc/
/// polygon/polyline). `solid` fills with the widget colour; `outline` (MEDM
/// `NoBrush`) draws only a border forced to width >= 1, as adl2pydm's
/// `write_basic_attribute` does. A `dash` pen style is flagged (no SidmDrawing
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
            "line {}: drawing dash border style not applied (SidmDrawing has no pen-style builder)",
            widget.line
        ));
    }
    // MEDM dynamic-attribute clr="alarm": recolour the colour this shape actually
    // draws with — the border for an `outline` shape, the fill for a solid one.
    builders.extend(drawing_alarm_builder(widget, fill_mode == "outline"));
    builders
}

/// `.with_size(...)` sized from MEDM geometry. Without it `SidmDrawing::show`
/// allocates its `DEFAULT_SIZE` (40×40), so a large group-box rectangle or a
/// long polyline collapses to a tiny square. Mirrors how `emit_image` sizes
/// `SidmImage` from the same `object` geometry — the drawing then fills exactly
/// the `place()` rect it is positioned into.
fn drawing_size_builder(geom: Geometry) -> String {
    format!(
        ".with_size(egui::Vec2::new({}, {}))",
        float_lit(f64::from(geom.width)),
        float_lit(f64::from(geom.height))
    )
}

/// `arc` — a `SidmDrawing(DrawingShape::Arc { begin_deg, span_deg })`. The MEDM
/// `begin`/`path` angles are parsed to degrees (`beginAngle`/`pathAngle`); SiDM's
/// arc keeps MEDM's X11 convention (0° at 3 o'clock, CCW positive), so the
/// parsed values are used directly (no Qt-style negation). An opaque fill paints
/// a pie wedge; `outline` paints an open stroked arc. Defaults: begin 0°, span
/// 360° when the keys are absent (a degenerate arc still draws a visible sweep).
fn emit_arc(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some(geom) = widget.geometry else {
        skip_no_geometry(b, widget);
        return;
    };
    let (addr, placeholder) = dynamic_channel(b, widget, options, "shape");
    let begin = angle_deg(widget, "beginAngle", 0.0);
    let span = angle_deg(widget, "pathAngle", 360.0);
    let new_call = format!(
        "SidmDrawing::new(&engine, {}, DrawingShape::Arc {{ begin_deg: {}, span_deg: {} }})",
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
            ty: "SidmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} (arc)"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// `polyline` / `polygon` — a `SidmDrawing(DrawingShape::Polyline|Polygon)` whose
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
        "SidmDrawing::new(&engine, {}, DrawingShape::{shape})",
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
            ty: "SidmDrawing",
            new_call: &new_call,
            connect_desc: &format!("adl2sidm: connect {addr} ({kind})"),
            builders: &builders,
            colors: WidgetColors::default(),
            font_px: None,
        },
    );
}

/// The stroke-only `.with_border(...)` builder for a `polyline` (MEDM line pen):
/// the widget colour at the `basic attribute` width, forced to >= 1 so it shows.
/// A `dash` pen style is flagged (no SidmDrawing pen-style builder).
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
            "line {}: drawing dash border style not applied (SidmDrawing has no pen-style builder)",
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

/// `composite` — a `SidmFrame` grouping its children. MEDM stores children in
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
        &format!("adl2sidm: connect {addr} (composite)"),
        &widget.children,
        (geom.x, geom.y),
        options,
    );
}

/// Emit a `SidmFrame` at `geom` whose draw closure re-draws `children`
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
        "let {frame_field} = SidmFrame::new(&engine, {addr_expr})\n            .expect({}){mark};",
        rust_str(connect_desc)
    ));
    b.fields
        .push((frame_field.clone(), "SidmFrame".to_string()));

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
    // Capture the frame's OUTER top-left before `show` insets the interior by
    // `BORDER_INSET`; children are positioned relative to this, so the inset never
    // shifts them. Named per frame so nested frames keep distinct origins.
    let origin = format!("__frame_origin_{frame_id}");
    let mut body = String::new();
    let _ = writeln!(body, "let {origin} = ui.max_rect().min;");
    let _ = writeln!(body, "let _ = {frame_field}.show(ui, |ui| {{");
    for p in &child_placements {
        write_placement(&mut body, p, dx, dy, "    ", options.use_layout, &origin);
    }
    let _ = write!(body, "}});");

    b.placements.push(Placement::drawn(z, frame_id, geom, body));
}

/// `strip chart` → `SidmTimePlot`: each MEDM `pen` is a time-series curve. A pen
/// with no `chan` is skipped (nothing to plot); a strip chart with no pens at all
/// is dropped with a warning. MEDM `period` (scaled by `units` to seconds) sets
/// the displayed time span; absent, sidm's own default span stands.
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
        adds.push(format!(
            "add_channel(&engine, {}, {}, {}).expect({});",
            medm_str(b, &addr),
            record_color(pen.get("color")),
            medm_str(b, chan),
            rust_str(&format!("adl2sidm: add strip-chart curve {chan}")),
        ));
    }
    if adds.is_empty() {
        return; // every pen lacked a channel; warnings already recorded
    }

    let mut with = Vec::new();
    if let Some(span) = strip_chart_span(widget) {
        with.push(format!(".with_time_span({})", float_lit(span)));
    }
    b.needs_color = true;
    let plot_id = plot_id_expr(b);
    push_plot_widget(
        b,
        z,
        geom,
        "SidmTimePlot",
        &format!("SidmTimePlot::new(rs, {plot_id})"),
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

/// `cartesian plot` → `SidmWaveformPlot` (default) or `SidmScatterPlot`
/// (`--use-scatterplot`). Each MEDM `trace` is one curve.
///
/// Waveform: a trace needs `ydata` (else it is skipped, as adl2pydm requires a
/// `y_channel`); `xdata` plots Y against an X array, its absence against the
/// array index. Scatter: a trace needs *both* `xdata` and `ydata` (sidm's
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
                rust_str(&format!("adl2sidm: add scatter {legend}")),
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
            // sidm waveform `add_xy_channel(y, Option<x>)`: X array optional.
            adds.push(match &xdata {
                Some(x) => format!(
                    "add_xy_channel(&engine, {}, Some({}), {}, {}).expect({});",
                    medm_str(b, &y),
                    medm_str(b, x),
                    color,
                    rust_str(&legend),
                    rust_str(&format!("adl2sidm: add waveform {legend}")),
                ),
                None => format!(
                    "add_channel(&engine, {}, {}, {}).expect({});",
                    medm_str(b, &y),
                    color,
                    rust_str(&legend),
                    rust_str(&format!("adl2sidm: add waveform {legend}")),
                ),
            });
        }
    }
    if adds.is_empty() {
        return; // no usable traces; warnings already recorded
    }

    let ty = if scatter {
        "SidmScatterPlot"
    } else {
        "SidmWaveformPlot"
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

/// The strip chart's displayed time span in seconds: `period` scaled by `units`
/// (`"minute"` → 60, `"hour"` → 3600, `"second"`/absent → 1), or `None` when no
/// `period` is given. This converts MEDM's unit-tagged period to sidm's
/// seconds-based `with_time_span`, where adl2pydm passes `period` through raw.
fn strip_chart_span(widget: &MedmWidget) -> Option<f64> {
    let period = widget.assignments.get("period")?.parse::<f64>().ok()?;
    let scale = match widget.assignments.get("units").map(String::as_str) {
        Some("minute") => 60.0,
        Some("hour") => 3600.0,
        _ => 1.0,
    };
    Some(period * scale)
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
/// `SidmImage` that decodes the file at run time and draws it scaled to the MEDM
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
    let new_call = format!("SidmImage::new({})", medm_str(b, file));
    let builders = vec![format!(
        ".with_size(egui::Vec2::new({}, {}))",
        float_lit(f64::from(geom.width)),
        float_lit(f64::from(geom.height))
    )];
    push_value_widget(b, z, geom, "SidmImage", &new_call, &builders);
}

/// Emit a fieldless labelled placeholder (a red marker `ui.label`) at the MEDM
/// geometry plus a converter warning — for widgets SiDM cannot represent but
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
/// but warned (SiDM has no run-time argument-substitution dialog).
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
/// MEDM/PyDM load at run time; SiDM has no run-time display loader, so the
/// faithful analogue is to read that file *now*, convert it, and emit its widgets
/// into a `SidmFrame` at the embedded geometry — the same inlining `composite`
/// uses, but sourced from an external file. The embedded `macros` extend (and
/// override) the parent's for the inlined subtree.
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

    let mut target = parse(&text);
    // Resolve the target's channels in the embedded directory (so a nested
    // embedded display resolves relative to *its* file), with the embedded macros
    // taking precedence over the inherited ones.
    let child_options = Options {
        macros: merged_macros(&macros, &options.macros),
        source_dir: canonical.parent().map(PathBuf::from),
        ..options.clone()
    };
    // Same parse→emit boundary as `generate`: bake this display's macros into its
    // subtree before inlining (MEDM expands macros per display, embedded values
    // winning over inherited via `merged_macros`).
    expand_macros(&mut target.widgets, &child_options.macros);
    let addr = b.synthetic_addr("embed");

    b.embed_stack.push(canonical);
    // The target's widgets are in its OWN screen coordinates (origin 0,0), so they
    // translate into the frame interior by (0, 0).
    emit_frame_container(
        b,
        z,
        geom,
        &addr,
        true,
        &format!("adl2sidm: connect {addr} (embedded {file})"),
        &target.widgets,
        (0, 0),
        &child_options,
    );
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

/// The macros for an inlined subtree: the embedded display's own macros first
/// (so they win on a key the parent also sets — [`substitute_macros`] applies the
/// first match), then the inherited parent macros.
fn merged_macros(embedded: &str, parent: &[(String, String)]) -> Vec<(String, String)> {
    let mut macros = parse_embedded_macros(embedded);
    macros.extend_from_slice(parent);
    macros
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
/// SiDM has no runtime display loader (a project-level deferral), so the button
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
    // MEDM captions the button from the widget's own `label` only; a label-less
    // related display renders just MEDM's overlapping-frames icon
    // (medmRelatedDisplay.c `renderRelatedDisplayPixmap`).
    let caption = medm_button_caption(widget);
    if caption.is_none() {
        b.needs_rd_icon = true;
        b.needs_color = true;
    }
    let (icon_fg, icon_bg) = icon_color_exprs(widget);
    let body = if let [entry] = entries.as_slice() {
        // Exactly one target: a plain button. A hover tooltip names the target so
        // it is discoverable in the GUI; adl2pydm likewise gives the button a
        // tooltip.
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
    /// MEDM `mode="replace display"` — MEDM reuses the parent's shell; SiDM
    /// opens a new window instead (deviation warned at emission).
    replace: bool,
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
             screen (SiDM has no runtime display loader; click logs the target)",
            e.name
        ));
        return (rust_str(&report), eprintln_literal(&report));
    };
    b.needs_rd_open = true;
    if e.replace {
        b.warnings.push(format!(
            "line {line}: related display mode \"replace display\" opens a new window \
             (SiDM keeps the parent open; MEDM would reuse its shell)"
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
    // instance's table at click time (MEDM `performMacroSubstitutions` — no
    // implicit inheritance of unnamed parent macros).
    let args_expr = if has_macro_ref(&e.args) {
        b.needs_macros = true;
        format!("__m.expand({})", rust_str(&e.args))
    } else if e.args.is_empty() {
        "String::new()".to_string()
    } else {
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
        let replace = spec.get("mode").is_some_and(|m| m.contains("replace"));
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
    (f64::from(height) * 0.6).round().clamp(6.0, 20.0) as f32
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
/// inside each sidm widget) inherit the layout, so the fill reaches the inner
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
    /// The sidm widget type (the `Screen` field's type).
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
    // the SiDM disconnect dash (PyDM's default would ring Minor/Major/Invalid).
    // A SidmDrawing has no framed border — its border flag recolours the
    // shape's own pen (set by drawing_alarm_builder), so it is left alone.
    if ty != "SidmDrawing" {
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
/// infallible and takes no `&engine` — e.g. a channel-less `SidmImage`. Emits
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

/// A `.with_precision(n)` builder from a widget's `precDefault` (its `limits`
/// block), or `None` when it carries no integer precision.
fn precision_default_builder(widget: &MedmWidget) -> Option<String> {
    let n = widget.assignments.get("precDefault")?.parse::<i32>().ok()?;
    Some(format!(".with_precision({n})"))
}

/// A `.with_format(DisplayFormat::String)` builder when MEDM asks for string
/// rendering — either an explicit `format="string"` or a long-string PV (a
/// `$`-suffixed channel name). Mirrors `adl2pydm`'s `write_display_format`,
/// which sets PyDM's `displayFormat=String` on exactly these two conditions for
/// text-update / text-entry widgets. `None` otherwise (the widget keeps its
/// `DisplayFormat::Default`, the only other format `adl2pydm` emits here).
fn string_format_builder(widget: &MedmWidget, addr: &str) -> Option<String> {
    let explicit_string = widget.assignments.get("format").map(String::as_str) == Some("string");
    if explicit_string || addr.ends_with('$') {
        Some(".with_format(DisplayFormat::String)".to_string())
    } else {
        None
    }
}

/// A `.with_alarm_sensitive_content(true)` builder when MEDM `clrmod="alarm"` —
/// the widget's foreground colour follows alarm severity instead of its static
/// `clr`. MEDM's other modes (`static`, the default, and `discrete`) keep the
/// static colour and emit nothing. `adl2pydm` leaves this to PyDM's widget
/// defaults; sidm defaults `alarm_sensitive_content` off, so reproducing MEDM's
/// alarm colouring needs the builder set explicitly. The MEDM palette rides
/// along: MEDM's `alarmColor` table replaces the foreground for EVERY severity
/// (`NO_ALARM` paints Green3 — medmWidget.c `alarmColorString`, utils.c
/// `alarmColor`), where PyDM keeps the static colour outside an alarm. Only
/// callers whose sidm widget actually exposes `with_alarm_sensitive_content`
/// (`SidmLabel`, `SidmDrawing`, `SidmScaleIndicator`, `SidmByteIndicator`) may
/// use this.
fn alarm_content_builder(widget: &MedmWidget) -> Option<String> {
    (widget.assignments.get("clrmod").map(String::as_str) == Some("alarm")).then(|| {
        ".with_alarm_sensitive_content(true)\n            \
         .with_alarm_palette(AlarmPalette::Medm)"
            .to_string()
    })
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

/// The `.with_alarm_sensitive_*` builder for a `SidmDrawing` whose MEDM dynamic
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
                rust_str(&format!("adl2sidm: connect alarm-colour source {addr}"))
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
/// `(sidm TextAlign variant, egui Align variant)`, for the non-default cases.
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

/// User-defined `(low, high)` limits for a control: present only when MEDM marks
/// `loprSrc`/`hoprSrc` as `"default"` (otherwise limits come from the channel).
/// Each missing default reads as `0.0`, matching `adl2pydm`'s `write_limits`.
fn user_defined_limits(widget: &MedmWidget) -> Option<(f64, f64)> {
    let lo_default = widget.assignments.get("loprSrc").map(String::as_str) == Some("default");
    let hi_default = widget.assignments.get("hoprSrc").map(String::as_str) == Some("default");
    if !(lo_default || hi_default) {
        return None;
    }
    let lo = widget
        .assignments
        .get("loprDefault")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let hi = widget
        .assignments
        .get("hoprDefault")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Some((lo, hi))
}

/// A `.with_orientation(...)` builder from a MEDM `direction`, or `None` when the
/// resolved orientation already equals the widget's own default (so no builder is
/// needed). `default_vertical` is that default (byte = vertical, scale indicator
/// = horizontal). MEDM `up`/`down` are vertical, `right`/`left` horizontal; an
/// unknown direction warns and is treated as `right` (horizontal), as adl2pydm's
/// `write_direction` default does. The single owner of MEDM direction → sidm
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

/// Decimals for a wheel-switch `format`: `"integer"` -> 0, `"w.d"` -> `d`,
/// anything else -> `None` (the caller warns).
fn wheel_decimals(fmt: &str) -> Option<i32> {
    if fmt == "integer" {
        return Some(0);
    }
    fmt.split_once('.')?.1.parse::<i32>().ok()
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
/// with macros substituted and the protocol prefixed.
fn channel_address(widget: &MedmWidget, options: &Options) -> Option<String> {
    let chan = widget
        .attributes
        .get("control")
        .and_then(|a| a.get("chan"))
        .or_else(|| widget.attributes.get("monitor").and_then(|a| a.get("chan")))?;
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
/// each parse→emit boundary ([`generate`] and embedded-display inlining). SiDM
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
/// child path, where macro values only exist at runtime — MEDM
/// `performMacroSubstitutions`). Emitted as `.as_str()` so the expression is a
/// `&str` in every context a literal fits (`&str` params, `Some(...)`,
/// `impl Into<String>`/`Into<WidgetText>`, `AsRef<OsStr>`); the expansion
/// temporary lives to the end of the enclosing statement.
fn medm_str(b: &mut Builder, s: &str) -> String {
    if has_macro_ref(s) {
        b.needs_macros = true;
        format!("__m.expand({}).as_str()", rust_str(s))
    } else {
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
        "// AUTO-GENERATED from {title} by adl2sidm -- do not edit by hand.\n"
    );

    // Imports: egis/Engine/siplot are always used; Color32 and the widget glob
    // only when something references them (keeps the output warning-clean).
    let _ = writeln!(s, "use sidm::Engine;");
    if b.needs_channel {
        let _ = writeln!(s, "use sidm::Channel;");
    }
    if b.needs_widgets {
        let _ = writeln!(s, "use sidm::widgets::*;");
    }
    if b.needs_color {
        let _ = writeln!(s, "use siplot::egui::{{self, Color32}};");
    } else {
        let _ = writeln!(s, "use siplot::egui;");
    }
    s.push('\n');

    // Struct.
    let _ = writeln!(s, "/// SiDM screen generated from `{title}`.");
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
        let _ = writeln!(s, "    __rs: Option<siplot::egui_wgpu::RenderState>,");
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
        s.push_str(MACRO_TABLE_HELPER);
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
/// whose macro values only exist at runtime).
const MACRO_TABLE_HELPER: &str = r#"
/// A display instance's macro table (MEDM `performMacroSubstitutions`):
/// substitutes `$(name)`/`${name}`, leaving unknown references in place
/// exactly as MEDM's lexer does (medm/medmCommon.c `getToken`).
pub struct MacroTable(pub Vec<(String, String)>);

impl MacroTable {
    fn expand(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (name, value) in &self.0 {
            out = out.replace(&format!("$({name})"), value);
            out = out.replace(&format!("${{{name}}}"), value);
        }
        out
    }
}
"#;

/// The shared `PlotId` allocator, emitted once at the output file's top level
/// when any screen in it carries plots (strip charts, cartesian plots): siplot
/// keys per-plot GPU resources by `PlotId` within a shared render state, so
/// every screen *instance* — related-display children included — must draw from
/// one counter.
pub(crate) const PLOT_IDS_HELPER: &str = r#"
/// Allocate a contiguous block of `count` siplot `PlotId`s, unique across every
/// screen instance built from this generated file (related-display children
/// included) -- siplot keys per-plot GPU resources by `PlotId`, so two
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
pub trait SidmDisplay {
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
    screen: Box<dyn SidmDisplay>,
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
        make: impl FnOnce() -> Box<dyn SidmDisplay>,
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
            viewport: egui::ViewportId::from_hash_of(("adl2sidm related display", n)),
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

/// Emit the constructors: `new(cc)` — the eframe entry, which installs siplot
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
            "        let rs = cc.wgpu_render_state.as_ref().expect(\"adl2sidm: a wgpu render state is required\");"
        );
        let _ = writeln!(s, "        siplot::install(rs);");
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
        "        {rs_param}: Option<&siplot::egui_wgpu::RenderState>,"
    );
    let _ = writeln!(s, "        {macros_param}: Vec<(String, String)>,");
    let _ = writeln!(s, "    ) -> Self {{");
    if b.needs_render_state {
        let _ = writeln!(
            s,
            "        let rs = render_state.expect(\"adl2sidm: this screen needs a wgpu render state for its plots\");"
        );
    }
    if b.needs_macros {
        let _ = writeln!(s, "        let __m = MacroTable(macros);");
    }
    if b.next_plot_id > 0 {
        let _ = writeln!(
            s,
            "        // This instance's block of siplot PlotIds (unique per instance, so"
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
    // closure (`SidmFrame::show(ui, |ui| ...)`) needs to touch sibling fields
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
/// so no widget's inner margin (`SidmFrame`'s `BORDER_INSET`) can shift a child.
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
    fn emits_struct_new_ui_and_place_helper() {
        let g = build(&Options::default());
        assert!(g.source.contains("pub struct Screen {"));
        assert!(
            g.source
                .contains("pub fn new(cc: &eframe::CreationContext<'_>)")
        );
        assert!(g.source.contains("pub fn ui(&mut self, ui: &mut egui::Ui)"));
        assert!(g.source.contains("fn place("));
        assert!(g.source.contains("siplot::install(rs);"));
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
                .contains("SidmLineEdit::new(&engine, \"ca://DMM1:set\")"),
            "macro+protocol not applied:\n{}",
            g.source
        );
        assert!(
            g.source
                .contains("SidmLabel::new(&engine, \"ca://DMM1:rbv\")")
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
                .contains("SidmLabel::new(&engine, __m.expand(\"ca://$(P)rbv\").as_str())"),
            "decimal text update should still be emitted:\n{}",
            g.source
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
                .contains("SidmLabel::new(&engine, __m.expand(\"ca://$(P)val\").as_str())"),
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
                .contains("SidmLabel::new(&engine, \"ca://X:val\")"),
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
        // severity; sidm's alarm_sensitive_content defaults off, so it must be
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
        // sidm widgets (severity recolours the bar/lit bits); the static clr/bclr
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
        // (default) static text. MEDM `align` → sidm alignment; left emits nothing.
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
        // Text update right-aligned → the SidmLabel carries with_alignment(Right).
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
        // (SidmLabel centres internally), so exactly the two `text` widgets.
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
                .contains("SidmPushButton::new(&engine, \"ca://MBB\", \"Go\", \"1\")"),
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
                .contains("SidmEnumComboBox::new(&engine, \"ca://MENU\")")
        );
        assert!(
            g.source
                .contains("SidmEnumButton::new(&engine, \"ca://CHO\")")
        );
        // stacking="column" -> horizontal layout.
        assert!(
            g.source
                .contains(".with_orientation(Orientation::Horizontal)")
        );
    }

    #[test]
    fn valuator_emits_user_limits_and_precision() {
        let g = controls();
        assert!(g.source.contains("SidmSlider::new(&engine, \"ca://VAL\")"));
        assert!(
            g.source.contains(".with_limits(-5.0, 5.0)"),
            "user-defined limits not emitted:\n{}",
            g.source
        );
        // dPrecision=3 -> with_precision(3).
        assert!(g.source.contains(".with_precision(3)"));
    }

    #[test]
    fn wheel_switch_format_sets_decimals() {
        let g = controls();
        assert!(g.source.contains("SidmSpinbox::new(&engine, \"ca://WHL\")"));
        // format="6.2" -> 2 decimals.
        assert!(g.source.contains(".with_precision(2)"));
    }

    #[test]
    fn wheel_switch_takes_clr_bclr_but_slider_does_not() {
        // The spinbox renders its value as an uncoloured-RichText button, so MEDM
        // `clr` reaches the number via override_text_color and `bclr` fills behind
        // it. The slider's `clr` is a track/handle colour override_text_color can't
        // reach, so it is deliberately excluded (a sidm-side gap).
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
                .contains("SidmByteIndicator::new(&engine, \"ca://BYT\")")
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
        // sbit > ebit, so NOT big-endian: no big-endian builder.
        assert!(
            !g.source.contains(".with_big_endian"),
            "little-endian byte must not emit a big-endian builder:\n{}",
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
        // instead of sidm's default green/grey.
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
    fn byte_big_endian_applied_when_sbit_below_ebit() {
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
        // sbit=0,ebit=3 -> num_bits 4, shift 0, big-endian (sbit<ebit) applied
        // (SidmByteIndicator's pub `big_endian` field honours the display order).
        assert!(g.source.contains(".with_num_bits(4)"));
        assert!(
            g.source.contains(".with_big_endian(true)"),
            "expected big-endian to be applied:\n{}",
            g.source
        );
        // It is now applied, not dropped — so no warning.
        assert!(
            !g.warnings.iter().any(|w| w.contains("big-endian")),
            "big-endian must be applied, not warned: {:?}",
            g.warnings
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
                .contains("SidmScaleIndicator::new(&engine, \"ca://BAR\")"),
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
        // label="limits" -> value label shown, so NO with_value_label(false).
        assert!(!g.source.contains(".with_value_label(false)"));
    }

    #[test]
    fn meter_and_indicator_are_pointer_scales() {
        let g = scales();
        assert!(
            g.source
                .contains("SidmScaleIndicator::new(&engine, \"ca://MTR\")")
        );
        assert!(
            g.source
                .contains("SidmScaleIndicator::new(&engine, \"ca://IND\")")
        );
        // Neither is a bar: exactly one `.with_bar_indicator(true)` (the bar).
        assert_eq!(g.source.matches(".with_bar_indicator(true)").count(), 1);
    }

    #[test]
    fn scale_indicator_bar_color_follows_medm_clr() {
        // A bar's `monitor` block `clr` is its bar/pointer colour. The parser
        // hoists it into `widget.color`; codegen must emit `.with_bar_color(...)`
        // so the bar matches MEDM instead of sidm's default blue.
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
        // A scale with no `clr` keeps sidm's default bar colour (no override).
        assert!(
            !scales().source.contains(".with_bar_color("),
            "a clr-less scale must not force a bar colour:\n{}",
            scales().source
        );
    }

    #[test]
    fn bar_without_value_label_hides_it() {
        // A bar with no `label` decoration hides the value label (PyDM default),
        // unlike the SiDM default which shows it.
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
    fn scale_indicators_are_monitors_in_the_middle_layer() {
        let g = scales();
        assert!(g.source.contains("egui::Order::Middle"));
        assert!(!g.source.contains("egui::Order::Foreground"));
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
                .contains("SidmDrawing::new(&engine, \"loc://adl2sidm_shape_"),
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
        // Sized to the MEDM 40x20 geometry, not SidmDrawing's 40x40 default.
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
                .contains("SidmDrawing::new(&engine, \"ca://DEV:STATE\", DrawingShape::Rectangle)"),
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
                .contains("SidmFrame::new(&engine, \"loc://adl2sidm_frame_"),
            "{}",
            g.source
        );
        assert!(g.source.contains(": SidmFrame,"));
        assert!(g.source.contains(": SidmDrawing,"));
        assert!(g.source.contains(": SidmLineEdit,"));
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
        // L1: SidmFrame::show insets its interior by BORDER_INSET; positioning
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
    fn composite_nests_children_under_a_single_frame_placement() {
        let g = composite();
        // The frame's Middle place() opens first, then its `show` closure, then
        // the control child's Foreground place() -- proving the control is nested
        // inside the frame, not a top-level sibling (ordering, not indentation,
        // since an 8-space prefix is a substring of a deeper-indented line).
        let frame_place = g
            .source
            .find("egui::Order::Middle")
            .expect("frame middle place");
        let frame_show = g.source.find(".show(ui, |ui| {").expect("frame show");
        let control_place = g
            .source
            .find("egui::Order::Foreground")
            .expect("control place");
        assert!(frame_place < frame_show, "{}", g.source);
        assert!(
            frame_show < control_place,
            "control must be nested in the frame closure:\n{}",
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
        let frames = g.source.matches(": SidmFrame,").count();
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
        let control_place = g
            .source
            .find("egui::Order::Foreground")
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
        assert!(g.source.contains(": SidmTimePlot,"), "{}", g.source);
        // period 2 * units "minute" (60) -> 120 s time span.
        assert!(
            g.source
                .contains("SidmTimePlot::new(rs, __plot_base).with_time_span(120.0)"),
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
    fn cartesian_plot_defaults_to_a_waveform_plot() {
        let g = plots(&Options::default());
        assert!(g.source.contains(": SidmWaveformPlot,"), "{}", g.source);
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
    fn cartesian_plot_uses_scatter_with_use_scatterplot() {
        let opts = Options {
            use_scatterplot: true,
            ..Options::default()
        };
        let g = plots(&opts);
        assert!(g.source.contains(": SidmScatterPlot,"), "{}", g.source);
        // count -> scatter buffer size.
        assert!(
            g.source
                .contains("SidmScatterPlot::new(rs, __plot_base + 1).with_buffer_size(500)"),
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
        assert!(g.source.contains("SidmTimePlot::new(rs, __plot_base)"));
        assert!(
            g.source
                .contains("SidmWaveformPlot::new(rs, __plot_base + 1)")
        );
    }

    // The formerly-deferred widgets, now all implemented for real: the static
    // shapes (arc/polyline → `DrawingShape::Arc`/`Polyline`), the static-file
    // image (`SidmImage`), the embedded display (inlined into a `SidmFrame`), and
    // the nav/shell controls (live `egui::Button`s). Each is asserted as its real
    // SiDM target below; degenerate inputs still fall back to a visible marker
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

    #[test]
    fn arc_and_polyline_emit_real_drawings_at_the_background_layer() {
        let g = deferred();
        // arc -> SidmDrawing(Arc) with the parsed begin/span degrees (2880/64=45,
        // 5760/64=90), no Qt-style negation, at the Background (decoration) layer.
        assert!(
            g.source
                .contains("DrawingShape::Arc { begin_deg: 45.0, span_deg: 90.0 }"),
            "arc not emitted with parsed angles:\n{}",
            g.source
        );
        // polyline -> SidmDrawing(Polyline) with its vertices normalised to the
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
        assert!(g.source.contains(": SidmDrawing,"), "{}", g.source);
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
    fn image_emits_a_channel_less_sidm_image_sized_to_the_geometry() {
        let g = deferred();
        // The MEDM static file image becomes a channel-less SidmImage naming the
        // file, sized to the MEDM geometry (100×73) — never a SidmImageView
        // (which would need an array channel a file image has none of).
        assert!(
            g.source.contains("SidmImage::new(\"apple.gif\")"),
            "{}",
            g.source
        );
        assert!(
            g.source
                .contains(".with_size(egui::Vec2::new(100.0, 73.0))"),
            "{}",
            g.source
        );
        assert!(!g.source.contains("SidmImageView"), "{}", g.source);
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
        let dir = std::env::temp_dir().join(format!("adl2sidm_embed_{}_{tag}", std::process::id()));
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
        // display and inlined into a SidmFrame at the embedded geometry.
        assert!(g.source.contains("SidmFrame::new"), "{}", g.source);
        // The child's text-update became a SidmLabel; the embedded macro EMB=count
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

    /// The synthetic `loc://adl2sidm_<kind>_<n>` placeholder address that each
    /// widget *constructor* connects to — one entry per channel-less widget
    /// (`SidmDrawing::new(&engine, "loc://…")`, `SidmFrame::new(&engine, …)`).
    /// Anchoring on the `(&engine, "` constructor argument skips the same address
    /// re-appearing in connect-description strings, so two equal entries mean two
    /// widgets genuinely share one channel — the E4 collision.
    fn synthetic_ctor_addrs(source: &str) -> Vec<String> {
        const ANCHOR: &str = "(&engine, \"loc://adl2sidm_";
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
        assert!(!g.source.contains("SidmPushButton"), "{}", g.source);
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
        // kept, and a click logs the target (SiDM has no runtime loader to
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
        assert!(!g.source.contains("SidmPushButton"), "{}", g.source);
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
    fn related_display_target_substitutes_parent_macros() {
        // The logged target name and macro args resolve the parent `-m` macros at
        // convert time (consistent with channel-address resolution; sidm has no
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
        // The entry's args carry an unbound $(P): expanded at click time
        // against the parent instance's table, then both the dedup key and the
        // child's macro table come from that string.
        assert!(
            g.source.contains("let __rd_args = __m.expand(\"P=$(P)\");"),
            "{}",
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
        assert!(g.source.contains("pub trait SidmDisplay"), "{}", g.source);
        assert!(g.source.contains("pub fn parse_macro_args"), "{}", g.source);
        assert!(
            g.source
                .contains("__rs: Option<siplot::egui_wgpu::RenderState>,"),
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
        assert!(!g.source.contains("pub trait SidmDisplay"), "{}", g.source);
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
        // vis="calc" calc="A=3" -> evalexpr "A==3", channel A bound to the rule's
        // chan, carried in a calc:// gate address.
        assert!(
            g.source.contains("expr=A==3&A=ca://DEV:sample&update=A"),
            "gate calc:// address missing or wrong:\n{}",
            g.source
        );
        // A gate Channel field is connected and the rectangle's place() is wrapped
        // in the visibility conditional.
        assert!(g.source.contains(": Channel,"), "{}", g.source);
        assert!(g.source.contains("use sidm::Channel;"), "{}", g.source);
        let gate = g.source.find("if gate").expect("visibility conditional");
        assert!(
            g.source[gate..].contains("place(ui,"),
            "gate must wrap a place() call:\n{}",
            g.source
        );
        // The rectangle itself still emits (gated, not dropped).
        assert!(
            g.source.contains(
                "SidmDrawing::new(&engine, \"ca://DEV:sample\", DrawingShape::Rectangle)"
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
                .contains("SidmDrawing::new(&engine, \"ca://DEV:always\", DrawingShape::Ellipse)"),
            "{}",
            g.source
        );
    }

    #[test]
    fn composite_dynamic_rule_gates_the_frame_not_its_child() {
        let g = calc();
        // vis="if zero" with no calc -> "A == 0", channel A = the composite's chan.
        assert!(
            g.source.contains("expr=A==0&A=ca://DEV:hide&update=A"),
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
        // The gated place() is the frame's Middle placement.
        let mid = g.source.find("egui::Order::Middle").expect("frame place");
        assert!(
            g.source[mid.saturating_sub(200)..mid].contains("if gate"),
            "composite gate must wrap the frame placement:\n{}",
            g.source
        );
    }

    #[test]
    fn medm_calc_translates_to_evalexpr_operators() {
        // `#` -> `!=`, standalone `=` -> `==`; the compound operators are kept.
        assert_eq!(translate_calc_to_evalexpr("A=3"), "A==3");
        assert_eq!(translate_calc_to_evalexpr("A#0"), "A!=0");
        assert_eq!(translate_calc_to_evalexpr("A>=2"), "A>=2");
        assert_eq!(translate_calc_to_evalexpr("A<=2"), "A<=2");
        assert_eq!(translate_calc_to_evalexpr("A==3"), "A==3");
        assert_eq!(translate_calc_to_evalexpr("A>1||B<2"), "A>1||B<2");
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
    fn dynamic_visibility_with_logical_and_is_left_visible_with_a_warning() {
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
        // `A&&B` has a `&`, which a calc:// query splits on -> no gate, warned, and
        // the rectangle is left always-visible (still emitted).
        assert!(!g.source.contains("calc://adl2sidm_vis_"), "{}", g.source);
        assert!(!g.source.contains("if gate"), "{}", g.source);
        assert!(
            g.warnings.iter().any(|w| w.contains("contains '&'")),
            "{:?}",
            g.warnings
        );
        assert!(g.source.contains("DrawingShape::Rectangle"), "{}", g.source);
    }
}
