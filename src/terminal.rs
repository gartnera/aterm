use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyLoop, Msg, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::Term;
use crossbeam_channel::{Receiver, Sender, unbounded};

#[derive(Clone)]
pub struct ChannelListener(Sender<TermEvent>);

impl EventListener for ChannelListener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<ChannelListener>>>,
    notifier: Notifier,
    events: Receiver<TermEvent>,
    title: String,
    cols: u16,
    lines: u16,
}

impl TerminalSession {
    pub fn spawn(cols: u16, lines: u16) -> std::io::Result<Self> {
        // Approximate cell size; the renderer will refine this when it knows
        // the real per-cell pixel size and call `resize`.
        let cell_width = 8;
        let cell_height = 16;
        let window_size = WindowSize {
            num_lines: lines,
            num_cols: cols,
            cell_width,
            cell_height,
        };
        let term_size = TermSize::new(cols as usize, lines as usize);

        let mut pty_options = PtyOptions::default();
        // On macOS, alacritty defaults to `/usr/bin/login`, which the harness
        // sandbox blocks. Explicitly use $SHELL (with a /bin/zsh fallback) so
        // we spawn the shell directly without `login`.
        let shell_path =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        pty_options.shell = Some(Shell::new(shell_path, Vec::new()));
        // window_id is opaque metadata used by alacritty's PTY layer.
        let pty = tty::new(&pty_options, window_size, 0)?;

        let (events_tx, events_rx) = unbounded();
        let listener = ChannelListener(events_tx);

        let term_config = TermConfig::default();
        let term = Term::new(term_config, &term_size, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_loop = PtyLoop::new(term.clone(), listener, pty, pty_options.drain_on_exit, false)?;
        let notifier = Notifier(pty_loop.channel());
        let _io_thread = pty_loop.spawn();

        Ok(Self {
            term,
            notifier,
            events: events_rx,
            title: "shell".into(),
            cols,
            lines,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn term(&self) -> &Arc<FairMutex<Term<ChannelListener>>> {
        &self.term
    }

    pub fn send_input<B: Into<std::borrow::Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, cols: u16, lines: u16, cell_width: u16, cell_height: u16) {
        self.cols = cols;
        self.lines = lines;
        let window_size = WindowSize {
            num_lines: lines,
            num_cols: cols,
            cell_width,
            cell_height,
        };
        let size = TermSize::new(cols as usize, lines as usize);
        let _ = self.notifier.0.send(Msg::Resize(window_size));
        self.term.lock().resize(size);
    }

    /// Drain any pending alacritty events. Returns whether a redraw is needed.
    pub fn pump_events(&mut self) -> bool {
        let mut wake = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::Title(t) => self.title = t,
                TermEvent::ResetTitle => self.title = "shell".into(),
                TermEvent::Wakeup => wake = true,
                TermEvent::Exit => wake = true,
                _ => {}
            }
        }
        wake
    }
}

