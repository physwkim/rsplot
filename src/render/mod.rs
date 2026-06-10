//! The wgpu renderer.
//!
//! All persistent GPU resources (pipelines/buffers/textures/LUTs) live in
//! `egui_wgpu`'s `callback_resources` — a type map that persists across frames.
//! Each frame the egui side only re-registers lightweight callbacks; the heavy
//! state is looked up here (`doc/design.md` §3).

pub mod backend_wgpu;
pub mod gpu_curve;
pub mod gpu_image;
pub mod jpeg;
pub mod save;

// Headless WGSL validation of the shaders in `shaders/` (naga parse + validate,
// no GPU). Test-only; see `shaders.rs`.
#[cfg(test)]
mod shaders;
