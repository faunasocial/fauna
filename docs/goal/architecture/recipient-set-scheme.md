# The recipient-set scheme — target state

Owns: recipient-set-scheme
Status: ratified — T20, the storage-group keying build design, ratified 2026-08-17 and refutable until the first group-scope build; the build began the same day, and § Implementation status today records which of its mechanics are code
Authority: **how a storage group scope is keyed and who is in it** — R15's storage-group keying from ruling to buildable mechanics: scope birth and the authority seam, the roster kind (the per-writer roster cell, its authored `Removed`, the bound `Enrolled` entry), generations re-targeted onto the R14 machinery, the mint triggers, severance per axis (member removal, a member's fleet severance, an authority device's removal and the revocation kind), the membership witness, the concurrent-membership lattice, and the constraint discharges — with the build-out of each. **NOT owned here** — the classes of account data, the audience ladder (R13), the generation machinery (R14) and the export-confidentiality axis → [`account-data-taxonomy.md`](account-data-taxonomy.md); the key-material half (the group generation key, the group-reception keypair, the wrap format, the per-kind derivations) → [`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: a storage group; the ceremony that births a scope → [`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation; the rooms keyed on it → [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) and [`../behavior/community-rooms.md`](../behavior/community-rooms.md); the ratified decisions (R15 among them) and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-28 (split verbatim; each ruling carries its own ratification date below)

Split verbatim out of [`account-data-taxonomy.md`](account-data-taxonomy.md) on 2026-09-28, when that doc stood 4 days from the 262,144 B whole-file read ceiling (228,586 B). Its `Owns:` line already named this concept beside the taxonomy itself, and the scheme — its rule and its build-out record together — carried about two-thirds of that doc's weekly growth, so the concept is the seam. The section moved whole with its status entries; a routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-taxonomy.md`, and before 2026-09-06 `docs/goal/architecture/account-data-plane.md`.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-taxonomy.md`](account-data-taxonomy.md), so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name in [`account-data-taxonomy.md`](account-data-taxonomy.md) first (the classes, the audience ladder, the generation machinery), then in the rest of its family as that doc's own reading note lists it — [`account-data-plane.md`](account-data-plane.md), [`account-sync-plane.md`](account-sync-plane.md) (§ Ordering model among them), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md). Positional words (“above”, “below”) inside the carried text point within this doc: the section moved whole, and the one boundary-crossing deictic (“the R14 generation machinery (previous subsection)”) was made a link before the move. The `W<n>`, `R<n>` and `T<n>` labels are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

## Section map

- **[The recipient-set scheme](#the-recipient-set-scheme-t20-build-design--ratified-2026-08-17-refutable-until-the-first-group-scope-build)** — the T20 build design, verbatim: scope birth and the authority seam, the roster kind, generations on the R14 machinery, the mint triggers, severance per axis, the membership witness, the lattice, the constraint discharges. **The heading is unchanged from `account-data-taxonomy.md`**, so a `§ The recipient-set scheme` citation resolves by swapping the filename.
- **[Implementation status today](#implementation-status-today)** — the scheme's build-out entries, carried with the rules they build.

## The recipient-set scheme (T20 build design — ratified 2026-08-17, refutable until the first group-scope build)

R15's storage-group keying, from ruling to buildable mechanics. The scheme is
a **generalization of two shipped/ratified patterns, never a third invention**:
the subscription `KeyBlob` scheme
([`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: an
opaque set of subscriber pubkeys) is its production X25519 ancestor — random
period key, per-recipient wraps, rotate-on-removal, archival backfill — and
the R14 generation machinery ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery) supplies the arbiter-free
lattice the `KeyBlob` scheme lacks (its `stale_rotation` monotonicity check is
nest-side — exactly the dependency constraint (a) forbids here). The
**key-material half** — the group generation key, the per-account
group-reception X-Wing keypair, wrap format, per-kind derivations — is owned
by [`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: a
storage group, never restated here. This section owns the **roster**, the
**authority seam**, the **lattice**, the **membership witness**, and the
**constraint discharges**. Consumer: offline share initiation
([`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation) — the
ceremony itself is owned there.

- **Scope birth + the authority seam.** A storage group scope is minted by an
  **initiating account**; the scope id is content-derived from its birth
  record (authority actor key + salt — the R14 key↔id commitment pattern). So a birth row is a scope's only if it re-derives to that scope's id: every reader of a stored birth row and every plane path that lands one (adopt, the walk, the own write) holds it to that, one shared door (`fauna_core::group_scope::decode_birth_for_scope`), and a row that is another scope's roots no authority and is never kept — the birth kind is `Immutable`, so a first-contact forgery a root holder sealed would otherwise stand for good.
  Membership authority in v1 is the **authority set = the initiator's device
  fleet**: roster entries are valid iff device-signed with an authoring chain
  to the authority actor root ([`../behavior/devices.md`](../behavior/devices.md)
  § Device-signed authoring — the shipped chain machinery, reused). The
  authority set is a deliberate seam: **T19 reuses this scheme with authority
  set = the box's admin set** (admin read reach = wraps over the admin set),
  which is why T19 and T20 were designed aware of each other.
  Multi-authority groups (co-owned scopes) are named out of v1.
  **The seam's first BUILT filling is a third one (2026-09-09): a community
  conversation room, whose authority set is its floor roster's owner and
  admins** ([`../behavior/community-rooms.md`](../behavior/community-rooms.md)
  § The three classes → *Community*, which owns the room plane's use of this
  scheme). Its roster is nest-side and its mints are actor-signed rather than
  device-signed with a chain, because the floor roster is that plane's
  membership authority — so the seam is filled, not stretched: what varies
  is exactly the authority set the bullet declares variable, and the
  generation, wrap, coverage and severance mechanics below are reused
  unchanged. The v1 *storage-group scope* — birth record, plane-data roster,
  machinery root, the two-party offline-share ceremony that births and
  delivers it — **is built and shipped** (2026-08-17 onward;
  [`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation owns
  the shipped surfaces; the "still unbuilt" this line carried until
  2026-09-16 was stale), so the section is refutable only by a ruling, and
  every evolution of its records is additive (the at-rest rows are alpha
  data). What is NOT yet built: any roster writer beyond the deliver (no
  production `Removed` writer, no re-mint trigger, no authority-device
  revocation publisher, no group-plane pump leg)
  — scopes today are two-party and written once, and they reach a reader
  only through ceremony adoption: there is no home-nest route for a group
  row, by the roster bullet's own "no nest write" rule.
- **The roster kind** — one cell per (entry, authoring device), value: the
  member's actor key, the reception pubkey observed at add,
  `Enrolled | Removed`, stamps. **Merge is the device-set lattice
  transplanted per writer: within a cell monotone, `Removed` absorbing;
  across writers the reader folds the cells of one entry under the
  authority line** (*The per-writer roster cell*, below); re-admission of a
  removed member is a fresh entry id, so add-wins resurrection is
  unrepresentable. The roster is plane data in the
  scope — per-writer logs, frontier merge, **no nest write and no arbiter on
  any path** (constraint (a) discharged structurally). **The bound roster
  entry (ruled 2026-09-16; owner
  of the binding between an entry and its wrap target).** The device-set
  bullet's *The self-signed enrollment* closed this shape on the account
  plane; here `authority_sig`'s preimage was the entry id alone, so the
  reception key beside it was unsigned and any group-plane key holder could
  re-file a member's authority-signed entry beside its OWN reception key,
  win the byte-order join, and have every later group mint and top-up seal
  to itself under the member's name (plus the permanent partition the
  account-plane ruling describes). **The binding is the authority
  device's**, not the member's: an additive `binding_sig` by the entry's
  authority device (the same key as `authority_sig`) over a fixed-width
  domain-tagged preimage of the entry id, the stamp and the length-prefixed
  reception key (`fauna_core::group_scope::roster_binding_signing_bytes`),
  built only by `sign_roster_enrollment`. Why the authority and not a
  member self-signature: the member's key already reaches the authority
  **member-signed** in the ceremony's accept (`GroupShareAccept.
  reception_published`, verified before any entry is written), so the
  authority binds a key its member vouched for; whereas carrying the
  member's own publication in the row (the other candidate) would bind
  neither scope nor entry id nor freshness — a stale publication replays
  to re-bind a member to a superseded key whose secret a removed device of
  that member's fleet may still hold, defeating the severance bullet
  below. Forging the binding needs the authority device's secret, which no
  non-authority plane-key holder has — and a device REMOVED from the
  authority's own account still has it, which is why "the authority
  device's" is read against the authority line's revocations (*Severance,
  per axis* → *An authority device's removal*), never the chain alone.
  **Join and readers:** the join ranks
  (`Removed` absorbs, binding self-verifies under the record's own embedded
  cert, bytes) — self-consistency is all a merge may check, so an attacker
  can make a row self-consistent under a cert of its own, but such a row
  fails the authority chain at every reader: availability vandalism of the
  pre-existing class, never confidentiality (the class, stated in full and
  closed at the cell: *The per-writer roster cell*, below); the entry id is
  recomputed from the record's core, so the join needs no cell key. Both
  readers — the
  merged-roster view and the admission witness door — share one verifier,
  which **requires the binding**: an entry that carries a binding and fails
  it is refused (never demoted to the unbound shape), and — **ruled
  2026-09-25 (the compat-remnant sweep, program 4)** — an entry with no
  binding is refused outright too. The unbound shape is what pre-ruling
  binaries wrote; no writer has produced it since the ruling (every
  production writer — the deliver and the severance's re-admissions — and
  every fixture builds through `sign_roster_enrollment`), no pre-ruling
  entry exists (`compat-remnant-sweep.md` § Program 4), and
  the legacy re-bind below re-published nothing — so its only producer left
  is the re-file forgery the binding exists to stop, and chain-verifying it
  was an accept path kept for a binary that does not exist. Refusing it at
  both readers, not only outranking it at the join, also closes the window
  the join alone left: a replica that adopted the forgery before the
  authority's bound row reached it would have enrolled the attacker's key
  as a wrap target until the merge. **First contact stays permissive about
  the binding** — the device-set twin's shape: a two-phase lattice's
  adoption checks no signature, since the join outranks and the verifier
  refuses a non-verifying row. **Additive posture (unchanged):** the field
  is `#[serde(default)]` and skipped when empty, no `deny_unknown_fields` —
  the serde upgrade mechanism, not an accept path: an unbound row still
  decodes, so it stays a lattice element the join ranks lowest and a
  removal absorbs — and the join returns the winning input verbatim and
  still ranks a self-verifying binding above bytes, so a forgery never
  displaces the authority's own row at rest (the row the readers would then
  have refused: availability vandalism, closed at the join). **The legacy
  re-bind is retired (2026-09-25, the compat-remnant sweep).** It
  re-published, bound, every entry an authority had written before this
  ruling; no such entry exists (`compat-remnant-sweep.md` § Program
  4), and every production roster writer — the deliver and the
  severance's re-admissions — builds through `sign_roster_enrollment`, so
  the pass could bind nothing: it skipped every cell whose retained
  snapshot row is itself bound, which is every row a current ceremony
  retains. **Its source rule stands for every writer that re-publishes a
  member: the re-published row is NEVER the merged one.** A forged cell
  carries the authority device's carriage and `authority_sig` verbatim
  (both cover the entry id alone, never the reception key), so an
  authority that bound *those* bytes would hand the attacker the one entry
  it cannot forge — the very escalation the binding exists to close. The
  source is therefore this account's own retained attestation of what it
  delivered, the signed deliver envelope kept verbatim at
  `InitiatedGroupShare::deliver`, verified under the authority actor's own
  signature before a byte of it is read. **The per-writer roster cell
  (ruled and built 2026-09-27; owner of the cell shape, of
  who may write a `Removed`, and of the fold).** Every displacement finding
  on this kind — the re-file beside an attacker's key (closed by the
  binding), the pre-squat of a derivable re-admission cell (closed by the
  random salt), the revoked device's re-bind of the cell the last severance
  recorded, paid off at one re-admission, one `Removed`, one mint and one
  record core per round — was one design fact: **a roster cell shared
  between writers, so the join had to keep one writer's bytes and drop
  another's, and could consult nothing but the two rows.** Three vehicles
  followed from it under the shape the build had until 2026-09-27, which
  *Join and readers* above accepted as "availability vandalism of the
  pre-existing class" without stating them: a self-consistent `Enrolled`
  under the attacker's own cert tied the genuine row on rank and won on
  bytes at a chosen stamp; a `Removed` was unsigned and **honored
  unconditionally** by every reader, so any machinery-root holder — every
  member ever admitted, of any account, and every device the authority ever
  certified — severed any member with one row, which no pass repaired; and
  a revoked authority device's root-chained re-bind, the one vehicle the
  pass did repair. The transplant's reason for an unconditional
  `Removed` does not hold on this plane, for the reason *Severance, per
  axis* → *An authority device's removal* rule (1) already gives: there
  every key holder is the user's own device, here the key is every
  ever-admitted member's. So the cell takes the shape this plane's own
  revocation kind and the account plane's per-healer top-up cell already
  have — **one cell per (entry, author): logical key
  `<entry-id-hex>/<author-device-hex>`**, the author being the device the
  row's own embedded carriage names. The join is **key-aware**, the
  revocation kind's shape — rank = (verifies at THIS cell: the recomputed
  entry id and the own carriage's device echo the two segments and the row
  self-verifies, `Removed`, bytes), total over adopted junk — and the roster
  arm's first-contact strictness refuses a row not verifying at its own
  recomputed cell (the revocation kind's `verifies_at` contract): only the
  named device can write a row that counts in its cell, so **no writer's row
  is ever displaced by another writer's bytes**, and the byte tie-break
  never crosses principals. (Refuted at build, as the ruling allowed: it
  first wrote the join key-free, self-verification alone ranking, on the
  argument that the cell is the writer's own — but a merge into a STANDING
  cell runs the join, not the adoption arm, and a self-verifying row from
  another writer's cell won a standing cell on bytes; the key is the one
  fact the two rows do not carry.) `Removed` becomes
  **authored** — the same carriage, and a signature over the entry id and
  the stamp — and, exactly like a revocation, counts only on the authority
  chain: the reader's fold per entry id is *excluded* iff some `Removed`
  cell verifies under a device cert chaining to the authority root (the
  remover's own standing not consulted — *A removal survives its author's
  revocation*, below), else *enrolled* iff some `Enrolled` cell verifies
  under a device the line does not refuse. The fold is a pure function of
  the merged cells and the line, so two replicas that merge the same rows
  in opposite orders fold alike; the `FleetView` non-monotonicity argument
  concerned a precondition evaluated against merged state at ingest and
  stored, which this fold never does, and this reader already consults the
  line for every `Enrolled`. What it closes: every cross-writer
  displacement, the re-squat included — a revoked device's row lives in its
  own dead cell, refused by every reader that has the line and displacing
  nothing. What remains is exactly the bound *Severance, per axis* states:
  the staleness window, and the root secret. **Consequences, each owned
  where the sentence it retires lives:** an authority-device severance
  re-publishes every entry the line leaves without an honored `Enrolled`
  **at the same entry id, under the live device's own cell**, from the
  ceremony snapshot's member-signed key, and writes no `Removed` (nothing is
  left to sever — the dead cell severs itself at every reader with the
  line); the fresh-id re-admission, the recorded-cell source and
  `UserConfig::group_readmissions` retired with the shared cell
  (*Severance, per axis* → *The re-admission source*, superseded there);
  the member-removal writer's strike becomes structural (an honored
  `Removed` at the entry is what the pass reads, so it never re-admits a
  removed member), and trigger (2)'s rotated key lands in the ceremony
  record's snapshot, the one retained source left; and the severance mint's
  trigger became the line-committed admissibility *Mint triggers* rules,
  since no `Removed` marks the severance. **Compat posture:** the cell key
  and the `Removed` shape were changed in place, not migrated — at-rest and
  wire facts of a kind no installation carried under the 2026-09-24
  baseline ([`version-compatibility.md`](version-compatibility.md)
  § Dimension 2, the fourth exception), the repo still private at the
  build. **A removal survives its author's revocation (ruled and built
  2026-09-27; owner of the
  remover's standing).** The build surfaced the consequence of folding a
  `Removed` under the line while rule (3) makes a revocation total: a member
  removal authored by a device the authority later revokes would stop
  excluding at every reader the moment the revocation merged, and the
  member's other honored `Enrolled` cells would stand again — a member the
  authority deliberately removed regaining every later generation because
  an unrelated device was retired, with no pass seeing anything to repair.
  So a `Removed` is ruled a **stop-trusting signal, the revocation's own
  class, and *Severance, per axis* rule (2) extends to it**: it is honored
  on its chain alone (root or a device cert chaining to one, expiry anchored
  to its stamp) and the remover's own standing is never consulted, so it
  outlives its author's revocation and exclusion is monotone under a
  growing line; every *positive* signal — an `Enrolled`, a binding, a mint,
  a top-up's authority side — stays refused under a revoked author, rule
  (3) unchanged. The vandal class this admits, a compromised authority
  device severing members before its revocation merges, is exactly the one
  rule (2) already accepts for revocations, with the same backstop: the
  root revokes the device, and the member is re-admitted under a fresh
  entry id (the member-add writer's job, its entry landing in the retained
  snapshot). The alternative — a live device re-publishing every removal
  the line was about to un-honor, under its own cell, before its
  revocations — was refused: it only moved the resurrection into a
  staleness window at every reader and made exclusion flip, and bought
  nothing on the compromised-device axis, where the removal is honored
  until the revocation either way.
- **Generations ride the R14 machinery, re-targeted.** Mints are `Minted`
  rows with content-derived generation ids, minter signatures
  (authority-device-signed), inline **member wraps** (the group key sealed to
  each Enrolled member's *current* reception key), descendant-retires-ancestor,
  byte-order-max winner among concurrent candidates, member-authored top-up
  rows for unkeyable Enrolled members, and the target-signed "cannot key"
  self-heal signal — all as the R14 subsection rules them, with two deltas:
  **(i)** wrap targets are member reception keys (cross-account), not device
  KEM keys; **(ii)** admissibility's escrow-ack arm is replaced by
  **roster coverage** (a mint is admissible when its wrap set + merged
  top-ups cover the merged Enrolled roster) — there is **no separate group
  escrow door**: durability is each member's own account custody of their
  reception secret plus their held wraps, so the group is recoverable while
  any member's account is.
- **Mint triggers — removes mint, adds never do.** (1) Any `Removed` merge
  (the severance mint); (2) a member's published reception-key rotation
  (their fleet-severance signal, below); (3) authority-chosen refresh. An
  **add** wraps the **retained generation bundle** — tip *and* the
  generations still covering live content — to the new member (the storage
  twin of the subscription scheme's archival backfill): forward secrecy
  against new members is deliberately not bought (R15's rationale — FS/PCS
  for an at-rest archive whose members retain plaintext is near-valueless).
  **Trigger (1) is a fact about MERGED STATE, never about what a pass
  happened to write (ruled 2026-09-21).** The severance mint is owed by a scope exactly while its
  merged roster holds a `Removed`, lists at least one verified member, and
  resolves **no admissible mint** — the resolver's own predicate with
  observer-keyability taken out of it (both coverage arms, authorship under
  the authority line *as the pass will leave it*), which is the same `None`
  that resolver already names the mint trigger. **Line-committed
  admissibility (ruled and built 2026-09-27 with *The roster kind* → *The
  per-writer roster cell*):** under the per-writer
  cell an authority-device severance writes no `Removed`, so the debt needs
  a second arm that is likewise a fact about merged state — a mint's core
  carries the sorted set of authority devices it was minted past
  (`revoked_past`, inside the content-derived id, so the minter signs it
  and nobody edits it), and a mint is admissible only if that set covers
  the reader's merged authority line *as the pass will leave it*. A
  generation minted before a revocation names a smaller set, resolves
  inadmissible at every reader with the line, and is the debt the next pass
  pays with a mint past it; the set only grows, so the predicate is
  monotone and needs no order. Authorship under the line stays a separate
  conjunct (a revoked device may list itself). Trigger (1) then reads: a
  `Removed` merge, or a line the tip was not minted past; adds still never
  mint. A writer that instead
  minted "because this pass wrote a `Removed`" loses the debt to a mint that
  errors or a crash before it: the stale cells are already `Removed`,
  nothing is left to sever, no later pass mints, and the revocation that
  follows strands every member with no tip **and never keys the compromised
  device out** — the re-key is trigger (1)'s whole point on that axis. Read
  as state the debt survives both, a scope is entered on it alone (no
  revocation owed, no stale cell), and it is stable: the mint the pass
  writes wraps the whole verified roster, so it is itself the admissible
  mint that ends the debt, and a healed scope never mints again. The
  `Removed` conjunct is what keeps "adds never mint" true **of a scope that
  never severed** — it is not even resolved, so an add's entry that reaches
  a replica ahead of its top-up triggers nothing. **In a once-severed scope
  the same race may cost one generation, and that is ratified, not refined
  away (ruled 2026-09-21):** `Removed` is absorbing, so the
  scope is resolved for ever after, and an add's entry that outruns its
  top-up reads as a coverage failure, hence a debt, hence a mint — at most
  one per authority device that sees the gap, per such add, written by the
  authority's severance pass and never by the add act itself (an add door
  still cannot move the tip). It is harmless:
  the mint wraps verified members only, the new one among them, and the
  add's top-up still carries the retained bundle. The two refinements are
  refused. *Dropping roster coverage from the debt* would let an ancestor
  mint that lists only the still-enrolled entries satisfy it while every
  reader, running the full predicate, resolves no tip — a stranded scope.
  *Excusing an entry "newer than the last severance"* needs an order the
  plane does not have: stamps are advisory and there is no arbiter (rule
  (3) below). The add writer, when it lands, inherits this sentence as
  written. Two live authority devices
  may each see the debt before either mint reaches the other and mint once
  each; that is the concurrent-candidates case the byte-order-max winner
  already rules, bounded at one mint per device per severance, and harmless.
  **A failed mint does NOT hold back the revocations:** they are the
  security-critical write, the no-tip gap is the fail-safe direction, and
  with the debt read as state the gap is transient — the next pass pays it.
- **Severance, per axis (constraint (c) discharged).** *Member removal:*
  severed from every generation minted after the remove merges; keeps what
  it could already open (the universal rule-6 bound). *A member's device
  removal:* the member's own R14 fleet mint rotates their account
  generations **and re-seals their reception secret**, the member publishes
  a fresh reception public key, and trigger (2) re-mints the group — so a
  removed/stolen device that once held the reception secret is severed from
  future group generations. The wrap target is therefore **never a static
  per-account key** — it is the member's *current* reception key, rotated on
  the member's own fleet events; that rotation coupling is exactly what
  closes the rule-6 hole constraint (c) named. ***An authority device's
  removal* (ruled 2026-09-19; owner of how a group-plane reader learns
  it).** The two axes above sever a *reader*; this one severs an *author*.
  Every authority check on the plane — a roster entry, its binding, a mint
  — accepted any device whose cert chains to the authority root, and a
  device removed from the authority's own account keeps its device secret,
  its root-signed cert and the never-rotated machinery root: enough to
  enrol an actor it controls, to bind a member's cell to its own reception
  key, and to mint. The account plane closes the same shape by `FleetView`
  excluding removed ids; but that plane is sealed to the authority's own
  fleet, so a cross-account member can never read it, and the group plane
  has no nest to ask (the roster bullet's "no nest write and no arbiter on
  any path" — which is also why routing group ingest through the home nest
  was refused as a ruling: it would contradict that bullet and the
  pull-only plane, and keeping the witness door closed is only ever a
  temporary guard, since the membership witness below is target state).
  Rotating the machinery root was refused too: `key-material-hierarchy.md`
  rules it never rotated, and no verifier reads it, so it would sever
  nothing. **So the fact is published INTO the group plane**, as the kind
  `fauna.group.authority-revocation` — one cell per *(revoked device,
  revoker)*, machinery-root sealed, no removing phase (the revoked set only
  grows) — and **every authority check reads it through one implementation**
  (`fauna_core::group_scope::GroupAuthority::verify_device_cert`: chain,
  expiry, not learned revoked), behind the merged-roster view, the
  membership witness, the mint resolver and an authored shred alike; the
  authority line is one value, so no reader can be built without a
  revocation input and no two can answer to different authorities. Three
  rules carry it. **(1) Authored, never unconditional** — the one
  deliberate departure from the device-set lattice this scheme otherwise
  transplants: there an unconditional `Removed` is fail-safe because every
  plane-key holder is the user's own device, whereas here the plane key is
  held by every member ever admitted, of other accounts, so an
  unconditional revocation would hand any former member the group's
  authority to kill. A revocation counts only when signed by the authority
  root (or a prior identity — the root ceremony's voice on this plane) or
  by a device whose cert chains to one. **(2) The revoker's own standing is
  not consulted** — "was the revoker still un-revoked?" reads non-monotone
  merged state and diverges in a mutual-revocation race (the `FleetView`
  ruling's first reason, unchanged), so a race revokes both; the vandal set
  is thereby exactly the account plane's accepted one, the authority
  account's own ever-certified devices, with the same backstop (the root
  revokes; a fresh device re-publishes) and, past that, a fresh scope.
  **(3) A revocation is total, never dated** — there is no arbiter, so
  there is no "before the revocation": stamps are advisory and the revoked
  device chooses its own, so `devices.md`'s authored-while-authorized rule
  (which rests on the home nest ordering ingest) does not transplant, and
  every signature of a revoked device is refused whenever it claims to
  have been made — every *positive* signal, that is: the two stop-trusting
  signals, a revocation it authored (rule (2)) and a roster `Removed` it
  authored (*The roster kind* → *The per-writer roster cell* → *A removal
  survives its author's revocation*), are honored on their chain alone and
  outlive it. What the authority still vouches for, a live authority
  device re-publishes under its own carriage: each entry the line leaves
  without an honored `Enrolled` re-published at the same entry id in the
  live device's own cell, and no `Removed` written — the revoked device's
  cell is dead on its own at every reader with the line (*The roster kind*
  → *The per-writer roster cell*; before 2026-09-27 the shared cell forced a
  fresh entry id plus a `Removed` on the stale cell, since the join ranked
  two self-verifying rows by bytes and could keep the revoked device's).
  The line the tip was not minted past is mint trigger (1)'s second arm
  (*Mint triggers*), and so re-keys the
  group past the revoked device, as an authority-device compromise
  deserves; until that lands at a reader the scope resolves no tip there —
  the gate, the fail-safe direction. **A device the line refuses publishes
  its revocations and nothing else (ruled 2026-09-21).** "Live" above is
  load-bearing: a device the authority line *as its pass will leave it*
  revokes — it merged its own fleet `Removed`, or a sibling's revocation of
  it — still owes every revocation rule (2) lets it sign, its own included,
  and writes those alone, asking its account's retained record for nothing.
  A re-admission under its carriage can never verify, so the entry would be
  stale again at its next pass and re-admitted again, a dead cell per pass;
  a mint it authors is
  inadmissible by construction, so the debt it tried to pay would stand and
  be paid again every pass. Both are a live sibling's to write. This is a
  defence, not the primary control: an ended device's data path already
  dies with its writer-key tombstone, before the plane `Removed` exists
  ([`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal → *Principal succession after a device delete* — the
  ended device stays ended, and the machine returns only as a successor
  with a fresh key, which no line revokes). The guard covers what that
  control does not reach: a revocation that arrives on the group line
  alone, and any future carriage of this plane. **The re-admission source,
  at EVERY
  severance (ruled 2026-09-21).** **Superseded by the per-writer
  build (*The roster kind* → *The per-writer roster cell*, built
  2026-09-27):** with no shared cell to lose, the ceremony snapshot alone
  sources every severance at the same entry id, and the record below
  retired with the build (`UserConfig::group_readmissions` is gone). The
  rest of this paragraph is the shared-cell ruling as it stood, kept as the
  reasoning the build inherited. The promise above is
  per removal,
  not once: a second authority-device removal finds every standing entry at
  a cell the *first* severance minted, which the ceremony's deliver never
  named, so a source looked up by ceremony cell alone re-admits each member
  exactly once and the next removal leaves the scope with no member and no
  tip, for good. A re-admission therefore takes its two facts from two
  retained sources, neither of them the plane. **Who stands where** comes
  from this account's own record of the cells it has written: before a
  severance writes a re-admission it appends that entry's identity core
  (scope, member, the fresh salt) to `UserConfig::group_readmissions` —
  record-then-act, so a crash leaves a recorded cell that was never written
  (inert: nothing stands there to go stale) and never a written cell with
  no record (unsourceable at the next removal) — and a stale cell is
  sourceable only when it is a ceremony cell of the retained deliver or a
  cell that record names. **At which key** is always the member-signed one:
  the reception key the member published in its ceremony accept, as the
  actor-verified deliver snapshot carries it — so the second severance
  re-admits at the same attested key the first used, and no severance ever
  copies a key from a roster row. The two refused alternatives: *sourcing
  by the stale cell's member actor* reads the link from the merged cell,
  which a revoked device can still write — it keeps the machinery root — so
  it could make the live authority re-admit whomever the snapshot ever
  named, a removed member included, and force a `Removed` and a mint per
  forged cell per pass; *sourcing from a bound merged entry an authority
  device authored* is rule (3) read backwards — at the second removal every
  stale cell's author IS the device just revoked, whose every signature is
  refused whenever it claims to have been made, and a stolen device binds
  a member's actor to its own reception key at a fresh cell at will — the
  attacker-writable merged-cell source the roster kind's source rule
  exists to refuse. A cell neither source names is left alone and counted
  (`unsourced`): its author is revoked, so every reader already refuses it.
  The record is grow-only and union-merged across the account's devices
  (one core, about a hundred bytes, per member per authority-device
  removal; a pruned entry would be resurrected by the union, so none is
  pruned), and it is a TOP-LEVEL `UserConfig` field because an older
  build's re-seal preserves an unknown top-level key through
  `UserConfig::extra` but silently drops one nested in a record it names.
  Its trust basis is the account plane's, exactly the retained deliver's:
  a device that was live on the authority's account when it wrote the
  record WAS the authority (the bound below). **Owed with it, by the
  writers that do not exist yet:** the member-removal writer strikes the
  member from this source in the same act, record first, or a revoked
  device's forgery at a recorded-never-written cell could resurrect a
  removed member; and the trigger-(2) build (a member's reception-key
  rotation, unbuilt — no member-signed channel carries a rotated key to
  the authority today) lands the newest verified member-signed key in this
  retained source *before* any re-admission or mint may use it, so that
  "the member-signed key" goes on meaning the current one. **The bound,
  stated:** a reader that
  has not yet merged the revocation still accepts the device (the staleness
  window every frontier-merged fact carries), and a holder of the authority
  ROOT secret is outside every device-level mechanism on either plane — the
  root ceremony's business, not this kind's. **And, under the shared cell
  the build had until 2026-09-27, the displacement residual — closed by the
  per-writer cell:** the
  revoked device keeps its device key and the machinery root, and the join
  ranks two self-verifying rows by bytes, so it can re-bind the member's
  newest cell — the one the last severance wrote and recorded — under its
  own carriage at a stamp that outranks the genuine row; every reader that
  has the line then refuses the cell and the member stands nowhere until
  the next live pass, which sources the recorded cell correctly, at the
  member-signed key, and pays one re-admission, one `Removed`, one mint and
  one permanent record core — and the device squats the fresh cell again.
  The member's availability is that device's to deny, one generation and
  one core per round, at the pace of live passes and only where its row
  reaches the live authority's replica. It is the narrow, self-repairing
  case of the class *The roster kind* → *The per-writer roster cell* states
  in full — beside it, any root holder's unsigned `Removed` severs a member
  for good with no pass repairing it — and it is latent: this plane is
  pull-only, the same-account peer leg severs a removed sibling, and the
  cross-user carriage is unwired. **What that carriage owes it: nothing at
  the channel, and nothing at ingest.** The share-plane channel proves an
  ACTOR (PT-1b — the node identity is the actor's own key,
  [`../behavior/p2p.md`](../behavior/p2p.md) § Transport seam), so a
  revoked device dials as its authority actor and no serve set can refuse
  it as a *source*; the row's own embedded carriage is the only device
  identity on this plane. An ingest-time refusal keyed on the line would
  not converge either: a replica that adopted the squat before the line
  reached it has lost the genuine row to the join and its frontier says it
  saw it, while the clean authority sees no stale cell to repair, so that
  replica stays severed for good. The residual is closed at the cell, not
  the carriage, and its record growth is left unbounded rather than
  bounded: it is one core per round the shared cell allows, and the
  per-writer cell allows no round. **Compat posture:** an additive
  kind, which a build that does not know it leaves alone (the registry's
  unknown-kind answer) and therefore does not honor — the stated transition
  cost, and live since 2026-09-21, when the publisher landed: a pre-ruling
  binary carries and re-serves a revocation row unchanged but goes on
  accepting the device it names, which is the staleness window this kind's
  bound already states, widened to "has not yet upgraded" from "has not yet
  merged". ***The shred clause* (same
  ruling).** A group mint's `Shredded` phase names a `shredded_by` that no
  reader ever verified, and R14's acceptance of crafted `Shredded`
  absorption is `BackupKey`-gated — the user's own device set — whereas
  this plane's gate is the machinery root, which former members keep. The
  two effects are therefore ruled apart: the resolver dropping a shredded
  generation's **candidacy** on any decodable `Shredded` stays accepted —
  availability only (the group needs a re-mint), and nothing a root holder
  does not already reach by same-key `Minted`-variant suppression — while
  **dropping a generation KEY never rides an unauthored row**: a consumer
  may discard a key only on a shred signed by a live authority device
  (additive `authorization` + `shredder_sig` on the variant, verified by
  `GroupGenerationMintRecord::shred_is_authored` through the same one
  check). No key-dropping consumer exists; the first one is built to that
  call.
- **The membership witness (the admission seam's fourth kind).** The witness
  is the member's `Enrolled` roster entry plus its authoring chain to the
  authority actor root, carried inline in the admission exchange
  (self-contained carriage, per the seam). The verifier binds the entry's
  actor key to the channel-proven identity (PT-1b key-binding), checks the
  chain, and checks its **own merged roster frontier** for a superseding
  `Removed`; verdict scope = exactly the group scope; revocation severs at
  the next admission evaluation (the seam's ratified bound — a stale
  co-member can be exploited only until the remove reaches it, the same
  staleness window every frontier-merged fact carries).
- **The concurrent-membership lattice, in one paragraph.** Adds commute
  (wrap-set union within a generation); `Removed` absorbs on the roster
  axis; concurrent severance mints converge by the R14 candidate rules
  (content-derived-id byte-order max wins, only candidate descendants
  retire); **the roster is authoritative and wraps are derived** — a wrap
  naming a `Removed` member is inert (never served against, dropped at the
  next mint), and an Enrolled member missing a wrap heals by member top-up.
  Convergence therefore never needs an ordering authority, only the merge.
- **Constraint (b) discharged explicitly, not inherited.** The v1 group
  scope registers **no nest-arbitrated kinds**: roster, keying, and content
  all converge arbiter-free, so the § Ordering model arbiter-trust
  acceptance is not extended to cross-user stakes — there is no arbitration
  to trust. A future group-scope kind wanting central assignment trips that
  section's revisit trigger by name: Space posture, or an explicit
  re-opening — never silence.

## Implementation status today

*The entries below were carried verbatim out of [`account-data-taxonomy.md`](account-data-taxonomy.md) § Implementation status today on 2026-09-28 (and before that out of [`account-data-plane.md`](account-data-plane.md)); the generation-machinery and export-confidentiality entries stay there. The scheme's listing in the cross-cutting ledger stays in [`account-data-plane.md`](account-data-plane.md) § Implementation status today.*

- **Built 2026-09-16 — the bound roster entry.** § The recipient-set scheme →
  *The roster kind* owns the ruling; what is code: the additive
  `binding_sig` on `GroupRosterRecord::Enrolled`, the preimage and builder
  (`fauna_core::group_scope::{roster_binding_signing_bytes,
  sign_roster_enrollment}`), `GroupRosterRecord::binding_self_verifies`,
  the ranking `join_group_roster` (key-free — the entry id is inside the
  record; the dispatcher arm and its permissive adoption are unchanged),
  and the shared verifier's refusal of a failing binding behind both
  `RosterView::build` and the admission witness door. The ceremony's
  deliver (`fauna_client_capabilities::group_ceremony::build_group_deliver`)
  and every fixture build through `sign_roster_enrollment`; the unbound
  shape survives in tests only as the refused case.

- **Ruled and built 2026-09-25 — the unbound `Enrolled` entry is refused.**
  § The recipient-set scheme → *The roster kind* owns the ruling; what is
  code: the shared verifier (`verify_enrolled_entry_parts`) refuses an
  empty `binding_sig` the same way it refuses a failing one, so both
  readers flag an unbound row (never a member, never a wrap target); the
  field's serde attributes, the join's ranking and the dispatcher arm's
  permissive adoption are unchanged. The pins that admitted the unbound
  shape flipped to refusals (`fauna_core::group_scope`'s reader and
  witness-door tests); the fixtures that carried an empty binding while
  testing another refusal build through `sign_roster_enrollment`.

- **Built 2026-09-16, retired 2026-09-25 — the legacy roster re-bind.** The group plane's first
  pump leg re-published, bound, the entries written before the bound-entry
  ruling, from the retained deliver snapshot. Removed by the compat-remnant
  sweep (§ The recipient-set scheme → *The roster kind*): no pre-ruling
  entry exists, and every current snapshot row is bound, so the pass could
  bind nothing. `held_authority_scopes` stays, the enumeration the
  severance leg opens its scopes through.

- **Built 2026-09-27 — the per-writer roster cell.** § The recipient-set scheme → *The roster kind* → *The per-writer
  roster cell* owns the ruling, *Mint triggers* the line-committed
  admissibility that goes with it, and *Severance, per axis* → *The bound,
  stated* the residual it closed. **What is code:** the roster row's logical
  key is `<entry-id-hex>/<author-device-hex>`
  (`fauna_core::group_scope::roster_cell_key`, the author being the device
  the row's own carriage names), `join_group_roster` is key-aware — ranks
  (verifies at the cell, `Removed`, bytes), total — and the roster arm of
  `fauna_protocol::merge_policy` parses the cell for it and refuses at first
  contact any row not verifying at its own recomputed cell
  (`GroupRosterRecord::verifies_at`, the revocation kind's shape);
  `GroupRosterRecord::Removed` is authored —
  `entry_id`, the carriage, `removed_at_ms`, a signature over the entry id
  and stamp under the frozen `ROSTER_REMOVAL_SIG_CONTEXT`
  (`sign_roster_removal`) — and `RosterView::build` folds per entry id
  under the line (excluded iff some `Removed` cell verifies under a device
  the line does not refuse, else enrolled iff some `Enrolled` does; several
  honored `Enrolled` cells enrol the newest by stamp, ties by author),
  `is_excluded_entry` answering honored removals only, which the witness
  door, the serve re-consult and the resolver's coverage arm all read
  through; `GroupMintCore::revoked_past` (sorted, inside the content-derived
  id, the additive-field discipline so a set minted past nothing keeps its
  id) and `resolve_admissible_group_tip`'s third arm — admissible only if
  the set covers the reader's line — with `owed_mint` entering a scope on
  an honored `Removed` OR a line that revokes anyone and minting past the
  line as the pass will leave it (`build_group_mint` takes the set); and
  the severance pass rewritten — every entry the as-it-will-be line leaves
  without an honored `Enrolled` and without an honored `Removed` is
  re-published at the same entry id in the live device's own cell from the
  ceremony snapshot, no `Removed`, no record — with
  `UserConfig::group_readmissions` and `GroupReadmission` removed under the
  2026-09-24 baseline (the repo private at the build). **Pinned:**
  `group_scope` — a non-authority-chained `Removed` excludes nobody
  (foreign root, revoked device, broken signature, another device's cell),
  a revoked device's re-bind at the recorded entry displaces nothing, two
  replicas merging the same cells in opposite orders fold alike,
  concurrent honored cells enrol the newest once; `group_generation` — a
  tip minted before a revocation resolves inadmissible and the mint past it
  is the tip, a non-sorted set is a forged shape; `merge_policy` — every
  displacement shape refused at first contact; `group_authority_revocation`
  — the first and second severances at the same entry id under the live
  cell with `revoked_past` covering the line, a scope the removed device
  authored nothing in still minting past it, the revoked device's re-bind
  landing only in its dead cell with no round paid
  (`a_revoked_device_rebinding_its_recorded_cell_displaces_nothing`, the
  inverse of the shared cell's pin), and both cross-account forgeries
  refused before they reach the plane. **Ruled and built the same day —
  a removal survives its author's revocation:** `RosterView::build` holds a `Removed` to the chain alone
  (`GroupAuthority::verify_device_chain`, the revoker's check), never to
  the revoked set, so revoking the remover resurrects nobody; pinned by
  `group_scope`'s `a_removal_survives_its_authors_revocation` (red under
  the line-consulting fold, green under the ruling) and the re-cut
  stranger's-removal and opposite-order pins.

- **Built 2026-09-21 — the authority-device severance PUBLISHER: the revocation writer, the
  re-admissions it owes, and this plane's first production re-mint
  trigger.** § The recipient-set scheme → *Severance, per axis* (*An
  authority device's removal*) owns the ruling; what is code is
  `fauna_sync_engine::group_authority_revocation`, the group plane's pump
  leg, running for every scope this
  device holds the machinery root of and is the birth record's authority
  of. Three writes, in this order: (1) every merged `Enrolled` whose own
  carriage names a device the pass is about to revoke is re-published at a
  **fresh** entry id under this device's carriage, with the stale cell
  `Removed` — the member and reception key taken from this account's
  retained, actor-verified `InitiatedGroupShare::deliver` snapshot and never
  from the merged cell (the roster kind's source rule), and a sibling
  device's snapshot row is ours to re-publish, since the new entry carries
  our own signature; (2) the
  severance mint over the post-severance roster, trigger (1) fired for the
  first time in production, its parents the mint DAG's leaves under the
  R14 `MAX_MINT_PARENTS` cap; (3) the revocations, LAST — a reader that
  merges a revocation before the re-admissions resolves no tip, so the
  write order is what keeps that fail-safe gap from ever being observed.
  The revoked set comes from `fleet_removal::removed_device_ids`, the same
  `FleetView` exclusion the peer leg severs a removed sibling on. The fresh
  `admission_salt` is **random, not derived**: a salt derived from the
  stale entry id and the revoked device — both public plane data — would
  let any machinery-root holder pre-squat every future re-admission cell
  and win the byte-order join for ever. Its cost is that a crash between a
  re-admission and its `Removed` leaves one benign duplicate entry (the
  pass skips a member the post-revocation roster already lists, so no
  second one follows); the inverse write order would instead strand the
  member severed with nothing owed to re-admit it. Proof:
  `group_authority_revocation`'s own test drives the ceremony on device A,
  removes A through `fleet_removal::write_removed`, runs device B's pass,
  and reads the merged rows as the MEMBER — A revoked, the member listed
  exactly once under a fresh entry, B's mint the keyable tip, and a roster
  entry or mint A authors afterwards refused. (The fresh-entry, `Removed`
  and recorded-cell mechanics this entry describes were the shared cell's;
  the per-writer roster cell entry above, built 2026-09-27, is the current
  shape — same entry id, own cell, no `Removed`, no record.) **The
  second-severance source, built the same day:** as first landed, the pass looked the snapshot up by stale
  cell alone, so it could re-admit each member once and the *next*
  authority-device removal stranded the scope with no member and no tip.
  Now a stale cell is sourced from the ceremony snapshot **or** from
  `UserConfig::group_readmissions`
  (`fauna_core::group_ceremony::GroupReadmission`, a grow-only set,
  union-merged in `format_user_config.rs`), to which the pass appends every
  fresh entry's identity core through its `RetainedRecord` seam BEFORE the
  plane write; the reception key is always the snapshot's member-signed one,
  found by the *recorded* member actor and never read off a roster row.
  Proof, same module: remove A, run B's pass, converge a third device C's
  replica, remove B, run C's pass — the member listed exactly once at a
  fresh cell under C's carriage, at the same attested key, C's mint the
  keyable tip, `unsourced: 0`, the pass idempotent; and with two bound
  forgeries revoked-B wrote first (the member bound to a thief's key, and a
  never-admitted actor), both are left `unsourced` and neither buys a
  re-admission, a `Removed` or a wrap. The severance mint's gate is now
  scope-local (it read the pass-wide counter, so a scope that only owed
  revocations minted whenever an earlier scope had severed). **The mint's
  trigger is state, built the same day:** § *Mint triggers* owns the ruling; what is code is `owed_mint` —
  a `Removed` in the merged roster, a verified member, and no admissible
  mint under an always-keyable closure — which both decides the mint after
  the pass's own writes and, in `severance_work`, enters a scope that owes
  nothing else. As first landed the mint fired only "because this pass
  wrote a `Removed`", so a mint that errored, or a crash after the last
  `Removed`, left no pass that ever minted. Proof, same module: a fresh
  replica of the same device holding the pass's rows minus its mint (the
  errored shape), and minus its mint and revocations (the crash shape), for
  the first severance and for the second one over recorded cells — each
  pass mints exactly once, the member resolves a keyable tip minted by the
  live device, and the pass after that writes nothing. **The self-revoked
  guard, built the same day:** `severance_work` distils a scope whose as-it-will-be line revokes
  this device to its owed revocations alone — no stale cell, no mint debt —
  and the pass publishes those before, and without, the retained-record
  load. Proof, same module: device B severs A, merges its own fleet
  `Removed`, and runs its pass against a record no request reaches — one
  revocation, then three passes that write nothing, the plane one row
  larger and `group_readmissions` unchanged; and B's replica converged on
  C's severance of it minus C's mint (the errored shape) mints nothing over
  four passes. **What is NOT
  code here:** the two duties the ruling places on writers that do not
  exist — the member-removal writer striking the member from this source,
  and the trigger-(2) build landing a member's rotated, member-signed
  reception key in it (group-plane reception-key rotation is unbuilt, so
  today the ceremony key IS the current key and no rotation arm can be
  tested). **What was NOT code here:** wiring
  the peer witness door — this leg *released* the block the 2026-09-19
  entry placed on it rather than lifting it, and the wiring landed as its
  own change (the entry below).

- **Built 2026-09-23 — the peer witness door, wired.** § The recipient-set scheme →
  *Severance, per axis* owns the rule this follows (the door's authority
  line is built from the evaluator's own merged
  `fauna.group.authority-revocation` rows). What is code: the production
  `GroupRosterState` is `fauna_sync_engine::group_scope_view::GroupRosterSnapshot`
  — every scope this device holds a machinery root for (the one held-root
  enumeration, `group_state_plane::held_group_roots`, shared with the pump
  legs' `held_authority_scopes`), each folded from its persisted rows by
  `read_group_scope`, the same fold `summarize_group_scope` lists with, so
  the door refuses exactly whom the folders page does not list;
  `authority()` is `None` for a scope not held, `is_entry_removed` reads the
  merged roster's `Removed` cells and answers *removed* for a scope not held
  (the trait's stated contract, every impl — `LiveGroupRoster`, the session
  seat with nothing lent, the test evaluators): the door's checks 3 and 4
  read the live cell twice, and a clear, a departure or a dropped lend
  between the reads must refuse, as it must at the serve re-consult, which
  reads `is_entry_removed` alone. `Removed` is
  absorbing, so a later state that still holds the scope still carries the
  removal and no snapshot needs pinning. The share plane's driver
  (`share_glue::run`) owns a `group_roster_door::LiveGroupRoster`, rebuilds
  it from the store every pass (`AccountStoreHandle::group_roster_snapshot`;
  a failed read clears it, fail-closed) and lends it to the session seat
  beside the M2 roster (`offline_share::bind_share_plane_seat`, held as a
  `Weak`), whose `CeremonyNode::set_group_roster` is the production caller of
  `ShareServer::set_group_roster`. There is ONE such seat — the session's
  actor-keyed seat — and no share data arm serves a group scope yet, so the
  effect is admission alone: the group serve arm, when built, owes the
  per-request `is_entry_removed` re-consult `verdict_admits_group`'s doc
  names. Pinned over a real store in `group_roster_door`'s tests: an
  enrolled member admitted, a `Removed` entry refused (the roster feed), an
  entry signed by an authority device the store learned revoked refused
  (the revocation feed), a scope with no held root refused, and a `Removed`
  entry refused when the cell clears between the door's two reads.

- **Built 2026-09-19 — the reader half of authority-device revocation, and
  the authored shred.**
  § The recipient-set scheme → *Severance, per axis* owns both rulings; what
  is code: the record, preimage, builder, cell key and decode-or-fail join
  (`fauna_core::group_scope::{AuthorityRevocationRecord,
  sign_authority_revocation, join_authority_revocation}`), the kind and its
  dispatcher arms with first-contact strictness
  (`fauna_protocol::group_state::KIND_GROUP_AUTHORITY_REVOCATION`), the
  authority line `GroupAuthority` and its one `verify_device_cert`, now the
  only way to call `RosterView::build`, `verify_enrolled_entry`,
  `resolve_admissible_group_tip` and the witness door's
  `GroupRosterState::authority`; every production reader builds the line
  from its own rows of the kind (the ceremony's admit from the deliver
  snapshot, `summarize_group_scope` from the store). The authored shred is
  `sign_group_shred` + `GroupGenerationMintRecord::shred_is_authored`.
  The publisher this entry left owed — and with it the removal writer, the
  re-mint trigger and the release of the peer witness door — is the
  2026-09-21 entry above.

- **Built 2026-09-28 — the group plane's key material rides as CBOR byte
  strings.** Every bare `Vec<u8>` on a
  signed payload or plane-row value in `fauna_core::{group_scope,
  generation, group_generation}` — the roster's `reception_pubkey`,
  `authorization`, `authority_sig`, `binding_sig` and `remover_sig`, the
  revocation's `authorization` and `revoker_sig`, every mint, top-up and
  unkeyable record's wrap and signature fields, `GroupMemberWrap.wrap`,
  `GroupReceptionPublished.reception_pubkey`, and the account generation
  machinery's twins (`DeviceSetRecord`, `MemberWrap`,
  `GenerationWrapRecordV2`, `EscrowTargetRecord`, `EscrowReceiptRecord`,
  `GenerationUnkeyableRecord`, `DeviceReachRecord`) — is `#[serde(with =
  "serde_bytes")]`: the *Bounded rows* byte-string rule
  ([`config-dissolution.md`](config-dissolution.md) § Phases and gates)
  applied at level 0 of the ceremony record's nesting, where these rows ride
  its deliver snapshot. A bare `Vec<u8>` encodes as a CBOR integer array at
  about 1.9× on key material; one inline mint wrap drops from about 2.4 KB
  to about 1.3 KB as encoded (`account-data-taxonomy.md` § The generation
  machinery → *The bounded mint* keeps the measured figure). Landed under
  the 2026-09-24 baseline reset, so no integer-array
  fallback is kept: the strict canonical decoder refuses the old form, and
  the peer-channel hardening corpus's roster frames were re-recorded. The
  fixed-width `[u8; 32]` ids followed on 2026-09-29, tree-wide, as byte
  strings ([`serialization.md`](serialization.md) § Canonical IPLD dag-cbor
  → *Fixed-size byte arrays* owns the rule and its built state). The
  ceremony's own `Vec<u8>` fields in `group_ceremony.rs` (levels 1 and 2)
  followed: they carry `serde_bytes` too.
