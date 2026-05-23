use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

mod input;

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
    mods: ModifiersState,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            config,
            window: None,
            gfx: None,
            tabs: Vec::new(),
            active_tab: 0,
            mods: ModifiersState::empty(),
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
            self.config.font_family.clone(),
        ));
        self.window = Some(window.clone());
        self.gfx = Some(gfx);

        // Spawn an initial terminal tab sized to the current window.
        let (cols, lines) = gfx_ref(&self.gfx).grid_for_window(TAB_BAR_HEIGHT);
        let session = TerminalSession::spawn(cols, lines).expect("spawn terminal");
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
                let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                let scale = window.scale_factor() as f32;
                let (cw, ch) = gfx.cell_dims_logical();
                let cell_w_px = (cw * scale).round().max(1.0) as u16;
                let cell_h_px = (ch * scale).round().max(1.0) as u16;
                for tab in &mut self.tabs {
                    tab.resize(cols, lines, cell_w_px, cell_h_px);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let active_tab = self.active_tab;
                let term = self.tabs.get(active_tab);
                if let Err(e) = gfx.render(term, &self.tabs, active_tab, TAB_BAR_HEIGHT) {
                    log::error!("render error: {e}");
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        repeat: _,
                        ..
                    },
                ..
            } => {
                // Cmd-prefixed shortcuts are app-level (tabs, copy, paste);
                // let those fall through without reaching the PTY.
                if self.mods.super_key() {
                    return;
                }
                let Some(session) = self.tabs.get(self.active_tab) else { return };
                if let Some(bytes) = input::encode_key(&logical_key, text.as_deref(), self.mods) {
                    session.send_input(bytes);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Drain terminal events; request a redraw if any tab produced output.
        let mut wake = false;
        for tab in &mut self.tabs {
            if tab.pump_events() {
                wake = true;
            }
        }
        if wake {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

fn gfx_ref(opt: &Option<Gfx>) -> &Gfx {
    opt.as_ref().expect("gfx initialized")
}

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

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let mut app = App::new(config);
    event_loop.run_app(&mut app).expect("run app");
}
