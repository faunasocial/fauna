//! Auto-start at desktop sign-in — the linux leg of shape-A residency
//! (`docs/goal/architecture/apps/linux.md` § Auto-start at sign-in;
//! cross-app intent + the reference implementation in
//! `apps/windows.md` § App Lifecycle → *Auto-start at sign-in*).
//!
//! File sync itself now runs in the external per-user `fauna-sync-agent`
//! (systemd user unit — `crate::sync_agent`), so sync no longer depends on the
//! app being open. Auto-start is what makes the *app* — notifications, tray,
//! the UI — present at every sign-in without a manual step; on a stock GNOME
//! (no tray host) it is the only such mechanism, because the tray-host gate
//! makes close-to-tray a no-op there (§ System Tray).
//!
//! Two decisions live here as pure functions so they are unit-testable
//! without GTK or a desktop session — the same split windows uses
//! (`AutoStartGate` policy vs. `AutoStartService` mechanics):
//!
//! * [`should_register`] — whether the post-auth hook registers at all.
//! * [`should_start_hidden`] — whether an `--autostart` launch presents its
//!   window.

use std::path::PathBuf;

/// The CLI flag the registered `.desktop` entry passes back to us, marking a
/// launch as "started by the desktop session at sign-in" rather than by the
/// user. Written by the app itself — OS wiring, never a user-facing knob.
pub const AUTOSTART_FLAG: &str = "--autostart";

/// Whether the universal post-auth hook should (re-)register the autostart
/// entry. The linux twin of windows' `AutoStartGate.ShouldRegister`
/// (`FaunaApp.Core/Services/AutoStartGate.cs`) — the same truth table, so the
/// two apps cannot drift (priority #1).
///
/// * `is_e2e` — under e2e automation, never register: a harness login must not
///   write the dev machine's real autostart directory.
/// * `choice` — the persisted tri-state: `None` = never chosen ⇒ register by
///   default (works out-of-the-box); `Some(false)` = an explicit opt-out ⇒
///   never re-register (the client UI is the one configuration surface, and an
///   explicit choice is never overridden); `Some(true)` = an explicit opt-in ⇒
///   register (which also self-heals a stale `Exec` path after a move/upgrade).
pub fn should_register(is_e2e: bool, choice: Option<bool>) -> bool {
    if is_e2e {
        return false;
    }
    choice != Some(false)
}

/// Whether an auto-started launch should stay hidden (tray-resident) rather
/// than present its window.
///
/// Hidden **only** when a tray host is *confirmed*: with no host there is
/// nothing to restore a hidden window from, so a hidden start would be
/// unreachable — the out-of-the-box trap on stock GNOME. Fails safe to
/// visible, which is always recoverable (`linux.md` § Auto-start at sign-in:
/// "Linux therefore starts visible unless a tray host is confirmed").
pub fn should_start_hidden(is_autostart_launch: bool, tray_host_available: bool) -> bool {
    is_autostart_launch && tray_host_available
}

/// True when this process was launched by the desktop session's autostart
/// entry. Parsed ad-hoc from argv — `fauna-desktop` has no clap, and GTK takes
/// over argv (see `--version` in `main`).
pub fn is_autostart_launch() -> bool {
    std::env::args().any(|a| a == AUTOSTART_FLAG)
}

/// The argv to hand GTK: ours, minus [`AUTOSTART_FLAG`].
///
/// **Load-bearing, and measured the hard way.** `GApplication` parses argv
/// itself (the app registers no main options and does not set
/// `HANDLES_COMMAND_LINE`), so an unrecognised option makes it print
/// `Unknown option --autostart` and refuse to start. Since the entry we
/// register execs exactly that, an unfiltered argv breaks **every** desktop
/// sign-in — the feature would be worse than not shipping at all, and no unit
/// test that stops at `should_start_hidden` would notice.
///
/// [`is_autostart_launch`] reads the *real* process argv, so filtering here
/// costs nothing.
pub fn strip_autostart_flag<I: IntoIterator<Item = String>>(args: I) -> Vec<String> {
    args.into_iter().filter(|a| a != AUTOSTART_FLAG).collect()
}

/// The freedesktop autostart entry's contents.
///
/// `Exec` carries [`AUTOSTART_FLAG`] so the sign-in launch is tray-resident
/// rather than a window over a fresh desktop, and it is re-written on each
/// login with the *current* executable path, which self-heals an install that
/// moved or upgraded.
pub fn desktop_entry_contents(exec_path: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Fauna\n\
         Exec={exec_path} {AUTOSTART_FLAG}\n\
         Icon=fauna\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// `$XDG_CONFIG_HOME/autostart/fauna.desktop`.
///
/// Resolved through the one shared XDG helper (`window_state::dirs_config`) —
/// the same resolver `app-settings.json`, `window-state.json` and `theme.json`
/// use. `None` only when neither `XDG_CONFIG_HOME` nor `HOME` is set, in which
/// case there is no autostart directory to write and registration degrades to
/// a no-op.
pub fn desktop_path() -> Option<PathBuf> {
    let mut path = crate::window_state::dirs_config()?;
    path.push("autostart");
    path.push("fauna.desktop");
    Some(path)
}

/// The `Exec` path to register: this process's own executable, falling back to
/// the bare command name if the exe path is unavailable (it then resolves off
/// `PATH`, which is how a packaged install is normally reachable anyway).
fn current_exec_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "fauna-desktop".to_owned())
}

/// Write the autostart entry at `path`.
///
/// Best-effort and infallible by design, exactly like the settings store: a
/// desktop directory we cannot write must never block the app from starting.
fn write_entry_at(path: &std::path::Path, exec_path: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, desktop_entry_contents(exec_path));
}

fn write_entry() {
    if let Some(path) = desktop_path() {
        write_entry_at(&path, &current_exec_path());
    }
}

/// Remove the autostart entry (an explicit opt-out).
fn remove_entry() {
    if let Some(path) = desktop_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The post-auth hook's mechanism, with every input injected so the composition
/// — *decision → entry on disk* — is testable without a desktop session or an
/// `XDG_CONFIG_HOME` mutation. [`register_at_post_auth`] is the thin wrapper
/// that resolves the real ones.
///
/// An opt-out simply doesn't write: removing a stale entry is the *toggle*'s
/// job ([`apply_user_choice`]), so the hook stays idempotent and never touches
/// a file it didn't decide to own.
fn apply_registration_at(path: &std::path::Path, is_e2e: bool, choice: Option<bool>, exec: &str) {
    if !should_register(is_e2e, choice) {
        return;
    }
    write_entry_at(path, exec);
}

/// Apply an explicit user choice from the Settings toggle: persist the
/// tri-state *and* make the on-disk entry match it.
pub fn apply_user_choice(enabled: bool) {
    if let Some(store) = crate::app_settings::AppSettingsStore::new() {
        store.set_autostart_choice(enabled);
    }
    if enabled {
        write_entry();
    } else {
        remove_entry();
    }
}

/// The effective auto-start state to *display* in Settings — the user's
/// explicit choice, or the default when they never chose. Never reads the
/// `.desktop` file: file-existence cannot distinguish "never chosen" from
/// "deliberately off", which is the whole reason the choice is tri-state.
pub fn effective_choice() -> bool {
    crate::app_settings::AppSettingsStore::new()
        .and_then(|s| s.autostart_choice())
        .unwrap_or(true)
}

/// The universal post-auth hook's registration step (`launch_authenticated`).
///
/// Runs on every login and returning-user relaunch, so the *first* successful
/// login wires every later desktop sign-in; re-writing the entry each time
/// self-heals a moved/upgraded install.
pub fn register_at_post_auth() {
    let Some(path) = desktop_path() else {
        return;
    };
    let choice = crate::app_settings::AppSettingsStore::new().and_then(|s| s.autostart_choice());
    apply_registration_at(
        &path,
        crate::e2e_mode_enabled(),
        choice,
        &current_exec_path(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- should_register: the truth table windows pins in AutoStartTests.cs ---

    /// Default-on: a user who never chose gets auto-start registered at the
    /// post-auth hook, so sync is live at every sign-in with no manual step.
    #[test]
    fn no_choice_in_production_registers() {
        assert!(should_register(false, None));
    }

    /// An explicit opt-out is never overridden — the client UI is the one
    /// configuration surface.
    #[test]
    fn explicit_opt_out_in_production_does_not_register() {
        assert!(!should_register(false, Some(false)));
    }

    /// An explicit opt-in re-registers (self-heals a stale `Exec` path).
    #[test]
    fn explicit_opt_in_in_production_registers() {
        assert!(should_register(false, Some(true)));
    }

    /// Under e2e the hook must never write the machine's real autostart
    /// directory, whatever the choice says.
    #[test]
    fn e2e_never_registers_whatever_the_choice() {
        for choice in [None, Some(true), Some(false)] {
            assert!(!should_register(true, choice), "choice = {choice:?}");
        }
    }

    // --- should_start_hidden ---

    /// The point of the feature: an auto-started launch with somewhere to
    /// restore from stays out of the user's way.
    #[test]
    fn autostart_with_a_tray_host_starts_hidden() {
        assert!(should_start_hidden(true, true));
    }

    /// The stock-GNOME trap: no tray host ⇒ a hidden window would be
    /// unreachable, so start visible instead.
    #[test]
    fn autostart_without_a_tray_host_starts_visible() {
        assert!(!should_start_hidden(true, false));
    }

    /// A normal user-initiated launch always shows its window — the user just
    /// asked for it.
    #[test]
    fn a_normal_launch_is_never_hidden() {
        assert!(!should_start_hidden(false, true));
        assert!(!should_start_hidden(false, false));
    }

    // --- desktop_entry_contents ---

    /// The registered entry must carry the flag, or the sign-in launch would
    /// pop a window on every login — the regression that makes default-on
    /// autostart worse than shipping nothing.
    #[test]
    fn the_entry_execs_with_the_autostart_flag() {
        let entry = desktop_entry_contents("/usr/bin/fauna-desktop");
        assert!(
            entry.contains("Exec=/usr/bin/fauna-desktop --autostart"),
            "entry was: {entry}"
        );
    }

    #[test]
    fn the_entry_is_a_valid_enabled_desktop_file() {
        let entry = desktop_entry_contents("/usr/bin/fauna-desktop");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("Type=Application\n"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(entry.ends_with('\n'));
    }

    /// The flag the entry writes is the flag the next launch parses — pinned
    /// against the two drifting apart.
    #[test]
    fn the_written_flag_is_the_flag_argv_is_matched_against() {
        assert_eq!(AUTOSTART_FLAG, "--autostart");
        assert!(desktop_entry_contents("fauna-desktop").contains(AUTOSTART_FLAG));
    }

    // --- strip_autostart_flag ---

    /// The regression this exists for: GApplication rejects the unrecognised
    /// flag with "Unknown option --autostart" and refuses to start, so an
    /// unfiltered argv would break every desktop sign-in — the exact launch the
    /// feature exists for. Measured against the real binary 2026-07-16; delete
    /// the filter and the feature is worse than not shipping at all.
    #[test]
    fn the_autostart_flag_never_reaches_gtk() {
        let args = vec!["fauna-desktop".to_owned(), AUTOSTART_FLAG.to_owned()];
        assert_eq!(strip_autostart_flag(args), vec!["fauna-desktop".to_owned()]);
    }

    /// Filtering is surgical: argv[0] and every other argument survive, so
    /// removing the flag cannot change how anything else is parsed.
    #[test]
    fn stripping_leaves_every_other_argument_untouched() {
        let args = vec![
            "fauna-desktop".to_owned(),
            "--autostart".to_owned(),
            "--version".to_owned(),
            "positional".to_owned(),
        ];
        assert_eq!(
            strip_autostart_flag(args),
            vec![
                "fauna-desktop".to_owned(),
                "--version".to_owned(),
                "positional".to_owned(),
            ]
        );
    }

    /// A normal launch's argv passes through unchanged.
    #[test]
    fn a_normal_argv_is_unchanged() {
        let args = vec!["fauna-desktop".to_owned()];
        assert_eq!(strip_autostart_flag(args.clone()), args);
    }

    // --- the post-auth hook's composition: decision → entry on disk ---
    //
    // The gate tests above prove the *decision*; these prove the hook actually
    // acts on it. Without them the two halves could each be green while the
    // hook wrote nothing — the failure a "needs a real desktop session" framing
    // would have hidden.

    fn entry_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("autostart").join("fauna.desktop")
    }

    /// The out-of-the-box path: a user who never chose gets an entry written at
    /// the first login, creating the autostart directory if the desktop hasn't.
    #[test]
    fn the_hook_writes_an_entry_for_a_user_who_never_chose() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path(&dir);
        apply_registration_at(&path, false, None, "/usr/bin/fauna-desktop");
        let written = std::fs::read_to_string(&path).expect("the hook must write the entry");
        assert!(written.contains("Exec=/usr/bin/fauna-desktop --autostart"));
    }

    /// **The property the tri-state exists for**, end-to-end: a user who turned
    /// auto-start off stays off across every later login. A two-state
    /// file-existence check could not express this — the absent file *was* the
    /// opt-out, and the hook would recreate it on the spot.
    #[test]
    fn the_hook_never_re_registers_over_an_explicit_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path(&dir);
        apply_registration_at(&path, false, Some(false), "/usr/bin/fauna-desktop");
        assert!(!path.exists(), "an explicit opt-out must not be overridden");
    }

    /// An explicit opt-in writes, and re-writes with the *current* exe path —
    /// which is what self-heals an install that moved or upgraded.
    #[test]
    fn the_hook_rewrites_a_stale_exec_path_for_an_opted_in_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path(&dir);
        apply_registration_at(&path, false, Some(true), "/old/path/fauna-desktop");
        apply_registration_at(&path, false, Some(true), "/new/path/fauna-desktop");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("Exec=/new/path/fauna-desktop --autostart"));
        assert!(!written.contains("/old/path"), "stale path must be healed");
    }

    /// A harness login must never write the dev machine's autostart directory,
    /// whatever the stored choice says.
    #[test]
    fn the_hook_writes_nothing_under_e2e() {
        for choice in [None, Some(true), Some(false)] {
            let dir = tempfile::tempdir().unwrap();
            let path = entry_path(&dir);
            apply_registration_at(&path, true, choice, "/usr/bin/fauna-desktop");
            assert!(!path.exists(), "choice = {choice:?}");
        }
    }

    /// The hook is idempotent: every login runs it, so re-running must converge
    /// on one entry rather than accumulate or corrupt.
    #[test]
    fn re_running_the_hook_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path(&dir);
        apply_registration_at(&path, false, None, "/usr/bin/fauna-desktop");
        let first = std::fs::read_to_string(&path).unwrap();
        apply_registration_at(&path, false, None, "/usr/bin/fauna-desktop");
        assert_eq!(first, std::fs::read_to_string(&path).unwrap());
    }
}
