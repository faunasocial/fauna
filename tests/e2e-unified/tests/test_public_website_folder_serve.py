"""A public website folder serves its own bytes — the app-driven exit proof.

Folders re-model **phase 4 slice 4e**. This is the latent-404 closure reached
the way a user reaches it, and it is the tier_3 proof
``web-content-hosting.md`` § Implementation status today names as deferred: the
web-sync tier had only *nest-side* coverage, so nothing proved that a file a
user drops into a folder on their own machine ends up served at their own
address.

The chain under test, end to end, all of it production:

    the folders page's audience control  (slice 4d)
      → FoldersClient::set_audience                 → folders.audience = 'public'
      → the folders page's website toggle (slice 4d)
      → FoldersClient::set_website_enabled          → folders.website_enabled = 1
      → a bound location + a real fauna-sync-agent  → the byte plane, PLAINTEXT
      → the nest's Host-routed serve chain          → GET / on <handle>.<domain>

Each link already had its own coverage and none of it composed:
``tests/api/test_web_folder_audience.py`` pins the nest half of the audience
flip, ``test_web_subdomain_hosting`` pins the Host-routed serve, and
``test_folder_agent_content_sync`` pins app→agent→nest upload. What no test
covered is that the bytes the agent uploads are the bytes the web serve returns
— which is precisely where the public audience earns its keep, because a
**sealed** upload would serve ciphertext or 404 and every one of those three
tests would still pass.

**Ordering is load-bearing, and deliberate.** The folder is made public and
website-enabled *before* anything is written into it, so the corpus is plaintext
from its first chunk and no re-seal pass is ever owed
(``folders.md`` § Target re-model — the born-public shape). Flipping *after* an
upload is a different mechanism — the declassify back-catalogue pass (slice 4b,
``SyncEngine::declassify_owner_corpus``) — which has its own coverage and would
turn a failure here into an ambiguous result. The audience reaches a running
seat's write path within one rescan tick in both directions (slice 4c), and the
binding here is added after the flip, so the engine is built already armed.

⚠ The spec for this slice said "born-public", meaning a folder created with
``FolderCreateRequest.audience`` set. That wire door is real and pinned
nest-side, but it is **not** how a user reaches it: the rule-A grant for slice
4d deliberately minted **no wizard IDs** — "a website folder is created as an
ordinary folder, then toggled on its row" — so the app path is create → make
public → serve, and that is what an app-driven proof has to drive.
"""

import secrets
import urllib.error
import urllib.request

import pytest

from conftest import MAIL_PRIMARY_DOMAIN

from helpers.folder_content import (
    SYNC_WINDOW_SECS as _SYNC_WINDOW_SECS,
)
from helpers.folder_content import (
    agent_diagnosis as _agent_diagnosis,
)
from helpers.folder_content import (
    atomic_write as _atomic_write,
)
from helpers.folder_content import (
    await_agent_upload as _await_agent_upload,
)
from helpers.folder_content import (
    bind_location_under_set as _bind_location_under_set,
)
from helpers.waiting import wait_until

# tui + linux + macos + windows: all four carry the whole chain — the slice-4d
# audience controls (linux joined 2026-08-27; macos widened 2026-09-02, row 283
# — macOS ships a bundled fauna-sync-agent, docs/goal/architecture/installers/
# macos.md:786,789; windows widened 2026-09-07, row 168 — windows ships
# fauna-sync-agent.exe, docs/goal/architecture/installers/windows.md:24,121,158)
# and a direct-spawned real fauna-sync-agent (`test_folder_agent_content_sync.py`
# is the linux upload proof these steps ride). A red on a newly covered app is a
# genuine product bug there, not test breakage. iOS is deliberately NOT here — it
# has no bound-location sync agent by design (`docs/goal/architecture/apps/
# sync-agent.md` § Scope per platform: "iOS entirely — no daemons"), the same
# declared absence `docs/features/local-folder-sync.md` front matter already
# carries; adding it here would contradict that and the cell-semantics rule that
# a declared absence may not also carry a passing witness. android is the
# remaining column still lacking the bound-location byte plane.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
]

# The scan cadence is no per-folder choice since phase 5 (2026-08-20): every
# e2e launch ticks at the harness's 30 s `FAUNA_E2E_RESCAN_MS` default
# (`drivers/tui.py` / `drivers/linux.py`; the compile-gated seam is
# `always_resident::rescan_interval`), so a scan-driven path still fires inside
# the poll windows below — the cadence the retired wizard picker used to set.

# Generous ceilings, not expectations (convention 14). The serve wait covers the
# nest's own web_files fan-out after the upload lands, which is a separate
# ingest step from the upload the agent reports.
_SERVE_WINDOW_SECS = 180.0
_FLAG_WINDOW_SECS = 60.0
# Per-request ceiling for the serve probe itself, distinct from the window we
# keep retrying it over: a single GET that hangs must not eat the whole budget.
_SERVE_REQUEST_TIMEOUT_SECS = 15.0


def _get_root_with_host(url: str, host: str) -> tuple[int, str]:
    """``GET /`` on the nest with an explicit ``Host`` header, returning
    (status, body_text).

    The serve chain routes off ``Host``, so this is how a localhost-bound tier_3
    nest is asked for ``https://<handle>.<domain>/``. Mirrors
    ``test_web_subdomain_hosting``'s helper of the same name — same chain, and
    deliberately not shared into a helper module until a third caller wants it.
    """
    req = urllib.request.Request(
        url.rstrip("/") + "/", method="GET", headers={"Host": host}
    )
    try:
        resp = urllib.request.urlopen(req, timeout=_SERVE_REQUEST_TIMEOUT_SECS)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def _folder_row(nest_url: str, actor, name: str) -> dict | None:
    """That actor's own ``fauna.folders.list`` row for ``name`` — the NEST's
    ground truth, never the app's rendering, so a UI that paints an optimistic
    flip cannot make this pass."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    ) as c:
        reply = c.call("fauna.folders.list", {})
    # By hash: a sealed set's row rests no plaintext name (schema 114).
    from helpers.set_names import find_set

    return find_set(reply.get("folders", []), name)


def _await_folder_flags(
    nest_url,
    actor,
    name: str,
    *,
    audience: str | None = None,
    website_enabled: bool | None = None,
    window: float = _FLAG_WINDOW_SECS,
) -> dict:
    """Poll the nest's own folder row until the named flags hold.

    Latency-independent (convention 14): a deadline poll on *state*, with a
    ceiling far above any non-pathological delay, never a settle-sleep. The
    failure names both the wanted and the observed row, because the two
    interesting ways this fails — the gesture never reached the nest, and the
    nest refused the transition — are distinguishable only from the row itself.
    """
    def _matches():
        row = _folder_row(nest_url, actor, name)
        if row is None:
            return None
        if audience is not None and row.get("audience") != audience:
            return None
        if (
            website_enabled is not None
            and bool(row.get("website_enabled")) is not website_enabled
        ):
            return None
        return row

    want = {"audience": audience, "website_enabled": website_enabled}
    return wait_until(
        _matches,
        window,
        interval=1.0,
        diagnose=lambda: (
            f"folder {name!r} never reached {want} — the nest's own row is "
            f"{_folder_row(nest_url, actor, name)!r}. An unchanged row means the "
            f"gesture never reached fauna.folders.update; a row that moved only "
            f"partway means the nest refused one of the two transitions."
        ),
    )


@pytest.mark.parametrize(
    "folder_share_owner_app", ["tui", "linux", "macos", "windows"], indirect=True
)
@pytest.mark.real_conversations
# real_sync_agent added row 283 (macOS widening): on macOS/windows the real
# fauna-sync-agent is a session-patch opt-in gated by FaunaMacApp's
# `FaunaE2E.realSyncAgent` / windows' `FAUNA_E2E_REAL_SYNC_AGENT` (set by
# conftest's `_apply_real_sync_agent_env` only when the collected session
# carries this marker) — without it `startSyncAgentProvisioner` /
# `HydrationSessionService` is never called at all, so a bound location never
# spawns an agent and the upload wait times out. linux spawns its agent
# unconditionally, so the marker is a no-op there.
@pytest.mark.real_sync_agent
# windows widened row 168: without this the app-spawned agent rendezvouses on
# the machine-global per-SID pipe (`\\.\pipe\fauna-sync.<SID>`) instead of a
# run-private one — a convention-10 breach. No-op on tui/linux/macos.
@pytest.mark.isolated_sync_agent
# Documented-long (testing.md § point 9: bounded always, unbounded never). One
# GUI app + one real sync agent, a real engine build after the binding, an
# upload cycle, and the nest's own web fan-out after it. A CEILING, not an
# expectation — a healthy run is far shorter.
@pytest.mark.timeout(1200)
@pytest.mark.feature("public-folders-and-websites")
def test_public_website_folder_serves_the_file_a_user_dropped_in_it(
    folder_share_owner_app, tmp_path
):
    """A file dropped into a public website folder is served at the owner's
    address, with its own bytes.

    The assertion that matters is the **body**, not the status: a 200 carrying
    the nest's info page (or anyone else's content) would mean the route
    resolved but the folder's own file never reached the web plane, which is the
    exact latent failure this slice exists to close.
    """
    app, nest, owner = folder_share_owner_app
    nest_url = nest["url"]
    handle = owner["handle"]

    set_name = f"site-{secrets.token_hex(4)}"
    marker = f"public-site-{secrets.token_hex(6)}"
    body_html = f"<!doctype html><title>{marker}</title><h1>{marker}</h1>\n"

    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    # ── 1. Make it public — the slice-4d gesture, confirm and all ────────────
    #
    # `make_public` asserts the declassify confirm actually armed, so a
    # regression that published on the bare select fails HERE rather than
    # silently passing the rest of the test.
    b.make_public(set_name)
    _await_folder_flags(nest_url, owner, set_name, audience="public")

    # ── 2. Serve it as the website — the other slice-4d gesture ──────────────
    #
    # ⚠ `find_and_expand_folder` TOGGLES the expander, so calling it on the row
    # `make_public` just expanded would CLOSE it and the body would vanish —
    # the trap `helpers/folder_content.bind_location_under_set` documents. Probe
    # first and only re-open if the body is actually gone, which also covers the
    # apps that rebuild the whole list on an observer tick and collapse it.
    if not b.website_toggle_visible():
        b.find_and_expand_folder(set_name)
    assert b.website_toggle_visible(), (
        f"folder-website-toggle must render on the expanded owner row of "
        f"{set_name!r}; it is the only door to a website folder since the "
        f"wizard's mode step retired. error={app.error_text()!r}"
    )
    assert b.website_toggle_enabled(), (
        "the website toggle must stay ENABLED on a public folder — it is "
        "disabled nowhere, and on a public folder it is unambiguously live"
    )
    b.toggle_website()
    _await_folder_flags(nest_url, owner, set_name, website_enabled=True)

    # Both flags together, on one row, from the nest itself: the cross-toggle
    # refusals mean a nest that accepted one could still have rejected the
    # other, and only the composed row proves it did not.
    row = _await_folder_flags(
        nest_url, owner, set_name, audience="public", website_enabled=True
    )
    assert row.get("audience") == "public" and bool(row.get("website_enabled"))

    # ── 3. Opt the ACTOR's address into serving ──────────────────────────────
    #
    # ⚠ The third switch, and the one run 2 of this test discovered the hard
    # way: the two folder-row controls say *this folder is public and published*,
    # but `<handle>.<domain>` only routes to an actor who has opted their address
    # in, and that opt-in is **default OFF** and lives on a different page
    # (`web-settings-subdomain-toggle`, `web-content-hosting.md` § Routing).
    # Without it the host falls through to the apex catch-all and the nest's own
    # info page is served — a 200 that looks like success.
    #
    # Driven through the UI rather than `fauna.web.set_subdomain_enabled`
    # directly, even though setup may use the API (convention 8): this switch is
    # part of the journey a user actually walks to publish a site, so an
    # app-driven exit proof should walk it.
    app.web_settings.navigate()
    app.web_settings.set_subdomain_enabled(True)
    b.navigate_folders()

    # ── 4. Drop a file in it through the real agent ──────────────────────────
    #
    # The binding is added AFTER the flip on purpose: the engine built for it
    # reads the current audience, so the corpus is plaintext from its first
    # chunk and no declassify pass is owed (see the module docstring).
    folder = tmp_path / "site-bound"
    folder.mkdir()
    _bind_location_under_set(app, set_name, folder, seat="owner")

    _atomic_write(folder / "index.html", body_html)
    _await_agent_upload(app, "index.html", seat="owner")

    assert not app.has_error(), (
        f"the upload round-trip surfaced an error: {app.error_text()!r}"
    )

    # ── 5. The nest serves those exact bytes at the owner's address ──────────
    #
    # The domain is the fixture's registration constant, not a nest-dict key:
    # `handled_nest` claims no domain start option (the arm-4 split retired
    # `handle_domain` — its `add_local_domain` call is what makes
    # MAIL_PRIMARY_DOMAIN the deployment identity), and the owner above was
    # registered onto exactly that domain.
    fqdn = f"{handle}.{MAIL_PRIMARY_DOMAIN}"
    seen: dict[str, object] = {"status": None, "body": ""}

    def _serves_our_bytes():
        status, served = _get_root_with_host(nest_url, fqdn)
        seen["status"], seen["body"] = status, served
        return status == 200 and marker in served

    wait_until(
        _serves_our_bytes,
        _SERVE_WINDOW_SECS,
        interval=2.0,
        diagnose=lambda: (
            f"GET / on {fqdn!r} did not serve the folder's own index.html.\n"
            f"  status={seen['status']}\n"
            f"  body[:400]={str(seen['body'])[:400]!r}\n"
            f"  wanted marker={marker!r}\n"
            f"  The agent DID report uploading index.html (asserted above), the "
            f"nest's own row carries audience=public + website_enabled=1, and "
            f"the actor's subdomain opt-in was switched on — so an INFO PAGE "
            f"here means the host still fell through to the apex catch-all "
            f"(check the opt-in actually round-tripped), and a 200 carrying "
            f"some OTHER body means the web fan-out never picked this folder's "
            f"file up.\n" + _agent_diagnosis(app, "owner")
        ),
    )


@pytest.mark.parametrize(
    "folder_share_owner_app", ["tui", "linux", "macos", "windows"], indirect=True
)
@pytest.mark.real_conversations
# real_sync_agent added row 283 (macOS widening): on macOS/windows the real
# fauna-sync-agent is a session-patch opt-in gated by FaunaMacApp's
# `FaunaE2E.realSyncAgent` / windows' `FAUNA_E2E_REAL_SYNC_AGENT` (set by
# conftest's `_apply_real_sync_agent_env` only when the collected session
# carries this marker) — without it `startSyncAgentProvisioner` /
# `HydrationSessionService` is never called at all, so a bound location never
# spawns an agent and the upload wait times out. linux spawns its agent
# unconditionally, so the marker is a no-op there.
@pytest.mark.real_sync_agent
# windows widened row 168: without this the app-spawned agent rendezvouses on
# the machine-global per-SID pipe (`\\.\pipe\fauna-sync.<SID>`) instead of a
# run-private one — a convention-10 breach. No-op on tui/linux/macos.
@pytest.mark.isolated_sync_agent
@pytest.mark.timeout(600)
@pytest.mark.feature("public-folders-and-websites")
def test_a_private_folder_is_not_served_even_with_the_website_toggle_on(
    folder_share_owner_app, tmp_path
):
    """The negative half, and the one that gives the positive its meaning.

    The website toggle publishes the folder's head; the **audience** decides who
    may read it. So a website-enabled folder that is still private must serve
    nothing — if it served anyway, the audience control would be decorative and
    the positive test above would prove only that *some* path reaches the web
    plane.

    Anchored to a **causal barrier**, never a settle-sleep (convention 14): the
    negative is asserted only after the nest's own row confirms
    ``website_enabled=1`` while ``audience`` is still private, and after the
    agent has reported the upload — i.e. after every event that *could* have
    published it has already happened.
    """
    app, nest, owner = folder_share_owner_app
    nest_url = nest["url"]
    handle = owner["handle"]

    set_name = f"privsite-{secrets.token_hex(4)}"
    marker = f"must-not-serve-{secrets.token_hex(6)}"

    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    idx = b.find_and_expand_folder(set_name)
    assert b.website_toggle_enabled(), (
        f"the website toggle must be ENABLED on a private folder (row {idx}) — "
        f"the setting is real and merely inert until the folder has a readable "
        f"audience, so the app hints rather than disabling"
    )
    b.toggle_website()
    row = _await_folder_flags(nest_url, owner, set_name, website_enabled=True)
    assert row.get("audience") != "public", (
        "the folder must still be private — turning on website serving must not "
        "change the audience as a side effect"
    )

    folder = tmp_path / "private-bound"
    folder.mkdir()
    _bind_location_under_set(app, set_name, folder, seat="owner")
    _atomic_write(folder / "index.html", f"<h1>{marker}</h1>\n")
    _await_agent_upload(app, "index.html", seat="owner")

    # The barrier has passed: the file is uploaded and the nest knows the folder
    # is website-enabled. Anything that was going to publish it has now had its
    # chance, so a single read is a verdict rather than a race.
    # MAIL_PRIMARY_DOMAIN for the same reason as the positive test above: the
    # nest dict deliberately carries no domain key since the arm-4 split.
    fqdn = f"{handle}.{MAIL_PRIMARY_DOMAIN}"
    status, served = _get_root_with_host(nest_url, fqdn)
    assert marker not in served, (
        f"GET / on {fqdn!r} served a PRIVATE folder's content (status={status}).\n"
        f"  body[:400]={served[:400]!r}\n"
        f"  The website toggle publishes the head; the audience decides who may "
        f"read it. Serving here means the audience gate is not consulted on the "
        f"web path at all, which would make every private website-enabled "
        f"folder world-readable."
    )
