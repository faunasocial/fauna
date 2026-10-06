# Direct messages — target state

Owns: direct-messages, mls
Status: ratified
Authority: the MLS channel protocol — DM channel setup (2-member policy), send/receive over the `fauna.conversations.*` kinds, key-package lifecycle (FIFO consume, 30-day expiry, last-resort marking, the unified replenish model), the same-nest + cross-nest Welcome delivery contract and the membership-gated cross-nest fetch relay, DM behavioral anti-spam, the platform MLS binding table, **and the N-member group-thread-fork mechanism riding this same channel plane** (§ Group-Forked Threads — `WelcomeChannelKind::Group`, lazy bootstrap, in-place add-member; this is the real, live, cross-app group-chat mechanism); defers the room model — membership, the three confidentiality classes, roles, join rules, the home nest, history for joiners — to [`conversation-rooms.md`](conversation-rooms.md) (this doc's N-member fork is its end-to-end class), the separate, currently-unreachable `fauna.conversations.group.*` RPC family + schema to [`groups.md`](groups.md), page UX (including the add-participant trigger) to [`../ui/conversations.md`](../ui/conversations.md), cross-device MLS group-state sync to [`devices.md`](devices.md), paired-nest mls_pull/ack buffering to [`../architecture/nest/private-mode.md`](../architecture/nest/private-mode.md), federation transport/trust to [`../architecture/federation.md`](../architecture/federation.md), at-rest properties to [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md), and the inbox drain-apply program to [`../architecture/core-client-kind-catalog.md`](../architecture/core-client-kind-catalog.md) § Inbox & Messaging.

Last verified: 2026-07-10 (cluster-#5 review + fix pass) | Sources: `libs/fauna-mls/src/channel.rs`, `bins/fauna-nest/src/conversations_handlers.rs` (same-nest `fauna.conversations.*` kinds), `bins/fauna-nest/src/federation_handlers.rs` (cross-nest `fauna.federation.*` channel kinds)

> **UI surface:** all DMs — across Fauna native, SMTP, Bluesky, Nostr,
> and Mastodon — render on the unified conversations page (`docs/goal/ui/conversations.md`).
> Behind the scenes, MLS-encrypted Fauna DMs use the protocol described
> below; SMTP / Bluesky / Nostr / ActivityPub DMs flow via the corresponding
> nest-side bridge providers. The 2-member-only DmChannel
> policy here matches the spec's "add-someone-to-1:1 forks a new group
> thread" rule (Signal semantics) — mechanism: § Group-Forked Threads below.

End-to-end encrypted DMs using MLS. See
[`../architecture/data-flow.md`](../architecture/data-flow.md) for the
message flow diagrams.

## Wire surface

The entire surface is WS-RPC. Same-nest request/reply rides
`fauna.conversations.channel.{send,fetch}`,
`fauna.conversations.keypackage.{upload,fetch,count}`, and
`fauna.conversations.welcome.deliver` (the legacy HTTP twins are deleted).
Cross-nest legs ride the nest↔nest `fauna.federation.{keypackage.fetch,
welcome.deliver, channel.fetch}` channel — a client reaches only its *home*
nest, which originates the cross-nest leg (`../architecture/federation.md`
§ Federation residue surface). Push notifications are **typed push frames
only**: Welcomes arrive as
`PushEvent::Welcome { welcome_bytes, channel_id?, nest_url?, channel_type?, group_id? }`
(the recipient feeds `welcome_bytes` to the MLS engine directly — no
follow-up inbox fetch on the push path); channel ciphertext arrives as
`PushEvent::ChannelMessage`. No untyped JSON WS framing remains
(`bins/fauna-nest/src/ws.rs` encodes only typed `PushEvent`s).

---

## User Experience

1. User opens the inbox and taps "New DM", then selects a contact.
2. Client fetches the peer's MLS key package from their nest.
3. Client creates an MLS `DmChannel` locally — this is a 2-member-only channel.
4. Client sends a Welcome message to the peer via the peer's nest.
5. Messages appear in the unified inbox — there are no protocol-specific DM views.
6. All encryption and decryption happen client-side; the nest relays ciphertext only.

---

## Technical Flow — Same Nest

Both actors are registered on the same nest instance.

### 1. Key Package Fetch

```
fauna.conversations.keypackage.fetch { actor_id: <peer_actor_id> }
```

- Returns and **consumes** the oldest non-expired one-time key package (FIFO order).
- Packages expire after 30 days.
- If the one-time pool is empty, the nest falls back to the peer's reusable **last-resort** key package (published via `keypackage.upload` with `last_resort: true`) and returns it **without consuming it**, so the peer stays reachable as long as a last-resort package is on file. Only when neither a one-time nor a last-resort package exists does the fetch return nothing, and the peer cannot receive new DM initiations until they publish more (`bins/fauna-nest/src/db/channels.rs::take_key_package`).

### 2. DM Channel Creation (client-side)

```rust
let (dm_channel, welcome_msg) = DmChannel::create(engine, peer_key_package)?;
```

- `DmChannel::create` is purely local — no network call.
- Returns the new `DmChannel` and an `MlsMessageOut` containing the MLS Welcome.
- The channel ID is stable and derived from MLS group state.

### 3. Welcome Delivery

```
fauna.conversations.welcome.deliver { actor_id: <peer_actor_id>, channel_id, welcome_bytes }
```

- Carries the MLS Welcome bytes.
- Nest stores the Welcome in the recipient's inbox via `push_inbox`.
- Nest notifies the recipient with a typed `PushEvent::Welcome { welcome_bytes, channel_id, … }` push frame.

> **Target state (designed 2026-08-17, unbuilt):** the sequencing/delivery
> seat this flow implicitly gives the channel-home nest becomes a named,
> re-pointable role a member device can hold — owner
> [`p2p.md`](p2p.md) § Offline share initiation (the delivery-seat design);
> nothing in the flow above changes until that builds.

### 4. Recipient Joins

The recipient processes the Welcome locally:

```rust
let dm_channel = DmChannel::join(engine, welcome)?;
```

After joining, the recipient has the same `channel_id` and can send and receive messages.

### 5. Sending a Message

1. Client encrypts the plaintext locally using the MLS engine to produce ciphertext.
2. Client sends the ciphertext:

```
fauna.conversations.channel.send { channel_id, envelope }
```

- Nest appends the ciphertext envelope to the per-channel `__conv/<channel_id_hex>` segment store (`bins/fauna-nest/src/segments/conv.rs::append`; mechanism in `../architecture/message-segment-store.md`).
- Nest assigns a monotonically increasing per-channel `seq`, mirrored in `segment_records` (Plan 7 cutover replaced the legacy `content(schema='channel/encrypted')` + `content_links(channel_seq)` storage).

### 6. Receiving Messages

**Push:** a typed `PushEvent::ChannelMessage { channel_id, data }` frame on the actor's WS connection.

**Poll (WS-RPC):**

```
fauna.conversations.channel.fetch { channel_id, since: <seq> }
```

Returns all messages with sequence number greater than `since`.

### 7. Decryption

The client passes each `data` blob to the local MLS engine for decryption. The nest never has access to plaintext.

---

## Technical Flow — Cross-Nest

Same channel creation and message posting as above. The difference is that Actor A's
client drives the **data plane only through its home nest (Nest-1)** over the
`fauna.conversations.*` WS-RPC kinds, passing the peer's `nest_url`; Nest-1 then
originates the cross-nest leg to Nest-2 over the nest↔nest `fauna.federation.*`
WS-RPC channel (since nest Spec Y2 slice 5 retired the HTTP federation interim —
`../architecture/federation.md` § Federation residue surface). The client's one
direct contact with a foreign nest is the **anonymous discovery hop** that
precedes all of this — the recipient picker's `fauna.actor.by_handle` against
Nest-2 over TLS — owned, with its failure semantics, by
`../architecture/federation.md` § Peer-auth model.

```
Actor A (Nest-1)              Nest-1              Nest-2              Actor B (Nest-2)
    │                           │                   │                       │
    │ conversations             │                   │                       │
    │  .keypackage.fetch        │ federation        │                       │
    │  {actor_id:B,nest_url:N2} │  .keypackage.fetch │                       │
    │ ─────────────────────────>│ ─────────────────>│                       │
    │ <─────────────────────────│ <─────────────────│                       │
    │                           │                   │                       │
    │ conversations             │                   │                       │
    │  .welcome.deliver         │ federation        │                       │
    │  {actor_id:B,channel_id:X,│  .welcome.deliver  │                       │
    │   nest_url:N2,            │  (+nest_url,       │                       │
    │   channel_type:dm}        │   channel_type)    │─ push to B's inbox ──>│
    │ ─────────────────────────>│ ─────────────────>│                       │
    │                           │                   │                       │
    │ conversations.channel.send│                   │                       │
    │  {channel_id:X}           │                   │  conversations         │
    │ ─────────────────────────>│   federation      │   .channel.fetch       │
    │   (buffered on Nest-1)     │   .channel.fetch  │   {nest_url:N1}        │
    │                           │<──────────────────│<──────────────────────│
    │                           │──────────────────>│──────────────────────>│
    │                           │  (Nest-1 returns  │   (B's drain pulls     │
    │                           │   ciphertext if B │    via its own nest)   │
    │                           │   is a member)    │                       │
```

### Steps

1. **Key package fetch** — Actor A calls `fauna.conversations.keypackage.fetch` on
   **Nest-1** with `{ actor_id: <actor_b>, nest_url: <nest-2> }`. Seeing the foreign
   `nest_url`, Nest-1 originates `fauna.federation.keypackage.fetch` to Nest-2 over the
   federation channel and returns B's key package to A. (`forbid_replay` is set on the
   destructive fetch — `../architecture/federation.md` § Key packages.)

2. **Welcome delivery** — Actor A calls `fauna.conversations.welcome.deliver` on Nest-1
   with `{ actor_id: <actor_b>, channel_id: X, nest_url: <nest-2>, channel_type: dm,
   welcome_bytes }`. Nest-1 originates `fauna.federation.welcome.deliver` to Nest-2,
   which wraps the Welcome bytes with `nest_url`/`channel_type`/`channel_id` and pushes
   it to Actor B's inbox (a `PushEvent::Welcome` on the typed-push path). The home
   `nest_url` B's drain later relays over is bound to Nest-2's handshake-**verified**
   view of Nest-1, never the per-request declared string, and B's client marks a
   same-nest channel explicitly so a hostile re-delivery cannot re-home it
   (`../architecture/federation.md` § Cross-nest shared folders + channel append).

3. **Message flow** — All messages flow through the channel's **home nest** (Nest-1,
   the group creator's nest), posted with `fauna.conversations.channel.send` and
   buffered in Nest-1's per-channel conv segment store. Because cross-nest
   conversations are **open federation** — *not* pairing-gated
   (`../architecture/federation.md` § Trust model) — and because a client reaches only
   its **own** nest (step's preamble; never a foreign nest directly), Actor B retrieves
   those messages by a **membership-gated fetch relay**: B's drain loop
   (`fauna.conversations.channel.fetch`) passes the channel's home `nest_url` (which B
   learned from the Welcome envelope, step 2) to **Nest-2**, which originates
   `fauna.federation.channel.fetch` to Nest-1. Nest-1 returns the channel's ciphertext
   entries **iff** the requesting actor B is a member of that channel **and** the
   request arrives over the federation channel verified to B's **home nest** — the
   membership gate `channel.fetch` enforces same-nest, extended cross-nest and bound to
   B's home `nest_id` so a hostile signer cannot harvest a channel it doesn't host
   (`../architecture/federation.md` § Trust model — "a nest-signature is attribution,
   not authorization"). MLS still carries the end-to-end guarantee: Nest-1 relays only
   ciphertext it cannot read (§ Security; `../architecture/federation.md` § Security).

3b. **Foreign-member send (ratified 2026-07-18; BUILT 2026-07-19 — kind, relay, shared-Rust client routing, and the two-nest client-level round-trip conformance: `conformance_cross_nest_conversations_client.rs::bob_receives_alices_cross_nest_message_through_the_relay`).** Step 3 covers only
   the home-nest member's send; before this fix a foreign member had **no** send path —
   B's plain `fauna.conversations.channel.send` appended to **Nest-2's local** log, which
   no channel member fetched (a silent blackhole). Fix: B's client calls the **distinct
   kind** `fauna.conversations.channel.send_remote { channel_id, nest_url, … }` on its
   own Nest-2, which originates `fauna.federation.channel.append` to Nest-1 — the
   mutating twin of the step-3 fetch, gated by the same membership-bound-to-home-nest
   check (`../architecture/federation.md` § Cross-nest shared folders + channel
   append owns the kind + gate). A distinct kind — not an additive `nest_url` on
   `channel.send` — is deliberate: an old Nest-2 ignoring the additive field would
   silently perpetuate the blackhole, while an unknown kind fails loud ("your nest
   needs an update"). The client picks `send` vs `send_remote` by whether it holds a
   foreign `home_nest_url` for the channel — the same signal that drives the step-3
   fetch relay. *Landed note (2026-07-19):* the pick lives in ONE shared place —
   `FaunaMlsBackend::send_on_channel` (`libs/fauna-conversations`), which every
   producer path (chat sends, scheduling iMIP, add/remove-member commits) routes
   through; the nest relay maps an old home nest's `unauthenticated` to the typed
   `fauna.federation.peer_nest_outdated` (NeedsUpdate class), and
   `channel.stale`/`permission_denied` ride through untouched so rebase-and-retry
   works unchanged on the remote path.

4. **Paired nests (the private nest-sync optimization)** — when Nest-1 and Nest-2 are a
   **paired** private↔public pair, Nest-2 may instead *eagerly buffer-pull* the same
   messages over the private nest-sync surface — `fauna.federation.sync.mls_pull`
   (`since_seq` cursor, ≤ 500 messages) + `fauna.federation.sync.mls_ack` (`up_to_seq`,
   purges Nest-1's buffer). This is an `is_paired`-gated convenience for paired
   topologies, **not** a precondition for cross-nest delivery — the step-3 fetch relay
   serves arbitrary unpaired deployments (the general federation case).

### Implementation status today

The drain-apply program's status is owned by `../architecture/core-client-kind-catalog.md`
§ Inbox & Messaging — Implementation status today (authoritative); the summary
matrix for this doc's surfaces:

| Surface | Status | Proof |
|---|---|---|
| Cross-nest fetch relay (step 3, membership-gated, bound to the verified origin `nest_id` via `channel_foreign_members`) | ✅ built | `bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs` |
| Paired `mls_pull`/`mls_ack` buffer-pull (step 4) | ✅ built | same conformance family |
| Native receive (typed push + shared drain ticker backstop, `InboxDrainSource` → `ConversationsSession::ingest_welcome_by_kind`) | ✅ built (2026-06-15) | tier_3 `test_fauna_mls_two_client_inbox_drain.py` (push arm suppressed, drain-only decrypt, two real engines) |
| Web receive (wasm `drainInbox` → `ingest_welcome`, driven by a 30 s ticker backstop **and**, since 2026-07-12, a `fauna.conversations.{channel.message,welcome.received}` push-wake arm — `apps/fauna-web/src/lib/conversations.ts::subscribeReceivePushes`/`wakeConvRail`; corrects the earlier "no push subscriptions in the web `WsRpcClient`" framing — the web `WsRpcClient` does carry `setOnPushEvent`) | ✅ built | tier_3 `test_fauna_mls_web_receive.py` (ticker path) + `test_fauna_mls_web_receives_from_linux_sender.py` — both **same-nest by construction** (one `nest_instance`, not a platform limit); the cross-nest row below is the same drain-apply path over a relayed Welcome |
| Cross-nest receive — the recipient's ingest reads the relayed Welcome's `nest_url` as the channel's home (`record_channel_home`), and the next rail sweep's `channel.fetch` carries it so the pull is relayed back to the sender's nest. **This doc is the sole owner of this status**; the kind catalog and `../architecture/federation.md` point here and state none. Not a per-app surface: one shared path, read by the wasm drain arm and the native `InboxDrainSource` alike — which is why the old "cross-nest **web** receive, `home_nest_url` discovery is native-gated" framing was doubly wrong (no such gate exists in any of the three libraries) | ✅ built (witnessed 2026-09-20; the code landed 2026-06-15 and no test had decided it since) | tier_3 `test_fauna_mls_cross_nest_receive.py` — green on tui and `--app web`. The channel's message log lives only on the sender's nest, so the decrypted body *is* the cross-nest hop: a receiver that recorded no home joins the group and shows an empty thread |
| Foreign-member **send** (step 3b: `channel.send_remote` → `fauna.federation.channel.append`) | ✅ built (2026-07-19) | `bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs::bob_receives_alices_cross_nest_message_through_the_relay` |

---

## Group-Forked Threads (N-Member MLS)

Adding a participant to a `(FaunaMls, OneToOne)` thread forks a **new
N-member MLS group** on this same channel plane (Signal semantics) — this is
the real, live, cross-app group-chat mechanism. (The legacy
`fauna.conversations.group.*` RPC family that once sat beside it, unreachable
from any shipped client UI, was retired 2026-09-26 —
[`groups.md`](groups.md).)

*Room-model note (ratified 2026-09-08): this fork is the **end-to-end class** of the room model — owner [`conversation-rooms.md`](conversation-rooms.md) — and membership lifts out of the rail: the room's floor roster on its home nest is a member-reported mirror of the MLS membership (never a read authority), roles arrive as an owner-signed authorization policy in the MLS group context that members verify before applying a commit, and history for joiners is per-room policy realized by re-sealing a `history/<channel_hex>` slice to the newcomer. Nothing in this section's mechanics changes; what the room model adds, and its current build status, are owned by that doc's § Implementation status today. The `Bridged` adapter half is owned by [`../ui/conversations.md`](../ui/conversations.md) § Where logic lives → *The `Bridged` adapter*.*

- **Trigger.** `ConversationsManager::confirm_add_participant`
  (`libs/fauna-conversations/src/manager.rs:1913`) — driven by the
  `thread-add-participant-button` overlay (`ui.yaml`; page UX owned by
  [`../ui/conversations.md`](../ui/conversations.md)). On a `(FaunaMls,
  OneToOne)` thread it forks a new thread and selects it; on any other
  `(rail, flavor)` — including an already-forked `(FaunaMls, MlsGroup)`
  thread — it adds the participant in place.
- **Fork, lazily bootstrapped on first send.** The forked thread has no
  MLS channel yet. `FaunaMlsBackend::bootstrap_group`
  (`libs/fauna-conversations/src/backends/fauna_mls.rs:1207`) creates it on
  the thread's first send: it fetches one key package per peer
  (`fauna.conversations.keypackage.fetch`, same-nest or cross-nest via the
  peer's `nest_url` exactly as in § Technical Flow above), calls
  `MlsEngine::create_group` with all of them — the same primitive
  `DmChannel::create` wraps for a single peer — and delivers the one
  resulting Welcome to every peer over `fauna.conversations.welcome.deliver`
  (or the cross-nest relay), tagged `WelcomeChannelKind::Group {
  group_id_hex }` (wire `channel_type: group` + `group_id`) instead of
  `WelcomeChannelKind::Dm`.
- **Adding to an already-forked group.** `FaunaMlsBackend::add_participant`
  (`libs/fauna-conversations/src/backends/fauna_mls.rs:1986`) fetches the
  new member's key package, stages an MLS add-commit (through the
  device-owned-epoch `CommitGate` when one is present, else directly against
  `MlsEngine`), delivers the newcomer's Welcome, and posts the commit
  ciphertext through the same `send_on_channel` routing point step 3b uses
  — so an add on a foreign-homed group correctly relays via
  `channel.send_remote` too.
- **Send/receive once forked.** From here on the forked thread is an
  ordinary MLS channel — the same `channel.send`/`channel.fetch`/
  `PushEvent::ChannelMessage` plane as a DM, just with more than two
  members (an N-member group, not a `DmChannel` — `add_member()` is no
  longer a `PolicyViolation`; `libs/fauna-mls/src/channel.rs`).

**Note on the `DmChannel`/`GroupChannel` wrapper types.** The Rust
snippets in § Technical Flow above (`DmChannel::create`, `DmChannel::join`)
describe the underlying MLS engine operations correctly, but no shipped
client actually calls those wrapper types in production — `DmChannel`/
`GroupChannel` (`libs/fauna-mls/src/channel.rs`) are used only by
`fauna-mls`'s own integration tests (`tests/dm_integration.rs`,
`tests/group_integration.rs`). All 7 apps integrate through the shared
`libs/fauna-conversations` crate (`ConversationsManager` /
`FaunaMlsBackend`, per priority #2), which drives the identical
`MlsEngine::create_group` / `join_from_welcome_bytes` /
`add_member_staged_from_bytes` calls directly — the wire behavior is
exactly as documented, just not via the `DmChannel`/`GroupChannel` struct
API a new client implementer would otherwise be tempted to reach for. The
*policy* half of that sentence used to say the same and did not hold:
2-member-DM is an app convention, not something the wrapper or the engine
enforces (§ Security Properties, corrected 2026-08-02) (§ Implementing DM Support on a
New App points at the real integration surface).

---

## Key Package Management

Clients should maintain a pool of pre-published key packages so that peers can always initiate DMs without waiting.

| WS-RPC kind | Purpose |
|-------------|---------|
| `fauna.conversations.keypackage.upload` | Publish packages — an array of hex-encoded MLS key packages (`last_resort: bool` marks the reusable last-resort package) |
| `fauna.conversations.keypackage.count` | Check how many valid packages remain |

**Guidelines:**
- Publish multiple packages at registration time (see `onboarding.md` — post-registration setup step).
- **Replenish model (unified, ruling 2026-07-10):** the shared `ConversationsSession` tops the pool up to `KEYPACKAGE_TARGET = 20` (`libs/fauna-conversations/src/session.rs`) — the one replenish mechanism. The former per-app page-load floors (apple/android Settings→Encryption "count < 5 → publish 10") are deleted (android 2026-07-14, apple 2026-07-16) — both Settings pages now show a read-only count with an optional manual refresh that calls `ensure_keypackages` on the shared session; the "count < 5" state survives only as a read-only "low" display flag, never an auto-mint trigger.
- Each one-time package is consumed exactly once (FIFO). If the one-time pool empties, the nest serves the actor's reusable **last-resort** package without consuming it (§ Technical Flow — Same Nest → *1. Key Package Fetch* owns the rule), so the actor stays reachable; only when neither a one-time nor a last-resort package is on file can peers not initiate new DMs to this actor until more are published.
- Packages expire after 30 days; the nest discards expired packages automatically.

---

## Reach policy — inbox mode on the DM plane (ratified 2026-08-02)

The recipient's **inbox mode** is enforced at DM initiation, nest-side, on the
same choke point the guardian floor uses (`welcome_deliver_core`): after the
routing floor's `Proceed`, the mode is "the caller's own routing"
(`ReachVerdict`'s contract — the floor itself stays mode-independent), applied
via the one shared mapping `fauna_core::data::dm_initiation_mode_verdict`.

**Per-mode initiation verdicts.** A mode acts on **new parties only** — an
`Accepted`/`Confirmed` contact keeps flowing under every mode, and a `Blocked`
sender is suppressed under every mode (the same rules the floor applies):

| Recipient's mode | A stranger's (or still-`Pending` knocker's) Dm/Group Welcome |
|---|---|
| `open` | delivers |
| `allow_knock` (default) | **refused** — the sender's path is the contact request, whose summary carries the intro message (the knock *is* the "message request" surface) |
| `contacts_only` | refused (the knock path refuses a stranger's knock too) |
| `closed` | refused |

**The refusal is one opaque typed error** — `fauna.conversations.forbidden`,
the same code the supervised floor answers with — so a sender cannot
distinguish `contacts_only` from `closed` from supervision by probing the DM
plane. Its i18n string (`error.conversations.forbidden`) points every refused
sender at the contact request, which is the remedy for each refusing cause.
Holding the Welcome itself in a second pending store was **rejected** for the
same reason it is forbidden on the supervised arm (`family-safety.md` § Don't
do these): the knock — which carries the sender's intro as its summary — is
the one staged first-contact surface; a parallel staged-Welcome rail would be
a duplicate concept.

**Scope — what exempts a same-nest Welcome from the mode, and it is NEST
STATE** (`welcome_mode_exemption`). Every same-nest initiation consults the
mode unless one of exactly two facts *this nest holds* says otherwise: the
delivery arrived on the **server-side scheduling rail**
(`deliver_scheduling_as_organizer`, the MDA CalDAV gateway's mailbox-less
delivery — a call site no client can select), or **the channel's folder claim
names the caller** (the `folder_channel_claims` row `folders.share` writes to
the owner before the client delivers the Welcome; first-binder-wins, and
refused outright on a roster-populated channel). Those are the two reaches this
section has always meant to allow — a stranger's share is staged by the
recipient-side pending-share gate (`../ui/folders.md` § Sharing owns that
routing), and a stranger's calendar invite is the CalDAV gateway's designed
reach (`../architecture/caldav-server.md` § Server-side auto-schedule owns that
trust story; it never surfaces as a chat thread) — but neither is keyed on
`req.kind` any more, for the same reason the cross-nest gate below is
kind-blind: a sender's self-declaration must not authorize itself. `Dm` and
`Group` were never exempt and still are not; a group Welcome is gated
identically to a DM, or the mode is evaded by minting a 3-member group (the
one-extra-Welcome evasion § Anti-Spam step 1 records). A **new** `WelcomeKind`
therefore arrives mode-gated and has to earn its exemption in nest state, which
is the safe direction to fail.

**The plane is nest state too, and that is what makes one exemption stay one
reach.** A channel this nest holds a folder claim for is a folder set's
channel, so every Welcome delivered onto it is stamped `channel_type =
"folder"` whatever the sender labelled it (its group id resolving from the
claimed set's own row when the label carried none). Only an unclaimed channel
takes its type from the label, and there the label merely *routes*: it bought
no exemption, so the floor and the mode authorized that delivery before the
type was ever read. Without this, an exempt first reach could be spent twice —
a stranger's ratified folder share does reach a `closed` recipient and does
seat them on the claimed channel, and the seat makes the next Welcome on that
channel read as in-band traffic; stamping the plane from the claim is what
stops a `Dm` label turning that seat into a chat thread.

**Cross-nest: the `closed` arm IS enforced; the other three are declared
gaps.** The mode is the *recipient's own nest's* fact. The relay leg
(`nest_url` set) runs on the **sender's** nest, whose view of a remote
recipient's mode is the empty default, so it does not consult it — enforcement
is the *receiving* nest's, at its federation ingest
(`federation_handlers.rs::welcome_deliver_handler`).

That ingest's wire carries no authenticatable sender, which splits the four
modes in two:

- **`closed` — enforced (since 2026-08-23).** It needs no sender identity: it
  is a fact about the recipient alone, and the sibling open-federation door
  (`fauna.federation.inbox.deliver`, via `deliver_inbox_payload_core`'s
  `"closed"` arm) had enforced it all along. Leaving the Welcome door open made
  the two disagree while the apps promised *"No new messages accepted"*
  (`inbox_privacy.closed_desc`) unqualified — and the Welcome door lands
  strictly more: an inbox row charged against the recipient's quota, a
  `PushEvent::Welcome` on their live socket, a device push, and
  `register_actor_channel` seating them on the channel.
- **`allow_knock` / `contacts_only` — still declared gaps.** Their verdicts
  turn on a *contact edge*, and with no signed sender every cross-nest party is
  a stranger; enforcing them here would refuse cross-nest *contacts* too.
  Follow-on (unbuilt), blocked on the contact model learning home-nests.

⚠ **The cross-nest `closed` gate is kind-blind, deliberately, and that
diverges from same-nest.** `req.channel_type` is peer-supplied and this handler
already refuses to trust it for authorization, so gating only the claimed
`dm`/`group` kinds would be evaded by relabelling one string — the same
unsoundness the 2026-07-10 `group/v1` schema-exemption correction removed from
the inbox path (a `schema` string the **sender** signs over their own post once
exempted it from the recipient's inbox mode and reach floor; **don't
reintroduce a routing decision keyed on a sender's self-declaration**), here
applied to a peer *nest's* self-declaration. Under `closed` every claimed kind is therefore
refused — including the folder shares and calendar invites § Scope exempts
same-nest. The cost is that a `closed`
recipient receives no cross-nest folder shares or calendar invites; that is a
defensible reading of the setting they chose, it is undone the moment they
leave `closed`, and it buys a gate a hostile peer cannot relabel its way past.

Re-Welcomes to a recipient already on the channel are in-band traffic, never
initiation, and flow regardless of mode — cross-nest as same-nest.

Proof: `bins/fauna-nest/tests/conformance_conversations_welcome.rs` (per-mode
matrix, contact flow-through, re-Welcome, group gating, the two nest-state
exemptions, the mode-exempt *label* buying nothing, the two-step reach, and the
relabel that cannot turn a folder seat into a chat thread) +
`conversations_handlers.rs`'s own unit for the scheduling gateway's rail +
`fauna_core::data` unit matrix. The cross-nest `closed` gate is pinned beside
its sibling door in
`bins/fauna-nest/tests/conformance_federation_channel.rs`
(`welcome_deliver_closed_inbox_is_forbidden`, the twin of
`inbox_deliver_closed_inbox_is_forbidden`) — it asserts no inbox row *and* no
roster seat, the half a bare delivery check would miss.

## Anti-Spam

The nest applies behavioral analysis to DM sends:

1. On each `fauna.conversations.channel.send`, the nest reads the channel roster from `actor_channels` and filters
   out the sender. The roster is *not* assumed to hold exactly two members — that is a product convention the
   protocol does not enforce (§ Security Properties), so all three outcomes are handled distinctly:
   - **exactly one other member** — a 1:1 DM. The nest records a `dm_sent` behavioral event for the sender with
     that member as `target_actor`, and the scorer (step 2) runs.
   - **no other member yet** — the recipient has not processed the Welcome, so only the sender is registered. The
     event is still recorded (with no `target_actor`) but the scorer is skipped: with no recipient there is no
     social context, and "no path found" cannot be told apart from "unknown".
   - **two or more other members** — a group-forked thread (§ Group-Forked Threads), which rides this same kind.
     The event is recorded with no `target_actor` and the scorer is skipped. Stated plainly, because the
     predicate is exactly what it looks like and no more: **a sender who adds any second participant exits DM
     fanout scoring permanently, by choice, at the cost of one extra Welcome.** `confirm_add_participant` is a
     shipped control (`thread-add-participant-button`), so this is self-service — the check tests "≥2 others on
     the roster right now", not group age, not who created it, not whether the roster predates the send.
     The exemption is still the right *shape* — the DM windows measure reaching many distinct individuals, and
     fanout is the wrong instrument for a thread the sender belongs to — but its honest cost is that group shape
     both reaches more recipients per channel and scores nothing, so the scored shape (1:1) is the one a spammer
     has least reason to use. One reused sock-puppet co-member exempts a channel permanently.
     **The long-term fix is not to fanout-score group sends** (a legitimate group send is not outreach); it is to
     measure outreach at the moment it happens — the add/Welcome — rather than at send time. Unbuilt, and not
     worth building while the label has no consumer (§ Implementation status today).
     The deferral *"group abuse is a membership question"* names a control that is **live since 2026-08-02**: the
     membership gate on this path is `welcome_deliver_core` → the reach floor **plus the recipient's inbox mode**
     (§ Reach policy) — under the default mode a stranger cannot Welcome a recipient into a group at all, so the
     exemption's reachable-by-strangers surface is now contacts-and-`open`-mode recipients. Closing that gate did
     **not** close this exemption (they were one knot with two ends): a sender the recipient accepted still exits
     fanout scoring by adding a second participant, so the honest-cost paragraph above stands unchanged.
2. When a single recipient is known, an anomaly scorer (`fauna_core::behavioral::compute_behavioral_anomaly`)
   computes a score in `[0.0, 1.0]` from the sender's recent DM-fanout windows (1 h / 24 h / 7 d), account age,
   public-posting history, DM response rate, and the sender↔recipient social graph (contact relationship, mutual
   contacts, hop distance derived from `contacts`). If the score is ≥ 0.3 a `spam/behavioral` content label (with
   the score as confidence) is applied to the channel. The label is informational — it does not block delivery.
3. **DM response rate is fed by the reply leg.** A send into a channel whose resolved recipient has *itself* DMed
   the sender inside the 7-day window is a reply, and records a `dm_replied` event **against that earlier sender**
   (one per correspondent pair per window). `dm_response_rate` is then *distinct correspondents who replied ÷
   distinct correspondents messaged* — a fraction bounded by construction, and robust to message volume. This feed
   is load-bearing: the 7-day rule is `unique_dm_recipients_7d > 10 && dm_response_rate < 0.05`, so if the writer
   ever disappears the second conjunct becomes structurally true and the rule silently degenerates to fanout alone,
   labelling honest users at 11 correspondents a week. Two flow tests in
   `bins/fauna-nest/tests/conformance_conversations_channel.rs` go red if that happens.
4. The contact system (knocks) is the first line of defense, and since 2026-08-02 the DM plane enforces it: a
   stranger's Dm/Group Welcome is refused under every mode but `open` (§ Reach policy; the knock flow itself is
   `data-flow.md` § Contact request (knock)).

---

## Platform MLS Implementations

Each platform uses a different binding to the same underlying `fauna-mls` Rust crate.

| Platform | MLS Library | State Storage |
|----------|-------------|---------------|
| Web | fauna-wasm (WASM) | the conversations manager's ONE web engine — multi-tab-safe via the nest `__mls` replica + CAS (owner: `devices.md` § Cross-device MLS group-state sync) |
| Android / iOS / macOS | fauna-ffi (UniFFI) | SQLite via MLS engine |
| Windows | fauna-ffi (UniFFI → C# via uniffi-bindgen-cs; the default-on `conversations-session` feature, consumed in-process over P/Invoke) | SQLite via MLS engine |
| Linux | fauna-mls (direct Rust) | SQLite via MLS engine |
| tui | fauna-mls (direct Rust) | SQLite via MLS engine |

The API surface is the same on all platforms: the shared `libs/fauna-conversations` session (`ConversationsManager`/`FaunaMlsBackend`) driving `MlsEngine::create_group`/`join_from_welcome_bytes`/encrypt/decrypt (§ Group-Forked Threads — Note on the `DmChannel`/`GroupChannel` wrapper types). Only the initialization and storage backend differ.

---

## Security Properties

- **End-to-end encrypted.** The nest stores and relays ciphertext only. Server-side decryption is not possible.
- **Forward secrecy.** MLS ratcheting means compromise of a current key does not expose past messages.
- **2 members by product convention, NOT by protocol enforcement (corrected 2026-08-02).** A DM is a 2-member group because the apps never offer an add on one — adding someone to a 1:1 forks a group thread instead (§ Group-Forked Threads). Nothing in the protocol enforces the count: `DmChannel::add_member()` returns `PolicyViolation`, but that is the *absence of a method on a wrapper with no production consumers*, not a rule in the group. One call to `MlsEngine::add_member` with the same `channel_id` seats a third leaf and the receive side accepts the commit — `process_commit` applies no roster policy, and nothing marks a group as DM-shaped for a policy to key on. This claim previously read as enforcement; it never was. Nothing is broken by it today (the wrapper is test-only, and no app offers the gesture), but **do not build on it as an invariant** — a guarantee here would need group-kind metadata in MLS state that does not exist. Escalating an existing DM to a group is still not a supported product flow; start a new group.
- **No server-side key escrow.** MLS state lives on client devices only.
- **Transport membership is by-activity; MLS — not the roster — is the removal boundary (ratified 2026-07-29).** On an unclaimed conversation channel the nest's `actor_channels` rows are delivery/routing state, self-maintained by activity: `channel.send`, `channel.fetch`, and Welcome delivery each register the acting local actor (`bins/fauna-nest/src/conversations_handlers.rs::register_actor_channel_gated`), so any authenticated local actor can (re-)enter the roster with one call. An MLS Remove therefore ends **cryptographic** access only — the removed member cannot decrypt post-removal epochs — and deliberately does **not** drop roster rows: a courtesy DELETE would silently revert on the removed member's next fetch and would misstate the boundary as transport-enforced when it is not. What a removed member (indeed, any local actor on the same nest) may still reach: the channel's ciphertext envelopes, their cadence/size metadata, and the storage they occupy — never content. The one conversation-roster row that *is* transport authorization is the **cross-nest** `channel_foreign_members` row (it feeds the `fauna.federation.channel.*` structural gate, and a foreign member has no self-service re-registration path); in v1 it is likewise not dropped on removal, so a removed foreign member's home nest retains ciphertext-relay read — same residue class, accepted by ruling. Real transport eviction for conversations arrives only with the future conversation-binding flow — its full obligation list is owned by `../architecture/mls-group-key-material.md` § M2 / Rotate-on-removal, not restated here — which must bring the roster-drop leg with it — the folder rail's "fetch authorization dies with the membership" is enforceable there precisely because folder channels are *claimed* and auto-register is suppressed; unclaimed channels have no such lever.
- **Content addressing.** Channel content IDs are derived as `blake3(channel_id || seq || envelope)`.
- **At-rest authority.** This doc owns the wire shape only. The per-content-kind at-rest property of channel envelopes and MLS group state — storage shape, client-side seal, and the floor/sealed split for memberships — is owned by [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Per-content-kind conformance rows `Conversation messages` and `Group memberships` (page-side spec: [`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest; segment mechanics: `../architecture/message-segment-store.md`).

---

## Implementing DM Support on a New App

Checklist for adding DM support to a new app:

1. **MLS engine** — initialize with the actor's secret key at startup (same as onboarding).
2. **Key package pool** — publish a batch of packages at registration; monitor count and replenish before the pool empties.
3. **Push listener** — on the actor's WS connection (`GET /api/v1/ws/{actor_id}` upgrade), handle typed `PushEvent::Welcome` frames (both same-nest and cross-nest — the cross-nest envelope includes `nest_url` and `channel_type`).
4. **Welcome processing** — on a `PushEvent::Welcome`, join via the shared `libs/fauna-conversations` session (`ingest_welcome_by_kind`, which drives `MlsEngine::join_from_welcome_bytes` directly — do not reach for `DmChannel::join`, unused by any shipped client, see § Group-Forked Threads) and persist the resulting channel state; wire the shared inbox drain (`fauna_client_inbox::drain` → the same `ingest_welcome_by_kind`) as the missed-push durability backstop.
5. **Send flow** — encrypt locally, `fauna.conversations.channel.send`, surface optimistically in the UI.
6. **Receive flow** — handle `PushEvent::ChannelMessage` frames and/or poll `fauna.conversations.channel.fetch` (with a `since` cursor) for missed messages.
7. **Decryption** — pass the `data` field from each message to the MLS engine; render only after successful decryption.
8. **Cross-nest initiation** — when the peer is on a different nest, call `fauna.conversations.keypackage.fetch` / `.welcome.deliver` on **your home nest** with the peer's `nest_url` (and `channel_type: dm`); the home nest relays the leg to the peer's nest over the `fauna.federation.*` channel. The data plane never calls a foreign nest directly; the only direct contact is the anonymous `fauna.actor.by_handle` discovery hop the recipient picker runs first (`../architecture/federation.md` § Peer-auth model, including what happens when that nest does not answer).
9. **Error handling** — handle 404 on key package fetch (pool empty), expired Welcome messages, and MLS decryption failures gracefully.

## Implementation status today

- **The room model (ratified 2026-09-08, [`conversation-rooms.md`](conversation-rooms.md)) has landed its end-to-end shared-Rust half on this doc's fork (§ Group-Forked Threads)** — the owner-signed policy, cryptographic role enforcement, the member-reported floor roster and history-for-joiners are no longer all-unbuilt as of 2026-09-08/09; what remains open (still unbuilt or partial) is tracked in that doc's § Implementation status today, not restated here, and nothing in this doc's own mechanics changes as the gaps close.

Two gaps in § Anti-Spam, both **declared, not
yet closed**. Neither is a licence to build on the described behaviour.

- **The `spam/behavioral` label has no consumer that changes what any user sees.** It is written on the *channel*
  (`content_type = 'channel'`), and the feed-filter rules that a `LabelBelow` consumer would ride all scope
  themselves to `content_type = 'post'` — so the filter consumer named in the original spam design can never match
  this label. Its one production reader today is the nest-wide label-count aggregate behind
  `fauna.moderation.stats`. Wiring a real reader means a recipient-facing trust-context surface on the
  conversations page in all 7 apps; until that lands, the scorer's output is observable to an admin count only.
  Consequence for the plaintext floor: [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)
  keeps the resolved `(sender, recipient)` pair in the clear *because the scorer needs it*, and that cost is
  currently being paid for a number no user-facing path reads.
- **The DM plane enforces the recipient's `inbox_mode` — same-nest since 2026-08-02, and the `closed` arm
  cross-nest since 2026-08-23** (§ Reach policy owns the ratified per-mode verdicts, scope, and refusal shape).
  The floor itself stayed mode-independent; the mode is applied as the Welcome plane's own routing after
  `Proceed`, via the shared `fauna_core::data::dm_initiation_mode_verdict`. The declared remainder is now
  narrower than it was: **`allow_knock`/`contacts_only` cross-nest** still keep the federation ingest's prior
  verdicts until the contact model learns home-nests, because those verdicts need a contact edge and the wire
  carries no signed sender. `closed` no longer waits on that slice — it needs no sender identity, and its
  cross-nest gate landed with the pin `welcome_deliver_closed_inbox_is_forbidden`. The relay leg (sender's nest)
  still does not consult the mode and never will: enforcement is the receiving nest's.

---

## FAQ

**Q: How many key packages should a client publish?**
A: There's no server-side limit; the shared session keeps the pool topped up to `KEYPACKAGE_TARGET = 20` (§ Key Package Management). Key packages expire after 30 days. If the one-time pool empties, the nest falls back to the actor's reusable last-resort key package (§ Key Package Fetch); only once neither a one-time nor a last-resort package is on file can no one initiate a DM to that user.

**Q: What's the default inbox mode?**
A: `allow_knock` — anyone can send a contact request (knock), but the recipient must accept before DMs flow. Other modes: `open` (anyone can DM), `contacts_only` (existing contacts only), `closed` (no incoming). Enforced on the same-nest DM plane since 2026-08-02, and `closed` is enforced cross-nest too since 2026-08-23 (§ Reach policy; the `allow_knock`/`contacts_only` cross-nest legs remain declared follow-ons there).

**Q: How long do unanswered knocks last?**
A: Pending knocks expire after 90 days. Accepted-but-unconfirmed contacts expire after 30 days.
