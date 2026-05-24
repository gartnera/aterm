use winit::event_loop::EventLoop;

/// Sent by the alacritty PTY thread to wake the winit event loop when the
/// terminal has produced new content.
#[derive(Debug, Clone, Copy)]
pub struct WakeEvent;

mod app;
mod binding;
mod config;
#[cfg(unix)]
mod debug_ipc;
mod gfx;
mod input;
mod quad;
mod terminal;

use app::App;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = config::load();

    // Convenience for headless test runs: set ATABS_EXIT_AFTER_MS=5000 to make
    // the process exit cleanly after N ms.
    if let Ok(ms) = std::env::var("ATABS_EXIT_AFTER_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                log::info!("ATABS_EXIT_AFTER_MS elapsed; exiting");
                std::process::exit(0);
            });
        }
    }

    let event_loop = EventLoop::<WakeEvent>::with_user_event()
        .build()
        .expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, proxy);
    event_loop.run_app(&mut app).expect("run app");
}
