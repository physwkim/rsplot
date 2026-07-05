//! Recursive related-display conversion: convert a root `.adl` *and* the
//! transitive closure of its related-display targets into one Rust source
//! file.
//!
//! MEDM opens a related display at click time, building the child's macro
//! table from the entry's `args` (medmRelatedDisplay.c
//! `relatedDisplayCreateNewDisplay`). The same file is typically referenced
//! many times with different args (a camera screen opening one plugin screen
//! per channel), so baking a module per *(file, args)* pair would explode
//! combinatorially; like MEDM, the unit of conversion is the **file** — one
//! `pub mod __rd_*` per distinct target `.adl`, with macro values applied at
//! runtime through the generated `MacroTable` — and the root `Screen` sits at
//! the file's top level next to the shared runtime ([`codegen`]'s
//! `RsdmDisplay` / `OpenDisplay` / `parse_macro_args` / `next_plot_ids`).
//!
//! Target names are resolved like MEDM's `dmOpenUsableFile`: against the
//! referencing display's own directory, then each `EPICS_DISPLAY_PATH` entry.
//! A target that cannot be resolved (missing file, name still carrying a
//! `$(macro)`) keeps the report-only button the single-file converter emits,
//! plus a warning — never a silent drop.
//!
//! [`codegen`]: crate::codegen

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::adl_parser::{MedmScreen, parse_in_dir};
use crate::codegen::{Options, PLOT_IDS_HELPER, RdModule, generate, has_macro_ref};

/// The result of a recursive conversion: one source file (root screen, child
/// modules, shared runtime) plus the aggregated warnings (child-screen
/// warnings are prefixed with their file name).
#[derive(Clone, Debug)]
pub struct ConvertedFile {
    pub source: String,
    pub warnings: Vec<String>,
}

/// One display file in the conversion registry.
struct Entry {
    /// `pub mod` ident holding its `Screen` (`None` = the root screen).
    ident: Option<String>,
    /// Parsed screen (parsed once; generated twice — discovery, then final).
    screen: MedmScreen,
    /// The directory the file lives in (its own targets resolve against it).
    dir: PathBuf,
    /// The file name — the child window's title, and the warning prefix.
    title: String,
    /// Discovered related-display targets: the name as the emitter sees it →
    /// registry index (`None` = unresolved, stays a report-only button).
    targets: Vec<(String, Option<usize>)>,
}

/// Convert `input` and every related display reachable from it into one Rust
/// source file. `options` carries the CLI flags; its `macros` bake into the
/// root only (children take their macro tables at runtime from the opening
/// entry's `args`, MEDM `relatedDisplayCreateNewDisplay`), and its
/// `source_dir`/`rd_modules`/`child_module` are set per file by this driver.
pub fn convert_file(input: &Path, options: &Options) -> Result<ConvertedFile, String> {
    let mut warnings = Vec::new();
    let display_path = epics_display_path();

    let root_canon = input
        .canonicalize()
        .map_err(|e| format!("resolving {}: {e}", input.display()))?;
    let mut entries = vec![registry_entry(&root_canon, None)?];
    let mut by_canon = BTreeMap::from([(root_canon, 0usize)]);
    let mut taken_idents = BTreeSet::new();

    // Discovery: breadth-first over related-display targets. Each screen is
    // generated once with an empty module map purely to surface its target
    // names — the emitter is the single owner of macro baking and composite
    // inlining, so discovery reuses it instead of re-walking the IR.
    let mut i = 0;
    while i < entries.len() {
        let dir = entries[i].dir.clone();
        let title = entries[i].title.clone();
        let opts = entry_options(options, &dir, i == 0, BTreeMap::new());
        let found = generate(&entries[i].screen, &opts).related_targets;
        let mut targets = Vec::new();
        for name in found {
            if has_macro_ref(&name) {
                warnings.push(format!(
                    "{title}: related display target {name} still carries a macro at \
                     convert time; left as a report-only button"
                ));
                targets.push((name, None));
                continue;
            }
            let Some(canon) = resolve_target(&name, &dir, &display_path) else {
                warnings.push(format!(
                    "{title}: related display target {name} not found (searched the \
                     display's directory and EPICS_DISPLAY_PATH); left as a \
                     report-only button"
                ));
                targets.push((name, None));
                continue;
            };
            let idx = match by_canon.get(&canon) {
                Some(&idx) => idx,
                None => {
                    let ident = module_ident(&canon, &taken_idents);
                    taken_idents.insert(ident.clone());
                    let idx = entries.len();
                    entries.push(registry_entry(&canon, Some(ident))?);
                    by_canon.insert(canon, idx);
                    idx
                }
            };
            targets.push((name, Some(idx)));
        }
        entries[i].targets = targets;
        i += 1;
    }

    // Final pass: regenerate each screen with its resolved target → module
    // map, so a related-display click opens the sibling module's screen.
    let mut sources = Vec::with_capacity(entries.len());
    let mut uses_plot_ids = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let rd_modules: BTreeMap<String, RdModule> = e
            .targets
            .iter()
            .filter_map(|(name, idx)| {
                let target = &entries[(*idx)?];
                let (width, height) = native_size(&target.screen);
                Some((
                    name.clone(),
                    RdModule {
                        ident: target.ident.clone(),
                        title: target.title.clone(),
                        width,
                        height,
                    },
                ))
            })
            .collect();
        let opts = entry_options(options, &e.dir, i == 0, rd_modules);
        let g = generate(&e.screen, &opts);
        warnings.extend(g.warnings.into_iter().map(|w| {
            if i == 0 {
                w
            } else {
                format!("{}: {w}", e.title)
            }
        }));
        uses_plot_ids.push(g.uses_plot_ids);
        sources.push(g.source);
    }

    // Assemble: root at the top level, one `pub mod` per child file, each
    // child implementing the shared `RsdmDisplay` trait (the root too when a
    // child cycles back to it).
    let mut sources = sources.into_iter();
    let mut out = sources.next().expect("registry holds at least the root");
    let root_is_target = entries
        .iter()
        .any(|e| e.targets.iter().any(|(_, idx)| *idx == Some(0)));
    if root_is_target {
        out.push_str(&display_impl(""));
    }
    for (e, source) in entries.iter().skip(1).zip(sources) {
        let ident = e.ident.as_deref().expect("child entries carry an ident");
        out.push('\n');
        writeln(
            &mut out,
            &format!(
                "/// Related-display target `{}`, converted alongside the root screen.",
                e.title
            ),
        );
        writeln(&mut out, &format!("pub mod {ident} {{"));
        for line in source.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&indent_block(&display_impl("super::"), "    "));
        writeln(&mut out, "}");
    }
    // Plots in child modules draw their `PlotId`s from the shared top-level
    // allocator; the root emits it with its own source only when it has plots
    // itself, so append it here when only children need it.
    if !uses_plot_ids[0] && uses_plot_ids.iter().skip(1).any(|&u| u) {
        out.push_str(PLOT_IDS_HELPER);
    }

    Ok(ConvertedFile {
        source: out,
        warnings,
    })
}

/// `writeln!` onto a `String` without the unused-`Result` dance at call sites.
fn writeln(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// The `impl RsdmDisplay for Screen` delegation a hosted screen needs;
/// `trait_prefix` is `"super::"` inside a child module.
fn display_impl(trait_prefix: &str) -> String {
    format!(
        "\nimpl {trait_prefix}RsdmDisplay for Screen {{\n    fn ui(&mut self, ui: &mut egui::Ui) {{\n        Screen::ui(self, ui)\n    }}\n}}\n"
    )
}

/// Indent every non-empty line of `block` by `pad`.
fn indent_block(block: &str, pad: &str) -> String {
    let mut out = String::new();
    for line in block.lines() {
        if !line.is_empty() {
            out.push_str(pad);
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Read and parse one display file into a registry entry, falling back to the
/// file name when the `.adl` carries no `file { name="…" }`.
fn registry_entry(canon: &Path, ident: Option<String>) -> Result<Entry, String> {
    let text =
        std::fs::read_to_string(canon).map_err(|e| format!("reading {}: {e}", canon.display()))?;
    let mut screen = parse_in_dir(&text, canon.parent());
    let title = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("display")
        .to_string();
    if screen.adl_filename.is_empty() {
        screen.adl_filename = title.clone();
    }
    Ok(Entry {
        ident,
        screen,
        dir: canon
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        title,
        targets: Vec::new(),
    })
}

/// The per-file [`Options`]: the CLI flags with this file's directory, the
/// child-module flag, and its resolved target map. Convert-time `--macro`
/// baking applies to the root only — a child's macro values come from the
/// *opening entry's* `args` at runtime (MEDM passes no implicit parent table).
fn entry_options(
    base: &Options,
    dir: &Path,
    is_root: bool,
    rd_modules: BTreeMap<String, RdModule>,
) -> Options {
    Options {
        protocol: base.protocol.clone(),
        macros: if is_root {
            base.macros.clone()
        } else {
            Vec::new()
        },
        use_scatterplot: base.use_scatterplot,
        source_dir: Some(dir.to_path_buf()),
        use_layout: base.use_layout,
        rd_modules,
        child_module: !is_root,
    }
}

/// The native display size a child viewport opens at: the `display` block's
/// geometry, with a plain fallback for a malformed header.
fn native_size(screen: &MedmScreen) -> (f64, f64) {
    match screen.geometry {
        Some(g) if g.width > 0 && g.height > 0 => (f64::from(g.width), f64::from(g.height)),
        _ => (400.0, 300.0),
    }
}

/// Resolve a related-display target name the way MEDM's `dmOpenUsableFile`
/// (medmCommon.c) does: an absolute name as-is, a relative one against the
/// referencing display's directory then each `EPICS_DISPLAY_PATH` entry, and —
/// when a name with a directory part resolves nowhere — its bare file name
/// through the same search.
fn resolve_target(name: &str, dir: &Path, display_path: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut candidates = Vec::new();
    if p.is_absolute() {
        candidates.push(p.to_path_buf());
    } else {
        candidates.push(dir.join(p));
        candidates.extend(display_path.iter().map(|d| d.join(p)));
    }
    if p.components().count() > 1
        && let Some(base) = p.file_name()
    {
        candidates.push(dir.join(base));
        candidates.extend(display_path.iter().map(|d| d.join(base)));
    }
    candidates
        .into_iter()
        .find_map(|c| c.canonicalize().ok().filter(|c| c.is_file()))
}

/// The `EPICS_DISPLAY_PATH` search directories (platform path-separator
/// splitting, like MEDM).
fn epics_display_path() -> Vec<PathBuf> {
    std::env::var_os("EPICS_DISPLAY_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default()
}

/// A collision-free `pub mod` ident for a target file: `__rd_` plus the
/// lower-cased file stem with every non-alphanumeric mapped to `_`.
fn module_ident(path: &Path, taken: &BTreeSet<String>) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("display");
    let mut base = String::from("__rd_");
    base.extend(stem.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        }
    }));
    let mut ident = base.clone();
    let mut n = 2;
    while taken.contains(&ident) {
        ident = format!("{base}_{n}");
        n += 1;
    }
    ident
}
