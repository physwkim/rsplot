//! MEDM-widget → RsDM-widget mapping, draw category, and z-layer table.
//!
//! Port of `adl2pydm/symbols.py`'s `adl_widgets` (the MEDM-type → target-widget
//! map plus each type's category), retargeted from PyDM to RsDM. The category
//! drives two things:
//!
//! * **channel expectation** — `static` decoration carries no primary channel;
//!   `monitor`/`controller` widgets do (so the emitter knows whether to wire a
//!   PV).
//! * **z-order** — the user's rule "decoration to the back, controls never
//!   occluded". In egui a later-drawn `Area` renders on top *and captures
//!   pointer input*, so a static rectangle over a control would hide it and
//!   steal its clicks. [`Category::z_layer`] is the single owner of the
//!   back-to-front ordering the code emitter applies per container.
//!
//! Divergence from `symbols.py` noted inline: `related display` and `shell
//! command` are typed `static` there but are clickable buttons here, so they
//! sit in the [`Category::Control`] (front) layer — that is exactly the
//! "control must not be occluded" case.

/// The back-to-front draw layer a widget is placed in. Maps directly onto
/// `egui::Order` in the generated code. The variant order is the draw order, so
/// `derive(Ord)` gives `Background < Middle < Foreground` — a stable sort by
/// `ZLayer` lays widgets out back-to-front.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZLayer {
    /// Decoration — drawn first, lowest layer (`egui::Order::Background`).
    Background,
    /// Read-only monitors and containers — middle layer (`egui::Order::Middle`).
    Middle,
    /// Interactive controls — drawn last, top layer (`egui::Order::Foreground`),
    /// so they are never occluded and always receive clicks.
    Foreground,
}

impl ZLayer {
    /// The fully-qualified `egui::Order` variant the code emitter writes.
    pub fn order_ident(self) -> &'static str {
        match self {
            ZLayer::Background => "egui::Order::Background",
            ZLayer::Middle => "egui::Order::Middle",
            ZLayer::Foreground => "egui::Order::Foreground",
        }
    }
}

/// A MEDM widget's draw/role category (the analogue of `symbols.py`'s `type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Pure decoration: static graphics and the static text label. No primary
    /// channel, no input. (`symbols.py` `type="static"`.)
    Decoration,
    /// A read-only data display driven by a channel. (`type="monitor"`.)
    Monitor,
    /// An interactive control the user edits or clicks; must never be occluded.
    /// (`type="controller"`, plus `related display` / `shell command`, which
    /// `symbols.py` types `static` but which are clickable.)
    Control,
    /// A container of other widgets (`composite`/`embedded display`); its
    /// children are re-layered inside it.
    Container,
}

impl Category {
    /// The z-layer this category is placed in (decoration behind, controls on
    /// top). Containers sit in the middle: a frame should not float above a
    /// sibling control, and its own children carry their own layering.
    pub fn z_layer(self) -> ZLayer {
        match self {
            Category::Decoration => ZLayer::Background,
            Category::Monitor | Category::Container => ZLayer::Middle,
            Category::Control => ZLayer::Foreground,
        }
    }

    /// Whether this category carries a primary PV channel the emitter must wire.
    pub fn has_channel(self) -> bool {
        matches!(self, Category::Monitor | Category::Control)
    }
}

/// The mapping of one MEDM widget to its RsDM target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetMap {
    /// Draw/role category (z-layer + channel expectation).
    pub category: Category,
    /// The RsDM widget the emitter targets, e.g. `"RsdmLabel"`. Every MEDM
    /// widget now maps to a faithful target — no stubs remain.
    pub rsdm_widget: &'static str,
}

/// Look up the RsDM mapping for a MEDM widget symbol. Returns `None` for a
/// symbol that is not a MEDM widget (screen metadata such as `file`/`display`).
pub fn lookup(symbol: &str) -> Option<WidgetMap> {
    use Category::{Container, Control, Decoration, Monitor};
    let (category, rsdm_widget) = match symbol {
        // --- direct: existing RsDM widget ---
        "text" => (Decoration, "RsdmLabel (static text)"),
        "text update" => (Monitor, "RsdmLabel"),
        "text entry" => (Control, "RsdmLineEdit"),
        "menu" => (Control, "RsdmEnumComboBox"),
        "choice button" => (Control, "RsdmEnumButton"),
        "message button" => (Control, "RsdmPushButton"),
        "valuator" => (Control, "RsdmSlider"),
        "wheel switch" => (Control, "RsdmSpinbox"),
        "byte" => (Monitor, "RsdmByteIndicator"),
        "bar" => (Monitor, "RsdmScaleIndicator"),
        "indicator" => (Monitor, "RsdmScaleIndicator"),
        "meter" => (Monitor, "RsdmScaleIndicator"),
        "composite" => (Container, "RsdmFrame"),
        "rectangle" => (Decoration, "RsdmDrawing(Rectangle)"),
        "oval" => (Decoration, "RsdmDrawing(Ellipse)"),
        "strip chart" => (Monitor, "RsdmTimePlot"),
        "cartesian plot" => (Monitor, "RsdmWaveformPlot"),
        "arc" => (Decoration, "RsdmDrawing(Arc)"),
        "polygon" => (Decoration, "RsdmDrawing(Polygon)"),
        "polyline" => (Decoration, "RsdmDrawing(Polyline)"),
        // Divergence from `symbols.py` (`type="monitor"`): the MEDM `image` is a
        // static GIF/TIFF *file* with no data channel, emitted as a channel-less
        // `RsdmImage`. It is decoration, so it belongs in the Background layer with
        // the other static graphics (Qt gives adl2pydm native z-order; our 3-bucket
        // model must bucket it with decorations to keep it behind monitors/controls
        // and preserve its draw order relative to sibling static shapes).
        "image" => (Decoration, "RsdmImage"),

        // --- clickable nav/action widgets: `symbols.py` types these `static`,
        // but they are interactive buttons here, so they sit in the Control
        // (front) layer where a decoration cannot occlude them ---
        "related display" => (Control, "egui::Button (related display)"),
        "shell command" => (Control, "egui::Button (shell command)"),
        // The MEDM embedded display is inlined at code-gen time: its target
        // screen is parsed and its widgets re-layered inside a `RsdmFrame`.
        "embedded display" => (Container, "RsdmFrame (inlined)"),

        _ => return None,
    };
    Some(WidgetMap {
        category,
        rsdm_widget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adl_parser::ADL_WIDGET_SYMBOLS;

    #[test]
    fn every_medm_widget_symbol_is_mapped() {
        for &symbol in ADL_WIDGET_SYMBOLS {
            assert!(
                lookup(symbol).is_some(),
                "MEDM widget {symbol:?} has no RsDM mapping"
            );
        }
    }

    #[test]
    fn non_widget_symbols_are_unmapped() {
        for symbol in ["file", "display", "color map", "object", "control"] {
            assert!(
                lookup(symbol).is_none(),
                "{symbol:?} should not be a widget"
            );
        }
    }

    #[test]
    fn z_layer_orders_decoration_behind_controls() {
        // The core of the user's rule: a decoration's layer is strictly behind
        // a monitor's, which is behind a control's.
        let deco = lookup("rectangle").unwrap().category.z_layer();
        let monitor = lookup("text update").unwrap().category.z_layer();
        let control = lookup("text entry").unwrap().category.z_layer();
        assert_eq!(deco, ZLayer::Background);
        assert_eq!(monitor, ZLayer::Middle);
        assert_eq!(control, ZLayer::Foreground);
    }

    #[test]
    fn static_text_and_text_update_share_widget_but_not_category() {
        // `text` is static decoration (a fixed string, no channel); `text
        // update` is a channel monitor. Both target RsdmLabel.
        let text = lookup("text").unwrap();
        let update = lookup("text update").unwrap();
        assert_eq!(text.category, Category::Decoration);
        assert!(!text.category.has_channel());
        assert_eq!(update.category, Category::Monitor);
        assert!(update.category.has_channel());
    }

    #[test]
    fn clickable_nav_widgets_are_front_layer_even_though_adl2pydm_types_them_static() {
        // related display / shell command are buttons -> Control (front), so a
        // decoration cannot occlude them.
        for symbol in ["related display", "shell command"] {
            let map = lookup(symbol).unwrap();
            assert_eq!(map.category, Category::Control, "{symbol}");
            assert_eq!(map.category.z_layer(), ZLayer::Foreground, "{symbol}");
        }
    }

    #[test]
    fn every_widget_maps_to_a_real_target_no_stubs_remain() {
        // The former stub set (arc/polygon/polyline/related display/shell
        // command/embedded display) and `image` are all implemented now, so no
        // mapping may describe a stub strategy.
        for &symbol in ADL_WIDGET_SYMBOLS {
            let map = lookup(symbol).unwrap();
            assert!(
                !map.rsdm_widget.contains("stub"),
                "{symbol} still maps to a stub: {}",
                map.rsdm_widget
            );
        }
    }
}
