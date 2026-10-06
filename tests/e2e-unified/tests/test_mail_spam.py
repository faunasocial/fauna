"""E2E coverage for the user-facing mail-spam page.

Target state: docs/goal/behavior/mail-spam.md § Reset / § Cold start Path 2 /
§ Training-sample retention + § Undo (the page a user manages their own
per-account spam classifier from); UX/IDs: tests/e2e-unified/ui.yaml `mail-spam`
page + `mail-spam-training-history-list` component.

linux is the **lead** client for the mail-UX seed (tracked internally, § the
12 per-app seed tracks — the linux row); the shared
`fauna_client_mail_settings::MailSpamMachine` + the linux GTK page are the prior
art the other five apps lift.

The per-user Bayesian training-management backend is live: the
`fauna.bridges.{list_spam_training_history,reset_spam_model,put_spam_model,
set_baseline_contribution}` RPCs have real nest handlers, and the shared client
seam (`RpcMailSpamNest` in `libs/fauna-client-mail-settings/src/rpc_glue.rs`)
calls them through the user-tier `MailAccountClient`. These tests exercise the
full client→nest round-trip.

Every training-history row rests SEALED to the user's own recipient key
(`mail-spam.md` § Training-sample retention): the nest returns the opaque subject
with the display line degraded to the mailbox, and only the app — holding the
user's MSEK — unwraps it to `"<subject> · <mailbox>"`. Undo is client-side too:
the app unwraps the row's sealed delta, inverts it on its own sealed model and
deletes the row in one `put_spam_model` write. So every test that seeds rows
enables mail in the app first (`_seed`).

tier_3 (real client driver against a real fauna-nest binary): the training
history is seeded straight into the nest DB via `_seed_spam_training_history`, in
the sealed shape a capability holder writes (the page reads it back over the real
`list_spam_training_history` RPC — a direct DB SELECT, no cache), then the page
is driven and ground truth is asserted over the rendered list. Seeding the rows
directly (rather than driving an IMAP `\\Junk` train through the mail bridge)
keeps the arrange small while the read/reset/undo path under test stays fully
real.
"""

import pytest

from helpers.app_surface import app_name, declared_absence

# tier_3 (full stack, real binaries); linux is the lead, web has lifted the page
# (MailSpamSection.svelte). Windows lifts the same shared machine. android
# landed the page too (2026-06-02, `MailSpamScreen.kt` + `MailSpamVM.kt`,
# Compose-content-proven in `MailSpamContentTest.kt`) — an omission from this
# module's marker list, not a real gap, fixed here. tui
# renders the page in direct Rust off the same shared `MailSpamMachine`
# (2026-07-29 — the seventh and last app). Apple is included too. The reset test below used to skip_unbuilt on apple, since the
# reset confirm lived in a system `.confirmationDialog` unreachable to
# in-process automation — fixed: it is now
# the same two-click inline relabel every other app already uses on
# mail-spam-reset-model-button, so the click-through is driveable here too.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
]


def _seed_rows():
    """Two training-history rows for the seed — one spam (IMAP \\Junk flag), one
    ham (moved out of Junk). Distinct subjects so the rendered
    `mail-spam-training-history-list-item-message` (`"<subject> · <mailbox>"`,
    composed by the app once it unwraps the sealed subject) is unambiguous."""
    return [
        {
            "message_id": bytes([0xA1] * 32),
            "mailbox": "INBOX",
            "subject": "Cheap pills now",
            "label": "spam",
            "source": "imap_junk_flag",
        },
        {
            "message_id": bytes([0xB2] * 32),
            "mailbox": "Junk",
            "subject": "Lunch tomorrow?",
            "label": "ham",
            "source": "imap_junk_move",
        },
    ]


def _seed(app, nest_instance, test_user, seal_helper_binary, rows):
    """Seed `rows` as the logged-in user's training history, sealed to their own
    recipient key. Mail is enabled in the app first: that is what puts the key on
    file, and it is what lets the app unwrap each row's subject and delta (the
    idempotent confirm every mail-enabled test makes)."""
    from actions.mail_settings import MailSettingsActions
    from conftest import _seed_spam_training_history

    mail = MailSettingsActions(app.driver)
    mail.navigate()
    mail.ensure_mail_enabled()
    return _seed_spam_training_history(
        db_path=nest_instance["db_path"],
        actor_id=bytes.fromhex(test_user["actor_id_hex"]),
        seal_helper_binary=seal_helper_binary,
        rows=rows,
    )


@pytest.mark.feature("spam")
def test_mail_spam_page_reachable(logged_in_app):
    """The mail-spam page is reachable and renders its controls (reset button +
    contribute toggle + the training-history list).

    The page grew a report-sharing section (report-sharing.md § Client wire)
    between the contribute-baseline toggle and the training-history list, pushing
    the training-history list further down the page's ScrollViewer. A windows
    FlaUI `ScrollIntoView` sweep this deep has proven unreliable (an
    already-fragile mechanism this growth newly exposes — see
    e2e_status_and_tips.md § Windows); asserting mere *existence* via
    `driver.count` (no scroll, same idiom the other tests in this file already
    use via `history_count()`/`wait_for_history_count`) is enough to prove the
    controls render, without forcing the fragile deep scroll.
    """
    from actions.mail_spam import MailSpamActions

    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.is_page_visible(), "mail-spam page not reachable"
    logged_in_app.driver.wait_for("mail-spam-contribute-baseline-toggle", timeout=10.0)
    assert logged_in_app.driver.count("mail-spam-training-history-list") >= 1


@pytest.mark.feature("spam")
def test_mail_spam_history_renders_then_undo(
    logged_in_app, nest_instance, test_user, seal_helper_binary
):
    """Seeded sealed training events render in the list with their subjects, and
    per-row undo removes one (the real list_spam_training_history → app unwraps
    the sealed subject → render → app unwraps the sealed delta → put_spam_model
    {model undo + history delete} → re-read round-trip)."""
    from actions.mail_spam import MailSpamActions

    _seed(logged_in_app, nest_instance, test_user, seal_helper_binary, _seed_rows())
    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.is_page_visible()

    assert spam.wait_for_history_count(2), (
        f"seeded 2 training rows; list shows {spam.history_count()} "
        f"(error: {spam.error_text()!r})"
    )
    # The message is content-derived + deployment-uniform ("<subject> · <mailbox>"),
    # so it is safe to assert across clients (unlike the localized label/source
    # badges). The subject is in it only because the app unwrapped the sealed
    # subject under the user's own key — the nest alone returns the mailbox.
    messages = spam.history_messages()
    assert any("Cheap pills now" in m for m in messages), messages
    assert any("Lunch tomorrow?" in m for m in messages), messages

    # Undo the first (newest) row → the app inverts the sealed delta on its sealed
    # model and deletes the row atomically, so it drops out.
    spam.undo_training(0)
    assert spam.wait_for_history_count(1), (
        f"after undo, list shows {spam.history_count()} (expected 1; "
        f"error: {spam.error_text()!r})"
    )


@pytest.mark.feature("spam")
def test_mail_spam_reset_clears_history(logged_in_app, nest_instance, test_user, seal_helper_binary):
    """Reset deletes the per-user model + all training history (the real
    reset_spam_model → re-read round-trip): the seeded rows disappear."""
    from actions.mail_spam import MailSpamActions

    _seed(logged_in_app, nest_instance, test_user, seal_helper_binary, _seed_rows())
    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.is_page_visible()
    assert spam.wait_for_history_count(2)

    spam.reset_classifier()
    # The button's label says whether the second click confirmed or re-armed
    # (its arm expires on a wall-clock timer). On linux, the app's own
    # `[two-click] mail-spam-reset-model-button` lines in `app.err` say whether
    # the commit went out.
    assert spam.wait_for_history_count(0), (
        f"after reset, list shows {spam.history_count()} (expected 0; "
        f"error: {spam.error_text()!r}; "
        f"{logged_in_app.driver.diagnose('mail-spam-reset-model-button')})"
    )


@pytest.mark.feature("spam")
def test_mail_spam_contribute_toggle_round_trips(
    logged_in_app, nest_instance, test_user, seal_helper_binary
):
    """The contribute-baseline toggle round-trips through set_baseline_contribution
    + the list read-back: flipping it surfaces no error and the history list is
    re-read cleanly (the old stub returned an `unimplemented` rejection)."""
    from actions.mail_spam import MailSpamActions

    _seed(logged_in_app, nest_instance, test_user, seal_helper_binary, _seed_rows())
    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.wait_for_history_count(2)

    spam.toggle_contribute_baseline()
    # set_baseline_contribution succeeded ⇒ the post-set refresh re-read the list
    # (still 2) with no rejection surfaced.
    assert spam.wait_for_history_count(2), (
        f"after toggle, list shows {spam.history_count()} (expected 2; "
        f"error: {spam.error_text()!r})"
    )
    assert not spam.error_text(), f"toggle surfaced an error: {spam.error_text()!r}"


@pytest.mark.feature("spam")
def test_mail_spam_report_share_toggle_and_published_list(logged_in_app, nest_instance, test_user):
    """The report-sharing transparency pane (report-sharing.md § Client wire +
    transparency surface): the opt-in toggle round-trips, and the published list
    renders the ≥k=3 aggregate this nest exports to peers.

    Seeds the aggregate directly over WS-RPC (three freshly-minted, opted-in
    reporters flag one post as spam — mirrors
    tests/api/test_report_sharing.py's k-gate ramp) against the SAME
    session-shared nest `logged_in_app` is connected to (the entrust block's "the
    app's nest") — a single small aggregate, not the k-gate arithmetic itself, so
    the shared-nest footprint stays proportionate to what other GUI tests already
    do against it (mirrors the training-history seed above).
    """
    if app_name(logged_in_app.driver) == "web":
        declared_absence(
            logged_in_app.driver,
            capability="the report-sharing transparency pane (opt-in toggle + published list)",
            doc="report-sharing.md § Client wire + transparency surface (web MAY omit — user 2026-07-07)",
        )

    import time

    from common.auth import create_actor_and_register
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post
    from actions.mail_spam import MailSpamActions

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    reporters = [create_actor_and_register(port, admin_signing_key=admin_sk) for _ in range(3)]

    now_us = int(time.time() * 1_000_000)
    body = "Congratulations! You have won. Click http://spam.example.test to claim."
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, body, tags=[])
    post_id = ws_api.create_post(port, author, post_bytes)
    time.sleep(0.3)  # settle into content_meta before the aggregate attaches

    for reporter in reporters:
        assert ws_api.report_share_set(port, reporter, True)["share"] is True
        ws_api.moderation_train(port, reporter, post_id, "spam")

    # Poll the raw WS-RPC status first: separates "the nest aggregate hasn't
    # settled yet" (a shared-nest-under-load timing question, seen intermittently
    # on this suite regardless of client) from "the GUI failed to render an
    # already-ready aggregate" (the actual behavior this test proves).
    def _nest_side_entry():
        status = ws_api.report_share_status(port, reporters[0])
        return next((e for e in status.get("published", []) if e["content_hash"] == post_id), None)

    nest_deadline = time.time() + 10.0
    entry = _nest_side_entry()
    while entry is None and time.time() < nest_deadline:
        time.sleep(0.3)
        entry = _nest_side_entry()
    assert entry is not None, f"nest-side aggregate for post {post_id!r} never settled (report-sharing.md k-gate)"
    assert entry["count"] == 3

    # The logged-in actor's opt-in is SESSION-SHARED state: `test_user` is a
    # session-scoped fixture (conftest.py), so every app parametrization in one
    # run logs in as the same actor on the same nest — and this test's own last
    # act flips that actor's flag ON. Without this reset the second app in any
    # multi-app run (`--app ios,macos`, and every `--app sweep` baseline) walks
    # into the first app's ON and fails the default read below, which is exactly
    # the red that made `[macos]` look load-flaky in the 2026-08-27 baseline
    # while passing every solo run.
    # Establishing a precondition over the API is fixture setup, which
    # e2e-conventions.md convention 8 places outside its drive-through-the-UI
    # rule — the behavior under test, the toggle round-trip below, stays a pure
    # UI gesture. Done before the first navigate so the page's mount hydrate
    # reads the normalized value.
    ws_api.report_share_set(port, test_user, False)

    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.is_page_visible()

    # published is the whole-nest export view (same for every caller), so it's
    # visible before the logged-in user opts in themselves.
    def _post_row_visible() -> bool:
        return any(
            spam.published_hash(i) == post_id for i in range(spam.published_count())
        )

    # Re-navigating to the SAME sub-page while already on it can be a no-op (no
    # fresh Panel_Loaded), so force a real transition each retry: away to the
    # mail-settings parent, then back.
    deadline = time.time() + 15.0
    while time.time() < deadline and not _post_row_visible():
        time.sleep(0.3)
        logged_in_app.driver.set_state({"nav": {"stack": [{"view": "settings"}]}})
        spam.navigate()
    assert _post_row_visible(), (
        f"published list never rendered post {post_id!r}; "
        f"count={spam.published_count()} (error: {spam.error_text()!r}); "
        # count==0 alone cannot tell "the nest published nothing" from "the
        # section was never realized" — the registry dump separates them.
        f"{spam.published_diagnosis()}"
    )
    row = next(i for i in range(spam.published_count()) if spam.published_hash(i) == post_id)
    assert spam.published_factor(row) == "report:spam"
    assert spam.published_reporter_count(row) == "3"

    # The toggle round-trip: the logged-in user's OWN opt-in is independent of the
    # (already-published) aggregate above.
    assert not spam.is_share_reports_on(), "share-reports reads off before opt-in"
    spam.toggle_share_reports()
    deadline = time.time() + 10.0
    while time.time() < deadline and not spam.is_share_reports_on():
        time.sleep(0.3)
    assert spam.is_share_reports_on(), "share-reports toggle did not round-trip to on"
    assert not spam.error_text(), f"toggle surfaced an error: {spam.error_text()!r}"
