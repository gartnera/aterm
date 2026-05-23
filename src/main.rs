use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// Sent by the alacritty PTY thread to wake the winit event loop when the
/// terminal has produced new content.
#[derive(Debug, Clone, Copy)]
pub struct WakeEvent;

mod input;

mod config;
mod gfx;
mod quad;
mod terminal;

use config::Config;
use gfx::Gfx;
use terminal::TerminalSession;

const TAB_BAR_HEIGHT: f32 = 28.0;

struct App {
    config: Config,
    proxy: EventLoopProxy<WakeEvent>,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    tabs: Vec<TerminalSession>,
    active_tab: usize,
    mods: ModifiersState,
    cursor_pos: (f64, f64),
}

impl App {
    fn new(config: Config, proxy: EventLoopProxy<WakeEvent>) -> Self {
        Self {
            config,
            proxy,
            window: None,
            gfx: None,
            tabs: Vec::new(),
            active_tab: 0,
            mods: ModifiersState::empty(),
            cursor_pos: (0.0, 0.0),
        }
    }

    fn spawn_tab(&mut self) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        match TerminalSession::spawn(
            cols,
            lines,
            self.proxy.clone(),
            self.config.colors.clone(),
        ) {
            Ok(s) => {
                self.tabs.push(s);
                self.active_tab = self.tabs.len() - 1;
            }
            Err(e) => log::error!("spawn tab: {e}"),
        }
    }

    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        if self.tabs.is_empty() {
            return;
        }
        let idx = self.active_tab.min(self.tabs.len() - 1);
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
        }
    }
}

impl ApplicationHandler<WakeEvent> for App {
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
            self.config.colors.background,
        ));
        self.window = Some(window.clone());
        self.gfx = Some(gfx);

        // Spawn an initial terminal tab sized to the current window.
        let (cols, lines) = gfx_ref(&self.gfx).grid_for_window(TAB_BAR_HEIGHT);
        let session = TerminalSession::spawn(
            cols,
            lines,
            self.proxy.clone(),
            self.config.colors.clone(),
        )
        .expect("spawn terminal");
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
        let Some(window) = self.window.clone() else { return };
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
                // Cmd-prefixed shortcuts are app-level; consume them here.
                if self.mods.super_key() {
                    if self.handle_app_shortcut(&logical_key, event_loop) {
                        return;
                    }
                    return;
                }
                let Some(session) = self.tabs.get(self.active_tab) else { return };
                let term_mode = input::TermKeyMode {
                    app_cursor: session.app_cursor_mode(),
                };
                if let Some(bytes) =
                    input::encode_key(&logical_key, text.as_deref(), self.mods, term_mode)
                {
                    session.send_input(bytes);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let scale = window.scale_factor();
                let y_px = self.cursor_pos.1;
                let tab_bar_h_px = TAB_BAR_HEIGHT as f64 * scale;
                let x_px = self.cursor_pos.0;
                let hit = if y_px <= tab_bar_h_px {
                    gfx.tab_at_x(x_px as f32)
                } else {
                    None
                };
                if let Some(idx) = hit {
                    self.select_tab(idx);
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: WakeEvent) {
        // The PTY thread woke us; drain its events and ask for a redraw if
        // any tab actually produced new content.
        let mut wake = false;
        for tab in &mut self.tabs {
            if tab.pump_events() {
                wake = true;
            }
        }

        // Reap tabs whose shell has exited. If that empties the tab list,
        // close the app.
        let pre_len = self.tabs.len();
        let active_was_exited = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.is_exited())
            .unwrap_or(false);
        self.tabs.retain(|t| !t.is_exited());
        if self.tabs.len() != pre_len {
            wake = true;
        }
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if active_was_exited || self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
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

impl App {
    /// Returns true if the shortcut was handled (caller should swallow it).
    fn handle_app_shortcut(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        match key {
            Key::Character(s) => match s.as_str() {
                "t" | "T" => {
                    self.spawn_tab();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    true
                }
                "w" | "W" => {
                    self.close_active_tab(event_loop);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    true
                }
                "1" => { self.select_tab(0); true }
                "2" => { self.select_tab(1); true }
                "3" => { self.select_tab(2); true }
                "4" => { self.select_tab(3); true }
                "5" => { self.select_tab(4); true }
                "6" => { self.select_tab(5); true }
                "7" => { self.select_tab(6); true }
                "8" => { self.select_tab(7); true }
                "9" => { self.select_tab(8); true }
                _ => false,
            },
            Key::Named(NamedKey::ArrowLeft) => {
                if self.active_tab > 0 {
                    self.active_tab -= 1;
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.active_tab + 1 < self.tabs.len() {
                    self.active_tab += 1;
                }
                true
            }
            _ => false,
        }
    }
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

    let event_loop = EventLoop::<WakeEvent>::with_user_event()
        .build()
        .expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, proxy);
    event_loop.run_app(&mut app).expect("run app");
}
