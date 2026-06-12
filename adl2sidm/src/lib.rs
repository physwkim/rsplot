//! `adl2sidm` — convert MEDM `.adl` screen files to SiDM (Rust) display modules.
//!
//! This crate mirrors the structure of [`adl2pydm`] (which converts MEDM `.adl`
//! screens to PyDM `.ui` files) but targets **SiDM**: it parses an `.adl` file
//! into an in-memory widget tree and emits **Rust source** that constructs the
//! equivalent [`sidm`] widgets at their MEDM geometry.
//!
//! SiDM has no runtime display loader — SiDM screens are programmatic Rust
//! structs — so the faithful analogue of "PyDM loads a generated `.ui`" is "the
//! generated Rust is compiled into a SiDM app". A side benefit is that the
//! generated screen can be *compile-verified* against the real `sidm` widget
//! APIs, a fidelity check `adl2pydm` cannot perform against Qt.
//!
//! The pipeline mirrors `adl2pydm`'s three stages:
//!
//! * `adl_parser` — block-structured `.adl` parser producing a widget-tree IR
//!   (port of `adl2pydm/adl_parser.py`). Pure and headlessly testable.
//! * `symbols` — the MEDM-widget → SiDM-widget map plus each widget's draw
//!   category (port of `adl2pydm/symbols.py`).
//! * `codegen` — walks the IR and emits Rust source, one emitter per MEDM
//!   widget type (the analogue of `adl2pydm/output_handler.py`).
//! * `convert` — the recursive driver: converts a root `.adl` plus the
//!   transitive closure of its related-display targets into one source file,
//!   so clicking a related-display button *opens* the converted child screen
//!   (MEDM `relatedDisplayCreateNewDisplay`).
//!
//! [`adl2pydm`]: https://github.com/BCDA-APS/adl2pydm
//! [`sidm`]: https://docs.rs/sidm

pub mod adl_parser;
pub mod codegen;
pub mod convert;
pub mod symbols;
