//! The FloraStack desktop app: a thin egui layer that turns user actions into `florastack-engine` commands and shows
//! project state and job progress. It contains no processing logic, and it may depend on no FloraStack crate other than
//! `florastack-engine`, which re-exports everything it needs.
//!
//! The binary (`main.rs`) only starts it; the app is a library so that its tests drive the whole UI headless
//! (`tests/smoke.rs`, with `egui_kittest`). [`FloraStackApp`] holds the state, `panels` draws it, [`shell::Shell`]
//! talks to the engine, and [`viewer::Viewer`] draws the image on the GPU.

pub mod actions;
pub mod app;
pub mod files;
pub mod i18n;
pub mod loader;
pub mod palette;
pub mod panels;
pub mod prefs;
pub mod retouch_tab;
pub mod shell;
pub mod stack_tab;
pub mod viewer;

pub use app::{FloraStackApp, Options};
use florastack_engine::branding;

/// The environment variable that asks for native Wayland instead of XWayland on Linux.
pub const WAYLAND_VARIABLE: &str = "FLORASTACK_WAYLAND";

/// The window's settings: its title and size, file drops, and on Linux X11 (XWayland under Wayland) unless
/// [`WAYLAND_VARIABLE`] is set, since winit has no file drag-and-drop on Wayland.
pub fn native_options() -> eframe::NativeOptions {
    let viewport = egui::ViewportBuilder::default()
        .with_title(branding::DISPLAY_NAME)
        .with_app_id(branding::APP_ID)
        .with_inner_size([1200.0, 720.0])
        .with_min_inner_size([800.0, 500.0])
        // Maximized the first time; then as the user left it, which eframe remembers.
        .with_maximized(true)
        .with_drag_and_drop(true);
    #[allow(unused_mut, reason = "changed on Linux only")]
    let mut options = eframe::NativeOptions { viewport, persist_window: true, centered: true, ..eframe::NativeOptions::default() };
    #[cfg(all(unix, not(target_os = "macos")))]
    if files::use_x11(std::env::var_os("DISPLAY").as_deref(), std::env::var_os(WAYLAND_VARIABLE).as_deref()) {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }));
    }
    options
}

/// Opens the window and runs the app until it is closed.
pub fn run(options: Options) -> eframe::Result {
    eframe::run_native(branding::DISPLAY_NAME, native_options(), Box::new(|cc| Ok(Box::new(FloraStackApp::new(cc, options)))))
}
