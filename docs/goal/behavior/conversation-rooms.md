# Conversation rooms — target state

Owns: room-model
Status: ratified — the room model (TP8's four remaining decisions) ratified 2026-09-08 by the conversations owner; the package containing the four recommendations was accepted by the user 2026-09-05 ([`../architecture/third-party.md`](../architecture/third-party.md) § The rulings — TP8). One sub-decision was settled by the ratifying session rather than put to the user — the community class's key model, § The three classes → *Community* — and was refutable until the first community-room build; that build landed 2026-09-09 and did not refute it (§ Implementation status today, *The sealing lands*), so the window is closed.
Authority: **the room model** — the one membership model every conversation shares regardless of transport: what a room is, who may be a member (principals with keys), the three confidentiality classes and the rule that derives a room's class from its member set, the per-class key model (the community class's own detail — its key model, who wrote a message, its attachment kind and what the home nest does with its read — together with the class's build record is [`community-rooms.md`](community-rooms.md)'s, split out 2026-09-10), roles and how each class enforces them, join rules and invites (the invitation's lifecycle once issued — its fate, its judgement at accept, its listing and withdrawal, and the cross-nest legs — is [`room-invitations.md`](room-invitations.md)'s, split out 2026-09-28), the home nest and its transfer, the per-room history-for-joiners policy, and the fate of the duplicate `fauna.conversations.group.*` plane. Defers the MLS channel protocol (Welcome, key packages, channel send/fetch, the N-member fork) to [`direct-messages.md`](direct-messages.md); the page UX, capability gating and the `Bridged` adapter to [`../ui/conversations.md`](../ui/conversations.md); the sequencing seat to [`p2p.md`](p2p.md) § Offline share initiation; the recipient-set key scheme to [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md) § The recipient-set scheme (mechanics) and [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md) § Audience: a storage group (key material); identity succession to [`identity-succession.md`](identity-succession.md) + [`succession-propagation.md`](succession-propagation.md); per-content-kind at-rest rows to [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md); the custody serve door to [`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md) § Shared-audience carve-out; and the dormant group plane's as-built wire contract to [`groups.md`](groups.md).

Last verified: 2026-09-26 (three passes — the third: the group plane's retirement executed, steps 1–3 — § The group plane's fate, § Done definition; the second: the foreign-inviter leg ratified and built, a member homed elsewhere issuing through its own nest under `member-invite` — [`room-invitations.md`](room-invitations.md) § Join rules and invites → *A cross-nest invitation*, the closing paragraph; § The home nest, the issue relay; the first: the cross-nest invitation ratified and built — [`room-invitations.md`](room-invitations.md) § Join rules and invites → *A cross-nest invitation*; § The home nest, the admission relay; § Done definition, the community box's second-nest clause); 2026-09-23 (three passes — the third: the anchor store's failing read backed off, one read per 10 s — § Implementation status today, the *Delete any message* bullet; the second: every verified policy-name line, positive or empty, is asked again on the same bounded cadence and only ever grows, and a rank refusal parks whenever a witness is registered — § Roles and authorization → *A name designates its verified line* → *A verified line holds only so far*; § Implementation status today, the *Delete any message* bullet; residual *(g)* closed, *(h)* restated; the first: a policy name verified as never-succeeded is asked again on a bounded cadence and parks a rank refusal meanwhile — the same sections, residuals *(g)*–*(h)* declared); 2026-09-22 (every community policy version bound to its room, and a room born requiring the binding — § Roles and authorization → *A fetched version is anchored, never believed* and *A room is born requiring the room signature*; § Implementation status today, the *Delete any message* bullet, residual *(f)*); 2026-09-21 (what a device accepts as a joiner's history ruled and built — § History for joiners → *What a device accepts*; the floor's signature check pinned at last and the “each red-verified” claim made true — § Implementation status today, the *Delete any message* bullet; residual *(e)* closed, a retained policy version is no longer rewritable; what account deletion and succession do to a retained version ruled — § Roles and authorization → *Members verify what they paint*; an invitation's fate under account deletion and succession ruled and built — § Join rules and invites); 2026-09-20 (the tombstone-across-a-restart question SETTLED — § Implementation status today, the *Delete any message* bullet: the `history/<ch>` slice now carries the projection's tombstone outcome, so a cross-sender delete survives a relaunch on the role the rail recorded at fold; the succession chain/parked-set residual at § Implementation status today (d) was un-filed from that question, which no longer covers it); 2026-09-13 (the retirement of the succession-axis bullet's declared bound (2) — the successor's wrap target, built as `room.set_reception_key` and recorded in `community-rooms.md` § Implementation status today → *A seat gains or rotates its wrap target*; the community class's floor succession leg, the policy names' chain resolution and the owner's account-deletion rule — § Implementation status today, the succession-axis bullet; § Roles and authorization); 2026-09-10 (eight passes that day — the sixth built the invitation's delivery through the inbox plane, § Implementation status today → *An invitation reaches its invitee*, which also re-scoped the cross-nest-invite residue; the seventh the membership doors and the severance rotation, § Implementation status today → *A removal severs*; the eighth the two policy-authoring doors and the roster read's policy bytes they needed, → *The policy doors reach a client*; the fourth and fifth the design passes that ratified the home nest's read purposes, § The three classes → *What the home nest does with its read*, and then the room-restricted post arm its purpose 3 points at, `restricted-posts.md` § Encryption at rest; the first three: the id-keyed handle read and the roster's decided federation answer; the add's retained-generation backfill; and the foreign member's generation-read relay, whose remaining gap was re-scoped after `fauna.federation.channel.append` was found already inheriting the community floor gate through `channel_send_core`. § Implementation status today's room-plane bullets re-checked against the tree that day) | Sources: `libs/fauna-conversations/src/{address.rs,capabilities.rs,thread.rs,backend.rs}`, `libs/fauna-core/src/{group_content.rs,group_generation.rs}`, `libs/fauna-mls/src/{room_policy.rs,wrapped_blob/group_generation_wraps.rs}`, `bins/fauna-nest/src/{conversations_handlers.rs,custody_admission.rs,federation_handlers.rs,room_read_key.rs,db/rooms.rs,db/groups.rs,db/migrations.rs}`, `libs/fauna-protocol/src/{offline_class.rs,inbox.rs}`, `libs/fauna-client-inbox/src/lib.rs`

## Goal

Every conversation a user has — a Fauna chat, an email thread, a bridged Matrix
room, a bot channel, a community's open channel — is a **room**: one membership
model, one set of roles, one message log, one vocabulary of operations. The word
"rail" used to entangle three things; this doc separates them ([`../architecture/third-party.md`](../architecture/third-party.md)
§ The rulings — TP8):

1. **The room** — the membership model, independent of transport: its
   participants, their roles, its join rule, the message log, the semantic
   operations (send, react, delete, invite, remove, rename, leave).
2. **Transport adapters** — what the `RailBackend` trait already is once
   membership is lifted out of it: each adapter projects its network onto the
   room model, lossily where the network is poorer, the loss declared by the
   capability vector. Owner: [`../ui/conversations.md`](../ui/conversations.md)
   § Where logic lives.
3. **Members with keys** — a room's **confidentiality class derives from its
   member set** (§ The three classes). A principal that holds a key may be a
   member: a user's devices, the room's home nest, a bridge principal, a mail
   transfer agent. What a member can read is exactly what its membership says.

Nothing here changes MLS. MLS is the IETF standard for large dynamic groups and
stays the end-to-end class's mechanism; what MLS deliberately leaves to the
application — authorization, history for joiners, the size class, the federation
home — is exactly what this doc ratifies.

## Implementation status today

**The room model is ratified target state; the end-to-end class's shared-Rust
half is built (2026-09-08), and the nest plane (the floor roster, below) and
the community class (its own build record: [`community-rooms.md`](community-rooms.md)
§ Implementation status today) have been landing since 2026-09-09.**
What exists, and the gap each ratified shape declares:

- **Two group mechanisms coexist on the native rail.** Mechanism **A** — the
  live one: adding a participant to a `(FaunaMls, OneToOne)` thread forks an
  N-member MLS channel ([`direct-messages.md`](direct-messages.md) § Group-Forked
  Threads). Under this doc it is the **end-to-end class**, and since
  2026-09-08 a group so forked is born **governed**: its MLS group context
  carries the owner-signed room policy (`libs/fauna-mls/src/room_policy.rs`;
  minted by `FaunaMlsBackend::bootstrap_group`). Every current app's key
  package advertises the policy extension, so a peer package that does not
  is a non-conforming client's and the fork **fails with the engine's
  by-name refusal** — it is never born policy-less instead (the older-app
  fallback that once minted such a group was a compat remnant, removed
  2026-09-25 under [`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md)
  § Program 4). A **policy-less room** —
  no policy, open membership, rendered with no roles — is a live shape of
  its own, never the product of a fallback: a 1:1 (not a group, § The room),
  a folder-share group (minted through the plain `create_group` and governed
  by its owner-managed-roster marker instead — `libs/fauna-client-folders/src/mls_adapter.rs`),
  or a group a peer minted without a policy; a policy-less room can never
  acquire a policy in place. Mechanism **B** — the retired one: the
  nine `fauna.conversations.group.*` kinds with named groups,
  `owner`/`admin`/`user` roles, invites, a sign-over-CID *plaintext* send path
  and the `groups` + `group_members` tables, nest-built and
  conformance-tested, reachable from no shipped app, and **deleted 2026-09-26**
  under the alpha carve-out ([`groups.md`](groups.md)). Under this doc its
  roles and invites became the room model's, its plaintext path the ancestor
  of the **community class**, and § The group plane's fate records how it ended.
- **The room object is on the thread for the native rail** — `ThreadDetail::room`
  (`libs/fauna-conversations/src/room.rs`: the class derived by
  `derive_room_class` from the members' principal kinds, one role per member
  index-parallel with `participants`, the policy's rendered fields, the
  viewer's own role), projected onto every emit by the thread's rail
  (`RailBackend::room_state`) and never computed by an app.
  `ThreadEncryption` is the class's render there (end-to-end ⇒ `E2E`; the
  community arm is spelled `NestReadable`; transport-only ⇒
  `TransportOnly`) — trivially end-to-end today, since this rail seats only
  user principals. **Mail and the bridged rail derive the transport-only
  class the same way** (`room::transport_room`), and no per-rail constant is
  left ([`../ui/conversations.md`](../ui/conversations.md) § Implementation
  status today); `Rail` is closed over `FaunaMls | Smtp | Bridged` and
  `ThreadFlavor` stays `OneToOne | MlsGroup | SubjectKeyed`.
- **Roles are enforced cryptographically in a governed end-to-end room, and
  nowhere else.** `MlsEngine::process_commit` judges every staged commit
  against the policy in the agreed group context
  (`fauna_mls::room_policy::judge_commit`) and refuses one its author's role
  does not permit through the same durable refused-commit memo the folder
  commit policy uses; the shared Rust refuses the same gesture **before** a
  commit is authored (`can_invite` / `can_remove_members` / `can_set_policy`
  / `can_appoint_admins` and the role-aware `supports_rename` on
  `ThreadCapabilities`, overlaid by `RoomSnapshot::gate`), so an honest app
  never posts a commit every other member refuses. A policy-less room enforces
  nothing, as before. (Mechanism B, the retired group plane, enforced its
  three at the nest only.)
- **Delete any message is built for the end-to-end class, tui-led
  (2026-09-19), and for the community class in shared Rust and at the floor
  (2026-09-20), short of successions and with no app journey yet** — § Roles
  and authorization → *Delete any message — the mechanism* owns the rule.
  *End-to-end:* `MlsEngine::decrypt_with_sender_role` reads the authenticated
  sender's role from the group context of the epoch the message was sealed in
  (`None` — fail closed — for a policy-less room or a message from an epoch the
  group no longer holds); the MLS ingest records it with the claim
  (`fauna_conversations::DeleteClaim`: author, the delete's own log position,
  role), several claims per message; the projection admits a claim on the
  sender match **or** a governing recorded role (`DeleteClaim::admits` — the
  sender match untouched); and `MessageSnapshot::can_delete` is the affordance
  (own, or `RoomSnapshot::viewer_deletes_any` — owner or admin of a governed
  end-to-end room), which `ConversationsManager::delete_message` re-checks
  before any send. tui paints the delete button off that flag; **the other six
  apps still gate on `is_own`**, so an owner or admin there is offered nothing
  new until the trickle-down lifts them (they already honour and paint the
  tombstone — that half is shared Rust). Pinned by
  `an_owners_delete_of_a_members_message_tombstones_it_and_a_members_does_not`
  (`fauna_mls_backend_tests.rs`, red-verified with the role check removed),
  the three `manager_integration_tests.rs` claim/affordance tests, and the
  three-seat tier_3 `test_conversation_room_delete_any.py`. **Settled
  2026-09-20: a tombstone survives a restart.** It did not when this rule
  landed — claims and the local deleted set were manager memory, projected onto
  the snapshot and never written into the `history/<ch>` slice, which was one
  question for both rules and lost the sender's own delete equally. The slice
  now carries the projection's outcome: the folded `deleted` flag on each
  message *and* the set of tombstoned ids the restored device re-seeds its
  projection from, so a relaunch whose re-walk can no longer open the `Delete`
  keeps the tombstone — including the cross-sender one, admitted exactly on the
  role the rail recorded at fold, because what is persisted is that admission's
  verdict rather than the claim it was made from (mechanism + the reaction half:
  [`devices.md`](devices.md) § Cross-device MLS group-state sync). Pinned by
  `a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore`
  (`manager_integration_tests.rs`, red-verified). *Community:* the class's
  half — the sender's own delete and reactions, the floor delete and its
  retained policy versions, the anchor, the succession witness, and residuals
  *(a)*–*(h)* — moved 2026-09-28 with the rest of the class's build record
  to [`community-rooms.md`](community-rooms.md) § Implementation status
  today → *Delete any message, the community class*.
- **The floor roster is stored, and the custody serve door reads it
  (2026-09-09).** The nest holds `rooms` + `room_members` (schema 51), written
  by `fauna.conversations.room.roster_report` — the report kind the
  `RoomRosterReporter` seam was declared against. A report replaces a room's
  roster **wholesale**; a principal it drops is `Removed`-absorbed rather than
  deleted, which is the shape the succession axis declares. The conv custody
  serve door reads it for every class, the `group_members` read gone
  ([`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md)
  § Shared-audience carve-out) — the first code step of § The group plane's
  fate, taken before any group kind or table is retired. The conformance conv
  arm's leave-severs case now drives a live room: a member's report seats the
  roster, and the departing member's own final report severs serving at the
  very next request
  (`bins/fauna-nest/tests/conformance_custody_nest_door_client.rs`).
  **The client reports (2026-09-09).** `NestConversationsRpc` and
  `WsConversationsRpc` implement `RoomRosterReporter` beside the
  `ConversationsRpc` they already carry — the `LinkPreviewRpc` shape, a
  separate trait on the same glue object rather than a method on
  `ConversationsRpc`, whose ten implementors are mostly test doubles with no
  room plane — and all four glue sites (`fauna-ffi`, `fauna-wasm`, linux, tui)
  install it with the same object they already built for the backend. So a
  membership commit on a governed room now reaches its home nest's floor
  roster on every app. **The producer gap is CLOSED on the lead app
  (2026-09-20):** the departing-member report the nest accepts now has a
  producer and tui paints the gesture — see the leave bullet below. The other
  six apps still reach a departure only through some other member's next
  commit, until the batched trickle-down lifts the control.

- **Leaving is built, tui leading (2026-09-20).**
  `ConversationsManager::leave_room` is the one verb, and both classes take
  the one self-scoped door (§ Roles and authorization → *Leaving — the
  mechanism*, amended 2026-09-23): `FaunaMlsBackend::leave_room_by_ceremony`
  opens `room.leave`, whose body `room_leave_apply` purges the binding only on
  a floor-authoritative room and unseats the caller on either. Before the
  amendment, an end-to-end departure was the final roster report outright —
  minus this account, at no position — and a leaver behind the newest
  membership commit rolled the floor back; the door replaced that
  report as the departure, and the compat-remnant sweep's fourth ratified
  exception
  ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)
  § Dimension 2) removed the report's residual fallback for a home nest older
  than the door, since none remains. The report door still refuses an
  owner-less report against a stored floor that names an owner. Pinned at the
  door by `conformance_conversation_rooms.rs`'s `an_end_to_end_*` cases and
  `a_role_less_report_cannot_un_govern_a_governed_floor`.
  `ThreadCapabilities::can_leave_room` carries the roles
  table's row (admin and member ✓, owner — transfer first) and is projected
  for drivers on the e2e state provider. `room-leave-button` +
  `room-leave-confirm` sit in tui's room policy editor below Save and outside
  its staged set (user-approved 2026-09-20 under rule A;
  [`../ui/conversations.md`](../ui/conversations.md) § Element IDs owns the
  paint contract). Pinned by `fauna_mls_backend_tests.rs`'s five leave cases,
  tui's own editor test, and the three-seat journey
  `test_conversation_room_leave.py` — which is also
  `docs/features/group-conversations.md` outcome 9's witness, which windows
  also runs. windows paints both ids in its editor, whose door is live for
  every member with the selects and Save greyed off `can_set_policy`. **What
  is not built:** the other five apps (web, linux, macos, ios, android); apple and android carry a
  graded `absences` entry in the offline-gate oracle until it lands.
  **Declared gap — the leaver's own room verbs do not close.** `my_role` is
  read from the room's signed policy, which still names the leaver until a
  remaining owner or admin re-signs it, so the leaver's device keeps rendering
  the rank it held. Closing it wants a *departed* reading of the floor — this
  device has read the room's floor and it does not name this account — which
  would also cover being removed by someone else, and is its own slice.
  **Declared residue — a remaining member's next commit re-seats the leaver.**
  The leaf stays until someone commits the Remove (§ Roles and authorization
  → *Leaving — the mechanism*), so a remaining member's engine roster still
  names the leaver, and the report its next membership or policy commit owes
  puts them back on the floor. The close is a remaining owner's or admin's
  device reconciling a departed floor row by committing the Remove; until
  then the departure holds only until the room's next membership change.
  `a_remaining_members_next_commit_re_seats_the_leaver_until_the_leaf_goes`
  pins it deliberately — that test turns red on the day the hole closes, which
  is exactly when this paragraph should be deleted.
- **The birth report (2026-09-10).** Until this,
  every report followed a membership or policy commit, and `bootstrap_group`
  made none — so a fresh end-to-end room had no floor roster until its first
  later commit, and a **1:1 never had one at all** (no policy, and never such
  a commit), leaving the custody serve door failing closed for every DM
  exactly as before the roster build, while this doc's done-item and
  `account-replica-posture.md` said "every class". Now `FaunaMlsBackend::send`
  reports the roster its creation produced, once, **after the first post** on
  a channel it just bootstrapped: that post is what registers the creator on
  the channel's routing roster at the room's home nest, which the report
  door's first gate requires, and the creator's channel is same-nest by
  construction. The birth report names **no log position** — the group's
  creation is not on the room log, the door's bootstrap bound admits a
  self-naming first report on an empty floor, and "no commit of its own to
  name" is exactly what an unordered report is for. Roles are the policy's
  when the room carries one and absent when it does not (a 1:1, a policy-less
  group — the wire's and the door's own model; `report_roster`'s governed-only
  gate was drift against both and is gone, so a policy-less group's later commits
  report too). The nest changed nothing: role, policy version and position
  were already optional at the door, and the custody door reads membership,
  never rank. Pinned by
  `fauna_mls_backend_tests.rs::a_fresh_1_1_reports_its_role_less_birth_roster_once_after_its_first_post`
  (the ordering witnessed through the mock nest) and
  `…::a_fresh_group_reports_its_birth_roster_with_the_roles_its_policy_gives`
  (every group is born governed since the 2026-09-25 fallback removal, below),
  and at the door by
  `conformance_conversation_rooms.rs::a_policy_less_room_reports_members_with_no_roles`
  — the birth report's exact shape, admitted by `is_room_member`. The birth report is
  best-effort — undelivered on a transport fault, skipped when no reporter is
  wired, lost to a crash between the first post and the report — and a room
  never bootstraps again, so such a room's home has no floor, and a 1:1 would
  never get one, carrying no membership or policy commit to ride;
  `FaunaMlsBackend::backfill_floor_roster` closes that on the poll, reporting
  once per channel and only when a roster read comes back empty-handed, since
  an unpositioned report to a floor that already answers would be taken
  wholesale and roll it back (2026-09-11). A current build still produces the
  floor-less room, so the backfill is crash and delivery recovery, ruled LIVE
  and kept 2026-10-02 under the compat-remnant sweep
  ([`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md));
  its "born before the birth report" wording was the pre-sweep reading and is
  gone. The one-off scheduling channel
  (`deliver_scheduling_imip`) is not a room on the page and owes nothing; and
  no shipped app mints a conv-scope custody grant today, so the door's DM
  admission closes a latent hole rather than a live outage.
- **The floor roster's authorization is a ratchet, with a declared bootstrap
  bound.** A report is admitted from a caller on the channel's routing roster
  that is a **live member of the roster it replaces** — true of an add, a
  remove and a departing member's final report alike, and false for the case
  that matters: naming yourself in the *new* roster is not sufficient, so a
  departed member cannot walk back in past the routing row that deliberately
  survives departure. That ratchet is what makes the roster the
  *non-self-assertable* record the custody door was declared to wait for.
  **The bootstrap bound is now bounded to the end-to-end class (2026-09-09).**
  A room with no stored roster still has nothing to ratchet against, so its
  first report is admitted from any routing-roster actor that names itself.
  What changed is which rooms can be in that state: the report door now
  refuses outright any room carrying a **birth record** (the create ceremony
  below), because a room has exactly one membership authority and a
  ceremony-founded room's is its floor. So the self-assertable window is
  exactly "an end-to-end room before its first report" — a room whose every
  read is MLS's, where a forged roster wins the custody door's admission to
  sealed bytes it cannot open and nothing else. What stands in the way even
  there is knowledge of the 32-byte channel id and a race against the room's
  own first report.
  **The gate is not tidiness, and its absence was exploitable.** Without it,
  an ordinary member of a *community* room satisfies both existing gates — it
  is on the routing roster after one send, and it is a live member of the
  stored roster the ratchet checks — so it could replace that room's
  authoritative roster wholesale, name itself owner and `Removed`-absorb the
  real one. The ratchet cannot catch that: the attacker is exactly the live
  member the ratchet exists to admit. Provenance is what separates the two
  doors, and it is stored as the birth salt rather than derived from the class
  so that write authority cannot swing with a membership change.
- **The community class's own build record moved to
  [`community-rooms.md`](community-rooms.md) § Implementation status today
  on 2026-09-10**, split out with the class itself (§ The three classes).
  Its entries there, by lead-in — a text search for any of them lands here:
  *A community room can be founded, and does nothing yet* (with *The
  membership half lands with it*, *The governance half lands with it too*,
  *The send path is floor-verified*, *The sealing lands*, *The tip is the end
  of the parent chain*, *The home nest's read, and its revoke* and *The key
  model's refutable window*); *The search index becomes readable* (with *The
  community-as-audience post pass lands too* and *Labelled too*); *Labels
  land* (with *The bytes half lands* and *The editor's shared half lands the
  same day*); *Attribution lands*; *The client half's read and send land*; *A
  community room can be FOUNDED from an app* (with *And the join, in the same
  landing*); *An invitation reaches its invitee*; *A removal severs, because
  the client rotates*; *The policy doors reach a client* (with *And the header
  stops lying about the class*); *The class reaches the app layer*; *The add's
  backfill lands* (with the class's unbuilt residue — re-homing, the
  `request` knock door; the cross-nest invite was built 2026-09-26, [`room-invitations.md`](room-invitations.md) § Join
  rules and invites → *A cross-nest invitation*); *The roster read serves the wrap
  targets*; and *The community room's attachment key*. The entries that
  follow are the room model's own: the report door, the end-to-end class,
  the home nest's attachment bytes and the succession axis.
- **A foreign member's roster report reaches the room's home nest
  (2026-09-10).** § The floor roster has the committing device report to the
  room's **home** nest; a member homed elsewhere reaches it the way it reads
  it: the report seam carries the channel's recorded home
  (`RoomRosterReport.home_nest_url`, from `FaunaMlsBackend::channel_home_url`
  — the same signal `read_roster` routes on), the glue picks the **distinct**
  kind `room.roster_report_remote { room_id, nest_url, members,
  policy_version }` on `Some(url)`, the member's own nest originates
  `fauna.federation.conversation.roster.report` to the home
  (`federation.md` § Federation residue surface, the *room roster report*
  row), and the home applies the same body its same-nest door runs
  (`conversations_handlers::room_roster_report_apply`: the roster validation,
  gate 0 provenance, the gate 2 ratchet, the wholesale replace) behind the
  family's structural gate `require_foreign_member` in place of the
  routing-roster gate 1 — strictly stronger, since the `channel_foreign_members`
  row is the home's own record of the Welcome it relayed, not a
  `channel.send`-self-registered row. The relaying nest stores nothing. The
  bootstrap bound above is therefore unchanged for a relayed first report: it
  is admissible only from a Welcome-bound member naming itself, never from a
  routing-roster actor. Proof:
  `conformance_cross_nest_conversations_client.rs::a_foreign_members_membership_commit_reaches_the_rooms_home_floor`
  (a foreign admin's Remove lands on the home floor; the relaying nest holds no
  room).
- **Reports are ordered (2026-09-10).** § The floor roster's ordering rule is
  built in the one shared body both doors run
  (`conversations_handlers::room_roster_report_apply`), so the same-nest and
  the relayed report are ordered alike. The wire carries the position
  additively — `commit_seq` on `room.roster_report`, on
  `room.roster_report_remote` and on the federation leg, which the relaying
  nest forwards; `superseded_by` on the ack — and the floor keeps it in
  `rooms.roster_commit_seq` (schema 62, additive nullable). The bound is the
  channel's `channel_commit_watermark`, which a relayed member's commit
  advances too, since `fauna.federation.channel.append` runs the same
  `channel_send_core`. The client names the position its commit's own send
  answered: all three commit paths return it (`admit_member_locked`,
  `evict_leaf_locked`, `commit_room_policy_locked`, gated or not), and the
  ownership transfer's completing commit reports it too. **Declared
  residue:** a report that follows no commit (a leave, the birth report, the
  floor backfill) names no position and applies unordered, so it can still
  roll the floor back until the next ordered report lands. Proofs:
  `conformance_conversation_rooms.rs`
  (`a_report_arriving_after_a_later_commits_report_rolls_nothing_back`, the
  in-order, bound, compat and ratchet-first cases),
  `conformance_cross_nest_conversations_client.rs::a_foreign_members_membership_commit_reaches_the_rooms_home_floor`
  (the position crosses both hops, and a stale relayed report changes
  nothing), and `fauna_mls_backend_tests.rs` (the report names its commit's
  position).
- **A positioned report is bound to its commit's sender (2026-09-11).**
  § The floor roster's authorship rule is built in the same shared body both
  doors run, one bullet after the bound: a report naming the **newest**
  commit's position is refused from anyone but the actor this nest observed
  sending that commit. The nest records that actor where it records the
  position — `channel_commit_watermark.last_commit_sender` (schema 64,
  additive nullable), written by `segments::conv::append_locked` under the
  same per-channel conv seq lock as the mark and only when the mark advances,
  so the pair always names one commit. The actor is the one each door
  authenticated: the `channel.send` caller same-nest, the
  `requesting_actor_id` the home bound at `require_foreign_member` for a
  relayed append ([`../architecture/federation.md`](../architecture/federation.md)
  § the room roster report row). **Declared residue:** a position *below* the
  newest commit is admitted — only the newest commit's sender is kept — and so
  is a position on a room whose mark predates the column, until its next commit
  rewrites the pair; both are narrower than the mirror's standing trust in a
  live member's report, since a report anchored below the honest one that
  follows is replaced by it. The reporting device now **reads the ack**: the
  report seam answers stored / superseded-at-a-position / undelivered rather
  than a bare delivered flag, so a report the home took but did not apply is
  warned and tallied (`RosterReportCounts::superseded`) instead of counting as
  a success — until this, `superseded_by` was computed by the nest and dropped
  by the glue, so no device could tell its own report from the one that
  replaced it. Proofs: `conformance_conversation_rooms.rs`
  (`the_member_a_commit_removes_cannot_claim_that_commits_position`,
  `authorship_binds_the_newest_position_and_nothing_else`),
  `db::channels::the_commit_marks_sender_is_paired_with_its_seq`, and
  `conformance_cross_nest_conversations_client.rs::a_foreign_members_membership_commit_reaches_the_rooms_home_floor`
  (the relayed leg: the home records the foreign member as the commit's
  sender, refuses a same-nest report at that position, and the stale relayed
  report comes back naming the position the floor holds). The column's
  succession verdict — and why the nullable column's permissive reading of it
  is wrong — is ruled at
  [`succession-repoint-axis.md`](succession-repoint-axis.md) § The declared
  re-point axis (the 2026-09-11 entry).
- **A removal severs on the end-to-end class too, by reading the floor rather
  than purging the binding (2026-09-11).** A foreign member reaches a room
  homed elsewhere through a `channel_foreign_members` binding, and three
  relayed doors are gated on that row *alone*, by ratified design — the roster
  read, `channel.actors` and the attachment write-token mint
  ([`../architecture/federation.md`](../architecture/federation.md)
  § Federation residue surface, the *room roster read* row). A **community**
  room ends that admission in its `room.remove` door, which purges the row
  before it unseats ([`community-rooms.md`](community-rooms.md)
  § Implementation status today → *A removal severs*). **This class has no such
  door** — `room.remove` refuses a room whose membership authority is its MLS
  group — so its removal arrives as a membership commit plus the committing
  device's roster report, and the report's absorb stamps `removed_at` on every
  live row the new roster did not name and purges no binding. Left there, the
  binding outlived the seat on the one class where cross-nest membership is
  actually built: a removed member's home nest kept being served the room's
  **live** floor. So the three doors now also refuse a requester the floor
  **positively says** was removed, while one with no floor row at all stays
  admitted — the newest-seated member, whose co-members are still nameless, is
  exactly what the binding-only gate is ratified to protect, and absence is not
  a removal. **The two ceremonies are complementary, never cumulative:** the
  floor read is skipped on a floor-authoritative room, exactly the class
  `room.remove` serves, so every room has one severance mechanism and none has
  two. That skip is required rather than an optimization — on the purge's class
  the binding *is* admission and the seat a separate act, so a re-invite's
  relayed Welcome re-inserts the purged binding and must be served while the
  `removed_at` row the purge superseded still stands, and a floor read there
  would blacklist an actor the room chose to take back.
  **Reading the floor rather than purging on absorb is the deliberate choice,
  and the reason is the mirror's own trust.** § The floor roster's ratchet
  admits a departing member's final report (refusing it would leave the
  departure invisible until some other member happened to commit), so a purge
  driven by the absorb would let one live member's false report sever **every**
  co-member's binding — and the Welcome relay's insert arm holds `InsertOnly`
  power, so no later honest report could write one back, while a member still
  inside the MLS group has no re-Welcome ceremony to be re-admitted by. That is
  client-causable unrecoverable nest state
  ([`../principles.md`](../principles.md) § No client-causable unrecoverable
  nest state). A refusal *derived* from the floor carries the same denial and
  none of the destruction: the next honest report's upsert clears `removed_at`
  and the member's reach returns with its seat.
  **Declared residue — the re-add window.** A member re-added by a fresh
  membership commit is refused until that commit's roster report lands: the
  report is the only signal this nest has, and the binding's own stamps cannot
  tell a re-add from the removal before it, the binding never having been
  purged on this class. The window is one the committing device already owes a
  report for, it heals at the next report from any device, and it fails
  **closed** — a briefly elided roster on the re-added member's own device,
  never a read for someone off the floor. It is the re-admission form of the
  lag the binding-only gate is ratified against, narrowed from "every
  newest-seated member" to "a re-added one, until the report it is already
  owed".
  **Declared residue — ciphertext.** `channel.fetch`'s drain stays open on this
  class where the community class's purge shuts it. That is the MLS position
  rather than a gap — the removing commit re-keys the group, so what a removed
  member may still fetch it cannot open, and the residue is the one its own
  retained keys already are. The generation read needs nothing: it resolves the
  wraps to serve from the requester's own **live** floor entry, so a removed
  member is answered nothing by construction, and the room-post verdict read
  already stacks `is_live_floor_member`. Proof:
  `conformance_cross_nest_conversations_client.rs::a_removed_foreign_member_of_an_end_to_end_room_loses_the_three_binding_only_doors`
  (two foreign members on one nest, one removed by the owner's MLS commit: all
  three doors refuse her at the wire and degrade to `None` at the client seam,
  all three still serve him, her binding survives the absorb, a member bound
  with no floor row at all is still served by all three, and a later honest
  report reopens every door).
- **On every member's own device, a governed room's rendered roster
  follows the group's agreed roster (2026-09-09).** After an inbound
  membership commit advances the epoch, the chat-plane receive loop hands
  the manager the engine's member set
  (`ConversationsManager::apply_inbound_roster`), so someone removed by
  another member leaves this device's participant list too — keeping a
  recorded succession's predecessor for the in-place re-point, since the
  succession statement can be parked and re-driven later. A policy-less room
  keeps the older behaviour (only the committing device edits its own
  list). **The add arm landed the same day:** a member *added* by someone
  else now joins this device's list from the agreed roster alone, seated
  **handle-less** — a `TypedAddress::Fauna` with an empty handle, exactly as
  a Welcome's members are seated, because the engine roster carries actor ids
  and nothing else. It appends; it never seats the local user; and it never
  seats an actor a held predecessor already stands in for, since that row
  *is* the successor's seat until the verified statement re-points it.
  ⚠ **Appending is not a guarantee an open editor may lean on** — the drop
  arm above removes from the *middle*, shifting every later row left, and
  that is a live-list position an editor's staged state must never be keyed
  to. The room-settings editor was keyed that way and could carry the owner's
  staged appointment onto whoever slid into the slot, signed with the owner's
  own credential; `RoomSettingsDraft`
  now carries an identity column and resolves every gesture, paint and Save
  through it, so no consumer depends on the row order holding still.
  **Seat-time handle resolution landed 2026-09-09**, as the one follow-on
  serving both this arm and the Welcome's:
  `ConversationsManager::seat_address_for` is the single path both seating
  sites now call, and it resolves the actor id against the handles this device
  is already rendering in its *other* threads
  (`ConversationsManager::handle_for_person`), so a member met before arrives
  by name. It does no I/O — an in-memory scan — which is what makes it safe on
  the add arm's path, inside the inbound poll's channel lock. A handle read
  that way is display only and never an identity claim; every membership
  decision still keys on the actor id. Nor is a handle this scan or the
  roster read below writes ever a *succession* anchor: which participant
  handles a succession's tier 2 may dial is owned by
  [`identity-succession.md`](identity-succession.md) § The succession
  statement → *which participant handles anchor tier 2* — only a handle the
  owner's own gesture put on the row qualifies, and the thread store carries
  that provenance beside the row (ratified 2026-09-15).
  **The id-keyed handle read landed 2026-09-10**, closing the actor this
  device has never met. `RoomRosterMemberWire` grew an optional
  `handle`/`domain` pair, **joined nest-side from the serving nest's own
  `users` row** at read time — the `fauna.contacts.list` shape
  (`ContactItem`: `Some` for a local user, `None` for a federated peer). The
  join, not the report, is the source, and that is the security property, not
  an implementation detail: an end-to-end room's floor roster is a
  member-reported mirror (§ The floor roster), so a handle riding the report
  would be attacker-chosen. `fauna.conversations.room.list_roster`'s client
  half (`fauna_client_conversations::ConversationsClient::room_list_roster`)
  reaches it through the `RoomRosterReader` seam — the read twin of
  `RoomRosterReporter`, on the same glue object — and
  `FaunaMlsBackend::resolve_nameless_members` drives it **after the inbound
  poll releases the channel lock**, never inside the walk: it awaits a nest
  round trip, and nothing downstream waits on its answer. The native glue
  installs the seam — linux, tui, and since 2026-09-10 `fauna-ffi` for every
  UniFFI app, whose glue had installed only the reporter, so a macOS or iOS
  witness kept a never-met member elided until the apple leg's journey
  asked it to name one. **Web installs both since 2026-09-11**, closing the
  last app that did not: `fauna-wasm` wires the reader onto the same
  `WsConversationsRpc` it already wires the reporter onto, and its own poll
  loop now runs the post-walk step — `tend_community_room`,
  `backfill_floor_roster`, `resolve_nameless_members`, in `poll_bound`'s own
  order and likewise only after the channel lock is released, so web's drain
  path and native's stay behaviourally identical.
  `ConversationsManager::apply_resolved_handles` then re-points the seated row
  **in place** — same list position, `participant_displays` following — the
  way an identity succession's re-point does, because the index *is* the
  `thread-member-chip[i]` it renders. It only ever adds a name: an answer
  never overwrites a handle this device already held. The read is bounded to
  membership events rather than to the poll tick — the backend remembers, per
  channel, which actors a *successful* read has answered for. A read that
  FAILS retries on the next poll and records nothing. A roster that lists an
  actor without a handle — homed on another nest, its own announce not yet
  landed — or that OMITS the actor entirely — this device routinely folds a
  membership commit before the committing device's own report of it reaches
  the nest, so an omission is not the answer "no handle" — is answered *for
  now*: each keeps its own count of consecutive misses and backs off at the
  same doubling gap, capped at `FaunaMlsBackend::NAMELESS_REASK_CAP` polls
  (below). The one difference: an omission's first miss is a free re-ask —
  the committing device's report is usually seconds away — and only the
  second and later consecutive omissions open the gap; that gap, alone, is
  also cleared outright by the channel's next advanced commit **on a
  governed room** (2026-09-13, `FaunaMlsBackend::clear_omitted_roster_reads`,
  called from `reconcile_roster`) — a fresh commit, whoever authored it, is
  exactly when a stalled omission is due to resolve, and waiting out a gap
  built from omissions the commit has already superseded would only slow the
  read down. A policy-less room's roster is never reconciled from the agreed
  group (above), so its omission gap has no commit-driven clear either — it
  only lifts once the floor itself lists the actor, same as any other
  member. A listed-but-nameless answer's gap is
  untouched by channel commits: it rides the named member's own home nest's
  next announce instead (below). Either way a permanently-elided member
  costs at most one read per cap polls in the steady state, never one per
  poll. **A "named" answer holds only while the member stays
  named (2026-09-10):** a member removed and then re-admitted is re-seated
  handle-less by the add arm, and a cached "named" for them would otherwise
  suppress the read that names them again — so the backend treats that answer as
  stale for any member rendering nameless, and asks afresh.
  **A member homed on ANOTHER nest is named too — by its own home nest's
  announce, never by a lookup (ratified and BUILT 2026-09-10).** The room's
  home cannot join a handle it has no `users` row for, and no nest ever asks
  another what handle an actor wears — the reverse lookup was weighed and
  **refused**, and the reasoning is
  [`../architecture/federation.md`](../architecture/federation.md) § Cross-nest
  shared folders + channel append, the id→handle bullet, which **owns the
  rule**. In one sentence: the member's own home nest, the authority for
  handles at its domain, volunteers `handle@domain` on the `channel.fetch` it
  already relays for that member, and the room's home records it beside the
  member's binding after resolving the domain to that nest's key — so the
  roster read above names a foreign member exactly as it names a local one,
  and the seam, the re-point and all seven apps' render are indifferent to
  where the handle came from. Until a member's home nest has announced
  (its first drain; an older build never does), the member is **not yet**
  named rather than never: it renders as its **elided actor id**, not as a
  blank — [`value-formatting.md`](value-formatting.md) § Account display label
  owns that fallback — and the read asks again at a widening gap of polls
  (`FaunaMlsBackend::NAMELESS_REASK_CAP`), because a listed-but-nameless
  answer is now one the member's own next drain changes.
  **And that member's OWN device reads the roster too — through its own home
  nest, never locally (ratified and BUILT 2026-09-10).** The floor roster
  lives on the room's home nest alone (§ The home nest), so before this a
  foreign-homed member had no roster read at all: its own nest holds no room
  record and refuses the read, so every co-member it had not met stayed
  elided — the announce above naming them for everyone *except* the member it
  was relayed by. The read is now relayed like every other room-plane read
  that member needs, over the distinct kind
  `fauna.conversations.room.list_roster_remote` →
  `fauna.federation.conversation.roster.fetch`, gated on the room's home by
  the same structural membership gate that admits its ciphertext fetch, and
  answered from the **same body** the same-nest door answers from — so a
  foreign member sees exactly the names a home member sees, both joins
  included. [`../architecture/federation.md`](../architecture/federation.md)
  § Federation residue surface, the `conversation.roster.fetch` row, **owns
  the kind, its gate and the disclosure argument** — including why it is not
  the refused reverse lookup, and why the gate is deliberately the channel
  binding rather than the floor mirror.
- **History for joiners is built for the end-to-end class.** Under a governed
  room's `full` policy the inviting device re-seals its
  `history/<channel_hex>` slice to the newcomer as a
  `GroupMetaMessage::HistorySlice` application message in the newcomer's
  first epoch (`ConversationsManager::confirm_add_participant` →
  `RailBackend::deliver_history_slice`), trimmed oldest-first to one channel
  record; the newcomer folds it through the narrow joiner door
  (`ConversationsManager::restore_joiner_history`), which takes the messages
  and the derived state on them and nothing else. **The receiving half is
  built (2026-09-21)**: every
  device judges a member's slice by § History for joiners → *What a device
  accepts* (`accept_joiner_slice` in the MLS rail) and a refused slice folds
  nothing, which is what makes "every other member folds a no-op" true — they
  refuse it. Under `none` nothing is produced, the rail refuses to produce a
  slice the policy does not authorize, and a receiver refuses one that was
  produced anyway. The inbound walk's already-folded skip is thread-local
  (both the MLS and the community arm). One gap, accepted: a device that
  joined a room on a build older than the admission record holds none, so a
  slice that reaches it after the upgrade is refused and its room starts at
  its admission.
- **All 7 apps' renders are complete for the end-to-end class (tui
  2026-09-09, linux 2026-09-09, macos + ios 2026-09-10 — one shared FaunaKit
  render — windows, web and android 2026-09-11), each witnessed by all three
  tier_3 journeys except android's, which waits with every android e2e track
  on the host emulator (web's witness landed 2026-09-12).**
  tui states the class on the header
  (`thread-room-class`, with its `class` attribute), marks owner and admin on
  `thread-member-chip[i]` (text plus a `role` attribute), greys the
  add-participant affordance and the chip's remove off the role-gated
  capabilities, opens the policy editor from `thread-room-settings-button`
  (the `room_settings` sub-page: join rule, history policy, the owner-only
  admin switches, one policy commit per staged change on Save, the page's
  `error-message` for a refusal), and the new-thread picker states the class
  of the room about to be created (`recipient-picker-class`, derived in
  shared Rust by `prospective_room_class` from the committed chips). Element
  ids: [`../ui/conversations.md`](../ui/conversations.md) § Element IDs
  (user-approved 2026-09-09). Witnessed by the tier_3 journeys
  `test_conversation_room_roles.py` (three seats: born governed, the plain
  member's remove greyed and refused, the owner appoints an admin through
  the editor, the admin removes a member, the removed seat hears nothing
  after) and `test_conversation_room_history.py` (a newcomer under `none`,
  then under `full` set through the editor). **linux
  paints the same six surfaces** (`apps/fauna-linux/src/views/conversations/`:
  `thread_header.rs` for the class, the role chips and the two greyings,
  `room_settings_overlay.rs` for the editor, `recipient_picker.rs` for the
  picker's statement) over the SAME shared staged state every app now uses —
  `fauna_conversations::RoomSettingsDraft` plus
  `ConversationsManager::apply_room_settings`, lifted out of tui on
  2026-09-09 so the seed, the at-most-one-staged hand-over rule, the
  eligibility of each row and the commit order are decided once for all
  seven. Its editor is a modal `gtk::Window` rather than the
  `adw::MessageDialog` its rename overlay uses, because a MessageDialog
  closes itself on any response and this editor must stay open until every
  commit has landed. **linux is witnessed at tier_3 since 2026-09-10** — the
  three journeys (the two above and `test_conversation_room_ownership.py`)
  run on it through the same seat shape as tui's, and their first run found
  three defects no unit test had reached, all fixed that day: a plain
  member's chip was demoted to the mail chip's address popover instead of a
  greyed Remove control; the chip's remove gesture sat on the pill while the
  `thread-member-chip` id sat on the label inside it, so the id carried
  nothing to press; and — shared, so every event-driven app had it — a
  commit that moved only the policy (an appointment, a hand-over, a rule
  change) never ticked the snapshot observers, because the poll's fold
  ticked only for a label or roster change while the room projection is
  derived from the group context on every read. The fold now ticks on every
  commit that advances the epoch (`ConversationsManager::room_projection_moved`,
  pinned by `a_policy_commit_that_moves_only_the_roles_ticks_the_observers`
  in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`); tui never
  showed it because it repaints every frame. **macos and ios paint the same
  six surfaces from shared FaunaKit** (`ThreadHeader.swift`,
  `RoomSettingsSheet.swift`, `RecipientPicker.swift`), consuming the same
  staged state through its UniFFI twins and the room's label and token
  helpers through theirs (`room_class_label`, `room_member_chip_text`,
  `room_join_rule_editor_choices`, `room_prospective_class`, … — added
  2026-09-10 so no FFI app retypes a mapping). The editor is a sheet whose
  presentation the thread view owns and clears only on an all-landed
  verdict; a plain member's chip is a greyed Remove control, never the
  address popover. **macos and ios are witnessed at tier_3 since
  2026-09-10** by the same three journeys (ios with each of the three seats
  on a simulator of its own); their first run found two defects, both fixed
  that day: every UniFFI app's glue lacked the floor-roster read (so no FFI
  app could name a member it had never met — the roster bullet above), and
  the apple automation registry read the controls of a closed sheet as
  present, because its dismissed window is ordered out while SwiftUI keeps
  the content attached. **web paints the same six surfaces since 2026-09-11**
  (`apps/fauna-web/src/routes/conversations/+page.svelte`), over the same
  shared staged state through **wasm** twins of the UniFFI ones the native
  apps use — `applyRoomSettings` plus the `roomSettings*` staging calls, the
  draft crossing as a plain JS value the SPA holds opaquely, so the staging
  rules stay in Rust here too. Its editor is a conditional block rather than
  a self-dismissing dialog, for the same reason linux's is a plain window: it
  must outlive its own Save. **Two facts are web's alone.** (1) The room
  policy calls did not exist at the wasm boundary at all before this leg —
  the read half had shipped with `state_json`'s `room`, so the class, the
  roles and the greying were already paintable, but the editor had no door to
  knock on. (2) The cross-app attribute contract collides with real HTML on
  exactly two of these elements: `class` on `thread-room-class` /
  `recipient-picker-class` and `role` on `thread-member-chip[i]` are the CSS
  class list and the chip's ARIA role there, so the tokens ride `data-class`
  / `data-role` and the web bridge now resolves an unprefixed attribute name
  to its `data-` twin FIRST (`tests/e2e-unified/web-bridge/server.py`). That
  had been a fallback, reached only where the bare read came back `None` —
  which a live HTML attribute never is — so the twin was unreachable for
  precisely the two names that needed it. **web is witnessed at tier_3 since
  2026-09-12** by the same three journeys, each seat a browser page on its own
  context. Their first run found two product defects no unit test had
  reached, both fixed that day: a plain member's chip was a bare label, which
  read as enabled and swallowed the click where this doc asks for a greyed
  control (it is now `role="button"` + `aria-disabled`); and — in
  `fauna-wasm`, so every real web session had it —
  `WasmConversationsManager::poll_conversations` panicked on every tick once a
  peer-anchor sweep was configured, a `RefCell` borrow held through its own
  put-back, so from that sweep's landing (2026-09-09) no web device drained a
  Welcome at all. Nothing told those users: a dead receive rail was silent on
  every app until 2026-09-13, when it began surfacing on `error-message`
  ([`../ui/conversations.md`](../ui/conversations.md) § Errors & edge cases).
  **android paints the same six surfaces since 2026-09-11, closing the
  trickle-down** (`ConversationDetailScreen.kt`, `NewThreadComposeScreen.kt`):
  the same shared staged state again, through the same UniFFI twins apple and
  windows paint from. Its editor is an `AlertDialog`, which — unlike linux's
  first attempt — does not dismiss itself on a button press, so the
  "closes only when all landed" contract is satisfied by clearing the draft on
  the verdict alone. Its driver-facing tokens ride Compose's
  `stateDescription`, this app's established carrier for a string state
  attribute, since the android bridge has no `/element/attr` route yet
   — so android is the one app where these attributes are
  painted but not yet readable by a driver.
- **Ownership transfer by the owner's act — BUILT 2026-09-09 for the
  end-to-end class** (§ Roles and authorization → *Ownership transfer*): the
  outgoing owner's device posts the countersigned offer
  (`GroupMetaMessage::OwnershipOffer`, `FaunaMlsBackend::offer_ownership`),
  the incoming owner's device completes it after its next inbound walk
  (`complete_ownership_offer_locked`, parked by the poll rather than
  committed inside it), and every member admits the commit only with both
  signatures (`fauna_mls::room_policy::judge_commit`'s transfer arm, pinned
  on real groups in `libs/fauna-mls/tests/room_policy_commits.rs`). tui
  renders `room-owner-transfer-button[i]` in the editor, as does every other
  app — the six-app trickle-down closed with android on 2026-09-11.
  Witnessed on every one of them but android (blocked on the emulator host
  emulator) by the tier_3 journey `test_conversation_room_ownership.py`. Succession still carries the owner
  role to the successor (§ *Successions*). **An offer the room moved past now
  tells the owner who made it (2026-09-09).** The offer is parked only on the
  *incoming* owner's device, which is where it is dropped — so the seat that
  must act on the refusal ("the owner offers again") was the one seat holding
  no record of it, and learned of the refusal only from the roles never
  changing. The offering device now keeps the version it offered at, and at
  its own commit fold, when the agreed policy has reached or passed that
  version with this identity still the owner, clears the record and paints the
  ordinary page error ([`../ui/conversations.md`](../ui/conversations.md)
  § Errors & edge cases). The transfer landing, or a policy still short of
  that version, clears or keeps the record silently — neither is a refusal.
  **Not built:** the app-side routing of this editor control to the community
  class's `room.transfer_ownership` door (nest-side since 2026-09-09, above —
  there the outgoing owner's single signed act, since the floor is the
  enforcement point), which waits on the community rail no app renders yet. Two review findings on the authorization root were closed the same
  day: a voucher
  now records a succession only for a principal it outranks, and a group
  context that drops the policy's `required_capabilities` is refused, with
  the honest add path refusing an unadvertised leaf by name on its own.
- **Attachment bytes rest on the home nest — BUILT 2026-09-09** (§ The home
  nest → *Attachment bytes*): `fauna.federation.conversation.write_token.mint`
  + its client relay `fauna.conversations.blob.write_token.get`
  (`bins/fauna-nest/src/{federation_handlers,conversations_handlers}.rs`), the
  bulk-token arm on `POST /api/v1/blob` (`blob_routes.rs::upload_blob` takes
  `BulkWriteAuth`), and the home-routed `ConversationsRpc::blob_put` /
  `blob_get` (`libs/fauna-client-conversations`, native + wasm) driven by
  `FaunaMlsBackend::channel_home_url`. Pinned by the two-nest client-stack
  journey
  `cross_nest_attachment_bytes_rest_on_the_home_nest_and_survive_gc_through_client_stack`
  (`bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs`:
  foreign upload → bytes on the home nest only → both members open it →
  zero-grace GC on both nests keeps it → a stranger's mint refused) and the
  seam-routing unit test
  `attachment_blob_put_and_get_route_to_the_channels_home_nest`
  (`libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`). **The
  reader's bounds — BUILT 2026-09-12** (same §, *The reader bounds what it
  fetches*): `fetch_open_cache_attachments`
  (`libs/fauna-conversations/src/backends/fauna_mls.rs`) walks at most
  `MAX_ATTACHMENTS_PER_RECORD` entries and skips a declared size or a fetched
  sealed blob over `INLINE_BLOB_BODY_LIMIT` and an opened length that is not
  the declared size; both `blob_get`s refuse a declared length over the limit,
  the native one stopping a read that runs past it and the wasm one refusing
  an over-long read before the open. The constants live in
  `fauna_core::attachment_limits`, read by the nest's door
  (`blob_routes.rs::BLOB_BODY_LIMIT`) and pin
  (`conversations_handlers.rs::MAX_ATTACHMENT_REFS_PER_RECORD`) and the
  labeler's facet loop. Pinned by the four receive-loop cases beside
  `bob_receives_one_message_carrying` in `fauna_mls_backend_tests.rs` and
  `blob_get_limit_tests` in `libs/fauna-client-conversations/src/lib.rs`
  (real loopback sockets).
- **The community room's attachment key** — moved on 2026-09-10 with the
  rest of the class's build record to [`community-rooms.md`](community-rooms.md)
  § Implementation status today.
- **The invitation's entries** — *An invitation shares its invitee's fate*,
  *An invitation is judged again when it is accepted* and *Pending
  invitations are listed and withdrawable* — moved 2026-09-28 with the
  ruling they record to [`room-invitations.md`](room-invitations.md)
  § Implementation status today.
- **The succession axis is re-ruled for the room plane (2026-09-09), and the
  community class's floor leg is built (2026-09-13).** The
  `Stay` on `group_members` / `groups.owner_id` was *conditional on the plane
  being dark* ([`succession-repoint-axis.md`](succession-repoint-axis.md)
  § The declared re-point axis, the membership/participation cluster), naming
  the plane shipping as its re-check trigger; the floor roster's build was that
  trigger, and the re-check is recorded at both declarations. The ancestors'
  verdicts stood — the room plane landed *additively beside* them and the nine
  group kinds had no shipped caller (the ancestor tables and their declarations
  were then retired with the plane, 2026-09-26, § The group plane's fate) — and the successors take the
  verdicts § The home nest → *The succession axis* rules: `rooms.owner_id`
  **moves** with the owner (it is not dark, unlike its ancestor, which is why
  the two disagree), and a `room_members` row **stays** as history with the
  successor's fresh entry beside it. **Who writes that fresh entry splits on the
  room's provenance, so `room_members.principal_id` is ruled `Partial`.** In an end-to-end room
  it is the add-successor commit's report — the table is a mirror. In a
  ceremony-born room, whose floor is the authority and whose report door
  refuses, it is the succession ceremony itself: in the same transaction that
  moves `rooms.owner_id` it seats the successor with the predecessor's role at
  a fresh roster entry and `Removed`-absorbs the predecessor's row. The
  signed policy still names the
  predecessor; the floor resolves its names through the succession chain
  (§ Roles and authorization → *Community rooms — enforced at the floor*). The
  data-driven sweep over `ACTOR_TABLES` proves the plain move,
  `successions::tests` pins the floor leg on both paths, and a real
  `succession.submit` in `conformance_conversation_rooms.rs` pins the owner's
  and an admin's doors after it, beside the owner's account-deletion rule
  (§ Roles and authorization, *An owner's account deletion*). **One declared
  bound, one retired.** *(1) Seats homed on another nest* are that nest's
  floor to hand over — a succession this nest only learns from a peer seats
  nobody. *(2) The successor's wrap target — retired 2026-09-13:* the ceremony
  still seats it with **no** reception key (the predecessor's belongs to the
  material the ceremony retires), and the seat gains its own through
  `fauna.conversations.room.set_reception_key`, which the successor's app
  calls on its next floor pass — minting itself back in when it is the room's
  only key authority, otherwise covered by an admin's ordinary top-up:
  [`community-rooms.md`](community-rooms.md) § Implementation status today →
  *A seat gains or rotates its wrap target*.
- **The older-app policy-less fallback is gone (2026-09-25, compat-remnant
  sweep program 4 —
  [`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)
  § Dimension 2, the fourth exception).** `FaunaMlsBackend::bootstrap_group`
  no longer asks whether every peer's key package advertises the policy
  extension and mints the group policy-less when one does not: every group it
  forks goes through `MlsEngine::create_group_with_policy`, whose by-name
  refusal of an unadvertised package is the outcome the sender sees (no
  channel bound, no Welcome delivered). The policy-less room itself was
  classed LIVE, not remnant (a 1:1, a folder-share group, a group a peer
  minted without a policy), so the engine's `create_group`, its `None` policy
  read and its refusal to install a policy in place stay, reworded to name
  those shapes. Pinned by
  `fauna_mls_backend_tests.rs::bootstrap_group_refuses_a_peer_whose_key_package_lacks_the_policy_extension`
  and
  `room_policy_commits.rs::an_unadvertised_key_package_is_refused_by_name_and_a_policy_less_group_stays_open`.
- **Bridged rooms** are modeled but render nowhere yet: since 2026-10-02 the
  shared-Rust `BridgedBackend` and the mail rail both answer a transport-only
  room (`room::transport_room` — the people participant-parallel, the bridge
  principal or mail transfer agent seated in the derivation), but no app
  registers the bridged backend until the nest serves the bridged family
  ([`../ui/conversations.md`](../ui/conversations.md) § Implementation status
  today). This doc's one constraint from that chain is in force: the bridged
  family never depends on the group plane in either form (§ Bridged rooms).

Build order — shared Rust first, then nest, then tui, then the six
([`../architecture/testing.md`](../architecture/testing.md) § Default app and
nest mode): the derived class + the room object in `libs/fauna-conversations`,
the floor roster and the community plane on the nest (the reshaped or re-minted
group plane, § The group plane's fate — the one user-gated step), the end-to-end
authorization policy in `libs/fauna-mls`, then the page's render of roles, class
and history policy. The `Bridged` adapter takes its membership shapes from here
and is gated on this ratification; the queue rows carry the slices.

## The room

A **room** is a membership set with a message log. Every conversation on the
page is one, whatever its transport:

| Field | Meaning |
|---|---|
| `room_id` | Content-derived from the room's birth record (the creating principal's key + a salt — the same key↔id commitment every plane uses, [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md) § The recipient-set scheme → *Scope birth*). For an end-to-end room it is the MLS channel id, which is already derived from the group's own state. |
| `class` | **Derived**, never stored as a choice: end-to-end / community / transport-only (§ The three classes). |
| `home` | The room's home nest (§ The home nest). |
| `members` | The roster — one entry per **principal**, each with a kind, a role, a join stamp and the home nest the principal is reached through. |
| `policy` | The owner-signed room policy: the room's name, its join rule, its history policy, its admin set (§ Roles and authorization). |
| `log` | The message log — sealed envelopes in the room's own `conv` scope of the message segment store ([`../architecture/message-segment-store.md`](../architecture/message-segment-store.md)); what key opens them is the class's business. |

**Principals.** A member is a *principal with a key*, of one of these kinds:

- **A user** — the actor and every device in their fleet. A user's membership is
  one roster entry; their devices share it (the fleet is the unit of
  membership, exactly as it is the unit of identity).
- **The room's home nest** — a member only in a community room, holding a
  room-read key so it can serve the room's readable views (§ The three classes
  → *Community*). Never a member of an end-to-end room.
- **A bridge principal or a mail transfer agent** — a third-party or
  first-party bridge ([`../architecture/third-party.md`](../architecture/third-party.md)
  § The principal model), or the mail perimeter. Its membership is what makes
  a room bridged, and what makes it transport-only (§ Bridged rooms).

Two kinds of member that are **not** principals: a *reply recipient* on a
subject-keyed mail thread (a per-message To/Cc, not a roster entry —
[`../ui/conversations.md`](../ui/conversations.md) § Participants vs reply
recipients), and a *reader of a public room* — there is no public class;
a room with no confidentiality is a feed, not a room.

**Operations.** The semantic operations are the same for every class; the
adapter's capability vector says which ones reach the wire on a given transport
([`../ui/conversations.md`](../ui/conversations.md) § Capability gating): send,
react, delete (own; any, for owner/admin), invite, remove (a member may remove
themself — leave), rename, set policy (owner), transfer ownership (owner).

## The three classes

**The rule (TP8): a room's confidentiality class is a pure function of its
member set, computed in shared Rust, never a per-rail constant and never a
stored choice.**

**And the member set is the floor's, which in the community class the home
nest serves — so the label carries the revoke's honest bound, one level up
(2026-09-13).** `derive_room_class` reads the principal kinds of the floor
roster this device has read, and in that class the roster is the nest's to
write (§ The floor roster). A home nest that omits its own row from the floor
it serves therefore paints its room **end-to-end** on every seat, while the
wrap the members minted still opens the log — an omitted answer keeps the
wrap, by the same rule that makes a `null` neither a revoke nor a re-grant
([`community-rooms.md`](community-rooms.md) § Implementation status today →
*How far the revoke reaches*, which owns how far a withdrawal binds). The
label is an honest render of the floor, never a proof that the nest cannot
read; a member who needs that asks for the end-to-end class. A device that
has not read the floor falls back to the participants and can then answer
only end-to-end or transport-only, which is honest for a room it knows
nothing else about.

| Class | Member set | Who can read the log | Key model | Size |
|---|---|---|---|---|
| **End-to-end** | User principals only — every member is a user's device fleet. | The members, on their own devices. Nests relay opaque bytes. | MLS — the `(FaunaMls, MlsGroup)` channel as built; owner [`direct-messages.md`](direct-messages.md). | Bounded — MLS's own operational limits; the page shows the number. |
| **Community** | User principals **plus the room's home nest**. | The members, and the home nest — which searches and labels it, serving what it derives as metadata only (*What the home nest does with its read*, below). | The recipient-set scheme (R15), with the home nest as one wrap recipient under its room-read keypair. | Unbounded. |
| **Transport-only** | Any member set containing a bridge principal or a mail transfer agent. | The far network and its bridge read it by construction; Fauna seals the legs it controls and labels the room honestly. | Per adapter: the mail perimeter's seal ([`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) `Mail body` row); the bridged family's HPKE seal to the recipient key inbound and to the bridge key outbound (TP10, [`../ui/conversations.md`](../ui/conversations.md) § Where logic lives → *The `Bridged` adapter*). | Per adapter. |

The derivation is total and ordered: any bridge/MTA member ⇒ transport-only;
else the home nest a member ⇒ community; else end-to-end. `ThreadEncryption`
becomes the render of this class — end-to-end ⇒ `E2E`, community ⇒ a
nest-readable arm, transport-only ⇒ `TransportOnly`; the build names the arm and
retires whatever variant no roster can produce. Two consequences the chain
already relies on: "never presume a bridge or plugin is an MLS group member"
(TP12) is a theorem — a bridge can only be a member of a room whose class
already says the bridge reads it; and a room cannot be *promoted* into a
stronger class by editing a flag, only by changing its members — adding the
home nest to an end-to-end room is a new room, never an in-place downgrade of
the transcript members already sealed to each other (§ Don't do these).

**The community class itself — why it exists (decision 1), its key model,
who wrote a message, its attachment kind, and what the home nest does with
its read — is [`community-rooms.md`](community-rooms.md) § The three classes,
split out on 2026-09-10**; the table and the derivation above stay here, and
that doc holds the Community row's own detail. Moved there, by lead-in — a
text search for any of them lands here: *Why a community class exists*;
*Community — the key model* (the four reasons); *Who wrote it*; *A signature
is not membership*; *Attachments — the second content kind*; *What the home
nest does with its read* (the purposes: search, labels, room-restricted
posts); *Struck from the list*; *What the read covers*; *Forbidden, stated
once*; *A derived view is only a purpose served once a member can reach it*;
and *The door answers where, never what*. A citation of this section naming
*Community*, or any of these, resolves there.

## The floor roster

**Every room has a nest-side roster on its home nest** — the *floor roster*:
one row per principal with its kind, role, inviter, home nest and join stamp.
Its authority differs by class, and the difference is the whole of "roles
enforced cryptographically in end-to-end rooms and at the floor in community
rooms" (decision 4):

- **Community rooms — the floor roster is authoritative.** The home nest
  refuses a send, invite, remove, delete or policy change whose signer's role
  does not permit it, before storing anything. Membership *is* the roster row.
- **End-to-end rooms — the floor roster is a member-reported mirror.** The MLS
  group is the membership authority; the nest cannot see inside it. After every
  membership commit — and every policy commit, since the roles it rewrites are
  part of the roster — the committing device reports the resulting roster to
  the home nest, **the room's birth included**: the creating device is the
  committing device of the group's creation, and its report is the one a 1:1
  ever makes (a 1:1's membership is fixed for life — its add-participant forks
  a new group) — the same *report, never guess* rule the succession sweep already
  follows for the roster it reports
  ([`succession-propagation.md`](succession-propagation.md) § Propagation, *MLS
  groups*). The nest stores the report as the room's floor roster and uses it
  for everything a nest legitimately decides about membership: routing
  fan-out, the custody serve door, the cross-nest relay gate, succession
  targets. It decides **nothing about confidentiality** — a wrong report
  cannot make a non-member read a message, because reading is MLS's.

**A mirror's reports are ordered by the commit each one follows.** Every
membership and policy commit owes a report, each built by whichever device
committed, and nothing sequences their delivery — so a report can reach the
home nest after the report of a *later* commit, and applied as it arrived it
would put back a member that later commit removed and the roles it rewrote.
So a report names the room log's position of the commit it follows (what that
commit's own send answered), and the home nest holds that claim to what it
observes itself — the newest commit the room's log has carried, the same
high-water mark the device-owned-epoch commit gate reads
([`devices.md`](devices.md) § Cross-device MLS group-state sync):

- a position **above** that mark is refused — the log carried no such commit;
- a position **at** the mark — the newest commit — is refused from anyone but
  the actor the nest observed *sending* that commit;
- a position **at or below** the one the floor already holds is not applied,
  and the ack names the later commit that superseded it — not a failure: the
  floor is at least as new as the report;
- otherwise the report replaces the roster and the floor's position advances,
  in the same transaction as the replace.

The bound is what keeps ordering from becoming a freeze: a member claiming a
position far ahead would otherwise lock out every honest report after it,
while the highest position anyone can claim here is the newest real commit,
and the next honest commit lands above it.

**A position is a claim about someone else's commit, so it is bound to that
commit's sender.** Ordering by position alone leaves the *first* report at a
position winning it, and the report that loses is silently written nowhere — so
the one report the rule exists to protect, the committing device's own, had no
standing over a bystander's. The case is the remover's: until an admin's report
of its own Remove lands, the member that Remove evicted is still a live member
of the stored floor and still on the routing roster (the routing row
deliberately survives a departure, § What the floor roster is not), so it could
name the Remove's *own* position, seat itself, and leave the admin's honest
report at that position superseded — durably, since the floor then moves only
on the next membership or policy commit. The nest observes the commit's sender
at the same append it observes the position, so it holds the claim to that too:
at the newest position, only that commit's sender may report. Two cases stay
admitted, and neither is wider than the trust a member-reported mirror already
carries: a position *below* the newest commit, whose sender the nest no longer
keeps — such a report is anchored below the honest one that follows it, so it
is replaced sooner rather than later — and a room whose stored mark predates
the sender being recorded, until its next commit rewrites the pair. What a live
member can still do, unchanged, is report a false roster at a position it
genuinely owns: it can always commit, and the commit it just made is a position
it is the sender of. The binding bounds a false report's **durability**, never a
member's ability to make one.

The policy version cannot order reports: an ordinary add or remove does not
change it. A report naming **no** position — the birth report's, and the floor
backfill's — applies unordered, as reports always did, from any live member, and leaves the floor's
position where it was: the binding orders and authenticates *positioned*
reports only.

**The custody serve door reads the floor roster** — for both classes. Until
2026-09-09 it read `group_members` and failed closed for every live room; the
floor roster is the non-self-assertable membership record that door was
declared to wait for
([`../architecture/message-segment-store.md`](../architecture/message-segment-store.md)
§ Which kinds the two planes serve;
[`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md)
§ Shared-audience carve-out owns the door's rule and adopts the roster by
reference). Re-homing that consumer onto the floor roster was the **first**
code step of § The group plane's fate, before any kind or table was retired.

**What the floor roster is not.** It is not `actor_channels` — that is the
channel's *routing* roster, self-registered by any local actor's first
`channel.send`, so a row there proves knowledge of a channel id, not membership
([`direct-messages.md`](direct-messages.md) § Security Properties). The routing
roster stays as the fan-out mechanism the nest can mutate without any key; the
floor roster is what says who the room *is*.

## Roles and authorization

Three roles, the same vocabulary in every class — **owner**, **admin**,
**member** (Mechanism B spelled the third `user`; the storage spelling follows
the reshape, § The group plane's fate):

| Operation | owner | admin | member |
|---|---|---|---|
| send, react, delete own message | ✓ | ✓ | ✓ |
| leave (remove self) | — (transfer first) | ✓ | ✓ |
| invite | ✓ | ✓ | per join rule (§ Join rules) |
| remove a member | ✓ | ✓ | — |
| delete any message | ✓ | ✓ | — |
| rename; set history policy; set join rule | ✓ | ✓ | — |
| appoint / demote admins | ✓ | — | — |
| transfer ownership; re-home (§ The home nest) | ✓ | — | — |

Exactly one owner. The owner's membership is not removable — by anyone, the
owner included — until ownership is transferred: an owner-less roster would
strand invite and remove forever (Mechanism B's rule, kept). A user succession
of the owner transfers ownership to the successor as part of the ceremony
(§ The home nest → *Transfer by succession*).

**An owner's account deletion.** A user's own deletion of their account is
**refused** while they hold a community room's owner seat and another live
user member homed on the room's home nest could take it over — at the
scheduling door, and again when the delayed deletion executes — because a
deleted owner leaves the room with no owner seat, so appoint, demote and
transfer would be gone for the life of the room while the one act that avoids
it was available. With nobody who could take over (the owner beside only the
home nest, or beside members homed on other nests, whom no transfer reaches
until re-homing exists) the deletion proceeds, and the room keeps its members
and everything they hold without an owner seat. So does an **admin's** deletion
of an owner, which is never refused for the room: the admin holds no door that
could transfer it, and a refusal would let any user make themselves undeletable
by seating one other account. A refusal nothing can lift is exactly the state
[`../architecture/nest/common.md`](../architecture/nest/common.md)
§ Client-state recoverability forbids, and the owner-less room meets that
section's per-object conditions — contained to the one room, replaceable by
founding another, nothing a member holds lost, inert. A deleted **admin's** name
stays in the signed policy until the owner re-signs without it; until then
every other admin's policy edit is refused, and the refusal names that fix.

**End-to-end rooms — enforced cryptographically.** The room policy — the admin
set, the join rule, the history policy, the name — is a **signed room
authorization policy carried in the MLS group context**, signed by the
principal that authored the version and re-signed on every policy change:
**the owner's credential for a change to the owner or the admin set, an
owner's or an admin's for the fields the table above lets admins set** (the
signer's role in the *previous* version decides which fields it may have
changed, and the signature only proves the bytes are that principal's).
Every member verifies it, and **a member refuses to apply a commit its
author's role does not permit** — a Remove proposed by a plain member, an
Add under an invite-only rule from a non-admin, a policy replacement its
signer's role does not cover — before the commit touches the member's epoch,
the same refusal shape members will apply to a commit not attested by the
room's sequencing seat ([`p2p.md`](p2p.md) § Offline share initiation,
obligation 1, still design there). The policy rides the group context, and
the group's `required_capabilities` names its extension type, for three
reasons the build relies on: every member at an epoch agrees on it (the
confirmation tag), a joiner receives it in the Welcome before any commit,
and a leaf that does not advertise the type cannot be seated at all — so an
older app is excluded from a governed room *loudly* rather than seated as a
member that merges what every newer member refuses, which would fork the
group between honest members (`libs/fauna-mls/src/room_policy.rs`, module
doc). That requirement is agreed state like the policy itself: a
group-context change that drops it is refused by every member, and the
honest add path refuses an unadvertised leaf by name on its own rather than
delegating the check to the requirement. The nest sees none of this and
enforces none of it; its floor roster is the mirror the members report. This
is why Mechanism B could never serve the end-to-end case: a role the nest
enforces is a role the nest can misapply, and inside an end-to-end room the
nest is not a member.

**Ownership transfer — the owner's act, two keys' consent.** A voluntary
hand-over is a policy change carrying **two** signatures: the outgoing
owner's, because only the owner may transfer, and the incoming owner's,
because an owner can neither leave nor be removed and nobody is made one
unasked. The ceremony is one round trip. The outgoing owner's device builds
the policy naming its successor (the next version; the successor leaves the
admin set), countersigns it **bound to the room** — a countersignature for
one room must never complete a hand-over of another room the same owner
holds at the same policy bytes — and posts it on the channel as an *offer*
every member decrypts. The named member's device signs the same policy as
itself and commits it, carrying the countersignature; every member admits
the commit only when the signer is the new owner, the countersigner is the
owner the succession chain resolves to, and both signatures verify. Until
that commit is folded nothing has changed hands, and the roles every seat
paints afterwards come off the agreed group context. An offer the room has
moved past (its version no longer the next one) is refused by every member
and dropped by the device holding it; the owner offers again. The old owner
is a plain member from the transfer commit on — removable like any member,
which is the point of transferring rather than leaving.

**Successions inside a governed room.** A user's identity succession
([`succession-propagation.md`](succession-propagation.md) § Propagation →
*MLS groups*) is add-successor by the **old** leaf and remove-old by the
**new** leaf — both, on their face, the shapes a plain member's role forbids.
They are admitted through one door: a succession record the old leaf itself
appends to the policy extension (a group-context change naming the
successor; a principal that **strictly outranks** the succeeded one may vouch
for it — the owner for anyone, an admin for a plain member — the member-side
re-add remedy, narrowed so that nobody but the owner's own leaf can ever
record the owner's succession), recorded **first** so it is agreed state —
and in every later Welcome — before any member judges the add. Every role resolves through the
chain: the owner's successor owns the room, an admin's successor is an
admin, and no policy rewrite is needed for the inheritance. What the door
deliberately admits, stated rather than glossed: whoever holds the old key
may replace itself with a fresh identity it controls — no new power, since
that key already read the room — and the planted-successor residual it can
leave after a *real* succession is exactly the unattested-member review the
sweep already raises. Whether a succession is legitimate is a nest-anchored,
time-based fact no in-group verdict can decide offline; the policy decides
only authorization, deterministically.

**Community rooms — enforced at the floor.** The same policy is stored on the
home nest beside the roster, owner-signed, and the nest applies the table
above on every write before it stores or fans out. Members verify the
owner's signature on the policy they render (the nest cannot forge a policy
change, only refuse to store one), and the class label on the header says
that the floor — not the members — is the enforcement point. **The policy's
names resolve through the succession chain on the floor**, as they do in the
end-to-end class's extension: the nest cannot rewrite a signed policy, while
an identity succession hands the predecessor's floor seat to the successor
(§ The home nest → *Transfer by succession*). So wherever the floor matches a
policy name against a seat — the owner, the admin set, an admin invitation —
the name stands for the newest live seat along its succession line, and a
policy signed before the ceremony keeps working unchanged until an owner or
admin next re-signs it.

**Delete any message — the mechanism (ratified 2026-09-19).** The table says
who; this says how, for both classes. Three rules are the classes' alike:

- **A second admission beside the sender match, never a loosening of it.** A
  delete whose authenticated author is the target's own sender is honoured
  everywhere, as it always was
  ([`../ui/conversations.md`](../ui/conversations.md) § Reactions & message
  delete owns that floor and the sender authentication under it). A
  *cross-sender* delete is honoured only in a governed room, only when its
  authenticated author — never a self-asserted field — holds owner or admin,
  through the succession chain. A policy-less room, a 1:1 and every other rail stay
  sender-only.
- **A delete is judged by the policy it was made under — never by the policy a
  member holds "now".** Each class pins that policy to something the delete
  itself authenticates (the epoch it was sealed in; the policy version its
  record names — below), so it is a fact every member agrees on and a replay
  reproduces: an admin's delete stays honoured after that admin is demoted, and
  a delete posted after the demotion is dropped, on every seat, whenever each
  seat folds it. So the verdict is **recorded with the claim when the delete is
  folded** — the author, the delete's own log position, and the role the author
  held there — and the projection reads the recorded verdict; it never re-asks
  the current policy. A message may carry several claims (a forged one must not
  displace an honoured one, nor the reverse), and it is tombstoned when any one
  of them is admitted.
- **The tombstone is cooperative in both classes.** The target envelope and its
  attachment blobs stay stored and served; no floor act withholds them. (A
  legal takedown is the one act that does, and it is
  [`moderation.md`](moderation.md)'s, not a role's.)

*End-to-end rooms.* The delete is the ordinary sealed `Delete` application
message — no new wire shape, and the nest sees nothing. The engine that
authenticates the sending leaf also resolves that leaf's role, and "the
delete's own position" is here **the MLS epoch the delete was sealed in** —
which the framing authenticates, so every member that can open the delete
agrees on it and on that epoch's group context. (The log position alone would
not do: an application send is a blind append, so a delete sealed in one epoch
can land in the log after the commit that ended it.) The role is read from the
group context of that epoch; a member that opens a delete from an epoch whose
group context it no longer holds records no role for it — the cross-sender
claim fails closed, the message stays, and the owner or admin deletes again.
A member whose device restores from history rather than re-folding
receives the folded tombstone with it — one rule for every delete, the sender's
own included (§ Implementation status today declares where that stands).

*Community rooms.* Two different acts, because the home nest can judge one of
them without reading and cannot judge the other at all:

- **Deleting one's own message** is a `Delete` body on the room's sealed path
  (the `RoomSealed` envelope every community message rides), its sender the
  signed author; members apply the sender match. The floor's send gate stays
  what it is — membership — and the nest neither reads nor judges it: a
  forged own-delete is dropped by every member, exactly as in the end-to-end
  class. Reactions ride the same path.
- **An owner's or admin's delete of another member's message is a floor act:
  a signed, floor-visible delete record appended to the room's log** — the
  room id, the target's log position, the author, and the **policy version**
  the author acts under, signed by the author under a domain tag of its own and
  carried *unsealed* in an envelope variant of its own on the ordinary send
  path (so the cross-nest send and relay legs carry it unchanged). The home
  nest judges it before storing anything, from the record alone: the signature
  verifies, the author is the authenticated caller, the named version is the
  policy version the floor holds, and the author's floor seat — through the
  succession chain — is owner or admin. Anything else is refused, and so is
  any such record addressed to a room that is not floor-authoritative. **This
  needs no read and makes none**: [`community-rooms.md`](community-rooms.md)
  § What the home nest does with its read closes the read's purposes and
  forbids its use as an authority, so "unseal the send to see whether it is a
  delete" was never available — the floor act is a ciphertext act, like remove
  and demote. What the unsealed record reveals to the floor is that a named
  owner or admin tombstoned position *N* — the same class of fact a remove
  already reveals.
- **Members verify what they paint.** The floor is the enforcement point, not
  the only witness: a member honours a floor delete record only when its
  signature verifies and its author is owner or admin — directly, or through a
  succession the member verifies as it does everywhere — in the owner-signed
  policy **of the version the record names**. So the home nest **retains every
  superseded signed policy version** and serves one by number; a member that
  holds another version fetches the named one, and until it verifies paints no
  tombstone (it fails closed: the message stays). The nest cannot forge a
  tombstone alone, and a plain member cannot mint one at all. Declared
  residual: a home nest colluding with a *demoted* admin could admit a record
  naming the older version — it gains a cooperative tombstone over a message
  the nest could already have withheld outright.
- **A retained version outlives the account it names — and is never rewritten
  (ratified 2026-09-21).** The retained versions are *verification history*, so
  the question "what happens to them when a named account goes" is answered by
  what a member needs, not by what the account holds. **Account deletion:** a
  retained version naming a since-deleted owner, admin or signer **stays**,
  verbatim. Every floor delete already made under that version was judged
  against it, and a member that re-walks the room re-judges from scratch — so
  dropping the version would un-tombstone those deletes on the next fresh
  session, silently restoring messages an owner or admin removed. The bytes are
  a signed public act, not a secret: every member of the room received and
  verified them, so retaining one hands the nest nothing the room did not
  already publish. **Succession:** nothing burns a retained version either; the
  floor resolves a name through the succession chain at judgment time
  (`floor_designee`), so a version naming the predecessor still verifies through
  the successor without being rewritten — which is the point, since rewriting it
  would break the author's signature over it. **Rewriting is refused outright:**
  a differing blob at a version already retained is a refusal, not an upsert
  ([`community-rooms.md`](community-rooms.md) § Implementation status today, residual *(e)*) — a history that can be
  rewritten answers a different question from the one every member asks of it.
  ⚠ This **supersedes** the answer recorded earlier for the same bytes — *"the owner re-signs"* — which is now **insufficient** rather than
  merely incomplete: before the versions were retained, an owner's re-sign genuinely
  left no copy of a deleted admin's id on the nest, because each re-sign
  overwrote the blob in place. Since schema 68 the re-sign *mints a new version*
  and the old one — still naming that id — is kept for the life of the room. The
  mechanism that used to erase the id no longer does, so "the owner re-signs" is
  no longer an answer to the deletion question at all; the ruling above is.
  These ids are also invisible to the actor-table census, which walks by column
  name and cannot see inside a blob — declared as a blob-held actor bearer, with
  the walk that reds on the next such column, in
  [`opaque-carrier-walks.md`](opaque-carrier-walks.md) § The declared
  re-point axis.
- **A room whose chain this device cannot prove says so (ratified
  2026-09-20).** Failing closed paints no tombstone, but silence is a claim
  too — the target reads as if nobody had acted on it. So a room with a record
  parked on a name the peer-anchor harvest has **settled** for the session
  without anchoring (a retired owner or admin homed on another nest — the
  harvest's reach is the declined read,
  [`identity-succession.md`](identity-succession.md) § The succession
  statement → *a community policy's names join the harvest's walk*, bound 3)
  reports on the thread header that moderation in it could not be verified on
  this device: **one room-level statement, never a mark on the record's
  target**, which would paint the record's claim before it is verified. Not
  before the harvest has spoken — a record parked for want of a version, or on
  a name the harvest has not reached, is *not yet* judged and the next pass may
  paint it — and gone the moment the record paints or the session ends (the
  harvest's settled set is session memory; the parked record is not). The
  name is one **the record itself** still waits on and one **the chain this
  device proved** carries: a name only a served, unanchored version names is
  the home nest's say-so, never the room's, and a name the harvest *seeded*
  was anchored, not settled without it. The statement says nothing else and offers
  nothing to do: the record is neither honoured nor refused, and the home nest
  can make it neither. Shared Rust derives it once
  (`RoomSnapshot::moderation_unverified`); the element and its copy are
  [`../ui/conversations.md`](../ui/conversations.md) § Element IDs'.
- **A fetched version is anchored, never believed.** A community policy's
  signature proves only that *somebody* signed it, the policy names no room,
  and the home nest chooses which bytes it serves. So a member ranks a record's
  author only under a version at the end of a **chain it has proven itself,
  for this room**: version 1 is the founder's — signed by the owner it names,
  naming no admins, and with the room's **birth salt** deriving exactly this
  room's id, which is what binds version 1 to the room (the home nest serves
  the salt beside a version it is asked for; a member already names the room,
  and the salt only ever kept the id unguessable to those who did not) — and
  each later version is **this room's by its room signature** and signed by a
  principal whose rank **in the version before it** allows the change: the
  owner anything, handing the room on included (in this class a transfer is
  the outgoing owner's own signed version); an admin everything but the owner
  and the admin set; a member nothing. The birth salt binds version 1 only:
  one owner's rooms can have byte-identical version-1 records that differ only
  in their salts, so without a binding of its own, a version the owner signs
  in one room would be a valid next version of every other room they founded.
  That is why the **room signature** exists. It is the signer's second
  signature on every community version, over the room id and the same policy
  bytes, the shape the ownership-transfer countersignature and the floor
  delete record already bind their room with. A version whose room signature
  is another room's never extends this room's chain. That step rule is **one
  judge, run on both sides** — the home nest before it stores a change, the
  member before a fetched version grants anyone a rank — so the floor never
  admits a version its members would refuse to follow. The two sides differ in
  one input only: whom a policy's *name* designates once successions are
  resolved — the nest reads its own floor, the member the succession
  statements it verifies as it does everywhere (the next rule). A nest can
  withhold a link, and the member then fails closed; it cannot mint one, nor
  lift one from another room. A member keeps the chain it proved first: a
  different served blob for a version already anchored is ignored, so the
  anchor does not rest on the nest's history being immutable.
- **Every room requires the room signature, and a nest cannot strip it
  (ratified 2026-09-22; universal since 2026-09-30).** A version keeps its
  ordinary signature beside the room signature, so a room has to *require*
  the room signature on every version above 1, or its home nest would strip
  it and serve what remains. Every community room does: a version above 1
  without a room signature for this room is refused by the floor and by every
  member alike. A room is founded with a **binding birth salt** — the salt
  opens with a fixed mark and the rest stays random — and because the id
  commits to the salt, the mark keeps every room id a commitment to the rule.
  A birth salt without the mark founds no room: the founder's own birth check
  and the nest's `room.create` refuse it, so no founder can mint a room whose
  id reads as predating the binding and so invite a splice of another room's
  versions. The version skews the room signature brings are recorded in
  [`community-rooms.md`](community-rooms.md) § Implementation status today,
  residual *(f)*.
- **A name designates its verified line (ratified 2026-09-20).** The floor
  judges at the time of the act and answers with **one seat**: the newest live
  seat along the name's succession line. A member re-proves the chain *later*,
  and cannot recover which seat that was — successions run forward only and
  carry no ordering against policy versions. So member-side a policy's name
  designates **itself and every successor after it on a line the member has
  verified** — never an identity *before* it, and never anyone on a line the
  member has not verified itself
  ([`identity-succession.md`](identity-succession.md) § The succession
  statement: the anchored walk to the identity's own home nest, guarded by any
  chain head already held; never a nest's say-so).
  - *Why the line and not its newest holder.* Terminal-only strands honest
    rooms twice over: a version signed by an intermediate successor, since
    succeeded again, would end the chain for good; and the floor itself keeps
    designating the **predecessor** of a member homed on another nest (nothing
    there hands the seat over), so members would refuse steps the floor
    admitted. The floor's one seat is always on the line, so the one-judge
    promise holds in the direction that matters: **whatever the floor admits,
    its members follow**. The line is also replay-stable — learning a further
    succession only ever admits more, so a chain proved once stays proved.
  - *Why not "monotone along the line"* (a signer may not precede an earlier
    signer for the same seat). It would need per-seat ordering state in the
    chain and in the shared judge, and it constrains only an adversary that
    does not need constraining: the one party that can show a member a retired
    key's signature at all is a colluding home nest, which also chooses where
    the chain it serves a fresh session forks — before the successor's first
    signature. Against an honest floor the newest-live-seat rule is already
    the gate.
  - *What retires a key.* A version that names the successor itself: a name
    never designates an identity before it, so from that version on the
    retired key is a stranger to every member. A successor that governs a room
    re-signs under its own name; until it does, the retired key stays on the
    line member-side. **Declared residual:** a *retired* owner or admin key
    colluding with the home nest can extend a chain the policy still names it
    in — the demoted-admin residual's class, and bounded the same way: what
    the chain grants a member-side rank for is a cooperative tombstone over a
    message that nest could already have withheld.
  - *Where a line comes from, and where it must not.* The member's session
    witness resolves a bare name by the succession statement's own rule and
    **only from what this device independently knows of that identity** — the
    owner's own anchor-grade handle for it, else the home domain harvested
    from its own signed profile. Never from the room: the room's home nest is
    the party the chain distrusts, and a dial target it supplied would let it
    choose the nest that "verifies" a line forged with a stolen seed. A name
    this device holds no anchor for, or whose anchor is unreachable,
    establishes nothing **yet**: the version does not extend, the record
    parks, and both are judged again on a later pass (a harvest may seed the
    anchor meanwhile — and since 2026-09-20 the room offers the harvest the
    anchored policy names it found no anchor for, so a retired name the
    member never shared a thread with is reached too; which names may be
    offered, and why the room's home nest still chooses nothing by it, is
    the succession statement's rule: [`identity-succession.md`](identity-succession.md)
    § The succession statement → *a community policy's names join the
    harvest's walk*). Nothing is asked until a rank refusal a succession
    could lift, and only a *policy's* names are ever asked about — a record's
    author never chooses whom a member dials. With no witness registered a
    name designates only itself.
  - *A verified line holds only so far.* An identity may succeed after a
    member verified its line — a line verified empty, and just as much a
    positive one, whose newest holder succeeds in turn (A1 → A2 verified,
    then A2 → A3: A3 is off the line the member holds for A1). So no verified
    line is ever the last word: every policy name a rank refusal asked about
    is asked again on a hard-coded, bounded cadence, the same for an empty
    line and a positive one, and a held line only ever **grows** — a re-ask
    that answers shorter, or forks off the held line, changes nobody's seat.
    And since every line may yet grow, while a witness is registered a rank
    refusal **parks** rather than being judged a plain member's claim — a
    claim is a verdict no later pass revisits, so settling one would lose, for
    good on that device, every delete a later successor makes; with no witness
    a name stands for itself and the refusal is final. The price is declared:
    a record that really is a plain member's (a thief's, a retired key's)
    parks too — bounded by the room's park cap, and it never paints.

*The affordance.* Apps never branch on role: each message snapshot says
whether *this viewer* may delete it (own, or the viewer governs the room), and
the same delete button and confirm appear on exactly those bubbles.

*Version skew, declared.* A seat running a build older than this rule drops an
owner's or admin's cross-sender delete as forged (end-to-end), or skips the
floor record it does not know (community), and keeps painting the message
while newer seats show the tombstone. Nothing breaks and nothing is lost —
additive under
[`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)
— but it is a visible divergence inside a major version, healed by the older
seat's update and by nothing else.

**Leaving — the mechanism (ratified 2026-09-20; amended 2026-09-23).** The
table says who; this says how. Both classes leave by **one door**,
`fauna.conversations.room.leave` (and its relayed twin, § The home nest), and
it is **self-scoped**: it stamps the caller's own floor row departed and moves
no one else's. What differs by class is only what the door severs beside the
seat — complementary, never cumulative, exactly as the classes' removals are
(§ Implementation status today → *A removal severs on the end-to-end class
too*).

- **A community room's departure purges the binding too.** The door unseats
  the departing member on the floor and purges the foreign-member binding that
  admitted it ([`community-rooms.md`](community-rooms.md) § Implementation
  status today → *A removal severs*). The floor **is** membership on this
  class, so the departure is complete when the door answers.
- **An end-to-end room's departure moves the seat alone.** The floor is a
  mirror here, and a member's own absence is the one thing about that mirror
  a member may assert without anyone else's word — so the door admits this
  class for the leave, and for nothing else it does (the invite, accept,
  remove and invitation doors still refuse it by provenance). It purges no
  binding: this class severs relayed reach by *reading* the floor, never by
  purging (§ Implementation status today → *A removal severs on the end-to-end
  class too* owns that choice), so the stamped row is what closes the
  floor-gated doors, and a later honest report that names the member again
  gives it back its reach with its seat.
- **The ordering rule for a departure: it names no roster, so it has nothing
  to order.** A departure is not a commit and holds no log position, and the
  floor's position does not move when one lands. It used to be a final roster
  report naming the roster minus the leaver, built from the leaver's own group
  view at no position — and an unpositioned report replaces the floor
  wholesale (§ The floor roster), so a leaver that had not yet folded the
  newest membership commit rolled the floor back to its own stale view: a
  member that commit removed was re-seated, and one it added was dropped,
  until the next commit's report. A report cannot be positioned to fix that —
  the newest position is bound to its commit's sender, and the leaver is not
  it — while a self-scoped door has no one else's row to get wrong, whatever
  the leaver's view. The door replaced that report as the departure outright;
  the compat-remnant sweep's fourth ratified exception
  ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)
  § Dimension 2) removed the report's residual fallback for a home nest older
  than the door, since none remains.
- **The owner leaves by transferring first, and no door needs the client's
  word for it.** The leave door refuses the owner by the stored floor's rank,
  on both classes. The report door refuses an owner-less roster against a
  **stored floor that names an owner** — judged against the stored room, not
  the report's shape, since a governed room's report from a device that could
  not read the policy arrives role-less exactly like a policy-less room's and would
  otherwise un-govern the floor (a role-less report against a floor that names
  no owner — a policy-less room, a 1:1, a birth report — is admitted as before). A
  room is never owner-less (Mechanism B's rule, kept), so an owner who wants
  out transfers ownership and then leaves — the same sentence the roles table
  above puts in the owner's row.
- **A retry is answered as the departure it repeats.** The door answers a
  caller the floor has seated and since stamped departed with the live count,
  as it answers a fresh departure — its own earlier leave whose reply was
  lost, or a removal that got there first; the postcondition, off this floor,
  holds either way, and a refusal would tell a user who has left that they
  could not, on every retry. A caller the floor has never seated is still
  refused. The relayed twin answers the same way
  ([`../architecture/federation.md`](../architecture/federation.md)
  § Federation residue surface, the *room leave* row).
- **The leaver's MLS leaf stays until a remaining member commits, and that is
  forced rather than deferred.** MLS never lets a commit remove its own
  committer, so no device can unseat its own leaf: a self-leave is a floor act
  and the leaf goes at the next membership commit any remaining owner or admin
  makes. This is the community class's ratified *leaving does not rotate*
  reached by the protocol instead of by authority
  ([`community-rooms.md`](community-rooms.md) § Implementation status today,
  the leave paragraph) — a leaver holds no key authority, and a room that
  wants a departed member sealed out of *new* traffic re-keys from a remaining
  owner or admin.
- **The leaver keeps its own copy.** The device retains the generations and
  bubbles it already holds — deleting them would destroy the user's own copy
  of a conversation they were legitimately part of, and would not un-read a
  byte. What the departure closes on this device is the room's own verbs and
  its composer: a leaver stops talking because it left, not because something
  stopped it.
- **Declared residue — a departure is not a mute.** The floor severs what the
  floor gates, at the very next request: the custody serve door, the three
  binding-only relayed doors and the room-post verdict read all refuse a
  principal the floor positively says was removed. None of them is
  `channel.send`, and the routing row deliberately survives a departure — so
  until a remaining owner or admin commits the Remove, a leaver's device is
  cryptographically still able to encrypt to the group — and to keep reading
  it, which `test_conversation_room_leave.py` asserts POSITIVELY so the
  journey is this paragraph's alarm rather than its blind spot. The client
  closing its own composer is a courtesy, never the boundary; the boundary is
  the re-key, and this paragraph is the honest statement of the window. It is the sending
  mirror of the ciphertext residue the removal half already declares
  (§ Implementation status today → *Declared residue — ciphertext*).

**Bridged and mail rooms** enforce what the far side enforces; the capability
vector says which role-bearing operations reach the wire at all, and the page
grays the rest ([`../ui/conversations.md`](../ui/conversations.md)
§ Capability gating).

## Join rules and invites

The room policy names one **join rule**:

- **`invite`** (default): owner and admins invite; an invite is the inviter's signed act, delivered to
  the invitee's home nest through the inbox plane and subject to the invitee's
  own reach policy ([`direct-messages.md`](direct-messages.md) § Reach policy —
  an invite is initiation, and initiation is what the recipient's inbox mode
  mediates; the group-Welcome gate there applies unchanged). Acceptance seats
  the member: an end-to-end room's inviter commits the Add and delivers the
  Welcome; a community room's home nest writes the roster row on acceptance
  and the inviter's device wraps the generation bundle to the newcomer.
- **`member-invite`** — any member may invite; otherwise as above.
- **`request`** (community rooms only) — anyone who can name the room may
  ask; owner/admins approve. A request is a knock on the room, and is
  rendered with the knock affordances the inbox already has.

A room with the `request` join rule is a community room by construction: an
end-to-end room has no reader who can vouch for a stranger's request without
the members' own keys, so it never carries one. (`member-invite` changes only
which members may invite, and an end-to-end room carries it like any other.)

**The invitation's lifecycle moved 2026-09-28 to [`room-invitations.md`](room-invitations.md) § Join rules and invites**, verbatim and under this heading. Its lead-ins, so a search for any of them lands here: *An invitation shares its invitee's fate, as one thing*; *The inviter is attribution*; *An invitation is a standing offer, judged again when it is accepted*; *The inviter is judged by its own seat — never through its succession line*; *A lapse consumes the invitation*; *Pending invitations are visible to whoever may withdraw them, and withdrawable* (with `fauna.conversations.room.list_invites` and `room.revoke_invite`); *A cross-nest invitation — the home judges, the invitee's nest relays and decides nothing*, with its four rulings; and *The foreign-inviter leg*. The join rules above stay here.

## The home nest

**One home nest per room (decision 2, ratified).** A room is homed on exactly
one nest: the nest that stores its canonical log, holds its floor roster and
policy, originates every cross-nest leg, and — by default — holds its
sequencing seat. Every other nest **relays**: a member on a foreign nest
reaches the room only through their own home nest, which originates the leg to
the room's home over the nest↔nest channel — Fauna's existing "a client reaches
only its home nest, which originates the cross-nest leg" shape
([`direct-messages.md`](direct-messages.md) § Technical Flow — Cross-Nest;
[`../architecture/federation.md`](../architecture/federation.md) § Federation
residue surface), generalized to every class. A foreign member's home nest keeps
a relay copy of the ciphertext it fetched for its member, under the existing
`channel_foreign_members` structural gate; it never holds a wrap, so a
community room's readable views exist on the home nest alone.

This is deliberately **not** Matrix's replicated room state: there is no
per-nest DAG of room events to reconcile, no state-resolution algorithm, no
fork. One nest orders; the rest relay. The cost is availability — a room is as
reachable as its home — and the answer to that cost is transfer, below, never
replication.

**The room's home is the owner's home.** A room is born on its creating
member's home nest, so at birth the owner's home and the room's home coincide.
They can diverge afterwards only through transfer, and a transfer of ownership
to a member homed elsewhere re-homes the room. One consequence is load-bearing
below: **a room's owner is never one of its foreign members**, so the owner's
"transfer before leaving" refusal cannot fire on a relayed door.

**A foreign member's admission is a relayed door of its own too (ratified and BUILT 2026-09-26)** — `fauna.conversations.room.accept_invite_remote` → `fauna.federation.conversation.room.accept`, the seating twin of the leave below, gated on the invitation the home delivered rather than on the binding it writes; [`room-invitations.md`](room-invitations.md) § Join rules and invites → *A cross-nest invitation* owns the ruling.

**And a foreign member's invitation is one too (ratified and BUILT 2026-09-26)** — `fauna.conversations.room.invite_remote` → `fauna.federation.conversation.room.invite_issue`, the issuing twin of that accept, gated on the inviter's binding as the leave below is and run through the same-nest invite body on the home; [`room-invitations.md`](room-invitations.md) § Join rules and invites → *A cross-nest invitation*, its closing paragraph, owns the ruling.

**A foreign member's leave is a relayed door of its own (ratified 2026-09-11;
BUILT the same day).** § Roles and authorization grants *leave (remove self)* to
every role but owner with no homing carve-out, so a member homed elsewhere has a
departure — and it rides the relay like every other act, on the distinct kind
`fauna.conversations.room.leave_remote` → `fauna.federation.conversation.room.leave`
([`../architecture/federation.md`](../architecture/federation.md) § Federation
residue surface owns the call, its gate and its idempotence). The same-nest
`room.leave` is not that door: a leaver's own nest holds no room record for a
room homed elsewhere and answers "no such room". **On a community room a
departure ends the seat and the `channel_foreign_members` binding together,
purge first**, exactly as a removal does ([`community-rooms.md`](community-rooms.md)
§ Implementation status today → *A removal severs* owns that pairing and its
order; an end-to-end departure moves the seat alone, § Roles and authorization
→ *Leaving — the mechanism*) — and the seat is
the half that must not be skipped: an unoccupied seat still obliges every later
generation mint to wrap to it and still refuses the departed member's
re-admission. Leaving still does not rotate (same §).

**Attachment bytes rest on the home nest (ratified 2026-09-09; BUILT the same
day).** A room's attachment blobs are part of the room's plane, so they rest
where its canonical log rests — on the home nest, beside the record and the
plaintext `attachment_refs` that pin them past the blob GC
([`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest →
*Attachment reachability* owns the pin; this paragraph owns *where the bytes
live*). A same-nest member uploads to its own nest under its session bearer, as
before. A member homed elsewhere never uploads to its own nest — bytes there
would be named by no record and served to no one — and never sends bytes over
the nest↔nest channel: bulk bytes never ride federation
([`../architecture/federation.md`](../architecture/federation.md) § Cross-nest
shared folders + channel append). Instead the two byte legs mirror the
cross-nest shared-folder plane one rail over. **Write:** the member's own nest
relays a short-lived, write-only bulk token from the home nest
(`fauna.conversations.blob.write_token.get` →
`fauna.federation.conversation.write_token.mint`, behind the home nest's
structural foreign-member gate — the `channel.fetch` gate verbatim, with no
`access == 'writer'` arm because a room has no claimant and every member
posts), and POSTs the sealed attachment DIRECT to the home nest's ordinary
`POST /api/v1/blob`, which accepts the token on the same door a session bearer
uses. **Read:** every member fetches by content address from the home nest's
public `GET /api/v1/blob/{hash}` — a foreign home reached direct, plain WebPKI,
integrity resting on the content address exactly as a cross-nest shared-folder
download does. The client picks the leg by the channel's recorded home, the
same `ChannelHome` signal that picks `channel.send` vs `send_remote`, in shared
Rust for all 7 apps (`ConversationsRpc::blob_put` / `blob_get` carry the home
URL). No metering or byte cap rides the token: a room carries no owner-pays
policy (that is folder policy — `federation.md` § Cross-nest → *Substrate vs.
policy*), so the POST is bounded by the same-nest door's `BLOB_BODY_LIMIT`
alone, and reclaim follows the record on the home nest
([`backup-restore.md`](backup-restore.md) § 9 step 2g). A re-home by transfer
(below) moves the bytes with the log — the segment backup protocol's own
snapshot/restore carries the blobs the records name — so residency never
splits.

**The reader bounds what it fetches (2026-09-12).** A message's attachment list
rides inside the sealed body, so neither the nest's per-record pin nor any
send-time check can bound it: the bound is the reader's own, in the one shared
receive loop every app runs. A member walks at most
`fauna_core::attachment_limits::MAX_ATTACHMENTS_PER_RECORD` (64) entries of one
message's list — an entry past that names a blob its own record never pinned —
and skips the rest. It refuses, before fetching, an attachment whose declared
plaintext size exceeds `INLINE_BLOB_BODY_LIMIT` (10 MiB): that is the upload
door's own ceiling above, so no legitimate sealed blob is larger, and a fetched
blob over it is skipped unopened. `blob_get` holds the same ceiling on the
wire — the native build refuses a declared length over it and stops reading a
body that runs past it; the web build, whose fetch hands a body over whole,
refuses a declared length before the read and an over-long read before the
open — so a home nest answering a named content address with more bytes than
its door accepts cannot make every member open them. An opened plaintext whose
length is not the declared size is a failed open, skipped like any other. The
upload door reads the byte ceiling and the pin the count, from that one module;
the labeler's facet loop reads the count too, but bounds bytes by its own
smaller ceilings, which follow from the module's memory rather than from the
door ([`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md)
§ Tier-3 community models & background re-processing → *The attachment facet*,
rule (4)).

**The sequencing seat.** The home nest holds the room's seat by default. The
seat is a re-pointable role a member device may also hold, with its own
seat-epoch chain and fencing — owner [`p2p.md`](p2p.md) § Offline share
initiation; a room whose seat sits on a device is still *homed* on the nest
that stores its plane, and the seat re-points to that nest by ordinary handover
when it is reachable. This doc adds nothing to the seat mechanism.

**Transfer by succession.** A room's home moves in exactly two ways, both
ceremonies with a signed record, never a config edit:

- **Identity succession of the owner** ([`identity-succession.md`](identity-succession.md)):
  the successor joins every room as a new member — the add-successor /
  remove-old MLS pair, or the recipient-set roster's fresh entry with the
  predecessor `Removed` — and the ownership re-point carries the owner role to
  the successor ([`succession-propagation.md`](succession-propagation.md)
  § Propagation). If the successor is homed on another nest, the room re-homes
  there as part of the same aftermath: the old home relays until every member's
  home nest has re-pointed, then holds ciphertext only.
- **Ownership transfer** (owner's act, § Roles): the new owner's home becomes
  the room's home by a signed room-succession record the old home publishes to
  every member's home nest; membership, policy and the log move by the segment
  backup protocol's own snapshot/restore
  ([`../architecture/segment-backup-protocol.md`](../architecture/segment-backup-protocol.md)),
  the community generation bundle is re-wrapped to the new home's room-read
  key and the old home's wrap rotated out. The old home becomes a relay.

**The succession axis.** The floor roster's tables declare their re-point
verdict beside their deletion policy like every per-actor table
([`succession-repoint-axis.md`](succession-repoint-axis.md)); the standing
`Stay` on `group_members`/`groups.owner_id` (both since retired, § The group
plane's fate) was ruled *conditional on the plane being dark*, so the floor
roster's first build re-ruled it: a room-ownership
row **moves** with the owner (the ownership re-point already carries every
other owned plane), a membership row stays as a `Removed`-absorbing history
with the successor's fresh entry beside it.

## History for joiners

**Per-room policy (decision 3, ratified)**, one of:

- **`none`** — a newcomer sees the room from their admission; nothing before.
  The MLS default, and the default for every room.
- **`full`** — a newcomer receives the room's history. Realized by an
  **existing member re-sealing a history slice to the newcomer**: for an
  end-to-end room, the same `history/<channel_hex>` state-replica shape the
  user's own devices already re-seal to one another
  ([`devices.md`](devices.md) § Cross-device MLS group-state sync;
  [`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest, the
  MLS-cryptographic group state bullet) less two things: its attachments'
  fetch coordinates (the newcomer holds no key for the epochs they name), and
  any **derived state naming a message the newcomer already holds** — the
  reaction/delete state the slice carries is taken for the history the slice
  is *giving* them, which this policy already trusts the inviter to supply,
  but never for a message that reached them first-hand, since a slice carries
  the folded verdict with no claimant for `DeleteClaim::admits` to judge (§
  Roles and authorization). A tombstone or reaction log naming a message the
  slice does not carry at all is refused for every reader, inviter or own
  device, in `ConversationsManager::restore_channel_slice` — the manager's two
  maps are keyed globally, so an unbounded slice would be a forged delete
  reaching out of its own channel. Produced
  by the inviting device and
  delivered as an opaque blob the nest stores and forwards; for a community
  room, the retained generation bundle wrapped to the newcomer at admission —
  the recipient-set scheme's archival backfill — after which the newcomer opens
  the sealed log itself through the ordinary read feed.

The policy is part of the owner-signed room policy (§ Roles and authorization);
in an end-to-end room a member refuses to produce a history slice the policy
does not authorize, in a community room the nest refuses to store one. Nothing
in either path gives a nest a key it did not already hold: an end-to-end
history blob is sealed to the newcomer, and a community backfill is a wrap.

### What a device accepts

**Ruled 2026-09-21.** An end-to-end history slice is an MLS application
message, and MLS lets **any** member seal one at **any** time — so the
producer-side refusal above binds honest members only. What makes a slice
*history* is decided by the device that receives it. A device folds a member's
`HistorySlice` only when **all** of the following hold, and otherwise folds
**nothing** of it:

1. **The policy says so.** The room is governed and the policy this device
   holds when its walk meets the slice is `full`. (The walk is in log order, so
   that is the slice's own epoch's policy unless a commit landed between the
   Add and the slice — and then the newer policy is the one obeyed.)
2. **It is from this device's inviter, for this device's admission.** The
   slice was sealed by the **account whose Welcome this device joined from**,
   in the **epoch this device joined at**. Both are read off what MLS
   authenticates — the Welcome's sender and the joined epoch, recorded by the
   engine at join; the slice's leaf credential and framing epoch, read at
   decrypt — and never off a field of the slice. Account-level on purpose: a
   joiner holds no record of which of the inviter's devices committed the Add.
3. **It is the first.** A join records **one history admission**, spent by the
   first slice that clears rules 1–2 (`MlsEngine::take_history_admission`;
   durable, so a relaunch re-opens nothing). A device that created the room,
   that was already a member, or that has taken its slice holds no admission
   and accepts no slice. A slice that fails rules 1–2 spends nothing —
   otherwise any member could burn a newcomer's history by posting first.
4. **It names only this channel's past.** Every carried message id is
   `conv:{this channel}:{seq}` with `seq` below the slice record's own seq.
   One id outside that and the slice is **refused whole, never filtered**: an
   honest slice is a snapshot of the channel's own thread and cannot contain
   one, so nothing else in such a slice is believed either (and rule 3's
   admission is already spent — a hostile inviter gets one attempt). The
   producer drops any entry of another shape before sealing, so an honest
   inviter never authors a slice rule 4 refuses.

**A slice is taken for its messages, and the derived state on them, alone.**
Not its `label`: a governed room's name is a field of the owner-signed policy
(§ Roles and authorization), and the newcomer reads it off the group context
its Welcome carried, at join. Not its participants, and not its anchor-grade
provenance **whether recorded or absent** — the roster is the group's, and
which handle an owner typed is the user's own devices' datum, the one
`ChainWitness` picks its dial target by; a member's slice marks no row. Not
attachment coordinates, not parked floor delete records. The member door is
therefore its own (`ConversationsManager::restore_joiner_history`) and never
the own-devices restore, which adopts all of those.

**The bound in rule 4 is the slice's own seq, not the admitting Add's — weighed
and refused.** The joiner never processes the Add (it joins from the Welcome),
so it holds no authenticated datum for the Add's position on the log: a commit
framed at the prior epoch is something any earlier member can plant, and a
planted bound would refuse the honest slice whole. The tighter bound would buy
nothing besides: the walk is in log order, so every record between the Add and
the slice that this device can read is already folded first-hand when the
slice arrives, and the first copy of an id stands. What stays inside the
inviter's trust is exactly what `full` grants — one slice, some time in the
newcomer's first epoch, fabricating whatever past it likes.

**The user's own devices' replica is a different trust root and is untouched**
by all of the above: `history/<channel_hex>` sealed under the user's own
backup key keeps the full restore (label, provenance, coordinates, parked
records).

**The inbound walk's already-folded skip is thread-local.** The walk may step
over a record *before opening it* only when the thread bound to **that
channel** already holds `conv:{channel}:{seq}` — a record's id is minted by its
own channel's log, so no other thread can honestly hold it. The store-wide
form of that question is what let an id planted in one room silence a record
in another, unread and with the cursor already past it. Both arms of the walk
(MLS application, community `RoomSealed`) ask the thread-local one.

**The current generation is not history (ruled 2026-09-10, at the community
backfill's build).** Under `none` the set a community room's nest will store a
backfill wrap for is the **tip alone**, not the empty set. A community room's
generation coverage is fixed at **mint** time over the floor as it stood then,
so a member seated after the last mint holds no wrap for the tip either — it
cannot read what is said *after* it joins, which is not what any history policy
asks for. Covering the tip is the scheme's own admission/mint-race heal (the
member top-up, [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
§ The recipient-set scheme); everything **earlier** than the tip is the history
`none` withholds. The alternative reading — `none` authorizes nothing — would
not enforce a history policy, it would make a newcomer a member that cannot
read the room at all.

Two things history policy does not do: it never rewrites what a member already
holds (a policy change to `none` stops future slices, it does not claw back
delivered ones — "forward secrecy from yourself is not a meaningful threat",
[`../architecture/mls-group-key-material.md`](../architecture/mls-group-key-material.md)
§ Voluntary member leave), and it never affects the segment store's own
retention, which is the home nest's backup posture, not the room's.

## Bridged rooms

A bridged room is a room whose transport is a bridge and whose member set
includes the bridge principal — hence transport-only by derivation. **This
family never depends on the group plane in either form** (the one constraint
the third-party chain contributed): a bridged room's membership lives in the
floor roster like every room's, reported by the bridge where its capability
vector says membership change reaches it (`conversation.room.members`,
[`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Bridge-kind
catalogue → Phase G), and never in Mechanism B's tables or kinds. **How the
roster is seated (ruled 2026-10-02):** the
nest seats it at the room's birth — `conversation.rooms.open` (the user's act)
or `conversation.room.upsert` (the bridge's) — with the user as a `User`
principal and the bridge principal as a `Bridge` one (a consented roster row,
or a first-party in-process leg's reserved id, the nest-owner precedent of
[`../architecture/third-party.md`](../architecture/third-party.md) § The
principal model → *Hosted principals*); the `rooms` row is born
`transport_only` and nothing re-classes it (§ The three classes — a class
changes only by changing members, and the bridge member is the room's reason
to exist). A `room.members` report replaces the far side's seats only; the
user's and the bridge's rows are never written by a report. The bridge's
seat outlives its principal — a revoked bridge's room stays transport-only,
since a bridge did read it — and is rewritten only when another principal
adopts the room by declaring its bridge id
([`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Bridge-kind
catalogue → Phase G → *When the bridge stops serving* owns the fate). Everything
else about the adapter — the one variant, the one backend, the blind sealed
mailbox, the user-side kinds — is owned by [`../ui/conversations.md`](../ui/conversations.md)
§ Where logic lives → *The `Bridged` adapter*.

## The group plane's fate

Mechanism B (`fauna.conversations.group.{create, send_message, invite, remove,
react, delete, list_members, list_messages, list_for_actor}` — **nine** kinds —
and the `groups` + `group_members` tables; its as-built contract lived in
[`groups.md`](groups.md), now a pointer stub) is the **ancestor** of this doc's community class and
floor roster, and it ends as the last step of the room model's build, in this
order:

1. **Re-home `group.remove`'s consumer first.** The conv custody serve door
   reads the floor roster (§ The floor roster) before the group plane changes,
   so no serving path ever points at a retired table.
2. **Mint the room plane.** The floor roster and room policy tables, the
   community class's sealed send, the `fauna.conversations.room.*` family that
   carries the operations of § The room — additively, beside the old kinds
   ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md):
   the wire only ever grows within a major). Whether the two old tables are
   **reshaped** in place (columns added: class, home nest, principal kind, the
   policy blob; `role = 'user'` re-spelled `member`) or the room tables are
   **minted fresh** and the old two dropped is the build's call, with one
   tie-breaker: no app ever wrote a row to either table, so a fresh mint costs
   nothing a user can lose.
3. **Retire the nine kinds and, if fresh-minted, the two tables — under the
   alpha carve-out's approval gate, which is absolute even here.** The
   deletion is surfaced to the user before it lands, naming what dies: the
   nine kinds and their `bridge_method_allowlist` / `offline_class` /
   `kind.rs` registrations, the two tables and their `ACTOR_TABLES` +
   succession declarations, the 35 nest conformance tests of
   `conformance_conversations_group.rs` and the wire-fidelity test
   `test_group_message_bare.py`, the unused app-side builders
   (`libs/fauna-client-core/src/group.rs`, the wasm/UniFFI/C-ABI twins) and
   the out-of-scope watchOS view that is their only caller — and what a user
   must redo: nothing, because no shipped app ever reached the plane. Without
   that approval the plane stays, dormant, beside the room family.

**Steps 1–3 are executed (2026-09-26).** The user approved the deletion exactly
as step 3 enumerates it, and it landed in one change. The room tables were
minted fresh (step 2), so `groups` + `group_members` were **dropped** —
nest schema 86, by a one-shot step the 2026-10-04 genesis collapse folded
away; no genesis creates either table. The enumeration turned out to be a floor,
not a ceiling: the set that had to go with the tables and kinds also included
the nest's `group_routes.rs` (its one live helper, the inbox path's
`(ContactRequest, Post)` verifier, moved beside its only caller in `routes.rs`),
the group handlers in `conversations_handlers.rs`, the account-deletion
cascade's `delete_group_memberships_for_actor`, the `ACTOR_TABLES` and
`SUCCESSION_REFERENCES` declarations and the export's `groups` domain
(`export/groups/<id>.json` in the archive, and the logical-dump table entries),
the `posted_to` link index only the plane's message store wrote, the
`ConversationsClient::group_*` methods, the C-ABI `fauna_group_*` exports and
their Python e2e twins, the wasm `buildGroup*`/`decodeGroupAction` exports, the
UniFFI `build_group_*`/`decode_group_action` exports with the `FfiGroupAction*`,
`FfiAttachment` and `FfiKeyValue` records, the Swift `FFICompat` wrappers and
the `APIClient` HTTP twins of the same plane, the watchOS `GroupChatView` and
`GroupListView` (the builders' only callers), and the regenerated tracked Go
binding. The conformance file held 36 tests, not 35. `tier_mls_groups` and the
account-data storage-group plane were not touched, and any `group/message` row
or `posted_to` link already in the shared `content` tables was left in place —
not part of the approved set, and kept out of every other plane by the
one-id-one-plane guards ([`../architecture/nest/common.md`](../architecture/nest/common.md)
§ One id, one plane). The one remnant — the Android app's local Room
`GroupMessage`/`GroupMessageAttachment` entities and `GroupDao` — is removed
too, via a Room schema migration.

**What was explicitly not in scope of that retirement — and has since gone
with its own plane:** `tier_mls_groups`, the subscriptions-plane table that
linked a legacy MLS-keyed tier to its epoch material. It shares a word with the
group plane and nothing else, which is why the group-plane retirement left it
alone; the compat-remnant sweep then retired the tier-MLS plane on its own
terms ([`restricted-posts.md`](restricted-posts.md) § Encryption at rest, room ruling 8,
lifted 2026-09-27 — the writer-less table left the schema with that plane's
retire step, schema 91). The earlier re-earmark of "community/user groups"
that named it beside `groups` + `group_members` is superseded by this doc: a
community that is also an audience for group-restricted posts is a community
room, and the post seal it uses is the `Posts` row's own.

## Architectural rules

1. **Class is derived, never stored, never chosen.** One function of the
   member set in `libs/fauna-conversations`; every app renders its result and
   none computes it.
2. **Membership is lifted out of the adapter.** A `RailBackend` projects a
   room; it does not own one. A new transport is an adapter plus its capability
   vector, zero app edits — the existing rule
   ([`../ui/conversations.md`](../ui/conversations.md) § Architectural rules).
3. **The nest enforces roles only where it is a member.** Inside an
   end-to-end room the nest is a relay and a mirror; its verdicts on
   membership are for routing, custody and succession, never for reading.
4. **One storage shape for every class** — the `conv` kind's per-room scope,
   sealed envelopes, one read feed; classes differ in who holds a key, not in
   where bytes rest.
5. **Home moves by ceremony.** A signed record, a relay window, a re-wrap —
   never a per-nest replica of room state, never a hand-edited pointer.
6. **The room policy is signed by a principal whose role covers the change
   — the owner for the owner and the admin set, owner or admin for the
   rest** — and verified by members in every class; the nest stores it,
   refuses what the signature does not cover, and cannot author it. A
   community room's labeler set is "the rest", carried in a signed record of
   its own beside the policy (§ The three classes → *What the home nest does
   with its read*).

## Don't do these

- Don't add a "make this room readable by the nest" toggle. A community room
  is made by *inviting the home nest* — a membership change the members see on
  the roster, not a flag. Promoting an existing end-to-end room's transcript is
  impossible by design; start a community room.
- Don't put the home nest in a community room's key **authority** set. It is a
  reader recipient; owner and admin devices mint and rotate.
- Don't let the nest's floor roster decide who can *read* an end-to-end room.
  It mirrors; MLS decides.
- Don't reintroduce a per-rail encryption constant, a per-rail roles table, or
  a `groups` page. One room model, one page.
- Don't retire a plane's table without the deletion approval in hand, and
  never as a rider on another plane's retirement: the group plane went with
  schema 86, and `tier_mls_groups` — never the group plane's — left with the
  subscriptions plane's own retire step (schema 91) ([`restricted-posts.md`](restricted-posts.md)
  § Encryption at rest, room ruling 8, lifted 2026-09-27).
- Don't replicate room state across nests to buy availability. Transfer the
  home instead.

## Done definition

- [x] `ThreadEncryption` (or its successor) is derived from the member set in
      shared Rust; a `libs/fauna-conversations` unit pins the three classes
      (2026-09-08, the native rail; the other rails' constants retire with
      the `Bridged` adapter).
- [x] Every room has a floor roster on its home nest; the custody serve door
      reads it and the conformance conv arm's leave-severs case drives a live
      room, not a Mechanism B fixture (2026-09-09, schema 51 +
      `fauna.conversations.room.roster_report`;
      `bins/fauna-nest/tests/conformance_conversation_rooms.rs`). ⚠ The
      client reports through the wired `RoomRosterReporter` seam on all
      seven apps (2026-09-09), and reports every end-to-end room's roster at
      its birth (2026-09-10) — so a fresh 1:1 has one — and backfills a room
      born before that change on the poll (2026-09-11); and a member can walk
      out from the app — the departing-member report got its producer
      2026-09-20, painted on tui, the other six apps following in the batched
      trickle-down (§ Implementation status today).
- [x] A community room: created on tui with the home nest invited, class
      rendered on the header, sealed log opened by a second member on a second
      nest through the relay, the home nest's search index built from the
      sealed log and deleted on rotation — tier_3, two seats on one machine.
      **Closed 2026-09-26**: the second-nest clause is witnessed by
      `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py`
      — two seats on two TLS loopback nests, founded on tui with a foreign
      handle in the picker, the knock crossing to the member's nest, the
      accept relayed back, the foreign seat keyed by the founder's device, a
      sealed message each way through the relay ([`room-invitations.md`](room-invitations.md) § Join rules and invites →
      *A cross-nest invitation*). The history below is how the line closed:
      ⚠ The nest half of this line is done (2026-09-09, schema 57): the index
      **is** built from the sealed log and **is** deleted when the members
      rotate the nest's wrap out, pinned by
      `bins/fauna-nest/tests/conformance_conversation_rooms.rs` — and since
      2026-09-10 a member can **read** it, through
      `fauna.conversations.room.search` (§ Implementation status today). Since
      2026-09-10 the shared manager founds, invites, joins and keys a room
      and rotates the nest's read (§ Implementation status today, *The class
      reaches the app layer*). The tui paint landed 2026-09-25, and every
      clause but the second nest is witnessed by the tier_3 two-seat journey
      `tests/e2e-unified/tests/test_conversation_room_community.py` — founded
      on tui with the home nest in, the class on the header, the sealed log
      opened by a second member, the nest's index finding the text and emptied
      when the read is withdrawn — with both seats on **one** nest. The
      cross-nest half followed 2026-09-26: the community invite to a member
      of another nest is built, and the second journey above joins through
      the relay.
- [x] An end-to-end room refuses a Remove commit from a plain member — a
      `libs/fauna-mls` test red-verified against a build with the policy check
      removed (2026-09-08, `libs/fauna-mls/tests/room_policy_commits.rs`).
- [ ] An owner's or admin's delete of another member's message tombstones it
      on every seat, in both classes, and a plain member's is dropped as forged
      exactly as today — a shared-Rust test red-verified against a build with
      the role check removed, the community floor's refusal pinned on the nest,
      and a tui journey (the end-to-end class: 2026-09-19, shared-Rust test
      red-verified, three-seat tui journey green; the community class and the
      other six apps' affordance unbuilt — § Implementation status today).
- [ ] History policy `full` delivers a history slice to a newcomer on both
      classes; `none` delivers nothing (the end-to-end class: 2026-09-08,
      `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`; the
      community class waits on the nest plane).
- [ ] Owner succession re-homes a room to a successor on another nest; the old
      home relays; the succession-axis declarations for the roster tables are
      re-ruled and data-driven-tested.
- [x] The group plane's fate executed to step 3 or explicitly parked at step 2
      with the approval ask recorded. **Executed to step 3, 2026-09-26** — the
      user's approval in hand; § The group plane's fate records what went.
- [ ] The goal-doc lint green; registry rows `room-model` +
      `community-rooms` resolve here.

## Reading list

1. [`../architecture/third-party.md`](../architecture/third-party.md) § The rulings — TP8, TP10, TP12 (the ruling this doc executes).
2. [`../ui/conversations.md`](../ui/conversations.md) — the page, capability gating, the `Bridged` adapter.
3. [`direct-messages.md`](direct-messages.md) — the MLS channel plane the end-to-end class rides.
4. [`groups.md`](groups.md) — the retired plane's pointer stub (2026-09-26); the old contract is in git history.
5. [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md) § The recipient-set scheme; [`../architecture/account-data-plane.md`](../architecture/account-data-plane.md) § The ratified decisions → R15.
6. [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md) § Audience: a storage group; § Audience: deployment infrastructure.
7. [`p2p.md`](p2p.md) § Offline share initiation — the sequencing seat.
8. [`succession-propagation.md`](succession-propagation.md), [`succession-repoint-axis.md`](succession-repoint-axis.md) — what succession obliges of a room.
9. [`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md) § Shared-audience carve-out — the custody door the floor roster serves.
