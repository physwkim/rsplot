//! System-printer enumeration and spooling via the CUPS / BSD command-line
//! tools (`lpstat`, `lp`), replacing the `printers` crate so a default build
//! links no CUPS library (`-lcups`) and needs no `libcups2-dev` at build time.
//!
//! Printing is best-effort: it depends on `lp` / `lpstat` being on `PATH`
//! (CUPS on Linux/macOS). Where they are absent — e.g. Windows, or a machine
//! with no print system — enumeration returns an empty list and there is no
//! default printer, which the callers already treat as "no destination"
//! (`print_graph` → `Ok(false)`, the dialog → "No printers found"). No process
//! is spawned until the user actually prints.

use std::path::Path;
use std::process::Command;

/// The available system printer destinations, from `lpstat -e` (one name per
/// line). Any failure — `lpstat` missing, no print system, non-zero exit —
/// yields an empty list; callers treat that as "no printers found".
pub(crate) fn system_printers() -> Vec<String> {
    let Ok(out) = Command::new("lpstat").arg("-e").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The system default destination, parsed from `lpstat -d`
/// ("system default destination: NAME"), or `None` when there is no default —
/// "no system default destination" — or `lpstat` is unavailable.
pub(crate) fn default_printer() -> Option<String> {
    let out = Command::new("lpstat").arg("-d").output().ok()?;
    if !out.status.success() {
        return None;
    }
    // Only the "…: NAME" form names a default; the no-default line has no colon.
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text.trim().split_once(':')?.1.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Spool an already-rendered file to `printer` via `lp -d <printer>`. Returns
/// `Ok(())` when the job is queued; `Err(message)` when `lp` is missing or the
/// submission fails (unknown printer, spooler down). The job is queued, not
/// waited on, mirroring the fire-and-forget submit the `printers` crate did.
pub(crate) fn spool_file(path: &Path, printer: &str) -> Result<(), String> {
    let out = Command::new("lp")
        .arg("-d")
        .arg(printer)
        .arg(path)
        .output()
        .map_err(|e| format!("lp not available: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}
