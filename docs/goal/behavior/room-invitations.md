# Room invitations — target state

Owns: room-invitations
Status: ratified — split verbatim out of [`conversation-rooms.md`](conversation-rooms.md) on 2026-09-28. Each ruling carries its own ratification: the invitation's fate, its judgement at accept and its listing and withdrawal were ruled and built 2026-09-21, and both cross-nest legs were ratified and built 2026-09-26.
Authority: **the room invitation's lifecycle** — what an invitation is once issued: the three facts the home nest holds for it and their fate under account deletion and identity succession, the inviter as attribution, the standing-offer judgement at accept with the inviter read by its own seat, the lapse that consumes it, the listing and withdrawal of pending invitations (`fauna.conversations.room.list_invites` / `room.revoke_invite`), and the cross-nest legs — the room home's push to a foreign invitee, the relayed accept, and the relayed issue by a member homed elsewhere — together with their build record. Defers the join rules themselves (`invite`, `member-invite`, `request`) and the rest of the room model to [`conversation-rooms.md`](conversation-rooms.md); the community class's invitation delivery through the inbox plane to [`community-rooms.md`](community-rooms.md) § Implementation status today; the federation kinds' wire to [`../architecture/federation.md`](../architecture/federation.md) § Federation residue surface; the pending-invitation element ids to [`../ui/conversations.md`](../ui/conversations.md) § Element IDs; and the invitation row's re-point declaration to [`opaque-carrier-walks.md`](opaque-carrier-walks.md).

Last verified: 2026-09-28 (the split; every build date, pin and gap below is its entry's own, carried across unedited) | Sources: `bins/fauna-nest/src/{conversations_handlers.rs,federation_handlers.rs,db/rooms.rs}`, `libs/fauna-conversations/src/{manager.rs,backends/fauna_mls.rs}`, `libs/fauna-client-conversations/src/lib.rs`

> **Audience:** the nest (the invite, accept, list and withdraw doors and their federation relays) and every app (the invitee's knock, the accept, the pending-invitation list in the room editor).
> **Purpose:** what happens to a room invitation between the moment it is issued and the moment it is accepted, lapses or is withdrawn — on one nest and across two or three.

*Split verbatim out of [`conversation-rooms.md`](conversation-rooms.md) on 2026-09-28. The invitation's lifecycle had become that doc's fastest-growing concept — its ruling and build record were written almost entirely in the preceding week — while the join rules it serves are one short enumeration that stayed behind. A stub remains at each original location; prior history: `git log --follow docs/goal/behavior/conversation-rooms.md`.*

*Reading this doc. Its text was carried verbatim under its original headings, so an unqualified `§ <name>` citation may name a section that is no longer a sibling on the page. Two headings exist in both docs: in the moved text an unqualified `§ Join rules and invites` or `§ Implementation status today` means* this *doc's — the invitation's ruling and its build record. Every other unqualified name — § The room, § The floor roster, § Roles and authorization, § The home nest, § History for joiners, § Done definition, and the rest — resolves in [`conversation-rooms.md`](conversation-rooms.md).*

## Section map

| Section | What it holds |
|---|---|
| § Implementation status today | The invitation's build record, moved verbatim and in its original order: the invitation's fate under account deletion and succession, the judgement at accept, and the listing and withdrawal of pending invitations. |
| § Join rules and invites | The invitation's lifecycle, moved verbatim: its fate as one thing, the inviter as attribution, the standing offer, the inviter judged by its own seat, the lapse, the pending-invitation doors, and the cross-nest invitation with its foreign-inviter leg. The join rules themselves stayed in [`conversation-rooms.md`](conversation-rooms.md) § Join rules and invites. |

## Implementation status today

The invitation's entries, carried verbatim out of [`conversation-rooms.md`](conversation-rooms.md) § Implementation status today on 2026-09-28 in their original order.

- **An invitation shares its invitee's fate — ruled and built (nest,
  2026-09-21), after standing unruled since the invite plane shipped.**
  `room_invites` names its people `invitee_id` / `inviter_id`, two words the
  actor census had no root for, so the table sat in no registry and **account
  deletion never reached it**: a deleted account's id rested there for good —
  in the clear, indexed, and twice more inside the signed act. It is now ruled
  on all three axes as § Join rules and invites states: the row is the
  invitee's (*Purge*; a plain *Move*, beside the envelope and quota charge the
  ceremony already carried; exported to the invitee), the inviter a `Stay`
  reference, the signed bytes an actor-bearing carrier that follows the row and
  is never rewritten. Witnessed by
  `db::rooms::tests::deleting_an_account_removes_the_invitations_addressed_to_it`
  and
  `successions::tests::a_pending_room_invitation_follows_the_account_with_its_envelope`,
  both red first. The census vocabulary gained both words
  ([`opaque-carrier-walks.md`](opaque-carrier-walks.md) § The declared
  re-point axis, the declared-type walk's blockquote). **Not built, declared:**
  no door compares the signed act's invitee with the accepting principal today
  (the accept door keys on the row), so the chain-resolution rule has no caller
  yet — it binds whichever verifier arrives first, the cross-nest leg's
  included.
- **An invitation is judged again when it is accepted — ruled and built (nest
  + the shared conversations library, 2026-09-21).** Until then the accept door
  checked only that a pending row existed and seated the invitee at the row's
  role, so an invitation outlived the authority behind it: a demoted admin's,
  a departed member's, an admin invitation whose invitee the owner had since
  dropped from the admin set (seated as admin against the signed policy), and —
  the case that raised it — whatever a seed thief issued as the owner before
  the owner recovered. § Join rules and invites → *An invitation is a standing
  offer* is the ruling. Built: one judgement both doors ask
  (`judge_room_invitation` in `conversations_handlers.rs`), the inviter read by
  its own seat; the lapse as one transaction (`db::rooms::consume_pending_room_invite` —
  row, envelope, quota refund, bound to the inviter that was judged); the
  seating bound to the judged policy version
  (`accept_room_invite`'s `judged_at_version`, `RoomInviteAccept::PolicyMoved`).
  Witnessed, red first, at the API tier in `conformance_conversation_rooms.rs`
  (`an_invitation_from_an_admin_since_demoted_lapses_at_accept`, which also pins
  the three-facts consumption and that a lapse is not a ban;
  `…_from_a_member_who_has_left_…`;
  `an_admin_invitation_lapses_when_the_policy_no_longer_names_the_invitee`;
  `an_invitation_from_an_identity_since_succeeded_lapses_at_accept`, through
  the real ceremony) and at the db tier
  (`an_accept_judged_under_a_policy_the_room_has_moved_past_writes_nothing`,
  `a_lapse_consumes_only_the_pending_invitation_its_judgement_named`). App
  side, shared for all 7: a refused accept re-lists the standing invitations,
  so a lapsed one leaves the list in the act that refused it
  (`ConversationsManager::accept_room_invitation`; pinned by
  `a_lapsed_invitation_leaves_the_list_when_its_accept_is_refused`, red
  first). The cross-nest bound is the ruling's own: a foreign-homed inviter's
  succession is not seen here.
- **Pending invitations are listed and withdrawable — ruled, built on the
  nest and in the shared library (2026-09-21), and painted on tui (2026-09-25);
  the other six apps follow in the batched trickle-down.** § Join rules and invites →
  *Pending invitations are visible to whoever may withdraw them* is the
  ruling. Built: the two additive user-class kinds
  `fauna.conversations.room.list_invites` / `room.revoke_invite` (wire types in
  `fauna-protocol`), one scope predicate both doors ask
  (`pending_invite_scope` in `conversations_handlers.rs`: owner and admins
  everything, any other seated member what it issued, off the floor refused),
  the list's "would this still be accepted" computed by the accept door's own
  `judge_room_invitation`, and the withdrawal through the lapse's writer
  (`db::rooms::consume_pending_room_invite`, the scope carried into the
  transaction) over the room-scoped read `pending_invites_for_room`. Witnessed,
  red first, in `conformance_conversation_rooms.rs`
  (`the_owner_and_admins_list_every_pending_invitation_and_a_member_only_its_own`,
  which also pins the lapsed-in-waiting flag and that an accepted invitation is
  never listed;
  `a_withdrawn_invitation_leaves_with_its_envelope_and_cannot_be_accepted`;
  `a_member_withdraws_the_invitation_it_issued`), with both kinds added to the
  MLS-authority refusal and the user-class pins. Built above the wire, id-free
  and for all seven apps at once: both doors on the room-ceremony seam
  (`RoomCeremonyRpc::room_list_invites` / `room_revoke_invite`, glue in
  `fauna-client-conversations`); the list read again on the floor's own
  cadence (`FaunaMlsBackend::tend_community_room`) and rendered as
  `RoomSnapshot::pending_invites` — `None` until served, and again once the
  nest refuses the read, never an empty list standing in for an answer; and
  the manager's `withdraw_room_invite`, which acts at once, re-lists, and puts
  a refusal on the page's `error-message`. No capability gates the gesture:
  the nest served the row *because* the viewer may withdraw it. Witnessed, red
  first, in `fauna_mls_backend_tests.rs`
  (`a_community_rooms_pending_invitations_reach_its_snapshot_and_a_withdrawal_re_lists`,
  `a_pending_invitation_list_this_device_was_not_served_paints_nothing`).
  Painted on tui, the lead app (2026-09-25): the row's sentence is shared
  (`RoomPendingInviteSnapshot::text`, FFI twin `room_pending_invite_text`),
  the two ids `room-pending-invite[i]` / `room-pending-invite-withdraw-button[i]`
  are in `ui.yaml` with their contract in
  [`../ui/conversations.md`](../ui/conversations.md) § Element IDs, the
  gesture is `Action::WithdrawRoomInvite` over the manager door in
  `apps/fauna-tui/src/conversations/mod.rs`, the user guide names it
  (`docs/guides/app-tour.md`), and the two-seat journey
  `tests/e2e-unified/tests/test_conversation_room_community.py` witnesses the
  founder listing the member's invitation as pending, withdrawing it, the
  member's row leaving their list, and the re-invitation as the undo.
  **Not built, declared:** the six apps behind tui do not paint the section
  yet. And **a room
  homed on another nest lists nothing**: neither kind has a relay twin, so a
  member homed elsewhere — an admin included — neither sees nor withdraws
  (`room.remove`'s own bound; the room's home-nest members are unaffected).

## Join rules and invites

The join rules — `invite`, `member-invite`, `request` — stay in [`conversation-rooms.md`](conversation-rooms.md) § Join rules and invites; what follows is the invitation's lifecycle under them, carried verbatim.

**An invitation shares its invitee's fate, as one thing.** What the home nest
holds for an invitation is three facts written in one act — the row the accept
door reads, the un-acked inbox envelope that is the invitee's only way to learn
the room id, and the inbox-quota charge for that envelope — and no account
event may leave them disagreeing. **Account deletion removes all three**,
pending or accepted: the row is the invitee's standing with one room and serves
nobody once they are gone. **An identity succession carries all three to the
successor**, who accepts under its own key: the envelope and its charge follow
the account with the rest of the undelivered inbox, and a row left behind would
be an invitation the successor can see and the accept door refuses — with no
way out, because an invitation has no revoke door and a decline is only the
invitee acking the envelope
([`community-rooms.md`](community-rooms.md) § Implementation status today).
Nothing carried is authority a seed thief could have minted: the invitation is
the *inviter's* act, judged against the inviter's standing — when it was issued
and again when it is accepted (*An invitation is a standing offer*, below) —
and accepting is a fresh act of the successor's. The signed act itself is **never
rewritten**, so after a succession it still names the predecessor as invitee;
a verifier that compares that name with the accepting principal resolves it
through the succession chain, the floor's rule for every name in a signed
policy (§ Roles and authorization → *Successions inside a governed room*).
**The inviter is attribution**, on the invitation and on the seat acceptance
writes: an inviter's own succession or deletion rewrites nothing on an
invitation they issued, and re-pointing it would credit an invitation a thief
may have issued to the identity recovering from them. Attribution is not
validity — whether the invitation may still be *accepted* is the next
paragraph's. Bound: an invitee homed
on another nest succeeds there, and this nest learns nothing of it — the floor
roster's own bound (§ The home nest → *The succession axis*).

**An invitation is a standing offer, judged again when it is accepted.** An invitation is not
history — it is authority nobody has exercised yet, and accepting it is the
moment it is exercised. So the rule is one sentence: **an invitation is good
only while its inviter could still issue it.** The accept door runs the invite
door's own judgement again — one function, asked by both doors, so a test added
to one is a test of the other — against the floor and the signed policy as they
stand at accept: the inviter holds a live seat; the join rule lets a seat of
that rank invite; and an admin invitation's invitee is still named in the admin
set (the policy reconcile touches live seats only, so without this an invitee
the owner has since dropped would be seated at a rank the policy members verify
does not grant). An invitation from an admin since demoted, from a member who
has left or been removed, from an owner who has transferred the room away under
the `invite` rule, or from an account since deleted therefore lapses. This is
the end-to-end class's rule already, reached by construction: there acceptance
is answered by the inviter's own Add commit, which every member judges under the
policy of the epoch it lands in. It is deliberately the opposite of a delete's
rule (§ Roles and authorization → *Delete any message*), which is judged by the
policy it was made under: a delete is an act in the log that every replay must
reproduce, and an invitation is an offer that has not happened yet.

**The inviter is judged by its own seat — never through its succession line.**
Everywhere else the floor resolves a policy name to the newest live seat along
its line, which is right for a *name*: an admin's successor is an admin. It is
wrong for a pending *act*. The invitations an identity left pending when it was
succeeded are exactly the standing authority a seed thief could have minted
([`succession-repoint-axis.md`](succession-repoint-axis.md) § The declared
re-point axis, the **burn** class), and resolving their inviter through the
line would land on the recovered owner's seat and pass them on the recovered
owner's rank. The ceremony absorbs the predecessor's floor seat, so judged by
its own seat a succeeded identity holds none and everything it left pending
lapses — the thief's invitations and the owner's own alike; the recovered
identity invites again, which is the burn class's "re-granting is bounded and
visible". No separate burn writer runs at the ceremony, on purpose: the accept
door is the single decision point, a second writer would have to be kept
agreeing with it, and the row's declaration stays what it was (the invitee's
row, a plain *Move*; the inviter a *Stay* reference). Bound: an inviter homed on
another nest succeeds there and keeps its seat here under the old identity, so
its invitations keep passing — no new power, since whoever holds that key holds
the seat itself; the floor roster's own bound again.

**A lapse consumes the invitation — the three facts leave as they arrived, in
one act.** A refused accept that left the row and its envelope standing would
be the state the succession ruling above exists to prevent: an invitation the
invitee can see and the accept door refuses. So the refusal deletes the row and
acks the envelope, refunding its quota charge, in one transaction; the invitee
is told the invitation has lapsed and may ask to be invited again, and the
app's list drops it in the same act. Only the invitation that was judged is
consumed — a re-invitation that landed in between is somebody else's offer and
stands — and a storage fault is never a verdict: it lapses nothing. The seating
itself is bound to the policy version the judgement read, so a policy landing
between judgement and seating sends the accept round again rather than seating
under a policy the room has moved past.

**Pending invitations are visible to whoever may withdraw them, and
withdrawable.** The lapse closes the authorization question; this is the
product gesture beside it — an admin who regrets an invitation takes it back,
and a recovered owner *sees* what was left pending on their room. One predicate
governs both doors, so nobody is shown an invitation they cannot act on and
nobody acts on one they were not shown: **the owner and admins see and
withdraw every pending invitation of the room; any other seated member sees
and withdraws the ones it issued; a caller off the floor is refused.** The
rank half is the removal rule's (§ Roles and authorization: an owner or admin
removes a member) applied one step earlier — whoever could unseat the invitee
the moment they accept may stop them being seated. The caller is read by its
own seat, the judgement's reading above, which settles the successor question
without a rule of its own: a successor is not the identity that issued its
predecessor's invitations and does not see them as "its own" — they are inert
already — while a successor who is owner or admin sees them with everything
else. Community rooms only, like every membership door: an end-to-end room's
floor holds no invitations.

- **`fauna.conversations.room.list_invites`** serves, per pending invitation:
  the invitee and the inviter (ids, with the handle this nest knows joined
  nest-side exactly as the roster read joins it), the role on offer, when it
  was issued, and **whether it would still be accepted** — the accept door's
  own judgement, run now. That last field is what makes a thief's or a
  demoted admin's leftovers legible: they are listed, marked as lapsed in
  waiting, and can be cleared instead of sitting invisible until somebody
  tries them. Accepted invitations are history on the roster (`invited_by`)
  and are never listed.
- **`fauna.conversations.room.revoke_invite`** names the invitee and consumes
  the invitation exactly as a lapse does — the row, its standing envelope and
  the envelope's quota charge, in one transaction. **The invitee is told
  nothing**: the invitation simply stops standing, the way a decline tells the
  room nothing — a withdrawal with an audience would make regretting an
  invitation an act performed at somebody. Withdrawing an invitation that is
  not pending is answered, not refused (`revoked: false`): the caller wanted it
  gone and it is. A withdrawal is not a ban — anyone with the authority may
  invite again.

Both kinds are additive, user-class only. The gesture lands on tui first and
trickles down to the other six; its capability gating is
[`../ui/conversations.md`](../ui/conversations.md)'s.

**A cross-nest invitation — the home judges, the invitee's nest relays and decides nothing (ratified 2026-09-26; BUILT the same day).** An invitee homed on another nest reaches the room, like every foreign member, "only through their own home nest" (§ The home nest), and an invitation is that member's first act on the room, so the ceremony above is split across the two nests along exactly one line: **everything about the room is the room home's, everything about the invitee is the invitee's nest's.** Four rulings, each the shape some neighbouring leg already has:

1. **Delivery is the room home's push, on a kind of its own.** `room.invite` judges a foreign invitee's invitation exactly as a same-nest one — the inviter's live seat, the join rule, the admin set — then records the row with **no local envelope** and originates `fauna.federation.conversation.room.invite` to the invitee's home, carrying the inviter's signed act verbatim and the home's own declared address ([`../architecture/federation.md`](../architecture/federation.md) § Federation residue surface owns the call). A push rather than a pull because the invitee does not yet know the room exists — the id is inside the signed record, and there is nothing to poll for; a kind of its own rather than `inbox.deliver` (whose wire is a signed contact-request pair with its own verification) or `welcome.deliver` (whose wire carries no signed sender and so fails the reach floor closed), because an invitation has a signed inviter and the invitee's nest can therefore run the invitee's reach policy in full. The row is recorded **before** the delivery and consumed if the delivery does not land — the same-nest door's one-transaction invariant ("a recorded invitation nobody can discover and a delivered one the accept door does not know about are both states no ceremony produced", [`community-rooms.md`](community-rooms.md) § Implementation status today → *An invitation reaches its invitee*) kept across a boundary one transaction cannot span; the invitee's nest's refusal is the inviter's answer, as a same-nest reach refusal is. The room home's own reach gate is deliberately not run for a foreign invitee: their inbox mode and contact edges are their nest's facts, and the home's view of them is the empty default.
2. **The invitee's nest stores one envelope and decides nothing about the room.** It refuses an unregistered recipient (the Welcome relay's gate), verifies the signed act and that it names its member as the invitee, runs its member's own reach policy against the *inviter* — the same-nest invite gate, at federation origin, which "the group-Welcome gate applies unchanged" is met by in the stronger form a signed sender allows — binds the room's home to the delivering connection's **verified** identity (the Welcome relay's dial-proven-address rule, one helper for both), and delivers the `RoomInvite` knock under its member's inbox quota, with `room_node` set. It keeps no room record and no invitation row; what it learns of the room is the signed record's four facts and the home's identity, nothing more. The invitee's app lists the knock through the same door as a same-nest one (`room-invitation[i]` paints it with no new id) and verifies the signature before rendering a name, as always.
3. **Acceptance is a relayed door of its own, gated on the invitation rather than the binding.** The app's accept picks `fauna.conversations.room.accept_invite_remote` off the knock's `room_node` — distinct, for the `channel.actors_remote` reason: an old own-nest ignoring an additive field would run its same-nest door on a room it does not home and answer "no invitation pending", indistinguishable from a genuine lapse — and the invitee's nest relays `fauna.federation.conversation.room.accept` to the home, keeping nothing. Every other relayed room door runs behind the `channel_foreign_members` binding; this one cannot, because the binding is what it *writes* — and writing it at delivery time instead would open the binding-only doors (the relayed roster read above all) to an invitee who never accepted, the read the same-nest twin refuses as "membership is not public". So the gate is what the home recorded when it delivered: the invitation must name the calling nest's **verified** identity as the invitee's home, resolved from the delivery dial and never inviter-declared. Behind it the home runs the same-nest accept body verbatim — the standing-offer judgement, the policy-version compare-and-swap, the lapse — and only then inserts the binding with the Welcome relay's `InsertOnly` power. **Seat first, bind second**, the inverse of the removal's purge-then-unseat and for the inverse reason: a seat without relayed reach is the tighter half-state (the member reads nothing until a retry binds it), where a binding without a seat would serve the floor to a principal not on it. The door is idempotent for the relay's re-send: a requester whose invitation from that nest is already accepted and who holds a live seat is answered with its role and its binding re-asserted, which is also what converges the half-landed state. No routing-roster row is written for a foreign member — it learns of new traffic by polling through its own nest ([`community-rooms.md`](community-rooms.md) § Implementation status today).
4. **The home judges the join rule at accept, on both legs; `policy_version` is read by no door, by ruling.** The signed invitation's version was carried against the day the invitee's nest would have to judge an offer it could not re-read the policy for; that day does not come, because the invitee's nest judges nothing. The field stays what it is for the invitee's device — the version the offer was made under — and stays unread by every door.

What follows from the seat is unchanged: the founder's device keys the foreign seat on its tend pass exactly as it keys a same-nest one (the roster row carries the reception key and the home URL), the newcomer reads its wraps through `generations_remote` and the log through the relayed `channel.fetch`, sends through `send_remote`, and leaves through `leave_remote`; a removal purges the binding with the seat, and the holes that were **latent** while no foreign member could be seated ([`community-rooms.md`](community-rooms.md) § Implementation status today → *A removal severs*, *leaving does sever*) are reachable in production from this day and stay closed. An invitation *issued* by a member homed elsewhere is the leg ratified and built below, the same day. Proof of the delivery leg: `conformance_cross_nest_conversations_client.rs::a_cross_nest_invitation_seats_the_foreign_member_through_its_own_nest` (two real nests: the knock lands on the invitee's nest with the home bound, an unseated invitee's relayed floor read is refused, the relayed accept seats and binds, a nest the invitation did not go to is refused, the re-sent accept converges) and the tier_3 journey `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py` (§ Done definition).

**The foreign-inviter leg — an invitation *issued* by a member homed elsewhere is a relayed door of its own (ratified and BUILT 2026-09-26), and the four rulings above extend to it without change.** Its own nest holds no room record, so `room.invite` there answers "no such room"; the app's invite picks `fauna.conversations.room.invite_remote` off the room's recorded home — the same `ChannelHome` signal the leave and the roster relays pick by, distinct for the `channel.actors_remote` reason (an old own-nest ignoring an additive field would answer that "no such room" as a clean refusal the seam cannot tell from a mistyped id) — and the inviter's nest relays `fauna.federation.conversation.room.invite_issue` to the home, keeping nothing: it binds the signed act to the member it authenticated (the same-nest door's own first act, run on both nests, so no nest ever relays an invitation under another's name) and decides nothing else. The home gates the relayed act on the inviter's **foreign-member binding** (`require_foreign_member`, the `channel.fetch` gate verbatim — a removal purges it with the seat, and the body's own seat read refuses whoever a purge missed) and then runs **the same-nest invite body verbatim** (`conversations_handlers::room_invite_apply`: the floor-authoritative check, the join-rule judgement, the already-a-member refusal, the delivery), so `member-invite` grants a foreign member exactly what it grants a member homed here and `invite` refuses it exactly the same — the rule is a rank, never a homing. What the issue door adds is only where the knock goes, and that is one body's three arms rather than three doors: `invitee_node` keeps its inviter-side meaning — the invitee's home as the inviter knows it, empty for the inviter's own nest — and the home resolves an empty node to the **relaying nest's verified identity** (the Welcome relay's dial-proven-address rule, never the inviter's declaration) and pushes ruling 1's knock back to it; a node naming the home itself is served as a same-nest delivery into the invitee's own inbox under ruling 2's reach gate run at federation origin (a nest never dials itself); any other node is ruling 1's push to that third nest. Each invitee then accepts as ruling 3 says — the relayed accept from the nest its knock went to, or the plain same-nest accept on the home — and ruling 4 holds as stated. Replay is forbidden on both hops, the same-nest door's own posture and unlike the leave and accept twins, because the issue door delivers a knock per call: the inviter re-issues, and the pending invitation is refreshed, never duplicated. The tui journey is unchanged — the picker gesture is the same, only the seam's routing grew — so no app learns of the leg. Proof: `conformance_cross_nest_conversations_client.rs::a_foreign_member_invites_through_its_own_nest_under_member_invite` (three real nests: an unseated principal's relayed act is refused at the gate with nothing recorded, an act relayed under another member's name is refused before it travels, the inviter's own same-nest door fails loud; a seated foreign member under `member-invite` then seats an invitee on its own nest, one on the room's home and one on a third nest through their respective accepts, and the already-a-member refusal and a flip to `invite` refuse it as they refuse a member homed on the room's home).
