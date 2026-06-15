//! Headless WGSL validation for the render shaders.
//!
//! wgpu compiles `shaders/*.wgsl` at runtime via `create_shader_module`, which
//! needs a GPU device. To gate shader correctness *without* a GPU, these tests
//! parse and validate every shader with `naga` (re-exported by `wgpu`, so no
//! extra dependency): [`naga::front::wgsl::parse_str`] then
//! [`naga::valid::Validator`]. A malformed shader — a syntax error, a type
//! mismatch, a bad `@group`/`@binding`, a broken struct layout — fails
//! `cargo nextest` headlessly.
//!
//! Scope: this proves the WGSL is *well-formed and statically valid*. It does
//! NOT prove draw correctness (pixels, blending, buffer binding offsets) — that
//! is GPU-only and unverifiable on a machine without a render device. New
//! `shaders/*.wgsl` files should get a one-line test here so a regression is
//! caught at the same gate.
//!
//! This module is compiled only under `#[cfg(test)]` (see `render/mod.rs`).

use egui_wgpu::wgpu::naga;

/// Parse + validate one WGSL shader, panicking with a span-annotated diagnostic
/// (naga's `emit_to_string`) if either step fails so the failing line is named.
fn validate_wgsl(name: &str, source: &str) {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|e| panic!("{name}: WGSL parse failed:\n{}", e.emit_to_string(source)));
    let mut validator = naga::valid::Validator::new(
        // Validate everything naga can check statically (expressions, blocks,
        // control-flow uniformity, struct layouts, constants, bindings).
        naga::valid::ValidationFlags::all(),
        // Most permissive capability set: accept anything any backend could
        // support, so a legal shader is never false-rejected.
        naga::valid::Capabilities::all(),
    );
    validator.validate(&module).unwrap_or_else(|e| {
        panic!(
            "{name}: WGSL validation failed:\n{}",
            e.emit_to_string(source)
        )
    });
}

#[test]
fn clear_wgsl_is_valid() {
    validate_wgsl("clear.wgsl", include_str!("shaders/clear.wgsl"));
}

#[test]
fn curve_wgsl_is_valid() {
    validate_wgsl("curve.wgsl", include_str!("shaders/curve.wgsl"));
}

#[test]
fn errorbars_wgsl_is_valid() {
    validate_wgsl("errorbars.wgsl", include_str!("shaders/errorbars.wgsl"));
}

#[test]
fn fill_wgsl_is_valid() {
    validate_wgsl("fill.wgsl", include_str!("shaders/fill.wgsl"));
}

#[test]
fn image_wgsl_is_valid() {
    validate_wgsl("image.wgsl", include_str!("shaders/image.wgsl"));
}

#[test]
fn image_rgba_wgsl_is_valid() {
    validate_wgsl("image_rgba.wgsl", include_str!("shaders/image_rgba.wgsl"));
}

#[test]
fn markers_wgsl_is_valid() {
    validate_wgsl("markers.wgsl", include_str!("shaders/markers.wgsl"));
}

#[test]
fn scene3d_wgsl_is_valid() {
    validate_wgsl("scene3d.wgsl", include_str!("shaders/scene3d.wgsl"));
}

#[test]
fn scene3d_blit_wgsl_is_valid() {
    validate_wgsl(
        "scene3d_blit.wgsl",
        include_str!("shaders/scene3d_blit.wgsl"),
    );
}
