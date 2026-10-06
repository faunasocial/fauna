//! Fire a new-message banner from a terminal — tui's leg of the DM OS-toast
//! *firing* every app owes.
//!
//! The when/for-whom decision is NOT here: it is the shared, unit-tested
//! `fauna_conversations::MessageNotificationTracker`
//! (`../../../docs/goal/ui/conversations.md` § Where logic lives — "only the
//! native toast *firing* is app glue"). This module is handed the threads that
//! tracker already decided warrant a banner, and does exactly one thing with
//! each: put it in front of the user.
//!
//! `../../../docs/goal/architecture/apps/tui.md` § System integration names
//! three arms — "in-app notification surface always; `notify-rust` desktop
//! notifications when a desktop session is present; OSC 9/777 terminal escapes
//! as the remote/SSH degradation". This module owns the last two and fires
//! exactly one of them per banner (see [`Arm`]).
//!
//! Neutral in the way `os_open` is neutral: the selection is a pure function of
//! (desktop session?, `TERM`), the escape bytes a pure function of (dialect,
//! label, snippet), and the dispatch takes its two sinks as arguments — so the
//! whole mechanism is unit-tested headlessly and the only thing left for a
//! human eye is "did a toast actually pop".

use std::io::Write as _;
use std::sync::OnceLock;

/// How the banner reaches the user. One arm fires per banner — never two.
///
/// The goal doc's "with a desktop session … as the remote/SSH degradation" is
/// an either/or: firing both would double every banner on a desktop box, so the
/// arm is *selected* ([`select_arm`]), never stacked. The selector is
/// `fauna_credential_store::keyring_probe` — the predicate tui already trusts
/// for exactly this "is there a usable desktop session here" question when it
/// picks a credential backend (a freedesktop Secret Service that connects AND
/// is unlocked on linux; the default keychain unlocked on macOS). No env knob,
/// and no second desktop-detection heuristic for the fleet to keep in sync.
///
/// **Windows has no such signal**: its Credential Manager probe is a constant
/// `true` (no lock state, no session bus), so on windows the desktop arm always
/// wins — a tui run over SSH included, where the toast lands in a session
/// nobody is looking at and the escape would have reached the user. `tui.md`
/// records that gap rather than papering over it with a windows-only heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// A `notify-rust` notification handed to the platform's notification
    /// service (freedesktop daemon, macOS Notification Center, Windows toast).
    /// The strings are data to that service, never control bytes, so this arm
    /// does not need [`sanitize`].
    Desktop,
    /// An OSC escape written to the controlling terminal. Works over SSH — the
    /// sequence is forwarded to the *user's* terminal emulator, which is the
    /// whole reason a terminal app is not notification-less.
    TerminalEscape(Dialect),
}

/// Which OSC dialect to speak. Exactly one is emitted per banner.
///
/// Emitting both would be the obvious "maximise coverage" move and it is wrong:
/// a terminal that understands both (WezTerm does) pops **two** toasts for one
/// message. There is no runtime capability query for either sequence, so the
/// choice is made from the terminal's own self-identification and defaults to
/// the wider-supported dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// `ESC ] 9 ; <body> BEL` — one field, no title. iTerm2, WezTerm, ConEmu,
    /// Windows Terminal, Ghostty. The default: strictly more terminals speak
    /// this than speak 777, and a terminal that speaks neither ignores it.
    Osc9,
    /// `ESC ] 777 ; notify ; <title> ; <body> BEL` — title/body split, so the
    /// thread label and the message preview stay visually distinct. Chosen only
    /// for terminals that speak 777 and *not* 9, so this is never a downgrade.
    Osc777,
}

/// Terminals that speak OSC 777 but not OSC 9, matched against `TERM`.
///
/// Deliberately a short allowlist of substrings rather than an attempt at a
/// complete terminal census: every terminal NOT named here still gets a working
/// banner via the OSC 9 default, so a missing entry costs a title/body split,
/// never a missed notification. That asymmetry is why the default is 9.
const OSC777_TERMS: &[&str] = &["foot", "rxvt", "rio"];

/// The longest thread label that goes into an escape. See [`MAX_SNIPPET`].
const MAX_LABEL: usize = 64;

/// The longest message preview that goes into an escape.
///
/// A terminal notification is a one-line popup; more than this is truncated by
/// the notification daemon anyway, and an unbounded write of remote-authored
/// text into the terminal's control channel is not something to hand over on
/// trust. Counted in `char`s, and sliced on a `char` boundary, so a multi-byte
/// grapheme is never cut in half into invalid UTF-8.
const MAX_SNIPPET: usize = 160;

/// Fire a banner for one thread. `label` is the DM peer / group name, `snippet`
/// the message preview — the same two fields linux hands `notify_message`, and
/// the same two `fauna_conversations::ThreadActivity` carries.
///
/// Fire-and-forget and silent on failure, like [`crate::os_open::open`]: a
/// terminal that ignores the sequence, a redirected stdout, or a closed pipe
/// just means no popup. The message is in the conversations list either way —
/// the banner is an accelerator, never the only delivery.
pub fn notify_message(label: &str, snippet: &str) {
    dispatch(arm(), label, snippet, show_desktop, emit);
}

/// Route one banner to exactly one sink. The sinks are parameters so the
/// one-arm-per-banner property is a unit test, not a promise.
///
/// A desktop arm that *fails* (no notification daemon answering on the session
/// bus, a refused toast) degrades to the escape for that banner — still exactly
/// one attempt reaches the user, so this is a fallback, not stacking.
fn dispatch(
    arm: Arm,
    label: &str,
    snippet: &str,
    desktop: impl FnOnce(&str, &str) -> Result<(), ()>,
    escape: impl FnOnce(&str),
) {
    let dialect = match arm {
        Arm::Desktop => match desktop(label, snippet) {
            Ok(()) => return,
            Err(()) => detect_dialect(&term()),
        },
        Arm::TerminalEscape(dialect) => dialect,
    };
    escape(&escape_for(dialect, label, snippet));
}

/// Resolve which arm fires for this process.
fn arm() -> Arm {
    select_arm(desktop_session(), &term())
}

/// The pure selection: a desktop session wins, otherwise the terminal escape in
/// the terminal's own dialect.
fn select_arm(desktop_session: bool, term: &str) -> Arm {
    if desktop_session {
        Arm::Desktop
    } else {
        Arm::TerminalEscape(detect_dialect(term))
    }
}

/// Whether a desktop session is present — `fauna_credential_store::keyring_probe`,
/// asked once per process.
///
/// Cached because the linux probe is a session-bus round trip on its own
/// thread, and this runs on the UI tick. A session whose keyring is unlocked
/// only after the first banner keeps the escape arm until restart: a banner
/// still reaches the user, so the cost is the choice of popup, never a lost one.
pub(crate) fn desktop_session() -> bool {
    static DESKTOP: OnceLock<bool> = OnceLock::new();
    *DESKTOP.get_or_init(fauna_credential_store::keyring_probe)
}

fn term() -> String {
    std::env::var("TERM").unwrap_or_default()
}

/// The desktop arm: the summary/body shape linux's `notify_message` uses,
/// restricted to the builder calls every `notify-rust` platform arm offers.
fn show_desktop(label: &str, snippet: &str) -> Result<(), ()> {
    notify_rust::Notification::new()
        .summary(&fauna_i18n::strings::notifications::message_from(label))
        .body(snippet)
        .appname("Fauna")
        .show()
        .map(|_| ())
        .map_err(|_| ())
}

/// Pick the dialect from the terminal's `TERM` self-identification.
///
/// Split from [`arm`] so it is a pure function of the string and can be tested
/// without setting a process-global env var (the `os_open` unit-test shape —
/// process-global mutation in a test is a cross-test race, not a test).
fn detect_dialect(term: &str) -> Dialect {
    let term = term.to_ascii_lowercase();
    if OSC777_TERMS.iter().any(|t| term.contains(t)) {
        Dialect::Osc777
    } else {
        Dialect::Osc9
    }
}

/// Build the escape sequence. Pure — this is the whole mechanism, so this is
/// what the unit tests pin.
fn escape_for(dialect: Dialect, label: &str, snippet: &str) -> String {
    let label = sanitize(label, MAX_LABEL);
    let snippet = sanitize(snippet, MAX_SNIPPET);
    match dialect {
        // No title field, so the label is folded into the body — otherwise the
        // popup says only "hi" with no hint of who sent it.
        Dialect::Osc9 => format!("\x1b]9;{label}: {snippet}\x07"),
        // `;` is the field separator, and the title is NOT the last field, so a
        // `;` in a thread label would shift the body one field left. The body
        // is terminal, so its own `;` are harmless and stay verbatim.
        Dialect::Osc777 => {
            let label = label.replace(';', ",");
            format!("\x1b]777;notify;{label};{snippet}\x07")
        }
    }
}

/// Strip everything that could break out of the escape, then truncate.
///
/// **This is a security boundary, not tidiness.** `snippet` is the body of a
/// message some remote actor sent this user, and it is about to be written into
/// the terminal's *control* channel. A `\x07` (BEL) or `\x1b` (ESC) that
/// survived would terminate the OSC string early and leave the rest of the
/// attacker's bytes being parsed by the terminal as fresh control sequences —
/// which is how a chat message becomes "rewrite the user's window title" or
/// "clear the scrollback".
///
/// So the filter is a whitelist-by-property, not a blocklist of known-bad
/// sequences: `char::is_control` covers C0, DEL and C1 in one predicate, and
/// every one of them becomes a space (rather than vanishing, which would
/// silently weld two words together). Runs of whitespace then collapse so a
/// body of newlines does not eat the whole length budget.
fn sanitize(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    let mut len = 0usize;
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_control() || ch.is_whitespace() {
            // Leading whitespace never opens the string; interior whitespace is
            // deferred until we know a real character follows, which collapses
            // runs and trims the tail in one pass.
            pending_space = len > 0;
            continue;
        }
        if pending_space {
            if len >= max {
                break;
            }
            out.push(' ');
            len += 1;
            pending_space = false;
        }
        if len >= max {
            break;
        }
        out.push(ch);
        len += 1;
    }
    out
}

/// Write the sequence to the controlling terminal.
///
/// Straight to stdout, the door `wizard::copy_to_clipboard`'s OSC 52 already
/// uses: an OSC string is consumed by the emulator and never enters the cell
/// grid, so this does not disturb ratatui's frame or need a redraw.
fn emit(seq: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_is_the_default_dialect() {
        assert_eq!(detect_dialect(""), Dialect::Osc9);
        assert_eq!(detect_dialect("xterm-256color"), Dialect::Osc9);
        assert_eq!(detect_dialect("screen.tmux"), Dialect::Osc9);
        assert_eq!(detect_dialect("wezterm"), Dialect::Osc9);
    }

    #[test]
    fn osc777_terminals_are_recognised_case_insensitively() {
        assert_eq!(detect_dialect("foot"), Dialect::Osc777);
        assert_eq!(detect_dialect("foot-extra"), Dialect::Osc777);
        assert_eq!(detect_dialect("rxvt-unicode-256color"), Dialect::Osc777);
        assert_eq!(detect_dialect("FOOT"), Dialect::Osc777);
        assert_eq!(detect_dialect("rio"), Dialect::Osc777);
    }

    #[test]
    fn a_desktop_session_selects_the_desktop_arm_whatever_the_terminal() {
        for term in ["", "xterm-256color", "foot"] {
            assert_eq!(select_arm(true, term), Arm::Desktop, "{term:?}");
        }
    }

    #[test]
    fn no_desktop_session_selects_the_escape_in_the_terminals_dialect() {
        assert_eq!(
            select_arm(false, "xterm-256color"),
            Arm::TerminalEscape(Dialect::Osc9)
        );
        assert_eq!(
            select_arm(false, "foot"),
            Arm::TerminalEscape(Dialect::Osc777)
        );
    }

    /// Runs one banner through [`dispatch`] and reports which sinks fired.
    fn fire(arm: Arm, desktop_ok: bool) -> (u32, Vec<String>) {
        let mut desktop_calls = 0;
        let mut escapes = Vec::new();
        dispatch(
            arm,
            "Ada",
            "shipped it",
            |_, _| {
                desktop_calls += 1;
                if desktop_ok { Ok(()) } else { Err(()) }
            },
            |seq| escapes.push(seq.to_owned()),
        );
        (desktop_calls, escapes)
    }

    /// The goal doc's "one arm fires per notification — never stacked": a
    /// desktop banner that landed writes no escape, and an escape banner never
    /// touches the desktop service.
    #[test]
    fn exactly_one_arm_fires_per_banner() {
        assert_eq!(fire(Arm::Desktop, true), (1, vec![]));
        assert_eq!(
            fire(Arm::TerminalEscape(Dialect::Osc9), true),
            (0, vec!["\x1b]9;Ada: shipped it\x07".to_owned()])
        );
    }

    /// A failed desktop notification degrades to one escape — the user still
    /// gets exactly one banner, and it still goes through [`escape_for`]'s
    /// sanitising.
    #[test]
    fn a_failed_desktop_banner_degrades_to_exactly_one_escape() {
        let (desktop_calls, escapes) = fire(Arm::Desktop, false);
        assert_eq!(desktop_calls, 1);
        assert_eq!(escapes.len(), 1);
        assert!(escapes[0].starts_with("\x1b]") && escapes[0].ends_with('\x07'));
    }

    /// Windows' Credential Manager probe is constant `true`, so on windows the
    /// desktop arm always wins — over SSH too (`tui.md` § System integration
    /// records the gap). Pinned so a change to that probe is a visible
    /// decision here, not a silent change of which arm windows users get.
    /// Asks the Credential Manager arm directly: the crate's public
    /// `keyring_probe` answers `false` in every test build by design
    /// (`live_keyring_allowed`), so it cannot witness the shipped answer.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_always_selects_the_desktop_arm() {
        let probe = fauna_credential_store::win_credman::keyring_probe();
        assert!(probe);
        assert_eq!(select_arm(probe, &term()), Arm::Desktop);
    }

    #[test]
    fn osc9_folds_the_label_into_the_single_body_field() {
        assert_eq!(
            escape_for(Dialect::Osc9, "Ada", "shipped it"),
            "\x1b]9;Ada: shipped it\x07"
        );
    }

    #[test]
    fn osc777_keeps_title_and_body_in_their_own_fields() {
        assert_eq!(
            escape_for(Dialect::Osc777, "Ada", "shipped it"),
            "\x1b]777;notify;Ada;shipped it\x07"
        );
    }

    /// The reason [`sanitize`] exists: a message body is remote-authored text
    /// heading for the terminal's control channel. If ESC or BEL survived, the
    /// sender would be writing control sequences on the user's terminal.
    #[test]
    fn a_hostile_message_body_cannot_close_the_sequence_or_inject_escapes() {
        let hostile = "hi\x07\x1b]0;pwned\x07\x1b[2J";
        for dialect in [Dialect::Osc9, Dialect::Osc777] {
            let seq = escape_for(dialect, "Ada", hostile);
            // Exactly one opening ESC and one closing BEL: ours.
            assert_eq!(seq.matches('\x1b').count(), 1, "{dialect:?}");
            assert_eq!(seq.matches('\x07').count(), 1, "{dialect:?}");
            assert!(seq.starts_with('\x1b'), "{dialect:?}");
            assert!(seq.ends_with('\x07'), "{dialect:?}");
            // The payload survives as inert text, minus the control bytes.
            assert!(seq.contains("hi ]0;pwned [2J"), "{seq:?}");
        }
    }

    /// Same boundary on the label — a group name is just as remote-authored as
    /// a message body.
    #[test]
    fn a_hostile_thread_label_cannot_inject_escapes() {
        let seq = escape_for(Dialect::Osc9, "Ada\x1b]0;x\x07", "hi");
        assert_eq!(seq.matches('\x1b').count(), 1);
        assert_eq!(seq.matches('\x07').count(), 1);
    }

    /// A `;` in a thread label would shift OSC 777's body one field left, so
    /// the title is the one place `;` is rewritten.
    #[test]
    fn a_semicolon_in_the_label_cannot_shift_osc777_fields() {
        let seq = escape_for(Dialect::Osc777, "a;b", "body;with;semis");
        assert_eq!(seq, "\x1b]777;notify;a,b;body;with;semis\x07");
    }

    #[test]
    fn newlines_and_tabs_collapse_instead_of_welding_words_together() {
        assert_eq!(sanitize("one\ntwo\t\tthree", 100), "one two three");
        assert_eq!(sanitize("  padded  ", 100), "padded");
        assert_eq!(sanitize("\n\n\n", 100), "");
    }

    #[test]
    fn truncation_counts_chars_and_never_splits_a_multibyte_grapheme() {
        let long = "é".repeat(500);
        let out = sanitize(&long, MAX_SNIPPET);
        assert_eq!(out.chars().count(), MAX_SNIPPET);
        // Round-trips as UTF-8 — a byte-slice truncation would have panicked or
        // produced invalid UTF-8 here.
        assert_eq!(out, "é".repeat(MAX_SNIPPET));
    }

    #[test]
    fn an_unbounded_body_cannot_flood_the_control_channel() {
        let seq = escape_for(Dialect::Osc9, &"L".repeat(9999), &"S".repeat(9999));
        assert!(
            seq.chars().count() <= MAX_LABEL + MAX_SNIPPET + 8,
            "{}",
            seq.len()
        );
    }

    #[test]
    fn an_empty_message_still_produces_a_well_formed_sequence() {
        assert_eq!(escape_for(Dialect::Osc9, "", ""), "\x1b]9;: \x07");
        assert_eq!(escape_for(Dialect::Osc777, "", ""), "\x1b]777;notify;;\x07");
    }
}
