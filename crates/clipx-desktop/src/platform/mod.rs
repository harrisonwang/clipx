#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
fn center_window(window: &tao::window::Window) {
    let Some(monitor) = window.current_monitor() else {
        return;
    };
    let origin = monitor.position();
    let screen = monitor.size();
    let size = window.outer_size();
    let position = tao::dpi::PhysicalPosition::new(
        (origin.x as f64 + screen.width.saturating_sub(size.width) as f64 / 2.0) as i32,
        (origin.y as f64 + screen.height.saturating_sub(size.height) as f64 / 2.0) as i32,
    );
    if window.outer_position().ok() != Some(position) {
        window.set_outer_position(position);
    }
}
