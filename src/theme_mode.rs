//! Dark/light color-scheme reporting: `CSI ? 996 n` queries and DEC private
//! mode 2031 unsolicited updates.
//!
//! This is contour's VT extension
//! (<https://contour-terminal.org/vt-extensions/color-palette-update-notifications/>),
//! also implemented by ghostty, kitty and foot. Neovim 0.11+ probes for it with
//! `DECRQM ?2031`, turns it on if the terminal says it knows the mode, and
//! re-reads the background (`OSC 11 ; ?`) whenever a notification arrives, so
//! `'background'` tracks the OS appearance live.
//!
//! - `CSI ? 996 n` → reply `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light).
//! - `CSI ? 2031 h` / `CSI ? 2031 l` → enable / disable sending that same
//!   report unprompted whenever the terminal's palette changes.
//! - `CSI ? 2031 $ p` (DECRQM) → `CSI ? 2031 ; 1|2 $ y` (set / reset).
//!
//! alacritty_terminal knows none of these: it drops the query and the
//! DECSET/DECRST, and answers the DECRQM with "not recognized", which makes
//! neovim give up on the feature. Its event loop owns the PTY reader and
//! feeds the parser directly, so [`FilteredPty`] wraps the PTY and picks these
//! sequences out of the byte stream before alacritty sees it.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollingEvent, PollMode, Poller};

/// The DEC private mode number for unsolicited color-scheme updates.
pub const MODE: u16 = 2031;
/// `CSI ? 996 n`: "which scheme are you using right now?"
const QUERY: u16 = 996;

const ESC: u8 = 0x1b;
/// Longest `CSI ? …` we'll hold back while waiting for its final byte. Every
/// sequence we answer fits comfortably; anything longer can't be one of ours
/// and is passed through untouched.
const MAX_PENDING: usize = 32;

/// The report for the given scheme, used both as the reply to `CSI ? 996 n`
/// and as the mode-2031 notification.
pub fn scheme_report(dark: bool) -> String {
    format!("\x1b[?997;{}n", if dark { 1 } else { 2 })
}

/// Per-tab state shared between the PTY reader thread (which sees the
/// program's requests) and the UI thread (which changes the palette).
pub struct ThemeState {
    /// Mode 2031: the program asked to be told about scheme changes.
    updates: AtomicBool,
    dark: AtomicBool,
}

impl ThemeState {
    pub fn new(dark: bool) -> Arc<Self> {
        Arc::new(Self {
            updates: AtomicBool::new(false),
            dark: AtomicBool::new(dark),
        })
    }

    pub fn updates_enabled(&self) -> bool {
        self.updates.load(Ordering::Relaxed)
    }

    pub fn set_dark(&self, dark: bool) {
        self.dark.store(dark, Ordering::Relaxed);
    }

    fn dark(&self) -> bool {
        self.dark.load(Ordering::Relaxed)
    }
}

/// What the program asked for, in stream order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// `CSI ? 2031 h` / `l`.
    SetUpdates(bool),
    /// `CSI ? 996 n`.
    QueryScheme,
    /// `CSI ? 2031 $ p`.
    QueryMode,
    /// RIS (`ESC c`) resets every mode, this one included.
    Reset,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC [`.
    Csi,
    /// Saw `ESC [ ?`, now in the parameters.
    Private,
    /// Saw `ESC [ ? <params> $`.
    PrivateDollar,
}

/// Streaming recognizer for the sequences above. Bytes that might be the
/// start of one are held in `pending` until the sequence completes (or turns
/// out to be something else), so a sequence split across two PTY reads is
/// still caught. Queries we answer are removed from the stream; DECSET /
/// DECRST pass through, since they may carry other modes for alacritty too.
#[derive(Default)]
struct Filter {
    state: State,
    pending: Vec<u8>,
}

impl Filter {
    fn advance(&mut self, input: &[u8], out: &mut Vec<u8>, actions: &mut Vec<Action>) {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Ground {
                // Fast path: copy everything up to the next ESC in one go.
                match input[i..].iter().position(|&b| b == ESC) {
                    None => {
                        out.extend_from_slice(&input[i..]);
                        return;
                    }
                    Some(p) => {
                        out.extend_from_slice(&input[i..i + p]);
                        i += p;
                    }
                }
            }
            self.step(input[i], out, actions);
            i += 1;
        }
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>, actions: &mut Vec<Action>) {
        match self.state {
            State::Ground => {
                if b == ESC {
                    self.pending.push(b);
                    self.state = State::Esc;
                } else {
                    out.push(b);
                }
            }
            State::Esc => match b {
                b'[' => {
                    self.pending.push(b);
                    self.state = State::Csi;
                }
                b'c' => {
                    actions.push(Action::Reset);
                    self.pending.push(b);
                    self.flush(out);
                }
                _ => self.reject(b, out, actions),
            },
            State::Csi => {
                if b == b'?' {
                    self.pending.push(b);
                    self.state = State::Private;
                } else {
                    self.reject(b, out, actions);
                }
            }
            State::Private => match b {
                b'0'..=b'9' | b';' if self.pending.len() < MAX_PENDING => self.pending.push(b),
                b'$' => {
                    self.pending.push(b);
                    self.state = State::PrivateDollar;
                }
                b'h' | b'l' | b'n' => self.finish(b, out, actions),
                _ => self.reject(b, out, actions),
            },
            State::PrivateDollar => {
                if b == b'p' {
                    self.finish(b, out, actions);
                } else {
                    self.reject(b, out, actions);
                }
            }
        }
    }

    /// `pending` turned out not to be one of ours: pass it through and
    /// reconsider `b` from the ground state (it may be a fresh `ESC`).
    fn reject(&mut self, b: u8, out: &mut Vec<u8>, actions: &mut Vec<Action>) {
        self.flush(out);
        self.step(b, out, actions);
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.pending);
        self.state = State::Ground;
    }

    /// A complete `ESC [ ? <params> [$] <final>`.
    fn finish(&mut self, final_byte: u8, out: &mut Vec<u8>, actions: &mut Vec<Action>) {
        // Skip the `ESC [ ?` prefix and any `$` intermediate.
        let params = self.pending[3..]
            .strip_suffix(b"$")
            .unwrap_or(&self.pending[3..]);
        let params: Vec<u32> = params
            .split(|&b| b == b';')
            .map(|p| {
                p.iter().fold(0u32, |n, &d| {
                    n.saturating_mul(10).saturating_add((d - b'0') as u32)
                })
            })
            .collect();
        let consumed = match final_byte {
            b'h' | b'l' => {
                if params.contains(&(MODE as u32)) {
                    actions.push(Action::SetUpdates(final_byte == b'h'));
                }
                false
            }
            b'n' if params == [QUERY as u32] => {
                actions.push(Action::QueryScheme);
                true
            }
            b'p' if params == [MODE as u32] => {
                actions.push(Action::QueryMode);
                true
            }
            _ => false,
        };
        if consumed {
            self.pending.clear();
            self.state = State::Ground;
        } else {
            self.pending.push(final_byte);
            self.flush(out);
        }
    }
}

/// The PTY alacritty's event loop reads from, with the color-scheme
/// sequences handled on the way through (see the module docs).
///
/// Replies go out as [`TermEvent::PtyWrite`] on the same channel alacritty
/// uses for its own query replies. They are queued when the bytes are read,
/// which is before alacritty parses that chunk. So a reply never arrives after
/// the reply to a query the program sent later, such as the DA1 that neovim
/// waits on as a sentinel.
pub struct FilteredPty<L> {
    inner: tty::Pty,
    filter: Filter,
    /// Filtered bytes not yet handed to the reader. Normally drained by the
    /// same `read` call that filled it.
    ready: Vec<u8>,
    actions: Vec<Action>,
    state: Arc<ThemeState>,
    listener: L,
}

impl<L: EventListener> FilteredPty<L> {
    pub fn new(inner: tty::Pty, state: Arc<ThemeState>, listener: L) -> Self {
        Self {
            inner,
            filter: Filter::default(),
            ready: Vec::new(),
            actions: Vec::new(),
            state,
            listener,
        }
    }

    fn apply_actions(&mut self) {
        for action in self.actions.drain(..) {
            match action {
                Action::SetUpdates(on) => self.state.updates.store(on, Ordering::Relaxed),
                Action::Reset => self.state.updates.store(false, Ordering::Relaxed),
                Action::QueryScheme => self
                    .listener
                    .send_event(TermEvent::PtyWrite(scheme_report(self.state.dark()))),
                Action::QueryMode => {
                    let state = if self.state.updates_enabled() { 1 } else { 2 };
                    self.listener
                        .send_event(TermEvent::PtyWrite(format!("\x1b[?{MODE};{state}$y")));
                }
            }
        }
    }
}

impl<L: EventListener> Read for FilteredPty<L> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.ready.is_empty() {
                let n = self.ready.len().min(buf.len());
                buf[..n].copy_from_slice(&self.ready[..n]);
                self.ready.drain(..n);
                return Ok(n);
            }
            // Leave room for held-back bytes that this read may release, so
            // the filtered output fits in `buf`. Otherwise bytes could be left
            // in `ready` with the fd idle, and alacritty only calls `read` again
            // once the fd is readable.
            let limit = buf.len().saturating_sub(self.filter.pending.len()).max(1);
            let n = self.inner.reader().read(&mut buf[..limit])?;
            if n == 0 {
                return Ok(0);
            }
            self.filter
                .advance(&buf[..n], &mut self.ready, &mut self.actions);
            self.apply_actions();
            // If the whole read was one of our queries there is nothing to
            // return yet. Read again: the next read either has more data or
            // returns WouldBlock, which alacritty expects.
        }
    }
}

impl<L: EventListener> EventedReadWrite for FilteredPty<L> {
    type Reader = Self;
    type Writer = <tty::Pty as EventedReadWrite>::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollingEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        unsafe { self.inner.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollingEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        self.inner.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl<L: EventListener> EventedPty for FilteredPty<L> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl<L> OnResize for FilteredPty<L> {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.inner.on_resize(window_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `chunks` through one filter, as consecutive PTY reads.
    fn run(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Action>) {
        let mut filter = Filter::default();
        let (mut out, mut actions) = (Vec::new(), Vec::new());
        for chunk in chunks {
            filter.advance(chunk, &mut out, &mut actions);
        }
        (out, actions)
    }

    #[test]
    fn plain_output_passes_through() {
        let input = b"hello \x1b[1;31mred\x1b[0m \x1b]0;title\x07\x1b[?25h\x1b[?1049l";
        assert_eq!(run(&[input]), (input.to_vec(), vec![]));
    }

    #[test]
    fn scheme_query_is_consumed() {
        assert_eq!(
            run(&[b"a\x1b[?996nb"]),
            (b"ab".to_vec(), vec![Action::QueryScheme])
        );
    }

    #[test]
    fn mode_query_is_consumed() {
        assert_eq!(run(&[b"\x1b[?2031$p"]), (vec![], vec![Action::QueryMode]));
    }

    #[test]
    fn other_decrqm_and_dsr_pass_through() {
        let input = b"\x1b[?2004$p\x1b[?6n\x1b[6n\x1b[?2031;1$p\x1b[2031$p";
        assert_eq!(run(&[input]), (input.to_vec(), vec![]));
    }

    #[test]
    fn decset_is_seen_but_passed_through() {
        let input = b"\x1b[?2031h\x1b[?1004;2031l";
        assert_eq!(
            run(&[input]),
            (
                input.to_vec(),
                vec![Action::SetUpdates(true), Action::SetUpdates(false)]
            )
        );
    }

    #[test]
    fn sequences_split_across_reads_are_caught() {
        let (out, actions) = run(&[b"x\x1b", b"[", b"?99", b"6", b"ny"]);
        assert_eq!(out, b"xy");
        assert_eq!(actions, vec![Action::QueryScheme]);

        let (out, actions) = run(&[b"\x1b[?2031", b"$", b"p"]);
        assert!(out.is_empty());
        assert_eq!(actions, vec![Action::QueryMode]);
    }

    #[test]
    fn near_misses_are_released_intact() {
        // A different final byte, a non-private CSI, and a doubled ESC.
        let input = b"\x1b[?996m\x1b[996n\x1b\x1b[?2031h";
        let (out, actions) = run(&[input]);
        assert_eq!(out, input);
        assert_eq!(actions, vec![Action::SetUpdates(true)]);
    }

    #[test]
    fn overlong_parameters_are_not_held_forever() {
        let mut input = b"\x1b[?".to_vec();
        input.extend(std::iter::repeat_n(b'1', 100));
        input.push(b'n');
        assert_eq!(run(&[&input]), (input.clone(), vec![]));
    }

    #[test]
    fn ris_resets_the_mode() {
        let (out, actions) = run(&[b"\x1bc"]);
        assert_eq!(out, b"\x1bc");
        assert_eq!(actions, vec![Action::Reset]);
    }

    #[test]
    fn report_format() {
        assert_eq!(scheme_report(true), "\x1b[?997;1n");
        assert_eq!(scheme_report(false), "\x1b[?997;2n");
    }
}
