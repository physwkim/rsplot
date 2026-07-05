//! Command-line driver for `adl2rsdm`: read a MEDM `.adl` screen, emit a RsDM
//! (Rust) display module — including every related display reachable from it,
//! converted as sibling modules in the same file (`convert::convert_file`).
//!
//! This module is local to the binary (`mod cli` in `main.rs`), so the library
//! crate stays free of the `clap` dependency. It is the analogue of
//! `adl2pydm`'s CLI: `.adl` in → `.rs` out, with `--protocol` / `--macro` /
//! `--out` / `--use-scatterplot` mirroring adl2pydm's options. The responsive
//! layout (adl2pydm `--use-layout`) is the default here; `--absolute` opts
//! back into fixed MEDM pixels.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use adl2rsdm::codegen::Options;
use adl2rsdm::convert::convert_file;
use clap::Parser;

/// Convert a MEDM `.adl` screen file to a RsDM (Rust) display module.
#[derive(Parser, Debug)]
#[command(name = "adl2rsdm", version, about, long_about = None)]
pub struct Cli {
    /// The MEDM `.adl` screen file to convert.
    pub input: PathBuf,

    /// Output `.rs` file (`-` for stdout). Defaults to the input path with its
    /// extension changed to `.rs`.
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// Channel protocol prefixed onto bare MEDM PV names.
    #[arg(short, long, default_value = "ca://")]
    pub protocol: String,

    /// `$(name)` / `${name}` macro substitution, repeatable (e.g. `-m P=DMM1:`).
    #[arg(short = 'm', long = "macro", value_name = "NAME=VALUE", value_parser = parse_macro)]
    pub macros: Vec<(String, String)>,

    /// Convert `cartesian plot` as a scatter plot rather than a waveform plot.
    #[arg(long)]
    pub use_scatterplot: bool,

    /// Emit a responsive layout that scales widgets to fill the window
    /// (adl2pydm `grid_layout` parity). This is the default; the flag is kept
    /// for compatibility.
    #[arg(long, conflicts_with = "absolute")]
    pub use_layout: bool,

    /// Place widgets at fixed absolute MEDM pixels instead of the default
    /// responsive layout.
    #[arg(long)]
    pub absolute: bool,
}

/// Parse a `NAME=VALUE` macro definition.
fn parse_macro(s: &str) -> Result<(String, String), String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("macro must be NAME=VALUE, got {s:?}"))?;
    if name.is_empty() {
        return Err(format!("macro name must be non-empty in {s:?}"));
    }
    Ok((name.to_string(), value.to_string()))
}

/// Binary entry point: parse arguments, run the conversion, report warnings.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(warnings) => {
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Read the input, convert it (and every related display reachable from it,
/// see [`convert_file`]), and write the result out. Returns the converter's
/// warnings (printed by `main`) on success.
fn run(cli: Cli) -> Result<Vec<String>, String> {
    let options = Options {
        protocol: cli.protocol,
        macros: cli.macros,
        use_scatterplot: cli.use_scatterplot,
        // Responsive layout is the default; `--absolute` opts out (and
        // `--use-layout`, the old opt-in, is accepted as a no-op — clap
        // rejects combining the two via `conflicts_with`).
        use_layout: cli.use_layout || !cli.absolute,
        // `source_dir` (embedded-display resolution), `rd_modules`, and
        // `child_module` are set per file by the recursive driver.
        ..Options::default()
    };
    let converted = convert_file(&cli.input, &options)?;

    match cli.out.as_deref() {
        Some(p) if p == Path::new("-") => print!("{}", converted.source),
        Some(p) => write_out(p, &converted.source)?,
        None => write_out(&cli.input.with_extension("rs"), &converted.source)?,
    }
    Ok(converted.warnings)
}

/// Write the generated source to `path`, reporting where it landed.
fn write_out(path: &Path, source: &str) -> Result<(), String> {
    std::fs::write(path, source).map_err(|e| format!("writing {}: {e}", path.display()))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn parse_macro_splits_name_and_value() {
        assert_eq!(
            parse_macro("P=DMM1:"),
            Ok(("P".to_string(), "DMM1:".to_string()))
        );
        // A value may itself contain '=' (only the first splits).
        assert_eq!(
            parse_macro("EXPR=A=B"),
            Ok(("EXPR".to_string(), "A=B".to_string()))
        );
        // An empty value is allowed (substitutes the macro away).
        assert_eq!(parse_macro("P="), Ok(("P".to_string(), String::new())));
    }

    #[test]
    fn parse_macro_rejects_malformed_input() {
        assert!(parse_macro("NOEQUALS").is_err());
        assert!(parse_macro("=value").is_err()); // empty name
    }

    #[test]
    fn cli_definition_is_valid() {
        // clap panics at startup on an inconsistent derive; assert it builds.
        Cli::command().debug_assert();
    }
}
