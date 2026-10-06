//! Hand a URL (or file path) to the OS default handler — the ONE opener
//! resolution every open-something-external call site shares: the media
//! handoff (`media/mod.rs`, gated by `tui-settings`' external-media mode),
//! the wizard's provider links (`wizard::open_in_browser`), a profile's
//! payment link, and a bridged notification's post on its network's website
//! (`notifications::open_notification`'s `External` arm). `tui.md`
//! § External media handoff: "the OS already owns 'which program plays
//! video'", and the same holds for "which program opens a URL", so fauna adds
//! no program-picker knob — the only injection point is the e2e override
//! below.
//!
//! What is media-specific is the *gate* (`settings::external_media_outcome`),
//! never the spawn — which is why this module is neutral: a wizard URL open
//! consults no media mode, but resolves the opener identically (three OS
//! arms; the old wizard copy lacked the Windows arm entirely, so
//! `dns-provider-link` / `vps-provider-link` silently never opened there).

/// The test/e2e opener override (`tui.md` § External media handoff). Set to a
/// fake handler binary so a test can prove the launch without a real player or
/// browser; the M6 media-open e2e uses it, and the spawn unit test injects the
/// path directly. Named for its original media consumer, but it overrides
/// EVERY `os_open` call site (the wizard's browser opens included) — one
/// opener, one override.
const ENV_OPENER: &str = "FAUNA_TUI_MEDIA_OPENER";

/// The Windows default handler is the `cmd` builtin `start`, not an
/// executable — so it rides `cmd /C start "" <url>` (see [`spawn_via`]).
const WINDOWS_START: &str = "start";

/// Hand `url` to the OS default handler. Fire-and-forget and
/// silent-on-failure by design — a headless box or missing handler just
/// leaves the on-screen fallback (painted link / metadata) as the way through.
pub fn open(url: &str) {
    // Fire-and-forget: the spawn result is dropped RIGHT HERE, on purpose.
    // `spawn_via` returns it only so the unit test can tell "the OS refused to
    // start the handler" apart from "the handler has not written yet" — a
    // distinction no caller has any use for.
    let _ = spawn_via(&resolve_opener(), url);
}

/// The spawn itself, with the opener injected — the unit test drives this with
/// a fake handler path, so it never sets a process-global env var.
///
/// Reports only whether the handler *process was created*; it is never awaited
/// (that is what fire-and-forget means), so a handler that starts and then
/// fails is still an `Ok` here. [`open`] discards this.
pub(crate) fn spawn_via(opener: &str, url: &str) -> std::io::Result<()> {
    // `xdg-open` / `open` / an injected fake take the url as a direct arg;
    // Windows's default `start` is a `cmd` builtin, so it rides `cmd /C start`.
    let mut cmd = if cfg!(target_os = "windows") && opener == WINDOWS_START {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new(opener);
        c.arg(url);
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The OS default handler program, or the [`ENV_OPENER`] override when set.
fn resolve_opener() -> String {
    opener_for_env(std::env::var_os(ENV_OPENER))
}

/// The opener the given env value implies — the OS default, or the override
/// when present and non-empty. Split out (pure, injectable) so the OS-default
/// vs override branch is unit-tested without touching the real environment.
pub(crate) fn opener_for_env(override_value: Option<std::ffi::OsString>) -> String {
    if let Some(value) = override_value.filter(|v| !v.is_empty()) {
        return value.to_string_lossy().into_owned();
    }
    if cfg!(target_os = "macos") {
        "open".to_string()
    } else if cfg!(target_os = "windows") {
        WINDOWS_START.to_string()
    } else {
        "xdg-open".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the opener resolver ──
    #[test]
    fn opener_prefers_the_override_when_set() {
        assert_eq!(
            opener_for_env(Some("/fake/handler".into())),
            "/fake/handler"
        );
        // An empty override falls through to the OS default.
        assert_ne!(opener_for_env(Some("".into())), "");
    }

    #[test]
    fn opener_defaults_to_the_os_handler() {
        let default = opener_for_env(None);
        let expected = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "start"
        } else {
            "xdg-open"
        };
        assert_eq!(default, expected);
    }

    // ── the real spawn actually launches the opener with the url (fake handler,
    //    no real player, no env var — the opener is injected) ──
    #[test]
    fn spawn_via_launches_the_opener_with_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("opened.txt");
        // A fake handler that records its argument (the url) to the sentinel.
        // Per-OS, because "a file the OS will execute" is: a `#!`-line shell
        // script with the exec bit on unix, and a `.cmd` batch on windows (a
        // `.sh` is not executable there at all — `spawn_via`'s fire-and-forget
        // spawn then fails silently and the sentinel never appears, which is
        // exactly what this test would report as a broken opener).
        #[cfg(unix)]
        let script = {
            let script = dir.path().join("fake-opener.sh");
            std::fs::write(
                &script,
                format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", sentinel.display()),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            script
        };
        #[cfg(windows)]
        let script = {
            let script = dir.path().join("fake-opener.cmd");
            // `<nul set /p "=…"` is batch's only newline-free write, so the
            // sentinel holds the url verbatim — the same byte-exact assertion
            // the unix arm makes with `printf`.
            std::fs::write(
                &script,
                format!(
                    "@echo off\r\n<nul set /p \"=%~1\">\"{}\"\r\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            script
        };

        let url = "https://example.test/clip.mp4";
        // Assert the OS actually created the handler process. `open()` drops
        // this by design, but a test that dropped it too would spend the whole
        // budget below and then report "the handler never ran" for what is
        // really "the spawn was refused" — a slow, misdirecting red.
        spawn_via(&script.to_string_lossy(), url).expect("the OS should have started the handler");

        // BOTH fake handlers publish the url through a `>` redirect — which
        // CREATES and truncates the sentinel *before* the write lands. So "the
        // sentinel exists" is not the state this test is waiting for: an
        // existence-poll can return inside that gap and read an empty file,
        // reporting a correctly-delivered url as `""`. That was the windows red
        // of 2026-07-31 — the spawn was healthy the whole time (a standalone
        // repro showed `%~1` receiving the url verbatim), while this loop
        // failed ~10% of runs here (3/60 and 6/40 measured A/B).
        //
        // So wait on the CONTENT — latency-independent state, per
        // `testing.md` § point 14 — with a budget far above any
        // non-pathological spawn: this box routinely runs 3–15 sessions, and a
        // full-suite run under that load once took >30 s just to schedule the
        // handler. A green run pays only until the bytes land, never the
        // budget.
        const OPENER_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
        let deadline = std::time::Instant::now() + OPENER_BUDGET;
        let seen = loop {
            // A read can also fail transiently while the handler still holds
            // the file open — that is "not ready yet", not a verdict.
            let seen = std::fs::read_to_string(&sentinel).unwrap_or_default();
            if seen == url || std::time::Instant::now() >= deadline {
                break seen;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };

        assert!(
            sentinel.exists(),
            "the fake opener should have run (no sentinel at {} after {OPENER_BUDGET:?})",
            sentinel.display()
        );
        assert_eq!(seen, url, "the opener is passed the url as its argument");
    }
}
