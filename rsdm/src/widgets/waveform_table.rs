//! `RsdmWaveformTable` — an editable grid of a waveform channel's elements.
//!
//! Ports `pydm/widgets/waveformtable.py` (`PyDMWaveformTable`): a numeric array
//! channel laid out across a fixed number of columns (rows = `ceil(len/cols)`),
//! each element shown in its own editable cell. Editing a cell and committing it
//! (Enter) parses the text as the array's element type, sets that one element,
//! and writes the **whole** array back (PyDM `send_waveform` emits
//! `self.waveform`). Optional column/row header labels fall back to 1-based
//! indices (Qt's default numeric headers).
//!
//! The layout and write-back are the pure, unit-tested [`row_count`] /
//! [`cell_index`] / [`apply_cell_edit`]; the egui shell reuses
//! [`RsdmLineEdit`](crate::widgets::RsdmLineEdit)'s focus-frozen-buffer /
//! Enter-commit / no-local-echo mechanism per cell.
//!
//! **Deviation:** only numeric arrays (`FloatArray` / `IntArray`) are editable;
//! the per-element display is the value's plain string (PyDM `str(element)`),
//! not a precision/format-spec rendering.

use rsplot::egui;

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{BorderMode, ChannelBase};

/// Default column count (PyDM `setColumnCount(1)`).
pub const DEFAULT_COLUMN_COUNT: usize = 1;

/// Number of rows needed to lay `len` elements into `cols` columns (PyDM
/// `len // col + (1 if len % col else 0)` — ceiling division). A `cols` of 0
/// yields 0 rows.
pub fn row_count(len: usize, cols: usize) -> usize {
    if cols == 0 { 0 } else { len.div_ceil(cols) }
}

/// Flat waveform index of the cell at `(row, col)` (PyDM `row*colCount + col`).
pub fn cell_index(row: usize, col: usize, cols: usize) -> usize {
    row * cols + col
}

/// Apply an edited cell `text` at flat `index` to the `current` waveform,
/// returning the whole new array to write (PyDM `send_waveform`: parse the text
/// as the array element type, set that element, emit the array). Returns `None`
/// when `current` is not a numeric array, `index` is out of range, or the text
/// does not parse as the element type.
pub fn apply_cell_edit(current: &PvValue, index: usize, text: &str) -> Option<PvValue> {
    let t = text.trim();
    match current {
        PvValue::FloatArray(a) => {
            let v: f64 = t.parse().ok()?;
            (index < a.len()).then(|| {
                let mut next = a.to_vec();
                next[index] = v;
                PvValue::FloatArray(next.into())
            })
        }
        PvValue::IntArray(a) => {
            let v: i64 = t.parse().ok()?;
            (index < a.len()).then(|| {
                let mut next = a.to_vec();
                next[index] = v;
                PvValue::IntArray(next.into())
            })
        }
        _ => None,
    }
}

/// The per-element display strings (PyDM `str(element)`), or empty for a value
/// that is not a numeric array.
fn cell_strings(value: Option<&PvValue>) -> Vec<String> {
    match value {
        Some(PvValue::FloatArray(a)) => a.iter().map(|x| x.to_string()).collect(),
        Some(PvValue::IntArray(a)) => a.iter().map(|x| x.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// An editable table of a waveform channel's elements (PyDM `PyDMWaveformTable`).
pub struct RsdmWaveformTable {
    base: ChannelBase,
    /// Number of columns the array is laid across (PyDM `columnCount`).
    column_count: usize,
    /// Column header labels; a missing entry falls back to the 1-based column
    /// number (PyDM `columnHeaderLabels`, default `["Value"]`).
    column_headers: Vec<String>,
    /// Row header labels; a missing entry falls back to the 1-based row number
    /// (PyDM `rowHeaderLabels`, default empty → numeric).
    row_headers: Vec<String>,
    /// Per-element edit buffers; the cell being edited is frozen against
    /// incoming updates.
    cells: Vec<String>,
    /// Flat index of the cell currently holding keyboard focus.
    editing: Option<usize>,
}

impl RsdmWaveformTable {
    /// Connect `address` and wrap it in a waveform table with PyDM's defaults
    /// (one column, a `Value` column header).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            column_count: DEFAULT_COLUMN_COUNT,
            column_headers: vec!["Value".to_owned()],
            row_headers: Vec::new(),
            cells: Vec::new(),
            editing: None,
        })
    }

    /// Set the number of columns (builder style; PyDM `columnCount`). A count of
    /// 0 is treated as 1 when rendering.
    pub fn with_column_count(mut self, column_count: usize) -> Self {
        self.column_count = column_count;
        self
    }

    /// Set the column header labels (builder style; PyDM `columnHeaderLabels`).
    pub fn with_column_headers(mut self, headers: Vec<String>) -> Self {
        self.column_headers = headers;
        self
    }

    /// Set the row header labels (builder style; PyDM `rowHeaderLabels`).
    pub fn with_row_headers(mut self, headers: Vec<String>) -> Self {
        self.row_headers = headers;
        self
    }

    /// Choose which severities draw a border (builder style; `DisconnectedOnly`
    /// for converted MEDM screens).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    fn column_header(&self, col: usize) -> String {
        match self.column_headers.get(col) {
            Some(h) if !h.is_empty() => h.clone(),
            _ => (col + 1).to_string(),
        }
    }

    fn row_header(&self, row: usize) -> String {
        match self.row_headers.get(row) {
            Some(h) if !h.is_empty() => h.clone(),
            _ => (row + 1).to_string(),
        }
    }

    /// Render the table this frame. Returns the whole array written this frame
    /// (on a successful cell commit), or `None`. There is no local echo: the
    /// edited cell re-syncs from the channel's next update.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let display = cell_strings(state.value.as_ref());
        let len = display.len();

        // Resync the edit buffers to the live value, keeping the cell being
        // edited frozen. A length change (new array) drops any stale edit.
        if self.cells.len() != len {
            self.cells = display.clone();
            self.editing = self.editing.filter(|&i| i < len);
        } else {
            for (i, d) in display.iter().enumerate() {
                if self.editing != Some(i) {
                    self.cells[i].clone_from(d);
                }
            }
        }

        let cols = self.column_count.max(1);
        let rows = row_count(len, cols);
        let grid_id = egui::Id::new(("rsdm_waveform_table", self.base.channel().address().raw()));
        let writable = state.connected;
        // Precompute the headers so the render closure borrows only the disjoint
        // `cells`/`editing` fields, not all of `self` (which `framed` borrows).
        let col_headers: Vec<String> = (0..cols).map(|c| self.column_header(c)).collect();
        let row_headers: Vec<String> = (0..rows).map(|r| self.row_header(r)).collect();

        let inner = self.base.framed(ui, &state, true, |ui| {
            let mut submitted: Option<PvValue> = None;
            egui::Grid::new(grid_id).striped(true).show(ui, |ui| {
                // Header row: a blank corner over the row-header column, then the
                // column labels.
                ui.label("");
                for header in &col_headers {
                    ui.label(header);
                }
                ui.end_row();

                for (r, row_header) in row_headers.iter().enumerate() {
                    ui.label(row_header);
                    for c in 0..cols {
                        let idx = cell_index(r, c, cols);
                        if idx >= len {
                            // Pad the trailing cells of the last short row.
                            ui.label("");
                            continue;
                        }
                        let resp = ui.add_enabled(
                            writable,
                            egui::TextEdit::singleline(&mut self.cells[idx]),
                        );
                        if resp.has_focus() {
                            self.editing = Some(idx);
                        }
                        if resp.lost_focus() {
                            let committed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if committed
                                && let Some(arr) = state.value.as_ref()
                                && let Some(value) = apply_cell_edit(arr, idx, &self.cells[idx])
                            {
                                submitted = Some(value);
                            }
                            // Drop the edit and resync this cell to the live value.
                            if idx < display.len() {
                                self.cells[idx].clone_from(&display[idx]);
                            }
                            if self.editing == Some(idx) {
                                self.editing = None;
                            }
                        }
                    }
                    ui.end_row();
                }
            });
            submitted
        });

        let submitted = inner.inner;
        if let Some(value) = &submitted {
            self.base.channel().put(value.clone());
        }
        submitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn row_count_is_ceiling_division() {
        assert_eq!(row_count(0, 1), 0);
        assert_eq!(row_count(5, 1), 5);
        assert_eq!(row_count(5, 2), 3); // 2 full rows + 1 partial
        assert_eq!(row_count(4, 2), 2);
        assert_eq!(row_count(1, 3), 1);
        assert_eq!(row_count(6, 3), 2);
        // A zero column count yields no rows (the widget renders it as 1 column).
        assert_eq!(row_count(5, 0), 0);
    }

    #[test]
    fn cell_index_is_row_major() {
        assert_eq!(cell_index(0, 0, 3), 0);
        assert_eq!(cell_index(0, 2, 3), 2);
        assert_eq!(cell_index(1, 0, 3), 3);
        assert_eq!(cell_index(2, 1, 3), 7);
    }

    #[test]
    fn apply_cell_edit_replaces_one_float_element() {
        let arr = PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice()));
        assert_eq!(
            apply_cell_edit(&arr, 1, "9.5"),
            Some(PvValue::FloatArray(Arc::from([1.0, 9.5, 3.0].as_slice())))
        );
    }

    #[test]
    fn apply_cell_edit_replaces_one_int_element() {
        let arr = PvValue::IntArray(Arc::from([1, 2, 3].as_slice()));
        assert_eq!(
            apply_cell_edit(&arr, 2, "  7 "),
            Some(PvValue::IntArray(Arc::from([1, 2, 7].as_slice())))
        );
    }

    #[test]
    fn apply_cell_edit_rejects_bad_input_and_bounds() {
        let farr = PvValue::FloatArray(Arc::from([1.0, 2.0].as_slice()));
        // Non-numeric text → no write.
        assert_eq!(apply_cell_edit(&farr, 0, "abc"), None);
        // Index past the end → no write.
        assert_eq!(apply_cell_edit(&farr, 5, "1.0"), None);
        // An int array rejects a fractional entry.
        let iarr = PvValue::IntArray(Arc::from([1, 2].as_slice()));
        assert_eq!(apply_cell_edit(&iarr, 0, "1.5"), None);
        // A scalar (non-array) value is not editable here.
        assert_eq!(apply_cell_edit(&PvValue::Float(1.0), 0, "2.0"), None);
    }

    #[test]
    fn cell_strings_renders_numeric_arrays_only() {
        assert_eq!(
            cell_strings(Some(&PvValue::IntArray(Arc::from([1, 20, 3].as_slice())))),
            vec!["1", "20", "3"]
        );
        // A non-array value produces no cells.
        assert!(cell_strings(Some(&PvValue::Float(1.0))).is_empty());
        assert!(cell_strings(None).is_empty());
    }

    #[test]
    fn header_fallbacks_are_one_based_indices() {
        let engine = crate::Engine::new();
        let table = RsdmWaveformTable::new(&engine, "loc://wf_headers")
            .expect("connect")
            .with_column_headers(vec!["A".to_owned()])
            .with_row_headers(vec![]);
        // Configured column 0, numeric fallback for column 1.
        assert_eq!(table.column_header(0), "A");
        assert_eq!(table.column_header(1), "2");
        // No row headers → 1-based row numbers.
        assert_eq!(table.row_header(0), "1");
        assert_eq!(table.row_header(3), "4");
    }
}
