//! 3D scene foundation — the `silx.gui.plot3d` port.
//!
//! This is the pure, headless math layer (matrices + camera); the GPU pipeline
//! lives in [`crate::render`] and the interactive widget in [`crate::widget`].
//! Tracked in `doc/plot3d-parity-roadmap.md`.

pub mod camera;
pub mod mat4;
