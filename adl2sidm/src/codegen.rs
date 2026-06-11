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
    /// `$(name)` / `${name}` macro substitutions applied to channel names.
    pub macros: Vec<(String, String)>,
    /// Translate `cartesian plot` as a scatter plot rather than a waveform plot
    /// (mirrors adl2pydm's `--use-scatterplot`).
    pub use_scatterplot: bool,
    /// Directory the source `.adl` lives in, used to resolve an `embedded
    /// display`'s `composite file` so its target can be inlined. `None` (the
    /// default, e.g. converting from stdin or in headless tests) disables
    /// inlining — an embedded display then falls back to a placeholder.
    pub source_dir: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            protocol: "ca://".to_string(),
            macros: Vec::new(),
            use_scatterplot: false,
            source_dir: None,
        }
    }
}

/// The generated source plus any warnings (unsupported widgets, skipped
/// emitters) the caller should surface.
#[derive(Clone, Debug, Default)]
pub struct Generated {
    pub source: String,
    pub warnings: Vec<String>,
}

/// One placed widget: where it goes (`z`, `geom`, a unique Area `id`) and the
/// statement(s) that draw it inside the `place` closure. `comment` is an
/// optional line emitted just above the placement — used for the `// TODO:
/// dynamic rule:` note SiDM cannot yet apply.
struct Placement {
    z: ZLayer,
    id: u64,
    geom: Geometry,
    body: String,
    comment: Option<String>,
}

impl Placement {
    /// A placement with no attached comment (the common case).
    fn drawn(z: ZLayer, id: u64, geom: Geometry, body: String) -> Self {
        Self {
            z,
            id,
            geom,
            body,
            comment: None,
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
    /// Whether any emitted code references `Color32` / `sidm::widgets`.
    needs_color: bool,
    needs_widgets: bool,
    /// Canonical paths of the `.adl` files currently being inlined (embedded
    /// display recursion), newest last. Guards against include cycles; its length
    /// is the current nesting depth (capped at [`MAX_EMBED_DEPTH`]).
    embed_stack: Vec<PathBuf>,
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
}

/// Generate the SiDM Rust source for a parsed MEDM screen.
pub fn generate(screen: &MedmScreen, options: &Options) -> Generated {
    let mut b = Builder::default();
    for widget in &screen.widgets {
        emit_widget(&mut b, widget, options);
    }
    Generated {
        source: assemble(&b, screen),
        warnings: b.warnings,
    }
}

/// Dispatch one MEDM widget to its emitter, recording a warning for any symbol
/// whose emitter has not landed yet (or is an unsupported stub).
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
        "text" => emit_static_text(b, widget, z),
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

    // A MEDM `dynamic attribute` visibility/CALC rule has no SiDM rules engine to
    // apply it, so annotate every placement this widget produced with a
    // `// TODO: dynamic rule:` note (and warn) rather than dropping it silently.
    // A composite's children are already emitted (and annotated) above, so by
    // here `placements[start..]` is just this widget's own placement(s).
    if let Some(comment) = dynamic_rule_comment(widget) {
        for placement in &mut b.placements[start..] {
            placement.comment = Some(comment.clone());
        }
        b.warnings.push(format!(
            "line {}: {:?} -> {comment} (no rules engine); emitted as a comment",
            widget.line, widget.symbol
        ));
    }
}

/// A `// TODO: dynamic rule:` note documenting a MEDM `dynamic attribute`
/// visibility/CALC rule SiDM cannot yet apply, or `None` when the widget has no
/// such rule. A rule exists when `vis` is conditional (anything but `"static"`)
/// or a `calc` expression is present; the MEDM fields (`vis`, `calc`, and the
/// A–D channels) are quoted verbatim so a human can port them.
fn dynamic_rule_comment(widget: &MedmWidget) -> Option<String> {
    let da = widget.attributes.get("dynamic attribute")?;
    let vis = da.get("vis").map(String::as_str);
    let calc = da.get("calc");
    let has_rule = calc.is_some() || matches!(vis, Some(v) if v != "static");
    if !has_rule {
        return None;
    }

    let mut parts = Vec::new();
    if let Some(v) = vis {
        parts.push(format!("vis={v:?}"));
    }
    if let Some(c) = calc {
        parts.push(format!("calc={c:?}"));
    }
    for key in ["chan", "chanB", "chanC", "chanD"] {
        if let Some(ch) = da.get(key).filter(|c| !c.is_empty()) {
            parts.push(format!("{key}={ch:?}"));
        }
    }
    Some(format!("TODO: dynamic rule: {}", parts.join(" ")))
}

/// `text` — a static label (a fixed string, no channel). Drawn with a plain
/// `ui.label`, so it needs no struct field.
fn emit_static_text(b: &mut Builder, widget: &MedmWidget, z: ZLayer) {
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
    let body = format!(
        "ui.label(egui::RichText::new({}).color({}));",
        rust_str(&text),
        color_expr(color)
    );
    b.placements.push(Placement::drawn(z, id, geom, body));
}

/// `text update` — a read-only `SidmLabel` bound to a channel.
fn emit_text_update(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmLabel::new(&engine, {})", rust_str(&addr));
    let builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    push_channel_widget(
        b,
        z,
        geom,
        "SidmLabel",
        &new_call,
        &format!("adl2sidm: connect {addr} (text update)"),
        &builders,
    );
}

/// `text entry` — an editable `SidmLineEdit` bound to a channel.
fn emit_text_entry(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmLineEdit::new(&engine, {})", rust_str(&addr));
    let builders: Vec<String> = precision_default_builder(widget).into_iter().collect();
    push_channel_widget(
        b,
        z,
        geom,
        "SidmLineEdit",
        &new_call,
        &format!("adl2sidm: connect {addr}"),
        &builders,
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
        rust_str(&addr),
        rust_str(&label),
        rust_str(&press)
    );
    let mut builders = Vec::new();
    if let Some(release) = widget.assignments.get("release_msg") {
        builders.push(format!(".with_release_value({})", rust_str(release)));
    }
    push_channel_widget(
        b,
        z,
        geom,
        "SidmPushButton",
        &new_call,
        &format!("adl2sidm: connect {addr} (message button)"),
        &builders,
    );
}

/// `menu` — a `SidmEnumComboBox` over the channel's enum strings.
fn emit_menu(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmEnumComboBox::new(&engine, {})", rust_str(&addr));
    push_channel_widget(
        b,
        z,
        geom,
        "SidmEnumComboBox",
        &new_call,
        &format!("adl2sidm: connect {addr} (menu)"),
        &[],
    );
}

/// `choice button` — a `SidmEnumButton` group over the channel's enum strings.
/// MEDM `stacking` maps to orientation as in `adl2pydm`: `row` (default) stacks
/// vertically, `column` lays the buttons out horizontally.
fn emit_choice_button(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmEnumButton::new(&engine, {})", rust_str(&addr));
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
        "SidmEnumButton",
        &new_call,
        &format!("adl2sidm: connect {addr} (choice button)"),
        &builders,
    );
}

/// `valuator` — a `SidmSlider`. User-defined limits (`*Src == "default"`) and a
/// `dPrecision` map to `.with_limits` / `.with_precision`.
fn emit_valuator(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmSlider::new(&engine, {})", rust_str(&addr));
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
        "SidmSlider",
        &new_call,
        &format!("adl2sidm: connect {addr} (valuator)"),
        &builders,
    );
}

/// `wheel switch` — a `SidmSpinbox`. User-defined limits map to `.with_limits`;
/// the MEDM `format` (`integer` or `w.d`) maps to `.with_precision` decimals.
fn emit_wheel_switch(b: &mut Builder, widget: &MedmWidget, options: &Options, z: ZLayer) {
    let Some((geom, addr)) = resolve_channel(b, widget, options) else {
        return;
    };
    let new_call = format!("SidmSpinbox::new(&engine, {})", rust_str(&addr));
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
        "SidmSpinbox",
        &new_call,
        &format!("adl2sidm: connect {addr} (wheel switch)"),
        &builders,
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

    let new_call = format!("SidmByteIndicator::new(&engine, {})", rust_str(&addr));
    let mut builders = Vec::new();
    // `SidmByteIndicator` defaults: 1 bit, no shift, vertical.
    if num_bits != 1 {
        builders.push(format!(".with_num_bits({num_bits})"));
    }
    if shift != 0 {
        builders.push(format!(".with_shift({shift})"));
    }
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
    push_channel_widget(
        b,
        z,
        geom,
        "SidmByteIndicator",
        &new_call,
        &format!("adl2sidm: connect {addr} (byte)"),
        &builders,
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
    let new_call = format!("SidmScaleIndicator::new(&engine, {})", rust_str(&addr));
    let mut builders = Vec::new();
    if bar {
        builders.push(".with_bar_indicator(true)".to_string());
    }
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
        "SidmScaleIndicator",
        &new_call,
        &format!("adl2sidm: connect {addr} (scale indicator)"),
        &builders,
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
    let addr = dynamic_channel(widget, options, "shape");
    let new_call = format!(
        "SidmDrawing::new(&engine, {}, DrawingShape::{shape})",
        rust_str(&addr)
    );
    let builders = drawing_brush_builders(b, widget);
    push_channel_widget(
        b,
        z,
        geom,
        "SidmDrawing",
        &new_call,
        &format!("adl2sidm: connect {addr} (drawing)"),
        &builders,
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
    builders
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
    let addr = dynamic_channel(widget, options, "shape");
    let begin = angle_deg(widget, "beginAngle", 0.0);
    let span = angle_deg(widget, "pathAngle", 360.0);
    let new_call = format!(
        "SidmDrawing::new(&engine, {}, DrawingShape::Arc {{ begin_deg: {}, span_deg: {} }})",
        rust_str(&addr),
        float_lit(begin),
        float_lit(span)
    );
    let builders = drawing_brush_builders(b, widget);
    push_channel_widget(
        b,
        z,
        geom,
        "SidmDrawing",
        &new_call,
        &format!("adl2sidm: connect {addr} (arc)"),
        &builders,
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
    let addr = dynamic_channel(widget, options, "shape");
    let shape = if polygon { "Polygon" } else { "Polyline" };
    let new_call = format!(
        "SidmDrawing::new(&engine, {}, DrawingShape::{shape})",
        rust_str(&addr)
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
    push_channel_widget(
        b,
        z,
        geom,
        "SidmDrawing",
        &new_call,
        &format!("adl2sidm: connect {addr} ({kind})"),
        &builders,
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
    vec![format!(
        ".with_border({}, {})",
        color_expr(color),
        float_lit(width.max(1.0))
    )]
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
    let addr = match widget.assignments.get("chan").filter(|c| !c.is_empty()) {
        Some(chan) => apply_protocol(chan, options),
        None => format!("loc://adl2sidm_frame_{}", widget.line),
    };
    // Composite children are in absolute SCREEN coordinates, so they translate
    // into the frame interior by the composite's own origin.
    emit_frame_container(
        b,
        z,
        geom,
        &addr,
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
    connect_desc: &str,
    children: &[MedmWidget],
    child_origin: (i32, i32),
    options: &Options,
) {
    let frame_id = b.index();
    let frame_field = format!("w{frame_id}");
    b.needs_widgets = true;
    b.ctors.push(format!(
        "let {frame_field} = SidmFrame::new(&engine, {})\n            .expect({});",
        rust_str(addr),
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
    let mut body = String::new();
    let _ = writeln!(body, "let _ = {frame_field}.show(ui, |ui| {{");
    for p in &child_placements {
        write_placement(&mut body, p, dx, dy, "    ");
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
            rust_str(&addr),
            record_color(pen.get("color")),
            rust_str(chan),
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
    let plot_id = b.plot_id();
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
                rust_str(x),
                rust_str(&y),
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
                    rust_str(&y),
                    rust_str(x),
                    color,
                    rust_str(&legend),
                    rust_str(&format!("adl2sidm: add waveform {legend}")),
                ),
                None => format!(
                    "add_channel(&engine, {}, {}, {}).expect({});",
                    rust_str(&y),
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
    let plot_id = b.plot_id();
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
        format!("let _ = {field}.show(ui);"),
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
    let new_call = format!("SidmImage::new({})", rust_str(file));
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
    let body = if let [(_, command)] = entries.as_slice() {
        // Exactly one command: the button caption is the widget/command label.
        let label = deferred_button_label(widget, "commands", "Shell Command");
        format!(
            "if ui.button({}).clicked() {{\n    {}\n}}",
            rust_str(&label),
            spawn_command_stmt(command),
        )
    } else {
        // Several commands: a menu whose items each run one command, then close.
        let title = menu_title(widget, "Shell Command");
        let mut body = format!("ui.menu_button({}, |ui| {{", rust_str(&title));
        for (label, command) in &entries {
            let _ = write!(
                body,
                "\n    if ui.button({}).clicked() {{\n        {}\n        ui.close();\n    }}",
                rust_str(label),
                spawn_command_stmt(command),
            );
        }
        body.push_str("\n});");
        body
    };
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
fn spawn_command_stmt(command: &str) -> String {
    format!(
        "let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg({}).spawn();",
        rust_str(command)
    )
}

/// The caption on a multi-target *menu* button (shell command / related display):
/// the widget's MEDM `label` (sans the leading `-` MEDM uses to hide the icon),
/// else `generic`.
fn menu_title(widget: &MedmWidget, generic: &str) -> String {
    widget
        .assignments
        .get("label")
        .map(|l| l.trim_start_matches('-'))
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| generic.to_string())
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

    let target = parse(&text);
    // Resolve the target's channels in the embedded directory (so a nested
    // embedded display resolves relative to *its* file), with the embedded macros
    // taking precedence over the inherited ones.
    let child_options = Options {
        macros: merged_macros(&macros, &options.macros),
        source_dir: canonical.parent().map(PathBuf::from),
        ..options.clone()
    };
    let addr = format!("loc://adl2sidm_embed_{}", widget.line);

    b.embed_stack.push(canonical);
    // The target's widgets are in its OWN screen coordinates (origin 0,0), so they
    // translate into the frame interior by (0, 0).
    emit_frame_container(
        b,
        z,
        geom,
        &addr,
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
    let body = if let [(_, report)] = entries.as_slice() {
        // Exactly one target: a plain button captioned by the widget/target label.
        let label = deferred_button_label(widget, "displays", "Related Display");
        format!(
            "if ui.button({}).clicked() {{\n    {}\n}}",
            rust_str(&label),
            eprintln_literal(report),
        )
    } else {
        // Several targets: a menu whose items each report one target, then close.
        let title = menu_title(widget, "Related Display");
        let mut body = format!("ui.menu_button({}, |ui| {{", rust_str(&title));
        for (caption, report) in &entries {
            let _ = write!(
                body,
                "\n    if ui.button({}).clicked() {{\n        {}\n        ui.close();\n    }}",
                rust_str(caption),
                eprintln_literal(report),
            );
        }
        body.push_str("\n});");
        body
    };
    b.placements.push(Placement::drawn(z, id, geom, body));
    b.warnings.push(format!(
        "line {}: related display emitted as a navigation-reporting button/menu \
         (SiDM has no runtime display loader; click logs the target)",
        widget.line
    ));
}

/// The `(caption, report)` pairs for a related-display widget: each `display[N]`'s
/// button caption (its `label`, else its target `name`) and the message logged on
/// click — the target file plus any macro `args`. A target with no `name` is
/// dropped with a warning (nothing to open).
fn related_display_entries(b: &mut Builder, widget: &MedmWidget) -> Vec<(String, String)> {
    let displays = widget
        .records
        .get("displays")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut entries = Vec::new();
    for spec in displays {
        let Some(name) = spec.get("name").filter(|s| !s.is_empty()) else {
            b.warnings.push(format!(
                "line {}: related display entry has no name; skipped",
                widget.line
            ));
            continue;
        };
        let args = spec.get("args").map(String::as_str).unwrap_or("");
        let report = if args.is_empty() {
            format!("related display: open {name}")
        } else {
            format!("related display: open {name} (macros: {args})")
        };
        let caption = spec
            .get("label")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| name.clone());
        entries.push((caption, report));
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

/// The caption for a deferred-control placeholder button: the widget's MEDM
/// `label` (sans the leading `-` MEDM uses to hide the menu icon), else the sole
/// target's `label`/`name` when there is exactly one, else a generic name.
fn deferred_button_label(widget: &MedmWidget, records_key: &str, generic: &str) -> String {
    if let Some(trimmed) = widget
        .assignments
        .get("label")
        .map(|l| l.trim_start_matches('-'))
        .filter(|l| !l.is_empty())
    {
        return trimmed.to_string();
    }
    if let Some(records) = widget.records.get(records_key)
        && records.len() == 1
    {
        if let Some(l) = records[0].get("label").filter(|s| !s.is_empty()) {
            return l.clone();
        }
        if let Some(n) = records[0].get("name").filter(|s| !s.is_empty()) {
            return n.clone();
        }
    }
    generic.to_string()
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

/// Emit a stateful, channel-bound widget: store it as a `Screen` field, build it
/// in `new()` (`new_call.expect(connect_desc)` then the `.with_*` `builders`),
/// and draw it back-to-front in `ui()`. The single owner of channel-widget
/// emission, so every widget is placed and drawn the same way.
fn push_channel_widget(
    b: &mut Builder,
    z: ZLayer,
    geom: Geometry,
    ty: &str,
    new_call: &str,
    connect_desc: &str,
    builders: &[String],
) {
    let id = b.index();
    let field = format!("w{id}");
    b.needs_widgets = true;

    let mut ctor = format!(
        "let {field} = {new_call}\n            .expect({})",
        rust_str(connect_desc)
    );
    for bld in builders {
        let _ = write!(ctor, "\n            {bld}");
    }
    ctor.push(';');

    b.ctors.push(ctor);
    b.fields.push((field.clone(), ty.to_string()));
    // The body references the field's `&mut` local (bound by `ui()`'s `let Self {
    // .. }` destructure), not `self.field`, so a container's draw closure can hold
    // disjoint borrows of the frame and its siblings.
    b.placements.push(Placement::drawn(
        z,
        id,
        geom,
        format!("let _ = {field}.show(ui);"),
    ));
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
        format!("let _ = {field}.show(ui);"),
    ));
}

/// A `.with_precision(n)` builder from a widget's `precDefault` (its `limits`
/// block), or `None` when it carries no integer precision.
fn precision_default_builder(widget: &MedmWidget) -> Option<String> {
    let n = widget.assignments.get("precDefault")?.parse::<i32>().ok()?;
    Some(format!(".with_precision({n})"))
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
/// placeholder (`shape`, `frame`) and the widget line keeps it unique.
fn dynamic_channel(widget: &MedmWidget, options: &Options, kind: &str) -> String {
    if let Some(chan) = widget
        .attributes
        .get("dynamic attribute")
        .and_then(|a| a.get("chan"))
        .filter(|c| !c.is_empty())
    {
        return apply_protocol(chan, options);
    }
    format!("loc://adl2sidm_{kind}_{}", widget.line)
}

/// Substitute macros and prefix the protocol onto a bare MEDM channel name.
fn apply_protocol(chan: &str, options: &Options) -> String {
    format!(
        "{}{}",
        options.protocol,
        substitute_macros(chan, &options.macros)
    )
}

/// Substitute `$(name)` and `${name}` macros; unmatched references are left
/// in place (the user supplies them via `--macro`).
fn substitute_macros(input: &str, macros: &[(String, String)]) -> String {
    let mut out = input.to_string();
    for (name, value) in macros {
        out = out.replace(&format!("$({name})"), value);
        out = out.replace(&format!("${{{name}}}"), value);
    }
    out
}

/// A Rust string literal for `s`, with escaping (`{:?}` produces exactly that).
fn rust_str(s: &str) -> String {
    format!("{s:?}")
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
    for (name, ty) in &b.fields {
        let _ = writeln!(s, "    {name}: {ty},");
    }
    let _ = writeln!(s, "}}\n");

    // impl: new() + ui().
    let _ = writeln!(s, "impl Screen {{");
    emit_new(&mut s, b);
    s.push('\n');
    emit_ui(&mut s, b);
    let _ = writeln!(s, "}}\n");

    emit_place_helper(&mut s);
    s
}

/// Emit the `new(cc)` constructor.
fn emit_new(s: &mut String, b: &Builder) {
    let _ = writeln!(
        s,
        "    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {{"
    );
    let _ = writeln!(
        s,
        "        let rs = cc.wgpu_render_state.as_ref().expect(\"adl2sidm: a wgpu render state is required\");"
    );
    let _ = writeln!(s, "        siplot::install(rs);");
    let _ = writeln!(s, "        let engine = Engine::new();");
    let _ = writeln!(s, "        engine.attach_repaint(cc.egui_ctx.clone());");
    for ctor in &b.ctors {
        let _ = writeln!(s, "        {ctor}");
    }
    let _ = write!(s, "        Self {{ _engine: engine");
    for (name, _) in &b.fields {
        let _ = write!(s, ", {name}");
    }
    let _ = writeln!(s, " }}");
    let _ = writeln!(s, "    }}");
}

/// Emit the `ui()` draw method: placements sorted back-to-front.
fn emit_ui(s: &mut String, b: &Builder) {
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
    if !b.fields.is_empty() {
        let _ = write!(s, "        let Self {{ _engine: _");
        for (name, _) in &b.fields {
            let _ = write!(s, ", {name}");
        }
        let _ = writeln!(s, " }} = self;");
    }

    let mut order: Vec<&Placement> = b.placements.iter().collect();
    order.sort_by_key(|p| p.z); // stable: preserves MEDM order within a layer

    if order.is_empty() {
        let _ = writeln!(s, "        let _ = ui;");
    }
    for p in order {
        write_placement(s, p, 0, 0, "        ");
    }
    let _ = writeln!(s, "    }}");
}

/// Emit one `place(...)` call at `indent`, offsetting the geometry by `(dx, dy)`
/// — `0, 0` at the top level; a composite's origin for its children so they land
/// inside the frame's interior coordinates. The `body` may be several lines (a
/// container's nested draws), each re-indented inside the closure. An attached
/// `comment` is written just above the `place(...)` call.
fn write_placement(s: &mut String, p: &Placement, dx: i32, dy: i32, indent: &str) {
    let Geometry {
        x,
        y,
        width,
        height,
    } = p.geom;
    if let Some(comment) = &p.comment {
        let _ = writeln!(s, "{indent}// {comment}");
    }
    let _ = writeln!(
        s,
        "{indent}place(ui, {}, egui::Id::new({}u64), {}.0, {}.0, {}.0, {}.0, |ui| {{",
        p.z.order_ident(),
        p.id,
        x - dx,
        y - dy,
        width,
        height
    );
    for line in p.body.lines() {
        let _ = writeln!(s, "{indent}    {line}");
    }
    let _ = writeln!(s, "{indent}}});");
}

/// Emit the shared absolute-placement helper.
fn emit_place_helper(s: &mut String) {
    s.push_str(
        r#"/// Place `add` at an absolute MEDM position inside its own `egui::Area`. The
/// Area's `order` is the z-layer, so decoration (`Background`) renders and takes
/// input below controls (`Foreground`) regardless of call order.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let origin = ui.max_rect().min;
    let rect = egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(w, h));
    egui::Area::new(id)
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
                .contains("SidmTimePlot::new(rs, 0).with_time_span(120.0)"),
            "strip-chart span not period*units:\n{}",
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
                .contains("SidmScatterPlot::new(rs, 1).with_buffer_size(500)"),
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
        // Distinct PlotIds keep their GPU resources separate.
        assert!(g.source.contains("SidmTimePlot::new(rs, 0)"));
        assert!(g.source.contains("SidmWaveformPlot::new(rs, 1)"));
    }

    // The remaining-gap widgets: the static-file image, the embedded display,
    // and the deferred nav/shell controls. None has a faithful SiDM mapping, so
    // each warns; the visible ones emit a placeholder, never a silent drop. The
    // arc and polyline here DO map (to `DrawingShape::Arc`/`Polyline`) and are
    // asserted as real drawings.
    const STUBS: &str = r#"
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

    fn stubs() -> Generated {
        generate(&parse(STUBS), &Options::default())
    }

    #[test]
    fn arc_and_polyline_emit_real_drawings_at_the_background_layer() {
        let g = stubs();
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
    }

    #[test]
    fn image_emits_a_channel_less_sidm_image_sized_to_the_geometry() {
        let g = stubs();
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
        // The STUBS embedded display is a literal block with no `composite file`,
        // so there is nothing to inline — a visible marker, not a silent drop.
        let g = stubs();
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

    #[test]
    fn shell_command_emits_a_live_menu_spawning_each_command() {
        let g = stubs();
        // Two commands and no widget label -> a `menu_button` with the generic
        // title and one item per command. Each item spawns `sh -c "<name>"` and
        // closes the menu — a live control, not a disabled placeholder.
        assert!(
            g.source
                .contains("ui.menu_button(\"Shell Command\", |ui| {"),
            "shell command not emitted as a menu:\n{}",
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
        // the joined `"<name> <args>"` string; no menu.
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
    }

    #[test]
    fn related_display_emits_a_live_navigation_reporting_button() {
        let g = stubs();
        // The sole target -> a live, enabled button captioned by the display's
        // label that logs the target on click (SiDM has no runtime loader to
        // actually swap screens), at the control (Foreground) layer.
        assert!(
            g.source
                .contains("if ui.button(\"Open Detail\").clicked() {"),
            "related-display button not labelled with its target:\n{}",
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
            .find("Open Detail")
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
        // target, each logging the target file (and macros where present).
        assert!(
            g.source.contains("ui.menu_button(\"Screens\", |ui| {"),
            "{}",
            g.source
        );
        assert!(
            g.source.contains("if ui.button(\"A\").clicked() {"),
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
    fn dynamic_rule_emits_a_todo_comment_above_the_placement() {
        let g = calc();
        let comment = "// TODO: dynamic rule: vis=\"calc\" calc=\"A=3\" chan=\"DEV:sample\"";
        let at = g.source.find(comment).expect("rule comment");
        // The comment sits immediately above the rectangle's place() call.
        let after = &g.source[at..];
        let nl = after.find('\n').unwrap();
        assert!(
            after[nl..].trim_start().starts_with("place(ui,"),
            "comment must directly precede the placement:\n{}",
            g.source
        );
        // The widget itself still emits — the rule is documented, not a drop.
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
                .any(|w| w.contains("dynamic rule") && w.contains("no rules engine")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn static_visibility_is_not_a_rule_so_emits_no_comment() {
        let g = calc();
        // The oval's dynamic attribute is vis="static" with only a channel —
        // no conditional rule — so it gets no TODO comment, though the drawing
        // still binds that channel.
        assert!(
            !g.source.contains("chan=\\\"DEV:always\\\"")
                && !g.source.contains("dynamic rule: vis=\"static\""),
            "static visibility must not produce a rule comment:\n{}",
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
    fn composite_dynamic_rule_annotates_the_frame_not_its_child() {
        let g = calc();
        let comment = "// TODO: dynamic rule: vis=\"if zero\" chan=\"DEV:hide\"";
        let at = g.source.find(comment).expect("composite rule comment");
        // The comment precedes the frame's Middle placement, and the only such
        // rule comment appears once (the child text entry has no rule).
        let after = &g.source[at..];
        let nl = after.find('\n').unwrap();
        assert!(
            after[nl..]
                .trim_start()
                .starts_with("place(ui, egui::Order::Middle"),
            "composite rule must annotate the frame placement:\n{}",
            g.source
        );
        assert_eq!(
            g.source.matches("DEV:hide").count(),
            1,
            "rule must annotate only the frame, not be duplicated onto the child:\n{}",
            g.source
        );
    }
}
