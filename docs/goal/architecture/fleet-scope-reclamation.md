# Fleet-scope reclamation — target state

Owns: fleet-scope-reclamation
Status: ratified — ruled and built 2026-09-16, its clauses ruled and built through 2026-10-13 (each carries its own date below), and § Implementation status today records the build; split verbatim out of `account-data-taxonomy.md` on 2026-10-10
Authority: **what keeps the fleet scope's live-entry count flat** — the cap that counts one row per live item, the retention-gated retire and the gate's watermark (clause (1)), reach as the possession evidence (clause (2)), the reclamation pass and its arms (clause (3): the removal arms, the hand-over and re-seal of a departed writer's rows, the shredder's veto, the authored shred and its shred cell, the predecessor arm, the let-go), and the sign-out's plane severance and the nest half that follows merged state (clause (4)), with the bounds each leaves. **NOT owned here** — the generation machinery this reclamation serves (generation tips, the mint protocol, the device set, escrow and *Escrow recovery*) → [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery; what holds the delegable scope at one live row per item → [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md); the bind leg, its retire record and the removal evidence → [`account-sync-plane.md`](account-sync-plane.md) § The bind leg; the retired identity's machinery keys and the kept wrap → [`owner-key-material.md`](owner-key-material.md); the sync-agent credential kinds → [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model. On conflict in those domains, raise it.

Last verified: 2026-10-10 (split verbatim; each ruling carries its own date below)

Split verbatim out of [`account-data-taxonomy.md`](account-data-taxonomy.md) on 2026-10-10, when that doc stood at 252,035 B, 96.6% of the 262,144 B whole-file read ceiling and about a week from it at its growth rate. This bullet was its largest single piece (about 72,000 B), one concept with its own measurements and build rows, so the concept is the seam. The bullet moved whole with the eight status entries that record its build; a routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-taxonomy.md`.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery, where it was one bullet among that section's others, and its hard-wrapped lines are kept as they were. A sibling bullet named by its title (the device-set bullet, the *source of `prior`* bullet, *The mint protocol*, the *escrow doors* bullet, *Escrow recovery*, *A holder change re-receipts and never mints*) and an unqualified `§ <name>` other than this doc's own two sections resolve in that doc first; a clause cited by number alone (clause (1), clause (3)(g)) is a clause of this doc's bullet. The two deictics that crossed the file boundary were rewritten to name their target; positional words inside a carried status entry keep their meaning, since the entries moved in their original order.

## Section map

- **[Fleet-scope reclamation](#fleet-scope-reclamation)** — the cap and the retention-gated retire, reach as possession evidence, the reclamation pass and its arms, the sign-out's severance and the nest half, with their bounds, compatibility and what was refused.
- **[Implementation status today](#implementation-status-today)** — the eight entries that record its build, carried with the rule they describe, newest first as they stood.

## Fleet-scope reclamation

- **Fleet-scope reclamation (ruled + built 2026-09-16).** The fleet scope's live-entry cap counts one row per
  `(item, writer)`, superseded only by the same writer re-putting the same
  item; every machinery row of a superseded generation, every row a removed
  device ever wrote, and every top-up cell a device has already consumed was
  therefore permanent, so a fleet spent the cap at the rate of one generation
  per removal and one device-set row per enrollment. The remedy is three
  mechanisms and one product ruling, each stated so it can be re-argued:
  **(1) The nest reclaims its own feed, behind a retention gate.**
  `fauna.account.state.retire` names a live row by its cleartext coordinates
  (scope, blinded item key, writer, writer seq) and the nest marks it
  superseded **inserting nothing** — the one plane write that needs no cap
  headroom, which is what makes a full scope recoverable from an app with no
  user act (principles.md § Client-recoverable nest state; question (d) of
  the row). The row's bytes and coordinate stay (the seq-reuse memory is
  intact, so a replay of the retired row is refused as before). The gate:
  **a row is retired only once every walker of the scope has walked past
  it** — a walker being a principal that has sent the feed its `walker_id`
  beside the serve-order watermark it banks (`held_through_seq`, the claim
  the walk already makes; the nest keeps the max per (scope, walker) in
  `state_walk_marks`), and a mark *counts* while its key holds a live,
  non-tombstoned grant on the account (a signed-out or deleted device's
  mark drops with its grant; a dead machine blocks reclamation exactly until
  the user removes it — removal is the control, as everywhere in this
  section). The gate is what makes a retire safe against every stale
  replica, the peer leg included: a replica that could still hold, or serve
  a peer, the pre-retirement state has by construction walked the row that
  supersedes it. Refused `not_yet_stable`; the caller retries next pass —
  **and asks only what the gate can accept (*the gate's watermark*, ruled
  2026-09-22):** the class-2 feed reply carries
  `retirable_through_seq`, the lowest counted mark of the scope (stamped
  after the requesting walker's own mark lands, so a converged walk never
  reads its own lag as the fleet's); the walk keeps each relay row's own
  serve coordinate (`RelayRow::feed_seq`, nullable and additive at rest,
  stamped from the put reply for an own row and from the page for a walked
  one); and the reclamation pass **withholds** the retire of any row above
  the watermark without asking, counting it `withheld` and keeping it exactly
  as a deferred row is kept. The rule is exact rather than a backoff — it
  predicts the nest's own verdict from state the pass's walk just merged —
  so it is computable on the first pass of a fresh runtime, never delays a
  retire the gate would accept, and moves no safety argument (the nest gates
  every retire it receives regardless). Not withheld: a row whose coordinate
  is unknown (a store-served leg's, one recorded before the column, an own
  row no reply stamped), every retire against an older nest that serves no
  watermark, and the sign-out's `sever_self` leg, which runs once with the
  nest's answer as its only witness. What this buys: a fleet with a dead
  walker costs no request per removed-device row per pass — some 240 refused
  retires per pass by the middle of a whole-suite sweep — and removal stays
  the control: the moment the user removes the dead machine its mark stops
  counting, the next walk reads a higher watermark, and the withheld retires
  go out and land. A
  retire may also carry `no_rows_sealed_under: G`, refused
  `generation_in_use` while any live form-v2 row in the scope names G in
  its cleartext header — the nest-side belt for clause (3e). Kinds and
  fields are additive (I4): an older nest fails the retire typed and the
  scope grows as before; the `walker_id` and `sealed_under` fields ride
  `fauna.sync.changes.list` unchanged for every old caller.
  **(2) Reach is the possession evidence.** The `fauna.state.device-reach`
  kind (its bullet in [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery): once a target's own signed row lists G, every
  cell wrapping G to it is redundant, and the top-up pass counts the reach
  as coverage before it looks at cells, so retirement never re-triggers a
  heal from a reach-reading binary. **The courtesy row's sunset (question
  (b))** — a healer wrote the legacy two-segment row only for a target with
  no reach row, so the dual-write stopped per target on the target's own
  signature — is history: the courtesy row itself was retired 2026-09-24 by
  the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4).
  **(3) The reclamation pass** (`fauna_sync_engine::generation_reclaim`,
  once per full pump pass, best-effort, after the top-up and unkeyable
  passes) retires only what is redundant, and only rows whose author is
  itself or is no longer a verified member — never a live sibling's, whose own pass retires
  them. **(a)** Refresh this device's reach when the set
  changed. **(b)** Wrap cells whose target's reach covers
  the generation, whose target is no longer a verified member, or whose
  generation is `Shredded`. **(c)** This device's own unkeyable cells once
  `Satisfied` is merged or the generation is shredded, and any cell whose
  target is removed. **(d)** A removed device's rows: its `Enrolled` row
  FIRST, then its reach and device-endpoints rows. The `Removed` evidence
  rows go after the enrollment — so no reader sees an enrollment without
  its removal — and behind the gate, which guarantees every walker bound
  to that nest saw the removal first. **The evidence rows are not this pass's
  to retire (ruled and built 2026-10-01):** a nest's gate counts only the
  walkers bound to that nest, so on an account with a linked replica a
  retire in the pass let the evidence go before a device bound to the other
  nest had read it — measured. When removal evidence leaves a nest, and
  what retires it, is [`account-sync-plane.md`](account-sync-plane.md)
  § The bind leg, ruling 7's to say; the enrollment, reach and
  device-endpoints rows stay this pass's, in this order, and a process that
  runs no secondary leg retires no evidence. **Only the device's own rows go
  (2026-09-19):** of the `GenerationTip` kinds, device-endpoints alone is
  device-scoped (keyed by the device's own id). The reception keypair, held
  group machinery roots, both custody registry kinds and share endpoints
  are the ACCOUNT's — written by whichever device ran the flow, never
  re-written by another — and no departure retires them, by removal or by
  sign-out: retiring one with its writer loses it for the whole account
  (§ No user-data loss). The classification is an exhaustive match the
  compiler owns (`fauna_protocol::merge_policy::TipSealedKind::scope`), so a
  new tip-sealed kind chooses a side in the commit that registers it; a row
  the pass cannot open is kept, never guessed at. The price, stated: a kept
  row stays sealed under the generation it was written in, which keeps that
  generation in use — it never reads dataless, so it never shreds and its
  escrow wrap stays (bound (v) below). The cure is the hand-over arm of
  clause (g): a surviving member re-seals the item under the tip, after
  which the departed author's row is redundant and retired — by that
  member, on the strength of its own published row, and by nobody else.
  **(e)** A dataless superseded
  generation is shredded and retired: G is *superseded* when every verified
  member's reach holds this observer's resolved tip and G is a proper
  ancestor of it (so no member still seals under G — every one resolves at
  or past the tip it holds); *dataless* when the nest serves no live row
  sealed under G (the feed's `sealed_under` filter, asked of the nest, never
  inferred from this replica's relay plane, which is never told about other
  writers' retirements); then the minter writes `Shredded` into its own row
  (count-neutral), or exactly one member does if the minter is removed (one
  transient row) — the byte-order-min verified member whose verifying reach
  lists G, or, when no member's does, the byte-order-min verified member —
  and once the gate admits it the mint row(s), the receipt
  and any remaining cells — its `fauna.state.generation-closed` row among
  them, by the pass's dead-cell step — are retired, the retire carrying
  `no_rows_sealed_under: G`. Reach lists live `Minted` generations only, so
  it shrinks as generations shred. **The shredder's veto (ruled 2026-09-19,
  built 2026-09-23): the nest's answer is necessary, never sufficient.** A nest that
  answers "none" while live rows are still sealed under G induces an
  absorbing `Shredded`; every device then drops G's key on merge, and every
  copy of those rows — the ones replicas would serve a new device over the
  peer leg included — is unopenable for good. That is more than the
  availability a nest already holds, which ends at its own leg, so it is
  not accepted.
  **The authored shred (ruled 2026-10-08).** The veto binds an honest shredder only, and `BackupKey` — which every reading replica holds, removed devices included — seals the mint row, so a crafted `Shredded` for the current tip skipped it and reached the very outcome refused above. So the shred is signed by its author, the group plane's shape less the cert: Ed25519 by the shredder over the recomputed generation id, the shredder's device id and the stamp under a frozen domain tag (`fauna_core::generation::sign_shred`, which the shredder's pass writes) — carried in the shredder's own **shred cell** (next paragraph, ruled 2026-10-13), never on the mint row, which names its shredder as attribution only — and a shred is **authored** at a reader only when the cell's shredder is a verified, non-removed member of that reader's fleet view and the signature verifies (`ShredAttestation::is_authored_at`, over the cell's record). No authorization rides the attestation: the device set is this plane's authority and every reader holds it, where the group plane's cert chain is not. **Two acts, two gates.** The join absorbs and the resolver drops candidacy on ANY decodable `Shredded`, authored or not — the `Minted` row's wraps are gone from the plane either way, so the generation could not be keyed from the plane again, and the fleet re-mints by trigger (a): availability only, the class the audience ladder's bullet accepts. Dropping a key, sweeping an escrow wrap, retiring a row or forgetting relay residue BECAUSE a generation reads `Shredded` rides an authored shred cell only: the read path serves an unauthored shred's generation from the retained bundle as if its mint row were absent, and the reclamation pass (its `shredded` set, the predecessor arm's licence), the walk's drop hook and the linked-leg sweep count authored shreds alone; the drop is eventual and idempotent across those three, since the shredder's own enrollment can merge after its shred. Membership is read at the reader's CURRENT view, never at the stamp — the device set is monotone and has no clock — so a shred by a device removed later goes inert wherever it has not yet been acted on, the fail-safe direction; its generation then rests out of candidacy with its key retained, residue O(such incidents), accepted. A mint row reading `Shredded` with no authored cell beside it — a forged marker, or a shred by a device removed since — is unauthored, and nothing is migrated (the 2026-09-24 reset). Top-up and re-escrow keep skipping every `Shredded` row: the members that hold the key re-seal their rows under the next tip (clause (3)(g)'s own arm), so that cost is transient. The build state is § Implementation status today.
  **The shred cell (ruled 2026-10-13).** The authored shred first rode the mint row itself, and the probe showed why it cannot: `fauna.state.generation-mint` is one cell per generation, whose join ranks `Shredded` over `Minted` and then, between two `Shredded`, the byte-order max — so a `BackupKey` holder (every device the account ever enrolled, removed ones included) writes a `Shredded` for G with an all-`0xff` shredder and any 64-byte signature, its bytes sort above the honest shred in both orders, every replica's merged row reads unauthored, G is in neither `minted` nor the authored `shredded` set, and no pass ever attests, retires or drops it: residue for good, the crypto-shred silently defeated. No rank at that one cell closes it. A join may rank on nothing but the cell key and the two values — a merge never consults the device set, the root or an authority line (`fauna_protocol::merge_policy`'s standing contract, and the only shape that converges, since membership is read at a view and two replicas' views differ); self-verification alone is grindable (an outsider mints a key whose id sorts above the honest shredder's); and the mint core names the members at mint, which the stand-in shredder — a device enrolled since — need not be among. So the authored act leaves the mint row for a cell the forger cannot sit in: **`fauna.state.generation-shred`**, one cell per (generation, shredder) at the logical key `<generation-hex>/<shredder-hex>` — the per-healer wrap cell's shape and the roster's. The value is the attestation: the generation id, the shredder's device id, the stamp and the signature `sign_shred` already computes over the same frozen preimage; it verifies at its cell only when segment 1 is the generation id it names, segment 2 is the key its signature verifies under, and the signature verifies; the join is key-aware and decode-or-fail, rank (verifies at the cell, bytes), and first contact refuses a row that does not verify at its cell — so **a standing attestation is never displaced by bytes from outside its own shredder's cell**: a forger files its own cell under a key it holds, which no reader honors, and displaces nothing. The mint row's `Shredded` is then the availability marker and attribution only — `core`, `shredded_at_ms`, `shredded_by`, no signature (the 2026-10-08 field leaves the variant; the 0.1.x window owes no migration) — and the two-acts rule above reads: candidacy drops on ANY decodable marker, every destructive act counts an authored shred cell alone, authored at the reader's CURRENT fleet view as before. The shredder's pass writes both — the marker into the mint row where it still reads `Minted`, and its attestation into its own cell — and keeps a generation in its shredder election while the mint row reads `Shredded` with no authored cell beside it (a forged marker, or a shredder removed since), attesting when the gate admits (superseded, dataless, the veto clear), so the drop is eventual under attack too; the dead-cell step retires a generation's `<g>/*` cells with its mint row. The residue, stated: a marker forged onto the current tip still drops its candidacy (availability only, accepted above; the fleet re-mints), and a grinded cell is a live entry under a key no reader honors — the per-healer wrap cell's accepted class — counted against the cap until its generation retires. **The group plane takes the same shape** (priority #1/#3 — one lattice, two planes): `fauna.group.generation-shred` at the same two-segment key, the attestation carrying the shredder's cert under the signature (the preimage commits to the cert's BLAKE3, so a copy with its cert swapped does not verify at the cell), authored at the reader through `GroupAuthority::verify_device_cert` as the cert-chain plane requires, its mint row's `Shredded` likewise attribution only; both kinds land in one build inside the 0.1.x window ([`recipient-set-scheme.md`](recipient-set-scheme.md) § The recipient-set scheme → *Severance, per axis*, the shred clause, points here). The build state is § Implementation status today.
  Before writing `Shredded` the shredder reads its own relay
  plane's rows whose cleartext header names G and refuses this pass while
  any row it can open is **uncovered**. A row is covered when it is
  device-scoped and its writer is no longer a verified member (it goes with
  the device: clause (d) on removal evidence, clause (g)'s let-go arm without it), or when its item's merged entry is carried by a
  verified member's row on this plane at the tip's item key (clause (g)'s
  product, or the writer's own later re-publish) that is published as the
  pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind
  leg, ruling 1(d)). The evidence is the
  covering row, which every walker receives and no nest can forge — never
  the old row's liveness, which the relay plane is never told — so the veto
  clears by itself as clause (g) runs and another writer's unseen
  retirement cannot wedge it; a live sibling that has not re-sealed yet
  holds the shred exactly as its row holds the nest's belt today. A row the
  shredder cannot open vetoes nothing, which is why the stand-in shredder
  is drawn from the members that key G whenever one does. The residue,
  accepted: when no member keys G the shred rests on the nest alone, and
  there the only path to G's rows is the escrow wrap that same nest holds —
  it could delete that with no lie at all. A second escrow holder class
  (the *escrow doors* bullet's holder-management surface) re-opens this
  residue and must rule on it. **Ruled for the first such class, the
  user's own linked nests (2026-09-30, built 2026-09-30;
  [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4,
  owns the class):** a shred must reach every holder, so the belted retire
  below is issued against each linked nest as well as the bound one, each
  nest's belt checking its own rows; and because `Shredded` is absorbing —
  every device drops the key on merging it — ANY member's secondary leg
  retires the rows a linked nest still lists under a shredded generation,
  whoever wrote them, and sweeps that nest's wraps for it. The residue moves
  and is stated: a linked nest no device can reach keeps a shredded
  generation's wrap until one does, and a nest the account has left keeps
  it until the account is deleted there. The holder-management surface
  for holders that are not the user's own nests still owes its own ruling.
  **The escrow sweep (2026-09-16):** the
  shredded generation's escrow wrap goes with the receipt row that named
  it — the receipt retire carries `delete_escrow_wraps` beside
  `no_rows_sealed_under: G`, and the nest deletes every wrap it holds for G
  in the same transaction as the retire that lands, after the belt has found
  G dataless once more. This is the one non-user caller of the
  per-generation crypto-shred (`fauna.generation.escrow.delete` stays
  user-gated: it can destroy recoverability), and it is safe for the reason
  the shred is: a dataless generation has nothing to recover, so its wrap
  protects nothing, and the client act that vouches for it is the one this
  ruling already trusts to shred. Three shapes keep it so: the flag names no
  generation of its own, so the deletion can never outrun the belt (a flag
  without a belt is refused); a retire that lands nothing deletes nothing;
  and a retained generation's wrap is never touched — an age-based sweep is
  rejected, the nest cannot tell retained from shredded by age and a
  retained wrap IS the recovery path. The belt scans only the retire's own
  scope and is complete for the account solely because every generation-
  sealed kind is fleet-only (the `GenerationTip` set in
  `fauna_protocol::merge_policy`, all routed to `state-fleet`); a delegable
  generation-sealed kind would need the scan widened in the same change. The retire kind enforces the fleet-only premise by construction: `no_rows_sealed_under` (and `delete_escrow_wraps` with it) is refused `invalid_request` unless the request names `state-fleet`.
  **(f) A writer retires its own
  superseded generation-sealed rows.** A re-seal of a `GenerationTip` row
  under a newer tip — `device-endpoints` on every tip change, any such kind
  on its own re-publish — is a NEW wire row: the form-v2 item key derives
  from the per-generation schedule, so the nest cannot collapse the old row
  under the new one, and only the writer knows the two are one logical item
  (its journal holds the `(kind, key)` at the writer seq its relay row
  carries). The pass keeps the newest row per item and retires every older
  generation-sealed row of its own once that newest row is published as
  the pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The
  bind leg, ruling 1(d) — a row the bound nest does not hold carries the
  item to nobody there), which is what lets a superseded
  generation ever read as dataless at the nest; the 12-seat sweep measured
  the count growing about two rows per cycle without it and flat with it.
  **(g) The re-seal pass (ruled 2026-09-19, built 2026-09-23) — one pass,
  two arms.** A live `GenerationTip` row sealed under a superseded generation
  keeps it in use whoever wrote it, and only device-endpoints re-seals on a
  tip change; so a fleet holding any account-level row never sheds a
  generation. One pass closes that for a live writer's rows and a departed
  writer's alike — the ratified loser re-seal and the hand-over are the
  same act, differing only in who performs it. It runs ahead of clause (e)
  in the same reclamation pass, over exactly the generations (e) would
  shred but for being in use — G *superseded* in (e)'s own sense, so every
  verified member already holds the tip a re-sealed row lands under, and a
  fork's loser waits, as it does for (e), until a later mint names it a
  parent — and only over rows this device can open. For such a row R of
  logical item X = (kind, key): **what is re-sealed is this replica's
  merged entry for X**, value, `merge_meta` and tombstone marker verbatim,
  through the ordinary writer door (which seals under the resolved tip) —
  never R's own bytes. The merged entry dominates every row of X under the
  kind's join, so whatever row carries it makes R redundant; a `LatestWins`
  stamp travels unchanged, so a re-seal can never outrank another write; an
  `Immutable` value is the same value; and a tombstone is re-put as a
  tombstone, because retiring one uncovered could resurrect what it
  deleted. **Own arm:** R's writer is this device and it holds no row of
  its own for X at the tip's item key → put; clause (f) then retires R.
  **Hand-over arm:** R's writer is no longer a verified member, R's kind is
  account-level (`TipRowScope::Account`), and this device is G's **hander**
  — the byte-order-min verified member whose verifying reach lists G, the
  same single deterministic writer clause (e) names → put, unless its own
  row at the tip's item key already carries the merged entry. A live
  sibling's row is its own pass's business, as everywhere in clause (3).
  **The retire of a departed writer's account-level row** belongs to a
  member whose OWN row carrying X's merged entry is published as the pass
  saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg,
  ruling 1(d)), and to nobody else: another member's
  covering row may have reached this replica over the peer leg before the
  nest holds it, and a retire licensed by it could leave the nest serving
  the item to no one. "Holds a row of its own at the tip's item key" is the
  device-endpoints writer's check, exact and stateless. **Races, stated:**
  reach skew, or a change of hander while R still stands, can produce a
  second member's copy — one extra live entry for that item, both copies
  correct, each retiring R idempotently; a copy is only ever retired by its
  own writer's clause (f), or handed over in turn when that writer departs.
  Nothing on this path can lose a row: the put precedes the retire, the
  retire waits for the put's publication, and a refused put (`scope_full`,
  no tip) leaves R where it was. **No member keys G** — the single-device
  sign-out → sign-in, where the new device holds no wrap and nobody is left
  to top it up: there is no hander, so nothing is handed over, nothing is
  retired, and G stays `Minted` with its escrow wrap, the one thing that
  can re-open it. A device that later keys G by any path lists it in its
  reach and is thereby the hander; the pass needs no recovery arm of its
  own. What *drives* that recovery is the *Escrow recovery* bullet ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery):
  the next seed-holding pump pass keys G from its escrow wrap, so the
  no-hander state lasts one pass, not for ever. **Compatibility:** every row
  the pass writes is an ordinary put and every retire an existing one — no
  wire or at-rest change; a pre-change binary never re-seals, so its
  account-level rows hold their generation until it upgrades or is removed
  (then the hand-over arm takes them), which is today's cost and no more.
  **(g), the let-go arm — a departed writer's device-level row is retired without removal evidence (ruled 2026-10-01; built the same day — § Implementation status today).** The hand-over arm skips a device-scoped row: it describes a device, so the account has nothing to keep by it and nothing is handed over. Clause (d) retires such a row on removal evidence, and until this ruling nothing else retired one. A device-level row whose writer is no member and was never removed therefore stayed live under its generation for good, and that generation never read dataless. A succession makes this the ordinary case. Under the successor a predecessor's device is no member and no removal names it ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, *What is re-made, never carried*), so its device-endpoints row pinned every carried generation. Measured 2026-10-01: that row is the whole obstacle. Retired by hand at the nest, the carried generation shreds in the next pass and its escrow wrap is swept. **The rule.** Over the generations the re-seal pass runs on — G superseded in (e)'s sense — G's hander retires each row sealed under G that it can open, whose kind is device-scoped (`TipRowScope::Device`) and whose writer is not a verified member. No put precedes the retire, and the hander forgets its relay copy when the nest confirms the row gone. **Why non-membership is evidence enough here, when clause (d) asks for a removal.** Clause (d) retires a removed device's rows under any generation, the tip included, so it needs a statement nothing can withdraw. This arm reaches a superseded generation only. Every verified member's reach holds the tip, a live writer re-publishes its device-level row under the tip on every tip change, and its own clause (f) retires the row it left behind. So where the writer is in fact a member, the row is one it has replaced or is about to. One case has a cost: a member this replica has not verified yet, its enrollment row not merged here, that has not re-published either. A replica that first walks in that window misses that member's dial candidates until the member's next pass re-publishes under the tip. No data is lost, because a device-level row is re-derived by its writer on every pass. The shredder's veto already rests on the same reading: it counts such a row covered and lets the shred go over it (clause (e)), after which no reader opens the row. This arm retires only what the veto had already let go. **Who asks, and what the nest checks.** The hander, as for the hand-over: one deterministic requester, and one that keys G, which it must to learn the row's kind. The retire door needs no change. It authorizes the account's own session and never the row's writer (clause (1)), and its gate applies as to any retire; a predecessor's device holds no grant on the successor's account, so its walk mark counts for nothing there. **What else it closes:** a removed device's device-level row still standing when its removal evidence left, and a row whose writer's enrollment never verifies. **What stays.** A row no member can open is kept, as everywhere in clause (3). A predecessor's device-level row under a generation no successor device carried is one such, and that generation is already dead to the account (`owner-key-material.md`, the rider's residual (i)). With no member keying G there is no hander, so nothing is retired until one does — the hand-over arm's own no-hander case. **Compatibility:** an existing retire, sent from an existing pass; no wire or at-rest change. A binary older than this rule retires nothing here, which is today's residue. **Refused.** *Dropping the predecessor devices' device-level rows in the succession ceremony's own transaction:* the kind is sealed, so the nest cannot tell a device-level row from an account-level one, and could only drop every tip-sealed row a predecessor device wrote, the account's reception key among them. *The ceremony's client retiring them:* it reaches only the rows that one client can open at that moment, and a retire the gate defers needs a pass that asks again, which is this one. *Accepting the residue as a bound:* each succession would leave a generation that never shreds, its key in every bundle and its wrap at every holder, for the sake of a row nothing reads, when the pass already knows how to send the retire.
  **(h) A replica forgets a dead row's relay copy, whoever wrote it (ruled + built 2026-09-27).** Nobody tells a replica's relay plane about another writer's retirements (clause (e)). Without this clause a long-lived replica kept for good every row a live sibling superseded under clause (f), every row a departed writer retired itself, and every gen-0 cell or receipt a sibling wrote. On the 12-seat sweep that grew one replica's plane from 180 to 480 rows over cycles 10 to 30, against a flat 43 live entries at the nest. The clause has two arms. Both are local — no journal row, nothing published — and both run in the reclamation pass. **A shredded generation's residue:** every relay row whose cleartext header names a generation that this replica's merged state reads `Shredded` is forgotten, whoever wrote it. The rows are found through an index over the header's generation (`relay_rows.generation_id`, an additive column back-filled once at the open that adds it). The index is probed only for the generations the plane still holds rows under, so the cost follows the live plane, never the fleet's history. **A dead gen-0 item's copies:** where the pass forgets a wrap cell, an unkeyable cell, a receipt or a removed device's reach as dead everywhere, it forgets the item's relay row under every writer, not only the rows it retired itself. Otherwise a live sibling's copy outlives the merged entry, and no later pass would ever name it again. **Why the peer leg ([`account-sync-plane.md`](account-sync-plane.md) § The peer leg) never needs either arm:** `Shredded` is absorbing, and every device drops the key on merging it (clause (e)), so no reader anywhere opens such a row. The shred also waited until every row under G that the shredder could open was covered by a row at the tip's item key (the shredder's veto), and that covering row carries the item a peer might want. A dead gen-0 item is dead by this ruling's own rule, the same rule that already forgets its merged state. **The record point stays unconditional** ([`account-replica-posture.md`](account-replica-posture.md) states the relay plane's record rule and points here for this exit). When a reconcile or a lagging peer serves such a row again, the walk records it again and the next pass forgets it again. The forget is an idempotent per-pass sweep, never a once-only memo that a re-serve would undo. **The bind leg's publish diff declines to push what this clause forgets** ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(b) owns that rule; ruled and built 2026-09-30): a row added to either arm, or taken out of one, changes what that diff sends, and a `Shredded` generation's own mint row is in neither arm, so a sibling's copy of it leaves by the diff's refused put (ruling 1(c)).
  **(i) The predecessor arm — a retired identity's generation-0 machinery rows are opened to be retired (measured and ruled 2026-10-01; built 2026-10-02 — § Implementation status today).** A succession moves the fleet scope's feed to the successor with every live row the predecessor's devices wrote ([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Re-key scope owns the move, and the ceremony touches no class-2 row). The successor's schedule opens none of the generation-0 ones, its walk carries one kind of them (the mint record: [`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, the succession rider), and every clause above retires only a row the pass can open. So they stayed live for good. Measured: a predecessor fleet of N devices on one generation leaves 3N + 3 live rows — per device its enrollment, its reach and its device-endpoints row, and per fleet the escrow target, the generation's mint row and its receipt. That is 6, 12 and 21 rows for 1, 3 and 6 devices, the same at the third successor pass and the fifteenth. The device-endpoints rows are the let-go arm's (clause (g)). The other 2N + 3 are this arm's. **The keys.** A runtime is handed, for each attested predecessor, that identity's generation-0 keys for every fleet-only machinery kind, open-only; `owner-key-material.md` (the rider → *The retired machinery keys open only to retire*) owns what is handed and that the walk still gets the mint kind's pair alone. This pass tries them on a form-v1 relay row that the plane's own schedule does not open and whose writer is not a verified member. **The rule.** A row that opens under them is that retired identity's machinery, of the kind the open names. (1) *Any kind but the mint record* is retired: a device-set row, enrollment and removal alike, a wrap cell, an unkeyable cell, a closed row, a reach, a receipt, the escrow target. Nothing precedes the retire. The succession re-makes these rows and the successor's merged state holds none of them (the rider → *What is re-made, never carried*). (2) *A mint record* is retired on either of two licences. The first is the hand-over arm's: this device's own row for that generation carries the merged record and is published as the pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(d)) — the row the walk's carry wrote. The second: the record it opens to, or this replica's merged state, reads the generation `Shredded`; that retire carries the belt, as clause (e)'s do. Otherwise the row is kept, because it is the one row a device that brings that generation's key later still needs (the rider's residual (iii)). The device forgets its relay copy when the nest confirms a row gone. **Why this is no guess.** Clause (3) keeps a row the pass cannot open, and this arm opens every row it retires. The open names the kind and proves the row was sealed under the retired schedule, whatever writer id it claims; and the retire names the blinded item key that schedule derives, which no row sealed under the successor's schedule sits at. So a row forged under the retired keys can do no more than be retired itself. **The retired keys open here to retire and for nothing else:** no row opened under them is merged, re-authored, vouched to the publish diff or pushed. **Who asks, and what the nest checks.** Every device that holds the material. A retire is idempotent, and a second asker's answer is its licence to forget its own copy. The retire door needs no change (clause (1)) and its gate applies as to any retire; the predecessor's devices hold no grant on the successor's account, so their marks count for nothing; and the retire record carries each retire to the linked nests (`account-sync-plane.md` § The bind leg, ruling 5). **What stays, stated.** (α) Where no successor device holds a predecessor's seed, nothing of that predecessor's is opened and all of it stays: 2N + 3 rows per succession beside what clause (g) leaves, measured at 6,737 B sealed for one device and 2,773 B more per further device with clause (g)'s rows counted in. A 64-device fleet is 131 rows against the 4096-entry cap. (β) A mint record of a generation no successor device keys and no holder wraps, and every row sealed under it. Nothing opens them, and a device that arrives later with the key is their one cure (the rider's residual (i)). Zero in the measured flow, whose one generation is carried; **11 rows for one predecessor device on one generation** in the kept-wrap measurement (the rider → *The kept wrap*): the account's reception key, senior rotation key, period keys, folder keys and mail custody, and the device's own endpoints row. A succession no longer puts a generation here — the ceremony keeps the holder's copy (ruled 2026-10-01) — so (β) is reached by the three roads clause (j) names, and clause (j) is what gives the room back. (γ) A replica that holds no predecessor material, and walked these rows before they were retired, keeps its relay copies: nothing tells a relay plane about another writer's retirement (clause (h)), and it cannot open the rows to learn they are dead. A replica that joins afterwards is served none. **Ruled 2026-10-01:** the ceremony may not burn a generation's last escrow copy — the rider → *The kept wrap* owns the rule — and the room (β) holds is given back by the user, clause (j). **The delegable scope's counterpart** is ruled where the carry is: `succession-aftermath.md` § Re-key scope → *The predecessor's own row is retired behind the carry*. There the walk already opens every predecessor row, so that rule reads the walk's list and needs no key handed to the pass. **Compatibility:** an existing retire, sent from an existing pass; no wire or at-rest change; the keys derive from a `BackupKey` every attesting host already holds. A binary older than this rule retires nothing here, which is today's residue. **Refused.** *Stating the residue as a bound:* each succession would leave a fleet's machinery on the feed for good, served to every fresh walker and kept in every relay plane, and a scope filled that way could be emptied by no client (principles.md § Client-recoverable nest state). *The nest dropping the rows in the ceremony:* they are sealed, so the nest cannot tell the machinery from a mint record or from the account's own tip-sealed rows. *Retiring every form-v1 row of a non-member writer that this device cannot open:* that is the guess clause (3) refuses. A kind this build predates and a predecessor this device does not attest both fail to open, and the second may be a mint record. *Handing the walk the wider keys:* the rider refuses it, and nothing here needs it.
  **(j) The let-go — a dead generation's rows are retired by the user, and by nothing else (ruled 2026-10-01; built 2026-10-02, tui first — § Implementation status today).** A generation is **dead** when no device keys it and no holder wraps it. Three roads lead there, and a succession is no longer one of them (the rider → *The kept wrap*): the minting device is lost between its mint and the deposit its re-escrow pass owes, a one-pass window; the user shreds the generation at every holder through `fauna.generation.escrow.delete`, their own gated act; or every device is lost and the holder no longer has the wrap — a nest restored from a store older than the deposit (a receipt proves a deposit, not a holding: *A holder change re-receipts and never mints*, clause (3)), or a nest the account has left (the linked-nest residue, clause (e)). The rows sealed under a dead generation are bound (β): the account's tip-sealed rows of the moment plus one device-endpoints row per device, 11 for one device, and they count against the cap for as long as they rest. **Why no pass retires them.** A client can observe three things — it does not key G, no verified member's reach lists G, and the holder's filtered get answered empty (the durable answered-empty bit of *Escrow recovery*) — and not the fourth: a device of the user's, asleep since before the loss, whose bundle still keys G and which reads every row under it the day it is signed in again. Nothing on the nest or in a replica can tell that device apart from one that never existed, so a timer, a cap-pressure drop, or a retire on the holder's empty answer would destroy data a later device could read (`principles.md` § No user-data loss). The decision is the user's, and it is the user's data. **The act.** From the Settings recovery-kit section ([`../ui/settings.md`](../ui/settings.md) § Recovery kit owns the placement and the view state; its element ids need the user's approval under rule A before the build), the app lists each dead generation as the data sealed under it — its mint stamp (`MintCore::minted_at_ms`) and its live-row count — and the let-go confirm says the one thing the client cannot know: a device not signed in since then may still read these, sign in there first. On confirm the client retires, through the existing retire door, every live row whose cleartext form-v2 header names G — the walk is served that header on every row, opened or not, which is what the nest's belt reads too, so the rows are named by their coordinates without being opened; the gate applies as to any retire, and the retire record carries each to the linked nests ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 5) — and then deletes G's wraps at every holder, an idempotent no-op where none rest. G's mint record goes with the rest where this device can open it (clause (i)'s keys, or its own schedule); where it cannot, it stays as bound (α) keeps it. A replica forgets its relay copies of the retired rows as clause (h) forgets a shredded generation's. **Why this is no guess.** Clause (3) keeps a row the pass cannot open because the pass cannot know what it holds; the user is told exactly that and decides. The header is cleartext and unforgeable as to generation only in the sense the belt already relies on: a row whose header lies about its generation is retired with the generation it claims, which it had made itself a row of. **Compatibility:** an existing retire and an existing delete, from a new caller; no wire or at-rest change. **Refused.** *A nest-side drop of every row under G once the scope is full:* the nest reads G from the header but cannot know whether a device keys it. *A client-side retire when the holder answers empty and no member's reach lists G:* the dormant device. *Stating (β) as a bound with no exit:* the cap is reachable in principle, and a scope a client cannot empty is what `principles.md` § Client-recoverable nest state calls a bug, however far the bound.
  **(4) Sign-out severs on the plane (question (c)).**
  `AccountStoreHandle::shutdown_for_sign_out` writes this device's own
  `Removed` row (superseding its `Enrolled` row at the nest — same item, same
  writer, count-neutral; an enrolled non-removed device may remove itself
  under the kind's authority rule), retires its own reach and
  device-endpoints rows — never an account-level row it wrote (clause
  (3)(d)'s classification) — and only then retires its enrollment nest-side (the
  grant tombstone is what drops its walk mark). One shared-Rust seam, so every
  app whose sign-out stops its runtime through it severs with no per-app change
  (all seven apps do; web since 2026-10-01 —
  [`account-client-lifecycle.md`](account-client-lifecycle.md) § Implementation
  status today, the 2026-10-01 entry); best-effort like the grant
  retirement — an offline sign-out leaves the enrollment, removable from the
  devices page. **The `Removed` row is the writer's last word, never queued
  behind a refused row (ruled 2026-09-24):**
  it drains this writer's unsent own rows first like any put, but a drain
  that fails does not hold it back — it is sent at its own coordinate
  regardless (`AccountStatePlane::put_last_word`), the one write outside
  refinement 11's ordered own publish ([`account-replica-posture.md`](account-replica-posture.md)).
  The contiguous-prefix law protects a slot a later pass or heal reads, and
  a signing-out writer has neither: its journal and key are erased right
  after. The case it closes is the sign-out's own cut
  ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) §
  Implementation status today, the third placeholder source): a pass cut
  inside a put the nest recorded loses the reply, the drain's re-send is
  refused `stale_writer_seq` as the replay it is — no value lost, the nest
  holds the row — and a `Removed` row wedged behind it left a named device
  un-severed on the fleet plane, its rows live for every later prologue to
  carry. Pinned by
  `account_runtime::tests::a_sign_out_cutting_a_fleet_put_mid_flight_still_severs_the_device`
  (tier_1; red-verified). **The devices page's remove-device action owes — and now
  writes — the same row from the *removing* device, built 2026-09-16**: `DevicesMachine::remove_device`
  (`libs/fauna-devices-machine`) drives the account runtime through an
  app-supplied `FleetRemoval` port — resolve, stage, delete at the nest,
  settle (the two rules below) — and a failure reaches `error-message` rather
  than dropping silently, which would reproduce this same leak.
  **The removal target is resolved from client-held truth, before anything
  is deleted (ruled + built 2026-09-19).**
  Every field of a `fauna.sync.devices.list` row is the nest's to write —
  its `principal` included — and a `Removed` row is absorbing and excludes
  unconditionally (the device-set bullet), so a removal that trusted the
  row's principal let a hostile nest choose which device the removal
  permanently excludes (a live sibling, the caller itself) **and which it
  spares**: the device the user meant stays a verified member and a wrap
  target of every later generation, which is exactly what severance exists
  to forbid. The nest cannot seal a fleet-plane row, so the binding is one:
  **each device states the nest `sync_devices` row it enrolled on** (the
  registration latch's row half) **on its own `fauna.state.device-endpoints`
  entry** — generation-sealed, one whole-record LWW row per device, so an
  adopt that moves a machine onto a co-located agent's row simply restates
  it (the enrollment's join could not carry a mutable fact), additive (an
  entry stating no row is byte-identical to the earlier shape), plane-only
  (the carried copy the admit exchange hands a peer outside the fleet never
  has it), and retired with the device like the rest of clause (3)(d). The
  rule (`fauna_core::fleet_removal::resolve_removal_targets`, pure; the
  runtime's `AccountStoreHandle::resolve_fleet_removal` gathers the facts):
  (a) the row is this device's own enrolled row → refused — leaving is this
  clause's first half, sign-out, never a roster gesture; (b) some verified
  member states the row → **those members are the target, whatever principal
  the nest claims** (several only when one machine re-minted its principal),
  refused if this device is among them; (c) nobody states it → the nest's
  claim is *checked*, never trusted: itself → refused; already removed →
  nothing to write; not a verified member of this replica's fleet view, or
  not 32 bytes → refused; a member that states a *different* row → refused
  (a re-pairing nest or a mis-stating member — *A disagreement is the user's
  to settle*, below); a member that has stated no row yet (a pre-binding
  build) → accepted; no claim at all → the row names no fleet member and the
  nest deletion proceeds alone. **A refusal deletes nothing and tells the
  user the device was not removed** (`devices.error_remove_own_device` /
  `devices.error_remove_unverified_device` /
  `devices.error_remove_row_mismatch` on `error-message`), so the row is
  still there to retry from — resolution runs *before*
  `fauna.sync.devices.delete`, and a runtime that cannot answer — up or not
  (*The completion rule*) — refuses rather than skipping the fleet leg. The writer door keeps the same
  honest-writer check behind whatever was resolved:
  `fleet_removal::write_removed` — reached through `settle` (`Gone`),
  `complete_pending` and `remove_fleet_member` — refuses this device's own id
  and any id its fleet view does not verify, and is a no-op on an id already
  removed. This narrows the *writer* only — readers still exclude
  unconditionally (the device-set bullet's ruling is untouched), and an
  enrolled insider can still write any `Removed` row directly, the accepted
  vandalism-not-escalation posture (what that write costs a guardian-enrolled
  device on a supervised account, and what answers it:
  [`../behavior/family-safety.md`](../behavior/family-safety.md) § Full
  visibility for young children → *The device marker*). **Bounds, stated:** a member that never
  runs a binding build (a stolen laptop on an old version) states no row,
  so for it arm (c) is all there is: the nest can no longer aim its removal
  at a *stating* device or at the caller, but it can still strip the claim
  (the laptop is spared, silently) or aim it at another non-stating member
  — closed by the member-addressed door below, whose page surface is built on
  tui, macOS and iOS (2026-09-25) and owed by linux, windows and android (web
  a declared absence);
  and the label a user picks a row by is the nest's too unless it rendered
  from its seal (`path-sealing.md` owns the device label), so a nest that
  swaps whole rows' labels still misleads the *choice* — the this-device
  marker (`../behavior/devices.md` § This-device marker) covers the caller's
  own row, nothing covers a sibling's.
  **A disagreement is the user's to settle, against client-held identity
  (ruled 2026-09-19; built 2026-09-25 — the rule, the runtime's read and
  removal doors, and the page surface on tui, macOS and iOS; linux, windows
  and android owe the render, web a declared absence).**
  The binding trusts each member's own statement, so by itself it lets an
  enrolled device decide whether it can be removed: a stolen device that
  states a row other than its real one — a fabricated row, a sibling's, the
  caller's own — turns every removal of its real row into the *different
  row* refusal, with an honest nest, for ever. One row-to-member binding
  cannot serve both adversaries: when a member's statement and the nest's
  claim disagree, the replica holds **no fact that says which one lies** (a
  re-pairing nest and a mis-stating member present identical facts), so the
  row gesture keeps refusing — trusting the claim would hand the nest back
  its aim at a stating sibling — and the disagreement goes to the one party
  who can settle it, the user, through a **second, member-addressed door**
  (`fauna_core::fleet_removal::resolve_member_removal`): the user picks a
  fleet member by its fleet id and that id is the target. It takes no nest
  input, so the nest cannot aim it, and reads no row statement, so the
  member cannot veto it; this device's own id is refused, an id the fleet
  view does not verify is refused, an id already removed writes nothing. It
  is one leg — no nest row is chosen, so there is no nest deletion to
  bracket and nothing to stage; a failed write says so and the member stays
  listed to retry from. The member's nest row, if it still has one, then
  comes off by the ordinary row gesture, whose claim now names a removed id
  — or none, once its grant is revoked (*The nest half follows merged state*,
  below) — and needs no fleet write. **Who is offered the door: every verified member
  other than this device that no roster row accounts for**
  (`fauna_core::fleet_removal::unaccounted_members`) — and a row accounts
  for a member only when removing that row resolves, by the rule above, to
  **that member alone**. The derivation is the rule itself run over the
  roster, so nothing a member can state takes it off the list while keeping
  it irremovable: a fabricated row or the caller's own resolves nowhere; a
  sibling's row makes that row resolve to two members, so *both* are listed
  and the user removes one by its key rather than both by the row; a
  non-stating member whose claim the nest stripped or re-aimed is listed
  (the bound above, closed by the same list); a member whose nest row was
  deleted alone — every removal made from web — is listed; and an honest
  device left stating a stale row (an adopt restates on the device's next
  pass; a device lost before it keeps the old statement) is either still
  removable through the stale row while that row exists or listed once it
  does not — the same ruling, no separate arm. **What the user confirms
  against, stated plainly because it is thin:** a fleet member's only
  client-held identity is its **fleet-id fingerprint**; its enrollment time
  is carried too but is the member's own self-signed word, a hint and never
  proof; its device *label* is the nest's and is never shown for it. The
  user settles by elimination — every device still in hand shows its own
  fingerprint on its own devices page, and the member to remove is the one
  that matches none of them. **Bounds, stated:** a user who confirms without
  comparing can still be steered — by a nest's labels, as before — into
  removing a live sibling; that costs a re-enrollment, not confidentiality,
  and the member they meant is still listed afterwards. And a member-door
  removal leaves the device's nest row until the row gesture follows — with
  a hostile nest that row was never the client's to delete anyway — while
  its connection auth and bearers end with its grant, at the next full pass
  (*The nest half follows merged state*, below). The refusal's copy is its own
  (`devices.error_remove_row_mismatch`): unlike the unverified-member
  refusal, which a sync can clear, no retry clears this one, so it never
  says to try again.
  **The completion rule: the removal is crash-safe across its two legs
  (ruled + built 2026-09-19).**
  Two writes on two systems, and until this rule the second was best-effort
  with no memory: a failed write, a runtime that was down, or a client killed
  between the legs left the nest row deleted — nothing to retry from — and
  the device a verified member and wrap target for ever, the user having been
  told it was removed. The nest deletion is the **single decision point**,
  and a **durable intent** brackets it
  (`fauna_core::fleet_removal::PendingFleetRemoval` — the row plus the
  *resolved* fleet ids, never a principal the nest hands back later). It
  lives in the per-actor credential slot beside the registration latch, not
  in the replicated store: it is this machine's own unfinished business, it
  must be writable before any plane write is, and every process sharing the
  slot sees it — on a desktop the co-located agent usually holds the pump.
  Staged (verified by read-back, decoded) **before** `fauna.sync.devices.delete`; an
  intent that cannot be staged deletes nothing. Every read-modify-write of the
  staged intents runs under the slot's own write section, so a co-located
  process's stage, sighting or clear is never overwritten by a stale copy.
  Settled on the deletion's
  outcome (`NestDeletion`): **gone** — deleted, or `not_found`, the
  deletion's own postcondition however it came about — re-stages the intent,
  journals every `Removed` row and only then clears it; **kept** — the nest
  definitively refused, by a refusal the page's error mapping names
  (`conflict`, a malformed request, and the deletion's own
  `fauna.sync.guardian_marked`, mapped to `Conflict`; the former
  `fauna.sync.sole_source` retired with the role contraction) — clears
  it unwritten, because `Removed` is absorbing and the user was told the
  device stays (which is also why writing `Removed` *first* was rejected);
  **unknown** — a transport failure, a reply lost after the nest acted, a
  refusal the page's error mapping does not name, a crash before the reply —
  leaves it staged. **The reconcile**
  (`fauna_sync_engine::fleet_removal::complete_pending`, the pump's step
  after the reclamation pass, so at every assembly's first full pass and
  every pass after) finishes whatever is staged, with no user gesture: it
  reads the roster — only while something is staged — and a row that is
  absent means the deletion happened (complete), and an unreadable roster
  means wait. **A row still present is not yet evidence (ruled + built
  2026-09-21)****:** a pass
  cannot tell a deletion that never happened from one still in flight — the
  runtime passes between the page's commands, and on a desktop the
  co-located agent's pump passes on its own clock — so the first pass to find
  the row records that sighting in the intent and waits, and only a later
  pass still finding it `fauna_core::fleet_removal::DELETION_IN_FLIGHT_BOUND_MS`
  (one hour, hundreds of times the deletion's own 5 s deadline) after the
  sighting drops the intent unwritten — the user's row is there to retry
  from. The bound also caps what a never-happened intent costs: a roster read
  per full pass, ending within the hour. A re-stage is a new flight and
  clears the sighting; a sighting is recorded, and a drop carried out, only
  on the intent the pass judged; the rule itself is pure
  (`fauna_core::fleet_removal::reconcile_verdict`). The sighting is a slot
  group of its own (`row@ms`), which a reader from before it drops alone, so
  the spelling stays additive in both directions: an older process reads
  every intent unchanged, and a slot it rewrites merely loses sightings,
  which the next pass re-takes (a later drop, never an earlier one). Trusting the roster here is sound: the
  targets are client-verified and the user asked for the removal; the nest
  only answers whether its own half happened, and lying either way gains it
  nothing it could not do by refusing the deletion. The verification question
  (`nest/common.md` § Client-state recoverability) holds at every write: die
  after the stage → reconcile completes it once the roster shows the
  deletion landed, or drops it once the row has outlasted the in-flight
  bound; die after
  the nest deletion → reconcile completes it; die between two `Removed`
  writes → the intent is still staged and `write_removed` (via `complete_pending`) is idempotent;
  and a pass that lands inside the deletion's flight only waits, so the page
  failing to settle afterwards strands nothing.
  **An absent runtime refuses too (ruled + built 2026-09-19)****:** every runtime-hosting seat starts its runtime fire-and-forget at login and best-effort, so on all six the handle is absent while the assembly runs and for a whole session when it failed — and resolving *nothing* then let the nest deletion proceed alone, this rule's own leak. The door is now one shared adapter (`fauna_account_seams::fleet_removal::RuntimeFleetRemoval`, re-exported at `fauna_client_account_runtime::fleet_removal`; wired by tui, linux and the `fauna-ffi` seam, and served to web's Devices page by its core chunk through the account port) that answers `FleetRemovalRefusal::Unavailable` at resolution, staging and settling alike: nothing is deleted, the page says so, the row is there to retry from. tui wires it when the machine is built rather than at its account-store-ready edge, which had left the machine door-less — web's shape — until then. **Bound, stated:** a session whose assembly failed cannot remove a device from that app until a relaunch assembles it — on web, a tab that hosts no runtime (a second tab of the same account) refuses the removal the same way; the gesture works in the hosting tab. Web's Devices machine sits in another wasm chunk than the runtime and reaches this same adapter through the account port (built 2026-09-30, [`account-client-lifecycle.md`](account-client-lifecycle.md) § The client-side lifecycle → *The account port*), so its removal stages and settles its fleet leg exactly as on the six native apps.
  Pinned: `fauna-sync-engine`'s
  `a_staged_removal_the_page_never_settled_completes_on_the_next_pass` (the
  review probe's scenario, green — and it waits rather than guesses while the
  roster is unreadable), `a_refused_nest_deletion_clears_the_intent_and_writes_nothing`
  (the reconcile's half waits out the bound first),
  `a_settled_removal_lands_at_once_and_leaves_nothing_staged`,
  `a_pass_during_the_deletions_flight_keeps_the_removal_it_then_completes`,
  `an_unknown_deletion_outcome_leaves_the_removal_for_the_reconcile` and
  `a_gone_settle_re_stages_a_removal_a_racing_pass_dropped`;
  `fauna_core::fleet_removal`'s
  `a_row_still_held_is_waited_out_before_the_intent_is_dropped`,
  `a_re_staged_removal_is_neither_stamped_nor_dropped_on_an_older_sighting`
  and `the_sighting_spelling_is_additive_in_both_directions`;
  `fauna-devices-machine`'s
  `remove_device_stages_before_the_nest_deletion_and_settles_on_its_outcome`,
  `a_removal_whose_intent_cannot_be_staged_deletes_nothing` and
  `a_kept_device_deletion_maps_to_conflict`;
  `fauna-client-account-runtime`'s
  `an_absent_runtime_refuses_the_removal_rather_than_resolving_nothing`.
  **Each seat's OWN build-time wiring is pinned too (ruled + built
  2026-09-25)****,** against a `DevicesMachine::fleet_removal()`
  read-back rather than the shared adapter alone — so a seat that stops
  wiring the door reds only its own pin: tui's
  `the_removal_door_refuses_until_the_account_runtime_lands` (now built over a
  real `DevicesState::build` machine, not a hand-built door), `fauna-ffi`'s
  `the_builder_hands_the_machine_the_fleet_removal_door` and linux's
  `the_page_machine_carries_the_fleet_removal_door` (built over
  `build_wired_devices_machine`, the fn the page builder takes its machine
  from). One gap remains open. The
  `fauna-ffi` pin can race sibling tests that install a store into the
  process-global host. **And end to
  end (2026-09-20):** the tier_3 kill-between-the-legs journey
  `test_crash_recovery_journeys.py::test_kill_client_between_the_removal_legs_reconcile_finishes_it`
  — two really enrolled seats on one account, the remover SIGKILL'd while the
  nest holds `fauna.sync.devices.delete` parked ahead of its handler (the
  `rpc_hold` seam, so the window is arranged rather than raced), the deletion
  then committed with no client alive to settle it, and a relaunched remover
  journaling the sibling's `Removed` row off its carried intent with no user
  gesture. Its two ground truths are what keep it from passing vacuously: the
  nest's roster read over the side channel (the deletion provably committed —
  on a roster that still held the row the reconcile correctly writes nothing:
  it waits, and drops the intent once the in-flight bound has passed) and the staged intent read out of the credential
  slot *after* the kill (the client provably died between the legs). Wired for
  tui and
  linux directly and for windows/macOS/iOS/android through the one
  `fauna-ffi::devices::build_devices_machine` seam, and for web through the
  account port (its leg crosses a wasm chunk boundary; the forwarder →
  loopback → `serve` wiring reruns the removal-order pins, and
  `fauna-wasm`'s `account_runtime` tests prove the core half over the real
  browser runtime). Pinned by `fauna_core::fleet_removal`'s rule
  tests, tier_1 tests on the `SettleFleetRemoval` and `ResolveFleetRemoval`
  Cmds (`the_honest_writer_refuses_itself_and_strangers_through_the_settle_door` pins the writer's arms through `settle`), `the_pump_publishes_the_latch_row_as_the_device_endpoints_statement` (the pump's own feed of the row statement; the tier_3 journey `test_device_member_removal.py` witnesses the same feed end to end), `fauna-devices-machine`'s resolve-before-delete and refusal pins, and
  the statement's own journey through a real nest
  (`conformance_account_state_walk`'s
  `a_members_row_statement_reaches_a_siblings_removal_facts`). **The
  member-addressed door is BUILT (2026-09-25; which apps paint the page
  surface, and that its confirm addresses a card by key, are
  `../ui/devices.md` § Implementation status today's):**
  the pure rule (`resolve_member_removal` and `unaccounted_members`, pinned in
  `fauna_core::fleet_removal` — `a_member_that_misstates_its_row_is_listed_and_still_removable`
  and its siblings: the two-member row, the re-pairing nest, the honest stale
  statement); the runtime's read door `AccountStoreHandle::unaccounted_fleet_members`
  (this device's fleet id plus every unaccounted member with its asserted
  enrollment instant, `fauna_sync_engine::fleet_removal::unaccounted_members`)
  and its one-leg writer `remove_fleet_member` (resolve by key, then the same
  honest-writer door `settle` and `complete_pending` share
  (`write_removed`); nothing staged — there is no
  nest deletion to bracket), both served locally like the quartet and pinned
  by `the_member_door_lists_the_unaccounted_and_removes_by_key` over two real
  enrolled runtimes; the `FleetRemoval` port's `fleet_members` / `remove_member`
  on the shared `RuntimeFleetRemoval` adapter (an absent runtime lists nobody
  and refuses, `an_absent_runtime_neither_lists_nor_removes_members`);
  `DevicesMachine`'s member list and `remove_member`, with both fingerprints
  rendered in the snapshot through the one `fauna_core::format::fleet_fingerprint`
  (`the_member_card_and_the_own_row_render_one_fingerprint`); the tui surface
  (`ui/devices.md` § Members without a matching entry); and end to end
  `test_device_member_removal.py` — a sibling seat mis-stating its row under
  the e2e-only `FAUNA_E2E_STATED_ROW` pump override (compiled out of release
  builds; the only way to produce the statement, which the harness cannot
  forge) refused by its row, listed, removed by key, its nest row then finished
  by the row gesture; and the row-deleted-elsewhere sibling listed by
  fingerprint and removed. The refusal's copy now ends with the pointer to the
  group.
  The live e2e convergence assertion this once called "still owed" is the
  kill-between-the-legs journey documented just above — closed once
  `fauna_client_account_runtime::device_set_state_json` (a debug/e2e-agent
  gated reader over the same `fauna.state.device-set` plane, convention 15
  rule (a)) gave it an observation path, with tui,
  linux, android, macOS, iOS and windows wired (macOS and iOS share one
  `FaunaKit` handler over the gated `FfiNestClient` export; windows wraps the
  same export in a Debug/e2e-agent-only `NestRpcClient` method, because the
  production `windows-ffi` bindings do not carry it). Windows has no unclean
  kill, so the journey itself never runs there; the reader's presence on every
  app that hosts the account runtime is pinned instead by
  `test_account_runtime_pump.py`'s reader test. Trigger
  (b)'s closure is built (2026-10-01): severance lands at the next
  origination after the remover's closure merges, whether or not the tip
  names the removed device (*The mint protocol, trigger (b)*).
  **The nest half follows merged state — a removed member's grant is revoked by key, at every nest (measured red + ruled 2026-10-01; built 2026-10-01 — § Implementation status today).** Without this rule the removal's nest half reaches one nest. `fauna.sync.devices.delete` goes to the nest the removing app is bound to, and the member-addressed door sends nothing nest-side at all. A device registers only at the nest it is bound to ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4: a secondary leg needs no device principal), so a device bound to another of the account's nests is, at the remover's nest, a member no roster row accounts for, and its removal is its fleet row alone. **Measured 2026-10-01, before the build,** on two nests each linked at the other: the one device bound to B is lost, a device bound to A removes it by its key, and after six rounds — each with a leg run that completed B — B still mints bearers for the removed key, still lists its row, and its gate's watermark stands at the lost device's mark while B's log moves past it. So removal was not the control clause (1) says it is: B's gate holds every retire above that mark for good, the removal evidence of [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7 included; a stolen device goes on authenticating at its own nest after the user removed it; and the writer-key tombstone a removed device's ending rests on ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal → *Principal succession after a device delete*) never lands there. **The rule: at every nest a runtime completes — its bound nest in each full pass, each linked replica in each secondary-leg run — it reads that nest's roster and revokes, by key, the grant of every row whose principal is a fleet id its merged state reads `Removed`.** The request is `fauna.sync.device_grant.revoke` on the account's own session ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model owns the kind and its two arms): the nest clears the grant, tombstones the key, ends every session and socket it minted, and — by the gate's own predicate, clause (1) — stops counting its walk mark. The roster is read only once merged state reads some device removed; the leg revokes after that nest's reconcile and ahead of its own retires, so the run that revokes is the run whose retires land; and whichever process pumps runs the bound nest's half, the seedless agent included, since the arm needs the account's session and no seed. No new kind, and nothing new at rest. **Why by key.** The key is client-held truth — the fleet id, read from a `Removed` row this replica verified — and `Removed` is absorbing, as the tombstone is: revoking that key is right wherever it rests, and can never reach a device that is still a member. The roster decides only whether to ask. A nest that hides a principal spares itself a request it could as well have refused, and one that claims a removed key on another row gets that key revoked, which harms no one. The arm never names this device's own key: a removed device's own ending is the posture doc's. **What the nest keeps: the row.** Its label and its places stay, keyless — what a sign-out leaves ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model, *never the whole device row*) — and the row comes off by the ordinary row gesture from an app bound to that nest. **A guardian-marked row is left alone:** the arm skips a row the roster flags, and the nest refuses the session arm for a marked row's key as it refuses that row's deletion ([`../behavior/family-safety.md`](../behavior/family-safety.md) § Full visibility for young children → *The device marker* owns the refusal). **Bounds, stated.** The roster read costs one request per nest per full pass or leg run for as long as merged state reads any device removed, which after the first removal or sign-out is for good. A nest no runtime of the account completes — no live device bound to it, and linked from no nest a live device is bound to — keeps the grant, as it keeps everything else it was given. A removal reaches a linked nest's grants when a seed-holding device's leg next reaches that nest, and no sooner (ruling 4's bound). **Refused: deleting the row** — `fauna.sync.devices.delete` at each nest, for the row that claims a removed key. A deletion is addressed by row, and which key a row carries can change between the roster read and the request: a machine that signs out and in again enrolls a fresh principal on the same named row. A row-addressed request can therefore end a live successor; a key-addressed one cannot. **Refused: listing every nest's roster on the Devices page**, so that the row gesture exists for a device bound elsewhere: a surface on seven apps for a duty the user discharged when they removed the device, with the gate held until they find it.
  **The steady state, measured:** a 64-device fleet driven through 200
  sign-out → sign-in cycles against the real nest holds its live-entry count
  flat (the count after cycle 200 within O(members) of the count after
  cycle 20, and never a `scope_full`) —
  `bins/fauna-nest/tests/conformance_account_state_walk.rs`, the reclamation
  section. **Bounds, stated:** (i) the reach gate for shredding makes "no
  member still seals under G" hold (the succession corner this bound once
  excepted, where admissibility diverged by `prior`, went with that arm —
  the *source of `prior`* bullet); a row
  published under a shredded generation is re-sealed by its writer —
  `device-endpoints` re-seals on any tip change already, and the general
  re-seal of one's own rows under a dead generation is the ratified loser
  re-seal pass — clause (3)(g)'s own arm; (ii) an enrolled device that died without
  signing out stays a wrap target and costs the fleet one cell per healer
  per generation until it is removed — removal is the control; (iii)
  concurrent first-need mints still fork and each loser costs a generation's
  rows once; losers go dataless and reclaim like any superseded generation;
  (iv) a fleet whose every member runs a pre-reach binary reclaims nothing —
  exactly today's growth, until one member upgrades; (v) an account-level
  row a departed device wrote (clause (3)(d)) keeps its generation from
  shredding — one pinned generation (and its escrow wrap) per such
  generation — only while no verified member can key it (the
  single-device sign-out → sign-in): clause (3)(g)'s hand-over arm
  re-seals it the moment a member does, which the *Escrow recovery*
  bullet's pass makes the next seed-holding sign-in; a removed device that
  held that generation's key keeps the reach it already had to that
  generation's rows, and every newer generation still excludes it; (vi)
  the re-seal pass costs one put per live account-level row per superseded
  generation that held it, count-neutral once clause (f) or the hand-over
  retire lands, plus at most one extra live entry per item per change of
  hander while a departed writer's row of it still stood.

  **A remove-account severs and retires the same way (ruled 2026-10-13; not built).** The gesture — its order, the credential snapshot its nest half rides, its budget and its offline bound — is owned by [`apps/account-scoping.md`](apps/account-scoping.md) § The scoping taxonomy → *Erasure follows scope* → *Removing an account retires this machine from it, as a sign-out does*; what it owes this gate is exactly the sign-out's: the mark stops counting with the grant, removal is the control, and a removal the nest never heard leaves a dead machine that blocks reclamation until the user removes it from another device.

## Implementation status today

- **BUILT 2026-10-14 — § The generation machinery → *Fleet-scope reclamation*, the shredder's veto → *the shred cell*, the group plane's cert is signed.** `fauna_core::group_generation::group_shred_signing_bytes` now commits to the BLAKE3 of the carried `authorization` after the generation id, the shredder and the stamp, under the bumped frozen tag `GROUP_SHRED_SIG_CONTEXT` = `fauna.group.generation-shred.v2`; `sign_group_shred` and `GroupShredAttestation::verifies_at` pass it, so a copy of an authored group attestation with its cert swapped no longer verifies at the shredder's cell, is refused at first contact, and loses `join_group_generation_shred` to the honest row in both orders — the group cell now holds "never displaced by bytes from outside its own shredder's cell" as the account plane's `ShredAttestation` (every field signed) already did. Proofs: `a_swapped_cert_never_displaces_an_authored_group_shred` (`group_generation.rs`; red before the fix, red again with the cert hash dropped from the preimage) and the group-plane cert-swap arm of `the_shred_kinds_register_and_never_yield_a_shredders_cell_to_a_forgery` (`fauna_protocol::merge_policy`). The 0.1.x window owed no migration of v1-signed rows.
- **Ruled and BUILT 2026-10-13 — § The generation machinery → *Fleet-scope reclamation*, the shredder's veto → *the shred cell*.** **Wire.** `fauna.state.generation-shred` (`fauna_protocol::merge_policy::KIND_GENERATION_SHRED`; `CrdtPerField`, `FleetOnly`, `Gen0`) and `fauna.group.generation-shred` (`fauna_protocol::group_state::KIND_GROUP_GENERATION_SHRED`; `CrdtPerField`, machinery root), one cell per (generation, shredder) at `<generation-hex>/<shredder-hex>`. The records are `fauna_core::generation::ShredAttestation` and `fauna_core::group_generation::GroupShredAttestation` (the latter carrying the cert as `authorization`), signed by `sign_shred` / `sign_group_shred` over the unchanged frozen preimages. `verifies_at` is pure over the record and the cell key; `is_authored_at` adds the reader's half — `FleetView::is_verified_member` on the account plane, `GroupAuthority::verify_device_cert` covering the shredder on the group plane. The joins `join_generation_shred` / `join_group_generation_shred` rank (verifies at the cell, bytes), decode-or-fail; the merge arms `merge_generation_shred_entry` / `merge_group_shred_entry` parse the key and delegate; `validate_generation_shred_adoptable` / `validate_group_shred_adoptable` refuse a non-verifying row at first contact. Both mint records' `Shredded` is attribution only (`core`, `shredded_at_ms`, `shredded_by`); `GenerationMintRecord::shred_is_authored` and its group twin are gone, nothing migrated (the 0.1.x window). **Consumers,** each reading `fauna_core::generation::authored_shred_generations` over the shred kind's merged rows at the reader's current view: `ReclaimState::read`'s `shredded` set (step 7's retires, receipt sweep and key drop, step 8's residue, the publish diff's dead rows), `mint_licence` (an authored cell for the generation — the opened record's marker licenses nothing), `generation_tip::generation_key_for` (read behind a marker only), `linked_leg::shredded_generations`, and `AccountStatePlane::drop_retained_if_shredded`, which now fires on the shred kind's rows. **The pass.** Step 7 writes the marker where the mint row still reads `Minted` and its own cell always; `ReclaimState`'s `marked` set (a `Shredded` mint row with no authored cell) stays in the shredder election and in `reclaimable`; the step-7 retire takes a dead generation's `<g>/*` cells behind the dataless belt once its mint row has settled, never forgetting them locally, and the linked leg's `shred_retires` owes them with the mint row. The group plane has no consumer yet (the scheme's shred clause: the first one is built to that call). **Tests.** The pin `generation_reclaim::tests::a_byte_greater_forged_marker_never_suppresses_an_authored_shred` (an honest shred and an all-`0xff` forged marker merged in both orders; the generation counted authored by the pass, the read path, the linked-leg sweep and the predecessor licence) — red on the pre-build tree; `a_forged_marker_is_attested_by_the_shredder_and_never_rewritten` (the re-election); the walk hook's `the_walk_hook_drops_a_retained_key_on_an_authored_shred_only` (a forged marker and a forged cell drop nothing). Core: `a_verifying_attestation_is_never_displaced_by_byte_greater_bytes`, `the_shred_join_is_commutative_associative_idempotent`, `only_an_authored_cell_puts_a_generation_in_the_authored_set`, `an_attestation_binds_the_generation_the_shredder_and_the_stamp`, `an_attestation_by_a_non_member_or_a_removed_member_is_unauthored`, and the group plane's `the_group_shred_join_keeps_the_verifying_row_and_obeys_the_laws` and `only_an_authored_shred_by_a_live_authority_device_may_drop_a_key`. Dispatcher: `the_shred_kinds_register_and_never_yield_a_shredders_cell_to_a_forgery`, plus both kinds in `an_undecodable_row_at_an_occupied_cell_is_refused_never_kept`. The unauthored twins (`an_unauthored_shred_*`, the dialed-leg pin in `peer_dial_convergence`) now pair a marker with no authored cell. Mutation-verified: `is_authored_at` reduced to `verifies_at`, or the join ranked on bytes alone, reds the pin or the join pin.
- **Ruled and BUILT 2026-10-08 — § The generation machinery → *Fleet-scope reclamation*, the shredder's veto → *the authored shred*.** Its carriage is superseded by the 2026-10-13 entry above — the signature left the mint row for the shred cell, and every consumer named here now reads the cells; the record of what was built then: Wire and writer: `Shredded::shredder_sig` (skipped when empty), `sign_shred`, `GenerationMintRecord::shred_is_authored` (`libs/fauna-core/src/generation.rs`, five tier_1 tests); `generation_reclaim`'s step 7 signs. Consumers, each gated on `shred_is_authored` at the reader's current view: `generation_tip::generation_key_for` (a per-pass `Option<&FleetView>`; `None`, the walk's per-row open, reads unauthored), `AccountStatePlane::drop_retained_if_shredded`, `ReclaimState::read`'s `shredded` set (whose step-7 loop now also re-drops the key, eventually) and `mint_licence`, `linked_leg::shredded_generations`. Tests: an unauthored twin per consumer (`an_unauthored_shred_*`, `the_walk_hook_drops_a_retained_key_on_an_authored_shred_only`) beside the authored fixtures, and `peer_dial_convergence`'s dialed-leg pin carrying both; every gate red-verified by reverting it to the bare `Shredded` match (`mint_licence`'s by `the_predecessor_arms_shred_licence_is_an_authored_shred_only`).
- **Ruled 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(j), the let-go (built 2026-10-02, tui first), and the succession clause of *Escrow recovery* (built 2026-10-02).** The kept-wrap half's code facts, tests and measurement are [`owner-key-material.md`](owner-key-material.md) § Implementation status today's (the kept-wrap entry). **The let-go:** `libs/fauna-account-plane/src/generation_let_go.rs` holds the read (`dead_generations`) and the act (`let_go`). The read takes the generations the fleet plane's relay rows are sealed under (`AccountStore::relay_rows_sealed_under`, the `relay_rows.generation_id` index) that are not `Shredded`, are keyed by no device here and are listed in no other verified member's reach. The bound holder must also not wrap them: the answered-empty bit, else one filtered `fauna.generation.escrow.get`. The act re-reads that, retires each row by its coordinates through `AccountStatePlane::retire`, retires the mint rows and receipts behind the dataless belt, deletes the wraps at the bound holder (`linked_leg::sweep_wraps`) and records the generation in the store's let-go set, which the secondary leg's step 7 sweeps at every linked holder. A replica that did not act forgets its copies at its next read, once the nest's listing no longer shows them. The runtime serves both as pass-bound commands (`AccountStoreHandle::dead_generations`, `::let_go`). Pinned by `a_dead_generation_is_let_go_by_the_users_act_and_a_live_siblings_rows_stay` (`bins/fauna-nest/tests/conformance_account_state_walk.rs`; measured: 8 rows under the dead generation, 19 live fleet entries before the act and 9 after, wrap gone, the siblings' rows unchanged). Which apps render the surface is `ui/settings.md` § Implementation status today's. **Residual:** the read asks the bound holder alone; a wrap that only a linked holder still keeps does not stop a generation reading dead.
- **Measured and ruled 2026-10-01, built 2026-10-02 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(i), the predecessor arm.** **What is code.** `AttestedPredecessors::retired_machinery_keys` (`libs/fauna-account-plane/src/attested_predecessors.rs`) derives, in the one derivation beside the delegable schedule and the mint kind's pair, each attested predecessor's generation-0 pair for every kind the registry seals fleet-only at `Gen0` — the eight machinery kinds today, taken from the registry. The driver hands them to the bound nest's fleet plane alone (`AccountStatePlane::with_predecessor_machinery_keys`); the peer leg's and the linked leg's source guards refuse the call, as they refuse the carries' keys. `AccountStatePlane::open_predecessor_machinery_row` is the one open that tries them — form v1 only, the hit pair first, a row no pair opens remembered and not tried again — and `open_relay_row`, the walk and the publish diff keep the plane's own keys. `generation_reclaim::ensure_reclaimed` runs the arm as its step 9, skipped outright on a plane handed no keys: over the relay rows of writers that are no verified member, a row the retired keys open and the plane's own do not is retired under its own item key and its relay copy forgotten on `Retired`/`Gone`; a mint row only on licence 1 (this device's own row at that generation's item key carries the merged record and is listed) or licence 2 (the record or merged state reads `Shredded`, retired with the `Dataless` belt). **The proof** (`bins/fauna-nest/tests/conformance_account_state_walk.rs`, red-verified by not handing the plane the keys): the succession flow that keeps S and D running ends with no live row a predecessor device wrote — 5 left without the arm, all form v1, clause (g)'s let-go having taken the endpoints row — and so does a predecessor fleet of three devices (`a_three_device_predecessor_fleet_leaves_no_row_on_the_successors_feed`; 9 without). `a_predecessors_mint_row_no_successor_device_carries_is_kept`: a successor device that attests the predecessor and keys nothing retires the predecessor's enrollment, reach, escrow-target and receipt rows and keeps the mint row (5 form-v1 rows without the arm, 1 with). `a_late_carrier_reads_the_mint_record_after_the_predecessors_row_is_retired`: S's first pass retires the predecessor's mint row on licence 1 before the generation shreds, and a successor device arriving afterwards with the generation's key merges the record from S's row and reads every pre-succession row. Unit pins: `attested_predecessors`' `the_retired_machinery_keys_open_every_fleet_only_generation_0_kind_and_no_delegable_one`; `generation_reclaim`'s `the_predecessor_arm_retires_only_a_non_members_row_the_retired_keys_open` (a member's row and a row no key opens are left); and `conformance_succession_carry.rs`'s `a_predecessors_fleet_only_generation_0_row_stays_unopened` holds with the new keys attached — the walk still opens nothing. **Bound (γ), measured 2026-10-02:** D, the seedless successor seat, walked the feed before the arm ran and ends holding 5 relay copies of the predecessor's rows for one predecessor device and 9 for three — exactly the 2N + 3 form-v1 rows the arm retired at the nest (the device-endpoints rows go with the carried generation's shred residue, clause (h)). A walk does not prune another writer's retired row, so they stay for that replica's life. **The measurement that ruled it (2026-10-01).** Before the build nothing opened a retired identity's generation-0 machinery row: `AttestedPredecessors` derived the mint kind's pair alone and only the walk read it, and every retire in `generation_reclaim` followed `AccountStatePlane::open_relay_row`, which tries the plane's own keys. The nest's side is read from code: `folders` and `sync_changes` are both `Succession::Move(MoveShape::Plain)` in `bins/fauna-nest/src/db/actor_tables.rs`, a state scope is a reserved folder of the actor (`CacheDb::find_state_scope`), and `successions.rs` touches no class-2 row. **Measured 2026-10-01** on the flow of `the_succession()` in `bins/fauna-nest/tests/conformance_account_state_walk.rs`, instrumented for the measurement and restored. Predecessor fleets of 1, 3 and 6 devices on one generation; the predecessor devices' grants revoked at the ceremony, which is the production shape (`sync_devices` stays with the retired identity); then two successor seats, one carrying the generation's key and attesting the predecessor and one seedless, run for 3, 7 and 15 full passes. Before the ceremony the feed held 16, 22 and 31 live rows. Afterwards the predecessor devices' rows still live were 6, 12 and 21, the same at every sample: per device a device-set row and a reach row (form v1) and a device-endpoints row (form v2, under the carried generation), and per fleet an escrow-target row, a mint row and an escrow-receipt row (form v1). The ten account-level rows under the carried generation were handed over and retired by the third pass (clause (3)(g)'s hand-over arm). The successor's own live rows numbered 21. The residue was 6,737 B sealed for one device and 2,773 B more per further device. A fresh seed-only successor device was served every one of those rows on its first walk, opened none of them, and kept them in its relay plane. The flow's one generation is carried, so bound (β) measured zero. With the predecessor seat's grant left live, as the committed fixture has it, the predecessor's count is the same and one successor top-up cell more stays live.
- **Ruled and built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(g), the let-go arm.** `generation_reclaim::reseal_superseded`, over a generation in `handing` (this device its hander), retires a departed writer's device-scoped row it can open with no put before it, through the ordinary `Retirer::retire`, and forgets the relay copy on `Retired` or `Gone`; a deferred or withheld retire is asked again next pass. Nothing changed at the nest. The veto's half was already code: `uncovered_row_under` counts such a row covered. **Proven:** `bins/fauna-nest/tests/conformance_account_state_walk.rs::a_successors_first_mint_supersedes_the_carried_generation_and_a_late_seed_only_device_reads_every_pre_succession_tip_sealed_row` — after the hand-over, no live row rests under the carried generation, both successor seats read it `Shredded` and the holder keeps no wrap of it, red-verified without the arm (one live row, the predecessor device's, left under it); its fixture `the_succession` now also revokes the predecessor device's grant beside the wrap burn, the production fact the gate depends on, and the test reads the seedless sibling's top-up of the carried key after every round, since the generation can now shred, and its key be dropped, within them. The `generation_reclaim` unit pins: `the_hander_lets_go_a_departed_writers_device_level_row`, `a_verified_members_device_level_row_is_not_let_go_by_a_sibling`, `a_member_that_is_not_the_hander_lets_go_of_nothing`. **Measured 2026-10-01, before the build,** on `bins/fauna-nest/tests/conformance_account_state_walk.rs::a_successors_first_mint_supersedes_the_carried_generation_and_a_late_seed_only_device_reads_every_pre_succession_tip_sealed_row`, instrumented for the measurement and restored. After seven full passes of the two successor seats, the predecessor device's device-endpoints row is the only live row under the carried generation, `AccountStatePlane::any_row_sealed_under` answers true at both seats, and neither pass retires or shreds anything. With that one row retired at the nest by hand, the next pass shreds the generation and both seats read it `Shredded`. The retires behind the shred — the mint row, the receipt and the escrow sweep that rides it — were then withheld: the fixture's predecessor seat keeps a live grant on the suite's one actor, and its walk mark held the gate's watermark at 16 against rows up to 41. With that grant revoked the same pass sweeps the wrap. In production the predecessor's device holds no grant on the successor's account (the nest's `sync_devices` table stays with the retired identity at a succession), so the fixture is what differs; the build's proof has to model that. **Observed:** each replica's merged entry for a departed device's endpoints outlives the row (the successor seat still held the predecessor device's entry after the shred). **Ruled and built 2026-10-02:** the entry stays in merged state, and the peer leg dials an entry only while its device is a verified member of the replica's fleet view — owner [`account-sync-plane.md`](account-sync-plane.md) § The peer leg → *Discovery*.
- **Measured red, ruled and built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest half follows merged state*.** One function, `fauna_account_plane::removed_grants::revoke_removed_grants`, generic over the requester and outside the `account-driver` feature — it sends `fauna.sync.devices.list` and `fauna.sync.device_grant.revoke` with `fauna-protocol` types — called from the two places a runtime completes a nest. `account_driver::pass::pump` runs it against the bound nest on the account's session in every full pass, whichever process pumps, sharing the staged-removal reconcile's roster read when that ran (`PumpReport::removed_grants`). `linked_leg::complete_linked_nest` runs it against each linked replica as the leg's step 5, after that nest's reconcile and diff and ahead of its retires (`LinkedCompletion::removed_grants`). It asks the nest nothing until merged state reads a device other than this one removed, never names this device's key, skips a row the roster flags `guardian_marked`, counts a `fauna.sync.guardian_marked` refusal without failing, and never sends `fauna.sync.devices.delete`. All 7 apps and the sync agent carry it with no per-app code. The member-door bound above (*connection auth and bearers end with its grant, at the next full pass*) is code with it. Proofs: `conformance_account_plane_bind::a_lost_device_removed_at_the_other_nest_stops_counting_at_its_own_nests_gate` (the measured case: the lost device's nest stops minting for its key, its gate counts no mark, and it keeps the row) and `a_lost_device_removed_by_key_stops_minting_at_the_nest_it_shares_with_its_remover` (the bound nest's half; the remover's own grant stands), each red-verified by reverting its call; the `removed_grants` unit pins (nothing removed sends no roster request; this device's key is never named; only rows claiming a removed principal are revoked; a marked row is left alone; a roster already read is not read again). The nest's refusal for a marked row: [`../behavior/family-safety.md`](../behavior/family-safety.md) § Implementation status today.
- **Ruled + built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(d): the removal evidence is no longer the reclamation pass's to retire.** [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7 owns the rule, and its status entry the build and its proofs. Here: `generation_reclaim::Retirer::retire_removed_device` retires a removed device's `Enrolled` rows, its reach and its device-scoped rows, and no `Removed` row; `generation_reclaim::removal_evidence` lists the evidence for the secondary leg's removed-device arm, which retires it. Proof: `generation_reclaim::tests::a_removed_devices_enrollment_is_retired_before_its_removal_evidence` — two passes retire the enrollment and never the evidence, and the arm retires it behind them.
- **Built 2026-09-22 — the gate's watermark, and the pass's step timings.** § The generation machinery →
  *Fleet-scope reclamation*, clause (1) → *the gate's watermark* owns the
  ruling. What is code: `CacheDb::retirable_through_seq` (the lowest counted
  mark, the gate's own predicate) served on every class-2 page as the
  additive `SyncChangesListReply::retirable_through_seq`, after the
  request's own mark is recorded (`bins/fauna-nest/src/sync_handlers.rs`);
  `RelayRow::feed_seq` (the nullable, additive `relay_rows.feed_seq` column,
  added to an existing store by `migrate()`), stamped from the put reply
  (`AccountStore::stamp_relay_feed_seq`) and from every walked page;
  `AccountStatePlane::retirable_through_seq`, banked per walk; and the
  reclamation pass's `Retirer::withhold_above` — every retire carries the
  row's coordinate, a row above the watermark is withheld as a local
  `NotYetStable` (`ReclaimPass::withheld`), `sever_self` never withholds.
  Beside it, the instrumentation the measured prologue lacked:
  `PumpReport::timings` (`PassTimings` — the two walks and the four
  generation-machinery steps, and the whole pass) with `log_pump` naming
  the slowest step at info on every pass of a second or longer, so a sweep's
  app log attributes a long prologue without a hand count. **The measured
  floor, stated** (the 2026-09-22 `--app linux` sweep's captured logs, the
  session account at 33 devices): the reclamation's refused retires cost
  10–20 ms each — 222 refusals in about 4.5 s, the part this rule removes;
  the escrow recovery's 89 generations ended 24 s after the store mounted,
  walk included, and its per-generation work is already the ruled minimum
  (one filtered `escrow.get`, a KEM decap and a bundle write per generation
  a fresh sign-in cannot key — bound (ii) above, the skips for a shredded
  or sibling-keyed generation already built); the top-up's per-pass cost is
  a local walk of the mint rows with one up-front `wrap_coverage` read, two
  rows published while walking 110. The captured logs end before that
  prologue's pass completes, so the rest of the five minutes is not
  attributable from them — which is what the timings are for: the next
  whole-suite sweep's log names the step. Proofs: the nest's `the_retirable_watermark_is_the_lowest_counted_walk_mark`;
  the store's `a_relay_row_remembers_its_feed_seq_once_told`;
  `generation_reclaim`'s `a_row_above_the_gates_watermark_is_withheld_without_a_request`
  and `sever_self_never_withholds_against_the_watermark` (tier_1, the fake
  nest counting retires); and end to end against the nest's own handler
  table, `conformance_account_state_walk.rs::a_stranded_walker_costs_the_fleet_no_retire_request_per_pass`
  (a round across the live fleet under a stranded walker sends no retire and
  is refused none; removing the dead machine sends and lands them).
- **Built 2026-09-16 — fleet-scope reclamation.** § The generation machinery → *Fleet-scope reclamation* owns
  the ruling; what is code: the nest's `fauna.account.state.retire` kind
  behind its retention gate (`state_walk_marks`, fed by the additive
  `walker_id` beside `held_through_seq` on the feed; a mark counts while its
  key holds a live, non-tombstoned grant) and its generation belt
  (`no_rows_sealed_under`, over the additive `sealed_under` feed filter) —
  `bins/fauna-nest/src/db/account_state.rs`; the `fauna.state.device-reach`
  kind (`fauna_core::generation::DeviceReachRecord`, registered in
  `fauna_protocol::merge_policy`); the reclamation pass
  (`fauna_sync_engine::generation_reclaim::ensure_reclaimed`, the pump's
  step after the unkeyable pass) and the sign-out's plane leg
  (`generation_reclaim::sever_self`, the first act of
  `AccountStoreHandle::shutdown_for_sign_out`); the top-up pass reading
  reach as coverage and writing the courtesy row only for a target without
  one; the resolver consulting the retained bundle for candidacy (the
  *sealing-epoch axis* bullet's O-keyability clause, amended the same day);
  and `AccountStoreHandle::settle_fleet_removal` / `remove_fleet_member`, the devices page's seams into `write_removed`
  (with `resolve_fleet_removal`, its client-truth target resolution, since
  2026-09-19 — clause (4)).
  Proofs: `fauna_core`'s reach laws; `fauna_protocol`'s registry pin; the
  nest's four DB tests (retire without insert + coordinate memory, the
  gate, the belt, the filter); `generation_reclaim`'s tier_1 scenarios (a
  healer's own cell retired on the target's reach and not before, the
  courtesy row written only without a reach, the reach published on change
  only, a removed device's enrollment retired before its removal evidence);
  and the steady state end to end against the real nest — the suite's fast
  twin `conformance_account_state_walk.rs::a_fleet_survives_sign_out_cycles_at_a_steady_live_entry_count`
  (12 seats, 30 cycles) and the ratified measurement
  `…::a_64_device_fleet_survives_200_sign_out_cycles_at_a_steady_live_entry_count`
  (explicit, `--ignored`: about an hour in a debug build, every cost real
  crypto over a real feed). Three costs that measurement exposed are fixed
  in the same landing, all pure and production-relevant: the resolver, the
  reach and the read path consult the retained bundle (a commitment compare)
  before any KEM open; every pass's signature checks — enrollment certs,
  healer cells, receipts, mints — go through a bounded memo of checks that
  passed (`fauna_core::generation`, the *verified-signature memo*); and the
  walk's trial-open tries the kind that opened the previous row first
  (rows arrive in runs of one kind). A row the pass knows is dead everywhere
  is also **forgotten locally** (`AccountStore::forget_state`, no journal
  row, nothing published), so a long-lived replica's merged state stops
  growing with every cell ever walked. **Built 2026-09-27 — clause (3)(h), the relay plane's twin.** The pass forgets a dead gen-0 item's relay row under every writer (`generation_reclaim`'s `forget_dead_item`), and step 8 sweeps every relay row sealed under a `Shredded` generation (`AccountStore::relay_generations` / `relay_forget_sealed_under` over the new `relay_rows.generation_id` index). Pins: the steady-state sweep now also asserts that a replica living through every cycle keeps its relay plane flat. It was red at 180 → 480 rows over cycles 10 → 30 before the build, with 280 of those rows sealed under shredded generations and 144 being gen-0 cells and receipts whose merged entries were already forgotten. After the build it holds 56 → 56 rows over the same cycles (43 live entries at the nest), at an unchanged wall clock. `sqlite::tests::relay_rows_are_swept_by_the_generation_their_header_names` pins the index, the one-time back-fill and survival across payload eviction. `the_hand_over_keeps_a_row_it_cannot_open` now pins the residue arm on a row the hander cannot open.
  **Fixed 2026-09-19 — a departure retires only device-scoped rows.** As first built,
  the removed-device arm and `sever_self` retired every generation-sealed
  row the departing device wrote, so a sign-out and sign-in on a
  single-device account, or removing the device that first seated a
  community room, lost the account's reception keypair for good (and any
  held group root or custody registry row it wrote). Both arms now retire
  only the kinds `fauna_protocol::merge_policy::retired_with_its_writer`
  names (clause (3)(d)). No release-pipeline nest image carries the retire
  kind (the last `build-nest-image.yml` dispatch and the pinned production
  release both predate it), and an older nest answers every retire
  unsupported, so no production row was retired. Proofs: the
  registry pins `the_tip_sealed_kinds_are_exactly_the_registry_set` and
  `only_device_endpoints_is_retired_with_its_writer`; against the real nest,
  `conformance_account_state_walk.rs`'s control
  (`the_reception_key_reaches_a_later_device_while_its_writer_stays`), its
  sign-out and removal arms (`a_sign_out_…` / `a_removal_retires_its_endpoints_and_keeps_the_accounts_reception_key`
  — red with the catch-all restored) and the single-device arm
  (`a_single_device_sign_out_and_back_in_keeps_the_reception_key_and_its_escrow_wrap`).
  **Ruled 2026-09-19, built 2026-09-23 — the re-seal pass and the
  shredder's veto.** Clause
  (3)(g) is step 5 of `fauna_sync_engine::generation_reclaim::ensure_reclaimed`,
  between the removed-device step and clause (f): one tip resolution now
  feeds both it and the generations step, and it scans this replica's relay
  plane only while some generation is reclaimable. "A row of its own at the
  tip's item key" is `generation_reclaim::own_row_at`, the
  device-endpoints writer's own check lifted to serve both (homed in the
  ungated module, which the writer imports). Clause (3)(e)'s
  veto and its reach-first stand-in election run in the generations step,
  after the nest's `sealed_under` answer. A local-only covering row of the
  shredder's own counts: the ordered own publish cannot send the `Shredded`
  marker ahead of it. Tier-1 pins, in the module:
  `the_hand_over_never_retires_before_its_own_row_is_published`,
  `a_siblings_covering_row_never_licenses_the_hand_over_retire`,
  `a_stale_own_row_never_licenses_the_hand_over_retire`,
  `the_hand_over_keeps_a_row_it_cannot_open` (the last three added at the
  build's verify-back, each red-verified by its own mutation),
  `a_member_that_is_not_the_hander_writes_nothing`,
  `a_tombstone_is_handed_over_as_a_tombstone`,
  `the_veto_holds_the_shred_until_the_covering_row_is_walked`,
  `a_shredder_that_cannot_key_the_generation_is_not_vetoed`. Against the
  real nest, in `conformance_account_state_walk.rs`: the sign-out and
  removal arms above now assert the whole chain (C holds the key, B serves
  its own re-sealed row of it, A's generation-1 row is retired, generation 1
  is `Shredded`, its escrow wrap is swept), and
  `a_single_device_sign_in_hands_the_recovered_generation_over_then_shreds_it`
  runs the same chain after escrow recovery. The seedless single-device pin
  and the 12-seat steady-state sweep pass unedited, the sweep within noise
  of its pre-build wall clock. The ignored 64 × 200 sweep passes unedited on
  this build: a detached run on 2026-09-23 exited 0 in 36,731 s on a loaded
  box, and a re-run on the post-re-seal tree on 2026-10-01 exited 0 in
  15,941 s. Each tier-1 pin was
  red-verified by reverting its own arm.
  **Ruled + built 2026-09-19 — escrow recovery.** The *Escrow recovery* bullet's pass
  is `fauna_sync_engine::generation_escrow_recover::ensure_recovered`, the
  one production caller of `fauna.generation.escrow.get`: the pump runs it
  after the fleet walk and before the top-up pass on every seed-holding
  runtime (`FleetWriter::escrow_recovery`; `None`, and skipped, on the
  seedless host), with one answered-set per runtime worker. Shared-Rust, so
  all 7 apps heal with no per-app change. Its ack gate is
  `fauna_core::generation::escrow_acked_generations`, lifted out of the tip
  resolver so the two share one predicate. Proof, against the real nest:
  `conformance_account_state_walk.rs`'s
  `a_single_device_sign_in_recovers_its_generation_from_escrow_and_holds_the_reception_key`
  (A′ keys generation 1 from the holder's wrap, reads the reception key, and
  tops up a later seedless device; recovery deletes no wrap — only generation 1's
  later shred sweeps it — red with A′ built seedless); the bounds are the module's own unit pins
  (`an_acked_unkeyable_generation_is_recovered_and_asked_for_once`,
  `a_generation_without_a_trusted_receipt_is_never_asked_for`,
  `an_unreachable_holder_is_asked_again_next_pass`,
  `a_wrap_that_does_not_open_is_an_answer_and_is_not_asked_for_again`). Item (2)'s commitment check is pinned at the door (`fauna_mls` `an_escrow_wrap_substituted_under_the_binding_fails_the_commitment`) and through the pass (`a_substituted_wrap_under_the_generations_own_binding_keys_nothing`; the Key↔id gate: `a_squatted_mint_row_draws_no_escrow_request`), and the read half re-checks behind it: `generation_tip::generation_key_for` never serves a retained key that fails a live, id-bound mint's commitment, exactly as `key_for_tip` refuses it.
  The far half of the chain — A′ handing the departed writer's rows over,
  generation 1 then shredding and its wrap being swept — is clause (3)(g)'s,
  pinned by `a_single_device_sign_in_hands_the_recovered_generation_over_then_shreds_it`
  (the re-seal pass entry above).
  **Built 2026-10-01 — the re-presentation behind a recovery.** Item (3)'s last rule: `pump`
  (`fauna_account_plane::account_driver::pass`) runs the fleet plane's
  `reconcile` again when `ensure_recovered` answers `Recovered`, reported in
  its own slot (`PumpReport::fleet_rewalk`). Measured before it, on tui: a
  same-device sign-out and sign-in settled its prologue with the rotation
  ring empty, and the critical-alert sweep's custody feeder skipped as
  `no-held-rotation-keys` while the nest still named the minted DID. Web did
  not show it because its sign-out keeps the origin's replica. Proof, through
  the production pump over the real nest doors:
  `conformance_account_runtime.rs`'s
  `v17_a_sign_in_after_a_sign_out_reads_the_rotation_ring_once_its_prologue_settles`
  (red without the re-presentation: the ring reads empty at `settled`); the
  journey is `test_alert_sweep_directory_feeders_e2e.py`'s
  `test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles`,
  which signs out and back in on every app.
