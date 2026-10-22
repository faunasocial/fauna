# Message reactions & delete — target state

Owns: message-reactions
Status: ratified — split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-10-16; reactions & message delete ratified 2026-06-23 and built in shared Rust and on all seven apps, the fuller picker ruled 2026-09-28; the per-app frontier rows stay on the page.
Authority: per-message reactions and message delete below the page — the `Reaction`/`Delete` channel-message variants and their capability flags, the manager actions and the ingest fold (the sender-match delete floor, out-of-order buffering, which reaction op wins), the per-bubble affordances and the "more" picker's two sanctioned families, the at-rest shape and durability across a relaunch, and the sender-authentication and leaf-credential bindings the delete floor rests on. Defers the element IDs, the § User actions rows, the per-app frontier table and every other page rule to [`../ui/conversations.md`](../ui/conversations.md); the owner/admin delete of another member's message to [`conversation-rooms.md`](conversation-rooms.md) § Roles and authorization; what the `history/<ch>` slice carries to [`devices.md`](devices.md) § Cross-device MLS group-state sync; the community class's signed-op rule to [`community-rooms.md`](community-rooms.md); the MLS channel protocol to [`direct-messages.md`](direct-messages.md); spam training to [`mail-spam.md`](mail-spam.md); reporting to [`moderation.md`](moderation.md).

> **Audience:** shared-Rust work in `fauna-mls` and `fauna-conversations` on the two variants, and each app's ⋯-menu, pill and picker glue.
> **Purpose:** how a member reacts to or deletes a message, and what every device keeps of it.

*Split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-10-16, when that page doc stood at 85 % of the whole-file read ceiling; the page's Authority line already named reactions & message delete as a concept of its own. The heading is unchanged, so a `§ Reactions & message delete` citation resolves here by changing only the file. A routing stub remains at the original location; prior history: `git log --follow docs/goal/ui/conversations.md`.*

*Reading this doc. Its text was carried verbatim, so an unqualified `§ <name>` citation may name a section that is not a heading here. § Reactions & message delete, *Manager + snapshot*, § Rendering / picker glue and *At rest* mean this doc's text. § Encryption at rest resolves in [`conversations-at-rest.md`](conversations-at-rest.md). Every other unqualified name — § State & data shape, § Architectural rules, § Implementation status today — resolves in [`../ui/conversations.md`](../ui/conversations.md).*

## Section map

- **§ Reactions & message delete** — the whole concept under its original heading: the wire, the capability, *Manager + snapshot*, the ingest, the affordances, *Rendering / picker glue*, *At rest*, the implementation status, *Durability across a relaunch*, the merge rule, *Sender authentication* (MLS-1), *Leaf-credential binding* (MLS-2), and what is deferred.

## Reactions & message delete

> **Spec:** (tracked internally, ratified with the user 2026-06-23). **FaunaMls-only today** via capability flags; mail/social light
> up when their backends + each rail's native semantics land.

Per-message **reactions** and **message delete** are the two per-bubble affordances on top of
send/reply/attachments. Both are **shared-Rust logic + thin app render glue with zero nest change**:
they ride the existing channel fan-out and the append-only `__conv` segment store as new
`ChannelMessageBody` variants sealed inside the MLS envelope — or, in a community room, inside the room's `RoomSealed` envelope, the sender its signed author ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization → *Delete any message — the mechanism* → *Community rooms* owns that class's two acts) — and the nest stays transparent (it stores
opaque envelopes and never opens them; § Encryption at rest). Capability-gated and observer-driven like
every other affordance (§ Architectural rules) — apps never branch on rail.

**Wire (`libs/fauna-mls`).** `ChannelMessageBody` gains `Reaction { target_seq, emoji, op }`
(`ReactionOp = Add | Remove`) and `Delete { target_seq }`. `target_seq` is the target message's nest
segment-store `seq` (the cross-member-stable identity behind `MessageId("conv:{channel}:{seq}")`). A
Reaction/Delete is not a user-visible message — the manager consumes it and renders no bubble (like
`GroupMeta`).

**Capability.** `supports_reactions` (already modelled, FaunaMls-only) gates reactions;
`supports_message_delete` (new, FaunaMls-only) gates delete. Other rails are `false` until their backends
implement the rail-native semantics.

**Manager + snapshot (`libs/fauna-conversations`).** `MessageSnapshot` carries the derived
`reactions: Vec<ReactionGroup>` (`{ emoji, count, reacted_by_me }`, ordered by first-added time),
`deleted: bool`, `is_own: bool`, and `can_delete: bool` — whether this viewer may delete the message, derived on every projection (§ State & data shape). Two actions route through
`ConversationsManager`:

- `toggle_reaction(thread_id, msg_id, emoji)` — resolves Add vs Remove against self's current state for
  that emoji (one emoji per user per message; multiple distinct emojis allowed), optimistically updates
  the aggregate, posts a `Reaction` channel message.
- `delete_message(thread_id, msg_id)` — the manager rejects a target whose snapshot does not say
  `can_delete` — posts a `Delete`, optimistically marks the target `deleted`. `can_delete` is the
  sender's own message everywhere; who else may delete in a governed room is the role table's
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization →
  *Delete any message — the mechanism*, which owns the rule and its build state).

**Ingest (`poll_inbound_conv`)** folds the variants into the derived state: a `Reaction` updates the
target's aggregate (any member may react); a `Delete` sets `deleted` **only if
`delete.sender == target.sender`** (a forged cross-sender delete is dropped — the security floor is
enforced on ingest, not just at the action). A `Reaction`/`Delete` arriving before its `target_seq` is
buffered and applied on the target's arrival. **Which reaction op wins a `(reactor, emoji)` pair is
the author's own signed stamp, not the log's order** — a community room's record can be re-appended
at a fresh `seq` by any member, so the fold reads the set of signed ops rather than the sequence;
rule and reasoning owned by [`../behavior/community-rooms.md`](../behavior/community-rooms.md)
§ The three classes → *Community* → *Who wrote it*. Display order is unaffected (below). The sender match is the as-built rule and stays the floor:
the owner/admin delete the room model promises
([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization) is a
second, role-checked admission beside it — owned there, with its build state — never a loosening of it.

**Affordances (ui.yaml).** A single `dm-message-actions-button[i]` ("⋯") per bubble opens
`dm-message-actions-menu` — reaction quick-set (`dm-reaction-option[i]`, fixed shared order
👍 ❤️ 😂 😮 😢 🙏) + `dm-reaction-more-button` (the fuller picker), `dm-message-delete-button` →
`dm-message-delete-confirm-button` (on a bubble whose snapshot says `can_delete` — *Manager +
snapshot* above; tui, linux, web, android and windows read it, macos and ios still read `is_own` until their lift), and `dm-message-mark-as-spam-button`
(spam-model training — behavior owner [`../behavior/mail-spam.md`](../behavior/mail-spam.md)
§ Wire shapes), and `dm-message-report-button` (the report verb — behavior owner
[`../behavior/moderation.md`](../behavior/moderation.md) § User-initiated reporting; IDs
user-approved 2026-09-25, unbuilt on every app). The ⋯ button is shown iff ≥1 action is available for that message
(keeps mail bubbles clean). Aggregated reactions render under the bubble as `dm-reaction-pill[i]`
(emoji + count, own highlighted), tap-to-toggle. A deleted message renders a localized "This message was
deleted" placeholder (body/attachments/reactions stripped). The inline `dm-reply-button` is untouched.

**Rendering / picker glue.** Uniform on all 7 apps (quick-set, pills, placeholder, ⋯ flyout). The
**only** sanctioned divergence is the "more" picker *widget* — the same class of app glue as the
attachment file-picker — and it comes in exactly two families, both of which reach **any** emoji
(the outcome the picker exists for): a **native chooser** where the platform has one a button can
raise (GTK `EmojiChooser` on linux, emoji2 `EmojiPickerView` on android), and a **free-entry emoji
field** everywhere else (tui, apple, web, windows) — a single-emoji input that commits on Enter and,
on a GUI app, also as soon as one complete emoji is in it, so an OS emoji panel (the macOS character
palette, the iOS emoji keyboard, Win+. on windows, a browser's own) picks and commits in one gesture,
and typing or pasting works with no panel at all. While the field is open it carries the
`dm-reaction-more-button` id itself (*entry mode* — the shape `attachment-button` already has on tui,
a button whose free-entry form is a committing input under the same id; ui.yaml's description
declares it, user-approved 2026-09-28). Web and windows keep their twenty-emoji **shortcut grid**
beside the field; its list lives once in shared Rust (`fauna_conversations::MORE_GRID_EMOJIS`, the
`quickset_emojis` shape over UniFFI and wasm) and its cells carry no id. A fixed grid *alone* is not
a fuller picker — it cannot reach an emoji outside itself, which is what web and windows shipped
before this ruling. The resulting "emoji string → `toggle_reaction`" is shared. The e2e seam is one
action for all seven (`react_with_custom_emoji`): click the button, type the emoji at the id (a
native chooser's driver emits the chooser's own pick; a field receives the text), and on tui click
once more for Enter; on apple the field commits by itself, so there is no second click. Ruled
2026-09-28; build state per app: outcome 5 of
`docs/features/reactions-and-message-delete.md` § Status (linux and tui green at the ruling, macos
and ios since, windows 2026-09-29; android's chooser-pick driver arm is built and unit-pinned, its
recorded run waiting on an android run venue; web 2026-10-08).

**At rest.** Reaction/Delete are normal sealed `__conv` envelopes — the same `Conversation messages`
encryption-at-rest row, **no new representation**. The aggregate and the deleted flag are derived —
but **not re-derivable**, so they ride the `history/<ch>` replica slice rather than being replayed: an
own `Reaction`/`Delete` is MLS-opaque to its own author off the log, and a restored device resumes past
every record it already folded, so a relaunch that replayed would lose every tombstone and pill it made
itself. What the slice carries, and why the fold alone is not enough, is owned by
[`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS group-state sync. The tombstone
**never hard-deletes** the target
envelope (No-user-data-loss is iron-clad; MLS can't recall a delivered message) — "delete for everyone"
is the standard cooperative marker compliant clients honor. The same holds for the target's attachment
blobs: a `Delete` is sealed and never a store tombstone, so the nest keeps pinning them for the record's
life (§ Encryption at rest → *Attachment reachability*).

**Implementation status.** Shared Rust + **all seven app render legs are
landed** (2026-06-24; § Implementation status today). Shared Rust:
`ChannelMessageBody::{Reaction,Delete}` + `ReactionOp` (`fauna-mls`);
`supports_message_delete`; `ReactionGroup` + `fold_reactions` +
`MessageSnapshot.{reactions,deleted,is_own}`; manager `toggle_reaction`
(optimistic) + sender-only `delete_message`; ingest via `poll_inbound_conv` →
`apply_inbound_{reaction,delete}` (forged-delete drop + out-of-order buffering,
validated at projection time); UniFFI + wasm exports. Every app renders the
same shape (⋯ menu, quick-set, pills, tombstone) capability-gated off the
snapshot; the only sanctioned divergence is the "more" picker widget
(§ Rendering / picker glue). tier_3 `test_conversations_reactions.py`
(incl. the cross-engine `test_react_cross_member_peer_sees_pill`) +
`test_conversations_message_delete.py` are green per app (android
host-emulator-gated; Robolectric covers its render). One e2e gotcha worth
keeping: own-message seeding is per-app — every mock-backend app uses the
compose-send echo, but **linux** (real FaunaMls backend at login) seeds via
`ConversationsManager::inject_own_for_test`.

**Durability across a relaunch (2026-09-20).** Both derived states now ride the `history/<ch>` replica
slice, closing the gap this doc's *At rest* paragraph described the wrong way round for three months
(it claimed stream replay rebuilds them; nothing could). `ConversationsManager::snapshot_channel_slice`
— the one door both slice writers take — projects the same fold `thread_detail` serves the apps
(`project_reactions_and_deletes`, one function, two callers) onto the slice's messages and records the
raw state beside it: `deleted_messages` and the per-message reaction log (`reaction_log`, stamped), both additive and
omitted when empty, so a slice with neither is byte-identical to one written before the fields and an
older client still renders the folded form. `restore_channel_slice` re-seeds the manager's `deleted`
set (union — a tombstone is monotone) and its reaction log (fill-never-overwrite — what this device
logged itself outranks another device's copy). `merge_history_slices` unions the tombstone rather than
taking the higher-watermark side's copy, since an own delete is opaque to its author and the deleting
device need not be the furthest-polled one. Pinned by
`a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore`
(`manager_integration_tests.rs` — own delete, an admin's cross-sender delete, the pills, and a reaction
toggled *after* the restore, which is the leg the folded aggregate alone cannot pass) and the
`store::history` merge tests. Contents authority: [`../behavior/devices.md`](../behavior/devices.md)
§ Cross-device MLS group-state sync.

**A merge no longer costs a device its own reactions (2026-09-22).** Two devices that each reacted on
a message while their replicas were forked now BOTH keep their pill through the slice merge, where
before the further-polled side's log won whole — an own reaction is MLS-opaque to its author, so the
reacting device need not be the further-polled one. The rule, and the one case it still cannot cover,
are owned by [`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS group-state sync.

**Sender authentication (2026-06-27, MLS-1).** The delete floor above ("a `Delete` sets `deleted` **only if
`delete.sender == target.sender`**") and every sender-attributed bubble/reaction rest on
`ChannelMessage.sender` being trustworthy. That anchor is enforced in shared Rust at
`fauna-mls::engine::decrypt`: after MLS `process_message` authenticates the sending leaf, `decrypt`
**overwrites** the returned `sender` with that authenticated leaf's `ActorId`, so the self-asserted value
inside the encrypted payload is never trusted. The rule is simply **the authenticated leaf is the sender**; the decrypt-side bind holds it for all seven
apps at once (no wire/struct change). The CalDAV scheduling rail is unaffected — its receive path re-authenticates via the iMIP `ORGANIZER` and never reads `cm.sender`.

**Leaf-credential binding (2026-06-27, MLS-2).** The MLS-1 bind trusts the *authenticated leaf credential*
as the sender. MLS authenticates only the leaf *signature*; the leaf `BasicCredential` (the 32-byte
`ActorId`) is opaque to MLS, so the protocol itself does not tie the identity a leaf *names* to the key it
*signs with* — that bind is ours to make, one layer below MLS-1, and every consumer of the credential (the
sender bind above, the roster) rests on it. Because a Fauna `ActorId` *is* the
Ed25519 public key (`fauna-core::identity`) and the MLS leaf signer *is* that key
(`build_credential_and_signer`), an honest leaf always satisfies `credential == leaf signature key` (the
same 32 bytes). Shared Rust now **enforces that equality for every leaf admitted to a group**, with a typed
rejection on a mismatch: at `create_group`/`add_member` (invited KeyPackages), in `process_commit` (every
leaf a commit introduces or updates — Add/Update proposals + the committer's update-path leaf, validated
before the merge), and on `join_from_welcome` (the whole learned roster, catching a malicious inviter whose
own leaf is forged). The nest adds a defense-in-depth check at the `keypackage.upload` choke point
(`fauna-mls::engine::verify_uploaded_key_package`): an uploaded KeyPackage is rejected unless its inner
credential == its leaf signature key == the authenticated uploader, so the nest never stores or serves a
forged-identity KeyPackage. One 32-byte equality protects every credential consumer at once — the sender
bind, `group_members()`/roster, and `find_leaf_by_identity` — for all seven apps with no wire/struct change.
One consumer up, the private contact overlay's paint gate rests on the same roster: a nickname paints on a
member chip only once the participant's actor id is a verified leaf of the thread's group
([`contacts.md`](../ui/contacts.md) § The private overlay → *The paint gate* owns that rule).
Reference: tracked internally (design review, Part 2).

**Deferred (captured in the spec):** delete-for-me (local hide, needs a synced per-user hidden-set);
reactions/delete on the mail + social rails (each rail's native semantics behind the same capability
gates). **No longer deferred (2026-09-19):** an owner's or admin's delete of another member's message —
listed here as deferred on 2026-06-23, then promised by the room model ratified 2026-09-08; it is
[`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization's to
own, and that doc's § Implementation status today declares what is built.
