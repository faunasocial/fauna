"""Shared drive-side helpers for the live Hetzner e2e tests (``tests/live/``).

Lifted verbatim from ``tests/live/test_hetzner_provision.py`` (2026-07-09) so the
private-relay live test reuses the exact same provisioning harness rather than
copying it (priority #2/#4 — one harness, not a copy per test). Gating, the env
contract, and the always-runs teardown stay in ``tests/live/conftest.py``; this
module owns only the *driving* of the shared client onboarding/provisioning
machine plus the no-SSH external verification primitives both tests share.
"""
from __future__ import annotations

import json
import os
import socket
import ssl
import time

import requests

# ── The token's two homes ──────────────────────────────────────────────────
#: The fleet's documented per-machine home for the Hetzner Cloud R/W token —
#: the same file the deploy-verify cloud-firewall check reads, in the same
#: order (env first, then this). Until 2026-09-02 the live gate read the env
#: ONLY, so a box that already carried the token in this file skipped the whole
#: suite and a session halted on a `NEEDS FROM USER:` for a secret sitting on
#: its own disk (measured on Windows, 2026-09-01). Never print or log its value.
HETZNER_TOKEN_FILE = "~/.hetzner-token"


def hetzner_token() -> str:
    """The live suite's Cloud R/W token: ``HETZNER_API_TOKEN`` when set and
    non-blank, else the stripped contents of :data:`HETZNER_TOKEN_FILE`, else
    ``""`` — an honest empty string the gate turns into a skip, never an error.

    tier_1 proofs: ``tests/test_live_token_resolution.py``.
    """
    tok = os.environ.get("HETZNER_API_TOKEN", "").strip()
    if tok:
        return tok
    try:
        with open(os.path.expanduser(HETZNER_TOKEN_FILE), encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return ""


# Hetzner curated offer + region. cx23 = 2 vCPU / 4 GB (the entry curated_offer;
# Hetzner retired the cx22 generation — 422 "deprecated" on create) — clears the
# >=2 GB mail floor (clamd holds its ~1.5 GB signature DB in RAM); fsn1 = DE.
#
# Both are env-overridable because a curated offer runs out of CAPACITY per DC,
# independently of anything in this repo: Hetzner then answers `create_server`
# with 412 `resource_unavailable` ("error during placement") and the run dies at
# the Server step, before a single fauna gate is exercised. The override lets a
# blocked run retry on a placeable pair without editing this shared file; the
# defaults are unchanged, so the other live tests keep today's behavior.
#
# ⚠ Do NOT pick the retry pair from `GET /datacenters` — its
# `server_types.available` list is an OFFER CATALOG, not live stock, and it will
# happily list a pair that cannot be placed. Measured 2026-08-27, all three
# against the real API with a minimal create body:
#   * cx23  @ fsn1 -> 412 resource_unavailable   (listed as unavailable — agreed)
#   * cpx22 @ fsn1 -> 412 resource_unavailable   (LISTED AS AVAILABLE — it lied)
#   * cax11 @ fsn1 -> 422 invalid_input          ("unsupported location for
#                                                 server type" — also listed)
#   * cx23  @ nbg1 -> 201 Created                (the pair that actually worked)
# The only reliable probe is an actual create + immediate delete.
SERVER_TYPE = os.environ.get("FAUNA_E2E_SERVER_TYPE", "").strip() or "cx23"
LOCATION = os.environ.get("FAUNA_E2E_LOCATION", "").strip() or "fsn1"

# Display metadata for the seeded location. Cosmetic — the provisioning call
# only ever receives `selected_location_id` — but kept honest so a LOCATION
# override cannot silently label a Finnish box "Falkenstein, DE".
_LOCATION_META = {
    "fsn1": ("Falkenstein", "DE"),
    "nbg1": ("Nuremberg", "DE"),
    "hel1": ("Helsinki", "FI"),
    "ash": ("Ashburn", "US"),
    "hil": ("Hillsboro", "US"),
    "sin": ("Singapore", "SG"),
}

# Provider-side label attached to every e2e box (via the machine's
# `set_provision_labels` IPC) so teardown's orphan sweep can select boxes by
# Hetzner `label_selector=fauna-e2e=1` rather than a name-prefix heuristic.
# MUST match tests/live/conftest.py's `E2E_LABEL_{KEY,VALUE}` (canonical there —
# it owns the sweep).
E2E_LABEL_KEY = "fauna-e2e"
E2E_LABEL_VALUE = "1"


# ── Which apps can drive a live run ────────────────────────────────────────
#: The apps whose e2e bridge reaches the shared onboarding machine-method table
#: this module drives. Everything below :func:`drive_provisioning` is already
#: app-agnostic — every name it sends resolves in the ONE shared dispatcher
#: (`libs/fauna-onboarding-machine/src/machine.rs`, `call_machine_method_async`)
#: — so what gates an app is purely whether its bridge forwards a name to that
#: table: tui and linux over `HttpBridgeDriver.call_machine_method`, web by
#: reflecting onto the wasm surface with `callMachineMethodForTest` as the
#: fall-through for dispatcher-only names, and macos + ios over the same
#: `HttpBridgeDriver.call_machine_method` they inherit through
#: `InProcessAgentDriver`, answered in `FaunaMacApp.swift` / `FaunaApp.swift`
#: respectively (two independent Swift bridge shells, not shared code).
#:
#: ⚠ Grow this tuple by BUILDING the bridge arm, never to silence a skip. A name
#: the app cannot forward fails mid-drive with a half-provisioned box still
#: running — the expensive failure this gate exists to prevent.
#:
#: ⚠ "Its bridge answers `call_machine_method`" is NOT sufficient, and a green
#: `test_nat_mode_choice.py` / `test_vps_config.py` does NOT imply live
#: readiness — those drive only SYNC setters/readers. Measured on macOS
#: 2026-08-29: with macos listed here but its bridge still sync-only, the drive
#: died with `overall: 'Idle'`, every step `Pending`, `attempt: 0`,
#: `started_at_ms: None` after the full 1200 s — nothing ever started (no server
#: created, €0, but 27 min). Two arms had to be BUILT before macos earned its
#: place here, and neither could come from the shared dispatcher:
#:   1. `start_provisioning` spawns work that outlives the call, so runtime
#:      ownership is per-app and the shared dispatcher deliberately excludes it
#:      (`apps/fauna-tui/src/automation.rs`, `apps/fauna-linux/src/main.rs`).
#:   2. The async names (`verify_dns`, `wizard_submit_claim_code`,
#:      `submit_nat_mode_choice`, …) run only via
#:      `OnboardingMachine::call_machine_method_async`; the SYNC dispatcher
#:      silently no-ops them in its `_` arm, so `verify_dns` never ran.
#: Both landed for macos 2026-08-29 — find them with
#: `git log --grep 'the macOS bridge drives the async dispatcher'`: the async
#: dispatcher is now uniffi-exported under the same `test-helpers` gate as its
#: sync twin, and `FaunaMacApp.swift` routes through it, keeping a local
#: `runProvisioning()` arm (NOT `startProvisioning()` — that one needs an
#: ambient tokio runtime the SwiftUI main thread does not have).
#:
#: iOS got the same two arms 2026-08-29 (`FaunaApp.swift`'s own copy of the
#: bridge shell, not shared code with macOS — each app owns its dispatch
#: switch) — the shared-Rust half needed nothing further, since the async
#: dispatcher export is one name table both apps call into.
#:
#: windows joined 2026-09-05, live-proven over a full provision → verify →
#: teardown. Its two bridge arms are in `TestAgent.cs`, the same shape as macOS:
#: `call_machine_method` through the ASYNC dispatcher, plus a local
#: `RunProvisioning()` arm — never `StartProvisioning()`, which needs an ambient
#: tokio runtime the WinUI UI thread does not have.
#:
#: ⚠ The lesson from the five sessions it took, because it generalises and it
#: cost the most: for three of them the box could not answer *which client the
#: Online poll had built*, and each session guessed a different cause instead.
#: `run_step` now narrates one `info!` per attempt and the probe builders narrate
#: their choice, so a frozen live run says which of "the builder failed", "the
#: dial is slow" and "the future is not polled" it is. Before adding an app here,
#: make sure its driver can surface `fauna.log` — windows' `ack_timeout_
#: diagnostics` reads it, and that is what turned a guess into a measurement.
#: The dead hypotheses themselves are in
#:  and are not worth
#: re-reading here.
LIVE_DRIVE_APPS = ("linux", "tui", "web", "macos", "ios", "windows")


def skip_unless_live_drive_app(driver, *, surface: str) -> None:
    """Route an app that cannot drive a live run through convention 7's
    `skip_unbuilt` — temporary parity debt, red under `--strict-app`.

    One home for the app set so the four live tests cannot drift apart, and so
    the day an app's bridge lands, one edit opens all of them.
    """
    from helpers.app_surface import app_name, skip_unbuilt

    app = app_name(driver)
    if app in LIVE_DRIVE_APPS:
        return
    skip_unbuilt(
        driver,
        surface=surface,
        detail=(
            "this app's e2e bridge does not forward onboarding machine methods "
            "to the shared dispatcher yet; built for "
            + ", ".join(LIVE_DRIVE_APPS)
        ),
        tracked="",
    )


# ── Reaching a box that has not been claimed yet ───────────────────────────
#: The apps that can reach a **freshly-provisioned, pre-claim** box. Strictly
#: narrower than :data:`LIVE_DRIVE_APPS`, and the reason is TLS, not the bridge.
#:
#: A new nest boots **domainless** and learns its name *from the claim*
#: (`docs/goal/behavior/onboarding.md` § handle entry; `nest/domains-and-tls-
#: bootstrap.md` § Boot / § Claim sets identity), so at Online-poll time — which
#: runs BEFORE the claim — it is necessarily serving its self-signed floor cert
#: and **no ACME cert can exist for a name the nest does not yet know**. Native
#: probes accept that floor provisionally (`NoPinPolicy::AcceptProvisional`);
#: web cannot, because the browser owns TLS in wasm and the strict client is
#: kept deliberately (same goal-doc paragraph).
#:
#: ⚠ So web's Online poll fails on TLS **however long DNS is given** — it is not
#: a propagation race. Two paid live runs on 2026-08-29 exhausted the
#: orchestrator's 480-attempt ceiling before this was understood; don't spend a
#: third. The ratified fix is the nest's IP bridge cert plus the wizard claiming
#: the box as Online's last substep.
#:
#: Deliberately NOT folded into `LIVE_DRIVE_APPS`: that tuple answers "can this
#: app drive the machine bridge", which web can and does. A test targeting an
#: ALREADY-CLAIMED box with a real cert (the live DNS-01 renewal against
#: example.com) needs only that one, so collapsing the two would over-gate web on
#: a test it can actually run.
#:
#: macos and ios join on the cfg gate, not on app identity: the
#: provisional-accept probe is `#[cfg(not(target_arch = "wasm32"))]`
#: (`machine.rs`'s `nest_probe_client` / `nest_probe_client_resolving`) and the
#: strict fall-through that defeats web is `#[cfg(target_arch = "wasm32")]`.
#: macos builds for `aarch64-apple-darwin` and ios for a native
#: (non-wasm) simulator/device target, so both take the native
#: `NoPinPolicy::AcceptProvisional` branch exactly as linux and tui do — web is
#: the sole exception because it is the sole wasm target, which is what the
#: paragraph above already says. macos added 2026-08-29 with its bridge build;
#: ios added the same day with its own (reasoning, not yet independently
#: measured on ios — the cfg gate is identical, but the live run is what
#: demonstrates it). windows joins on the same cfg reasoning — it builds for
#: `aarch64-pc-windows-msvc`, another non-wasm target — and unlike ios the
#: property is MEASURED: its live `Online` poll reached a pre-claim box over the
#: floor cert and succeeded (`attempt 14 finished (Succeeded)`, 2026-09-04),
#: which is exactly what this tuple asserts.
FRESH_BOX_REACH_APPS = ("linux", "tui", "macos", "ios", "windows")


def skip_unless_fresh_box_reach_app(driver, *, surface: str) -> None:
    """Skip when the app cannot reach a pre-claim box over its floor cert."""
    from helpers.app_surface import app_name, skip_unbuilt

    skip_unless_live_drive_app(driver, surface=surface)
    if app_name(driver) in FRESH_BOX_REACH_APPS:
        return
    skip_unbuilt(
        driver,
        surface=surface,
        detail=(
            "a pre-claim box serves only its self-signed floor cert (it learns "
            "its name from the claim, so no ACME cert can exist yet) and this "
            "app's TLS stack cannot accept it — on web the browser owns TLS in "
            "wasm and the strict client is deliberate. Not a DNS race: no "
            "propagation delay makes this succeed. Unblocks with the nest's IP "
            "bridge cert + the wizard claiming the box as Online's last substep"
        ),
        tracked="",
    )


# ── Driving the shared provisioning path via the onboarding bridge ──────────
def drive_provisioning(app, box, token: str, *,
                       server_type: str = SERVER_TYPE,
                       location: str = LOCATION,
                       labels: list[list[str]] | None = None,
                       image_tag: str | None = None) -> None:
    """Drive the REAL onboarding machine on the linux app via the cross-app
    test bridge (``call_machine_method``) — same code every app ships — with a
    real token and NO fake-cloud override, so ``fauna-provisioning`` hits real
    ``api.hetzner.cloud``. Injects the verified VPS config, runs real
    ``verify_dns`` (lists the token's zones), then fires ``start_provisioning``
    (fire-and-forget; poll with :func:`await_provisioning`)."""
    drv = app.driver
    # Identity + handle: set_current_handle's domain part becomes the box's FQDN.
    drv.call_machine_method("seed_identity", json.dumps(box.identity_secret_hex))
    drv.call_machine_method("set_current_handle", json.dumps(box.handle))
    # DNS stage: Hetzner, single token, REAL verify_dns (lists the token's zones,
    # sets the zone id the orchestrator publishes A/MX/SPF/DMARC into).
    drv.call_machine_method("select_dns_provider", json.dumps("hetzner"))
    drv.call_machine_method("set_dns_cred", json.dumps(["api-token", token]))
    drv.call_machine_method("verify_dns")  # async, driven to completion by the bridge
    # VPS stage: inject a verified Hetzner config (the real create_server still
    # runs during provisioning). server_types must contain the selected id —
    # run_provisioning_inner looks it up there (machine.rs).
    _loc_name, _loc_country = _LOCATION_META.get(location, (location, ""))
    vps_seed = {
        "provider_id": "hetzner",
        "creds": {"api-token": token},
        "server_types": [{
            "id": server_type, "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
            "price_monthly_cents": 0, "currency": "EUR",
        }],
        "selected_server_type_id": server_type,
        "locations": [{
            "id": location, "name": _loc_name, "city": _loc_name, "country": _loc_country,
        }],
        "selected_location_id": location,
    }
    drv.call_machine_method("set_vps_state_for_test", json.dumps(vps_seed))
    drv.call_machine_method("set_step_for_test", json.dumps("NestProvisioning"))
    # Tag the box so teardown's orphan sweep can select it by provider
    # `label_selector` instead of a name-prefix + age heuristic. Bucket-1 IPC
    # (no human config surface) — production passes no extra labels (every box
    # additionally carries the orchestrator's constant `managed-by=fauna`
    # marker); only the e2e sets it (machine.rs `set_provision_labels`).
    #
    # ⚠ `labels` exists for a box that must OUTLIVE the run (the staging box,
    # tests/live/test_staging_box_provision.py): every live test's teardown
    # deletes any `fauna-e2e=1` server older than an hour, so a kept box must
    # never carry that label. The default stays the throwaway label.
    if labels is None:
        labels = [[E2E_LABEL_KEY, E2E_LABEL_VALUE]]
    drv.call_machine_method("set_provision_labels", json.dumps(labels))
    # Pin a specific nest image when the run asks for one (e.g. the security
    # review's live-negative pins an OLD image to assert first-contact HARD-FAILS
    # on the injected-root path). Default is `latest` (the machine's slot default),
    # so an unset env keeps production behavior. Bucket-1 IPC, same as above.
    # An explicit `image_tag` argument wins over the env (the staging box
    # follows `dev` by definition, whatever the caller's shell holds).
    if image_tag is None:
        image_tag = os.environ.get("FAUNA_E2E_IMAGE_TAG", "").strip()
    if image_tag:
        drv.call_machine_method("set_provision_image_tag", json.dumps(image_tag))
    # Fire-and-forget; the bridge returns immediately and we poll the snapshot.
    drv.call_machine_method("start_provisioning")


def read_snapshot(app):
    raw = app.driver.call_machine_method("provisioning_snapshot", "")
    if raw is None:
        return None
    return json.loads(raw) if isinstance(raw, str) else raw


#: The one `Online`-step failure `OnboardingMachine::run_provisioning_claim`
#: reports when its probe (`claim_provisioned_box`, a distinct WS-RPC call
#: from the orchestrator's own already-succeeded `/api/v1/health` poll —
#: both display under the SAME `OnlineWaiting` substep) comes back `NotYet`.
#:
#: ⚠ **Measured 2026-08-29 on macOS: the ORIGINAL root cause of this (5 real
#: ios runs, 65-137s each) was NOT a slow box** — a direct curl confirmed the
#: box's `/health` answered in milliseconds throughout. Diagnostic logging
#: added to `claim_provisioned_box`'s swallowed `Err` arm caught the real
#: error: `WebSocket error: invalid ws url: URL error: No host name in the
#: URL nest_url=""` — `run_provisioning_claim` read `effective_nest_url()` /
#: `state.nest_url`, which is populated only by `stash_provisioning_result`
#: AFTER `run_provisioning_inner` (and this claim call inside it) returns,
#: so the FIRST claim attempt always dialed an empty URL. Fixed in
#: `machine.rs::run_provisioning_claim` (now takes the caller's own
#: just-computed `nest_url` as a parameter instead) — see
#: `git log --grep 'run_provisioning_claim reads the URL it was already
#: handed'`. With that fixed, a live run's first attempt should now
#: generally succeed outright, so this retry path should rarely fire.
#:
#: ⚠ **A SECOND, separate, still-unfixed bug means a retry that DOES fire
#: cannot help once the VPS already exists.** `run_provisioning_inner` mints
#: a FRESH `claim_code` + `deployment_seed` on every call
#: (`fauna_provisioning::generate_claim_code()` /
#: `resolve_deployment_seed_and_domain`), including a `retry_provisioning`
#: re-run — but the box's cloud-init (and thus its real identity) was baked
#: in at the ORIGINAL `ServerCreating`, which a retry skips as
#: `ServerAlreadyExists`. So a retry mints a fresh identity for the SAME
#: unrecreated box and can never match it: measured, `retry_provisioning`
#: against an already-created box now fails with `nest trust: channel
#: binding: nest_actor_id is not the expected nest` on every subsequent
#: attempt, not `_TRANSIENT_CLAIM_NOT_YET` again. This is a real product bug
#: in the page's own "Retry" button too (not test-only) — captured as its
#: own row. Until it's fixed, a retry
#: that fires here trades one honest transient error for a more confusing
#: terminal one; kept anyway (capped at `max_claim_retries`) because it still
#: matches production Retry-button behavior byte-for-byte, and a genuinely
#: slow box (the scenario this was originally written for) remains possible.
_TRANSIENT_CLAIM_NOT_YET = "the nest did not complete the claim"


def await_provisioning(app, timeout: float, *, max_claim_retries: int = 2) -> dict:
    deadline = time.monotonic() + timeout
    last = None
    retries_used = 0
    while time.monotonic() < deadline:
        snap = read_snapshot(app)
        if snap:
            last = snap
            if snap.get("overall") == "Failed" and \
                    snap.get("final_error") == _TRANSIENT_CLAIM_NOT_YET and \
                    retries_used < max_claim_retries:
                retries_used += 1
                print(
                    f"[live] Online claim probe reported {_TRANSIENT_CLAIM_NOT_YET!r} "
                    f"(retry {retries_used}/{max_claim_retries}) — pressing "
                    f"retry_provisioning, same as the page's own Retry button"
                )
                app.driver.call_machine_method("retry_provisioning")
                time.sleep(5)
                continue
            if snap.get("overall") in ("Succeeded", "Failed", "Cancelled"):
                return snap
        time.sleep(5)
    # Convention 6 on the OTHER live failure path. The ack-timeout raise carries
    # the driver's app-liveness + recent-output block, but a provisioning budget
    # that simply expires carries only the snapshot — and the snapshot cannot say
    # whether the app is even alive, nor show what it last printed. That gap cost a
    # real diagnosis: the run that first survived the windows fail-fast failed
    # HERE, so the app's own stderr — the only place
    # the fatal-report hooks write — was never surfaced at all.
    #
    # Same question as an ack timeout ("is the app alive, and what did it last
    # say?"), so it reuses that hook rather than growing a second one; drivers
    # without an app log return "" and the message is unchanged.
    diagnostics = ""
    try:
        diagnostics = app.driver.ack_timeout_diagnostics()
    except Exception:  # noqa: BLE001 - diagnostics must never replace the failure
        pass
    raise AssertionError(
        f"provisioning did not finish within {timeout:.0f}s; last snapshot: {last}"
        f"{diagnostics}"
    )


def finish_provisioned_wizard(app, *, nat_mode: str | None = None) -> None:
    """Walk the wizard off a ``Succeeded`` provisioning page the way a user
    does: Continue → `nat_mode_choice` (confirmed; `nat_mode` selects a radio
    first when given) → the trust prompt where an app renders it → `LoggedIn`.

    The box is ALREADY CLAIMED when this is called: provisioning's own `Online`
    step claims it as its last substep, by IP, with the code the wizard minted
    (`docs/goal/behavior/onboarding.md` § 6 *Provisioning = build + claim*,
    built 2026-08-29) — so ``Succeeded`` from :func:`await_provisioning` means
    built **and** claimed, and what is left is exactly the tail a claim-code
    submit leaves. The in-run claim does not move the wizard's step by itself;
    Continue does (`OnboardingActions.continue_from_provisioning`).

    ⚠ Do NOT claim again here. Until 2026-08-29 every live test followed the
    run with a second claim — `test_hetzner_provision.py` bridge-drove
    `wizard_submit_claim_code` with the snapshot's code, the two-box tests
    relaunched and typed it into the claim-code page — because the run itself
    never claimed (`continue_from_provisioning` was pure routing). Now that it
    does, that second submission is rejected `fauna.auth.already_claimed` (the
    box IS claimed), the wizard stays on the claim-code page with its dedicated
    already-claimed message (§ 3a), and the ``NatModeChoice`` assertion fails —
    measured 2026-08-29 on ios the moment the run's own claim started to
    succeed. § 6's recovery edge is "never a second
    claim": ownership of an already-claimed box is confirmed by the silent
    challenge inside `claim_provisioned_box`, never by re-submitting a code.
    The claim-code page keeps its own coverage where a code is genuinely
    typed — `test_mail_enable_at_admin_claim.py`, and the two-box live tests'
    PRIVATE box, which is claimed outside any provisioning run.
    """
    app.onboarding.continue_from_provisioning()
    assert app.onboarding.finish_nat_mode(mode=nat_mode), (
        "finish_nat_mode() found nat_mode_choice not showing right after "
        "continue_from_provisioning() had waited for it"
    )


#: The owed live witness: press `retry_provisioning` once against a
#: box the run's own claim already brought to `Succeeded`, and confirm the
#: already-claimed arm resolves ownership by the silent challenge instead of
#: erroring. Distinct from `await_provisioning`'s own retry (which fires on a
#: transient `_TRANSIENT_CLAIM_NOT_YET` failure *before* `Succeeded`) — this is
#: a deliberate press *after* success, matching the page's own Retry button
#: reachable via `call_machine_method` without its `can_retry_provisioning()`
#: UI gate. Call between the `Succeeded` snapshot and `finish_provisioned_wizard`
#: (the tail leaves the wizard, taking this door with it).
#: Matched as a SUBSTRING, and deliberately shorter than the message that
#: prompted it. The guard below was written against the pre-fix wording "nest
#: trust: channel binding"; the failure it exists to catch resurfaced on
#: 2026-09-04 spelled "channel binding: nest_actor_id is not the expected nest"
#: (`nest_trust.rs`), which does not contain that string — so the guard passed
#: and the run reported only the generic "did not resolve back to Succeeded",
#: throwing away the one word that names the cause. Match the invariant, not one
#: crate's phrasing of it.
_CHANNEL_BINDING_SIGNATURE = "channel binding"


def press_post_claim_retry_witness(app, *, timeout: float = 300) -> dict:
    pre = read_snapshot(app)
    assert pre and pre.get("overall") == "Succeeded", (
        f"press_post_claim_retry_witness called before Succeeded: {pre}"
    )
    print("[live] pressing retry_provisioning against the already-claimed box "
          "(the owed post-claim retry witness)")
    app.driver.call_machine_method("retry_provisioning")
    post = await_provisioning(app, timeout)
    final_error = post.get("final_error") or ""
    assert _CHANNEL_BINDING_SIGNATURE not in final_error, (
        f"retry_provisioning against an already-claimed box hit the pre-fix "
        f"channel-binding signature: {post}"
    )
    assert post.get("overall") == "Succeeded", (
        f"retry_provisioning against an already-claimed box did not resolve "
        f"back to Succeeded via the silent-challenge already-claimed arm: {post}"
    )
    print("[live] retry-witness OK: retry_provisioning after Succeeded "
          "reached Succeeded again via the already-claimed arm, no channel-"
          "binding regression")
    return post


# ── Verification helpers (no SSH — externally observable) ───────────────────
def retry_get(url: str, *, verify: bool, timeout: float, attempts: int, delay: float):
    last = None
    for _ in range(attempts):
        try:
            r = requests.get(url, verify=verify, timeout=timeout)
            if r.status_code == 200:
                return r
            last = f"status {r.status_code}"
        except requests.RequestException as e:
            last = repr(e)
        time.sleep(delay)
    raise AssertionError(f"GET {url} never returned 200 ({last})")


def tcp_open(ip: str, port: int, timeout: float = 5.0) -> bool:
    try:
        with socket.create_connection((ip, port), timeout=timeout):
            return True
    except OSError:
        return False


def mail_banner_ok(ip: str, port: int, *, tls: bool, expect: bytes, hostname: str,
                   timeout: float = 8.0) -> bool:
    raw = socket.create_connection((ip, port), timeout=timeout)
    try:
        sock = raw
        if tls:
            # Unverified: we're proving the port serves the protocol, not its cert
            # (the main cert is covered by the TLS-trust gate; the mail cert is
            # for mail.<primary> and isn't the subject here).
            sock = ssl._create_unverified_context().wrap_socket(raw, server_hostname=hostname)
        sock.settimeout(timeout)
        return expect in sock.recv(256)
    finally:
        try:
            raw.close()
        except OSError:
            pass


def assert_mail_port(ip: str, port: int, *, tls: bool, expect: bytes, hostname: str,
                     attempts: int = 36, delay: float = 10.0):
    last = None
    for _ in range(attempts):
        try:
            if mail_banner_ok(ip, port, tls=tls, expect=expect, hostname=hostname):
                return
            last = "wrong banner"
        except OSError as e:
            last = repr(e)
        time.sleep(delay)
    raise AssertionError(
        f"mail port {port} (tls={tls}) never produced banner {expect!r} on {ip} ({last})"
    )
