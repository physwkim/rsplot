//! Parse a MEDM `.adl` screen file into an in-memory widget-tree IR.
//!
//! This is a faithful port of `adl2pydm/adl_parser.py`. MEDM `.adl` files are a
//! list of brace-delimited blocks:
//!
//! ```text
//! symbol {
//!     contents
//! }
//! ```
//!
//! where *contents* are nested blocks and `key=value` assignments (a value is a
//! number, a `"`-quoted string, or — for `points` — a list of `(x,y)` lines).
//! The first three top-level blocks are always `file`, `display`, and
//! `"color map"`; the remaining top-level blocks are GUI widgets (or
//! `composite`, a group of widgets).
//!
//! Like the Python original, the parser is line-oriented: a line is a block
//! opener when it ends with `" {"` and a block closer when it ends with `"}"`,
//! and `key=value` lines at the current nesting depth are assignments. The same
//! heuristics (and their limitations) are reproduced so the IR matches
//! `adl2pydm`'s.
//!
//! The result is a [`MedmScreen`] (the `display` plus its colour table and a
//! tree of [`MedmWidget`]s); [`crate::codegen`] walks it to emit RsDM Rust.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// MEDM angle unit: angles are stored as integer 1/64-degree units.
const MEDM_DEGREE_UNITS: f64 = 64.0;

/// An RGB colour from the MEDM colour map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A widget's geometry from its MEDM `object` block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A vertex from a `polyline`/`polygon` `points` block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// One MEDM widget block parsed from an `.adl` file.
///
/// The fields mirror the attributes `adl2pydm`'s `MedmBaseWidget` ends up with
/// after parsing: the geometry, foreground/background colours resolved against
/// the colour map, an optional `title` (a `text` widget's string or a labelled
/// widget's title), the remaining scalar assignments, the parsed attribute
/// sub-blocks (`control`/`monitor`/…), `points`, `composite` children, and the
/// indexed repeated sub-blocks (`traces`/`pens`/`displays`/`commands`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MedmWidget {
    /// MEDM widget type, e.g. `"text entry"` (the block symbol).
    pub symbol: String,
    /// 1-based line where this widget's block opens (for diagnostics/warnings).
    pub line: usize,
    pub geometry: Option<Geometry>,
    pub color: Option<Color>,
    pub background_color: Option<Color>,
    pub title: Option<String>,
    /// Scalar assignments remaining at this widget's level (e.g. `format`,
    /// `align`, `clrmod`), plus any folded-in `limits` fields.
    pub assignments: BTreeMap<String, String>,
    /// Parsed attribute sub-blocks (`control`, `monitor`, `param`,
    /// `basic attribute`, `dynamic attribute`) -> their assignments.
    pub attributes: BTreeMap<String, BTreeMap<String, String>>,
    /// `polyline`/`polygon` vertices.
    pub points: Vec<Point>,
    /// `composite` children (also used for the screen's top-level widgets).
    pub children: Vec<MedmWidget>,
    /// Indexed repeated sub-blocks keyed by family: `"traces"` (cartesian
    /// plot), `"pens"` (strip chart), `"displays"` (related display),
    /// `"commands"` (shell command); ordered by the MEDM index.
    pub records: BTreeMap<String, Vec<BTreeMap<String, String>>>,
}

/// A parsed MEDM screen: the `display` block plus the colour map and the
/// top-level widget tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MedmScreen {
    pub adl_filename: String,
    pub adl_version: String,
    pub color_table: Vec<Color>,
    pub geometry: Option<Geometry>,
    pub color: Option<Color>,
    pub background_color: Option<Color>,
    /// Remaining `display`-block assignments (e.g. `cmap`, `gridSpacing`).
    pub assignments: BTreeMap<String, String>,
    /// A non-blank `cmap` naming an external colormap file that could not be
    /// resolved or parsed; the colour table fell back to MEDM's default palette
    /// and [`crate::codegen`] warns. `None` for inline maps, blank-`cmap`
    /// defaults, and successful external loads.
    pub unresolved_cmap: Option<String>,
    pub widgets: Vec<MedmWidget>,
}

/// The set of MEDM block symbols that are GUI widgets (the keys of
/// `adl2pydm/symbols.py`'s `adl_widgets`). Top-level blocks not in this set
/// (`file`, `display`, `"color map"`) are screen metadata, not widgets.
pub const ADL_WIDGET_SYMBOLS: &[&str] = &[
    "arc",
    "bar",
    "byte",
    "cartesian plot",
    "choice button",
    "composite",
    "embedded display",
    "image",
    "indicator",
    "menu",
    "message button",
    "meter",
    "oval",
    "polygon",
    "polyline",
    "rectangle",
    "related display",
    "shell command",
    "strip chart",
    "text",
    "text entry",
    "text update",
    "valuator",
    "wheel switch",
];

/// A located block within a line buffer: `[start]` is the opener line, `[end]`
/// the closer line, and the content is the lines in between.
#[derive(Clone, Debug)]
struct Block {
    start: usize,
    end: usize,
    symbol: String,
}

/// Convert MEDM 1/64-degree units to degrees (port of `adl_to_deg`).
fn adl_to_deg(value: &str) -> f64 {
    value.trim().parse::<f64>().unwrap_or(0.0) / MEDM_DEGREE_UNITS
}

/// True when a line opens a block (ends with `" {"` after trimming trailing
/// whitespace), matching Python's `text.rstrip().endswith(" {")`.
fn opens_block(line: &str) -> bool {
    line.trim_end().ends_with(" {")
}

/// True when a line closes a block (ends with `"}"`).
fn closes_block(line: &str) -> bool {
    line.trim_end().ends_with('}')
}

/// The symbol of a block-opening line: the text before `" {"`, unquoted.
/// Mirrors Python's `text.strip()[:-2].strip('"')`.
fn block_symbol(line: &str) -> String {
    let stripped = line.trim();
    let without_brace = &stripped[..stripped.len() - 2];
    without_brace.trim_matches('"').to_string()
}

/// Identify the start/end of every block at nesting level 0 within `buf`.
fn locate_blocks(buf: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut nesting = 0i32;
    let mut pending: Option<(usize, String)> = None;
    for (idx, line) in buf.iter().enumerate() {
        if opens_block(line) {
            if nesting == 0 {
                pending = Some((idx, block_symbol(line)));
            }
            nesting += 1;
        } else if closes_block(line) {
            nesting -= 1;
            if nesting == 0
                && let Some((start, symbol)) = pending.take()
            {
                blocks.push(Block {
                    start,
                    end: idx,
                    symbol,
                });
            }
        }
    }
    blocks
}

/// Record every `key=value` assignment at nesting level 0 within `buf`
/// (last value wins, as in the Python dict). Block openers/closers are skipped.
fn locate_assignments(buf: &[&str]) -> BTreeMap<String, String> {
    let mut assignments = BTreeMap::new();
    let mut nesting = 0i32;
    for line in buf {
        if opens_block(line) {
            nesting += 1;
        } else if closes_block(line) {
            nesting -= 1;
        } else if nesting == 0
            && let Some(p) = line.find('=')
            && p > 0
        {
            let key = line[..p].trim().trim_matches('"').to_string();
            let value = line[p + 1..].trim().trim_matches('"').to_string();
            assignments.insert(key, value);
        }
    }
    assignments
}

/// Like [`locate_assignments`], but collecting `key=value` lines at ANY nesting
/// depth. MEDM's token-based attribute parsers match keys regardless of brace
/// depth (`parseBasicAttribute`/`parseDynamicAttribute` never gate the `T_WORD`
/// match on `nestingLevel` — medmCommon.c:534-580, :870-934), which is what makes
/// the pre-2.2 nested wrappers (`attr {}`, `mod {}`, `param {}`) parse in every
/// MEDM version. Only for blocks whose sub-block keys cannot collide.
fn locate_assignments_deep(buf: &[&str]) -> BTreeMap<String, String> {
    let mut assignments = BTreeMap::new();
    for line in buf {
        if opens_block(line) || closes_block(line) {
            continue;
        }
        if let Some(p) = line.find('=')
            && p > 0
        {
            let key = line[..p].trim().trim_matches('"').to_string();
            let value = line[p + 1..].trim().trim_matches('"').to_string();
            assignments.insert(key, value);
        }
    }
    assignments
}

/// Find the first block with the given symbol.
fn named_block<'a>(symbol: &str, blocks: &'a [Block]) -> Option<&'a Block> {
    blocks.iter().find(|b| b.symbol == symbol)
}

/// The content lines of a block (between, not including, its braces).
fn block_content<'a>(buf: &[&'a str], block: &Block) -> Vec<&'a str> {
    buf[block.start + 1..block.end].to_vec()
}

/// Resolve `clr`/`bclr` colour-index assignments against the colour table,
/// returning `(color, background_color)` and removing the consumed keys.
/// Mirrors `parseColorAssignments`: only numeric indices within the table are
/// resolved (a non-numeric `clr=alarm` is left in `assignments`).
fn take_colors(
    assignments: &mut BTreeMap<String, String>,
    color_table: &[Color],
) -> (Option<Color>, Option<Color>) {
    let mut resolve = |key: &str| -> Option<Color> {
        let value = assignments.get(key)?;
        let index: usize = value.parse().ok()?;
        let color = color_table.get(index).copied();
        if color.is_some() {
            assignments.remove(key);
        }
        color
    };
    let color = resolve("clr");
    let background = resolve("bclr");
    (color, background)
}

/// Parse an `object` block's `x/y/width/height` into a [`Geometry`].
fn parse_object_block(content: &[&str]) -> Option<Geometry> {
    let a = locate_assignments(content);
    let get = |k: &str| a.get(k).and_then(|v| v.parse::<i32>().ok());
    Some(Geometry {
        x: get("x")?,
        y: get("y")?,
        width: get("width")?,
        height: get("height")?,
    })
}

/// Labels that are reserved and must not become a widget `title`.
const RESERVED_LABELS: &[&str] = &["channel", "limits", "outline", "none", "no decorations"];

/// The attribute sub-blocks lifted into [`MedmWidget::attributes`]. `plotcom`
/// (title/xlabel/ylabel + the plot's clr/bclr — MEDM `parsePlotcom`) and the
/// cartesian-plot axis blocks (`rangeStyle`/`minRange`/`maxRange`/`axisStyle` —
/// MEDM `parsePlotAxisDefinition`) appear only on `strip chart`/
/// `cartesian plot`; like `control`/`monitor`, a `plotcom` block's `clr`/`bclr`
/// override the widget colours (they ARE the plot's fg/bg in MEDM).
const ATTRIBUTE_BLOCKS: &[&str] = &[
    "basic attribute",
    "dynamic attribute",
    "control",
    "monitor",
    "param",
    "plotcom",
    "x_axis",
    "y1_axis",
    "y2_axis",
];

/// Rolling pre-2.2 attribute state. For `versionNumber < 20200` MEDM parses
/// top-level `basic attribute`/`dynamic attribute` blocks into rolling state that
/// each later static graphic inherits (`parseAndAppendDisplayList`,
/// display.c:475-546) — the basic attribute persists across graphics, the dynamic
/// attribute is consumed by the first graphic after it is set. The same function
/// parses composite `children {}` lists, so the state threads through composites
/// in document order.
struct OldAttrs {
    /// The last-seen basic attribute (`clr`/`style`/`fill`/`width`). MEDM assigns
    /// it to EVERY old-format graphic unconditionally (display.c:516), so it
    /// always carries at least `clr` — `basicAttributeInit` is `clr=0` plus
    /// solid/solid/0, and those non-colour defaults are what absent keys already
    /// mean downstream.
    basic: BTreeMap<String, String>,
    /// The last-seen dynamic attribute (`clr` mode / `vis` / `calc` / `chan*`).
    /// Applied only while `chan` is non-empty; consumed once (display.c:526-529
    /// clears `chan[0]`, the MEDM 2.2.9 behaviour).
    dynamic: BTreeMap<String, String>,
}

impl OldAttrs {
    fn new() -> Self {
        Self {
            basic: [("clr".to_string(), "0".to_string())].into_iter().collect(),
            dynamic: BTreeMap::new(),
        }
    }
}

/// Classify a top-level block symbol as an old-format attribute carrier:
/// `Some(true)` for the basic attribute, `Some(false)` for the dynamic one.
/// Includes the `<<…>>` spellings MEDM accepts for ancient files, misspelling
/// and all (display.c:539-545).
fn old_attr_symbol(symbol: &str) -> Option<bool> {
    match symbol {
        "basic attribute" | "<<basic attribute>>" | "<<basic atribute>>" => Some(true),
        "dynamic attribute" | "<<dynamic attribute>>" => Some(false),
        _ => None,
    }
}

/// Apply the rolling old-format attributes to a just-parsed graphic — the six
/// element types MEDM rolls them onto (display.c:509-514). The basic attribute
/// REPLACES the widget's own (`pe->…->attr = attr`, unconditional), with its
/// `clr` index resolved into the widget colour exactly as a widget-carried block
/// would be; the dynamic attribute lands only while its `chan` is set, and that
/// `chan` is then cleared so the next graphic does not re-consume it.
fn apply_old_attrs(widget: &mut MedmWidget, old: &mut OldAttrs, color_table: &[Color]) {
    const OLD_GRAPHICS: &[&str] = &["arc", "oval", "polygon", "polyline", "rectangle", "text"];
    if !OLD_GRAPHICS.contains(&widget.symbol.as_str()) {
        return;
    }
    let mut basic = old.basic.clone();
    let (color, _) = take_colors(&mut basic, color_table);
    if color.is_some() {
        widget.color = color;
    }
    widget
        .attributes
        .insert("basic attribute".to_string(), basic);
    if old.dynamic.get("chan").is_some_and(|c| !c.is_empty()) {
        widget
            .attributes
            .insert("dynamic attribute".to_string(), old.dynamic.clone());
        old.dynamic.remove("chan");
    }
}

/// Parse one widget block's content into a [`MedmWidget`] (generic handling,
/// with the per-symbol extensions applied afterwards). `old` is the pre-2.2
/// rolling-attribute state, threaded through composite children in document
/// order; `None` for `versionNumber >= 20200` files.
fn parse_widget(
    symbol: &str,
    line: usize,
    content: &[&str],
    color_table: &[Color],
    old: &mut Option<OldAttrs>,
) -> MedmWidget {
    let mut assignments = locate_assignments(content);
    let blocks = locate_blocks(content);

    let (mut color, mut background_color) = take_colors(&mut assignments, color_table);

    // Geometry from the `object` block.
    let geometry =
        named_block("object", &blocks).and_then(|b| parse_object_block(&block_content(content, b)));

    // `label` (when not reserved) becomes the title.
    let title = assignments
        .get("label")
        .filter(|l| !RESERVED_LABELS.contains(&l.as_str()))
        .cloned();

    // Splice a `limits { … }` block's fields into the top-level assignments.
    if let Some(block) = named_block("limits", &blocks) {
        for l in block_content(content, block) {
            if let Some(p) = l.find('=') {
                let k = l[..p].trim().to_string();
                let v = l[p + 1..].trim().trim_matches('"').to_string();
                assignments.insert(k, v);
            }
        }
    }

    // Attribute sub-blocks (`control`/`monitor`/…). As in `adl2pydm`'s
    // `parseColorAssignments`, an attribute block's own `clr`/`bclr` OVERRIDE
    // the widget colour (a control/monitor widget carries its colour in that
    // block, not at the widget level). The blocks are visited in a fixed order
    // (`ATTRIBUTE_BLOCKS`), last resolved colour wins.
    let mut attributes = BTreeMap::new();
    for &name in ATTRIBUTE_BLOCKS {
        if let Some(block) = named_block(name, &blocks) {
            // The two attribute carriers collect keys at ANY depth: pre-2.2 MEDM
            // wrapped them in `attr {}` (basic) / `attr { mod {} param {} }`
            // (dynamic), and every MEDM version parses those nested shapes because
            // its key matching ignores brace depth (parseBasicAttribute /
            // parseDynamicAttribute). The rest keep level-0 (they never nest).
            let mut aa = if name == "basic attribute" || name == "dynamic attribute" {
                locate_assignments_deep(&block_content(content, block))
            } else {
                locate_assignments(&block_content(content, block))
            };
            let (c, b) = take_colors(&mut aa, color_table);
            if c.is_some() {
                color = c;
            }
            if b.is_some() {
                background_color = b;
            }
            attributes.insert(name.to_string(), aa);
        }
    }

    // `begin`/`path` angles (arc) -> degrees under `beginAngle`/`pathAngle`.
    for angle in ["begin", "path"] {
        if let Some(v) = assignments.remove(angle) {
            assignments.insert(format!("{angle}Angle"), adl_to_deg(&v).to_string());
        }
    }

    // `points` vertices (polyline/polygon).
    let mut points = Vec::new();
    if let Some(block) = named_block("points", &blocks) {
        for pair in block_content(content, block) {
            let cleaned = pair.replace(['(', ')'], "");
            let mut it = cleaned.split(',');
            if let (Some(x), Some(y)) = (it.next(), it.next())
                && let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse())
            {
                points.push(Point { x, y });
            }
        }
    }

    let mut widget = MedmWidget {
        symbol: symbol.to_string(),
        line,
        geometry,
        color,
        background_color,
        title,
        assignments,
        attributes,
        points,
        children: Vec::new(),
        records: BTreeMap::new(),
    };

    apply_widget_specifics(&mut widget, content, &blocks, color_table, old);
    widget
}

/// Per-symbol parsing beyond the generic handling: `text` `textix`, `composite`
/// children, and the indexed repeated sub-blocks (`trace[N]`/`pen[N]`/
/// `display[N]`/`command[N]`).
fn apply_widget_specifics(
    widget: &mut MedmWidget,
    content: &[&str],
    blocks: &[Block],
    color_table: &[Color],
    old: &mut Option<OldAttrs>,
) {
    match widget.symbol.as_str() {
        "text" => {
            if let Some(textix) = widget.assignments.remove("textix") {
                widget.title = Some(textix);
            }
        }
        "composite" => {
            if let Some(block) = named_block("children", blocks) {
                let inner = block_content(content, block);
                let inner_blocks = locate_blocks(&inner);
                // The rolling old-format state threads into the children: MEDM
                // parses a composite's list through the same
                // `parseAndAppendDisplayList` (medmComposite.c:582-585), whose
                // rolling attr/dynAttr are function-`static` — one document-order
                // stream across nesting.
                widget.children = parse_children(&inner, &inner_blocks, color_table, old);
            }
        }
        "cartesian plot" => {
            widget.records.insert(
                "traces".to_string(),
                indexed_records("trace[", content, blocks, color_table, "data_clr", false),
            );
        }
        "strip chart" => {
            widget.records.insert(
                "pens".to_string(),
                indexed_records("pen[", content, blocks, color_table, "clr", true),
            );
        }
        "related display" => {
            widget.records.insert(
                "displays".to_string(),
                indexed_records("display[", content, blocks, color_table, "", false),
            );
        }
        "shell command" => {
            widget.records.insert(
                "commands".to_string(),
                indexed_records("command[", content, blocks, color_table, "", false),
            );
        }
        _ => {}
    }
}

/// Collect indexed repeated sub-blocks whose symbol starts with `prefix`
/// (e.g. `"trace["`), ordered by their `[N]` index. When `color_key` is
/// non-empty, that colour-index field is resolved against the table and stored
/// back as the named field's index (kept as a string for the IR).
///
/// `deep` selects [`locate_assignments_deep`] over [`locate_assignments`], so a
/// record's nested sub-block fields flatten into the same map — used for
/// `strip chart` pens, whose `limits {}` block (per-pen range, MEDM `parsePen`
/// → `parseLimits`) would otherwise vanish. Only safe when the record's
/// sub-block keys cannot collide with its top-level keys (a pen's `limits`
/// keys — `loprSrc`/`hoprSrc`/`loprDefault`/… — do not collide with `chan`/`clr`).
fn indexed_records(
    prefix: &str,
    content: &[&str],
    blocks: &[Block],
    color_table: &[Color],
    color_key: &str,
    deep: bool,
) -> Vec<BTreeMap<String, String>> {
    let mut rows: Vec<(i64, BTreeMap<String, String>)> = Vec::new();
    for block in blocks {
        if !block.symbol.starts_with(prefix) {
            continue;
        }
        let pen_content = block_content(content, block);
        let mut aa = if deep {
            locate_assignments_deep(&pen_content)
        } else {
            locate_assignments(&pen_content)
        };
        if !color_key.is_empty()
            && let Some(value) = aa.get(color_key)
            && let Ok(index) = value.parse::<usize>()
            && let Some(color) = color_table.get(index)
        {
            aa.insert(
                "color".to_string(),
                format!("{},{},{}", color.r, color.g, color.b),
            );
        }
        let index = block
            .symbol
            .trim_start_matches(prefix)
            .trim_end_matches(']')
            .parse::<i64>()
            .unwrap_or(0);
        rows.push((index, aa));
    }
    rows.sort_by_key(|(i, _)| *i);
    rows.into_iter().map(|(_, aa)| aa).collect()
}

/// Parse the widget blocks within a buffer (the screen's top level, or a
/// `composite`'s children), recursing into nested composites. In old-format mode
/// (`old` is `Some`), attribute-carrier blocks update the rolling state in
/// document order and each parsed graphic inherits it — the pre-2.2 contract of
/// MEDM's `parseAndAppendDisplayList`.
fn parse_children(
    buf: &[&str],
    blocks: &[Block],
    color_table: &[Color],
    old: &mut Option<OldAttrs>,
) -> Vec<MedmWidget> {
    let mut widgets = Vec::new();
    for block in blocks {
        if old.is_some()
            && let Some(is_basic) = old_attr_symbol(&block.symbol)
        {
            // `parseOldBasicAttribute`/`parseOldDynamicAttribute` both RESET to
            // defaults before parsing (medmCommon.c:588, :943), so each block
            // replaces the rolling state rather than merging into it.
            let map = locate_assignments_deep(&block_content(buf, block));
            let state = old.as_mut().expect("checked is_some above");
            if is_basic {
                let mut basic = OldAttrs::new().basic;
                basic.extend(map);
                state.basic = basic;
            } else {
                state.dynamic = map;
            }
            continue;
        }
        if ADL_WIDGET_SYMBOLS.contains(&block.symbol.as_str()) {
            let content = block_content(buf, block);
            let mut widget =
                parse_widget(&block.symbol, block.start + 1, &content, color_table, old);
            if let Some(state) = old.as_mut() {
                apply_old_attrs(&mut widget, state, color_table);
            }
            widgets.push(widget);
        }
    }
    widgets
}

/// Parse the `"color map"` block into the colour table (the `colors` list of
/// `RRGGBB` hex, or `dl_color` r/g/b blocks).
fn parse_color_map(content: &[&str]) -> Vec<Color> {
    let blocks = locate_blocks(content);

    if let Some(block) = named_block("colors", &blocks) {
        let text = block_content(content, block).join(" ");
        return text
            .replace(',', " ")
            .split_whitespace()
            .filter_map(|hex| {
                if hex.len() < 6 {
                    return None;
                }
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color { r, g, b })
            })
            .collect();
    }

    // `dl_color` blocks: each carries r/g/b assignments.
    let mut table = Vec::new();
    for block in &blocks {
        if block.symbol != "dl_color" {
            continue;
        }
        let a = locate_assignments(&block_content(content, block));
        let get = |k: &str| a.get(k).and_then(|v| v.parse::<u8>().ok());
        if let (Some(r), Some(g), Some(b)) = (get("r"), get("g"), get("b")) {
            table.push(Color { r, g, b });
        }
    }
    table
}

/// Resolve `name` against `dir` (then `EPICS_DISPLAY_PATH`), read it, and parse
/// the `"color map"` block inside with the inline grammar ([`parse_color_map`]),
/// mirroring MEDM's `parseAndExtractExternalColormap` (medmCommon.c:1315), which
/// tokenizes the file, finds its `"color map"` word, and calls the same
/// `parseColormap` the inline path uses. `None` when the file is not found,
/// unreadable, has no `"color map"` block, or the block is empty — MEDM's
/// `NULL` return, which upstream falls to the default palette.
fn load_external_colormap(name: &str, dir: &Path) -> Option<Vec<Color>> {
    let path = resolve_colormap_path(name, dir)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let buf: Vec<&str> = text.lines().collect();
    let blocks = locate_blocks(&buf);
    let block = named_block("color map", &blocks)?;
    let table = parse_color_map(&block_content(&buf, block));
    (!table.is_empty()).then_some(table)
}

/// Resolve an external colormap file name the way MEDM's `dmOpenUsableFile`
/// (display.c) does for a search with no related-display directory: an absolute
/// name as-is, a relative one against `dir` (the `.adl`'s own directory — a
/// deliberate, faithful-in-spirit choice for a batch converter, where MEDM's
/// literal process-CWD has no analogue) then each `EPICS_DISPLAY_PATH` entry.
fn resolve_colormap_path(name: &str, dir: &Path) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut candidates = Vec::new();
    if p.is_absolute() {
        candidates.push(p.to_path_buf());
    } else {
        candidates.push(dir.join(p));
        candidates.extend(epics_display_path().iter().map(|d| d.join(p)));
    }
    candidates
        .into_iter()
        .find_map(|c| c.canonicalize().ok().filter(|c| c.is_file()))
}

/// The `EPICS_DISPLAY_PATH` search directories (platform path-separator
/// splitting, like MEDM's `dmOpenUsableFile`).
fn epics_display_path() -> Vec<PathBuf> {
    std::env::var_os("EPICS_DISPLAY_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default()
}

/// MEDM's built-in default colormap (`siteSpecific.h` `defaultDlColormap`, 65
/// entries r/g/b — the `inten` column is display-only and dropped). MEDM applies
/// it to any display that has no inline `"color map"` block and a blank `cmap`
/// (`executeDlDisplay` → `createDlColormap`, `medmCommon.c:277-284`); porting it
/// means such a screen's `clr`/`bclr` indices resolve to the same colours MEDM
/// would show instead of falling to rsdm theme defaults.
fn default_dl_colormap() -> Vec<Color> {
    const RGB: [(u8, u8, u8); 65] = [
        (255, 255, 255),
        (236, 236, 236),
        (218, 218, 218),
        (200, 200, 200),
        (187, 187, 187),
        (174, 174, 174),
        (158, 158, 158),
        (145, 145, 145),
        (133, 133, 133),
        (120, 120, 120),
        (105, 105, 105),
        (90, 90, 90),
        (70, 70, 70),
        (45, 45, 45),
        (0, 0, 0),
        (0, 216, 0),
        (30, 187, 0),
        (51, 153, 0),
        (45, 127, 0),
        (33, 108, 0),
        (253, 0, 0),
        (222, 19, 9),
        (190, 25, 11),
        (160, 18, 7),
        (130, 4, 0),
        (88, 147, 255),
        (89, 126, 225),
        (75, 110, 199),
        (58, 94, 171),
        (39, 84, 141),
        (251, 243, 74),
        (249, 218, 60),
        (238, 182, 43),
        (225, 144, 21),
        (205, 97, 0),
        (255, 176, 255),
        (214, 127, 226),
        (174, 78, 188),
        (139, 26, 150),
        (97, 10, 117),
        (164, 170, 255),
        (135, 147, 226),
        (106, 115, 193),
        (77, 82, 164),
        (52, 51, 134),
        (199, 187, 109),
        (183, 157, 92),
        (164, 126, 60),
        (125, 86, 39),
        (88, 52, 15),
        (153, 255, 255),
        (115, 223, 255),
        (78, 165, 249),
        (42, 99, 228),
        (10, 0, 184),
        (235, 241, 181),
        (212, 219, 157),
        (187, 193, 135),
        (166, 164, 98),
        (139, 130, 57),
        (115, 255, 107),
        (82, 218, 59),
        (60, 180, 32),
        (40, 147, 21),
        (26, 115, 9),
    ];
    RGB.iter().map(|&(r, g, b)| Color { r, g, b }).collect()
}

/// Parse the `display` block: geometry, foreground/background colour, and any
/// remaining assignments.
fn parse_display(content: &[&str], color_table: &[Color], screen: &mut MedmScreen) {
    let mut assignments = locate_assignments(content);
    let blocks = locate_blocks(content);

    let (color, background_color) = take_colors(&mut assignments, color_table);
    screen.color = color;
    screen.background_color = background_color;

    if let Some(block) = named_block("object", &blocks) {
        screen.geometry = parse_object_block(&block_content(content, block));
    }
    screen.assignments = assignments;
}

/// Parse the `file` block's `name`/`version` into the screen metadata. A `file`
/// block WITHOUT a `version` key means version 0 — MEDM's `parseFile` initialises
/// `versionNumber = 0` before reading the keys (medmCommon.c:107), which is how
/// ancient pre-version-key files land in the old (< 20200) format path.
fn parse_file(content: &[&str], screen: &mut MedmScreen) {
    let a = locate_assignments(content);
    if let Some(name) = a.get("name") {
        screen.adl_filename = name.clone();
    }
    screen.adl_version = a.get("version").cloned().unwrap_or_else(|| "0".to_string());
}

/// Parse a full MEDM `.adl` document into a [`MedmScreen`] with no source
/// directory: an external `cmap` colormap file cannot be resolved, so a non-blank
/// `cmap` falls back to MEDM's default palette (as it does when the file is
/// missing). Use [`parse_in_dir`] to resolve external colormaps.
pub fn parse(text: &str) -> MedmScreen {
    parse_in_dir(text, None)
}

/// Parse a full MEDM `.adl` document into a [`MedmScreen`]. `source_dir` is the
/// directory the `.adl` lives in; a non-blank `cmap` naming an external colormap
/// file is resolved against it (and `EPICS_DISPLAY_PATH`), read, and parsed so
/// `clr`/`bclr` indices resolve to the file's colours during this parse (MEDM
/// `executeDlDisplay` → `parseAndExtractExternalColormap`, medmDisplay.c:386-427).
pub fn parse_in_dir(text: &str, source_dir: Option<&Path>) -> MedmScreen {
    parse_impl(text, source_dir, None)
}

/// Parse a `.adl` that is being **embedded** into a host display, resolving every
/// `clr`/`bclr` index against `host_colormap` instead of the file's own table.
///
/// MEDM's `parseCompositeFile` (`medm/medmComposite.c:687-700`) reads the child's
/// `display` block and its `"color map"` block with `parseAndSkip` — both are
/// discarded — and appends the child's elements straight into the **parent**
/// `displayInfo`, whose colormap therefore colours them. The child's `file`
/// block is not skipped: `medmComposite.c:684` copies its version number over,
/// which is what selects the old-format attribute rolling.
///
/// The host table is the top-level display's, at any nesting depth: a composite
/// file that itself embeds another passes the same `displayInfo` down.
pub fn parse_embedded_in_dir(
    text: &str,
    source_dir: Option<&Path>,
    host_colormap: &[Color],
) -> MedmScreen {
    parse_impl(text, source_dir, Some(host_colormap))
}

/// `host_colormap` is `Some` when this document is embedded into a host display
/// that owns the colour table — see [`parse_embedded_in_dir`].
fn parse_impl(
    text: &str,
    source_dir: Option<&Path>,
    host_colormap: Option<&[Color]>,
) -> MedmScreen {
    let buf: Vec<&str> = text.lines().collect();
    let blocks = locate_blocks(&buf);

    let mut screen = MedmScreen::default();

    // `file` and `"color map"` must precede `display` (the colour table is
    // needed to resolve the display's own colours).
    if let Some(block) = named_block("file", &blocks) {
        parse_file(&block_content(&buf, block), &mut screen);
    }
    // An embedded document's own colour table is parsed and thrown away, as
    // `parseCompositeFile` does; the host's is installed in its place.
    match host_colormap {
        Some(table) => screen.color_table = table.to_vec(),
        None => {
            if let Some(block) = named_block("color map", &blocks) {
                screen.color_table = parse_color_map(&block_content(&buf, block));
            }
        }
    }
    if let Some(block) = named_block("display", &blocks) {
        let content = block_content(&buf, block);
        // MEDM colormap fallback chain (executeDlDisplay, medmDisplay.c:386-427):
        // with no inline `"color map"` block, a NON-blank `cmap` names an external
        // colormap file — read + parse it against `source_dir`; on any miss (no
        // dir, file absent, unparsable) fall to the built-in default 65-colour
        // palette (createDlColormap) and record the file so codegen warns. A BLANK
        // `cmap` falls straight to the default palette.
        if host_colormap.is_none() && screen.color_table.is_empty() {
            let cmap = locate_assignments(&content)
                .get("cmap")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            match cmap {
                None => screen.color_table = default_dl_colormap(),
                Some(file) => match source_dir.and_then(|d| load_external_colormap(&file, d)) {
                    Some(table) => screen.color_table = table,
                    None => {
                        screen.color_table = default_dl_colormap();
                        screen.unresolved_cmap = Some(file);
                    }
                },
            }
        }
        let table = screen.color_table.clone();
        parse_display(&content, &table, &mut screen);
    }

    // Pre-2.2 (`versionNumber < 20200`) files use the old attribute format:
    // top-level `basic attribute`/`dynamic attribute` blocks roll into each later
    // graphic (display.c:487,507-546). A missing `version` key inside a `file`
    // block is version 0 (old); a file with NO `file` block at all only occurs
    // synthetically and is treated as current-format (MEDM's `createDlFile`
    // default is the running version).
    let mut old =
        matches!(screen.adl_version.parse::<u32>(), Ok(v) if v < 20200).then(OldAttrs::new);

    // The remaining top-level blocks are widgets (plus, in old-format mode, the
    // rolling attribute carriers `parse_children` consumes in document order).
    let color_table = screen.color_table.clone();
    screen.widgets = parse_children(&buf, &blocks, &color_table, &mut old);

    screen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process- and call-unique scratch directory under the system temp dir,
    /// for the external-colormap file tests (R3-20). The caller creates and
    /// removes it.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("adl2rsdm_{tag}_{}_{n}", std::process::id()))
    }

    const SAMPLE: &str = r#"
file {
	name="demo.adl"
	version=030111
}
display {
	object {
		x=0
		y=0
		width=400
		height=300
	}
	clr=14
	bclr=4
}
"color map" {
	ncolors=5
	colors {
		ffffff,
		000000,
		ff0000,
		00ff00,
		0000ff,
	}
}
"text entry" {
	object {
		x=10
		y=20
		width=100
		height=22
	}
	control {
		chan="$(P)$(M).VAL"
		clr=2
		bclr=1
	}
	format="decimal"
	limits {
		precDefault=3
	}
}
text {
	object {
		x=5
		y=5
		width=80
		height=18
	}
	"basic attribute" {
		clr=0
	}
	textix="Hello"
}
"#;

    #[test]
    fn parses_file_and_color_map() {
        let screen = parse(SAMPLE);
        assert_eq!(screen.adl_filename, "demo.adl");
        assert_eq!(screen.adl_version, "030111");
        assert_eq!(screen.color_table.len(), 5);
        assert_eq!(
            screen.color_table[0],
            Color {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(screen.color_table[2], Color { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn no_inline_colormap_blank_cmap_uses_medm_default_palette() {
        // R3-20: MEDM's executeDlDisplay falls to the built-in default 65-colour
        // palette when there is no inline color map and a blank cmap. The parser
        // now injects it (createDlColormap parity) so `clr` indices resolve.
        let adl = r#"
file {
	name="x.adl"
	version=030111
}
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
"#;
        let screen = parse(adl);
        assert_eq!(screen.color_table.len(), 65, "default palette not injected");
        // Index 14 is MEDM's black; index 0 its white — the display's clr=14 /
        // bclr=0 resolve through the injected table.
        assert_eq!(screen.color_table[14], Color { r: 0, g: 0, b: 0 });
        assert_eq!(
            screen.color,
            Some(Color { r: 0, g: 0, b: 0 }),
            "display clr must resolve against the default palette"
        );
        assert_eq!(
            screen.background_color,
            Some(Color {
                r: 255,
                g: 255,
                b: 255
            })
        );
        assert!(
            screen.unresolved_cmap.is_none(),
            "a blank cmap is not an unresolved external file"
        );
    }

    #[test]
    fn non_blank_cmap_without_source_dir_falls_to_default_palette_and_signals() {
        // R3-20: a non-blank cmap names an external colormap file. With no source
        // dir (bare `parse`) it cannot be read, so — matching MEDM's "Using the
        // default colormap" (medmDisplay.c:404-420) — the table falls to the
        // default palette and `unresolved_cmap` records the file so codegen warns.
        let adl = r#"
file {
	name="x.adl"
	version=030111
}
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
"#;
        let screen = parse(adl);
        assert_eq!(
            screen.color_table.len(),
            65,
            "an unresolved external cmap falls to MEDM's default palette"
        );
        assert_eq!(
            screen.color,
            Some(Color { r: 0, g: 0, b: 0 }),
            "clr=14 resolves through the default palette (MEDM black)"
        );
        assert_eq!(
            screen.unresolved_cmap.as_deref(),
            Some("site.map"),
            "the unresolved external file must be recorded for the codegen warning"
        );
    }

    #[test]
    fn external_cmap_file_is_read_and_its_colours_resolve() {
        // R3-20: with a source dir, the named external colormap file is read and
        // parsed with the inline grammar so `clr` resolves to ITS colours.
        let dir = unique_temp_dir("r3_20_external_cmap");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("site.map"),
            "color map {\n\tncolors=2\n\tcolors {\n\t\t112233,\n\t\t445566,\n\t}\n}\n",
        )
        .unwrap();
        let adl = r#"
file {
	name="x.adl"
	version=030111
}
display {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	cmap="site.map"
	clr=1
	bclr=0
}
"#;
        let screen = parse_in_dir(adl, Some(&dir));
        assert_eq!(
            screen.color_table,
            vec![
                Color {
                    r: 0x11,
                    g: 0x22,
                    b: 0x33
                },
                Color {
                    r: 0x44,
                    g: 0x55,
                    b: 0x66
                }
            ],
            "the external file's colours must populate the table"
        );
        assert_eq!(
            screen.color,
            Some(Color {
                r: 0x44,
                g: 0x55,
                b: 0x66
            }),
            "clr=1 resolves against the external file"
        );
        assert!(screen.unresolved_cmap.is_none(), "the file resolved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_external_cmap_with_source_dir_falls_to_default_and_signals() {
        // R3-20: a source dir that does NOT contain the named file falls to the
        // default palette and still records the unresolved file.
        let dir = unique_temp_dir("r3_20_missing_cmap");
        std::fs::create_dir_all(&dir).unwrap();
        let adl = r#"
file {
	name="x.adl"
	version=030111
}
display {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	cmap="absent.map"
	clr=14
	bclr=0
}
"#;
        let screen = parse_in_dir(adl, Some(&dir));
        assert_eq!(
            screen.color_table.len(),
            65,
            "missing file → default palette"
        );
        assert_eq!(screen.unresolved_cmap.as_deref(), Some("absent.map"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inline_color_map_wins_over_a_named_cmap() {
        // R3-20: an inline `"color map"` block resolves colours directly, so a
        // `cmap` naming an external file is never consulted (and not unresolved).
        let dir = unique_temp_dir("r3_20_inline_wins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("site.map"),
            "color map {\n\tcolors {\n\t\t010203,\n\t}\n}\n",
        )
        .unwrap();
        let adl = r#"
file {
	name="x.adl"
	version=030111
}
color map {
	ncolors=2
	colors {
		aabbcc,
		ddeeff,
	}
}
display {
	object {
		x=0
		y=0
		width=100
		height=100
	}
	cmap="site.map"
	clr=1
	bclr=0
}
"#;
        let screen = parse_in_dir(adl, Some(&dir));
        assert_eq!(
            screen.color,
            Some(Color {
                r: 0xdd,
                g: 0xee,
                b: 0xff
            }),
            "the inline color map must win over the external cmap"
        );
        assert!(screen.unresolved_cmap.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_display_geometry_and_colors() {
        let screen = parse(SAMPLE);
        assert_eq!(
            screen.geometry,
            Some(Geometry {
                x: 0,
                y: 0,
                width: 400,
                height: 300,
            })
        );
        // clr=14 is out of range for the 5-colour table, so it is left as a raw
        // assignment (matching parseColorAssignments' in-range guard).
        assert_eq!(screen.color, None);
        assert_eq!(
            screen.assignments.get("clr").map(String::as_str),
            Some("14")
        );
    }

    #[test]
    fn parses_widgets_in_order_with_channels() {
        let screen = parse(SAMPLE);
        assert_eq!(screen.widgets.len(), 2);

        let entry = &screen.widgets[0];
        assert_eq!(entry.symbol, "text entry");
        assert_eq!(
            entry.geometry,
            Some(Geometry {
                x: 10,
                y: 20,
                width: 100,
                height: 22,
            })
        );
        assert_eq!(
            entry.assignments.get("format").map(String::as_str),
            Some("decimal")
        );
        // The `limits` block's field is spliced into the assignments.
        assert_eq!(
            entry.assignments.get("precDefault").map(String::as_str),
            Some("3")
        );
        // The control block's channel and resolved colours.
        let control = entry.attributes.get("control").expect("control block");
        assert_eq!(
            control.get("chan").map(String::as_str),
            Some("$(P)$(M).VAL")
        );
        assert_eq!(entry.color, Some(Color { r: 255, g: 0, b: 0 })); // clr=2
        assert_eq!(entry.background_color, Some(Color { r: 0, g: 0, b: 0 })); // bclr=1
    }

    #[test]
    fn text_widget_uses_textix_as_title() {
        let screen = parse(SAMPLE);
        let text = &screen.widgets[1];
        assert_eq!(text.symbol, "text");
        assert_eq!(text.title.as_deref(), Some("Hello"));
        assert!(!text.assignments.contains_key("textix"));
    }

    #[test]
    fn composite_children_are_parsed_recursively() {
        let adl = r#"
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
		width=200
		height=100
	}
	"composite name"=""
	children {
		rectangle {
			object {
				x=1
				y=2
				width=20
				height=10
			}
			"basic attribute" {
				clr=1
			}
		}
		"text entry" {
			object {
				x=5
				y=5
				width=50
				height=20
			}
			control {
				chan="ABC"
			}
		}
	}
}
"#;
        let screen = parse(adl);
        assert_eq!(screen.widgets.len(), 1);
        let comp = &screen.widgets[0];
        assert_eq!(comp.symbol, "composite");
        assert_eq!(comp.children.len(), 2);
        assert_eq!(comp.children[0].symbol, "rectangle");
        assert_eq!(comp.children[1].symbol, "text entry");
        assert_eq!(
            comp.children[1]
                .attributes
                .get("control")
                .and_then(|c| c.get("chan"))
                .map(String::as_str),
            Some("ABC")
        );
    }

    #[test]
    fn strip_chart_pens_are_ordered_and_colored() {
        let adl = r#"
"color map" {
	colors {
		ffffff,
		ff0000,
		00ff00,
	}
}
"strip chart" {
	object {
		x=0
		y=0
		width=300
		height=200
	}
	pen[1] {
		chan="PV.B"
		clr=2
	}
	pen[0] {
		chan="PV.A"
		clr=1
	}
}
"#;
        let screen = parse(adl);
        let chart = &screen.widgets[0];
        let pens = chart.records.get("pens").expect("pens");
        assert_eq!(pens.len(), 2);
        // Ordered by index: pen[0] then pen[1].
        assert_eq!(pens[0].get("chan").map(String::as_str), Some("PV.A"));
        assert_eq!(pens[1].get("chan").map(String::as_str), Some("PV.B"));
        // clr resolved against the table into an r,g,b "color" field.
        assert_eq!(pens[0].get("color").map(String::as_str), Some("255,0,0"));
        assert_eq!(pens[1].get("color").map(String::as_str), Some("0,255,0"));
    }
}
