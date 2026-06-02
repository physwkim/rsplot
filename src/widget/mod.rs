//! egui widgets.
//!
//! [`plot_widget::PlotView`] hosts a plot inside an egui `Ui`: it draws the
//! chrome, handles interaction, and registers the wgpu paint callback for the
//! data area. Chrome drawing and interaction land in later milestones
//! (`doc/design.md` §8).

pub mod chrome;
pub mod colormap_dialog;
pub mod complex_image_view;
pub mod fit_widget;
pub mod high_level;
pub mod interaction;
pub mod limits_widget;
pub mod mask_tools;
pub mod plot_widget;
pub mod profile_window;
pub mod roi_manager;
pub mod roi_stats;
pub mod sync;
