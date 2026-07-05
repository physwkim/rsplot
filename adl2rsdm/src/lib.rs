//! `adl2rsdm` — convert MEDM `.adl` screen files to RsDM (Rust) display modules.
//!
//! This crate mirrors the structure of [`adl2pydm`] (which converts MEDM `.adl`
//! screens to PyDM `.ui` files) but targets **RsDM**: it parses an `.adl` file
//! into an in-memory widget tree and emits **Rust source** that constructs the
//! equivalent [`rsdm`] widgets at their MEDM geometry.
//!
//! RsDM has no runtime display loader — RsDM screens are programmatic Rust
//! structs — so the faithful analogue of "PyDM loads a generated `.ui`" is "the
//! generated Rust is compiled into a RsDM app". A side benefit is that the
//! generated screen can be *compile-verified* against the real `rsdm` widget
//! APIs, a fidelity check `adl2pydm` cannot perform against Qt.
//!
//! The pipeline mirrors `adl2pydm`'s three stages:
//!
//! * `adl_parser` — block-structured `.adl` parser producing a widget-tree IR
//!   (port of `adl2pydm/adl_parser.py`). Pure and headlessly testable.
//! * `symbols` — the MEDM-widget → RsDM-widget map plus each widget's draw
//!   category (port of `adl2pydm/symbols.py`).
//! * `codegen` — walks the IR and emits Rust source, one emitter per MEDM
//!   widget type (the analogue of `adl2pydm/output_handler.py`).
//! * `convert` — the recursive driver: converts a root `.adl` plus the
//!   transitive closure of its related-display targets into one source file,
//!   so clicking a related-display button *opens* the converted child screen
//!   (MEDM `relatedDisplayCreateNewDisplay`).
//!
//! [`adl2pydm`]: https://github.com/BCDA-APS/adl2pydm
//! [`rsdm`]: https://docs.rs/rsdm

pub mod adl_parser;
pub mod codegen;
pub mod convert;
pub mod symbols;
