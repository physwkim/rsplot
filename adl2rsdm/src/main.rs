//! Binary entry point for `adl2rsdm`.
//!
//! A thin wrapper over [`cli::main`]; the argument parsing and `.adl` → `.rs`
//! driver live in the binary-local [`cli`] module so the library crate stays
//! free of the `clap` dependency.

use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    cli::main()
}
