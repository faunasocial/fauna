//! Which graphics protocol — if any — this terminal can paint an image with.
//!
//! [`apps/tui.md` § Rendering] puts the whole of this module's mandate in one
//! sentence: *"Protocol choice is **auto-detected, never configured** (bucket 1
//! of the configuration invariant — no human chooses it)."* So there is no knob
//! here, and there must never be one: no config file, no env var a human is told
//! to set, no `tui-settings` row. A user who has to tell fauna that their
//! terminal does sixel is a user fauna failed to detect.
//!
//! **The split.** [`decide`] is a pure function over [`Env`] + an optional
//! [`Da1`], and [`probe_da1`] is the only I/O in the module. That is deliberate:
//! the decision is the part with the branches worth testing, and a pure function
//! is testable headlessly and exhaustively. Only the round-trip needs a terminal.
//!
//! **Why DA1 and not env-sniffing.** Asking the terminal (`ESC [ c`, primary
//! device attributes — a sixel-capable terminal reports `4` among its parameters)
//! is what actually answers *"can this terminal paint a sixel"*. `TERM` answers
//! *"what termcap should I use"*, which is a different question and a famously
//! unreliable proxy. It matters most in the case below.
//!
//! **The multiplexer branch is first-class, not an afterthought.** Inside tmux or
//! screen:
//!
//! - kitty and iTerm2 escapes are **stripped** unless the multiplexer is
//!   configured to pass them through, so those arms can never arrive. Passthrough
//!   is off by default, and — per the invariant above — asking the user to turn it
//!   on is not an option available to us.
//! - tmux 3.4+ parses **sixel** natively, so sixel is the one arm that survives.
//! - the outer terminal is **not discoverable from inside**: the tmux server is
//!   parented to init, so it does not inherit (and cannot be asked for) the
//!   window's `TERM_PROGRAM`. Env-sniffing therefore *cannot* answer the question
//!   here — but DA1 can, because tmux forwards the query to the outer terminal
//!   and relays its reply. This is the case that makes DA1 load-bearing rather
//!   than merely tidy.
//!
//! **The probe has two bodies, one per OS family, and that is the only thing
//! about this module that is platform-divergent.** The question, the reply
//! format, the parse and the decision are identical everywhere; what differs is
//! how you hand a terminal a byte and read one back. Unix has a file descriptor
//! (`/dev/tty`); Windows has the console API (`CONIN$`/`CONOUT$` plus
//! `ReadConsoleInputW`), which delivers the reply as *key events* rather than a
//! byte stream and so needs one extra step — reassembling those events into the
//! bytes [`parse_da1`] reads. Both arms answer the same `Option<Da1>`, and every
//! failure on either means "assume nothing", where the caller's half-block
//! fallback paints a real picture.

use std::time::Duration;

/// How long [`probe_da1`] waits for the terminal to answer.
///
/// A terminal answers DA1 in microseconds; this budget exists for the terminal
/// that never answers at all (and for a slow link, where the reply is a round
/// trip away). It is spent **once**, at startup, and only when [`Env`] is not
/// already decisive — so the common local cases pay nothing. The cost of it
/// expiring is the half-block fallback, which is a picture either way.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// The primary-device-attributes query: *"what are you?"*.
///
/// The same eight bits on every OS — a terminal is a terminal. Only the way it
/// is handed over differs (module docs), so both probe arms write this constant.
const DA1_QUERY: &[u8] = b"\x1b[c";

/// A reply longer than this is not a DA1 response, and reading further is a
/// terminal that will never terminate the sequence. Bounds the probe's memory
/// and its loop — on both arms, like [`DA1_QUERY`] above.
const MAX_REPLY: usize = 1024;

/// The graphics protocol a run paints thumbnails with.
///
/// Not `Option<Protocol>` with a `None` fallback: half-blocks are a real arm
/// that paints a real picture (`crate::thumbnail`), not the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// kitty's graphics protocol — direct RGB, true colour, its own image layer.
    Kitty,
    /// iTerm2's inline-images protocol (OSC 1337).
    Iterm2,
    /// Sixel — the oldest arm, and the only one that survives a multiplexer.
    Sixel,
    /// The `▀` cell art of [`crate::thumbnail`]. Always available; always correct.
    HalfBlock,
}

/// The environment inputs [`decide`] reads. A struct rather than direct
/// `env::var` calls so the decision stays pure and every branch is constructible
/// in a test.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Env {
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub kitty_window_id: Option<String>,
}

impl Env {
    /// Read the real process environment.
    pub fn from_process() -> Self {
        Self {
            term: std::env::var("TERM").ok(),
            term_program: std::env::var("TERM_PROGRAM").ok(),
            kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok(),
        }
    }

    /// Whether a terminal multiplexer sits between us and the terminal.
    ///
    /// `TERM` is set by the multiplexer itself for the programs it hosts, which
    /// makes it reliable for *this* question specifically — unlike the "which
    /// terminal am I in" question it cannot answer (module docs).
    fn in_multiplexer(&self) -> bool {
        self.term
            .as_deref()
            .is_some_and(|term| term.starts_with("tmux") || term.starts_with("screen"))
    }

    fn is_kitty(&self) -> bool {
        self.kitty_window_id.is_some() || self.term.as_deref() == Some("xterm-kitty")
    }

    fn is_iterm2(&self) -> bool {
        self.term_program.as_deref() == Some("iTerm.app")
    }

    /// Whether [`decide`] would consult a DA1 reply for this environment.
    ///
    /// The probe is a round trip with a timeout, so skip it when the answer
    /// cannot change: a native kitty or iTerm2 window is already decisive.
    /// Inside a multiplexer it is never skippable — it is the *only* signal
    /// (module docs).
    pub fn needs_da1_probe(&self) -> bool {
        self.in_multiplexer() || !(self.is_kitty() || self.is_iterm2())
    }
}

/// A parsed primary-device-attributes reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Da1 {
    /// The reported attribute parameters, verbatim.
    pub params: Vec<u32>,
}

impl Da1 {
    /// Attribute `4` is "sixel graphics" — the one bit of a DA1 reply we act on.
    pub fn supports_sixel(&self) -> bool {
        self.params.contains(&4)
    }
}

/// Parse a DA1 reply out of `bytes`, tolerating leading noise and a partial tail.
///
/// The shape is `ESC [ ? <p> ; <p> ; … c` — e.g. `ESC [ ? 6 4 ; 1 ; 2 ; 4 c` for
/// an xterm reporting sixel support. Returns `None` while the terminating `c`
/// has not arrived yet, which is what lets [`probe_da1`] call this on a growing
/// buffer and stop the moment the reply completes.
///
/// Unparseable parameters are skipped rather than failing the reply: we act on
/// the presence of `4` and nothing else, so a vendor extension we do not
/// understand must not cost us the answer.
///
/// OS-neutral by construction, and called by **both** probe arms: unix parses a
/// growing read buffer, Windows parses the bytes reassembled out of console key
/// events. A DA1 reply is the same sequence either way, so there is exactly one
/// parser and its unit tests run on every platform this client ships to.
pub fn parse_da1(bytes: &[u8]) -> Option<Da1> {
    let start = bytes.windows(3).position(|window| window == b"\x1b[?")? + 3;
    let len = bytes[start..].iter().position(|&byte| byte == b'c')?;
    let params = bytes[start..start + len]
        .split(|&byte| byte == b';')
        .filter_map(|field| std::str::from_utf8(field).ok()?.trim().parse::<u32>().ok())
        .collect();
    Some(Da1 { params })
}

/// The decision. Pure — every input is an argument.
///
/// Order is meaning, not preference-by-taste:
///
/// 1. **Multiplexer first.** It strips kitty/iTerm2 regardless of what the outer
///    terminal is, so no env signal can promote them (module docs). Sixel or
///    nothing, and only DA1 knows which.
/// 2. **kitty before iTerm2 before sixel.** kitty's protocol carries true colour
///    and its own image layer; sixel goes through a palette. Where a terminal
///    offers both, the better arm wins.
/// 3. **DA1 last**, for everything that did not announce itself in the
///    environment — the general case, and the honest one.
pub fn decide(env: &Env, da1: Option<&Da1>) -> Protocol {
    let sixel = da1.is_some_and(Da1::supports_sixel);

    if env.in_multiplexer() {
        return if sixel {
            Protocol::Sixel
        } else {
            Protocol::HalfBlock
        };
    }
    if env.is_kitty() {
        return Protocol::Kitty;
    }
    if env.is_iterm2() {
        return Protocol::Iterm2;
    }
    if sixel {
        return Protocol::Sixel;
    }
    Protocol::HalfBlock
}

/// Detect the protocol for this process: read the environment, probe if the
/// environment is not already decisive, decide.
///
/// **Call this exactly once, after raw mode is enabled and before the crossterm
/// event stream starts** — see [`probe_da1`] for why the ordering is load-bearing
/// rather than stylistic.
pub fn detect() -> Protocol {
    let env = Env::from_process();
    let da1 = if env.needs_da1_probe() {
        probe_da1(PROBE_TIMEOUT)
    } else {
        None
    };
    let protocol = decide(&env, da1.as_ref());
    tracing::info!(?protocol, term = ?env.term, da1 = ?da1, "terminal graphics protocol detected");
    protocol
}

/// Ask the terminal for its primary device attributes and read the reply.
///
/// **⚠ Ordering is load-bearing, and getting it wrong fails silently.** crossterm
/// parses a DA1 reply into `InternalEvent::PrimaryDeviceAttributes` — an
/// *internal* event that never surfaces on the public `Event` stream. So a probe
/// racing a live `EventStream` has its reply **eaten**, and the only symptom is
/// that every terminal on earth appears not to support sixel. Probe before the
/// event stream exists. (crossterm reads DA1 itself for
/// `supports_keyboard_enhancement`, but its filter/`read_internal` machinery is
/// crate-private, so we cannot borrow the reader — only the pattern.)
///
/// Raw mode must already be on, or the reply sits in the line discipline's
/// buffer waiting for a newline that is never coming.
///
/// Prefers `/dev/tty` — the controlling terminal is the thing being asked, and
/// it stays reachable when either standard stream is redirected, which is
/// crossterm's own reasoning for the same query (`terminal/sys/unix.rs`).
///
/// **But it falls back to the standard streams, and that path is not exotic.**
/// A process with no controlling terminal cannot open `/dev/tty` at all — the
/// open fails outright with `ENXIO` — and that describes any `setsid` launch,
/// including a pty child that inherited the slave as its stdio without ever
/// claiming it as a controlling terminal. Its terminal is still perfectly
/// reachable: it is on the other end of the fds it already has. crossterm falls
/// back the same way for the write half of this very query. Without the
/// fallback the probe fails on exactly those launches, and — because failure
/// means "assume nothing" — every terminal reads as incapable, silently. That is
/// what the e2e (`test_tui_media_graphics_protocol.py`) runs on, and it is what
/// caught this.
///
/// Reading from stdin can in principle consume a keystroke typed inside the
/// probe's window, which is why that is the fallback and not the default; the
/// window is one round trip at startup, before anything is on screen to type at.
///
/// Returns `None` on any failure — no terminal, no reply, a timeout, a reply we
/// cannot parse. Every one of those means "assume nothing", and the caller's
/// fallback paints. (The Windows arm below answers the same question through the
/// console API; every sentence above about ordering, raw mode and "assume
/// nothing" applies verbatim to it.)
#[cfg(unix)]
pub fn probe_da1(timeout: Duration) -> Option<Da1> {
    use std::os::unix::io::AsRawFd;

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok();
    let (read_fd, write_fd) = match &tty {
        Some(file) => (file.as_raw_fd(), file.as_raw_fd()),
        None => (libc::STDIN_FILENO, libc::STDOUT_FILENO),
    };
    probe_fds(read_fd, write_fd, timeout)
}

/// The query/reply round trip on a pair of already-open descriptors.
///
/// Raw `libc` rather than `File`: the fallback path hands this the process's own
/// stdin/stdout, and wrapping those in a `File` would close them on drop.
#[cfg(unix)]
fn probe_fds(read_fd: i32, write_fd: i32, timeout: Duration) -> Option<Da1> {
    if !write_all_fd(write_fd, DA1_QUERY) {
        return None;
    }

    let deadline = std::time::Instant::now() + timeout;
    let mut reply = Vec::new();
    let mut chunk = [0u8; 64];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        if !poll_readable(read_fd, remaining) {
            return None;
        }
        // SAFETY: `chunk` is a live, owned buffer and the length matches it.
        let read = unsafe { libc::read(read_fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read <= 0 {
            return None;
        }
        reply.extend_from_slice(&chunk[..read as usize]);
        if let Some(da1) = parse_da1(&reply) {
            return Some(da1);
        }
        if reply.len() > MAX_REPLY {
            return None;
        }
    }
}

#[cfg(unix)]
fn write_all_fd(fd: i32, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a live slice and the length matches it.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written <= 0 {
            return false;
        }
        bytes = &bytes[written as usize..];
    }
    true
}

#[cfg(unix)]
fn poll_readable(fd: i32, timeout: Duration) -> bool {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll_fd` is a single initialized `pollfd` owned by this frame, and
    // the count matches the one-element pointer.
    unsafe {
        libc::poll(
            &mut poll_fd,
            1,
            timeout.as_millis().min(i32::MAX as u128) as i32,
        ) > 0
    }
}

/// Ask the terminal for its primary device attributes — the Windows arm.
///
/// Same question, same answer, different plumbing (module docs). `CONIN$` and
/// `CONOUT$` are Windows' `/dev/tty`: they name the console this process is
/// attached to, and stay reachable when either standard stream is redirected —
/// so the unix arm's reasoning for preferring the controlling terminal, and its
/// fallback to the standard streams when there is none, both carry over
/// unchanged. They are opened through `OpenOptions` rather than `CreateFileW`
/// precisely so the two arms read as the same three lines.
///
/// A process with no console at all (a service, a GUI-subsystem launch) cannot
/// open either, and `None` is the right answer there for the same reason it is
/// on unix: assume nothing, and paint half-blocks.
#[cfg(windows)]
pub fn probe_da1(timeout: Duration) -> Option<Da1> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;

    let conin = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .ok();
    let mut conout = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .ok();

    let stdin = std::io::stdin();
    let read = HANDLE(match &conin {
        Some(file) => file.as_raw_handle(),
        None => stdin.as_raw_handle(),
    });
    let mut stdout = std::io::stdout();
    let written_to = HANDLE(match &conout {
        Some(file) => file.as_raw_handle(),
        None => stdout.as_raw_handle(),
    });
    let write: &mut dyn std::io::Write = match conout.as_mut() {
        Some(file) => file,
        None => &mut stdout,
    };

    probe_console(read, written_to, write, timeout)
}

/// The query/reply round trip on an already-open console.
///
/// **This function changes no console mode, and that is a measured decision, not
/// an oversight.** The obvious-looking move is to set
/// `ENABLE_VIRTUAL_TERMINAL_INPUT` for the round trip so the console hands the
/// reply through verbatim. It is unnecessary: the console delivers a
/// device-attributes reply as ordinary key events either way, which
/// [`reply_byte`] reassembles. Measured, not assumed — removing the flag was run
/// as a mutation against the tier_3 e2e on Windows (2026-08-01) and the test
/// stayed green, so the flag was doing nothing. Two reasons not to set it back:
/// crossterm's event stream is configured for the mode the terminal init left
/// behind and does not expect a different one, and a set/restore pair around a
/// fallible round trip is a mode left flipped if anything between them panics.
/// The reply is read as key events on purpose.
///
/// The reply arrives one key event per character, so this is the unix read loop
/// plus that reassembly step. Reading input can in principle consume a keystroke
/// typed inside the probe's window — the same tradeoff the unix arm takes for
/// the same reason: the window is one round trip at startup, before anything is
/// on screen to type at.
#[cfg(windows)]
fn probe_console(
    read: windows::Win32::Foundation::HANDLE,
    written_to: windows::Win32::Foundation::HANDLE,
    write: &mut dyn std::io::Write,
    timeout: Duration,
) -> Option<Da1> {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Console::{
        CONSOLE_MODE, GetConsoleMode, INPUT_RECORD, ReadConsoleInputW,
    };
    use windows::Win32::System::Threading::WaitForSingleObject;

    let mut out_mode = CONSOLE_MODE::default();
    // SAFETY: `written_to` is a live console-output handle owned by the caller,
    // and `out_mode` is a live, owned out-parameter.
    unsafe { GetConsoleMode(written_to, &mut out_mode) }.ok()?;
    if !console_relays_a_query(out_mode) {
        return None;
    }

    write.write_all(DA1_QUERY).ok()?;
    write.flush().ok()?;

    let deadline = std::time::Instant::now() + timeout;
    let mut reply = Vec::new();
    let mut records = [INPUT_RECORD::default(); 64];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        // A console input handle is signalled while its buffer is non-empty, so
        // this is the `poll` of the unix arm.
        // SAFETY: `read` is a live console-input handle owned by the caller.
        let waited = unsafe {
            WaitForSingleObject(read, remaining.as_millis().min(u32::MAX as u128) as u32)
        };
        if waited != WAIT_OBJECT_0 {
            return None;
        }
        let mut count = 0u32;
        // SAFETY: `records` is a live, owned buffer and `count` a live
        // out-parameter; the wait above guarantees at least one record, so this
        // does not block.
        unsafe { ReadConsoleInputW(read, &mut records, &mut count) }.ok()?;
        for record in &records[..count as usize] {
            if let Some(byte) = record_reply_byte(record) {
                reply.push(byte);
            }
        }
        if let Some(da1) = parse_da1(&reply) {
            return Some(da1);
        }
        if reply.len() > MAX_REPLY {
            return None;
        }
    }
}

/// Whether this console will pass our query on to the terminal at all.
///
/// **Do not ask a console that will print the question instead.** A terminal
/// only ever sees `ESC [ c` if the console is forwarding our output as VT;
/// without `ENABLE_VIRTUAL_TERMINAL_PROCESSING` the console renders it as three
/// glyphs on the user's screen and no reply is coming. Every other failure in
/// this module costs a half-block fallback and nothing else — this one would
/// cost visible corruption, which is why it is a guard rather than a mode the
/// probe sets for itself.
///
/// The terminal init turns the flag on (it is how every frame is painted), so a
/// real launch passes; a console old enough not to have it has no sixel to offer
/// either. That also means the e2e can only ever exercise the `true` side —
/// hence the predicate, so the other side is pinned by a test rather than by
/// reasoning.
#[cfg(windows)]
fn console_relays_a_query(out_mode: windows::Win32::System::Console::CONSOLE_MODE) -> bool {
    use windows::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;

    (out_mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING).0 != 0
}

/// The reply byte an input record carries, if any — the union access, and
/// nothing else. The decision itself is [`reply_byte`], which is where the
/// branches worth testing live.
#[cfg(windows)]
fn record_reply_byte(record: &windows::Win32::System::Console::INPUT_RECORD) -> Option<u8> {
    use windows::Win32::System::Console::KEY_EVENT;

    if u32::from(record.EventType) != KEY_EVENT {
        return None;
    }
    // SAFETY: `EventType` selects the union arm, and it was just checked to be
    // `KEY_EVENT`. `UnicodeChar` is the documented arm of `uChar` for a
    // wide-character read (`ReadConsoleInputW`).
    let key = unsafe { record.Event.KeyEvent };
    // SAFETY: as above — `uChar`'s two arms are the same two bytes read as
    // UTF-16 or as ANSI, and the wide read fills the UTF-16 one.
    let unicode = unsafe { key.uChar.UnicodeChar };
    reply_byte(key.bKeyDown.as_bool(), unicode)
}

/// Whether a console key event contributes a byte to the reply, and which.
///
/// Three things get dropped, and each would corrupt the reply if it were not:
///
/// - **key-up records**, because a keypress delivers one of each and keeping
///   both would double every character — `ESC [ ? …` arriving as
///   `ESC ESC [ [ ? ? …`, which is not a DA1 reply and never becomes one;
/// - **records carrying no character** (`0`) — a bare modifier, or a key event
///   the console has no character for;
/// - **code units outside ASCII**, dropped rather than decoded: a DA1 reply is
///   ASCII by construction, so anything wider is a keystroke caught in the
///   probe's window, and [`parse_da1`] already tolerates noise. Dropping them
///   also means no surrogate pair can ever be half-consumed here.
#[cfg(windows)]
fn reply_byte(key_down: bool, unicode: u16) -> Option<u8> {
    if !key_down {
        return None;
    }
    u8::try_from(unicode)
        .ok()
        .filter(|byte| byte.is_ascii() && *byte != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(term: &str) -> Env {
        Env {
            term: Some(term.to_string()),
            ..Env::default()
        }
    }

    fn sixel_da1() -> Da1 {
        // An xterm built with sixel: VT420-class, with `4` among the attributes.
        Da1 {
            params: vec![64, 1, 2, 4, 6, 9, 15],
        }
    }

    fn plain_da1() -> Da1 {
        Da1 {
            params: vec![64, 1, 2, 6, 9, 15],
        }
    }

    #[test]
    fn parses_a_real_xterm_da1_reply() {
        let parsed = parse_da1(b"\x1b[?64;1;2;4;6;9;15c").expect("a complete reply parses");
        assert_eq!(parsed.params, vec![64, 1, 2, 4, 6, 9, 15]);
        assert!(parsed.supports_sixel());
    }

    #[test]
    fn a_reply_without_attribute_4_is_not_sixel_capable() {
        let parsed = parse_da1(b"\x1b[?64;1;2;6;9;15c").expect("a complete reply parses");
        assert!(!parsed.supports_sixel());
    }

    #[test]
    fn parses_a_reply_buried_in_leading_noise() {
        // The probe reads whatever is in the tty buffer; a stray keystroke ahead
        // of the reply must not cost us the answer.
        let parsed = parse_da1(b"xy\x1b[?64;4c").expect("leading noise is skipped");
        assert!(parsed.supports_sixel());
    }

    #[test]
    fn an_unterminated_reply_parses_as_none_so_the_probe_keeps_reading() {
        // This is what lets `probe_da1` call `parse_da1` on a growing buffer:
        // "not yet" and "never" must be the same answer to the caller.
        assert_eq!(parse_da1(b"\x1b[?64;1;2;4"), None);
        assert_eq!(parse_da1(b""), None);
        assert_eq!(parse_da1(b"\x1b[?"), None);
    }

    #[test]
    fn unparseable_parameters_are_skipped_not_fatal() {
        // A vendor extension we do not understand must not cost us the `4`.
        let parsed = parse_da1(b"\x1b[?64;xx;4c").expect("garbage params are skipped");
        assert!(parsed.supports_sixel());
    }

    #[test]
    fn in_tmux_sixel_is_the_only_arm_that_can_arrive() {
        // The recorded user terminal: tmux, passthrough off, outer terminal does
        // sixel. tmux relays the DA1 reply, which is the only reason we know.
        assert_eq!(
            decide(&env("tmux-256color"), Some(&sixel_da1())),
            Protocol::Sixel
        );
        assert_eq!(
            decide(&env("screen.xterm"), Some(&sixel_da1())),
            Protocol::Sixel
        );
    }

    #[test]
    fn in_tmux_without_sixel_falls_back_to_half_blocks() {
        assert_eq!(
            decide(&env("tmux-256color"), Some(&plain_da1())),
            Protocol::HalfBlock
        );
        assert_eq!(decide(&env("tmux-256color"), None), Protocol::HalfBlock);
    }

    #[test]
    fn in_tmux_kitty_and_iterm2_are_never_chosen_however_loud_the_environment() {
        // The load-bearing branch. The multiplexer strips those escapes, so a
        // kitty *hosting* tmux still cannot receive them — believing the env here
        // paints nothing at all, which is strictly worse than half-blocks.
        let kitty_in_tmux = Env {
            term: Some("tmux-256color".to_string()),
            kitty_window_id: Some("1".to_string()),
            term_program: Some("iTerm.app".to_string()),
        };
        assert_eq!(decide(&kitty_in_tmux, Some(&sixel_da1())), Protocol::Sixel);
        assert_eq!(decide(&kitty_in_tmux, None), Protocol::HalfBlock);
    }

    #[test]
    fn a_native_kitty_window_is_detected_from_the_environment() {
        let by_var = Env {
            kitty_window_id: Some("1".to_string()),
            ..env("xterm-256color")
        };
        assert_eq!(decide(&by_var, None), Protocol::Kitty);
        assert_eq!(decide(&env("xterm-kitty"), None), Protocol::Kitty);
    }

    #[test]
    fn kitty_wins_over_sixel_when_a_terminal_offers_both() {
        // True colour and its own image layer beat a 216-entry palette.
        assert_eq!(
            decide(&env("xterm-kitty"), Some(&sixel_da1())),
            Protocol::Kitty
        );
    }

    #[test]
    fn a_native_iterm2_window_is_detected_from_the_environment() {
        let iterm = Env {
            term_program: Some("iTerm.app".to_string()),
            ..env("xterm-256color")
        };
        assert_eq!(decide(&iterm, None), Protocol::Iterm2);
    }

    #[test]
    fn a_bare_terminal_reporting_sixel_gets_sixel() {
        assert_eq!(
            decide(&env("xterm-256color"), Some(&sixel_da1())),
            Protocol::Sixel
        );
    }

    #[test]
    fn a_terminal_that_says_nothing_gets_half_blocks() {
        assert_eq!(decide(&Env::default(), None), Protocol::HalfBlock);
        assert_eq!(decide(&env("xterm-256color"), None), Protocol::HalfBlock);
        assert_eq!(decide(&env("dumb"), None), Protocol::HalfBlock);
    }

    #[test]
    fn the_probe_is_skipped_only_when_the_environment_is_already_decisive() {
        // A round trip we do not need is startup latency we do not spend.
        assert!(!env_kitty().needs_da1_probe());
        assert!(!env_iterm2().needs_da1_probe());
        assert!(env("xterm-256color").needs_da1_probe());
        assert!(Env::default().needs_da1_probe());
    }

    #[test]
    fn inside_a_multiplexer_the_probe_is_never_skipped() {
        // Even a kitty-announcing environment must probe here: the reply is the
        // only thing that can distinguish sixel from nothing (module docs).
        let kitty_in_tmux = Env {
            term: Some("tmux-256color".to_string()),
            kitty_window_id: Some("1".to_string()),
            ..Env::default()
        };
        assert!(kitty_in_tmux.needs_da1_probe());
    }

    fn env_kitty() -> Env {
        Env {
            kitty_window_id: Some("1".to_string()),
            ..env("xterm-256color")
        }
    }

    fn env_iterm2() -> Env {
        Env {
            term_program: Some("iTerm.app".to_string()),
            ..env("xterm-256color")
        }
    }
}

/// The Windows arm's one platform-specific step: turning console key events back
/// into the bytes [`parse_da1`] reads.
///
/// Windows-gated because the thing under test is a Windows console behaviour —
/// the same reason `fauna-credential-store`'s `win_credman` tests are. The
/// *rest* of the arm (the query, the parse, the decision) is shared with unix
/// and tested above on every platform; what is left here is where a Windows
/// probe can go wrong, and it needs no console to test, because a key event is
/// two values.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// The characters of a reply as the console delivers them: one key-down and
    /// one key-up per character, which is the shape that makes [`reply_byte`]'s
    /// key-up filter load-bearing rather than tidy.
    fn as_key_events(bytes: &[u8]) -> Vec<(bool, u16)> {
        bytes
            .iter()
            .flat_map(|&byte| [(true, u16::from(byte)), (false, u16::from(byte))])
            .collect()
    }

    fn reassemble(events: &[(bool, u16)]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|&(down, unicode)| reply_byte(down, unicode))
            .collect()
    }

    #[test]
    fn a_reply_delivered_as_key_events_reassembles_and_parses() {
        // The whole Windows-specific claim in one assertion: what the console
        // hands us becomes exactly what the shared parser expects.
        let events = as_key_events(b"\x1b[?64;1;2;4;6;9;15c");
        let parsed = parse_da1(&reassemble(&events)).expect("the reassembled reply parses");
        assert!(parsed.supports_sixel());
    }

    #[test]
    fn keeping_key_up_records_would_double_every_character_past_recognition() {
        // Why the filter exists. Doubled, the reply reads `ESC ESC [ [ ? ? …`,
        // which contains no `ESC [ ?` at all — so the parse does not merely
        // mis-read it, it never finds a reply, and every terminal on Windows
        // would report as incapable.
        let doubled: Vec<u8> = as_key_events(b"\x1b[?64;4c")
            .iter()
            .filter_map(|&(_, unicode)| u8::try_from(unicode).ok())
            .collect();
        assert_eq!(parse_da1(&doubled), None);
    }

    #[test]
    fn a_bare_modifier_press_carries_nothing() {
        // Shift/Ctrl/Alt arrive as key events with no character. Pushing a `0`
        // for each would put NUL bytes inside the parameter list.
        assert_eq!(reply_byte(true, 0), None);
    }

    #[test]
    fn a_non_ascii_keystroke_in_the_probe_window_is_dropped_not_truncated() {
        // `é` is 0x00E9 — inside `u8` range and outside ASCII, so this is the
        // case a bare `as u8` would silently pass through as a stray byte. It
        // cannot be part of a DA1 reply, and a leading one must not cost us the
        // answer (the parse's noise tolerance covers it either way).
        assert_eq!(reply_byte(true, 0x00E9), None);
        assert_eq!(reply_byte(true, 0x30A2), None);
        let noisy = reassemble(&as_key_events(b"\x1b[?64;4c"));
        let mut with_leading_noise = reassemble(&[(true, 0x00E9), (true, u16::from(b'x'))]);
        with_leading_noise.extend_from_slice(&noisy);
        assert!(
            parse_da1(&with_leading_noise)
                .expect("noise ahead of the reply is skipped")
                .supports_sixel()
        );
    }

    #[test]
    fn a_console_not_forwarding_our_output_as_vt_is_never_asked() {
        // The branch the e2e structurally cannot reach: its console always has
        // the flag on, so only the `true` side is ever exercised there. Sending
        // the query to a console without it would print `ESC [ c` on the user's
        // screen — the one failure in this module that is worse than falling
        // back to half-blocks.
        use windows::Win32::System::Console::{
            CONSOLE_MODE, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        };

        assert!(console_relays_a_query(ENABLE_VIRTUAL_TERMINAL_PROCESSING));
        assert!(console_relays_a_query(
            ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING
        ));
        assert!(!console_relays_a_query(CONSOLE_MODE::default()));
        // A console with other flags but not this one is the real legacy case,
        // and it must read as "do not ask" rather than as "some flags are set".
        assert!(!console_relays_a_query(ENABLE_PROCESSED_OUTPUT));
    }

    #[test]
    fn every_byte_of_a_reply_survives_the_reassembly() {
        // A weaker version of the first test that still discriminates: the
        // reassembled stream must equal the reply verbatim, not merely contain
        // enough of it to parse.
        let reply = b"\x1b[?64;1;2;4;6;9;15c";
        assert_eq!(reassemble(&as_key_events(reply)), reply.to_vec());
    }
}
