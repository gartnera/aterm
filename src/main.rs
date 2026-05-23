use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

mod config;
mod gfx;
mod terminal;

use config::Config;
use gfx::Gfx;
use terminal::TerminalSession;

const TAB_BAR_HEIGHT: f32 = 28.0;

struct App {
    config: Config,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    tabs: Vec<TerminalSession>,
    active_tab: usize,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            config,
            window: None,
            gfx: None,
            tabs: Vec::new(),
            active_tab: 0,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("alacritty-tabs")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let line_height = (self.config.font_size * 1.25).round();
        let gfx = pollster::block_on(Gfx::new(
            window.clone(),
            self.config.font_size,
            line_height,
        ));
        self.window = Some(window.clone());
        self.gfx = Some(gfx);

        // Spawn an initial terminal tab.
        let session = TerminalSession::spawn(80, 24).expect("spawn terminal");
        self.tabs.push(session);
        self.active_tab = 0;
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else { return };
        let Some(gfx) = self.gfx.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gfx.resize(size);
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let active_tab = self.active_tab;
                let term = self.tabs.get(active_tab);
                if let Err(e) = gfx.render(term, &self.tabs, active_tab, TAB_BAR_HEIGHT) {
                    log::error!("render error: {e}");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // For now: request redraws each frame so we keep rendering. We will
        // switch to event-driven repaints once the terminal wakes us up.
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = config::load();
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let mut app = App::new(config);
    event_loop.run_app(&mut app).expect("run app");
}
