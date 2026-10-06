# Region blocking — target state

Owns: region-blocking
Status: partially-specified — the direction is user-ratified 2026-07-29 and the invariants in § Invariants hold regardless of how the open questions resolve; **the region/authority plumbing is resolved as of the 2026-08-10 design pass** (§ The region/authority plumbing — registry, publication/signing, transparency log, fail posture: ratified-refutable at build time; Q1 determination = declared-never-detected and Q7 render scope = all rendered content are **user-ratified** the same day, in-session) — the plumbing serves BOTH planes (this doc + dynamic-features' region tier), designed once — the remaining specification gap is resolved by the build design pass that proposes the content plane's wire kinds and element IDs under the normal approval gate; build design passes own element IDs and the per-surface fold-in mechanics (the **content** plane is partly built — below; the plumbing's shared decode-and-verify half landed 2026-08-11 as `fauna_core::region_authority`, consumed first by the feature plane's region tier — § Implementation status today; the log-inclusion gap that build found is **resolved 2026-08-11**: § The transparency log now specifies checkable inclusion evidence — proof, head monotonicity, compiled-in anchor, witnessed checkpoints as the go-live bar — ratified-refutable, unbuilt). **The content plane's build design pass ran 2026-09-09 — § The content plane: the policy-document grammar and the render seam (both **user-ratified 2026-09-25**), the app-side fetch through the nest relay, the nest-as-publisher leg, region composition along the registry's parent chain, and the blocked render with its proposed element-ID package (rule A — user approval owed before any ID lands). Partly built — the shared-Rust half (C1), the relay (C2) and the nest-as-publisher leg (C4) are in; the app side (C3) and the element-ID package are not (§ Implementation status today).**
Authority: the region-blocking mechanism — the region/authority model, the transparent published blocking algorithm and its permanent public history, the app-side apply with the visible-block render invariant, and the nest-as-publisher scope. Does NOT own: scorer *placement* ([`../architecture/content-scoring.md`](../architecture/content-scoring.md) — its placement matrix carries the nest-as-publisher carve-out row this doc instantiates); the tier frame ([`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) — region blocking is a tier-2 instance; that doc's § Boundaries names it); the enforcement-model integration ([`moderation.md`](moderation.md) § Categories & enforcement item 3 is the scope pointer); the legal takedown ([`moderation.md`](moderation.md) § Legal takedown — a distinct, built mechanism); the render-verdict engine ([`family-client-enforcement.md`](family-client-enforcement.md) § Content policy). On conflict: placement → content-scoring.md; tiers → the frame; takedown / enforcement integration → moderation.md.

## The ratified direction (user-ratified 2026-07-29)

The project's ratified position:

> Only apps do blocking (not the nest except for web content), and each app runs a blocking algorithm according to the region it either is in or is registered in (details TBD). Each authority (in practice country) gets a region administered by them and can set the blocking algorithm for that region. The blocking algorithm is completely transparent and can be examined by experts. The history of published blocking algorithms stays public forever. Blocking is also transparent to the user, in that the act of blocking is shown clearly ("this image was blocked due to: <a reason given by the algorithm>"). One complication: the EU's new chat law might also require uploading the offending content to the authorities — details fuzzy there. […]

This implements the bylaws' guiding principle 3, "Authorities can block illegal content" (published at fauna.social/organization/bylaws). It was also described — clearly marked as planned — on the public `/safety` page; that page was retired in the 2026-08-04 site consolidation to the single-edition site (content archived, not live), so the bylaws page is the only public-site mention of this direction today.

As spec bullets:

1. **Blocking runs in the apps, on the device.** The nest does not block — with one exception: web content the nest itself publishes to the open internet, where the nest is the publisher and blocking applies where the publishing happens.
2. **Each app applies the blocking algorithm of its region** — the region it is in, or is registered in; determination ratified 2026-08-10: **declared, never detected** (§ The region/authority plumbing → Region determination).
3. **Each authority — in practice a country — administers its own region** and sets the blocking algorithm for that region.
4. **The algorithm is completely transparent**: published, examinable by experts, and the history of every published algorithm stays public forever.
5. **Blocking is just as transparent to the user**: a blocked item is clearly marked in the app with the reason the algorithm gave — never a silent disappearance.

## Why this shape — fit with the ratified architecture

- **It is a tier-2 instance of the moderation/ranking frame.** The frame's authority-tier table already anticipates it: tier 2 is "the deployment / **a legal authority**, compulsory, applied *to* them, **transparent**" ([`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) § The three model-authority tiers). The region algorithm is a compulsory transparent model — no new tier, no new trust category.
- **The app-side placement is forced by the architecture, not just chosen.** Content rests sealed on every nest and the nest never holds plaintext beyond the floor ([`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)), so the only position that can evaluate an algorithm over most content is the app, post-decrypt — the same reason every content scorer already runs client-side or at the perimeter ([`../architecture/content-scoring.md`](../architecture/content-scoring.md)). The one place a nest legitimately holds plaintext *and publishes it to an audience with no Fauna app in the loop* is its own public web serving — exactly the ratified exception, recorded as the nest-as-publisher row in content-scoring.md's placement matrix (that doc owns placement; this doc is the row's instance).
- **Distinct from the legal takedown, deliberately.** The takedown ([`moderation.md`](moderation.md) § Legal takedown) is per-item, ordered by an authority against a specific nest, and withholds the content from every viewer of that nest. Region blocking is algorithmic and region-scoped: content stays on the nest and stays visible outside the region; the block happens at render, in the region, visibly. The two are complementary compulsory surfaces and share the transparency-by-construction property (visible marker + reason; audited/public trail).
- **Distinct from user-side filtering.** Tier-1 filtering is the user's own voluntary preference; region blocking is compulsory. It composes *alongside* the user's filters and the guardian floor — it never replaces them, and it gives the nest admin no lever.
- **The likely render seam already exists.** [`family-client-enforcement.md`](family-client-enforcement.md) § Content policy owns the shared render-verdict engine (`fauna_core::obligation::render_verdict_composed` → `Show | Badge | Collapse | Block`, strictest-wins), which already composes the user's own thresholds with a compulsory floor (the guardian's). A region algorithm's verdict is naturally a third rule source into that same fold, with the blocked-notice render carrying the algorithm's reason. The build design pass decided it 2026-09-09 — § The content plane → *Where it composes* (user-ratified 2026-09-25; the fold itself is built — § Implementation status today).
- **The same region/authority model carries a second plane — feature gating.** [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md) (minted 2026-08-10) applies the same authorities-publish-transparent-policy model to controversial-class feature *availability* (quota-shaped gates on payments, zaps, bulk file sharing). Open questions 1–4 and 6 were shared surfaces, **resolved exactly once for both planes** by the 2026-08-10 design pass (§ The region/authority plumbing); that doc's region tier consumes the answers, never re-derives them.

## Invariants (hold regardless of how § Open questions resolve)

1. **Visible, reasoned blocking — never silent.** A blocked item is clearly marked where it would have rendered, with the reason the algorithm gave ("this image was blocked because: …"). A silent disappearance is a bug against this doc.
2. **The algorithm is public, and its history is public forever.** An authority decides what is blocked in its region; it cannot decide in secret, and it cannot quietly rewrite the past — the published history is append-only.
3. **Region-scoped, nothing destroyed.** A block in region A removes nothing from any nest and hides nothing outside region A. The nest's store is unaffected; blocking is a render/serving verdict, not a data operation.
4. **Placement: apps only, plus the single nest-as-publisher exception.** Apps apply their region's algorithm on the device. The nest applies it only to web content it itself publishes to the open internet (where no app exists to do it). No other nest-side blocking, ever.
5. **No admin lever, no config knob.** The region algorithm is set by the region's authority, not by the nest admin and not by an operator file — the nest-wide no-social-censorship invariant ([`moderation.md`](moderation.md) § Categories & enforcement item 1) is untouched, and the one-configuration-surface invariant gains no third tier: apps discover and apply the published algorithm; nobody hand-configures it per deployment.

## The region/authority plumbing (resolved 2026-08-10, refutable at build time — serves two planes)

Resolved by the 2026-08-10 design pass (open questions 1–4, 6, 7 below). One model, consumed by **both** planes — this doc's content blocking and the
dynamic-features region tier ([`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)
§ The region tier); neither plane re-derives any of it.

### The region registry (Q2)

A **Fauna-curated, signed, versioned catalog**: region code (ISO 3166 basis; supra-national
authorities such as the EU are representable as their own region rows) → authority name,
official domain, authority signing key(s), enrollment date. Curation — verifying through
official channels that a key genuinely belongs to a region's administering authority — is an
administrative act by the Fauna organization, done in the open: the registry is itself a
transparent artifact whose every revision lands in the public history (§ The transparency log),
so who was trusted, when, with which keys, stays permanently examinable. (The browser
root-program trust model; the in-repo precedent for a curated catalog compiled in as data is
the provider catalog.)

Apps and nests ship a compiled-in registry snapshot (works out of the box, offline-safe, no
config surface) and refresh it over the same channel as policies. Key rotation is a registry
revision; an artifact stays attributable to the key that was valid in its era.

### Publication and signing (Q3)

An authority publishes **one artifact envelope** — `{ region, authority key id, monotonic
sequence, issued-at, payload kind, payload, signature }`, dag-cbor, size-bounded. Two payload
kinds, one per plane:

- **Content policy** — the "blocking algorithm" of the ratified direction, concretely a
  **policy document: verdict rules over named factors, plus optionally bundled scorer
  artifacts producing factors the rules reference**. Any algorithm is fair game (user
  clarification, 2026-08-10): threshold rules over labels other scorers already produce
  (pre-run tags — NSFW etc.), a published id/hash list (the tier-3 `list` artifact kind, pure
  lookup), a bundled executable model (the tier-3 `wasm` kind — the fuel/memory-bounded
  deterministic sandbox), or any combination. Every rule entry carries the reason surfaced at
  render (invariant 1). The artifact kinds and sandbox are reused from the tier-3 labeler
  plane ([`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md));
  the composition seam is the shared render-verdict engine ([`family-client-enforcement.md`](family-client-enforcement.md)
  § Content policy) — the region source is one more strictest-wins rule source beside the
  user's own filters and the guardian floor, never a separate engine.
- **Feature policy** — the quota-grammar document; grammar owned by
  [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md) § The policy shape.

Update cadence is pull-based: nests (and the app-side content plane) refresh on a bounded
cadence sized as a tier-1-style constant; staleness past it surfaces as a warning (admin-side
for the nest, in-app for the content plane), never an outage (§ Fail posture below).

### The transparency log (Q4)

The permanent public history is a **Fauna-hosted, universally mirrorable append-only log** —
concretely a public git repository (hash-chained, diffable, examinable with ordinary tools by
exactly the experts invariant 2 invites). It holds the registry's full revision history and
every published artifact of every region. The repository uses git's **SHA-256 object
format**: inclusion proofs (below) walk the object hashes, so the object hash is
security-load-bearing — git's default is still SHA-1, whose collision resistance is not
acceptable for a chain that *is* the property (the cost is that mirrors are plain `git
clone`s anywhere rather than hosted on platforms without SHA-256 support, which is the
correct trade for a log whose mirrors exist to be verified, not browsed).

**Log inclusion is what makes a policy effective (the CT property — load-bearing), and it is
checked, never assumed** (strengthened 2026-08-11; this section originally derived the
property from fetch provenance alone — *"accept only as fetched from the log or a mirror of
it"* — which cannot deliver it: a signature binds an artifact to an authority and says
nothing about publication, so a hostile mirror, or anyone able to answer as one to a single
deployment, could serve a genuinely signed, never-logged artifact to exactly one victim). A
consumer accepts an artifact only with **inclusion evidence**, four pieces:

- **Inclusion proof.** Beside each artifact the log serves the **head commit** and the git
  object chain (commit → trees → blob) proving the artifact's exact bytes sit at their path
  in that head. Checking it is a bounded hash walk — no git dependency in the consumer.
- **Head monotonicity.** The consumer persists the last head it accepted; a new head must be
  a **descendant** of it (a bounded parent-chain walk over served commit objects — append-only
  is the property mirrors can actually be held to). A non-descendant head is refused and
  surfaced like staleness — a warning, never an unbinding of the last-known-good artifact
  (§ Fail posture).
- **Compiled-in anchor.** Builds ship the then-current log head beside the compiled-in
  registry snapshot, so a consumer's very first accept already descends from a known-honest
  head — no trust-on-first-fetch window for any build that postdates the log.
- **Witnessed checkpoints — the go-live bar.** The served head is a checkpoint cosigned by a
  small quorum of independent witnesses (the CT-gossip/sigsum precedent; the roster is an
  administrative enrollment act like the registry's own). What a witness signs is a
  consistency claim, not an observation — the contract is sigsum's own, and it is part of
  the property: **each witness persists the last checkpoint it cosigned, and cosigns a new
  checkpoint only if it descends from that one.** The memory is load-bearing: a stateless
  fetch-and-sign witness would cosign — with no compromise at all — a history rewritten
  after the compiled-in anchor, since such a fork still descends from that anchor and so
  passes every other check a fresh install can make; fresh installs are precisely the
  consumers head monotonicity does not yet protect. Enrolling the **first authority**
  is gated on the quorum existing: self-consistency alone cannot expose a fork a victim has
  been on since its anchor, so the witness bar arrives before the property is ever relied
  on, never as a retrofit.

What this delivers, stated honestly: to bind a consumer, an artifact must be committed to
the one witnessed public history — a secret or victim-targeted policy is no longer a covert
one-shot serve but a **fork of the public log maintained against the witness quorum
forever**, exposed by any cross-check. The residual that remains is compromise of the
witness quorum *together with* the victim's serving path — the same residual Certificate
Transparency itself carries, named here rather than waved off. That conjunction is the
consistency contract's doing: under stateless witnesses the "compromise" conjunct would
evaporate and the residual would be merely *an equivocating log or a compromised serving
path* — which is why the contract above is a build requirement, not an implementation
detail. **And the fork is permanent, by the linear layout below (settled 2026-09-11):** a log that could *merge* would let an operator holding that split quorum
heal a victim-targeted fork by merging it back into the public line — every consumer, the
victim included, would then descend cleanly onto the main line, the equivocation surviving
only as a merge parent for an expert to find, and "descends from" would be satisfiable by
naming the victim's head as *any* parent, which the operator can always arrange. Under a
linear log none of that is available: the victim's consumer refuses every later public head
as not descending from the fork it accepted, loudly and for as long as the deployment lives,
with no operator surface to quiet it (§ The region registry: no config surface). That
wedge is accepted with eyes open — **it is the exposure**, not a failure of it: a consumer
that has been served a fork is one the quorum has been split against, and the loud, lasting
refusal is what makes the split visible rather than healable.

Three corollaries. Authorities never learn who consumes their policies (consumers touch the
log and its mirrors, not government servers). Apps normally fetch through their own nest
(one log fetch per nest; zero per-app fingerprint surface) — but the nest is a **relay, not
a trust point**: the inclusion checker is shared Rust (`fauna_core::region_authority` is
network-free and WASM-usable on purpose), and every consumer — the nest for the feature
plane, the app for the content plane — verifies proof and descendance itself. And the
artifact+proof bundle the log serves is **identical for every consumer** — no per-consumer
query on the ordinary path, so the routine fetch carries nothing to track. One qualified
edge: a consumer offline longer than the bundle's history window walks deeper via ranged,
content-addressed object fetches, and the range reveals its last-seen head (roughly, how
long it was offline). The per-nest relay contains this in practice — the log sees one
aggregate fetch per nest, never a per-app pattern — but it is a narrowing of the first
corollary, not an absolute.

One structural consequence, binding on the build pass: inclusion evidence is a **fetch-layer
companion beside the envelope, never a field inside it** — a logged blob cannot contain the
hash of the commit that contains it, and an authority signs before publication. The envelope
(`PolicyArtifact`) is final as built; the evidence rides beside it on the fetch path.

#### Repository layout and mirrors (2026-09-09 — ratified-refutable at the inclusion checker's build)

The log is **one repository, one branch, linear history**: every commit has exactly one
parent, so "descends from" is a parent-chain walk with no merge to reason about — and the
checker holds the log to it (settled 2026-09-11): **a commit naming a second
parent is refused outright**, wherever a commit is read — the proof walk over the head, and
the descendance walk on the consumer side and the witness side alike — so a merge is never
a descent, whichever parent it names, and a fork can never be reconciled back into the line
(the consequence is stated beside the split-quorum residual above). Its paths are the
consumer's addresses, and they are the same paths in the repository and on the served
surface:

- `registry/<version>.cbor` — the curated catalog (§ The region registry) at that revision,
  dag-cbor, one file per revision, never rewritten; `registry/<version>.md` beside it is the
  **curation record** — what was verified through which official channel, when — so the
  enrollment itself is examinable, not only its result. `registry/current.cbor` is a
  byte-identical copy of the newest revision. The compiled-in snapshot is a transcription
  of one revision, and the consumer's source pins which.
- `regions/<region>/<kind>/<sequence>.cbor` — every artifact ever published, immutable, at
  its envelope's sequence number. `<region>` is the code exactly as `RegionCode` spells it
  (uppercase letters and digits, no separator); `<kind>` is the payload kind's stable string.
- `regions/<region>/<kind>.cbor` — the **current** artifact of that kind: a byte-identical
  copy of the highest-sequence file beside it. This is the one path a consumer fetches on
  the ordinary path (the nest's refresh already reads exactly it).

The served surface is a **static export of the head commit** — the base URL plus the
repository path, nothing computed per request — which is why a mirror is a plain clone plus
the same static serving. Inclusion evidence rides beside the artifact at a sibling path the
checker defines, never inside it; and because a cosignature over a head cannot live in that
head, checkpoints and cosignatures are **served, not committed** — a mirror carries the
newest it has fetched. A mirror can therefore live only where the SHA-256 object format is
supported: re-hashing the history into SHA-1 is not a mirror, because the object hashes are
the property. The compiled-in anchor is the head at build time, and each release advances it
as an ordinary reviewed commit beside the registry snapshot. A fork of Fauna that wants its
own log changes the base-URL constant, the anchor and the registry snapshot **together, in
source** — one build, never a setting (§ The region registry: no config surface).

Hosting the log, enrolling an authority and seating the witness quorum are administrative
acts of the association, each written as a runbook the user executes; the hosting choice and the witness roster are the
user's picks, unmade.

### Region determination (Q1 — user-ratified 2026-08-10)

**Declared, never detected.** Store-distributed builds: the storefront region (stores already
partition by storefront — shading into the dynamic-features tier 0). Sideloaded and self-built
apps: the OS-declared region setting (visible, user-set, no network consulted). Deployments:
the admin declares the nest's region — its legal situs — in the admin UI, persisted in nest
state (bucket 2). IP geolocation and network fingerprinting are never consulted — not for
travel, not for VPN, not for anything (the honest trust model: a false declaration is the
non-conforming case, accepted and documented; lawful operation stays the effortless default).
This one determination rule serves both planes and both subject kinds: the app for on-device
render, the deployment for nest-side feature enforcement over the accounts it hosts.

### Fail posture (Q6)

**Last-known-good** — the family-safety unfetched-policy rule applied one level up
([`family-client-enforcement.md`](family-client-enforcement.md) § Content policy, the 2026-08-02 three-clause ruling):
a failed fetch yields no information, and enforcement state never moves on no information; the
last-known artifact persists (nest state on the nest; an app-side snapshot for the render
plane) and stays in force until replaced; the declared residual is the never-yet-fetched fresh
subject — on the feature plane still structurally bounded by tier-1 constants, on the content
plane rendering unblocked until the first successful fetch. Staleness warns; it never relaxes
silently, and it is never an outage.

**The anti-replay floor survives a change of situs, and it is keyed on the authority (ratified
2026-08-20).** *"Never relaxes silently"* has to hold across administrative acts
too, so the highest sequence ever accepted for a (region, payload kind) is stored **separately from
the artifact it came from** — clearing a *binding* never clears the *replay defence*. The three
paths that retire a document (a withdrawal, a change of situs, a de-listed authority's re-fold) all
leave the floor standing: an artifact this nest once accepted stays validly signed forever, so
refusing it a second time is the only thing that can stop a rollback. Before this was ratified the
floor was read off the stored artifact row, which those same paths delete — a withdraw-then-
re-declare round trip on one region was enough to re-open the replay window on it.

The **consequence, stated rather than left as a side effect:** a floor that outlives every artifact
would also refuse a legitimately *lower* sequence, so the floor belongs to the **authority** whose
counter it tracks, not to the region code. Key rotation does not reset it — the registry models
rotation as another key under the same authority entry, which is the behaviour wanted, since the
counter continues. A curated registry revision handing a region to a **different** authority does
start a fresh sequence space, because that authority's counter is unrelated and its first document
is legitimately at a low sequence. That is what keeps a surviving floor from wedging the plane
permanently shut: there is no operator surface to clear a stuck floor, and by the
one-configuration-surface invariant there must not be one. Keying on the authority is safe because
the name comes from the curated registry, never from the artifact an attacker delivers.

### Render scope (Q7 — user-ratified 2026-08-10)

The region rule source composes into **all rendered content** — feed and media (bridged
content included), profiles, nest-published web (the nest-as-publisher exception), and
private-correspondence renders (conversations/DMs, mail, folders) — on-device, post-decrypt,
in the same shared engine as every other rule source. Three boundaries hold unchanged:
**blocking never implies reporting** (this scope composes a render filter and nothing
else); **no rule source obtains a
decrypt path** (composition happens where the user's client already holds plaintext); and the
**declared conforming-client bound** applies (a third-party IMAP/CalDAV client renders outside
any Fauna engine — the same honesty bound the guardian content floor declares).

## The content plane (build design pass 2026-09-09 — grammar and render seam user-ratified 2026-09-25; partly built)

Resolved by the 2026-09-09 build design pass. Everything here is
buildable on the plumbing above exactly as it stands, and **nothing here varies with what any
authority chooses to publish** — the mechanism is the same under every policy a region could
ever adopt; only three things ever differ between deployments and eras: which authorities are
enrolled, what payloads they publish, and whether some *separate* mechanism is ever ratified
beside this one (§ Render scope's three boundaries stand unchanged, and this section adds no
path that could relax any of them). Advisory in content, default-act in force: a build slice
that finds a claim below wrong refutes it in its own commit and says so here.

### The policy document (the content-policy payload)

The `content-policy` payload kind of the envelope (§ Publication and signing) is one dag-cbor
document, decoded only through `VerifiedArtifact` like the feature plane's, float-free, with an
`extra` catch-all at every level (transport.md's additive discipline):

- **`version`** — the grammar version. A consumer meeting a version it does not implement
  treats the document as **inert and says so** on the transparency surface (the `text-model`
  labeler's rule) — never silently mis-applies a document it half-understands. Inert is the
  fail direction because a region rule can only ever *restrict*, and this plane's fail posture
  is last-known-good over an authored document, not fail-closed over an unreadable one: the
  content plane's subject is the user's own render, where a blanket block on an undecodable
  document would blank every item on the device.
- **`rules`** — the verdict rules, each `{ factor, min_permille, verdict, reason_code, reason }`:
  `factor` names a label category exactly as the shared engine already keys them (the
  canonical set — `nsfw`, `spam`, `phishing`, `commercial` — or a factor a bundled scorer
  below produces, namespaced `region:<region>/<name>`); `min_permille` is the same per-mille
  trigger the engine's rules carry; `verdict` is `block` or `collapse` — the two render verbs
  the engine has, so a region rule is one more `ObligationRule` and never a new verb;
  `reason_code` is a stable authority-assigned token; `reason` is the text invariant 1 shows
  the user, a `{ lang → text }` map with a required default entry, rendered **verbatim**
  under the app's own frame (the frame is the app's i18n; the reason is the authority's, and
  an app never paraphrases an authority). A rule missing its reason is a malformed document.
- **`scorers`** — optional bundled scorer artifacts, `{ name, kind, bytes }`, reusing the
  tier-3 artifact kinds exactly as built: `list` (a content-id → per-mille map, pure lookup,
  no execution — `fauna_core::scoring::validate_list_artifact` is the validator) and `wasm`
  (the `fauna-labeler` fuel- and memory-bounded deterministic sandbox, no host I/O). A scorer
  produces the factor `region:<region>/<name>` over the item the app is about to render,
  on-device, post-decrypt, and that factor joins the item's label list before the fold. Bounds:
  the envelope's 4 MiB total; per-scorer limits are the labeler crate's own constants.
  `text-model` is not admitted at v1 — it is a *training* artifact, and an authority's
  transparency obligation is better met by a list or a module whose behaviour experts can
  read in full. **Validation and execution are separate layers, and the split is forced**
  (C1's build, 2026-09-09): `fauna_core::region_policy` validates a scorer *structurally* —
  the kind is one of the two, `text-model` is not a decodable kind at all, `list` bytes go
  through the built `validate_list_artifact`, both kinds are size-bounded, and a rule naming a
  `region:<region>/<name>` factor no bundled scorer produces makes the document malformed —
  but it never *runs* one, because `fauna-labeler` depends on `fauna-core`, so calling the
  sandbox from there is a dependency cycle. Scorer execution belongs to the consumers that
  already link the labeler: the app (§ How an app obtains its region's policy) and the nest
  (§ The nest-as-publisher leg).

### Where it composes — the render seam (user-ratified 2026-09-25)

The likely seam § Why this shape named is the seam: the region content policy is the **third
strictest-wins source** of the shared render-verdict engine
([`family-client-enforcement.md`](family-client-enforcement.md) § Content policy —
`fauna_core::obligation::render_verdict_composed`), beside the viewer's own thresholds and the
guardian floor. Concretely, a shared `rules_from_region_policy(document, region, authority) →
RegionRuleSet` assembler and one more argument on the composed call; the verdict's precedence
(`Block > Collapse > Badge > Show`) is untouched. Which source drove the verdict is returned
beside it, so the placeholder can name the region and its authority rather than a generic
"policy": the family plane's placeholder names *its* policy for the same reason.

**One claim above was refuted at C1's build (2026-09-09)**: this section specified the assembler as
returning a bare `Vec<ObligationRule>`, which cannot carry the authority's `reason` text — and
invariant 1 requires that text be shown to the user **verbatim** when the rule fires. So the
assembler returns a `RegionRuleSet` (the region, the authority name, the document's applicability
status, and the rules each carrying its own `reason_code` + `{ lang → text }`), and the attribution
rides *with* each rule rather than in a parallel vector, which would be one refactor away from
naming a different rule's reason. Two smaller shapes settled the same way: on a **tie** at the
winning strictness the attribution goes to the region (an authority's reason is one the user is
entitled to read; the family placeholder carries no such external obligation), and among regions
to the most specific on the chain; and the composed argument is a plain slice rather than an
`Option`, an empty chain being the same thing as no chain and one fewer way to spell it. There is no
separate engine, no second fold, and no render surface that reaches the composed call for one
source while bypassing it for another — the surfaces already routed through it (feed cards,
conversation bubbles, post detail) gain the region source for free, and every surface § Render
scope lists that is *not* yet routed through it is enrolled by the build slices, never
special-cased.

### How an app obtains its region's policy

- **The app's region is declared, never detected** (§ Region determination): a
  store-distributed build reads its storefront; a sideloaded or self-built one reads the OS's
  user-set region setting (web, which can read no OS region setting, reads the region subtag of the browser's user-set language, `navigator.language`, source "browser language" — the one user-set, visible, network-free declaration a page can read; a tag without a region subtag declares nothing, never a region inferred from the language). Each platform shell hands shared Rust one `RegionCode` and the
  *source* it came from — a genuinely platform-divergent leaf, kept to one function per shell;
  everything downstream is shared. The app shows its region and its source read-only on the
  transparency surface below, with the change path named ("set in your system settings"); there
  is no in-app override, because an override is a knob that makes the declaration a choice
  rather than a fact.
- **Fetch rides the nest as a relay.** An app asks its own nest for the bundle — one kind,
  `fauna.region.artifact.get { region, payload_kind }`, answered from the nest's per-(region,
  kind) cache and refilled from the log on the same cadence and with the same staleness clock
  the feature plane already runs. The nest is a **relay, not a trust point**: it returns the
  envelope and, once the log serves them, the inclusion-evidence companions beside it, and the
  app verifies signature and — when built — inclusion itself in shared Rust. The one thing the
  relay learns is which regions its own apps declare, which it already knows more precisely
  from the connection; stated rather than hidden. A nest that enrols nobody for the asked region
  answers "no document", which is the fresh-subject arm of § Fail posture, not an error.
- **Last-known-good, persisted on the device.** The verified document is stored per device,
  loaded at launch ahead of the first fetch (and re-verified against the current registry
  there, so a de-listed authority's document stops binding), and replaced only by a newer
  verified document — a failed fetch writes nothing (§ Fail posture); the device keeps its own
  replay floor, keyed on the authority exactly as the nest's is. Staleness past the cadence
  (the shared `fauna_core::region_authority::STALE_AFTER_SECS`) warns on the transparency
  surface, never blocks and never relaxes. **The declaration itself is last-known-good too (2026-09-27):**
  a leaf that answers asynchronously (a store build's storefront) leaves the plane standing on the
  declaration the device record carries until it answers, so a held document binds from the first
  paint rather than lapsing while the store is asked — a device with no record declares nothing
  until then (the fresh-subject residual). A leaf that answers at once is never overridden by the
  recorded declaration. **Refuted at C3's build (2026-09-26):** this bullet
  first put the document in the family plane's persistence seam — `fauna-client-accounts`'
  `SecretStore`, a platform credential store — which cannot hold it: an envelope is bounded at
  4 MiB (it may bundle a scorer) and a platform keyring entry is not (Windows Credential
  Manager caps a blob at 2.5 KiB), and a public signed per-device document is not an account
  secret. So the at-rest *format* is shared Rust's (`fauna_client_region::RegionPlane`'s one
  byte record), and each shell keeps those bytes in its own install-scoped state.

### Regions compose along the registry's parent chain

A device is in several regions at once — a subdivision, its country, and any supra-national
authority the country belongs to — and each may administer its own policy. So the registry row
carries an optional **`parent`**, the app's declared region resolves to its ancestor chain, and
the app applies **every** policy on the chain, strictest-wins in the same fold (each region's
rules are one more rule set; a block in any binds). This is the plumbing made regime-generic
rather than a feature: the plane cannot know which level of government legislates where, so it
must be able to carry all of them at once. Two consequences for the built plumbing, both
additive: `RegionCode` admits the ISO 3166-2 subdivision form (`CC-SSS`, one hyphen) beside the
alpha-2 and supra-national rows it accepts today — an older consumer refuses the new spelling as
malformed and applies nothing for that row, which is exactly what it did before; and the
compiled-in registry snapshot (still empty) gains the field.

### The nest-as-publisher leg

The nest applies the content policy of its **admin-declared situs** (the same declaration
the feature plane reads — nothing new for the admin to set) to the one surface where it is the
publisher: the public web pages it renders to the open internet from public posts
(`web_content`'s render path). Evaluation is over the labels the nest already holds for public
posts plus the document's bundled scorers run over that public plaintext — the sanctioned
nest-position content evaluation, scoped exactly as [`../architecture/content-scoring.md`](../architecture/content-scoring.md)'s
placement row scopes it; no other nest surface evaluates anything. **Which** of the labels the
nest holds this fold may read is the label plane's own rule, not this leg's: only a row that
names its writer, written by the post's author or by a holder of that author's grant — a
member's label on someone else's post reaches this fold never, so nobody but the authority
decides what its rules fire on (invariant 5). Owner:
[`moderation.md`](moderation.md) § Per-row badge data path. The public page renders the
same visible, reasoned placeholder as the apps (invariant 1 applies to a web visitor as much as
to a user), with the authority's reason verbatim. In-app surfaces of the same posts are
untouched by this leg: the viewer's own app applies the viewer's own region.

**What the build settled (C4, 2026-09-10 — refutable like the rest of this section):**

- **The whole chain, as an app applies it.** The nest folds every policy on its situs's
  registry chain (§ Regions compose along the registry's parent chain), most specific first,
  through the same composed engine the apps call — with the region source only, since a public
  page has no viewer thresholds and no guardian floor. Each region's document is read from the
  relay's `(region, content-policy)` cell and re-verified against the registry at render; the
  render never fetches. Declaring a situs records that demand itself, because the relay fetches
  only what has been asked for and no app may ever ask for the nest's own situs.
- **What each verb does to a web page.** `block` puts the placeholder — the app's frame naming
  the region and its authority, then the authority's reason verbatim — in place of the post
  everywhere the site carries it: its own page, the index, the feed and any template listing
  it. The title and tags go with the body, because both are read off it, and a gated post's
  paywall box goes because there is nothing on the page to subscribe to. `collapse` keeps the
  post one reveal away: the frame is the reveal's label, the reason sits under it, the body
  behind it; the title and tags are replaced as under a block.
- **The sealed full page follows its post.** A gated post's full page, sealed to its
  subscribers, is the same post published by the same nest, so the verdict governs it too:
  withheld outright under `block` (the nest never opens the full body at all), collapsed the
  same way under `collapse`. The verdict itself is computed over public inputs only — the
  labels the nest holds and the scorers' output over the public plaintext, which for a gated
  post is its preview; the sealed body is never a scorer's input.
- **The pages are static, so every change of what is in force re-renders.** A declaration, a
  re-declaration, a withdrawal, a newer document accepted for a region on the chain and a
  de-listed one retired each re-render every publishing site, in the fail-closed shape the
  legal takedown uses: a site whose render fails is cleared rather than left serving pages
  rendered under the policy that no longer holds.
- **Scorers at render.** A `list` is looked up by the post's id; a `wasm` module runs in the
  labeler sandbox at the host ceilings over the post's public plaintext (the shared
  `LabelerPostInput::from_post` mapping). A scorer that does not name the post, emits no label
  or fails says nothing, and no factor joins. A module is deterministic, so an answer is cached
  per (post, digest of the scorer's bytes) and a render scores only what it has not seen.
- **No element ids.** The placeholder's markup carries neutral classes and data attributes; the
  page's ids arrive with the element-ID package below, under its approval.

### The blocked render and the transparency surface (element IDs user-approved 2026-09-25 under rule A)

A region verdict renders **in place of** the withheld content — never a blank, never a
page-level error — with three visible parts: that it was blocked (or collapsed, with the reveal
affordance the engine's `collapse` already has), by which region's authority, and the
authority's reason. The tombstone-style placeholder is the legal takedown's and the family
floor's shape, one concept in a third instance. A dedicated transparency surface shows the
app's declared region and source, each policy in force on the chain (authority name from the
registry, sequence, issued-at), when it was last checked, and the staleness warning.

The IDs, mirroring `content-policy-blocked-notice` and the `admin-nest-region-*` set (the
package the user approved 2026-09-25 under rule A; in ui.yaml since C3's build):
`region-blocked-notice` (indexed; the in-place placeholder), `region-blocked-reason`,
`region-blocked-authority`, `region-collapsed-reveal-button`; and on the settings page
`settings-region-section`, `settings-region-declared`, `settings-region-source`,
`settings-region-policy-item` (indexed, one per policy on the chain),
`settings-region-policy-authority`, `settings-region-policy-version`,
`settings-region-last-checked`, `settings-region-stale-warning`, `settings-region-inert-notice`
(the unimplemented-grammar-version case, so "inert and says so" has a place to say it).
Strings are the app's frame only (`region.blocked_notice`, `region.collapsed_notice` — minted with the nest-as-publisher leg, which shows the same frame — and the
labels above); the reason text is never an i18n string.

### What the build owes in tests

tier_1: the document decodes and validates (a rule without a reason is malformed; an unknown
grammar version is inert-and-flagged, never applied); `rules_from_region_policy` composes into
the existing fold with strictest-wins across all three sources; the chain resolution and the
subdivision spelling; the inclusion checker red-verified against a forged non-descendant head
(§ The transparency log). tier_3, driven through the UI on tui first: a synthetic region
artifact from a test-only registry enrolling a synthetic authority (the
`conformance_region_tier.rs` fixture pattern, never a real region) reaches a visible, reasoned
placeholder on the exact card, and the settings surface names the authority and version; the
same for the nest's public page. Convention-17 invariant, riding every existing e2e walk: **a
region `Block` verdict never renders silent** — wherever the engine says `Block` for the region
source, the placeholder is present on that surface.

## Open questions — remaining

| # | Question | Resolver / resolution |
|---|---|---|
| 1 | **Region determination** — "the region the app is in, or is registered in": which, when evaluated, how established, travel/VPN behavior. | **RATIFIED 2026-08-10 — declared, never detected** (§ The region/authority plumbing → Region determination). |
| 2 | **What a region registry technically is** — enumeration, authentication, discovery. | **Resolved 2026-08-10 (refutable at build time)** — Fauna-curated signed catalog, compiled-in snapshot + log-refreshed (§ … → The region registry). |
| 3 | **How an authority publishes and signs an algorithm** — artifact format, key distribution, update cadence. | **Resolved 2026-08-10 (refutable at build time)** — one signed envelope, two payload kinds; content policy = rules over factors + optional bundled scorers (§ … → Publication and signing). |
| 4 | **Where the permanent public history lives.** | **Resolved 2026-08-10 (refutable at build time)** — mirrorable append-only log with the log-inclusion-validity (CT) property; **inclusion made checkable 2026-08-11** (proof + head monotonicity + compiled-in anchor + witnessed checkpoints as the go-live bar — § … → The transparency log). |
| 6 | *(surfaced 2026-07-30)* **Failure mode when the region algorithm is unfetchable.** | **Resolved 2026-08-10** — last-known-good, the family-safety three-clause shape (§ … → Fail posture). |
| 7 | *(surfaced 2026-07-30)* **Content-kind scope.** | **RATIFIED 2026-08-10 — all rendered content**, with the blocking-≠-reporting, no-decrypt-path, and conforming-client boundaries stated (§ … → Render scope). |

## Implementation status today

**The *content* plane is partly built (its three built halves are the paragraphs below); the shared plumbing's decode-and-verify half is built (2026-08-11).** `fauna_core::region_authority` carries the pieces § The region/authority plumbing specifies for **both** planes — `RegionCode` (validated on the wire, never case-folded), the curated catalog types (`RegionRegistry` / `RegionEntry` / `AuthorityKey`, with `retired_at` making rotation a revision rather than a deletion), the signed envelope `PolicyArtifact` with its **stable-string** payload kind, and `verify_artifact` (envelope size bound, registry lookup, **key-era** check, clock-skew bound, monotonic-sequence replay refusal, `verify_strict` over the canonical bytes with `sig` zeroed). Its payload is reachable only through the `VerifiedArtifact` that verification returns, so decoding an unverified artifact is unrepresentable rather than merely discouraged. Built network-free on purpose — fetching is the consumer's, which is what keeps the types usable from the web SPA's WASM build.

**The compiled-in registry is EMPTY at version 0, and that is its correct content, not a placeholder.** Enrolling an authority is the administrative act § The region registry describes and it has not happened; no jurisdiction has published a Fauna artifact. Every artifact therefore fails `UnknownRegion`, nothing is ever ingested, and a deployment runs unrestricted — which is exactly the ratified fresh-subject arm of § Fail posture. Enrolling the first authority is an ordinary revision of that file, its diff being the transparent artifact invariant 2 asks for.

🪦 **The transparency-log gap is RESOLVED — the strengthening ratified 2026-08-11 (refutable at build time); the checker is BUILT (2026-09-09); the log is unpublished and enrollment is still gated on the witness quorum.** The 2026-08-11 build pass found § The transparency log claiming a *structural* result (*"a secret or victim-targeted policy is thereby structurally impossible"*) that its stated mechanism — fetch provenance alone — cannot deliver: a signature says nothing about publication, so a hostile mirror could serve a genuinely signed, never-logged artifact to one victim. The section now specifies the mechanism that does deliver it: the inclusion proof, head monotonicity, the compiled-in anchor, and witnessed checkpoints as the go-live bar for the first authority enrollment — with one correction to the note that first recorded the gap: the inclusion evidence **cannot** be "an additive field" on the envelope (a logged blob cannot contain the hash of the commit that contains it, and an authority signs before publication) — it is a **fetch-layer companion** beside the envelope, verified in shared Rust by every consumer (the nest relays to its apps; it is not a trust point). **What the 2026-09-09 build landed, all in `fauna_core::region_authority::inclusion` (network-free, WASM-usable):** `InclusionEvidence { head, chain, trees, checkpoint, cosignatures }` — commit objects head-first, the trees root-down, both carried without git's object header so the checker hashes each as the kind the step expects — over git's **SHA-256** object format, pinned against git's own empty-object ids; `check_inclusion` walks commit → trees → the canonical-dag-cbor blob at `regions/<region>/<kind>.cbor` (one `log_path` home the fetch URL and the proof share), requires the served head to descend from the consumer's persisted head (else the compiled-in anchor), and counts a checkpoint's cosignatures only from *current* roster members over a domain-separated canonical checkpoint naming this log; `admit_artifact` states the pre-log rule in code — an artifact with **no** evidence is admitted only while the anchor is the compiled-in empty one, and past it is refused; and the **witness half of the consistency contract** is built beside the consumer half, `cosign_checkpoint` over a persisted `WitnessState` refusing any checkpoint that does not descend from the last one cosigned, so the future witness binary is a shell over one reading of the contract. **Three facts about today, all deliberate:** the compiled-in anchor is `None` and the witness roster is empty at version 0 — the true state, not placeholders — and **evidence served under the empty roster is refused rather than waved through**, because the quorum is the go-live bar and arrives before the property is relied on. The bundle shape: the log serves the artifact at `regions/<region>/<kind>.cbor` (a tracked blob) and its evidence beside it at `regions/<region>/<kind>.evidence.cbor` (regenerated per head, never a tracked blob, since it names the head containing the artifact); a 404 on the evidence path is "no evidence" (admitted only pre-log), any other failure is a failed fetch (§ Fail posture's staleness), so a mirror cannot downgrade a consumer to evidence-less acceptance by failing a request. **Consumers wired:** the nest's feature-plane fetch (`region_tier.rs` → `fetch_bundle` + `ingest_feature_policy`'s `admit_artifact` step, the anchor persisted as `region_log_anchor` beside the replay floor, and created only by an acceptance so its absence *is* the pre-log era). The content plane's relay (C2) and the app's fetch (C3) call the same checker when they are built — owed by those slices, not this one. Today's ingress restriction (`REGION_LOG_BASE_URL` compiled in, no submit kind) remains the first line, unchanged. Still administrative, not code: publishing the log with its then-current head landing in `COMPILED_IN_LOG_ANCHOR` beside the registry snapshot, enrolling the witness roster (its shortlist and the enrollment runbook are C7's), and only then the first authority. **One shape in the built checker is contingent on an unmade user pick:** the checkpoint witnesses cosign is a raw git head today (`Checkpoint { log_id, head }`); if the user chooses to reuse the existing public witness ecosystem — which cosigns Merkle checkpoints with consistency proofs, not git parent chains (the C7 finding above) — the checkpoint becomes a signed note over a Merkle log of heads and the checker gains a tlog inclusion step for the accepted head, an additive reshape of the witness step alone; the proof walk, the descendance walk, the anchor and the pre-log rule are unaffected either way. **The checker enforces the linear layout (2026-09-11):** as built on 2026-09-09 it accepted descent through *either* parent of a merge, against the layout sentence ratified the same day; `parse_commit` now refuses a commit with a second `parent` line (`InclusionRejection::MergeCommit`), which binds the proof walk and both sides of the descendance walk through the one helper, and the pin inverted — a merge head whose second parent is the trusted head is refused on the consumer side and by a remembering witness alike.

**The log's repository layout is RATIFIED-REFUTABLE (2026-09-09, § The transparency log → *Repository layout and mirrors*) and the log is UNPUBLISHED.** The paths are fixed so the checker's evidence has an address and the nest's existing refresh path is already the current-artifact path; the served surface is a static export of the head. The enrollment runbook and the witness-roster shortlist exist as association documents; hosting and the witness roster are unmade user decisions, and nothing is enrolled. One finding binds the checker's build: the existing public witness ecosystem cosigns Merkle-tree checkpoints with consistency proofs, not git parent chains, so reusing it means fronting the git history with an append-only Merkle log of heads — the git repository stays the mirrorable artifact store, and the checkpoint witnesses sign is over that log.

**The anti-replay floor is built as § Fail posture ratifies it** (2026-08-20): `region_sequence_floor` is its own table keyed (region, payload kind, authority), raised inside the same **transaction** that records an acceptance (2026-08-21 — the writer used two independent autocommit statements until then, see below), and swept by none of the three retirement paths. Pinned by `conformance_region_tier.rs`'s `a_situs_round_trip_does_not_reset_the_replay_floor` (the finding, red-verified against the previous code) and `::a_newly_enrolled_authority_starts_its_own_sequence_space` (the authority keying, likewise red-verified), guarded by `::a_situs_round_trip_still_accepts_a_newer_artifact` against over-correcting into a wedge, and covered at the storage seam by `db::region_tier`'s `retiring_the_artifact_does_not_lower_the_floor` over both deleters. A nest upgrading into the table seeds its floor from the artifact already in force, so the defence is never disarmed by the upgrade itself — and that same boot seed now **raises** a lagging floor (`ON CONFLICT ... DO UPDATE SET sequence = MAX(...)`, matching the writer's own upsert) rather than an earlier `INSERT OR IGNORE` that left one in place: the writer's two autocommit calls could leave the artifact row ahead of the floor across a crash/OOM-kill/`SQLITE_FULL` between them, and `OR IGNORE` was declining to repair the one case the seed exists for. Pinned by `conformance_region_tier.rs`'s `a_boot_replaying_migrations_raises_a_lagging_replay_floor` (red-verified against the `OR IGNORE` seed).

**The plumbing has its first consumer, and it is the sibling plane** (2026-08-11): the nest ingests a **feature-policy** artifact end to end — `fauna.admin.region.{get,set}` for the declared situs (§ Region determination), verify → last-known-good store → fold into the feature tier, a pull-based refresh worker on a compiled-in cadence, and the admin-side staleness warning § Fail posture asks for. Three of its rulings bind this doc's own plane when it is built, so they are recorded here rather than only next door: **there is no kind that submits an artifact** (the ingress is a compiled-in log URL — the provenance line § The transparency log's 2026-08-11 strengthening builds its checkable inclusion evidence on top of — and a submit surface would hand that ingress to the admin — invariant 5 inverted); **a region with no enrolled authority is never fetched** (dormant by construction today, and no false staleness alarm about a channel that does not exist); and **de-listing an authority retires its document**, via an at-boot re-fold that re-verifies the stored envelope against the current registry. Owner of the feature-plane specifics: [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md) § Implementation status today item (c).

**The content plane's SHARED-RUST HALF IS BUILT (2026-09-09); the relay (C2) and the nest-as-publisher leg (C4) followed 2026-09-10; the app side (C3) and the element-ID package followed on tui, the lead app, 2026-09-26 (below).** `fauna_core::region_policy` carries § The policy document as code: `ContentPolicyDocument { version, rules, scorers, extra }` with a float-free `#[serde(flatten)] extra` at every level, and one `status(&region)` answering whether it applies — **version first, structure second**, so a grammar version this build does not implement is `InertUnimplementedVersion` (inert, and the surface says so) and is never reported as "malformed", which is exactly what an older reader would otherwise call a newer grammar's perfectly good shapes. Structural refusals: a rule with no `reason` for the required `default` key, no `reason_code`, a blank factor, a trigger outside `0..=1000`, a `region:<region>/<name>` factor no bundled scorer produces, and a scorer that is nameless, duplicate-named, empty, oversized or a malformed `list` artifact. `text-model` is not a decodable `ScorerKind` and neither is a third render verb a `ContentVerdict`, so both are refused by the type rather than by a check each consumer must remember. The document is reachable **only** through `VerifiedArtifact::content_policy()`, the twin of `feature_policies()` — there is no public `decode`. The engine takes the third source: `render_verdict_composed` gained a region argument and now returns a `ComposedVerdict { verdict, source }`, folding each source separately (a flattened rule vector has forgotten *whose* rule fired by the time it answers) with the precedence untouched — pinned by the pre-existing hand-assembly test, which still agrees exactly. `ContentPolicyState` holds the chain's rule sets and threads them through `verdict_for`, so tui's and linux's actual render paths compose them, and an identity change drops them with everything else actor-scoped. `RegionCode` admits the ISO 3166-2 `CC-SSS` spelling (one hyphen, alpha-2 country part, 1–3 character subdivision part; case still never folded), `RegionEntry` gained `parent` (`#[serde(default)]`), and `RegionRegistry::chain` walks the ancestor chain most-specific-first, refusing a cycle or a depth over 8 rather than silently applying whichever prefix it reached first. The FFI and wasm shells gained a **region-aware face beside** their existing verdict-only one — `content_render_composed` / `contentRenderComposed`, returning the verdict and the attribution together — rather than a fifth argument: this uniffi version supports `#[uniffi(default = …)]` only on record fields, not on exported-function parameters, so a fifth argument would have broken the Kotlin, Swift, C# and Go call sites at once. **What C1 left unbuilt** was everything that would let any of this bind: the relay kind (C2 — built since, below), the app-side fetch, device store, platform region leaf and render (C3 — built on tui since, below), and the nest-as-publisher leg (C4 — built since, below). The compiled-in registry is still empty, so no document reaches any consumer today.

**The nest-as-publisher leg is BUILT (2026-09-10)**, as § The nest-as-publisher leg's build notes describe: the public web render (`bins/fauna-nest/src/web_content/region.rs`, the nest's only caller of the composed engine — a test pins that) folds the declared situs chain's content policies over each published post's held labels plus its bundled scorers' output, and a blocked or collapsed post renders the reasoned placeholder on every page carrying it, the sealed full page included. `fauna.admin.region.set`, a relay acceptance and a relay retirement each re-render every publishing site. The scorers run through `fauna_labeler::region` — the shared runner the app side (C3) reuses rather than growing its own — over `fauna_core::scoring::LabelerPostInput::from_post`. Proofs: `conformance_region_tier.rs` (`the_situs_content_policy_governs_the_public_page_and_nothing_else`: declare → a document accepted → the placeholder on the page with no test-side render → the authenticated `fauna.posts.get` untouched → withdraw → re-declare → de-list, each reaching the page by itself) + the render's unit set in `web_content/service.rs` (block, collapse, a de-listed document binding nothing, list and wasm scorers, the sealed full page) + `fauna_labeler::region`'s. It is inert today on every deployment for the same reason the rest of the plane is: the compiled-in registry enrols nobody.

**The relay is BUILT (2026-09-10)** — `fauna.region.artifact.get { region, payload_kind }`, class `User`, answered from the nest's per-(region, kind) cache `region_relay_cache` (schema 59) and never from the network on the request path (`bins/fauna-nest/src/region_relay.rs`). The ask **is** the demand: a pair nobody asked for has no row and is never fetched, and a pair's first ask schedules one background refill rather than waiting a whole cadence; thereafter the region tier's worker refills every demanded pair on the same `REFRESH_INTERVAL` tick, before its situs checks, so a nest declaring no region of its own still relays for its apps. The three rules the feature plane holds carry over unchanged: the ingress is still only the compiled-in log (no kind submits, and the kind set around the relay is pinned to exactly `fauna.admin.region.{get,set}` + `fauna.region.artifact.get`); an unenrolled region is never fetched and records no attempt; and a refused or failed refresh writes nothing, the refusal coming from the **same** `region_sequence_floor` the feature plane raises, in the same transaction as each acceptance. **De-listing retires what the relay holds**: each pass re-verifies every cached envelope against the current registry and clears one whose authority is gone, keeping the demand. The relay **never decodes the document inside** — `verify_artifact` checks bounds, registry, key era, clock, sequence and signature and is payload-agnostic — so an app newer than its nest can read a document version the nest's build does not know; the conformance witness relays a payload that is not a policy document at all. **Staleness reads the last time the log answered** (`reached_at`: an accepted or a refused artifact), measured from the first ask for a pair it never answered, so an unreachable log goes stale while the worker keeps trying. Proofs: `conformance_region_tier.rs` (the relayed envelope, byte for byte; the unenrolled region; the refused replay; de-listing, mutation-killed; the pinned kind set and an admin declaration leaving the answer untouched) + `region_relay.rs`'s staleness unit pair. **The app side is BUILT on tui, the lead app (2026-09-26)**, with the element-ID package the user approved 2026-09-25 in ui.yaml. The shared half is `libs/fauna-client-region`: the declared-region contract (`DeclaredRegion { code, source }`) and the POSIX-locale parse two shells share; `effective_registry` (the compiled-in snapshot, plus — in test-capable builds only — the `FAUNA_E2E_REGION_REGISTRY` seed, convention 15); `RegionPlane`, which folds each relay reply through `admit_artifact` and `verify_artifact` against the app's own registry and its own authority-keyed replay floor, holds the last-known-good documents, serializes one at-rest byte record (re-verified at load, so a de-listed authority stops binding), answers the chain's `RegionRuleSet`s for the composed engine and the bundled scorers' factors through `fauna_labeler::region`, and paints the settings view; and `fetch_chain` over `fauna.region.artifact.get`. The refresh cadence and staleness bound are `fauna_core::region_authority::{REFRESH_INTERVAL_SECS, STALE_AFTER_SECS}`, one clock for the nest and every app. tui's leaf is the locale territory (`LC_ALL`, else `LANG`); its record lives in its config dir, device-scoped, restored at start ahead of the first fetch and kept across identity changes; it refreshes at login and on the cadence. Feed cards, post detail (which had composed no source at all before) and conversation bubbles paint `region-blocked-notice`/`-authority`/`-reason` (+ `region-collapsed-reveal-button`) in place of the body; Settings carries the `settings-region-*` section. Proofs: the crate's tier_1 set; tui's render tests; `test_region_content_policy.py` (tier_3, `--app tui`: the relay seed → settings names the authority and sequence → the exact card and the exact bubble → collapse reveals → a cold relaunch after the relay forgets still blocks → an unimplemented grammar version is inert and says so), driven by the `test-hooks` seed `/api/v1/test/region/content-policy`; and the convention-17 invariant `region-block-never-silent` riding every e2e frame (tui's `region_block_render` state — each surface's own verdict walk against the painted block placeholders — red-verified against a reverted feed render). **The six-app trickle-down (2026-09-26) first lifted tui's glue into the shared crate** — `RegionPlane::apply_replies` / `join_labels`, `refresh_due`, the placeholder fold `placeholder_for` (which verbs paint, which language of the reason shows), the native device-record file (`fauna_client_region::store`), the browser leaf's parse `declared_from_bcp47` (source `BrowserLocale`, key `region.source_browser_locale`), and one test-capable-build-only declared-region override every leaf passes through (`with_e2e_override`, `FAUNA_E2E_REGION_DECLARED`, keeping the leaf's source — how a journey declares the synthetic region where a test cannot set a storefront or a user geo; the registry seed gained a hex entry point, `seeded_registry`, for a shell that reads no environment) — then built one face per boundary: `FfiRegionPlane` (`libs/fauna-ffi/src/region.rs` — open with the leaf's answer and the config dir, `refresh`/`refresh_if_due` over the session's nest, `render` → verdict + placeholder, `view`) for the four UniFFI apps, and `WasmRegionPlane` (`libs/fauna-wasm/src/region.rs`, the SPA keeping the record's bytes in browser storage; its e2e seeds come from browser storage under `test-helpers`). **linux is BUILT (2026-09-26)**: the POSIX-locale leaf, the record under `$XDG_CONFIG_HOME/fauna/region/`, refresh at login + a minute tick on the shared cadence, the region arm ahead of the family arm on feed cards, post detail and conversation bubbles, and the `settings-region-*` group on the Status sub-page after feature limits; `test_region_content_policy.py` is green on it (and on tui) through the shared declared-region override; it publishes the convention-17 `region_block_render` counts too (each on-screen surface's verdict walk over its snapshot against the mapped block placeholders, `region::block_render_json`). **web is BUILT (2026-09-26)**: the browser-language leaf (`navigator.language`, parsed in shared Rust), the record in IndexedDB (an envelope can reach 4 MiB, past what `localStorage` holds), a refresh at every connect plus the minute tick on the shared cadence, the region arm ahead of the family arm on feed cards, post detail and conversation bubbles (one `contentRender` call per item, the region composed in), the `settings-region-*` section on the Status sub-page after feature limits, and `region_block_render`; its e2e declaration is the two browser-storage keys `WasmRegionPlane` reads under `test-helpers`; the journey is green on it. **macOS and iOS are BUILT (2026-09-26)**, one shared FaunaKit leg (`RegionStore` over `FfiRegionPlane`, `RegionViews.swift`): the leaf is the OS's user-set region (`Locale.current.region`, source `SystemRegion`) on a self-built or sideloaded build, and on a store-distributed one (decided by the App Store receipt's presence — a local file check, never a StoreKit transaction call that could prompt a sideloaded user to sign in) the storefront (StoreKit's `Storefront.current`, ISO 3166-1 alpha-3, mapped to the region in shared Rust by `fauna_client_region::iso3166` / `DeclaredRegion::from_storefront_alpha3`, source `Storefront`), with the plane opened pending on the recorded declaration until the storefront answers (`RegionPlane::new_pending` / `redeclare`, `FfiRegionPlane::open_pending` / `redeclare_storefront`). No apple store-distributed artifact exists yet, so the store arm is proven headlessly (the crate's and `fauna-ffi`'s tier_1 sets, FaunaKit's leaf tests) and only its label on a real store install is unseen — the record under the install-scoped `<Application Support>/Fauna/`, a refresh at login and every reconnect plus the minute tick on the shared cadence, the region arm ahead of the family arm on feed cards, both post details and conversation bubbles (one `ContentPolicyInputs.recordedRender` door), the `settings-region-*` section on the Status sub-page after feature limits, and `region_block_render` (each item container's verdict against the block placeholders painted, `#if DEBUG`). **windows is BUILT (2026-09-28)** over the same `FfiRegionPlane` (`RegionPlaneHost` in FaunaApp.Core, the leaf in `Services/RegionLeaf.cs`): the leaf is the Windows "Country or region" setting (`GlobalizationPreferences.HomeGeographicRegion`, source `SystemRegion` — not `RegionInfo.CurrentRegion`, which follows the display-format culture), for a Store build too, since the Microsoft Store's market *is* that setting and there is no separate storefront to read (refutable: a Store build, none of which exists yet, would prove it); the record under the install-scoped `%LocalAppData%\Fauna` (never Credential Manager), opened at launch ahead of the first fetch; a refresh at login and every reconnect plus the minute tick on the shared cadence; the region arm ahead of the family and muted arms on feed cards, post detail (which had composed no source before) and conversation bubbles, one `ContentPolicyCache.RenderFor` door (`SocialRenderGate`'s `RegionWithheld`); the `settings-region-*` section on the Status sub-page after feature limits; and `region_block_render` (each on-screen item container's verdict against the block placeholders painted). `test_region_content_policy.py` is green on it. **Still unbuilt on the app side:** android (its FFI face exists; no app calls it yet), and the app-side inclusion check has nothing to check until the log publishes (`admit_artifact` admits evidence-less artifacts only in the pre-log era, exactly as the nest's). The declared-region control — the one piece of this plumbing a human touches, and the declaration **both** planes read — **has its admin screen on tui as of 2026-08-11** (the lead app), then on linux, web, and android as of 2026-08-20, then macOS + iOS as of 2026-08-25, then windows as of 2026-08-27 (the last of 7 apps) — each lift a paint of the shared `fauna_client_admin::admin_region_view`, not a build. So when the *content* plane is built it inherits a declared situs rather than needing one: surface owner [`admin.md`](admin.md) § N Nest → *Declared region*, which carries current per-app state. What else exists today:

- The public fauna.social `/safety` page described this direction as planned (clearly marked design-stage) until the 2026-08-04 site consolidation retired the page (content archived, not live); the bylaws page's guiding principle 3 is the only live public-site mention today.
- The reconciling one-line pointers in [`moderation.md`](moderation.md) § Categories & enforcement (item 3), [`../architecture/content-scoring.md`](../architecture/content-scoring.md) (the nest-as-publisher placement row) and [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) § Boundaries landed with this doc.
- The built legal takedown ([`moderation.md`](moderation.md) § Legal takedown) is the sibling compulsory mechanism and is unaffected.

## Reading list

1. `docs/goal/architecture/content-moderation-and-ranking.md` — the frame: region blocking is a tier-2 (authority) instance; § Boundaries names it beside the mail-perimeter and legal-takedown compulsory surfaces.
2. `docs/goal/architecture/content-scoring.md` — the placement rule this design obeys; the nest-as-publisher carve-out row is owned there.
3. `docs/goal/behavior/moderation.md` — the enforcement model this joins (§ Categories & enforcement item 3) and the distinct legal-takedown mechanism.
4. `docs/goal/behavior/family-client-enforcement.md` § Content policy — the shared render-verdict engine, the likely render seam.
5. The Fauna bylaws, guiding principle 3 — "Authorities can block illegal content" (published at fauna.social/organization/bylaws).
6. `sites/fauna-social/src/pages/organization/bylaws.astro` — guiding principle 3, the only live public-site statement of this direction today; the former `/safety` page's fuller description is archived, not live, at `docs/sites/fauna-social/archived/long/safety.md` (must never run ahead of this doc).
