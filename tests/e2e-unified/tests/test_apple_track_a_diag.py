"""tier_3 macOS DIAGNOSTICS for the two apple-requested TRACK A probes.

These are **not** baseline gates — they produce the in-process datum apple needs
to unblock two of its render-gated reds whose first-guess hypotheses it already
disproved by code-read. Each prints a labelled trace (grep ``[DIAG-DRAFTS]`` /
``[DIAG-EVENTS]`` out of the build-log flood) and asserts only its preconditions,
so a real gap surfaces as DATA in the trace rather than a red that muddies the
baseline (the ``test_events_diag.py`` diagnostic precedent).

Run: ``pytest tests/e2e-unified/tests/test_apple_track_a_diag.py --client macos``
at LOW load (macOS VM near-full + tier_3 — check ``df -h .`` first).

Re-run both when apple lands a draftsSync / events-registration fix: the VERDICT
line flips, telling apple (and the next harness session) exactly what changed.
"""

import hashlib
import time

import pytest

# `drafts_production_window`: step 2 polls the nest for the DEBOUNCED put, so
# this probe needs the production window to fire on its own — see that marker's
# entry in pytest.ini.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.drafts_production_window,
]


# ── Drafts SAVE-vs-RESTORE isolation ──────────────────────────────────────────
#
# Apple disproved its first hypothesis ("the local `$body_` never mutates the
# manager snapshot"): `NewThreadComposeForm.swift:45` DOES forward
# `vm.setNewThreadBody` → manager → `draftsSnapshotBytes` includes `new_thread`.
# So the open question is purely *where* the chain breaks: does the debounced
# `fauna.drafts.put` actually reach the nest `__drafts` plane (SAVE side), and if
# so does the relaunched app's launch-time `restoreDrafts` repaint the composer
# (RESTORE side)? This probe answers both by reading the nest's per-actor drafts
# blob via a side-channel WS-RPC client for the SAME actor the macOS app is logged
# in as (the cross-device read proven in `tests/api/test_drafts_sync.py`).
#
# The blob is sealed under the actor's BackupKey, so we cannot read the plaintext
# body — but presence + change-from-baseline is a faithful SAVE-side signal, and
# the composer read-back (`compose_body_text`) is the RESTORE-side signal.


def _blob_summary(reply) -> str:
    blob = reply.get("blob") if isinstance(reply, dict) else None
    if blob is None:
        return "None (no blob persisted)"
    b = bytes(blob)
    return f"{len(b)} bytes sha256={hashlib.sha256(b).hexdigest()[:16]}"


def _wait_compose_body(app, expected: str, timeout: float = 6.0) -> str:
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        try:
            last = app.conversations.compose_body_text()
        except Exception as e:  # noqa: BLE001 — diagnostic; never mask
            last = f"<{type(e).__name__}: {e}>"
        if last == expected:
            return last
        time.sleep(0.2)
    return last


def _nest_drafts_log_tail(log_path: str | None, n: int = 30) -> str:
    if log_path is None:
        return ("(nest log not available in this nest mode — `log_path` is the "
                "standalone provider's key; a container's log lives in the daemon)")
    try:
        from pathlib import Path
        lines = Path(log_path).read_text(errors="replace").splitlines()
    except Exception as e:  # noqa: BLE001
        return f"<could not read {log_path}: {e}>"
    hits = [ln for ln in lines if "draft" in ln.lower()]
    return "\n".join(hits[-n:]) if hits else "(no 'draft'-matching nest-log lines)"


def test_drafts_save_restore_isolation_macos(logged_in_app, nest_instance, test_user):
    """Isolate the macOS new-thread-draft persistence gap to SAVE vs RESTORE."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])
    # `.get`, not a subscript: `log_path` is the STANDALONE provider's key and
    # `nest_instance` is mode-routed, so a subscript here raises `KeyError` at
    # the TOP of the body — before any assertion — in every other mode. It only
    # ever feeds a diagnostic print, which now says so itself.
    log_path = nest_instance.get("log_path")

    def drafts_get(path: str = "conversations") -> dict:
        """Read the actor's drafts blob via a fresh side-channel device."""
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            return dev.call("fauna.drafts.get", {"path": path})

    def wait_blob_change(baseline_blob, timeout: float = 10.0):
        """Poll the nest until the drafts blob appears / changes from baseline."""
        deadline = time.time() + timeout
        last = {}
        while time.time() < deadline:
            last = drafts_get()
            blob = last.get("blob")
            if blob is not None and blob != baseline_blob:
                return last, True
            time.sleep(0.5)
        return last, False

    draft_body = "macOS new-thread draft must survive a restart 4827"

    base = drafts_get()
    base_blob = base.get("blob")
    print(f"[DIAG-DRAFTS] step0 baseline drafts.get(conversations) = {_blob_summary(base)}")

    # 1. Open the new-thread composer and type a draft body (same chokepoint the
    #    web leg exercises: navigate → new-conversation-button → dm-text-field).
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)
    typed = _wait_compose_body(app, draft_body)
    print(f"[DIAG-DRAFTS] step1 composer body after type = {typed!r}")
    assert typed == draft_body, (
        f"precondition: the draft must type into the macOS composer "
        f"(got {typed!r}, error={app.error_text()!r})"
    )

    # 2. SAVE side — poll the nest until the debounced put lands (or times out).
    after_type, save_works = wait_blob_change(base_blob, timeout=10.0)
    print(f"[DIAG-DRAFTS] step2 SAVE-side drafts.get(conversations) = {_blob_summary(after_type)}")
    print(f"[DIAG-DRAFTS] SAVE verdict: "
          f"{'PERSISTED (blob reached nest)' if save_works else 'NOT PERSISTED (save-side gap)'}")

    # 3. Restart the app (force-quit + relaunch, identity replayed) and re-open
    #    conversations; give the relaunched manager time to restoreDrafts.
    app.driver.hard_reload()
    app.conversations.navigate()
    time.sleep(2.0)
    after_reload = drafts_get()
    print(f"[DIAG-DRAFTS] step3 post-reload drafts.get(conversations) = {_blob_summary(after_reload)}")

    # 4. RESTORE side — re-open the composer; start_new_conversation PRESERVES a
    #    restored new-thread draft, so the body must reappear if restore works.
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    restored = _wait_compose_body(app, draft_body)
    restore_works = restored == draft_body
    print(f"[DIAG-DRAFTS] step4 RESTORE-side composer body = {restored!r}")
    print(f"[DIAG-DRAFTS] RESTORE verdict: {'RESTORED' if restore_works else 'NOT RESTORED'}")

    if save_works and restore_works:
        verdict = "BOTH OK — macOS new-thread draft persists end-to-end"
    elif save_works and not restore_works:
        verdict = ("SAVE OK / RESTORE BROKEN — blob reaches the nest __drafts plane but the "
                   "relaunched app does not repaint the composer (restoreDrafts wiring or "
                   "start_new_conversation re-seed clobbering the restore)")
    else:
        verdict = ("SAVE BROKEN — the draft never reaches the nest __drafts plane (debounced "
                   "fauna.drafts.put not firing, or draftsSync handle not wired at login)")
    print(f"[DIAG-DRAFTS] FINAL VERDICT: {verdict}")
    print(f"[DIAG-DRAFTS] nest-log 'draft' tail:\n{_nest_drafts_log_tail(log_path)}")


# ── Events `new-calendar-btn` registration probe ─────────────────────────────
#
# Apple says the button LOOKS correct (`MacCalendarListView.swift:35-40` carries
# BOTH `.accessibilityIdentifier` AND `.automationActivate`), so the 9-red events
# cluster is NOT a missing-modifier gap — likely a view-lifecycle/registration
# issue. This probe answers: is the events tab even rendered, and is
# `new-calendar-btn` in the in-process registry at all?


def test_events_new_calendar_btn_registration_macos(logged_in_app):
    """Probe whether the macOS events tab renders and `new-calendar-btn` registers."""
    app = logged_in_app
    driver = app.driver

    try:
        driver.navigate_to("events")
    except Exception as e:  # noqa: BLE001 — capture the nav failure as data
        print(f"[DIAG-EVENTS] navigate_to('events') raised: {type(e).__name__}: {e}")
    time.sleep(2.0)

    state = driver.get_state() or {}
    nav_view = None
    try:
        nav_view = state.get("nav", {}).get("stack", [{}])[0].get("view")
    except Exception:  # noqa: BLE001
        pass
    err = None
    try:
        err = state.get("messages", {}).get("error")
    except Exception:  # noqa: BLE001
        pass
    print(f"[DIAG-EVENTS] nav.view = {nav_view!r}  (events tab rendered? expect 'events')")
    print(f"[DIAG-EVENTS] messages.error = {err!r}")

    # Per-element registry snapshot — visible / count / text for each.
    for eid in (
        "page-heading",
        "calendar-view-agenda",
        "events-view-toggle",
        "new-calendar-btn",     # the gated button
        "new-event-btn",
        "calendar-item",
        "calendar-name",        # create-calendar form field (below the fold?)
        "create-calendar",      # create-calendar confirm button
        "calendar-visibility",  # per-calendar visibility toggle
        "error-message",
    ):
        print(f"[DIAG-EVENTS] {driver.diagnose(eid)}")

    # Narrow in-process tree (focused/detail pane on apple) — last so a tree
    # failure can't suppress the per-element data above.
    try:
        print(f"[DIAG-EVENTS] tree:\n{driver.tree()}")
    except Exception as e:  # noqa: BLE001
        print(f"[DIAG-EVENTS] tree() raised: {type(e).__name__}: {e}")

    # Diagnostic only — assert nothing about the gap; the trace IS the deliverable.
    # (A bare always-pass keeps it off the baseline as a clean diagnostic.)
    assert True


# ── mail MUA-instructions container-vs-child registration probe ───────────────
#
# After apple's status-indicator fix, the macOS mail
# credential cluster flipped 5/6 green, but `enable_populates_mua_instructions`
# stays red: `mail-settings-mua-instructions` (an 8-child container) never goes
# visible after enable, while its page-siblings (credential-row, keys-info,
# rotate) DO register. This isolates whether the CONTAINER id is absent while its
# CHILDREN register (the macOS Section/container-clobber pattern → move the
# sentinel to a leaf or add `.accessibilityElement(children: .contain)`) or the
# whole block fails to render.


def test_mail_mua_instructions_registration_macos(logged_in_app):
    """Probe the macOS `mail-settings-mua-instructions` container vs its children."""
    app = logged_in_app
    driver = app.driver

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    time.sleep(1.5)  # let the async EnableMail round-trip settle

    print(f"[DIAG-MUA] CONTAINER {driver.diagnose('mail-settings-mua-instructions')}")
    for child in (
        "mail-settings-mua-imap-host",
        "mail-settings-mua-imap-port",
        "mail-settings-mua-smtp-host",
        "mail-settings-mua-smtp-port",
        "mail-settings-mua-caldav-host",
        "mail-settings-mua-username-format",
        "mail-settings-mua-auth-mechanism",
    ):
        print(f"[DIAG-MUA] child   {driver.diagnose(child)}")
    # Page-siblings that DID register (the 5/6 that pass) — for contrast.
    for sib in ("mail-settings-keys-info", "mail-settings-credential-item-name"):
        print(f"[DIAG-MUA] sibling {driver.diagnose(sib)}")

    # Diagnostic only — the trace IS the deliverable.
    assert True


# ── Body-text markdown read-uniformity probe ─────────
#
# apple swapped the 5 body automation reads to
# `renderDocumentToPlaintext(document:)` expecting
# `test_conversations_markdown::test_detail_renders_markdown_on_select` (DM) and
# `test_feed::test_post_body_renders_markdown` (FEED) to go GREEN on macOS. They
# did NOT — both reads return the RAW markdown source (`**bold**`). The FFI strips
# correctly (`libs/fauna-ffi/src/render.rs` unit test) and the shared document
# builders markdown-parse, so the document reaching each read must be UNPARSED.
# This probe pins WHERE, per path, and re-arms (the verdict flips when apple fixes
# the construction):
#   * DM   — the macOS inject handler (`Fauna-macOS/App/FaunaMacApp.swift:924`)
#            AND the iOS one (`Fauna-iOS/App/FaunaApp.swift:570`) hard-code
#            `bodyFormat: .plainText`, whereas linux (`main.rs:2076`) and wasm/web
#            (`conversations.rs:792`) derive `match rail { FaunaMls => Markdown,
#            _ => PlainText }`. So the injected FaunaMls message's `document` is
#            `plaintext_to_document(body)` → a flat Text node → raw `**`.
#   * FEED — the failure is NOT a document-construction gap: shared
#            `build_post_document` ALWAYS `markdown_to_document`s
#            (`libs/fauna-feed/src/manager.rs:735`). The macOS `feed-post-text`
#            element never registers in-process (so the painted-`document` read
#            apple added is never exercised), and the harness
#            falls back to STATE reads on macOS (`actions/feed.py`
#            `_use_state_for_feed_reads` → `first_post_text` returns
#            `posts[0]["body"]`). That state field is serialized RAW
#            (`Fauna-macOS/App/FaunaMacApp.swift:608` → `"body": p.body`), so the
#            `"**" not in rendered` assert reads raw source and fails regardless
#            of the element swap. This probe records both the element (absent) and
#            the state read (raw) so the verdict pins the real cause and re-arms.


def test_body_text_markdown_documents_macos(logged_in_app):
    """Pin why the macOS body-text markdown reads return raw `**` per path (DM/feed)."""
    app = logged_in_app
    driver = app.driver
    conv = app.conversations

    # ── DM leg: inject a FaunaMls **bold** message, open it, read dm-message-text ──
    conv.inject_and_open_thread(
        rail="FaunaMls", sender="bob-diag@self-nest.test", subject=None, body="**bold** body"
    )
    print(f"[DIAG-BODYMD] DM {driver.diagnose('dm-message-text')}")
    dm_text = (
        driver.get_text("dm-message-text", index=0)
        if driver.count("dm-message-text")
        else "<absent>"
    )
    print(f"[DIAG-BODYMD] DM dm-message-text[0] = {dm_text!r}")
    dm_verdict = (
        "STRIPPED (document markdown-parsed — fix landed)"
        if "**" not in dm_text
        else "RAW ** — injected document is plaintext (inject handler bodyFormat: .plainText)"
    )
    print(f"[DIAG-BODYMD] DM verdict: {dm_verdict}")

    # ── FEED leg: create a **bold** post; record BOTH the element (apple's
    #    painted-document read) AND the state read the harness actually
    #    uses on macOS (`first_post_text` → state `posts[0]["body"]`, raw source). ──
    import uuid

    driver.navigate_to("feed")
    suffix = uuid.uuid4().hex[:8]
    app.feed.create_post(text=f"**bold{suffix}**")
    print(f"[DIAG-BODYMD] FEED feed-post-text element {driver.diagnose('feed-post-text')}")
    fpt_n = driver.count("feed-post-text")
    fpt_text = driver.get_text("feed-post-text", index=0) if fpt_n else "<absent>"
    state_text = app.feed.first_post_text()  # what test_post_body_renders_markdown reads
    print(f"[DIAG-BODYMD] FEED feed-post-text[0] (element) = {fpt_text!r}")
    print(f"[DIAG-BODYMD] FEED first_post_text() (STATE read, what the test asserts) = {state_text!r}")
    if "**" not in state_text:
        feed_verdict = "STRIPPED (state body now painted-plaintext — fix landed)"
    elif fpt_n == 0:
        feed_verdict = (
            "RAW ** — feed-post-text element ABSENT (never registers in-process on "
            "macOS), so harness reads STATE body which is raw source "
            "(FaunaMacApp.swift:608 `\"body\": p.body`). Apple's element-swap is "
            "bypassed; fix = register feed-post-text OR push painted plaintext in state."
        )
    else:
        feed_verdict = (
            "RAW ** while feed-post-text registers — element read not stripping "
            "(unexpected; re-check the element automationValue swap)"
        )
    print(f"[DIAG-BODYMD] FEED verdict: {feed_verdict}")

    # Diagnostic only — the trace (DM + FEED verdicts) IS the deliverable.
    assert True
