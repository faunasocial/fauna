"""Readers for `diagnose=` messages that quote a page the test is not on.

`e2e-conventions.md` § point 6 — *"failures must diagnose themselves"* — is why
these messages quote progress lines from elsewhere in the app at all: the
identity-succession aftermath runs four legs in one post-auth task, and which of
them settled is what separates *"this leg is broken"* from *"nothing downstream
ran"*. But the readers those messages call (`aftermath_*_status`) are
`is_visible` probes: they navigate nowhere, and an element on another page is
simply not visible, so they answer `""` rather than raising.

Every diagnostic runs immediately after a poll body that just left the app on
Backups, Nests or Conversations, so a bare read there could only ever report
`""` — which reads exactly like *"that leg never ran"*. The session was
handed one of those as its single discriminator and spent a run aimed at the
wrong candidate, while the app log for that very run showed both legs settling.

⚠ **Call these LAST in a diagnostic, after `error_text()`** — they navigate, and
`error-message` is per-page, so a read afterwards would report the (empty) error
of the page we navigated to rather than the one on the page that failed.
"""

from __future__ import annotations


def settings_line(app, reader: str) -> str:
    """Quote a Settings progress line — after **going to the page it lives on**.

    Never raises and never answers `""` for a read that did not happen: a
    diagnostic runs on the failure path, so an exception here would replace the
    assertion error it was meant to explain, and an empty string would recreate
    the original bug one layer down.
    """
    try:
        app.settings.open_recovery_kit()
        return getattr(app.settings, reader)()
    except Exception as exc:
        return f"<unreadable: {type(exc).__name__}>"


def backup_regrant_line(app) -> str:
    """Aftermath leg 2's progress line — the `NestBackupKey` re-grant and the
    destination re-registration.

    The discriminator leg 2's *own* journey polls on
    (`test_the_successors_backups_restart_without_a_user_command`), and the one
    a Backups-page diagnostic needs most: the destination list the page renders
    comes out of the account plane (a `fauna.state.backup` row), so an empty
    list means something different
    depending on whether this leg has settled. Not settled → the aftermath is
    still mid-window and the page is quoting a corpus that has not arrived yet;
    settled → the list really is empty at rest, which is a carry-across break.
    Reading `count=0` without this line cannot tell those apart.
    """
    return settings_line(app, "aftermath_backup_regrant_status")


def remint_line(app) -> str:
    """Aftermath leg 4's progress line — the capability-grant re-mint."""
    return settings_line(app, "aftermath_grant_remint_status")


def mail_burn_line(app) -> str:
    """Aftermath leg 6's progress line — the MSEK burn.

    ⚠ A NON-empty answer here is not by itself good news: the *failed* arm
    renders too, and it means the burn has **not** run (the predecessor's
    passwords still open the mailbox). A diagnostic quoting this line must
    therefore be read for *which* arm it is, never for presence — which is
    exactly why the caller prints the text rather than a boolean.
    """
    return settings_line(app, "aftermath_mail_burn_status")


# The `_status` readers above all quote the app's own RENDERED progress lines,
# which is exactly what makes them useless on a shell that has not built that
# render layer yet: apple paints none of the seven `recovery-kit-*-status`
# elements (`FaunaKit/Core/SuccessionAftermath.swift`'s `LoggingAftermathSink`
# logs them instead), so every one of them answers `""` there whether the leg
# settled, failed, or never ran. A diagnostic built only out of them therefore
# reports the SAME empty string for the two cases it exists to separate.
#
# The reader below is the witness that survives that: the aftermath's outcome
# reaches the app's log unconditionally on every native shell, so it answers
# "which arm did the pass take" even where nothing is painted.
# ⚠ Leg 3 logs from ANOTHER CRATE and matched none of these until 2026-08-31.
# It is the one leg that does not report through `run_succession_aftermath` — it
# is a barrier inside the replica's own `load()` — so its verdict rides
# `fauna_client_mls_sync::sync`, not `fauna_client_recovery::aftermath`. That is
# also the leg whose empty rendered line is most ambiguous (`NothingStored` and
# `AlreadyCurrent` both paint nothing, and so does a pass that never ran), so
# dropping it here blinded the reader exactly where it was needed most.
_AFTERMATH_LOG_MARKERS = (
    "aftermath",
    "succession",
    "fauna.recovery",
    "fauna_client_mls_sync",
    "__mls replica",
)

# ⚠ A blind tail is the wrong shape here and cost this reader its first run: the
# legs log in order, so the last N lines are the LAST legs — while the earliest
# legs scroll off the top. These are the lines that carry
# an arm rather than a "running" announcement, and they are few, so they are kept
# whole and the tail applies only to whatever is left over.
_AFTERMATH_VERDICT_MARKERS = (
    "settled",
    "succession aftermath:",
    # Leg 3's inputs, logged before they collapse into an outcome
    # (`fauna_client_mls_sync::succession`). Not a "settled" line, but the arm
    # it explains — `AlreadyCurrent` — carries no fields and paints nothing, so
    # this is the ONLY line separating "every slice was already ours" from
    # "the provider had no channels, nothing was examined". Kept whole rather
    # than left to the tail, where the legs that log after it would push it off.
    "examined the replica",
    "WARN",
    "ERROR",
)

# The web driver's leading "N earlier console line(s) EVICTED" warning
# (`drivers/web.py::console_log`). Matched on the verb alone so the count and
# the surrounding wording stay free to change.
_TRUNCATED_MARKER = "EVICTED"

# The app log is written for a terminal, so every line arrives wrapped in SGR
# escapes; left in, they turn a diagnostic into an unreadable wall (measured on
# this reader's first run). Stripped rather than rendered — a failure message is
# read out of a CI log as often as a tty.
_ANSI_RE = __import__("re").compile(r"\x1b\[[0-9;]*m")


def strip_ansi(line: str) -> str:
    """``line`` with every ANSI SGR escape removed — the one stripper the
    harness's log readers share."""
    return _ANSI_RE.sub("", line)


def _log_reader(app):
    """This driver's app-log surface, as a `() -> str`, or `None` if it keeps none.

    Dispatch is by CAPABILITY, never by driver type (convention 3) — but the
    capability is not one method: the native shells hand out the process's
    stderr, and web is a browser whose log arrives as the captured console ring
    (`drivers/web.py::console_log`, fed by every `tracing` event through
    `libs/fauna-wasm/src/logs.rs` § Browser-console layer). Reading only the
    first is how web came to report `<no app-log reader on this driver>` while
    holding the witness the whole time, and it cost the leg-3 investigation its
    discriminator.

    The call itself is deliberately left to the caller's `try` — a reader that
    raises must be reported as a fault, not mistaken for a driver that has none.
    """
    stderr = getattr(app.driver, "app_stderr_text", None)
    if callable(stderr):
        return lambda: stderr() or ""
    console = getattr(app.driver, "console_log", None)
    if callable(console):
        return lambda: "\n".join(console() or [])
    return None


def row_judge_log_lines(app, limit: int = 4) -> str:
    """The shared row judge's refusals for this launch — the other reason a
    listing reads empty.

    A projection reader (`fauna_client_sync::row_judge`) drops every Media item
    or version whose writer signature does not verify, warning once per row
    (`a row did not verify — treated as absent error=…`). An empty Media page
    then looks exactly like a label plane that never opened, so a names
    assertion quotes these lines to tell the two apart: measured 2026-09-30, a
    successor's inherited corpus read empty on tui and windows alike with
    `no cert for delegated signer` on every predecessor-signed row.
    """
    reader = _log_reader(app)
    if reader is None:
        return "<no app-log reader on this driver>"
    try:
        text = reader()
    except Exception as exc:  # never mask the real failure with a reader fault
        return f"<unreadable: {type(exc).__name__}: {exc}>"
    hits = [
        _ANSI_RE.sub("", ln).strip()
        for ln in text.splitlines()
        if "row_judge" in ln and ("did not verify" in ln or "cannot be judged" in ln)
    ]
    if not hits:
        return "<no row-judge refusal in this launch's log>"
    return "\n      ".join([f"({len(hits)} refusal line(s); last {min(limit, len(hits))})"] + hits[-limit:])


def aftermath_log_lines(app, limit: int = 12) -> str:
    """The aftermath's own log lines for this launch — the unpainted witness.

    ⚠ **Read this BEFORE concluding anything from an empty `*_line` reader.**
    The FFI apps log the pass's two-valued outcome unconditionally
    (`succession aftermath: notASuccessor|ran`, `SuccessionAftermath.swift`),
    and `notASuccessor` is the one answer that
    looks identical to a healthy "nothing to do" from the outside while meaning
    the predecessor LINK is missing and **no leg ran at all**
    (`identity-succession.md`'s ⚠ on `record_succession`: an app that persists
    the seed alone answers "no predecessors" to every aftermath consumer, each
    of which then degrades *quietly*). Web's own history is why that line
    exists; this reader is why a test can see it.

    Portable by capability, never by driver type (convention 3): the native
    shells expose `app_stderr_text`, web is a browser and has none, and a
    missing reader is reported as such rather than as an empty result — an
    absent witness and a silent one must not read alike.
    """
    reader = _log_reader(app)
    if reader is None:
        return "<no app-log reader on this driver>"
    try:
        text = reader()
    except Exception as exc:  # never mask the real failure with a reader fault
        return f"<unreadable: {type(exc).__name__}: {exc}>"
    hits = [
        _ANSI_RE.sub("", ln).strip()
        for ln in text.splitlines()
        if any(m in ln.lower() for m in _AFTERMATH_LOG_MARKERS)
    ]
    # A truncation warning is not an aftermath line and matches no marker, but
    # it is the one line that changes how every other line may be read: this
    # reader's whole job is to let a caller conclude something from what is NOT
    # here, and a ring that dropped its head makes that conclusion unsound.
    # Carried whole, ahead of the limit.
    truncation = [
        _ANSI_RE.sub("", ln).strip() for ln in text.splitlines() if _TRUNCATED_MARKER in ln
    ]
    if not hits:
        return "\n      ".join([""] + truncation) if truncation else (
            "<no aftermath line in this launch's log>"
        )
    verdicts = [ln for ln in hits if any(m in ln for m in _AFTERMATH_VERDICT_MARKERS)]
    rest = [ln for ln in hits if ln not in verdicts]
    kept = verdicts + rest[-max(0, limit - len(verdicts)):] if limit > len(verdicts) else verdicts
    return "\n      ".join([""] + truncation + kept)


# What the account runtime says about itself, and the one line a dead wasm task
# leaves (`[pageerror]`). The successor's inherited preference values arrive
# through its own walk's carry (`config-dissolution.md`, *What replaces the
# bridge's two carriages*), so a read that stays empty is explained here or
# nowhere.
_ACCOUNT_PLANE_LOG_MARKERS = (
    "account runtime",
    "account pump",
    "engine singleton",
    "[pageerror]",
)


def account_plane_log_lines(app, limit: int = 40) -> str:
    """The account runtime's own log lines for this launch, with its pump role
    and pass counters — what a plane-rail read that stays empty needs beside it.

    Same capability dispatch and truncation rule as :func:`aftermath_log_lines`.
    """
    from helpers.waiting import account_pump_cycles, account_pump_role

    try:
        role = account_pump_role(app.driver)
        cycles = account_pump_cycles(app.driver)
    except Exception as exc:  # never mask the real failure with a reader fault
        role = cycles = f"<unreadable: {type(exc).__name__}: {exc}>"
    head = f"(runtime_up, is_holder)={role!r} (started, completed)={cycles!r}"
    reader = _log_reader(app)
    if reader is None:
        return head + "\n      <no app-log reader on this driver>"
    try:
        text = reader()
    except Exception as exc:
        return head + f"\n      <unreadable: {type(exc).__name__}: {exc}>"
    lines = [_ANSI_RE.sub("", ln).strip() for ln in text.splitlines()]
    truncation = [ln for ln in lines if _TRUNCATED_MARKER in ln]
    hits = [ln for ln in lines if any(m in ln for m in _ACCOUNT_PLANE_LOG_MARKERS)]
    if not hits:
        hits = ["<no account-runtime line in this launch's log>"]
    return "\n      ".join([head] + truncation + hits[-limit:])
