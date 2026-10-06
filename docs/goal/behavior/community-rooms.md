# Community rooms — target state

Owns: community-rooms
Status: ratified — split verbatim out of [`conversation-rooms.md`](conversation-rooms.md) on 2026-09-10; the class was ratified there on 2026-09-08 with the room model (TP8, decision 1 — [`../architecture/third-party.md`](../architecture/third-party.md) § The rulings — TP8). Its key model's refutable window closed unrefuted at the first community-room build (2026-09-09), and the read's purposes' and the attachment kind's at the first label builds (2026-09-10).
Authority: **the community class** — the room class whose member set includes its home nest: why the class exists (the room model's decision 1), its key model (the recipient-set scheme — ratified decision R15, [`../architecture/account-data-plane.md`](../architecture/account-data-plane.md) § The ratified decisions — with the home nest as one reader recipient, never in the authority set), who wrote a message (the author's signature, bound by every reader to the floor roster), the attachment content kind, and what the home nest does with its read — the purposes (search, labels, room-restricted posts), what the read covers, what is forbidden, and the search door's where-never-what rule — together with the class's build record: the birth ceremony, the floor-authoritative doors as this class runs them, the sealing, the derived views and their revoke, and the client half. Defers the room model the class is one class of — the class rule and table, principals, the floor roster, roles and how each class enforces them, join rules and invites (the invitation's lifecycle is [`room-invitations.md`](room-invitations.md)'s), the home nest and its transfer, history for joiners, the group plane's fate — to [`conversation-rooms.md`](conversation-rooms.md); the recipient-set scheme's mechanics to [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md) § The recipient-set scheme; the room-read keypair and the per-kind content keys to [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md); a scoring purpose's placement to [`../architecture/content-scoring.md`](../architecture/content-scoring.md) § The placement matrix; the labeler input ABI to [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) § Tier-3; the room-restricted post's shape and seal to [`restricted-posts.md`](restricted-posts.md) § Encryption at rest; and the class statement's copy to [`../ui/conversations.md`](../ui/conversations.md) § Element IDs.

Last verified: 2026-09-26 (two passes — the second: the cross-nest community invite BUILT — § Implementation status today, the *What is still unbuilt* entry; the two *latent* holes now reachable and closed, *A removal severs*, *leaving does sever*; the first: every glue site registers the four nest-backed room seams as one bundle, and web, which has no account plane, founds and joins nothing — § Implementation status today, the founding entry); 2026-09-21 (the class's records are replayable, and an order-dependent fold must read the set — § The three classes → *Community* → *Who wrote it*, the reaction fold's stamp rule); 2026-09-14 (a stored reception key is FIPS-203-valid before it is ever stored — § Implementation status today → *A reception key is FIPS-203-valid before it is ever stored*, row 749); 2026-09-13 (the wrap-target door and the tend pass's own-seat heal — § Implementation status today → *A seat gains or rotates its wrap target*; the reach of the nest-read revoke — § Implementation status today, the sealing entry → *How far the revoke reaches*; the search door's hits bounded by the caller's own wraps, 2026-09-11 — § The three classes → *The door answers where, never what*, its second half; a foreign member's cross-nest delivery is pull-only, traced end to end and closed as no gap — § Implementation status today → *A foreign member of a community room learns of a new message only by polling*; every other build date, pin count and gap below is its entry's own, carried across the 2026-09-10 split unedited) | Sources: `libs/fauna-mls/src/{room_policy.rs,room_message.rs,wrapped_blob/{generation_wraps.rs,group_generation_wraps.rs}}`, `libs/fauna-pq-kem/src/lib.rs`, `libs/fauna-core/src/{group_content.rs,group_generation.rs}`, `libs/fauna-conversations/src/{backend.rs,session.rs,backends/fauna_mls.rs}`, `bins/fauna-nest/src/{conversations_handlers.rs,federation_handlers.rs,federation_pool.rs,room_read_key.rs,db/{rooms.rs,channels.rs}}`

> **Audience:** the nest (the floor doors, the sealing's admission, the derived views and their revoke) and every app (founding, joining and keying a room, the client half's read and send) — anyone touching a room whose member set includes its home nest.
> **Purpose:** what the one nest-readable room class is and why it exists, how its content is keyed and attributed, and exactly what its home nest may do with the read its members grant.

*Split verbatim out of [`conversation-rooms.md`](conversation-rooms.md) on 2026-09-10. That doc's `Owns:` line already named two concepts — `room-model` and `community-rooms` — and the second had become nearly all of the doc's growth: the class's build record and its part of § The three classes took the doc from 39,372 B at ratification on 2026-09-08 to 132,973 B two days later, a pace that would have put the whole room model past the whole-file read ceiling within days. The room model stays there with its own status entries, and a routing stub remains at each original location; prior history: `git log --follow docs/goal/behavior/conversation-rooms.md`.*

*Reading this doc. Its text was carried verbatim, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Two headings exist in both docs. An unqualified `§ Implementation status today` means* this *doc's build record — every such citation in the moved text is about the community class. `§ The three classes` names the section the class's own part was carried out of: its rule, table and derivation stayed in [`conversation-rooms.md`](conversation-rooms.md) § The three classes, while `→ *Community*` and every lead-in beneath it are here. Every other unqualified name — § The room, § The floor roster, § Roles and authorization, § Join rules and invites, § The home nest, § History for joiners, § Bridged rooms, § The group plane's fate, § Architectural rules, § Don't do these, § Done definition — resolves in [`conversation-rooms.md`](conversation-rooms.md). Every `above`/`below` in the moved text resolves inside this doc: the moved blocks keep their original relative order.*

## Section map

| Section | What it holds |
|---|---|
| § Implementation status today | The community class's build record, moved verbatim and in its original order: the birth ceremony and the floor-authoritative doors (membership, governance, the floor-verified send), the sealing and the home nest's read and revoke, the search door and the community-as-audience post pass, labels and their attachment-bytes facet, attribution, the client half's read and send, founding and joining from an app, the invitation's delivery, severance by rotation, the policy doors and the header's class, the manager's app-layer cycle, the add's backfill and the class's unbuilt residue, the roster read's wrap targets, the attachment key, and — moved in 2026-09-28 — the class's half of the *Delete any message* entry (the floor delete, the anchor, the succession witness and residuals *(a)*–*(h)*). The room model's own entries — the report door, the end-to-end class, the home nest's attachment bytes, the succession axis — stayed in [`conversation-rooms.md`](conversation-rooms.md). |
| § The three classes | The Community row's own detail, moved verbatim: why the class exists, its key model and the four reasons, who wrote a message, the attachment content kind, what the home nest does with its read (the purposes, what was struck, what the read covers, what is forbidden) and the search door's where-never-what rule. The class rule, the table and the derivation stayed in [`conversation-rooms.md`](conversation-rooms.md) § The three classes. |

## Implementation status today

The community class's entries, carried verbatim out of [`conversation-rooms.md`](conversation-rooms.md) § Implementation status today on 2026-09-10 in their original order; that section keeps the room model's head and its other entries.

- **A community room can be founded, and does nothing yet (2026-09-09).**
  `fauna.conversations.room.create` is the class's **birth ceremony**: the
  creator sends a 32-byte salt and its own signed initial policy, the nest
  re-derives `room_id` from that birth record
  (`fauna_mls::room_policy::derive_room_id`, so the id commits to whose key
  founded the room), and seats two principals — the creator as the one
  `owner`, and this nest as a `member` of kind `nest`. The stored class is
  then the ordered derivation over those kinds and lands on `community` with
  no class word ever coming off the wire; the request carries no class, no
  home and no member list, because all three are consequences (§ Don't do
  these — the home nest is a *member*, never a readable-by-nest flag; § The
  home nest — a room is born on its creating member's nest). `room.list_roster`
  reads the floor back to a live member. The birth record is stored
  (`rooms.birth_salt`, schema 53) so it stays re-verifiable, and its presence
  is what makes the room floor-authoritative.
  **The membership half lands with it (2026-09-09):** `room.invite`,
  `room.accept_invite`, `room.remove` and `room.leave` write the floor as the
  authority, applying the roles table the end-to-end class enforces in
  `judge_commit` — owner and admins invite and remove, a plain member invites
  only under `member-invite`, the owner is not removable and cannot leave
  until ownership is transferred. **An invite does not seat a member;
  acceptance does** (§ Join rules and invites) — deliberately, unlike the
  retired group plane's `group.invite`, which seated the member outright;
  the invitation is the inviter's signed act
  (`fauna_mls::room_policy::SignedRoomInvite`), the nest binds its signer to
  the authenticated caller, and the invitee's reach policy gates it through
  the *same* helper the group Welcome uses, so "the group-Welcome gate
  applies unchanged" is literal rather than a copy. A departure clears the
  settled invitation with it, so a removed member cannot re-seat itself by
  replaying its accept.
  **The governance half lands with it too (2026-09-09):** `room.set_policy`
  applies rule 6 exactly — an **admin-set change needs the owner's
  signature**, name/join-rule/history-policy take an owner's or an admin's,
  and the version is a strict ratchet at exactly `stored + 1` so a replayed
  older policy can never be installed over a newer one. Storing a policy
  **reconciles the floor roster's roles to it**: the signed policy is the
  authority members render and `room_members.role` is the projection every
  nest-side gate reads, so letting the two drift would let the nest enforce a
  rank the policy does not grant. The same reasoning bounds invitations — an
  **admin invitation is admissible only when the current policy already names
  the invitee**, because the nest cannot author a policy, so admin
  appointment stays one owner-signed act rather than two doors that can
  disagree. `room.transfer_ownership` is its own door because it moves the
  roster row as well as the bytes: it is signed by the *outgoing* owner
  (whose role in the previous version is what lets it change the owner
  field), the incoming owner must already be a live **user** member — a room
  is never owned by the home nest that reads it, and an owner-less room is
  unrepresentable — and the outgoing owner takes the rank the new policy
  gives them. **An identity succession hands a seat on (2026-09-13):** the
  ceremony seats the successor at the predecessor's rank and absorbs the
  predecessor's row, and the floor resolves the policy's names through the
  succession chain — recorded in [`conversation-rooms.md`](conversation-rooms.md)
  § Implementation status today, the succession-axis bullet; the owner's
  account-deletion rule is that doc's § Roles and authorization.
  **The send path is floor-verified (2026-09-09).** "The home nest refuses a
  send … before storing anything" is enforced on `channel.send` for a
  ceremony-born room: a live floor member of any rank may send (the roles
  table gives send to all three), and anyone else is refused before the
  envelope is stored. It fires only for a floor-authoritative room — an
  end-to-end room's send stays gated on the routing roster alone, because its
  membership is its MLS group's and the reported floor is a mirror, not a
  send authority. What that closes: the routing roster self-registers on
  first send, so without it any actor that learned the 32-byte channel id
  could append to a community room's log — and unlike the end-to-end case
  those are bytes the home nest holds a wrap for and will fan out and index.
  **The read is deliberately not floor-gated**: § The floor roster's
  enforcement list is send, invite, remove, delete and policy change, and a
  community room's stored bytes are sealed, so serving them to a
  routing-roster caller reveals nothing the class does not already accept.
  **The sealing lands (2026-09-09, schema 57) — and the ratified key model
  survives its first build.** A community room's log now rests sealed under
  a **room generation key** minted by an owner or admin *device* and
  X-Wing-wrapped to every live floor principal — the home nest among them,
  under the room-read keypair — through
  `fauna_mls::wrapped_blob::group_generation_wraps`, the recipient-set
  scheme's own doors, reused rather than re-implemented. Two kinds carry it:
  `fauna.conversations.room.publish_generation` (the members' keying act,
  which the nest *admits* and never performs) and
  `fauna.conversations.room.generations` (a member reads back **its own
  wraps only**, so the door cannot enumerate a room's key material). The
  message body seals under a per-kind content key derived from the
  generation ([`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
  § Audience: a storage group → *Per-kind content keys*, built the same day)
  and rides a third `ChannelEnvelope` variant, `RoomSealed`, naming its
  generation in cleartext so a reader knows which wrap to open. **Admission
  is three checks and no more:** the minter is the authenticated caller and
  holds `owner` or `admin` on the floor; the mint names the room's current
  tip as its parent (a strict ratchet — this plane has no arbiter to resolve
  a fork); and **roster coverage**, the scheme's own admissibility rule,
  every live floor principal with a wrap target having a wrap.
  **The tip is the end of the parent chain, never the latest stamp**
  (corrected 2026-09-10): a mint's `minted_at_ms` is the minting device's
  wall clock and its own type calls it advisory, so ordering by it let two
  admins with skewed clocks order the room by their skew — a revoke stamped
  earlier than its parent never became the tip, and a stale sibling was
  then admitted as a second child. The nest walks the chain
  (`db::rooms::order_generations_by_chain`; a fork left from before the fix
  follows its deepest branch, ties by lowest id, and is logged), and the
  publish **re-checks the ratchet under the lock its insert holds**, so two
  concurrent rotations cannot both land as children of one tip.
  ⚠ **The home nest is deliberately outside coverage** (ruled at the build,
  after the conformance suite caught the two rules contradicting each other).
  It is a live floor member, so requiring a wrap for it would make
  "revocable by rotating it out" unexpressible and the grant irrevocable in
  practice. The asymmetry is the class's own: coverage exists so that nobody
  the room says is a **member** holds ciphertext they cannot open, and a
  member who cannot read is broken — whereas the home nest is a *reader
  recipient, never in the authority set* (§ Don't do these), so a nest that
  cannot read is not broken but revoked. Never silently: the reply says so
  and the views go in the same act.
  **The home nest's read, and its revoke.** The nest opens the *tip's* wrap
  and builds one derived view — the room's **search index** over the sealed
  log, under a per-room FTS class. A mint whose wrap set omits the nest's
  entry is the members withdrawing the materialization grant: the nest
  reports `nest_read_revoked`, deletes that room's views **in the same act**,
  and derives nothing further — including from a straggler sealed under an
  older generation it still holds a wrap for, because the read is keyed on
  the tip and not on the envelope — the chain's tip, which is what makes the
  revoke hold even when its mint carries an earlier stamp than the
  generation it replaces
  (`a_revoke_stamped_before_its_parent_still_ends_the_nests_read`). Removing the home nest from the *floor*
  stays refused, and now for a reason rather than a gap (ruled here): class
  is a function of the member set (§ Architectural rules, rule 1), so
  unseating it would derive the room back to `end_to_end` while its log
  stayed sealed under the recipient-set scheme — a room whose stored class
  names a key model it does not use. Rotation revokes without touching the
  member set, which is exactly what "revocable by rotating it out" asks for.
  **How far the revoke reaches (ruled 2026-09-13).** The
  withdrawal is the materialization grant's own revoke — reason 1 of the key
  model says the nest's readable position is "exactly the shape of the
  materialization grant" — and it inherits that grant's ratified honest bound
  ([`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)
  § Capability tiering, the standing *trust-until-revoke* grant: scope and
  unforgeability are cryptographic, expiry and revocation bind the holder).
  So the revoke **binds an honest home nest**, and is **not a cryptographic
  revocation against a home nest that misreports its own floor**. Three
  properties hold, and only these: (a) an honest nest's read ends at the
  revoke and its views go in the same act; (b) no honest device re-grants it
  unasked — an ordinary rotation keeps the withdrawal while the roster
  records it (`NestRead::Keep`, the entry below), and the automatic key-in
  wraps only to `user` rows; (c) no single admin restores it through any door
  but the toggle — the backfill refusal below. It cannot reach further,
  because in this class the floor is the nest's to write
  ([`conversation-rooms.md`](conversation-rooms.md) § The floor roster):
  every input to a member's mint but the signed policy is the nest's word —
  which rows are members, the reception key each wraps to, their roles, the
  `tip_wrapped` answer `Keep` reads, and the principal kinds the **class
  label** is derived from ([`conversation-rooms.md`](conversation-rooms.md)
  § The three classes, which owns what that label may be trusted for) — and a
  nest willing to lie needs no
  lie about its own row, since it can seat a `user` row carrying a key it
  holds and be keyed in as a member. Carrying the members' choice in the
  signed policy would move one bit off the nest's word and leave the rest
  there, at the price of a policy version coupled to every rotation and a
  rollback pin on every device; it would claim a strength the class cannot
  deliver, and is refused here. A member who wants confidentiality *from* the
  home nest is asking for the end-to-end class, and a home nest the members
  no longer trust is not rotated out of a community room but left behind — a
  new room, in another class or on another nest (class is a function of the
  member set, § Don't do these). Two consequences the code carries: a
  `tip_wrapped` answer of `null` on the nest's own row keeps the status quo —
  wrap — which is a never-keyed room's founding grant and neither a revoke
  nor a re-grant signal; and `set_room_nest_read` is not rank-gated on the
  device — the nest's mint door ("only an owner or admin mints a room
  generation") is what refuses a plain member's Save, reaching the editor as
  the nest's own refusal.
  **A re-admitted member returns on a fresh roster entry** (the entry id
  derives from the *seating* stamp), so the wraps of the generations it was
  severed from stay bound to a slot it no longer holds.
  **The key model's refutable window (§ The three classes → *Community*)
  closes here, unrefuted.** All four ratified reasons held at the build: the
  nest is literally one more recipient of the same kind (reason 1 — it is
  seated with its own `room_read_pubkey`, never one a founder named); no
  nest-resident MLS engine or identity-class credential was needed
  (reason 2); the log rests sealed on every nest with no second at-rest
  carve-out (reason 3); and removal-rotation and admission-backfill were
  the scheme's existing moves rather than new ones (reason 4). What the
  build *added* to the design is one ruling, above: the revoke is a
  rotation, not a roster removal.
- **The search index becomes readable — the first purpose of the read served
  end to end (2026-09-10, schema 58).**
  `fauna.conversations.room.search` is the door: a live floor member of any
  rank searches the room, and the hits name **log positions and ranks, never
  text** (the rule above) — and, since the wrap bound below, only positions
  that member could open. Until it landed, the nest had indexed every
  community-room message it could open since the sealing build and *nothing
  could ask the corpus a question* — the derived view existed and served
  nobody, which is why "only search is built" understated the gap rather than
  overstating it.
  **What was actually missing was the map, not the query.** An FTS row is
  keyed by a one-way hash of `(schema, seq)`, so a hit could not name the
  message it came from; `room_message_views` (schema 58) is the
  `(room, seq) → doc key` map the indexer now writes beside every indexed
  message. It is itself a derived view — it says which positions of the log
  the nest was able to read — so `purge_room_derived_views` deletes it in the
  **same act** as the FTS rows, and the revoke stays one statement about one
  room. Pinned by five cases in
  `bins/fauna-nest/tests/conformance_conversation_rooms.rs` (75 today): a hit
  names its message, the encoded reply contains no plaintext of the message
  it matched, a non-member is refused rather than served an empty list, and a
  mint that rotates the nest out leaves the search answering empty — a real
  answer, not an error, because withdrawing the grant is a member's own act.
  **The community-as-audience post pass lands too (2026-09-10; purpose 3).**
  A room-restricted post this nest stores — created here, or relayed here —
  and addressed to a community room it homes is opened under the tip's wrap
  in the act that stores it and indexed into the room's per-room **post**
  class beside its message class, with a `(room, post id)` map
  (`room_post_views`, schema 61) that the revoke deletes with everything
  else; the search door names such a post by id to a caller that asks for
  posts. Mechanics owned by `restricted-posts.md` § Encryption at rest →
  *Room-restricted — the ruling*. **Labelled too (later the same day):** the
  label pass (the next bullet) runs in `index_room_post` exactly as in
  `index_room_message` — the same set, the same shared scorers
  (`run_room_labelers`, one home) — with the rows keyed `(room, post)` under
  a `room_post` kind; and because a post is read through `fauna.posts.get`,
  the feed pages and the deep-link door by followers who need not be
  members, its verdicts are served through a floor-gated read of their own,
  `fauna.posts.room_labels`, and never on any envelope read (the mechanics
  and the pins: `../ui/feed.md`, *Built* detail (v)). **The attachment facet
  reaches a post too (2026-09-10):** a room post's
  media seal under the post's own per-post key rather than as room
  attachments, so the pass opens them with the key it already opened the body
  with and hands a declaring labeler the same facet a message's picture rides
  — one bounding loop for both key models
  (`fauna_labeler::build_attachment_facet`: the declared-size pre-filter, the
  sealed-size allowance, and `AttachmentFacetBudget`'s admission by opened
  length), so the two positions cannot drift. Pinned by
  `a_room_labeler_that_asks_for_bytes_labels_a_room_posts_picture`
  (red-verified with the facet withheld) beside the message case it mirrors.
  **A text-less post is viewed and labelled too (later still, 2026-09-10).** The pass had indexed only a `Text` or
  `TextWithMedia` body's content and returned before the map row and the
  label step for anything else, so a picture-only post was unlabelled where a
  picture-only *message* is labelled. Ruled: the `(room, post)` map row
  records that the home nest derived a view of the post for the room — *any*
  view — and is the precondition of every derived view of a post, not a
  receipt for its search row; so it is written for every post that passes the
  pass's gates, under the deterministic document key whether or not a
  document was indexed (a pure function of the ids, so nothing changes at
  rest, and a purge of a key with no row removes nothing), and the invariant
  the two consumers need — every post search row and every `room_post`
  verdict has a map row — is kept by the order index-if-text → map → labels.
  "The map is a subset of what is searchable" is the message map's own (its
  verdicts key on `(room, seq)` and never route through it) and was never
  what a post consumer relied on: a hit comes only from a search row, so a
  map row with none is a row the search door never asks about. Typed text is
  `PostBody::text`'s — a caption, a media post's alt text, a structured post's
  content; a video's is empty — indexed when non-empty, as the plaintext post
  rail already reads it; media is `PostBody::media_items`'s, so a `Media` or
  `Structured` body's picture rides the attachment facet as a captioned one's
  does, and a `Video` body, naming no media item, reaches the labelers with
  its metadata alone, as everywhere. Pinned by
  `a_room_labeler_that_asks_for_bytes_labels_a_media_only_room_post_by_its_picture`
  (no alt text: viewed, no hit, labelled, purged with the post through the
  map row — red-verified against the early return) and
  `a_media_only_room_posts_alt_text_is_the_text_the_member_typed`.
  **The hits become wrap-bounded (2026-09-11, schema 63).** The door had gated on live floor
  membership alone and answered over the room's **whole** corpus, so a member
  seated under `none` — every room's default — was served the positions and
  ranks of messages sealed before its admission, under generations it holds
  no wrap for: the *what* oracle the second half of the rule above now
  forbids, iterable one term per call. Fixed where it belongs, at the map:
  the reception pass records **which generation it indexed under** beside
  every map row (additive `generation_id` on `room_message_views` and
  `room_post_views`), the map lookup joins that against
  `room_generation_wraps` for the caller's own roster entry, and the door
  never holds a position it may not serve. Two consequences worth stating:
  the page **over-fetches** (`wrap_bounded_page`) because the filter drops
  hits after FTS has applied its own limit, so a filtered page would
  otherwise come back short and read as "nothing more" — it walks the ranked
  corpus up to a bounded scan instead; and a map row written before the
  column — a NULL generation — is **fail-closed**, served to nobody, since a
  row that cannot say which key it needs must not be guessed at (the rows are
  derived, and the pass rewrites them). Pinned by three cases that only work
  together — `a_hit_is_served_only_for_a_position_the_caller_could_open` (the
  newcomer finds nothing before its admission and everything after; the
  founder finds both), `a_backfilled_member_finds_the_history_it_was_given`
  (the `full` arm, and why the bound is wrap coverage and not admission time)
  and `a_post_hit_is_served_only_for_a_post_the_caller_could_open` — each
  red-verified against the unfiltered door, which served the withheld
  position. The entry above's five cases are eight.
  **Still unbuilt of the read (its purposes ratified 2026-09-10 — § The three
  classes → *What the home nest does with its read*):** nothing of the
  purposes — search, labels (the next bullet), the post pass and its labels
  all landed that day, and "the unbounded fan-out" was struck from the
  purposes, delivery never having needed the read — only the client half,
  the six-app trickle-down that paints it and authors room posts.
- **Labels land — the read's second purpose, served end to end (2026-09-10,
  schema 60).** A room names its labelers in a **signed record of its own**,
  `fauna_mls::room_policy::SignedRoomLabelers` `{room, version, labelers}`,
  set through `fauna.conversations.room.set_labelers` and served back on the
  roster read beside the policy (`RoomListRosterReply.labelers`). A sibling
  rather than a `RoomPolicy` field was the slice's one open choice, and the
  compat rule decided it: a policy verifies over the canonical re-encode of
  what its verifier decoded, so a field an older app does not know would fail
  that app's verification of every policy carrying it, and an older app
  authoring the next `set_policy` from its own decode would drop the field
  without knowing. The door reads rule 6 as "the rest": the owner or an admin
  signs, the version ratchets from 1, and every id must be a `wasm` or
  `text-model` artifact this nest's labeler registry publishes — at most
  `MAX_ROOM_LABELERS` (4), because every named labeler runs on every send.
  **The pass runs in `index_room_message`**, the act that already opened the
  message, after search and before the fan-out — so the timing rule holds by
  construction — and only the shared scorers run it: a `wasm` labeler through
  `fauna_labeler::run_published_labeler` (the one execution boundary, lifted
  out of the FFI holder, which now calls it too), a `text-model` through
  `fauna_text_model::publish::PublishedTextModel::damped_score`, the number a
  subscriber's client computes. Every labeler that runs writes one tier-3
  `labeler:<id>` row to `content_scores`; a `wasm` labeler's verdicts in the
  canonical five also land in `content_labels` (an off-list category still
  counts toward its labeler's score and never reaches the category plane; a
  `text-model` names no category, so it lands on the factor plane only). The
  rows key on the message's `(room, seq)` — deliberately not a hash, so the
  revoke finds them from the room alone — under the `room_message` kind, which
  keeps them out of the List plane's withdrawals, every owner-scoped re-score
  worklist, and the deployment-wide `fauna.moderation.stats`. **Served beside
  the envelope on `channel.fetch`** (`ChannelFetchEntry.labels` / `.scores`,
  wire-additive) **and only to a live floor member**: the envelope stays
  un-floor-gated because it is sealed, while a verdict was derived from the
  plaintext. The shared manager merges the served verdicts into the message's
  own by the moderation queue's superset rule
  (`fauna_core::content_category::merge_server_labels`), so every app's
  existing badge paints them with no app change. **Purged in the same act as
  the search index**; the set itself stays, being the room's choice rather than
  something the nest read. Pinned in `bins/fauna-nest/tests/conformance_conversation_rooms.rs`
  (92 today): a room naming both kinds labels a message at send and a member's
  read carries both planes with no plaintext in its bytes; a room naming none
  derives nothing; a set naming a `list`, an unpublished id, or signed by a
  plain member is refused; a mint that rotates the nest out leaves the label
  rows at zero, counted in the store rather than through a read
  (red-verified: the purge with its label half removed fails that case while
  the search-revoke cases stay green); a stranger and a removed member read the
  envelopes and no verdicts (red-verified against the floor gate removed) — and
  in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`
  (`a_community_messages_server_labels_merge_into_the_bubbles_own`,
  red-verified against the merge disabled). **The purposes' refutable window
  closes here, unrefuted:** the producer, the placement, the room-chosen
  transparent set, metadata-only output, the purge and the timing all held;
  what the build added is the one mechanism choice above.
  **The bytes half lands (2026-09-10):** a labeler
  that declares `needs_attachment_bytes` — the `label()` ABI's fifth signed
  input declaration, owned with its shape, compat rule and ceilings by
  [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md)
  § Tier-3 → *The attachment facet* — receives each attachment's **opened
  plaintext** in this same act (`conversations_handlers.rs::open_room_attachment_facet`,
  under the tip's generation, the second per-kind content key), bounded by
  that owner's constants and dropped with the pass; a labeler that does not
  declare it reads exactly what it did before. **The attachment kind's
  refutable window closes here, unrefuted** — the seal, the key derivation,
  the blob's placement on the home nest and the reference in the message all
  held for a read that opens the bytes (red-verified: the conformance case
  `a_room_labeler_that_asks_for_bytes_labels_an_attachment_only_message_by_them`
  fails with the facet withheld, and the unflagged fixture beside it stays
  blind to the same picture). **The relayed read carries them too
  (2026-09-10):** a member homed on another nest
  drains the room through its own nest, which relays `channel.fetch` as
  `fauna.federation.channel.fetch` (§ The home nest), and the home nest's reply
  carries the same two planes beside each envelope — filled by the one body
  both doors call (`conversations_handlers::page_verdicts`): the floor gate
  keyed on the actor the relay names, which `require_foreign_member` has bound
  to the calling nest, never on that nest, and nothing beside a withheld
  envelope. The relaying nest forwards them onto its member's
  `ChannelFetchEntry` and stores none (the wire, its skew and the relay's
  duty are [`../architecture/federation.md`](../architecture/federation.md)'s
  `channel.fetch` row), so the shared manager's merge paints them with no app
  change. Pinned in `bins/fauna-nest/tests/conformance_conversation_rooms.rs`
  — a seated foreign member's relayed read carries both planes, equal to a
  home member's, with no plaintext in its bytes; a bound actor never seated,
  and one removed from the floor, drain the envelopes and no verdicts; a
  taken-down message's verdicts stay behind on both doors — and in
  `bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs`
  (`a_foreign_members_relayed_read_carries_the_community_rooms_verdicts`: two
  real nests, the verdict crossing both hops into the client seam the bubble
  is painted from, an unseated bound actor reading none, the relaying nest
  keeping no row). Each was red against the reply that carried no verdicts,
  and the negatives are red-verified against the shared body mutated: with
  the floor gate bypassed the unseated cases fail in both files, and with the
  withheld-envelope rule dropped the takedown case fails. **Not built:**
  a message already on the device when its verdict lands (the sender's own
  echo) keeps the device's labels; a new set applies to messages sent after it
  lands, and nothing re-labels the backlog; and the set's editor on six of the
  seven apps (tui paints it — the next bullet's last paragraph). **The class statement that must name the purposes is BUILT**
  : `room_class_community` (`i18n/strings/en.yaml`,
  `RoomClass::label`) is now "Community — searched and labelled by the home
  nest" on both `thread-room-class` and `recipient-picker-class`, all 7 apps,
  the consent-surface sentence's own wording unchanged.
  **The editor's shared half lands the same day.** `RoomSnapshot.labelers`
  carries the set a floor read serves, **verified** — its signature, and that
  it names this room, so a set an admin of two rooms signed for the other one
  never renders here; `Some(empty)` for a community room naming none, `None`
  where nothing can be staged. The record binds no policy version, so the
  signer's rank at signing is the nest's to have checked, and **a set stays in
  force after its signer loses the rank** — as a policy version does — so it
  renders as in force until an owner or admin replaces it. `RoomSettingsDraft`
  stages it (`toggle_labeler`, bounded at `MAX_ROOM_LABELERS`) and Save commits
  it as one `RoomSettingsEdit::Labelers`, ordered before the hand-over, which
  `FaunaMlsBackend::set_room_labelers` signs as the device's actor from the
  **stored** version (the nest's ratchet) and sends through
  `RoomCeremonyRpc::room_set_labelers`; a plain member is refused before any
  round trip, in the policy editor's words. Pinned in
  `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs` and
  `room_settings.rs`. **tui paints the editor (2026-09-26; ids user-approved
  2026-09-25, owner [`../ui/conversations.md`](../ui/conversations.md)
  § Element IDs).** One `room-labeler-toggle[i]` per labeler the nest's
  catalog publishes as a kind a room may name — the kind list is now ONE
  constant, `fauna_mls::room_policy::ROOM_LABELER_KINDS`, which the door
  admits by and every editor filters by (`room_may_name_labeler_kind`) —
  with its `checked` mark and its liveness read off the draft
  (`labeler_staged`, `labeler_toggle_live`: a full set greys only the rows it
  does not name), and `room-labeler-inspect-button[i]` opening the catalog's
  own inspect view in place, so the owner inspects before choosing. Pinned in
  `room_settings.rs` (`a_painter_reads_staged_live_and_nameable_off_the_draft`)
  and the tui's `conversations/mod.rs`
  (`room_settings_editor_stages_a_community_rooms_labeler_set`,
  `a_plain_member_reads_the_labeler_set_greyed`), and end to end by the
  tier_3 two-seat journey `tests/e2e-unified/tests/test_conversation_room_labelers.py`:
  a tui owner founds the room, inspects a published fixture labeler in the
  editor, names it and saves; the room's verified set names it on the
  owner's seat; a member's message the labeler has something to say about
  paints a `content-label-badge` on the owner's bubble, and the founding
  message sent before the set existed stays unlabelled. **Not built:** the
  six-app lift.
- **Attribution lands — a community-room message is authored, not just sealed
  (2026-09-10; § The three classes → *Community* → *Who wrote it*).** The
  sealed plaintext of a `RoomSealed` envelope is now a **signed**
  `(room, generation, author, sent_at_ms, body)` core
  (`fauna_mls::room_message`) rather than a bare `ChannelMessageBody`, because
  the class's generation key is held by every member and so attributes nobody.
  The home nest's derived-view builder verifies it and additionally binds the
  author to the live floor roster, so neither a co-member's message sealed in
  another member's name nor a non-member's honestly-signed one reaches the
  search corpus — both pinned in `conformance_conversation_rooms.rs`
  (`a_send_labelled_with_another_members_name_builds_no_view`,
  `a_non_members_honestly_signed_send_builds_no_view`), each red-verified
  against the guard removed. **No data migrates:** the class was reachable from
  no app on the day this landed, so the only `RoomSealed` bytes that ever
  existed were the conformance suite's own.
- **The client half's read and send land — a community room round-trips
  between two seats (2026-09-10).** The receive walk's
  `ChannelEnvelope::RoomSealed` arm is a real arm rather than the declared
  skip: it resolves the room's generation key through two seams
  (`RoomGenerationReader` over `fauna.conversations.room.generations` /
  `…generations_remote`, home-routed like the roster read;
  `GroupReceptionKeyReader` over the account plane's
  `fauna.state.group-reception-key`), opens and **verifies** the message, and
  folds it into the same bubble an end-to-end room's produces — attributed to
  its signed author. The send is the mirror: sealed under the room's **tip**
  generation, re-read on every send because sealing under a rotated-past
  generation is the one thing a rotation exists to prevent, and posted through
  the ordinary `channel.send` as a `RoomSealed` envelope. Neither path uses an
  MLS group, and that absence is also the send's own discriminator: a bound
  channel with no group *is* a community room, since the class is born by
  `room.create` and never by `bootstrap_group`. Four pins in
  `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`: two seats
  exchange an attributed message; a co-member's message in another member's
  name never becomes a bubble; a seat holding no wrap skips the record without
  stalling the walk; a page of bubbles costs one generation read.
  **Not built:** the invitation's delivery to the invitee (below — an app
  cannot discover a room it has been invited into), a correct
  `RoomSnapshot.class` for the class — `room_state` still projects every member
  as a user principal and so derives a community room as `end_to_end` — and any
  app's paint of it.
- **A community room can be FOUNDED from an app, and is keyed in the same act
  (2026-09-10).** `FaunaMlsBackend::found_community_room` is the class's birth
  ceremony as a user performs it, and it is four acts in one because three of
  them alone would leave a room nobody could use. It resolves this account's
  **wrap target** — the newest `fauna.state.group-reception-key` record, minted
  and *persisted before the public half goes out* when the account holds none,
  the record-then-act rule the group-share consent already follows — then runs
  `room.create` with a fresh random salt and its own signed initial policy, and
  **re-derives the answered room id** rather than trusting it (the id commits to
  whose key founded the room, so a nest answering somebody else's id is refused
  before anything is bound).
  **Then it keys the room, and that is the act that makes founding mean
  anything.** A freshly founded room has no generation at all — the nest admits
  mints and never performs one — so a founding that stopped at the ceremony
  would leave a thread whose every send fails on a room only its own owner
  could repair. The mint reads the floor back through `room.list_roster` and
  wraps to every principal seated with an `(entry_id, reception_pubkey)` pair,
  the **home nest included**: that wrap *is* the materialization grant, and a
  later mint omitting it is the revoke. A principal with no pair is *unkeyable*
  and skipped, which is the coverage rule's own reading rather than a
  refusal to found.
  Six pins in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`, the
  load-bearing one opening the founder's send with the key recovered from the
  **nest's own wrap** — a client-built mint granting a real read to a principal
  the client never shared a secret with, red-verified against a `wrap_target`
  that wraps only to users. Two refusals are pinned as hard as the success: a
  reception keypair that cannot be persisted founds *nothing* (no ceremony
  reaches the nest, because a room founded and keyed to a secret that never
  reached disk is unfixable from the app), and a room that cannot be keyed is
  never bound. `libs/fauna-account-seams` (`group_reception`, wasm-capable
  since 2026-09-29; `fauna-client-account-runtime` re-exports it) carries
  the account-plane write half; the ceremony rides `NestConversationsRpc` /
  `WsConversationsRpc` like every other room seam. **Since 2026-09-26 every
  glue site (tui, linux, `fauna-ffi` for the UniFFI apps, and web) registers
  the four nest-backed room seams as one `RoomSeams` bundle**
  (`FaunaMlsBackend::set_room_seams`), so none can wire the roster pair and
  forget the ceremony and generation read, as `fauna-ffi` and web had done
  until then. **Web's key home is RULED (2026-09-26)
  and BUILT (2026-09-29): the same account-plane kind as every native app's,
  `fauna.state.group-reception-key` (fleet-only, generation-tip-sealed),
  written and read by the same `AccountStatePlane` and registered through the
  same `conversation_seams` join, at web's account runtime's store-ready
  edge** (`account-client-lifecycle.md` § The client-side lifecycle → *The
  trigger fired*, which owns the program and the host). No web twin of the
  seam exists or will. A nest-mediated
  per-kind read/write over the former `ConfigClient` would have either sealed
  a fleet-only kind under `BackupKey` — the blob rail the `__config`
  dissolution schedule retired on 2026-10-02
  ([`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
  § The `__config` dissolution schedule → *The closure order*, step (6)),
  and R14 severance lost for exactly the key severance exists
  to rotate — or re-implement tip resolution, the writer journal and the
  in-seal signature outside the plane, a second account-plane path; and
  deriving the key from the account root contradicts the record's own
  contract (a random ikm per rotation, every held key retained — rule (b)
  of the `set_reception_key` door entry below). Done correctly, both
  collapse into hosting the plane. So on web, founding and accepting an
  invitation are refused by name only while the tab's account runtime is not
  up (still assembling, or its start failed), and a community room's records
  stay unopened for that window; web's paint of the founding and accepting
  controls, and the web leg of `test_conversation_room_community.py`, are the
  trickle-down's. The governance doors (`set_policy`,
  `transfer_ownership`, `set_labelers`) need no reception key, and they work
  on web.
  **And the join, in the same landing.** `invite_to_room` signs the invitation
  as this device's identity (the fourth door the actor key stays behind, and
  the clearest case for it: the signature is what survives the nest boundary),
  `accept_room_invite` is the founding's mirror — the joiner's wrap target
  persisted before its public half goes out, for the same reason and with a
  sharper consequence, since a re-admitted member returns on a *fresh* roster
  entry and so a mis-keyed seating is not repaired by re-admission — and
  `key_in_room_member` is the inviter's own act once acceptance has put the
  newcomer on the floor. The three cannot be collapsed into one door: an
  invitation seats nobody, and a wrap names the roster entry the **seating**
  derived. The backfill carries the **tip alone**, because no client can read a
  room's `history_policy` — the roster read serves roles and a version, and the
  room plane has no policy-get door — and a batch naming one unauthorized
  generation is refused whole, which would leave a newcomer with nothing rather
  than with less. Pinned by a two-seat journey through client doors alone,
  ending with the newcomer opening the founder's message under the key it
  recovered from the top-up wrap the founder's device built for its own entry,
  red-verified against a backfill addressed to the wrong entry.

- **An invitation reaches its invitee, and the class is in a user's hands
  (2026-09-10).** § Join rules and invites' "delivered to the invitee's home
  nest **through the inbox plane**" is built, and it needed **no new RPC kind**:
  `room.invite` enqueues an `InboxKind::RoomInvite` envelope carrying the
  inviter's signed act **verbatim**, on the invitee's ordinary inbox, in the
  **same transaction** that records the invitation — a recorded invitation
  nobody can discover and a delivered one the accept door does not know about
  are both states no ceremony produced. Until this the nest recorded an
  invitation and pushed a notification naming no room, so a second seat could
  join only if a human read the room id out of a log.
  The envelope **restates nothing** the signed record says: the record names the
  room, the invitee, the role and the policy version, and a second unsigned copy
  beside it would be something a reader could be shown *instead* of what it
  verified. So the reader decodes those bytes and checks the signature binds
  them to the inviter they name **before rendering anybody's name** — the nest
  binds the signer to the authenticated caller on the way in, but this record is
  what crossed the boundary, which is the whole reason it is signed. A record
  that does not verify is **dropped, not surfaced**, and does not hide the
  genuine invitations beside it.
  It is a **knock**, not an apply: the shared drain retains the kind un-acked
  rather than dispatching it, because accepting is a decision only the user
  makes — which makes "skipped un-acked by any older client that does not know
  it" and this build's own behaviour **one** observable rather than two. The
  peek that lists standing invitations walks the queue with the skip cursor, for
  the measured reason § Sharing a folder's pending-share list already carries:
  the rows ahead of an invitation are the un-ackable residue additive-everywhere
  guarantees. A decline tells the room nothing, because a refusal the inviter
  could read would give declining an audience.
  **One standing envelope per open invitation, quota-charged (2026-09-13).**
  The first cut delivered a fresh, un-quota'd envelope on **every**
  `room.invite` call — including a client's retry of a lost reply — so N calls
  for one (room, invitee) left N un-acked rows, none of them counted against
  the invitee's inbox quota: an unbounded, free way to bury the invitee's
  genuine invitations behind the peek's page limit.
  `room_invites` now carries the delivery's own link id; a re-invitation acks
  and refunds the invitation's previous envelope before delivering the fresh
  one, so exactly one envelope stands at a time, and the fresh delivery is
  charged through the same accounting an ordinary inbox push uses. When that
  charge doesn't fit, the invitation is refused instead of delivered — but
  only where `enforce_tier_quotas` is on; it is off on the single-user
  desktop nest, which tracks no inbox quota for anyone, so there every
  invitation delivers. The check runs inside the same transaction as the
  delivery, after the prior envelope's refund and before the fresh charge,
  sharing the one connection lock held across that whole transaction — which
  is why a re-invitation that only replaces its own standing envelope nets
  zero rather than being refused by the envelope it is about to consume, and,
  read rather than separately tested, why two concurrent invitations for the
  same invitee cannot both pass the check and both charge. A refusal there
  delivers nothing and charges nothing, surfacing as `forbidden` "invitee's
  inbox is full" for every failed check alike — invitee unregistered,
  invitee suspended, envelope over `max_blob_size`, or the inbox itself over
  quota — one wording where an ordinary inbox push instead splits
  unregistered/suspended from an oversized-or-over-quota envelope into two
  different rejections. Pinned in `conformance_conversation_rooms.rs`
  (`a_first_invitation_over_quota_is_refused_and_delivers_nothing`,
  red-verified against the check removed;
  `a_reinvitation_that_nets_zero_is_not_refused_by_its_own_standing_envelope`,
  red-verified against the check moved above the refund). **Acceptance
  consumes it too:** the accept door acks the tracked envelope in the same
  transaction that seats the invitee, so the nest itself retires the knock
  rather than depending only on the accepting client's own ack of the one row
  it happened to see.
  **Painted on tui (2026-09-25):** a standing invitation is a
  `room-invitation[i]` row atop the conversation list, with accept and
  decline ([`../ui/conversations.md`](../ui/conversations.md) § Element IDs);
  the other six apps follow in the batched trickle-down.

- **A removal severs, because the client rotates (2026-09-10).** The membership
  doors reach a client for the first time: `room.remove` and `room.leave` are
  `FaunaMlsBackend::remove_room_member` / `leave_room`. The removal is **two
  acts presented as one**, and it has to be — unseating alone severs nothing,
  since a removed member keeps every generation key it was ever wrapped into and
  a room's ciphertext is fetchable by anyone the relay serves. What ends the
  read is the mint that follows, whose wrap set no longer names them ("a removal
  rotates the generation", § The three classes → *Community*, reason 4). **The
  nest never mints**, so a `room.remove` door with no rotation behind it would
  have left every caller believing in a severance that had not happened.
  **The order cannot be swapped, and is pinned red-verified.** Coverage is
  judged at the nest against the floor *as it stands now*, so a mint built
  before the unseat wraps the room's next key to the very member being removed —
  the rotation would hand out exactly what it exists to withhold.
  **A rotation that does not land leaves a named, recoverable residue**, not a
  silent one: the target is off the floor and cannot send, but can still open
  traffic sealed under the un-rotated tip, so the error says that rather than
  reading as a failed removal, and `rotate_room_key` — the same mint, aimed at
  an existing room — is the retry any owner or admin runs from any device. It is
  deliberately not undone by re-seating the target, because re-admission returns
  a member on a **fresh** roster entry and would not restore what was there.
  **A removal also ends a FOREIGN member's admission, and the rotation is no
  substitute for that (2026-09-11).** The rotation covers ciphertext. It covers
  no plaintext door, and a member homed elsewhere reaches this room through a
  `channel_foreign_members` binding whose relayed doors are gated on that row
  *alone*, by design — the roster read, the attachment write-token mint and
  `channel.actors`
  ([`../architecture/federation.md`](../architecture/federation.md) § Federation
  residue surface). Left standing, that binding outlived the seat: a removed
  member's home nest kept being served the room's **live** floor — every later
  join's `handle@domain`, roles, the signed policy, the labeler set and each
  member's reception key — the very read the same-nest door refuses as
  "membership is not public". So `room.remove` purges the binding too, the way
  `members.evict` already does on the folder plane (S8: the fetch authorization
  dies with the membership). It purges **before** the unseat, which is the
  inverse of the unseat-then-mint order above and for the inverse reason: the
  mint must follow because coverage is judged against the floor as it stands,
  while the purge reads no floor, so ordering it first makes every failure mode
  fail closed — a failed purge changes nothing and the caller retries, and a
  purge over a failed unseat leaves a seat with no relayed reach, tighter than
  it was rather than leakier, which the same retry converges. Re-admission is
  unaffected and in fact only now possible: the Welcome relay's insert arm holds
  `InsertOnly` power, so the *confirmed* row a removal used to leave behind
  would have refused a re-invite from a different home nest. The hole was
  **latent** when it was closed — a cross-nest community invite was unbuilt
  until 2026-09-26 (the invite handler admitted a same-nest invitee only), so
  nothing in production had yet bound a foreign member to a community room;
  since the cross-nest invitation landed ([`room-invitations.md`](room-invitations.md)
  § Join rules and invites → *A cross-nest invitation*) the path is reachable
  and the purge is what keeps it closed. **On the end-to-end class the same hole was reachable, and it is closed
  by a different ceremony** — that class removes by roster report, not by this
  door, so the three doors read the floor's `removed_at` verdict instead:
  [`conversation-rooms.md`](conversation-rooms.md) § Implementation status
  today → *A removal severs on the end-to-end class too* owns that half and the
  per-class asymmetry it leaves.

  **Leaving does not rotate, and that is a reason rather than an omission:** a
  leaver holds no mint authority, and a mint it could build would still wrap to
  itself, since it is on the floor until the unseat lands. A room that wants a
  departed member sealed out of new traffic rotates from a remaining owner or
  admin. The leaver's own device keeps the generations and bubbles it already
  holds — deleting them would destroy the user's own copy of a conversation they
  were legitimately part of, and would not un-read a byte.
  **But leaving DOES sever, and a foreign member had no leave at all until
  2026-09-11.** The rotation is what a leave omits; the two writes above are
  what it shares. `room.leave` is a same-nest door, and a leaver's own nest
  holds no room record for a room homed elsewhere, so what a departing foreign
  member could actually reach was the generic `channel.leave` — the binding
  delete, and nothing on the floor. That left the *opposite* residue to the one
  the removal fixed, and a worse one: the member was gone from the relay but
  still seated, so the roster-coverage gate went on **obliging** every later
  generation mint to wrap to a seat nobody occupied (a live floor entry carrying
  a reception key must be covered or the mint is refused — the room could not
  key itself without handing the key to someone who had left), while the same
  ghost made `room.invite` answer "that principal is already a member" and so
  refused the re-admission that would have healed it. The fix is the relayed
  twin `fauna.conversations.room.leave_remote` →
  `fauna.federation.conversation.room.leave`, running the same body the
  same-nest door runs, in the same purge-then-unseat order
  ([`conversation-rooms.md`](conversation-rooms.md) § The home nest names the
  door; [`../architecture/federation.md`](../architecture/federation.md)
  § Federation residue surface owns the call) — and the generic `channel.leave`
  runs that body too when the channel it names is a room, so a peer that picks
  the older kind converges to the same state rather than to half of it. Latent
  when it was closed, for the same reason the removal's hole was: a cross-nest
  community invite was unbuilt until 2026-09-26, so nothing in production had
  yet bound a foreign member to a community room; it is reachable since.
  **One clause of the removal's order does NOT carry over (2026-09-13): the
  leaver cannot be its own retry.** The order above makes a half-landed call fail closed on both
  doors — a failed purge changes nothing, and a purge over a failed unseat
  leaves a seat with no relayed reach, tighter rather than leakier — but a
  *self*-leave's own authorization is the binding it just purged, so the
  leaver's retry is refused at the gate instead of converging. What converges
  it is `room.remove` from any remaining owner or admin, the door that reads a
  rank rather than a binding, so the state stays recoverable by a client and
  never needs a hand at the nest
  ([`../architecture/nest/common.md`](../architecture/nest/common.md)
  § Client-state recoverability). Reaching it takes a storage failure between the
  two writes; the leave's own doors are otherwise idempotent.
- **The policy doors reach a client, over a policy the client can finally read
  (2026-09-10).** `room.set_policy` and `room.transfer_ownership` are
  `FaunaMlsBackend::set_room_policy` / `transfer_room_ownership`, and the
  prerequisite they waited on landed with them: **the roster read now serves the
  signed policy its version names.** Until it did, a client could learn a room's
  policy *version* and nothing else — which left the doors unusable rather than
  merely awkward, because a change is a **replacement** at `stored + 1`, so an
  app authoring one without the stored document would carry defaults for every
  field it did not show the user and storing it would silently reset them: a
  rename that also turned history on.
  It rides the roster read rather than a `room.get_policy` kind because it *is*
  the same read, and it widens nothing — that read is admitted only to a live
  member, and a member is exactly who renders and verifies a policy. The reader
  checks the signature before trusting a field, for the invitation's reason: a
  policy names an owner, an admin set and a room's name, and amending an
  unverified record would launder it into the room's history under the amender's
  own signature. One that does not verify reads as **no policy at all** — the
  same answer a policy-less room gives, and the honest one.
  So an edit **names only what changes** and is applied to the bytes just read
  back; every field it does not name survives byte for byte. The version bump is
  the nest's own ratchet, so two devices editing at once means the second is
  refused rather than silently winning, and the recovery is to re-read and
  re-apply.
  **A transfer drops the successor from the admin set**, which § Roles and
  authorization already required and the structure enforces: exactly one owner
  exists and the owner is never also an admin, so a policy naming it in both
  does not sign. Every other rank survives, the outgoing owner's included — it
  becomes a plain member, and so becomes removable like any other.
  **The apps reach all of it through the door they already call.** The rail's
  `update_room_policy` arm — the room-settings editor's and the thread-header
  rename's — now routes a community room to these doors and an end-to-end room
  to the MLS commit, branching on the same local signature the send path uses:
  **a bound channel with no MLS group is a community room**. So no app learns
  the class, and one edit vocabulary covers both. Until this, that arm read the
  policy out of a group context a community room does not have, and refused
  every gesture with "this room has no room settings" — on exactly the class
  the editor was most needed for.
  **And the header stops lying about the class (same day).** `room_state`
  derived a room's class from its *participants*, so it could never see the home
  nest — which is on the **floor** and is not a participant any thread renders —
  and every community room painted as `end-to-end`: a claim of privacy the room
  does not have, which is the worst direction to be wrong in. The class is a
  function of the member set (§ Architectural rules, rule 1), so it is now
  derived from the floor's own principal kinds, cached by the poll pass that
  already reads the roster to name members. Two things make that safe rather
  than convenient: the read is bounded to channels with **no MLS group** (an MLS
  room decides its class locally and correctly, so it costs no read), and until
  the read lands the header reports the local answer — which can never say
  `community` — rather than a guess. "No MLS group" alone could not have
  answered it: an MLS room this device has not joined looks identical locally,
  and rendering *that* as community would be the same lie in the other
  direction. The floor also carries the ranks and the policy a community room's
  editor needs, which no group context holds for it. **An unnameable principal
  kind derives transport-only** — an older app meeting a kind a newer nest
  introduced degrades to the cautious label, the mirror of the same variant
  never being handed a key.
  Four defects surfaced by building all this, all fixed here: the class above;
  the rail routing; **a community room's policy could not be signed at all** if
  it carried the `request` join rule
  (`RoomPolicy::sign` runs the end-to-end validation, which refuses it), so
  `sign_community` is now the signing half of the pair
  `verify_signature_community` was already the verifying half of; and the
  transfer's admin-set rule above, which no code had ever applied because no
  client had ever authored a transfer.

- **The class reaches the app layer: a room is founded, joined and keyed
  through the doors the apps already call (2026-09-10).** Until this every
  community-room door lived on the rail backend and nothing above it called
  one, so no app could found or join a room even in principle. Now
  `ConversationsManager` carries the whole cycle, for all seven apps at once
  (priority #2):
  **Founding is a member choice in the composer.** The home nest is never a
  chip, so the new-thread picker carries it as `include_home_nest`; checked,
  `prospective_room_class` states `community` before the first message (the
  ordered rule still makes a bridge-ridden room transport-only whatever else
  is seated), and `send_new_thread` founds the room — the ceremony, a first
  mint wrapped to the nest, one invitation per recipient — instead of
  bootstrapping an MLS group. The typed topic becomes the room's name. The
  thread is keyed by the room's channel from the ceremony's answer on, so a
  plain conversation with the same people stays a different thread.
  **Joining is the invitations list.** `ConversationsSnapshot::room_invitations`
  lists every verified invitation standing for the account, refreshed on the
  receive loop's full sweep beside the inbox drain whose kind it rides;
  `accept_room_invitation` seats the account and opens the room as a
  channel-keyed thread, `decline_room_invitation` settles it and tells the room
  nothing. An invitation into a room this device holds a **live seat** in is
  settled quietly rather than offered again — a bound thread alone is not
  "sits in": a removed member keeps the thread binding (the device keeps its
  bubbles), so a legitimate re-invitation after a removal is offered rather
  than swallowed.
  **Inviting is the existing add-participant gesture.** The rail's add arm
  routes a bound channel with no MLS group to `room.invite` — the same local
  signature the send and policy arms use — and the manager adds no optimistic
  chip, because an invitation seats nobody.
  **The inviter's key-in finally runs.** Acceptance seats a member with a wrap
  target and no wrap, and until now nothing covered them: the door existed and
  had no caller. `FaunaMlsBackend::tend_community_room`, on every poll pass,
  re-reads a group-less channel's floor every `FLOOR_REFRESH_POLLS` polls
  (sooner after this device's own room act), refreshes the cached class, ranks,
  policy and nest read, seats the floor's users as the thread's participants,
  and — on an owner's or admin's device — keys in every user member the tip
  does not cover. It needed one additive roster field, `tip_wrapped`: which
  seatings the room's current generation wraps to, entry ids only.
  **A newcomer's walk waits for its key-in.** The receive walk used to step
  past any `RoomSealed` record it held no key for — right for a generation the
  member was never wrapped into, and a permanent loss for everything sealed
  under the tip between acceptance and key-in, which *is* the newcomer's to
  read. It now stops before such a record while the account holds no wrap in
  the room at all, or the generation read failed, and reports the wait as an
  expected state rather than a strand; a record under a generation it predates
  is still stepped past once it holds any key.
  **The wait is scoped to the room, and the room says it is waiting
  (2026-09-12).** Nothing bounds the wait — it ends when an owner's or admin's
  device next polls, which may be never — so it may hold nothing else hostage:
  the room folds *for now*, and the account's Conversation catch-up boundary
  closes on its other channels
  ([`content-index-ingest.md`](content-index-ingest.md) § Ingest triggers, v1 →
  *A community room waiting for its key-in*, which owns that boundary and what
  withholds the room's own backlog once the key lands). Before this, one
  unkeyed room stalled the whole account's catch-up for as long as nobody keyed
  it in. `RoomSnapshot::awaiting_key` is what a waiting room's own row says, so
  an app paints the wait rather than an empty thread — as `thread-room-notice`'s
  `awaiting-key` state, on tui since 2026-09-26 (*Still dark*, below, for the
  rest). **A home nest that serves no
  `tip_wrapped` answer gets no special refusal**, deliberately. A `user` row
  answering `null` is not keyed in — `null` means unknown, and wrapping to every
  unknown row on every poll is the re-wrap the field exists to prevent — and it
  is not refused at the invitation either: no released nest predates these
  doors (they landed 2026-09-09; the released nest is 2026-08-27), so the only
  party that can withhold the field is a nest that could equally refuse the
  key-in itself, and in this class every roster input but the signed policy is
  that nest's word anyway (*How far the revoke reaches*, above). Against it,
  what protects the account is the scoping above, not a door check it can pass
  at will. The room waits, alone and visibly. A `null` on the nest's **own** row
  is a different answer, ruled there: it keeps the wrap.
  **The nest's read is the members' standing choice, and a rotation keeps
  it.** The same `tip_wrapped` answer on the nest's own row is what the roster
  says about the grant, so every member sees it, not only the minter; an
  ordinary rotation — a removal's severance included — wraps to the nest
  exactly when the current tip does, where before it wrapped to every target it
  saw and would have re-granted the withdrawn read unasked, on an honest
  device's ordinary rotation — the keep guards against that drift, and reads
  the nest's own answer, which is as far as it reaches (§ Implementation
  status today, the sealing entry → *How far the revoke reaches*). Granting and
  withdrawing it are `set_room_nest_read`, a rotation with the nest in or out,
  staged in the room-settings editor's shared draft
  (`RoomSettingsDraft::nest_read`) and committed on Save before any hand-over;
  `RoomSnapshot::nest_read` is its projection.
  **Painted on tui (2026-09-25):** the founding toggle
  (`recipient-picker-home-nest-toggle`), the invitations list and the
  editor's nest-read control (`room-nest-read-toggle`) — the whole class is
  reachable from an app, witnessed end to end by the tier_3 two-seat
  journey `tests/e2e-unified/tests/test_conversation_room_community.py`
  (found, accept, key-in, a message each way, the nest's index finding the
  text for a member, and the read withdrawn from the editor emptying it).
  The six others follow. **A waiting room says
  so** on tui (2026-09-26) through `thread-room-notice`'s `awaiting-key`
  state, shared with the unverified-moderation notice
  ([`../ui/conversations.md`](../ui/conversations.md) § Element IDs); the
  six others follow in the room trickle-down.
  **Not witnessed end to end:** the journey above keys the member in with no
  gesture, so the waiting window is a race against the founder's device, not
  a state a latency-independent test can hold — the paint is pinned by the
  tui unit test and the shared `RoomNotice` test instead.

- **The add's backfill lands, and `history_policy` becomes enforceable
  (2026-09-10).** `fauna.conversations.room.backfill_generations` is the
  scheme's **archival backfill** as a door: an owner or admin publishes
  `GroupTopupRecord` wraps covering one newly-seated member, and the nest
  admits them on five checks — a floor-authoritative room, the caller ranked
  owner-or-admin (the mint's own rank, because the act hands out a generation
  key), a **live user** floor member as the target, each record verifying at
  its cell with its healer bound to the authenticated caller and its
  `target_entry` to the target's **current** roster entry, and the history
  policy. It is the *add* side of the scheme's mint triggers
  ([`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
  § The recipient-set scheme → *Mint triggers*: adds never mint), so the door
  structurally cannot move the room's tip, and the batch is all-or-nothing —
  a refusal leaves no partial bundle behind.
  **What the policy bounds, and the one thing it does not.** `full`
  authorizes the whole retained bundle; `none` — every room's default —
  authorizes the **tip alone**, which is § History for joiners' "the nest
  refuses to store one" made operative. The tip stays authorized under `none`
  for a reason worth not re-litigating: coverage is checked at **mint** time
  over the floor as it stood then, so a member seated afterwards holds no
  wrap for the tip either and cannot read the room's *new* messages. That is
  the admission/mint race the scheme's member top-up exists to heal, not
  history — withholding the tip would not enforce a history policy, it would
  leave a newcomer unable to read the room at all.
  ⚠ **The home nest is never a backfill target** (ruled here). Its read is a
  grant the members make *at a mint* and withdraw by rotating it out, so a
  door that could re-wrap to the nest would let one admin restore a read the
  members had just revoked, outside the single act the design gives them.
  The threat the refusal answers is an admin acting outside the toggle — one
  device's act that every member would otherwise miss — never the nest
  itself, which the revoke does not bind beyond honesty (*How far the revoke
  reaches*, above). The refusal is explicit rather than incidental.
  **What is still unbuilt** of the sealing: the read position's **other**
  purpose (the community-as-audience post pass — search is built, and since
  2026-09-10 readable through `fauna.conversations.room.search`, the bullet
  below; labels landed the same day; the fan-out clause was struck), and item 5's **home-nest relay** for a foreign member — which is
  narrower than it was recorded as being: the floor gate the relay needs on
  `fauna.federation.channel.append` **has been inherited since the sealing
  build**, because that handler runs the same `channel_send_core` the
  same-nest `channel.send` does and the community floor gate lives inside it;
  and `channel.fetch` is deliberately not floor-gated (above). What the relay
  genuinely lacks is the **generation read** — a foreign member has no way to
  fetch its own wraps through its home nest. Beyond the
  sealing: **re-homing** — and, until 2026-09-26, cross-nest invite delivery.
  **The cross-nest invitation is BUILT (2026-09-26):** the room home delivers a foreign invitee's knock to the
  invitee's own nest over `fauna.federation.conversation.room.invite`, that
  nest runs its member's reach policy and binds the home's verified address
  as the knock's `room_node`, and the acceptance rides
  `room.accept_invite_remote` → `fauna.federation.conversation.room.accept`,
  which the home gates on the invitation it delivered, seats through the
  same-nest body and only then binds as a foreign member — ruling, order and
  proofs in [`room-invitations.md`](room-invitations.md) § Join rules and
  invites → *A cross-nest invitation*. The invitation *issued* by a member
  homed elsewhere is BUILT the same day:
  `room.invite_remote` → `fauna.federation.conversation.room.invite_issue`,
  gated on the inviter's binding and run through the same-nest invite body
  on the home, whose three delivery arms reach an invitee on the inviter's
  nest, on the home, or on a third nest — the same section's closing
  paragraph owns it.
  **Re-homing** is the same shape: § The home nest gives a transfer to
  an owner homed elsewhere a signed room-succession record the old home
  publishes to every member's home nest, with the log moving by the segment
  backup protocol; none of that exists, and a transfer that silently left the
  room homed here would leave the new owner's nest believing it homes a room
  it does not — so such a transfer is refused rather than half-applied.
  One further bound, a refusal rather than silence: the `request` join
  rule's knock door is a later slice, so under `request` the owner and
  admins invite as under `invite`. The nest's room-read keypair
  (2026-09-09, schema 52, published on `fauna.nest.info` as
  `room_read_pubkey` —
  [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
  § Audience: deployment infrastructure → *Room-read keypair*) now has both
  a writer and a reader, and the home nest of a community room is the one
  declared exception to "no nest path opens a conversation envelope"
  ([`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest,
  amended the same day — the sentence survives literally, since a community
  envelope is not an MLS message).
  Every room born through the *report* door still derives `end_to_end`,
  because a report only ever carries user principals.
  **No app calls the ceremony yet** — it is nest-side only, so the class
  cannot be reached from any of the seven apps.
- **The roster read serves the wrap targets, so a mint is buildable from
  outside the nest process (2026-09-10).** `fauna.conversations.room.list_roster`
  now carries each member's `entry_id` and `reception_pubkey` — additive
  `Option` fields, so a nest older than the pair reads as
  wrap-targets-*unknown* rather than as a decode failure or as a roster of
  unkeyable members. This is the roster entry's own defined value
  ([`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
  § The recipient-set scheme → *The roster kind*: "the member's actor key,
  the reception pubkey observed at add, `Enrolled | Removed`, stamps"), not a
  widening of the read: a mint is admissible only when its wraps cover the
  Enrolled roster (same § → roster coverage), so a minter must be able to see
  what it is required to cover, and a *member* must be able to check that a
  mint covered it rather than take the nest's word.
  ⚠ **Why this was load-bearing and invisible.** Every mint in the nest's own
  conformance suite was built from `list_floor_roster` — a direct database
  read, a door only code inside the nest process has. So the class could pass
  its whole suite while remaining unmintable by any client, and did: the
  wrap-target pair reached no wire. The pin is shaped to keep it that way —
  `the_roster_read_serves_the_wrap_targets_a_minter_must_cover` asserts the
  *wire* read reproduces what the database helper produces, rather than
  asserting the fields are merely present. The relayed twin
  `room.list_roster_remote` inherits the pair, since it returns the home
  nest's own reply verbatim.
- **The community room's attachment key — ratified 2026-09-10; the client
  half BUILT the same day** (§ The three classes →
  *Community* → *Attachments — the second content kind*):
  `fauna_core::group_content::ROOM_ATTACHMENT_CONTENT_KIND` beside the message
  kind, `fauna_mls::room_message::{seal,open}_room_attachment` (the
  `blob_crypto` shape under the attachment kind, generation-bound), and
  `FaunaMlsBackend::send_room_message` seals + uploads each attachment under
  the tip generation and lists its `attachment_refs` exactly as the end-to-end
  path does, while the `RoomSealed` receive arm fetches, opens and caches them
  under the message's generation (`epoch` written `0`, unread). Pinned by
  `two_seats_of_a_community_room_exchange_an_attachment`
  (`libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`) and the
  kind-separation tests in `group_content.rs` / `room_message.rs`. **Not
  built:** the nest side — no nest path opens an attachment blob today. The
  search view indexes the caption only, by § *What the read covers*, and the
  label build (2026-09-10, § *Labels land* above) hands a room's labelers an
  attachment's declared metadata — that there is one, and its MIME type — but
  not its bytes, because the labeler input has no bytes facet; growing one is
  an ABI decision every published module is party to.
- **A seat gains or rotates its wrap target (2026-09-13).** Until this landed every writer of
  `room_members.reception_pubkey` was a *seating* act — the birth ceremony and
  `room.accept_invite` carry the key with the row — so two seats could exist
  and never become keyable: the successor an identity succession seats (the
  ceremony deliberately writes no key, since the predecessor's belongs to the
  material it retires — [`conversation-rooms.md`](conversation-rooms.md)
  § Implementation status today, the succession-axis bullet), and a member
  seated before the sealing plane existed. Both governed a room they could not
  read: coverage skips a keyless seat (§ The three classes → *Community*, the
  admission's third check) and the backfill refuses one. **The door:**
  `fauna.conversations.room.set_reception_key` — a live **user** member
  supplies its own X-Wing public half for the seat it already holds; the nest
  checks the room is floor-authoritative, the key is the one shape every wrap
  seals to, and the caller holds a live user seat, and nothing about rank —
  every member owns its own wrap target, as it does at acceptance. Never
  another principal's: the home nest's read is a grant the members make at a
  mint, a bridge is seated by its enrollment. **Two rules decided here.** *(a)
  A key already set is replaced, never refused* — the scheme's wrap target is
  the member's *current* reception key, rotated on its own fleet events
  ([`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
  § The recipient-set scheme → *Severance, per axis*), so a door that froze
  the first key would contradict the owner doc. *(b) The seat keeps its roster
  entry across a rotation.* A wrap is bound to `(generation, entry)` and sealed
  to the key of that moment, and the account retains every reception key it
  held, so the wraps already stored at the entry stay openable and nothing is
  re-sealed; a fresh entry is the scheme's *re-admission* rule (a `Removed` →
  `Enrolled` transition), and a live seat changing its key is not that
  transition. A seat with no entry at all is given one, derived from now, so
  the pre-sealing seat is healed by the same statement. **The door mints
  nothing** (the nest never mints); it answers what the seat still owes —
  `uncovered_tip`, the room's current generation when the seat holds no wrap
  for it — because `room.generations` serves a caller only the generations it
  holds a wrap for, so an uncovered seat cannot learn from that read the tip a
  mint must name as its parent. Replay forbidden and online-only: a replayed or
  offline-queued older set would roll a rotated seat back to a retired key.
  **The client half rides the tend pass, on every app.**
  `FaunaMlsBackend::tend_community_room` — the per-poll step every app's
  shared session already runs for a community room — now heals this device's
  **own** seat before it keys anybody else in (`heal_own_room_seat`): a seat
  the floor holds keyless, or under a key the account has rotated past, is
  re-bound to the account's current key; then, when the seat ranks owner or
  admin, an answered `uncovered_tip` makes it **mint a fresh generation
  parented on that tip** — the severance mint the predecessor's `Removed` row
  calls for (the taxonomy's trigger 1) and the only way a seat that is the
  room's sole key authority ever reads again, since a top-up is an owner's or
  admin's act — and a reported *rotation* of a still-covered seat makes it
  rotate the room (trigger 2, the member's fleet-severance signal). A plain
  member mints in neither case: an owner's or admin's tend pass keys an
  uncovered one in on its next pass exactly as it covers a seated newcomer. A
  seat that is current and covered costs the pass nothing — no round trip.
  **Why the successor's key is fresh, not the predecessor's:** the successor's
  account plane is its own — the predecessor's `fauna.state.group-reception-key`
  rows are sealed under a `BackupKey` schedule this identity does not hold and
  no aftermath leg re-seals that plane — so the first pass finds no record and
  `current_reception_key` mints one: exactly the fresh published half the
  taxonomy's fleet-severance rule asks for. **Pinned:** a REAL
  `fauna.recovery.succession.submit` in `conformance_conversation_rooms.rs`
  (`a_successor_supplies_its_wrap_target_and_reads_a_generation_minted_after_the_ceremony`
  — keyless seat opens nothing, the door keeps the ceremony's entry, an
  admin's top-up covers the tip, a generation minted after the ceremony wraps
  to the successor and not to the retired seat;
  `a_successor_that_is_the_rooms_only_key_authority_mints_itself_back_in` —
  the mint door admits a parent named but not held; the rotation keeping its
  entry with the earlier wrap still served under the retained key; the three
  refusals), the db's entry-minting and user-seat-only rules in
  `db/rooms.rs`, and the tend pass's four shapes in
  `fauna_mls_backend_tests.rs` (keyless owner mints itself back in over the
  floor as the nest now serves it, keyless member mints nothing, a current
  seat costs nothing unless it governs and is known uncovered, a rotated owner
  re-binds and rotates). **Declared bounds.** *(i)* A plain member's rotation
  waits for a key authority's next mint: no owner's or admin's pass observes
  another member's rotation yet, because the floor read carries coverage, not
  key age — the taxonomy's trigger (2) is served for the governing seat only. *(ii)* Same-nest only, as `room.accept_invite` is: a seat homed on another
  nest reaches its room's home through a relay kind that does not exist yet,
  and today every seat is local.

- **A reception key is FIPS-203-valid before it is ever stored (2026-09-14).** Until this landed, `set_reception_key`
  checked only that a key was 1216 bytes, and `room.create`/`accept_invite`
  checked nothing at all — so a well-formed-length key whose ML-KEM-768 half
  fails FIPS 203 §7.2 validation could be bound to a live seat. Coverage (the
  admission's third check, above) does not skip such a key — it is
  non-empty — so every later mint or rotation over the room tried to wrap to
  it and failed inside the seal, freezing the room's key authority until the
  poisoned seat was found and removed; a removal attempted meanwhile landed
  the unseat but not the rotation, leaving the removed member reading new
  traffic. **The fix: one validator, four call sites.**
  `fauna_pq_kem::XWingPublicKey::parse_and_validate` — length AND
  `mlkem768::validate_public_key` on the ML-KEM half (the X25519 half is any
  32 bytes) — is now what `fauna_mls::wrapped_blob::generation_wraps::
  parse_target` runs before ANY seal (device, escrow, or group/room), so a
  key a writer admits is a key the seal accepts by construction. Every nest
  writer of `room_members.reception_pubkey` runs the identical check through
  `conversations_handlers::reject_invalid_reception_key` before storing a
  **non-empty** key: `room.set_reception_key` (unconditionally — a key is
  never optional there), `room.create` and `room.accept_invite` (only when
  the caller supplied one — empty stays the ordinary "not keyed yet" seat,
  additive and optional exactly as `fauna-protocol`'s `RoomCreateRequest`/
  `RoomAcceptInviteRequest` docs say). **The rule this makes true: a stored
  NON-EMPTY reception key is always FIPS-203-valid — every writer refuses one
  that isn't, so coverage and the seal may assume it.** **Already-poisoned
  rooms heal themselves, non-destructively** — no sweep empties any row.
  `usable_reception_key` (non-empty AND valid) replaces the bare non-empty
  check at the two READERS of this column that decide what the scheme can
  wrap to: the coverage rule (above) now skips an invalid stored key exactly
  like no key at all, and `room.list_roster`'s wire projection reports it as
  absent rather than as unwrappable bytes — which is what lets the member's
  own next `FaunaMlsBackend::tend_community_room` pass notice it holds no
  *usable* key (`own.reception_pubkey` reads back `None`) and re-supply its
  current one through `heal_own_room_seat`, exactly the path that already
  heals a keyless seat. `room.backfill_generations` applies the same
  predicate to the same column for the same reason (a target with only an
  invalid stored key is exactly as uncoverable as a keyless one). **A safety
  net, not a load-bearing check:** the account's own key is re-derived from a
  stored seed (`GroupReceptionKeyRecord::keypair`) and ML-KEM-768 keygen
  yields a valid key for every seed by construction, so
  `FaunaMlsBackend::self_check_reception_pubkey` — run before
  `found_community_room`, `accept_room_invite`, and `heal_own_room_seat`
  publish a key — is never expected to fire; it exists so a corrupted local
  derivation fails loudly on the device rather than poisoning the room.
  **Pinned:** `fauna-pq-kem`'s `parse_and_validate_*` unit tests (the
  validator itself: accepts a derived key, rejects the wrong length, rejects
  an all-0xFF key of the right length); `fauna-mls`'s
  `a_mint_refuses_a_fips_invalid_target_key_before_sealing` and
  `a_group_mint_refuses_a_fips_invalid_reception_key_before_sealing` (both
  seal paths refuse before `encapsulate` ever runs, as `InvalidInput` rather
  than `HpkeFailed`); `bins/fauna-nest/tests/conformance_conversation_rooms.rs`'s
  `set_reception_key_refuses_a_fips_invalid_key`,
  `room_create_refuses_a_fips_invalid_founder_key` (asserts no room is
  founded), and `accept_invite_refuses_a_fips_invalid_key` (asserts the
  invitation stays pending and no seat is added) — each red before its
  door's writer ran the check. The recovery arm — a seat poisoned before this
  fix, never writable again by any door — is pinned by the same file's
  `a_poisoned_seat_is_skipped_not_frozen_and_heals_on_its_own_rekey`, seeded
  straight through `CacheDb::set_room_member_reception_key` (the one writer
  with no validation): the roster reports the seat's key absent, the owner's
  mint over the rest of the floor is admitted, `room.backfill_generations`
  refuses the seat, and the seat's own `room.set_reception_key` heals it
  (`rotated: true`) and is covered by the next mint.

- **A foreign member of a community room learns of a new message only by
  polling its own home nest — traced end to end, and the class needs no fix
  (2026-09-13).** Same-nest fan-out is
  `list_channel_actors` → `notify_push` inside `channel_send_core`
  (`bins/fauna-nest/src/conversations_handlers.rs:1121-1131`), and
  `list_channel_actors` reads the local `actor_channels` table only
  (`bins/fauna-nest/src/db/channels.rs:510-513`) — a foreign member's
  `channel_foreign_members` row is never among the subscribers pushed. The
  inbound relay leg `fauna.federation.channel.append` runs the identical
  `channel_send_core`
  (`bins/fauna-nest/src/federation_handlers.rs:1941-1961`), so even a foreign
  member's own send fans out only to its **home** nest's local WS
  subscribers, never back across the federation channel to the sender's own
  nest. The federation kind inventory carries `channel.fetch` / `append` /
  `leave` / `actors` and no advance-or-notify kind
  ([`../architecture/federation.md`](../architecture/federation.md):563-566:
  "nothing nest-side drains a channel on its own, only a member's own client
  calling `fauna.conversations.channel.fetch`"). A community room inherits
  this shape verbatim, not by extension: its foreign-member relayed read is
  the same membership-gated pull (*The relayed read carries them too*,
  above), which is exactly [`direct-messages.md`](direct-messages.md)
  § Technical Flow — Cross-Nest step 3's fetch relay, shared uniformly across
  every class (priority #1). So arrival is bounded only by the client's own
  poll cadence — `DEFAULT_CONV_POLL_SECS = 30` s
  (`libs/fauna-conversations/src/session.rs:1812`), the same backstop every
  other class already runs under, never a slower one for being a community
  room. **Not a gap needing a build**: the row that opened this entry asked
  whether the class needed a keyless push; verified, it doesn't — the pull
  path it already has is the whole of the cross-nest leg. A keyless
  nest-to-nest "channel advanced" push, cutting cross-nest arrival latency
  below the 30 s backstop for every class at once, remains unbuilt and
  unratified: no goal doc calls for it today, and scoping it is a design
  proposal to [`../architecture/federation.md`](../architecture/federation.md)
  (the federation kind's owner) and
  [`direct-messages.md`](direct-messages.md) (the cross-nest flow's owner),
  not a claim this doc makes.
- **A community room carries the sender's own delete and reactions
  (2026-09-19), and an owner's or admin's delete of another member's message
  (2026-09-20) — in shared Rust and at the floor; no app journey reaches the
  class yet.** The first ride the sealed path, the shared send routed by class
  and the `RoomSealed` ingest folding them by the signed author. The second is
  a signed floor act the home nest judges **without reading**, with the
  retained policy versions it needs; what members do with the record, the
  anchor they prove first, and the residuals still declared (successions
  first among them) are
  [`conversation-rooms.md`](conversation-rooms.md) § Roles and authorization →
  *Delete any message — the mechanism*'s, and the next entry declares them —
  that doc's *Delete any message* entry's community half, moved here 2026-09-28.
- **Delete any message, the community class — moved 2026-09-28 from
  [`conversation-rooms.md`](conversation-rooms.md) § Implementation status
  today, whose *Delete any message* entry keeps its head and the end-to-end
  half.** *Community:*
  **the sender's own delete and reactions are built (2026-09-19)** —
  `send_delete` / `send_reaction` route by class as `send` does
  (`FaunaMlsBackend::post_side_effect`: no MLS group → the body seals under
  the room's tip generation), and the `RoomSealed` ingest arm folds `Reaction`
  and `Delete` with the claimant the verified signed author and no role, so
  the sender match alone admits a sealed delete; pinned by
  `a_community_rooms_own_delete_and_reactions_ride_the_sealed_path`
  (`fauna_mls_backend_tests.rs`, red-verified). **The owner's or admin's
  delete is built at the floor (2026-09-19) and on members (2026-09-20).** At
  the floor: the record (`fauna_mls::room_policy::RoomFloorDelete`, its own domain tag,
  bound to its room) and the additive `ChannelEnvelope::RoomFloorDelete`
  variant; the floor's four checks in the one `channel_send_core` chokepoint
  (`judge_floor_delete` — so the cross-nest append leg inherits them), each
  red-verified by mutation, plus the refusal on a room whose authority is not
  its floor. ⚠ **That "each" was untrue for check 1 until 2026-09-21**, and the
  gap is recorded rather than quietly closed: every conformance case filed a
  validly signed record for the room under test, so `signed.verify(room_id)`
  reddened nothing when removed — a pinned predicate is not a pinned *use*, and
  `room_policy.rs`'s own `verify` tests were answering a different question. The
  witness is now `a_floor_delete_record_with_a_bad_signature_or_another_rooms_bind_is_refused`
  (`conformance_conversation_rooms.rs`), which tampers the signature and files a
  record its author genuinely signed for a *different* room it also owns — so
  every other check passes and only the room bind refuses it — and asserts the
  next honest record lands at the very next seq, which is what makes "nothing
  stored" an assertion rather than a hope; the sealed-storage shape check admitting the variant on its own
  strict decode; and the retained versions — additive `room_policy_versions`
  (schema 68), written by **every** policy writer (founding, `set_policy`, an
  ownership transfer) and served by number through an additive
  `at_policy_version` on the roster read and both its cross-nest legs. A room
  founded before schema 68 retains its then-current version onward; versions
  it had already superseded were overwritten in place and are gone (no record
  can name one — none existed). The version skew is pinned at the wire: a
  build that predates the variant fails its decode. **On members:** the one
  step judge and the anchored chain are shared Rust
  (`fauna_mls::room_policy::judge_community_policy_step`,
  `verify_community_birth`, `CommunityPolicyChain`), and the home nest's three
  policy doors run them too — the birth door the birth check, `set_policy` and
  `transfer_ownership` the step judge with the floor's own designees
  (`judge_policy_step`), keeping only what the floor alone knows (the caller
  is the signer; named admins are live members; an incoming owner is a local
  user). The birth salt rides the roster read beside a version asked for
  (additive `birth_salt`, both cross-nest legs — they forward the home's reply
  unchanged); the client seam is `RoomRosterReader::read_policy_version`
  (default: not held — fails closed), one helper for the native and wasm arms.
  The backend judges a record on ingest (`floor_delete_verdict`: the
  signature, this room, then the author's rank in the **anchored** policy of
  the named version, the chain fetched from the head it already proved and
  kept per room for the session) and records the claim with that rank; a
  refused record paints nothing, and one whose version could not be fetched
  *yet* is parked (64 per room, 32 version reads per judgment) and judged again
  on the next pass, since the walk has stepped past it. The send:
  `RoomSnapshot::viewer_deletes_any` now holds for a governed community room,
  and `ConversationsManager::delete_message` sends the viewer's own target as
  the sealed `Delete` and anyone else's through
  `ConversationBackend::send_delete_any` — the floor record under the policy
  version a fresh floor read names (an end-to-end room seals the same `Delete`
  either way). Pinned by
  `a_community_floor_delete_is_honoured_only_under_a_policy_the_member_anchored`
  (`fauna_mls_backend_tests.rs` — the owner's record through the manager's
  gesture and the admin's tombstone on another seat; a plain member's, one
  naming an unheld version, and **a nest-minted key's under a forged version 1
  and under a forged later link** paint nothing; a parked record paints on the
  pass after its version arrives), the three chain tests in
  `room_policy.rs`, and the conformance roster read anchoring what it was
  served. **Successions resolve member-side (2026-09-20, *A name designates its
  verified line*):** the shared judge takes a designation
  (`room_policy::SeatDesignation` — the floor's single-valued closure
  unchanged, the member's `SuccessionLines`), and `floor_delete_verdict` asks
  the session witness (`SuccessionWitness::succession_line`, satisfied once by
  `ChainWitness::resolve_line` over `resolve_succession_line`, the anchored
  walk with every hop returned, for all 7 apps) only on a rank refusal a
  succession could lift — at most 4 first-time asks per judgment, the refused
  link kept so a parked record's retries do not re-read it. **Every verified
  line is asked again (2026-09-23 — the empty line first, the positive line
  later that day):** the witness memoizes a line per dial
  (`ChainWitness::resolve_line`), and `RoomSeats::verified` holds every name
  a rank refusal asked about — its line positive or empty — with the pass it
  was last asked at, apart from `not_yet`; once `LINE_REASK_PASSES` (16) of
  the room's inbound passes have gone by a rank refusal re-asks it through
  `SuccessionWitness::recheck_line`, which forgets whatever the memo holds
  for the name (a positive line, an empty one, a walk that failed) and walks
  again — **the bound: per room, at most one re-dial per verified name every
  16 passes**, each spending the same 4-ask judgment budget, whatever drives
  the passes. **A re-ask that dials nothing still reads the anchor store** (a
  not-yet name's, every
  judgment), under the store-read bound
  [`identity-succession.md`](identity-succession.md) § The succession
  statement → *What a statement may cost the member who receives it* owns —
  including its backoff while the anchor store cannot be read (built
  2026-09-23 against the `__config` copy, retired 2026-10-02; the store is
  the account-state plane kind `fauna.state.peer-anchors`,
  [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*;
  `ThreadParticipantAnchors::refresh`, pinned by
  `a_failing_store_read_is_retried_once_per_backoff_interval`,
  `fauna-client-recovery/tests/witness.rs`). A line only grows:
  `SuccessionLines::insert` takes a re-answer
  for a root it already holds only when it extends the held line, so a
  shorter or forking answer changes nobody's seat. With a witness registered
  a rank refusal always parks (a policy step keeps its link as `pending`)
  instead of settling as a member's claim — every line may yet grow — so a
  successor's record parked under the stale answer paints on the pass the
  re-ask learns the line; with no witness a name stands for itself and the
  refusal is final. Pinned by
  `a_later_succession_of_a_name_settled_empty_is_honoured_in_the_same_session`
  (`fauna_mls_backend_tests.rs`: the admin succeeds after a thief's record
  verified both names empty; the successor's record parks, paints within the
  cadence, each empty line re-asked once over 18 passes, the thief's parked
  and unpainted),
  `a_successors_own_later_succession_is_honoured_in_the_same_session` (same
  file: the owner's verified successor succeeds in turn after its own record
  painted; the third holder's record parks, paints within the cadence, the
  positive line re-asked once, the thief's parked and unpainted),
  `a_held_line_grows_only_by_extension_and_never_shrinks` (`room_policy.rs`)
  and `a_recheck_rewalks_whatever_the_memo_holds`
  (`fauna-client-recovery/tests/witness.rs`). ⚠ **A record judged before
  2026-09-23 under a stale line — empty, or positive with its newest holder
  since succeeded in turn — was recorded as a member's claim and stays so** —
  a relaunch does not heal it: the walk had stepped past it, and it was never
  parked. Pinned by
  `a_succeeded_seats_community_floor_delete_is_honoured_only_through_a_verified_line`
  (an intermediate successor's version anchors, each successor's record
  tombstones, a thief's and a retired key's under a version naming its
  successor park, bounded, and never paint, no witness fails closed, no
  anchor parks then paints) and
  `a_name_designates_its_verified_line_forward_and_never_backward`
  (`room_policy.rs`). **Declared, all failing closed (the message stays):**
  *(a) a line resolves only where this device holds an anchor for the name* —
  **narrowed 2026-09-20 to the cross-nest case, and permanent there**: the
  peer-anchor harvest now walks the room's own retired policy names as well as
  the member's thread rosters, so a same-nest owner or admin the succession
  ceremony absorbed before this member joined is anchored on the sweep's next
  pass and the parked records paint on the poll after (the rule, its four
  bounds and its pins: [`identity-succession.md`](identity-succession.md) § The
  succession statement → *a community policy's names join the harvest's walk*)
  — but a retired name homed on **another** nest stays unanchored (the
  harvest's cross-nest reach is the declined read, that doc's § Don't do
  these), so its records park for the session. **The room says so — in shared
  Rust (2026-09-20), painted on tui (2026-09-26):**
  `RoomSnapshot::moderation_unverified` holds when a record is parked on a
  name the harvest has **settled** for the session without anchoring (§ Roles
  and authorization → *Delete any message — the mechanism* → *Members verify
  what they paint* owns the rule); the backend derives it once
  (`FaunaMlsBackend::moderation_unverified`, off the parked set, each parked
  record's own waiting names (`RoomSeats::waiting`) cut to the anchored
  chain's, and the unseeded settles `settle_parked_successions` delivers —
  never off a harvest outcome, which that crate cannot see) and repaints only
  when it turns; pinned by
  `a_room_says_so_once_its_parked_moderation_rests_on_a_name_the_harvest_settled_without_an_anchor`
  (`fauna_mls_backend_tests.rs`: silent before the settle, said after it, the
  target shown throughout, cleared when a line verifies),
  `a_name_only_the_served_unanchored_version_carries_never_says_a_room_is_unverified`,
  `a_seeded_name_never_announces_the_room_as_unverified_before_the_record_paints`,
  `a_seed_after_a_settle_withdraws_the_rooms_unverified_statement_before_the_record_paints`,
  `a_record_parked_for_want_of_a_version_is_not_yet_whatever_an_earlier_records_names_did`
  (each record's own names) and
  `the_unverified_statement_repaints_on_a_turn_and_only_then`. **The paint closed
  this residual's last arm:** the user-approved element `thread-room-notice`
  ([`../ui/conversations.md`](../ui/conversations.md) § Element IDs), its
  state chosen once in shared Rust (`RoomSnapshot::notice`, which also carries
  a waiting room's `awaiting_key`), pinned on tui by
  `the_room_notice_is_absent_says_unverified_moderation_and_yields_to_awaiting_key`;
  the six other apps follow in the room trickle-down. **Journey gap, recorded:** no e2e reaches the notice — tui founds a
  community room since 2026-09-25, but no journey parks a floor delete behind
  a cross-nest retired name (below: no journey deletes in one yet);
  *(b)* a room founded before schema 68 whose founding version was
  already overwritten can never anchor; *(c)* a long chain costs one full
  roster read per unproven version, once per session — a range serve is the
  optimization; *(d)* **settled 2026-09-20 — two shapes, not one.** The
  member's anchor chain (`FaunaMlsBackend::room_policy_chains`) and its three
  neighbours — the verified succession lines (`room_seats`), a fetched-but-
  refused next version (`RoomSeats::pending`) and the harvest's settled set
  (`harvest_settled`) — are session memory and stay so: a relaunch re-proves
  the chain from the home nest at (c)'s cost (`floor_delete_verdict`: an empty
  map starts the walk at version 1, and the prefix a pass proves is cached
  back), bounded by (b) and by a `NotHeld` refusal, both restart-independent,
  and the lines and the settled set re-derive on the harvest's next pass at
  (a)'s. The **parked floor delete records** (`parked_floor_deletes`) were a
  real loss: the inbound walk advances the durable cursor past a record BEFORE
  judging it (`poll_inbound_conv`), the parked set was RAM only, and
  `retry_parked_floor_deletes` is the only thing that ever meets a parked
  record again — so a quit between the park and the pass that would have
  painted it left the target painted on that account for ever while every
  other member showed it deleted, and silently, since `moderation_unverified`
  is derived off the same set. The parked set now rides `history/<ch>`
  (`ChannelHistorySlice::parked_floor_deletes` — a claim awaiting its verdict,
  deliberately unlike `deleted_messages`, which is verdicts only; the record's
  bytes are public, so persisting the claim costs no secret; the at-rest rule:
  [`devices.md`](devices.md) § Cross-device MLS group-state sync → the
  `history/<ch>` contents bullet): stamped by the manager's one slice door,
  re-seeded by its restore, drained by the backend into its live set on the
  first pass over the room, and the notice returns with it once the harvest
  settles the name again. Weighed and declined: a durable low-water seq
  re-walked on restore (Rule-2 safe, but a record parked for the session —
  (a)'s cross-nest case — would re-walk the room from that seq on every
  relaunch, unbounded as the room grows, where the at-rest set is 64 small
  records per room and no fetch). Pinned by
  `a_floor_delete_record_parked_at_quit_is_judged_and_painted_after_the_relaunch`
  (`fauna_mls_backend_tests.rs`, red-verified with the restore re-seed
  removed) and the two `parked_floor_deletes` slice tests (`history.rs`);
  *(e)* **closed 2026-09-21** — `retain_policy_version` no longer overwrites a
  version's blob in place: an identical replay converges (the founding door is
  an upsert at version 1 and must), and a *differing* blob at a number already
  held is refused, rolling back the policy move that carried it. Nothing live
  could reach that conflict — both other writers compare-and-set on
  `expected_version`, and version 1 is owner-only with no admins, so no verdict
  could have turned — which is exactly why it is closed now rather than after a
  reader acquires the dependency. An already-anchored member was never at risk
  (it keeps the chain it proved first); the reader this protects is the FRESH
  session, which re-proves from whatever the nest serves then. Pinned by
  `db::rooms::tests::a_retained_policy_version_converges_on_a_replay_and_refuses_different_bytes`.
  **Every policy version is bound to its room (2026-09-22, § Roles and
  authorization → *A fetched version is anchored, never believed*).** Before
  this, only version 1 was: a later version was bound by nothing but its
  signer's rank, so a version an owner signed in another room they founded
  extended this room's chain, and a hostile home nest could serve one to rank
  a stranger admin here, whose floor delete then painted over a message the
  member already held.
  Now `RoomPolicy::sign_community` signs for a named room: beside the ordinary
  signature it writes the **room signature**
  (`SignedRoomPolicy::room_signature`, domain tag `TAG_ROOM_POLICY_IN_ROOM`
  over `room ‖ canonical(policy)`, additive and absent from the canonical
  bytes when absent, as `countersignature` is). The one judge
  (`judge_community_policy_step`) checks it through
  `SignedRoomPolicy::verify_community_for` under the room's `RoomBinding`, on
  both sides — the floor's `judge_policy_step` and a member's
  `CommunityPolicyChain` alike; every room requires the room signature above
  version 1, so the binding needs nothing but the room id. The current-policy
  decoder (`verified_room_policy`) refuses a missing room signature or one for
  another room too, but it cannot tell whether the version follows the chain —
  so nothing a device acts on comes from it alone. It only names the version
  to walk to: what the settings editor seeds its join rule and history policy
  from (`cache_floor`) and what every policy change is amended from
  (`read_room_policy`) is the version this device's own anchored chain holds
  there (`FaunaMlsBackend::anchored_room_policy`, the walk a floor delete
  judgment runs), never the bytes the floor serves as current. A version the
  owner signed in another room and the nest stripped, or one no rank in this
  room signed, reads as no policy and nothing is signed on it — pinned by
  `a_current_policy_the_device_has_not_anchored_is_neither_amended_nor_rendered`
  (`fauna_mls_backend_tests.rs`, red-verified by reading the served bytes
  again). **Declared, what remains:** a room that can never anchor *(b)* now
  renders no policy and cannot be amended; and the walk's cost *(c)* is paid
  at a room's first floor read of a session, not only at its first floor
  delete — a chain longer than one pass's fetch cap renders and amends once
  later reads have walked it. Serving the birth salt beside the current policy
  was weighed and not taken as the fix: it proves the current version's room
  but not that it follows the chain; it stays acceptable only as an additive
  complement. A room
  founded through `found_community_room` gets a binding birth salt
  (`binding_birth_salt`: the 8-byte `BINDING_SALT_MARK`, then 24 random bytes;
  `is_binding_birth_salt` reads it), and its birth record carries a room
  signature too. A salt without the mark founds no room: `verify_community_birth`
  refuses it, and so does the nest's `room.create` before it reads the record
  (the early compat-remnant audit's ruling 3, 2026-09-30 — an unmarked salt
  was the one lever left to mint a room that admits an unsigned version).
  Pinned by
  `a_version_signed_in_one_room_does_not_extend_another_rooms_chain` and
  `a_salt_without_the_binding_mark_founds_no_room`
  (`room_policy.rs`), `a_policy_version_signed_for_another_room_is_refused_by_the_floor`
  and `room_create_refuses_a_birth_salt_without_the_binding_mark`
  (`conformance_conversation_rooms.rs`), and the two splice arms of
  `a_community_floor_delete_is_honoured_only_under_a_policy_the_member_anchored`
  (`fauna_mls_backend_tests.rs` — a version the owner signed in another room,
  as signed and with its room signature stripped), each red-verified by
  removing the check it pins. **Declared:** *(f)* a build predating the room
  signature cannot change policy in any room — no room admits a version above
  1 without a room signature any more (the pre-binding room that once did, and
  failed open to a splice, was a remnant with no installation behind it,
  removed 2026-09-30 with the salt refusal above). **Three version skews,
  declared:** such a build's policy change is refused at the floor, as every
  current member would refuse it — it keeps everything else it could do in the
  room; an older *member* checks only the ordinary signature, so it stays
  exposed as before until it updates; and a nest predating the room signature
  verifies the ordinary signature and stores the blob whole, so a current
  build's policy change still lands there, but it does not require the room
  signature either — an older build's policy change, on a nest not yet
  updated, lands on its floor and stops every current member's chain at that
  version for the room's life (fail closed: the room's later floor deletes
  paint on no current member).
  No app journey witnesses the class: a community room can be founded from
  tui since 2026-09-25, but no journey deletes in one yet, and the six
  non-tui apps still gate the button on `is_own`.
  **Declared with the line re-ask (2026-09-23; *(g)*, the positive line
  kept for the session, closed the same day — every verified line is re-asked
  now):** *(h)* a record that really is a plain member's parks whenever a
  witness is registered — every line may yet grow — so a hostile home nest
  can fill a room's 64-record park with such records and evict a genuinely
  parked one oldest-first — **nil delta**: that nest can already withhold any
  floor delete record outright.

## The three classes

The class rule, the class table and the derivation are [`conversation-rooms.md`](conversation-rooms.md) § The three classes; what follows is the Community row's own detail, carried verbatim out of that section on 2026-09-10.

**Why a community class exists (decision 1, ratified).** Every platform at
scale ended with a bounded end-to-end class and an unbounded community class.
A community's content belongs to the community: joining one is a deliberate
audience choice, like an owner setting a folder's audience to public
([`../principles.md`](../principles.md) § The user always controls their data,
the one deliberate exception). The page says so where it matters — the class is
on the thread header, and the recipient picker says which class the new room
will be before the first message is sent.

**Community — the key model (settled at ratification on 2026-09-08 rather
than put to the user; refutable until the first community-room build, which
landed 2026-09-09 and confirmed all four reasons — § Implementation status
today).** The two candidates were the
home nest as an MLS member, or the R15 recipient-set scheme with the home nest
as a wrap recipient. The recipient-set scheme is ratified, for four reasons:

1. **It is what "the nest is a member with a key" means literally.** The scheme
   wraps a random room generation key to each member's reception key on every
   membership change; the home nest's room-read keypair is one more recipient
   ([`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
   § Audience: deployment infrastructure → *Room-read keypair*). The nest
   reads; it never mints — key authority stays with the room's owner and admins
   (§ Roles and authorization), so the nest's readable position is a **grant
   the room's members made**, revocable by rotating it out, exactly the shape
   of the materialization grant ([`../principles.md`](../principles.md) § The
   user always controls their data: readable derived views on the user's own
   nest, canonical planes sealed, revoke deletes the views).
2. **R15 already rules it for the case.** MLS roots messaging groups where
   transcripts are the asset and forward secrecy / post-compromise security
   earn their machinery ([`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)
   § The ratified decisions → R15). Against a member that reads everything by
   design, FS/PCS are near-valueless — and an MLS leaf on the nest would need
   an identity-class signing credential the nest deliberately does not hold
   (rule #7 of the key hierarchy), plus a nest-resident MLS engine on every
   commit of an unbounded group, which is the delivery-service coupling R15's
   rationale names.
3. **It keeps the at-rest invariant without a second carve-out.** A community
   room's canonical log rests sealed on every nest, like everything else; what
   is different is only who holds a wrap. A plaintext-at-the-home-nest design
   would have been a second exception to "user content rests sealed on every
   nest" with its own consent ceremony and a plaintext variant of every storage
   path — the storage-modes shape the no-modes ruling retired
   ([`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md)).
4. **History and removal fall out of it.** A newcomer's history slice is the
   retained generation bundle wrapped to them at admission (archival backfill,
   the subscription `KeyBlob` ancestor's own pattern); a removal rotates the
   generation, so a removed member who can still fetch ciphertext through a
   relay reads nothing new. Both are the scheme's existing moves, not new ones.

The scheme's mechanics — roster kind, lattice, membership witness — are owned
by [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
§ The recipient-set scheme and are reused, not restated; what a community room
adds is (a) the home nest as a **reader** recipient that is never in the
authority set, (b) the room's floor roster as the scheme's roster projection
the nest enforces (§ The floor roster), and (c) messages in the `conv` kind's
per-room scope, so a community room and an end-to-end room share one storage
shape and one read feed, differing only in which key the reader holds
([`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest →
*Receiving into the conversations view*).

**Who wrote it — the author signs, and every reader verifies (settled
2026-09-10 by the client-half build, as the key model
above was settled by the sealing build).** An end-to-end room's bubbles are
attributed by MLS: the framing carries the sender's leaf and the engine
authenticates it. **A community room has no MLS group**, and the key its
content seals under is one generation key *every member holds* — a key every
member holds authenticates nobody, so on its own the class would let any member
seal a message in any other member's name, indistinguishably. The channel log
cannot supply the missing half either: a `conv` record persists no sender by
design ([`./moderation.md`](moderation.md) § Implementation status today), and a
nest-asserted author would be an attribution no member could check.

So a community-room message is **authored before it is sealed**: the sealed
plaintext is the author's `(room, generation, author, sent_at_ms, body)` signed
under its own actor key (`fauna_mls::room_message`, the
[`./identity-succession.md`](identity-succession.md)-style injective tag
framing the room policy already uses), and every reader — each member, and the
home nest building its derived views — checks it before rendering or indexing a
word. Each signed field earns its place: the **room** because a member of two
rooms must not be able to lift a co-member's bubble from one into the other;
the **generation** because a member *holds* the key and could otherwise re-seal
a co-member's plaintext under a later generation and replay it past a rotation;
the **stamp** so a bubble carries the time its author gave it. The log `seq` is
deliberately *not* covered — the nest allocates it after the author signs.

**So this class's records are REPLAYABLE, and every order-dependent fold over
them must read the set rather than the sequence (ratified 2026-09-21).** The
uncovered `seq` is not an oversight to be fixed — it cannot be covered, because
the signature exists before the position does — so it is a property the readers
have to hold. Identical envelope bytes, re-appended by any member (the floor
admits any member's send and never reads it) or by the home nest that holds the
log, open and verify as their author's op again at a later position. No key, no
re-seal, no forged signature. Most bodies are indifferent: a text body makes a
duplicate bubble at its signed time, and a sender's own `Delete` is idempotent
because a tombstone only grows. A **reaction** is not — add and remove are
order-dependent — so a log-order fold let a re-appended `Add` bury the
retraction that followed it, restoring a member's reaction on every seat under
their own valid signature. The rule: a reaction folds per
`(target, reactor, emoji)` by the author's own signed `sent_at_ms`, with
`Remove` taking an equal stamp — which makes the outcome a **pure function of
the set of distinct signed ops**, so re-appending one the log already holds
cannot change it. Neither *"break the tie by `seq`"* nor *"by log position"*
would do: a replay always lands later, so on the equal stamp it shares with its
original either rule hands it the win, and the stamp is
millisecond-resolution — an add-then-retract inside one millisecond is
ordinary. The same stamped fold serves the end-to-end class, which cannot be
replayed at all (a consumed MLS ratchet key does not re-open a re-appended
ciphertext) and takes it for uniformity. Shared Rust:
`fauna_conversations::fold_reactions`; the end-to-end proof over real sealed
bytes is `a_replayed_reaction_op_cannot_override_a_later_retraction`
(`fauna_mls_backend_tests.rs`, red-verified against the fold reverted to log
order). **The standing obligation this leaves:** every future body folded off
this carriage with order-dependent semantics inherits the replay, and must say
how it orders — from something signed, or from nothing.

**A signature is not membership.** It establishes who wrote the bytes, never
that they were entitled to; the floor roster is the membership authority
(§ The floor roster), and a reader binds the two — a non-member's perfectly
well-signed message builds no view and renders no bubble. This is the same
split the end-to-end class draws between an MLS-authenticated sender and the
policy verdict on what that sender may do.

**Attachments — the second content kind (ratified 2026-09-10; refutable until a label build read one, as the key model was until
the sealing build — **closed the same day, unrefuted**: the bytes-reading label
build opens an attachment's plaintext under exactly this key, § Implementation
status today → *The bytes half lands*).** A community room's attachment bytes seal under a
**second per-kind content key off the same generation the message seals under**
— `fauna.conversations.room.attachment`
(`fauna_core::group_content::ROOM_ATTACHMENT_CONTENT_KIND`), the `blob_crypto`
shape with the generation id as AAD, exactly as the message kind
([`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
§ Audience: a storage group → *Per-kind content keys*, which owns the
derivation) — and the bytes are **inside the home nest's read**, for the
purposes below and no other. Everything else about an attachment is the
end-to-end class's, reused verbatim: a separate content-addressed blob named by
`sealed_cid` inside the sealed message, resting on the home nest (§ The home
nest → *Attachment bytes*), pinned by the plaintext `attachment_refs`
([`conversations-at-rest.md`](conversations-at-rest.md) § Encryption at rest →
*Attachment reachability*), rendered under the plaintext `blob_hash`. The one
field that means nothing here is `ChannelAttachment::epoch`: an end-to-end
attachment names the MLS epoch whose blob key opens it, a community attachment
opens under the **message's** generation — the one the `RoomSealed` envelope
names in cleartext and the author's signature binds — so the field is written
`0` and never read on this class.

Three things were decided, and the order matters:

1. **Whether the nest reads attachment bytes is settled by the class, not by
   a key choice.** The nest's wrap *is* the generation key, and a scope is
   only as narrow as its key material (`key-material-hierarchy.md` § Don't do
   these): any kind derived from the generation is nest-openable by
   construction, and so is any per-attachment key carried inside the sealed
   message body. The only way to seal a picture shut from the nest while its
   caption stays open would be a **second roster** — a recipient set that is
   "the members minus the nest", with its own mint, rotation, top-up and
   backfill — and a second member set is a second *class* riding inside the
   room. TP8 makes the class a pure function of one member set (§ The three
   classes), and the class's own rationale is that a community's content
   belongs to the community, a picture no less than a sentence. So the
   question this ruling was asked — does the read cover attachment bytes — has
   the class's answer: it does, because the nest is a member.
2. **A second kind, not the message kind's key.** Per-kind derivation is what
   the content-key module exists for — two kinds of one group never share a
   key, the precedent the room-restricted post ruling already set with its
   post kind beside the message kind ([`restricted-posts.md`](restricted-posts.md)
   § Encryption at rest → *Room-restricted*, ruling 4) — and an attachment is
   a different record from a message: raw bytes
   with no signed core of their own (authorship rides in the message that
   names the blob, whose signature covers the `sealed_cid`), content-addressed
   and convergent (two members attaching one file under one generation store
   one blob). Keeping the kinds apart costs nothing today and keeps a later,
   narrower position representable — a labeler holding the attachment kind
   and not the message kind; under one key it never would be.
3. **The read's purposes do not widen — their object is stated.** The list
   below covers *the message as its author sealed it*, attachment bytes
   included; no purpose is added or struck. *What the read covers*, below,
   says what that means per purpose.

What falls out unchanged: a newcomer's backfill wraps older generations, so the
attachments of the history slice open with the messages that name them; a
removal rotates the generation, so a removed member opens nothing attached
after it; every derived view is built under the tip (the paragraph below), for
attachments as for text; a legal-takedown withhold already answers 451 for
every blob a withheld record names ([`moderation.md`](moderation.md) § Legal
takedown); and `send_room_message`'s refusal of an attachment — the honest "not
yet" the client half shipped with — is retired by the build.

**What the home nest does with its read (the purposes — ratified 2026-09-10 by
the room-model design pass; the refutable
window closed unrefuted at the first label build the same day, as the key
model's did at the first sealing build — § Implementation status today).**
The readable position exists for the purposes that need a reader the members
are not
([`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)
§ Readable classes, the read-position purpose test) — **and for no other.**
The list below is the whole of the grant a member makes by joining a community
room; a purpose not on it is not authorized by the class, and becomes one only
by amending this list. Where a purpose *scores* content, its placement is
[`../architecture/content-scoring.md`](../architecture/content-scoring.md)'s —
§ The placement matrix's *capability-holder* row names this nest as the
position and bounds it (transparent models only, the grant's own authority
chooses them, output is metadata) — and this section owns only what the read
is *for*. Every derived view is built **at reception** from the tip's wrap
(the moment the nest already opens the bytes), lives on the home nest only, is
rebuilt from the sealed log, and is deleted — every table of it — when the
nest's wrap is rotated out.

1. **Search** (BUILT — the paragraphs after this list). The index the class
   exists for: a member holds a vanishing fraction of an unbounded log.
2. **Labels** (ratified here; BUILT 2026-09-10 — § Implementation status
   today). The
   moderation signal for an unbounded log — the same argument as search, plus
   the timing rule (`content-scoring.md` § Timing: a presentation-gating label
   must exist before a member first sees the message, which a member's own
   client cannot guarantee for a log it has not fetched). The home nest applies
   the **transparent** labelers the **room names** — an owner/admin-signed
   record beside the policy (`fauna_mls::room_policy::SignedRoomLabelers`,
   § Architectural rules, rule 6: a record of its own rather than a policy
   field, so a policy change never resets it and no app that predates it has
   to verify it), the room-level twin of a user's own labeler subscription
   ([`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md)
   § Tier-3 community models); no entry, no labels — to each message in the act
   that indexes it, and serves the result as metadata beside the message,
   rendered as the badge every app already paints. **What a room may name** is
   an artifact its home nest's labeler registry publishes as `wasm` or
   `text-model` — both transparent by construction, both able to score a
   message. A `list` is keyed by post id and has nothing to say about one, and
   an id the registry does not hold names nothing transparent: a user's own
   model is sealed under its owner's key and is never published there. **Enforcement is each
   member's own threshold** ([`moderation.md`](moderation.md) § Categories &
   enforcement item 1): a label never removes, and the room's floor acts —
   remove, demote, an owner's or admin's delete, legal takedown — are
   ciphertext acts that need no read. A
   member's own post-decrypt detections stay private to that member, as
   everywhere; a tier-1 per-user model is never a room labeler. The deployment
   imposes nothing here — its compulsory surfaces stay the three
   `moderation.md` enumerates.
3. **Room-restricted posts** addressed to a community that is also an
   audience ([`restricted-posts.md`](restricted-posts.md) § Encryption at rest, the
   *Room-restricted* class — ruled 2026-09-10, the owner of the post's shape,
   seal and wire evolution). What this purpose authorizes is narrow: the home
   nest opens a room-addressed post **it stores** — the author or a follower
   homed there — under the tip's wrap in the act that ingests it, and derives
   the same kinds of view as for a message (index positions, labels), under
   the same rule. **No post travels to the home nest for it**: such a post is
   its author's ordinary post, never a room message, so a home nest that
   stores none derives none. The search half BUILT 2026-09-10 (the mechanics
   are `restricted-posts.md` § Encryption at rest → *Room-restricted — the ruling*
   and its *Built* paragraph); its labels landed later the same day, served
   through a floor-gated read of their own (§ Implementation status today →
   *Labelled too*).

**Struck from the list (2026-09-10): "the unbounded fan-out."** Delivery never
needed the read — the routing roster is the fan-out mechanism the nest mutates
without any key (§ The floor roster), and a community message fans out exactly
as an end-to-end one does, envelope verbatim. The reading of that clause that
*would* need the read — telling a member of an unbounded log **which** messages
address them (mentions, replies) without holding it all — is a genuine
purpose-shaped need, **not authorized by this pass**: it needs a mention facet
on the message body that does not exist today, and it joins the list only by
amendment here, already bound by the rule below (positions, never previews or
digests).

**What the read covers (ratified 2026-09-10 with the attachment kind, above):
the message as its author sealed it — text, caption and attachment bytes alike
— because the class, not a key choice, decides what the nest opens.** Per
purpose, and without adding one: **search** indexes what a member *typed* —
the text or the caption — and never derives from attachment bytes (no OCR, no
transcript, no perceptual hash in the corpus; an index over attachment bytes
is a new source for the purpose and joins only by amendment here); **labels**
see the whole message, attachment bytes included — an image classifier that
could not see the image would make the moderation signal worthless for exactly
the content it most exists for — and serve, as ever, metadata beside the
message; **room-restricted posts** carry their own attachments under the post's
own seal, which the Posts row owns. The build state of each is the purpose's
own (§ Implementation status today).

**Forbidden, stated once, for every purpose present or future:** a derived
view that carries text (snippets, previews, digests, summaries) or bytes (a
thumbnail, a crop, a transcript or OCR text of an attachment — the picture's
snippet); any per-member profiling from the read; any use of the read as an authority (the
nest still mints and rotates nothing); and any reader but the home nest (a
relay holds ciphertext only — a member homed elsewhere receives the verdicts
it is served through its own nest, which forwards them beside the envelope
and stores none, so the relay reads nothing and derives nothing). The
consent surface for this list is the class
statement the recipient picker and the thread header show
([`../ui/conversations.md`](../ui/conversations.md) § Element IDs →
`recipient-picker-class`, which owns the copy): it must name the purposes in
words — searched and labelled by the home nest — not only the class.

**A derived view is only a purpose served once a member can reach it, and the
search index reaches them through `fauna.conversations.room.search`.** Why
this search is the nest's work and not the member's device's is the class's
own unboundedness: a member holds whatever slice of the log it has fetched,
which for a community may be a vanishing fraction, so searching the room is
exactly the "purpose that needs a reader the members are not". Every other
class searches client-side against the `__index` segments
([`content-index.md`](content-index.md) is the
engine's owner), and this door refuses them, because for them the members
**are** the readers.

⚠ **The door answers *where*, never *what* — and only *where* the caller could
have read anyway. That is a rule, not an economy.** A hit carries the
message's log position and a relevance rank and no text. The nest indexes
under the **tip**, while a member holds a wrap only for the generations minted
while it sat on the floor: a member seated after the current mint holds no
wrap for the tip and cannot read what is said until the next mint covers it
(§ Implementation status today — the admission/mint race the member top-up
heals), and a member seated under `history_policy: none` holds no wrap for
anything said before its admission (§ History for joiners). A snippet would
hand such a member precisely the plaintext the sealing plane withholds from
it, with the nest's own read as the leak.

**A position is an answer too, so the same reasoning binds it** — and that is
the rule's second half, stated after the first half was built without it
(2026-09-11). A term the caller chooses, answered by *where*, is a *what*
oracle over exactly the history the room's own policy withholds: *does any
message before I joined contain this name, this word, this URL?* — asked one
probe at a time and answered with positions and ranks. So: **a hit is served
only for a position the caller could open.** The door filters every hit
against the wraps the caller's own roster entry holds, before the reply; the
member then takes the positions it was served, fetches the sealed bytes
through `fauna.conversations.channel.fetch`, and opens them with the wrap it
holds. A member given the room's history under `full` holds those generations
and is served those hits; a member seated under `none` sees the room from its
admission here exactly as everywhere else; a member with no wrap for a
generation is not told that generation exists. The filter is by **wrap
coverage, never by time** — a member removed and re-admitted searches only
what its new roster entry holds wraps for; its earlier tenure's positions
return only if an existing member backfills them to the new entry under
`full`.

The same reasoning binds every future purpose of the read: **what the nest
derives it may serve as metadata; what a member may *read* — and what it may
be told the *position* of — is decided by the wrap it holds and never by the
nest's own position.**
