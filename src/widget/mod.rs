//! egui widgets.
//!
//! [`plot_widget::PlotView`] hosts a plot inside an egui `Ui`: it draws the
//! chrome, handles interaction, and registers the wgpu paint callback for the
//! data area. Chrome drawing and interaction land in later milestones
//! (`doc/design.md` §8).

pub mod actions;
pub mod alpha_slider;
pub mod chrome;
pub mod colorbar;
pub mod colormap_dialog;
pub mod complex_image_view;
pub mod curves_roi_widget;
pub mod detached;
pub mod fit_widget;
pub mod high_level;
pub mod histogram_colorbar;
pub mod image_stack;
pub mod interaction;
pub mod items_selection_dialog;
pub mod limits_widget;
pub mod mask_tools;
pub mod plot_widget;
pub mod position_info;
pub mod profile_window;
pub mod radar_view;
pub mod roi_manager;
pub mod roi_stats;
pub mod roi_stats_widget;
pub mod scatter_mask;
pub mod stats_widget;
pub mod sync;
pub mod tool_buttons;
