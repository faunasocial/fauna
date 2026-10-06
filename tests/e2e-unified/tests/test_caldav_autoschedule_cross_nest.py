"""tier_3: the MDA server-side auto-schedule **CROSS-NEST** mailbox-less sealed
rail, proven end-to-end through the **real Go MDA binary** across **two** nests
(caldav-server.md § Server-side auto-schedule → the cross-nest twin of the
same-nest sealed rail).

What this uniquely proves over the same-nest twin
(`test_caldav_autoschedule_mailbox_less.py`): that test invites a mailbox-less
attendee on the **organizer's own** nest, so the MDA classifies her with reads
against its own nest. Here the attendee lives on a **different, unpaired** nest B,
so the MDA's off-box-domain branch must:

  - classify `<carol>@<nestB-authority>` via the shared resolver reached over
    UniFFI (`mailfauna.ClassifyAttendeeTransport` →
    `fauna_client_caldav::resolve_attendee_transport` with `AnonAttendeeDiscovery`):
    open an **anonymous** WS-RPC connection **directly to nest B**,
    `fauna.actor.by_handle(carol)` → nest B's `fauna.setup.status.email_enabled`
    (false — nest B never enabled mail) → the **cross-nest sealed rail**;
  - fetch carol's key package **cross-nest** via the organizer nest's own
    federation relay (`keypackage.fetch` carrying nest B's URL →
    `originate_keypackage_fetch`);
  - seal a one-off `WelcomeKind::Scheduling` group itself (ephemeral MLS signer)
    and ship it over `fauna.bridges.deliver_sealed_scheduling` with
    `peer_domain = <nestB-authority>`, so the organizer nest relays the welcome to
    nest B — landing it in carol's durable inbox **on nest B**, with no email
    anywhere (carol has no mailbox, and nest B isn't even a mail deployment).

RED before this slice: the MDA's classifier routed every OFF-box-domain attendee
unconditionally to the email rail (`autoschedule.go` pre-cross-nest), so a foreign
mailbox-less Fauna attendee was sent to MX → bounce (no mailbox), and **no**
Scheduling welcome ever landed on nest B. (Unit-pinned by autoschedule_test.go
`TestClassifyAutoScheduleRecipientsCrossNest`.)

The loopback-authority trick (`127.0.0.1:<port>` as the handle domain) is the e2e
mirror of the in-process cross-nest conformance test
(`bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs`): nest B
advertises its own loopback authority, so `by_handle` replies with a domain
`resolve_handle_domain` maps straight back to nest B's loopback URL — both for the
MDA's anon discovery AND the organizer nest's federation relay.

tier_3: two real `fauna-nest` binaries + the real `fauna-mail-bridge` MDA binary +
a raw CalDAV PUT over the MDA's self-signed HTTPS listener — a stub can't reach the
real cgo anon-discovery + cross-nest federation relay.
"""

from __future__ import annotations

import time

import pytest

from helpers.caldav_client import CalDAVClient, build_invite_vevent
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.scheduling_inbox import (
    scheduling_welcomes,
    utc_offset,
    wait_scheduling_welcomes,
)
from common.auth import register_handled_actor
from tests.api import conv_api

# windows added 2026-07-18 (was a stale exclusion): the only client mutation is
# the generic, driver-agnostic `enable_mail_plain` gesture — no CalDAV-specific
# TestAgent command needed (unlike the same-nest sibling
# `test_caldav_autoschedule_mailbox_less.py`, which stays linux-only).
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    # tui added 2026-08-21: the only client mutation is the generic
    # enable_mail_plain gesture (already tui-proven); everything else is a raw
    # CalDAV PUT + cross-nest federation, driver-agnostic.
    pytest.mark.tui,
]

# The organizer's chosen mail/CalDAV password (PLAIN credential path) — the CalDAV
# client authenticates with it. Shared with the IMAP AEAD blob.
_PASSWORD = "CrossNestAutoSchedPlainPw01AbCd"
# The cross-nest mailbox-less attendee's handle on nest B = the local part the
# organizer invites. The MDA's anon `by_handle(<local>)` on nest B must resolve it.
_ATTENDEE_HANDLE = "carol"
# Key packages carol publishes on nest B. The MDA's cross-nest `keypackage.fetch`
# consumes one per sealed delivery (REQUEST, then CANCEL); publish enough for both
# plus a margin so a pre/post count is observable.
_KP_COUNT = 4


@pytest.mark.feature("calendar-in-standard-apps")
def test_cross_nest_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda(
    app, dedicated_caldav_mailbox_less_nest, caldav_cross_nest_peer, request
):
    """A stock CalDAV organizer on nest A invites a mailbox-less Fauna attendee on a
    DIFFERENT, unpaired nest B → the real MDA's off-box branch anon-discovers nest
    B, classifies her onto the sealed scheduling rail, fetches her key package
    cross-nest via the organizer nest's federation relay, seals the iMIP, and
    delivers it over `deliver_sealed_scheduling` (peer_domain = nest B). carol's
    inbox ON NEST B gains a Scheduling welcome (REQUEST), then a second (CANCEL).

    RED if the MDA routed the off-box attendee to the email rail (the pre-cross-nest
    behavior) — no Scheduling welcome would ever land on nest B.
    """
    handle = dedicated_caldav_mailbox_less_nest  # nest A: MDA + the organizer
    handle.assert_mta_running()
    nest_a = handle.nest
    domain_a = handle.domain  # fauna.test (nest A's local domain)

    peer = caldav_cross_nest_peer  # nest B: carol's email-disabled foreign nest
    peer_authority = peer["authority"]  # 127.0.0.1:<portB> — also her handle domain

    # ── 1. Organizer = the claimed admin on nest A, mail-enabled via the client UI
    #       (mints the `default` mail credential the CalDAV client authenticates
    #       with — the AEAD blob is shared IMAP + CalDAV). ──
    login_as_nest_admin(app, nest_a, dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — the CalDAV listener this test PUTs against
    # only exists on the far side of it.
    handle.rebind_after_enable()
    organizer_addr = alias_admin_to_address(nest_a, domain_a)  # admin@fauna.test

    # ── 2. The CROSS-NEST mailbox-less attendee: a handled Fauna actor on nest B
    #       (email-disabled) with published real key packages but NO mail alias.
    #       Her handle domain IS nest B's loopback authority, so the MDA's anon
    #       by_handle on nest B reaches her and resolve_handle_domain maps the rail
    #       back to nest B. ──
    carol = register_handled_actor(
        peer["port"], handle=_ATTENDEE_HANDLE, domain=peer_authority, base_url=peer["url"]
    )
    attendee_addr = f"{_ATTENDEE_HANDLE}@{peer_authority}"  # carol@127.0.0.1:<portB>
    stored = conv_api.keypackage_upload(
        peer["port"], carol, conv_api.mint_key_packages(bytes(carol["signing_key"]), _KP_COUNT),
        scheme="https",
    )
    assert stored == _KP_COUNT, f"carol should have published {_KP_COUNT} key packages, stored={stored}"
    kp_before = conv_api.keypackage_count(peer["port"], carol, carol["actor_id_hex"], scheme="https")
    assert kp_before >= 1, "carol must have ≥1 usable key package so by_handle reports her addressable"
    assert scheduling_welcomes(conv_api.inbox(peer["url"], carol)) == [], (
        "carol's inbox on nest B must start with no scheduling welcomes"
    )

    # ── 3. The stock CalDAV organizer connects over nest A's MDA self-signed HTTPS
    #       listener and lazy-provisions the Personal calendar (first PROPFIND). ──
    caldav_url = f"https://127.0.0.1:{handle.caldav_port}"
    organizer = CalDAVClient(caldav_url, organizer_addr, _PASSWORD, verify=False)
    organizer.wait_until_serving(timeout=120.0)
    cal = organizer.personal_calendar()

    # ── 4. Organizer PUTs an event inviting the CROSS-NEST attendee → nest A's MDA
    #       off-box branch anon-discovers nest B, classifies carol sealed, fetches
    #       her key package via the federation relay, seals, and delivers
    #       cross-nest (peer_domain = nest B authority). ──
    nonce = f"xnest{int(time.time())}"
    uid = f"{nonce}@{domain_a}"
    summary = f"Cross-nest kickoff {nonce}"
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, utc_offset(60), utc_offset(120), organizer_addr, [attendee_addr]),
    )

    welcomes = wait_scheduling_welcomes(peer, carol, at_least=1, timeout=120.0)
    assert len(welcomes) == 1, (
        f"the real MDA must classify the CROSS-NEST mailbox-less attendee {attendee_addr} "
        f"onto the sealed scheduling rail and deliver exactly one Scheduling welcome to her "
        f"inbox on nest B — got {len(welcomes)}. A miss means the off-box branch did not "
        f"anon-discover nest B (resolve_attendee_transport), could not fetch her key package "
        f"cross-nest (federation relay), or routed her to the email rail (off-box bounce, the "
        f"pre-cross-nest behavior).\n"
        f"  carol inbox on nest B (raw): {conv_api.inbox(peer['url'], carol)}\n"
        f"  {handle.bridge_log_hint('mda')}"
    )
    channel_id = welcomes[0]["channel_id"]
    assert channel_id, f"the scheduling welcome must carry the bound channel id; got {welcomes[0]!r}"

    # The sealed iMIP rides the bound channel as one application message. The
    # channel's message log lives on the ORGANIZER's nest (nest A — where the MDA
    # `channel.send`'d it as the organizer inside `deliver_sealed_scheduling`); carol
    # on nest B reads it the way her real client does, via the membership-gated
    # `fauna.federation.channel.fetch` relay (channel.fetch with nest A as the home
    # `nest_url`) — a direct same-nest fetch on nest B sees only the relayed welcome,
    # not the message log.
    messages = conv_api.channel_fetch(
        peer["port"], carol, channel_id, after=0, nest_url=nest_a["peer_url"], scheme="https"
    )
    assert len(messages) >= 1, (
        f"the bound scheduling channel {channel_id} (home nest A) must carry the sealed "
        f"iMIP application message, readable cross-nest from nest B via the federation "
        f"relay; got {len(messages)} message(s)"
    )

    # The MDA consumed one of carol's key packages on nest B (cross-nest fetch) to
    # seal to her — proof the `keypackage.fetch` federation-relay leg fired.
    kp_after_request = conv_api.keypackage_count(
        peer["port"], carol, carol["actor_id_hex"], scheme="https"
    )
    assert kp_after_request == kp_before - 1, (
        f"the MDA must have consumed exactly one of carol's key packages (cross-nest) sealing "
        f"the REQUEST (was {kp_before}, now {kp_after_request})"
    )

    # ── 5. CANCEL-on-removal: the organizer removes the only attendee (empty roster,
    #       organizer retained) → the gateway reads the prior roster and fans an iMIP
    #       CANCEL to the dropped cross-nest attendee over the SAME sealed rail (a
    #       second Scheduling welcome on nest B). No new REQUEST (the new roster is
    #       empty). The prior-roster read needs the organizer's MLS snapshot
    #       in-session, present because the step-4 sealing PUT succeeded. ──
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, utc_offset(60), utc_offset(120), organizer_addr, [], sequence=1),
    )
    welcomes2 = wait_scheduling_welcomes(peer, carol, at_least=2, timeout=120.0)
    assert len(welcomes2) >= 2, (
        f"removing the cross-nest mailbox-less attendee must fan a second sealed scheduling "
        f"delivery (the iMIP CANCEL) to her inbox on nest B — the scheduling-welcome tally "
        f"stayed at {len(welcomes2)} (was 1 after the REQUEST). Its absence means the CANCEL "
        f"path's cross-nest classification did not fire.\n"
        f"  {handle.bridge_log_hint('mda')}"
    )
