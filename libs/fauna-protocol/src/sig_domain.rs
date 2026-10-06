//! Domain-separation prefixes for every Fauna signing context, whichever key
//! signs it.
//!
//! `key-material-hierarchy.md` § Architectural rules #8 (and its `security.md`
//! § Shared-signing-key domain separation restatement) requires each signed
//! message to begin with a **distinct constant domain-separation prefix**
//! whenever one Ed25519 key signs in more than one protocol context, so a
//! signature made in one context can never be reinterpreted as valid in another
//! — cross-context confusion is then *structurally* impossible rather than
//! resting on the message formats happening to be disjoint (the signing analogue
//! of rule #3's key-derivation contexts). One pairwise-distinct, prefix-free tag
//! namespace serves the whole system so the guarantee holds no matter which key
//! signs (rule #8 is "shared across every domain-separated signing key in the
//! system, not only the deployment key").
//!
//! Three key families register here today. The **deployment** key (the nest's
//! single identity) — cert-binding, federation, the rotation statement, and the
//! retired outbox and KeyBlob contexts. The **device / custodian** keys — the renewal-device
//! handshake / grant-revoke family (plus the retired adopt tag) and the custody
//! handshake. And the
//! **actor** (user) key — fully swept under the finding:
//! [`CLAIM_ADMIN_V1`], [`ACCOUNT_LOCKOUT_V1`], [`AUTH_HANDSHAKE_V2`],
//! [`AUTH_VERIFY_V2`], [`ACCOUNT_REGISTER_V1`], [`INVITE_SUBMIT_V1`],
//! [`INVITE_CANCEL_V1`], [`SETUP_NAT_MODE_V1`], [`SETUP_NAT_MODE_V2`],
//! [`SETUP_STORAGE_MODE_V1`] — all applied **tagged-only** (the
//! additive-for-compat transition was collapsed under the 2026-08-17
//! no-existing-users ratification; no untagged actor-key signer or verifier
//! remains).
//!
//! The registry below **exhaustively** enumerates every context the deployment
//! key signs in, plus the tagged
//! device/custodian and actor contexts.
//! Three carry an **applied** tag — cert-binding ([`CERT_BINDING_V1`], context A),
//! federation ([`FEDERATION_HELLO_V1`], context C) and the deployment-seed
//! rotation statement ([`NEST_ROTATION_V1`], applied from birth). Two are
//! *reserved but not applied*, both **retired**: the **outbox**
//! ([`OUTBOX_FORWARDED_POST_V1`], the former context B) — post forwarding moved
//! to the channel-authenticated `fauna.federation.post.forward` kind and no
//! longer nest-signs each forward — and the **KeyBlob** ([`KEYBLOB_V1`], the
//! former contexts D/E) — the nest wraps no subscription period key any more,
//! so the subscription and archival KeyBlob signers left the nest with the
//! nest-held period-key plane (2026-09-27, the compat-remnant sweep); every
//! KeyBlob is now minted and signed by the author's client. Each retirement only
//! *strengthens* the exclusion argument below: it removes a deployment-key
//! signing context outright, so the deployment key signs no bare CID at all.
//!
//! **Why KeyBlob was exempt (structural, not incidental) — kept for the
//! reservation.** Unlike A/B/C — each a *bespoke* per-context signer where
//! prefixing a tag is a one-line change — the KeyBlob was the **only**
//! deployment-key context that rode the **universal
//! embed-as-bytes / [`fauna_cbor::SignedEnvelope`] sign-over-CID wire** shared by
//! *every* signed Fauna kind (Post, Profile, DeliveryReceipt, ContactRequest,
//! DeviceAuthorization, Tombstone, ShareToken). Its signature lives
//! in the fixed **100-byte envelope** (36-byte CID ‖ 64-byte sig), pinned by the
//! cross-language fixtures (`libs/fauna-cbor/tests/cross_language_interop.rs`) and
//! `serialization.md` § Embed-as-bytes — so a tag can be neither a struct field
//! (it would sit *inside* the signed value, circular) nor an envelope extension
//! (breaks the 100-byte invariant). Applying it would mean **forking KeyBlob off
//! the universal wire** onto a bespoke sidecar signature across all 7 apps,
//! for **zero** forgery reduction: the signed message is a fixed 36-byte CID with
//! **no attacker-influenced tail**, so cross-context confusion would need a blake3
//! collision/preimage on that CID (infeasible). And the separation is
//! **structural by exclusion**: once A and C carry their tags, the KeyBlob is the
//! *sole* deployment-key context that verifies a **bare** CID (B no longer signs
//! at all; A and C verifiers each require their distinct prefix; A's untagged
//! compat half, and the verifier fallback that accepted it, were removed
//! 2026-09-24 by the compat-remnant sweep, so the deployment key signs no bare
//! channel-binding bytes at all — see [`CERT_BINDING_V1`]), so there is
//! no second bare-CID verifier for a KeyBlob signature
//! to be confused with. Tagging it would trade the clean universal wire for
//! divergence and tech debt and buy nothing.
//!
//! Each applied tag follows the established codebase convention (`lan_cert.rs`
//! `LAN_CERT_SIG_CONTEXT`): `b"fauna.<context>.v<n>\0"`. The trailing NUL makes
//! every tag **self-delimiting** — no tag is a prefix of another, and the tag
//! can never blend ambiguously into the (sometimes attacker-influenced) bytes
//! that follow it.
//!
//! **Invariant for adding a signer:** never sign over attacker-/peer-chosen raw
//! bytes with the deployment key without prefixing one of these tags, and add a
//! new tag here in the same change that introduces a new context
//! (`key-material-hierarchy.md` § Don't do these). With the KeyBlob signers
//! retired, the deployment key has **no** untagged context left. Any **new**
//! deployment-key signing context — even
//! one that signs over a CID — MUST carry an applied tag (exactly as the
//! federation context C did, a fresh bespoke signer with no shared-wire barrier);
//! introducing a *second* untagged bare-CID verifier would re-open cross-context
//! confusion that the exclusion argument above currently rules out.

/// TOFU transport channel-binding proof. **Two producers** share this tag, both in
/// `bins/fauna-nest/src/auth_handlers.rs` via the single signer
/// `sign_channel_binding`: `build_cert_binding`, over `served_cert_spki ‖
/// client_nonce` (and `served_cert_spki ‖ challenge_nonce ‖ client_nonce` on the
/// verify path), and the cert-*optional* twin `build_identity_binding`, which serves
/// the pre-identity `fauna.auth.nest_handshake` and has no SPKI to prefix on a nest
/// with no readable served cert.
///
/// The highest-risk member: the nonce tail is attacker-influenced. Carried on the
/// required `CertBinding.tagged_sig` wire field — the **only** signature a
/// binding carries since 2026-09-24, when the compat-remnant sweep removed the
/// untagged `sig` half that once rode beside it for pre-tag peers (and the
/// verifier fallback that accepted it). With every emitted signature tagged,
/// the exclusion argument above needs no producer-side bound to survive: no
/// message this context signs is a bare 36-byte CID. The producers still
/// **enforce** the canonical nonce length ([`crate::auth::CLIENT_NONCE_LEN`],
/// or its 64-byte challenge fold) as rule #8 guard 1 — a wire field a caller
/// can make any length is bounded by policy owned at the signer, not trusted
/// from the field docs.
pub const CERT_BINDING_V1: &[u8] = b"fauna.cert-binding.v1\0";

/// Private-nest outbox `ForwardedPost` envelope signer — **RETIRED, RESERVED.**
///
/// Nothing signs this any more. The context existed for the HTTP twin
/// `POST /api/v1/forward`, whose JSON body carried an extra nest-signature over
/// `{post, blobs, private_nest_id, timestamp}`. Spec Y2 slice 5 deleted that
/// route: post forwarding is now `fauna.federation.post.forward` on the nest↔nest
/// channel, which authenticates this nest to the peer **once at handshake**, so
/// the per-forward nest-signature is redundant — only the post's own *author*
/// sign-over-CID envelope rides (`private-mode.md` § Post Forwarding). The signer
/// and its builder were removed with the outbox's channel migration; in practice
/// no signature under this tag was ever produced, because the builder was never
/// wired onto the post-create path.
///
/// The tag stays **reserved and must never be reused**: it keeps the pairwise-
/// distinct + prefix-free guarantees the registry tests assert, so a value that
/// once denoted "outbox forward" can never be re-minted for a different context.
pub const OUTBOX_FORWARDED_POST_V1: &[u8] = b"fauna.outbox.forwarded-post.v1\0";

/// Nest↔nest federation channel handshake sign-over-CID (`federation_sig.rs`
/// `sign_payload`, over the canonical dag-cbor CID). Not live in alpha (no
/// external peers), but exercised by the cross-language e2e harness.
pub const FEDERATION_HELLO_V1: &[u8] = b"fauna.federation.hello.v1\0";

/// The former **deployment-key** subscription + archival KeyBlob signer
/// (contexts D/E), over the bare dag-cbor CID via the universal embed-as-bytes
/// [`fauna_cbor::SignedEnvelope`] wire.
///
/// **RESERVED, NOT APPLIED — and RETIRED.** It was documented exempt while it
/// lived (see the module docs: it rode the universal sign-over-CID wire shared by
/// every signed kind, had no attacker-influenced tail, and was the sole bare-CID
/// deployment-key verifier ⇒ structurally separated by exclusion); the nest
/// signers left with the nest-held period-key plane, and every KeyBlob is now
/// minted by the author's client. The tag stays registered so it is never
/// reused, and so a future retrofit of the KeyBlob wire (e.g. a major bump that
/// reshapes it anyway) uses a value already proven pairwise-distinct +
/// prefix-free against the applied tags.
pub const KEYBLOB_V1: &[u8] = b"fauna.subscription.keyblob.v1\0";

/// Deployment-seed **rotation statement** signer (`nest_rotation::NestRotation`,
/// over the canonical dag-cbor encoding of the statement). **APPLIED from birth**
/// — a new deployment-key signing context has no compat reason to go untagged
/// (`key-material-hierarchy.md` § Don't do these), and this one is signed *twice*
/// per statement: once by the superseded key (`old_sig`, the continuity license)
/// and once by the successor (`new_sig`, possession proof). Both signatures carry
/// this same tag; they are told apart by which key verifies them, exactly as the
/// succession plane's per-role signatures are.
///
/// Owner: `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed
/// rotation → *The rotation statement (wire)*.
pub const NEST_ROTATION_V1: &[u8] = b"fauna.nest-rotation.v1\0";

/// **RETIRED 2026-09-23 — never reuse.** The unbound form: it named no nest,
/// so one signature was a bearer mint at every nest where the signer was
/// known (`login.md` § Binding the nest). Replaced **in place** by the
/// nest-bound [`DEVICE_HANDSHAKE_V2`] — a user-ruled pre-user reset, the third such write-off
/// (`version-compatibility.md` § Dimension 2) — with no accept path kept.
/// Registered only so the bytes stay pairwise-distinct from every live tag.
/// Was: sync-agent renewal-grant handshake (`fauna.auth.device_handshake`), over
/// `actor_id ‖ device_key ‖ timestamp_be ‖ nonce`. Unlike
/// every tag above it is signed by a **renewal device key** (the fresh
/// per-agent Ed25519 keypair the identity client mints and authorizes via a
/// `RenewBearer`-scoped `DeviceAuthorization`), never by the deployment key
/// and never by the identity key. That key signs in exactly this one context,
/// so no cross-context confusion exists to exclude today; the tag is applied
/// from day one (a NEW wire surface has no compat reason to go untagged —
/// `key-material-hierarchy.md` § Don't do these) so the guarantee is
/// structural if the key ever gains a second context. Registered here to keep
/// one pairwise-distinct, prefix-free tag namespace for every Fauna signing
/// context, whichever key signs it. Owner:
/// `docs/goal/architecture/apps/sync-agent.md` § Credential model.
pub const DEVICE_HANDSHAKE_V1: &[u8] = b"fauna.device-auth.handshake.v1\0";

/// Sync-agent renewal-grant handshake (`fauna.auth.device_handshake`), over
/// `actor_id ‖ device_key ‖ timestamp_be ‖ nest_id ‖ nonce` — signed by the
/// **renewal device** key ([`crate::auth::device_handshake_signed_message`]).
/// **APPLIED.** Binds the receiving nest's identity: [`DEVICE_HANDSHAKE_V1`]
/// named no nest, so a grant the app registered on two linked nests made one
/// signature a bearer at either — a nest the agent connected to could relay
/// it (`login.md` § Binding the nest). Same mandatory nonce, same drift window
/// and replay guard as V1.
pub const DEVICE_HANDSHAKE_V2: &[u8] = b"fauna.device-auth.handshake.v2\0";

/// Sync-agent **self-retirement** of a renewal grant
/// (`fauna.sync.device_grant.revoke`), over `actor_id ‖ device_key ‖
/// timestamp_be ‖ nonce`. **APPLIED.** Signed by the very renewal device key
/// being retired — the proof-of-possession arm of the revoke's two
/// authorizations (`sync-agent.md` § Credential model → the RULED 2026-08-15
/// block, decision 2). Distinct from [`DEVICE_HANDSHAKE_V1`] precisely because
/// the two are the *same key over the same field layout*: without separate
/// tags, a captured handshake signature would replay as a revoke of the
/// signer's own grant (a denial-of-renewal any eavesdropper could mount), and
/// a captured revoke would replay as a mint. Owner:
/// `docs/goal/architecture/apps/sync-agent.md` § Credential model.
pub const DEVICE_GRANT_REVOKE_V1: &[u8] = b"fauna.device-auth.grant-revoke.v1\0";

/// **RETIRED 2026-09-29 — never reuse.** Registered only so the bytes stay
/// pairwise-distinct from every live tag. Was: the self arm of the retired
/// grant adoption kind `fauna.sync.device.adopt`, over `actor_id ‖ device_key
/// ‖ target_device_id ‖ timestamp_be ‖ nonce`, signed by the renewal device
/// key being moved. The kind, its handler and its signer went with the
/// writer-pub-hex placeholder row it converged
/// (`docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model,
/// the RULED 2026-09-28 block, decision 4). The same key still signs the
/// revoke, and an adoption body re-splits byte-for-byte into a revoke body
/// (the nonce is variable-length and last), so these bytes must never name a
/// new context.
pub const DEVICE_ADOPT_V1: &[u8] = b"fauna.device-auth.adopt.v1\0";

/// A device's **own report of its p2p participation**
/// (`fauna.sync.devices.p2p_participation.set`, the self arm), over `actor_id
/// ‖ device_key ‖ device_id ‖ participating ‖ timestamp_be ‖ nonce`.
/// **APPLIED.** Signed by the row's own principal — the device key the
/// enrollment granted on it — which is what makes "only the device itself
/// can enable its listeners" a verified property rather than a convention
/// (`docs/goal/behavior/p2p.md` § Per-device participation): the owner arm
/// of the same kind carries no signature and can only brake.
///
/// Its own tag for the reason [`DEVICE_GRANT_REVOKE_V1`] has one: the same key
/// over an overlapping field layout. `device_id` and the `participating` byte sit
/// inside the body so a captured report can neither be re-aimed at another
/// row nor replayed with its verdict flipped.
pub const DEVICE_P2P_PARTICIPATION_V1: &[u8] = b"fauna.device-auth.p2p-participation.v1\0";

/// One-time admin **claim** (`fauna.auth.claim_admin`), over `actor_id ‖
/// timestamp_be` — signed by the claiming **actor** key. **APPLIED**,
/// tagged-only: `ClaimAdminRequest.signature` carries a signature over
/// [`crate::claim::claim_admin_signed_message`] and nothing else verifies.
///
/// Its own tag is the crux of the finding: the untagged `actor_id ‖
/// timestamp_be` a claim used to verify was **byte-identical** to a no-nonce
/// login handshake signature (the pre-tag
/// [`crate::auth::handshake_signed_message`] with `client_nonce = None`), so
/// every client login signature was also a valid `claim_admin` signature —
/// replayable within the 300 s window to an attacker's own unclaimed nest,
/// minting the victim as its superadmin. The tag makes a claim signature
/// structurally un-confusable with a login (or a lockout) one; the untagged
/// accept was deleted outright under the 2026-08-17 no-existing-users
/// ratification. Owner: `key-material-hierarchy.md` § Architectural rules #8.
pub const CLAIM_ADMIN_V1: &[u8] = b"fauna.auth.claim-admin.v1\0";

/// Emergency no-token **account lockout** (`fauna.account.lockout`), over
/// `actor_id ‖ timestamp_be` — signed by the **actor** key on the anonymous
/// (pre-identity) surface. **APPLIED**, tagged-only:
/// `AccountLockoutRequest.signature` carries a signature over
/// [`crate::account::account_lockout_signed_message`] and nothing else verifies.
///
/// Its own tag for the standing reason: the lockout
/// message is the same `actor_id ‖ timestamp_be` shape as claim-admin and the
/// login handshake, separated today only by which timestamp *unit* each
/// freshness check accepts (lockout: seconds; claim/login: milliseconds) — and
/// rule #8 forbids resting separation on the message formats "happening to be
/// disjoint" by value range. The tag makes it structural instead.
pub const ACCOUNT_LOCKOUT_V1: &[u8] = b"fauna.account.lockout.v1\0";

/// **RETIRED 2026-09-23 — never reuse.** The unbound form: it named no nest,
/// so one signature was a bearer mint at every nest where the signer was
/// known (`login.md` § Binding the nest). Replaced **in place** by the
/// nest-bound [`CUSTODY_HANDSHAKE_V2`] — a user-ruled pre-user reset, the third such write-off
/// (`version-compatibility.md` § Dimension 2) — with no accept path kept.
/// Registered only so the bytes stay pairwise-distinct from every live tag.
/// Was: **custody handshake** proof-of-possession (`fauna.auth.custody_handshake`),
/// over `owner_actor_id ‖ custodian_key ‖ timestamp_be ‖ nonce`.
/// Signed by the custodian's device-principal key — the PoP half of the nest
/// custody door (W8.6 (account-data-plane.md § Workstreams), `account-data-plane.md` § Replica posture → *The
/// custody grant + ceremony*; the witness half rides beside it and is
/// verified separately). Its own tag for the standing reason: the custodian
/// key also signs the peer-channel identity proof and (on its own account's
/// nest) the device-handshake family over overlapping field layouts, so an
/// untagged or tag-shared signature could replay across doors that mint
/// different authority. Owner: `account-data-plane.md` § Replica posture.
pub const CUSTODY_HANDSHAKE_V1: &[u8] = b"fauna.custody.handshake.v1\0";

/// Custody-session handshake (`fauna.auth.custody_handshake`), over
/// `owner_actor_id ‖ custodian_key ‖ timestamp_be ‖ nest_id ‖ nonce` — signed
/// by the **custodian** key ([`crate::auth::custody_handshake_signed_message`]).
/// **APPLIED.** The custody instance of the nest binding
/// ([`DEVICE_HANDSHAKE_V2`], `login.md` § Binding the nest): a custodian's
/// unbound PoP could be relayed by the owner's nest to another nest holding
/// the same grant. Its own tag for the standing reason [`CUSTODY_HANDSHAKE_V1`]
/// had one — overlapping field layouts under the same key family.
pub const CUSTODY_HANDSHAKE_V2: &[u8] = b"fauna.custody.handshake.v2\0";

/// **RETIRED 2026-09-23 — never reuse.** The unbound form: it named no nest,
/// so one signature was a bearer mint at every nest where the signer was
/// known (`login.md` § Binding the nest). Replaced **in place** by the
/// nest-bound [`AUTH_HANDSHAKE_V2`] — a user-ruled pre-user reset, the third such write-off
/// (`version-compatibility.md` § Dimension 2) — with no accept path kept.
/// Registered only so the bytes stay pairwise-distinct from every live tag.
/// Was: direct-auth login handshake (`fauna.auth.handshake`), over `actor_id ‖
/// timestamp_be [‖ client_nonce]` — signed by the **actor** key. The login
/// message was the confusable half of the finding's item 1: untagged, its
/// no-nonce form was byte-identical to what claim-admin verified, so every
/// client login signature doubled as a claim signature. Injective without
/// length prefixes: the two fixed-width fields come first and the only
/// variable field (`client_nonce`) is the tail.
pub const AUTH_HANDSHAKE_V1: &[u8] = b"fauna.auth.handshake.v1\0";

/// Direct-auth login handshake (`fauna.auth.handshake`), over `actor_id ‖
/// timestamp_be ‖ nest_id ‖ client_nonce` — signed by the **actor** key on the
/// anonymous surface ([`crate::auth::handshake_signed_message`], the single
/// source every signer and the nest verifier share). **APPLIED.** Binds the
/// receiving nest's identity — the channel-binding `nest_actor_id` the client
/// read off the same connection — so a blob signed for one nest verifies at no
/// other (`login.md` § Binding the nest). The nonce is mandatory: it is still
/// what uniquifies the deterministic signature for the replay guard, and no
/// nonce-less signer remains. Injective without length prefixes: three
/// fixed-width fields first, the sole variable field (`client_nonce`) as the
/// tail.
pub const AUTH_HANDSHAKE_V2: &[u8] = b"fauna.auth.handshake.v2\0";

/// **RETIRED 2026-09-23 — never reuse.** The unbound form: it named no nest,
/// so one signature was a bearer mint at every nest where the signer was
/// known (`login.md` § Binding the nest). Replaced **in place** by the
/// nest-bound [`AUTH_VERIFY_V2`] — a user-ruled pre-user reset, the third such write-off
/// (`version-compatibility.md` § Dimension 2) — with no accept path kept.
/// Registered only so the bytes stay pairwise-distinct from every live tag.
/// Was: challenge-response verify (`fauna.auth.verify`), over `actor_id ‖ nonce`
/// — signed by the **actor** key. Fixed 32 ‖ 32 layout, injective by construction; the tag is what
/// keeps a verify signature structurally distinct from every other actor-key
/// context (rule #8's "never rest on formats happening to be disjoint").
pub const AUTH_VERIFY_V1: &[u8] = b"fauna.auth.verify.v1\0";

/// Challenge-response verify (`fauna.auth.verify`), over `actor_id ‖ nonce ‖
/// nest_id` — signed by the **actor** key
/// ([`crate::auth::challenge_verify_signed_message`]). **APPLIED.** The
/// silent-challenge twin of [`AUTH_HANDSHAKE_V2`], and the one that matters
/// hourly: every app-held bearer, refresh included, is minted here, so the
/// unbound [`AUTH_VERIFY_V1`] was a *continuous* relay window rather than a
/// one-shot one. Fixed 32 ‖ 32 ‖ 32 layout, injective by construction.
pub const AUTH_VERIFY_V2: &[u8] = b"fauna.auth.verify.v2\0";

/// Account registration (`fauna.account.register`), over the **length-prefixed**
/// `actor_id, handle, domain, timestamp_be` element list
/// ([`crate::account::register_signed_message`]) — signed by the **actor** key.
/// **APPLIED.** Length-prefixed (item 3 of the finding): the untagged legacy
/// form placed two variable-length fields (`handle`, `domain`) adjacent with no
/// delimiter, so distinct `(handle, domain)` splits could yield the same bytes —
/// reachable on a multi-domain nest, whose verifier tries each active domain as
/// a candidate.
pub const ACCOUNT_REGISTER_V1: &[u8] = b"fauna.account.register.v1\0";

/// Invite-request submission (`fauna.account.invite_request.submit`), over the
/// **length-prefixed** `actor_id, handle, message, timestamp_be` element list
/// ([`crate::invite::invite_submit_signed_message`]) — signed by the **actor**
/// key. **APPLIED.** Length-prefixed for the same item-3 reason as registration:
/// `handle` and the free-text `message` sat adjacent with no delimiter.
pub const INVITE_SUBMIT_V1: &[u8] = b"fauna.account.invite-request.submit.v1\0";

/// Invite-request cancellation (`fauna.account.invite_request.cancel`), over
/// `actor_id ‖ timestamp_be` ([`crate::invite::invite_cancel_signed_message`]) —
/// signed by the **actor** key. **APPLIED.** Replaces the ad-hoc literal
/// `b"cancel"` separator the legacy message carried: that literal was doing a
/// domain tag's job without the registry's distinctness guarantees, and its
/// message was otherwise the same `actor ‖ ts_be` shape as claim/lockout/login.
pub const INVITE_CANCEL_V1: &[u8] = b"fauna.account.invite-request.cancel.v1\0";

/// Attested age claim (`family-safety.md` § The account age band), over the
/// **length-prefixed** `nonce, band, application_id, actor_id` element list
/// ([`crate::age::age_claim_signed_message`]) — the payload the **platform**
/// (Apple App Attest / Google Play Integrity) signs over, never a key held by
/// the Fauna app or the actor. The iOS app hashes the built message into its
/// `clientDataHash`; the nest verifier recomputes it. Length-prefixed from
/// birth: `band` and `application_id` are adjacent variable-length fields.
pub const ACCOUNT_AGE_CLAIM_V1: &[u8] = b"fauna.account.age-claim.v1\0";

/// **RETIRED 2026-09-24 — never reuse.** The unbound wizard NAT-mode commit
/// (`fauna.setup.nat_mode` without `nest_id`), over `mode ‖ \n ‖ actor_id_hex
/// ‖ \n ‖ ts_decimal`, signed by the **actor** key. It named no nest, so one
/// blob was valid at every nest where that actor was admin (PROBE-482-B); the
/// nest-bound [`SETUP_NAT_MODE_V2`] replaced it, and the V1 accept path the
/// nest kept alongside for a transition window was removed **in place** by
/// the compat-remnant sweep (`version-compatibility.md` § Dimension 2, the
/// fourth user-ruled write-off) with no accept path kept. Registered only so
/// the bytes stay pairwise-distinct from every live tag.
pub const SETUP_NAT_MODE_V1: &[u8] = b"fauna.setup.nat-mode.v1\0";

/// The **nest-bound** NAT-mode commit (`fauna.setup.nat_mode`), over `mode ‖
/// \n ‖ actor_id_hex ‖ \n ‖ ts_decimal ‖ \n ‖ nest_id_hex`
/// ([`crate::nat_mode::nat_mode_signed_message`]) — signed by the **actor**
/// key during onboarding and from the admin panel. **APPLIED — the only
/// form.** The nest identity is the binding (the verifier requires its own
/// identity, before any signature work), so a blob signed for one nest
/// verifies at no other. A context the finding's table missed:
/// the newline-delimited text shape made collision with the binary contexts
/// unlikely, but rule #8 requires the separation to be structural, not
/// incidental. Injective within the context: all four fields are newline-free
/// (closed enum, hex, decimal, hex).
pub const SETUP_NAT_MODE_V2: &[u8] = b"fauna.setup.nat-mode.v2\0";

/// **RETIRED 2026-09-24 — never reuse.** The wizard storage-mode commit
/// (`fauna.setup.storage_mode`): the same canonical body shape as the
/// NAT-mode commit with the storage-mode wire string, signed by the **actor**
/// key — its own tag because the two bodies were the same layout over
/// overlapping enum vocabularies. The storage-mode axis retired 2026-07-12
/// and the kind lived on as a validate-then-discard shim for pre-retirement
/// clients until the compat-remnant sweep removed it
/// (`version-compatibility.md` § Dimension 2, the fourth write-off).
/// Registered only so the bytes stay pairwise-distinct from every live tag.
pub const SETUP_STORAGE_MODE_V1: &[u8] = b"fauna.setup.storage-mode.v1\0";

/// The folder **audience attestation** — the owner's identity-key signature that
/// a folder is `public`, which every seat verifies before it writes that
/// folder's content unsealed (`encryption-at-rest.md` § Readable classes →
/// *The declassification is owner-ATTESTED*). An **actor-key** context, signed
/// by `fauna-protocol` `folders::AudienceAttestation::mint` and verified by
/// `FolderSummary::judge_declassification` — both through the one message
/// builder `folders::audience_attestation_signed_message`, length-prefixed
/// because the body's last element (the folder name) is variable-length.
pub const FOLDER_AUDIENCE_ATTESTATION_V1: &[u8] = b"fauna.folders.audience-attestation.v1\0";

/// **Writer-signed change records** — the writer's signature over a file-sync
/// change record's `SignedChange` statement (`mls-group-key-material.md` § M2 →
/// *Multi-writer* → *Writer-signed change records* (2)). A **device-principal
/// / actor-key** context: signed by the store principal's writer key (or the
/// identity key directly) through `sync_writer_sig::SignedChange::sign`,
/// verified by `sync_writer_sig::verify_statement` — both through the one
/// builder `SignedChange::signed_message` (tag ‖ canonical dag-cbor, a single
/// self-delimiting CBOR item, so no length prefix is needed).
pub const SYNC_CHANGE_WRITER_SIG_V1: &[u8] = b"fauna.sync.change.writer-sig.v1\0";

/// **The owner-signed content-key envelope** — the shared set's owner's
/// signature over the sealed envelope blob the nest stores
/// (`writer-signed-change-records.md` ruling (11)(b): the AEAD is group-held,
/// so without it any member could seal a payload naming a nonce of its
/// choosing). An **actor-key** context: signed by
/// `fauna_client_folders::FoldersAuthor` at every publish and verified at
/// every member ingest, both through the one builder
/// `folder_envelope_sig::signed_message` (tag ‖ channel id ‖ sealed bytes —
/// the channel is fixed-width and the sealed bytes run to the end, so no
/// length prefix is needed).
pub const FOLDER_ENVELOPE_OWNER_SIG_V1: &[u8] = b"fauna.folders.envelope.owner-sig.v1\0";

/// Build the domain-separated message `context ‖ message` to be signed (or
/// verified). The single construction point both the signer and the verifier of
/// a context call, so the two can never drift.
pub fn domain_separated(context: &[u8], message: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(context.len() + message.len());
    m.extend_from_slice(context);
    m.extend_from_slice(message);
    m
}

/// Build the domain-separated **length-prefixed** message for a context whose
/// body has adjacent variable-length elements: every element — the tag included
/// — is encoded as `(len as u64).to_be_bytes() ‖ bytes`, the same injective
/// encoding the push relay's signed messages use (`bins/fauna-push-relay`).
/// No re-split of the byte stream can move bytes between elements, so two
/// distinct element lists can never produce the same signed message. The single
/// construction point for both signer and verifier of such a context.
pub fn domain_separated_length_prefixed(context: &[u8], elements: &[&[u8]]) -> Vec<u8> {
    let mut m =
        Vec::with_capacity(8 + context.len() + elements.iter().map(|e| 8 + e.len()).sum::<usize>());
    m.extend_from_slice(&(context.len() as u64).to_be_bytes());
    m.extend_from_slice(context);
    for e in elements {
        m.extend_from_slice(&(e.len() as u64).to_be_bytes());
        m.extend_from_slice(e);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every registered context — the *applied* tags plus the two *reserved*,
    // retired ones: the KeyBlob tag and the outbox tag. Holding both
    // reservations to the same pairwise-distinct + prefix-free guarantees keeps
    // a future KeyBlob retrofit safe, proves neither reservation collides with
    // an applied context, and keeps both permanently un-reusable.
    const ALL: &[&[u8]] = &[
        CERT_BINDING_V1,
        OUTBOX_FORWARDED_POST_V1,
        FEDERATION_HELLO_V1,
        KEYBLOB_V1,
        DEVICE_HANDSHAKE_V1,
        DEVICE_HANDSHAKE_V2,
        DEVICE_GRANT_REVOKE_V1,
        DEVICE_ADOPT_V1,
        DEVICE_P2P_PARTICIPATION_V1,
        CUSTODY_HANDSHAKE_V1,
        CUSTODY_HANDSHAKE_V2,
        CLAIM_ADMIN_V1,
        ACCOUNT_LOCKOUT_V1,
        NEST_ROTATION_V1,
        AUTH_HANDSHAKE_V1,
        AUTH_HANDSHAKE_V2,
        AUTH_VERIFY_V1,
        AUTH_VERIFY_V2,
        ACCOUNT_REGISTER_V1,
        ACCOUNT_AGE_CLAIM_V1,
        INVITE_SUBMIT_V1,
        INVITE_CANCEL_V1,
        SETUP_NAT_MODE_V1,
        SETUP_NAT_MODE_V2,
        SETUP_STORAGE_MODE_V1,
        FOLDER_AUDIENCE_ATTESTATION_V1,
        SYNC_CHANGE_WRITER_SIG_V1,
        FOLDER_ENVELOPE_OWNER_SIG_V1,
    ];

    /// The tags a live signer actually prepends today.
    const APPLIED: &[&[u8]] = &[
        CERT_BINDING_V1,
        FEDERATION_HELLO_V1,
        DEVICE_HANDSHAKE_V2,
        DEVICE_GRANT_REVOKE_V1,
        DEVICE_P2P_PARTICIPATION_V1,
        CUSTODY_HANDSHAKE_V2,
        CLAIM_ADMIN_V1,
        ACCOUNT_LOCKOUT_V1,
        NEST_ROTATION_V1,
        AUTH_HANDSHAKE_V2,
        AUTH_VERIFY_V2,
        ACCOUNT_REGISTER_V1,
        ACCOUNT_AGE_CLAIM_V1,
        INVITE_SUBMIT_V1,
        INVITE_CANCEL_V1,
        SETUP_NAT_MODE_V2,
        FOLDER_AUDIENCE_ATTESTATION_V1,
        SYNC_CHANGE_WRITER_SIG_V1,
        FOLDER_ENVELOPE_OWNER_SIG_V1,
    ];

    #[test]
    fn tags_are_pairwise_distinct() {
        for (i, a) in ALL.iter().enumerate() {
            for (j, b) in ALL.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "two contexts share a domain tag");
                }
            }
        }
    }

    #[test]
    fn no_tag_is_a_prefix_of_another() {
        // The NUL terminator guarantees prefix-freeness; assert it so a future
        // edit that drops a terminator (re-opening cross-context confusion at the
        // tag boundary) fails loudly.
        for (i, a) in ALL.iter().enumerate() {
            assert_eq!(a.last(), Some(&0u8), "tag {i} is not NUL-terminated");
            for (j, b) in ALL.iter().enumerate() {
                if i != j {
                    assert!(
                        !b.starts_with(a),
                        "tag {i} is a prefix of tag {j} — cross-context confusion"
                    );
                }
            }
        }
    }

    #[test]
    fn keyblob_tag_is_reserved_distinct_and_well_formed() {
        // KEYBLOB_V1 is *reserved, not applied* (the deployment-key KeyBlob
        // signers are retired — see the module docs). Pin that it is
        // nonetheless a well-formed, pairwise-distinct, prefix-free tag, so a
        // future retrofit that ever does apply it inherits the same structural
        // guarantees as the applied A/C tags. (Any NEW context must apply a tag.)
        assert_eq!(
            KEYBLOB_V1.last(),
            Some(&0u8),
            "reserved tag must be NUL-terminated"
        );
        for applied in APPLIED {
            assert_ne!(
                *applied, KEYBLOB_V1,
                "reserved tag collides with an applied tag"
            );
            assert!(!applied.starts_with(KEYBLOB_V1) && !KEYBLOB_V1.starts_with(applied));
        }
    }

    /// The retired outbox context has no signer left, but its tag must stay
    /// distinct from every live one so it can never be silently re-minted for a
    /// different context.
    #[test]
    fn retired_outbox_tag_stays_reserved_and_distinct() {
        assert_eq!(
            OUTBOX_FORWARDED_POST_V1.last(),
            Some(&0u8),
            "retired tag must stay NUL-terminated"
        );
        for applied in APPLIED {
            assert_ne!(*applied, OUTBOX_FORWARDED_POST_V1);
            assert!(
                !applied.starts_with(OUTBOX_FORWARDED_POST_V1)
                    && !OUTBOX_FORWARDED_POST_V1.starts_with(applied)
            );
        }
    }

    /// **every guarantee in this module iterates `ALL`, and
    /// `ALL` is hand-maintained** — so a tag declared here but never added to
    /// the list silently inherits none of them: not distinctness, not
    /// prefix-freeness, not NUL-termination. That matters more here than
    /// almost anywhere, because — the finding this module exists to
    /// close — *was* two contexts producing byte-identical signed messages. A
    /// copy-pasted duplicate tag no list names would reproduce it unchecked.
    ///
    /// So the completeness claim is a **scan of this file's own source**, not a
    /// list: the census pattern this codebase uses wherever a hand-list would
    /// otherwise be the guarantee (`state.rs`'s nest walks, the lesson —
    /// *widen by walking, never by re-listing*).
    #[test]
    fn every_declared_tag_appears_in_the_all_list() {
        let src = include_str!("sig_domain.rs");
        let declared: Vec<&str> = src
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix("pub const "))
            .filter_map(|rest| rest.split_once(": &[u8]"))
            .map(|(name, _)| name)
            .collect();
        // Guard against a vacuous pass: if the declaration shape ever changes,
        // the scan must fail loudly rather than find nothing and pass (the
        // vacuous-pin lesson — a pin that cannot fail is not coverage).
        assert!(
            declared.len() >= 18,
            "the source scan found only {} tag declarations — the `pub const NAME: &[u8]` \
             shape has changed and this guard has gone vacuous; fix the scan, do not delete it",
            declared.len()
        );
        let all_block = src
            .split_once("const ALL: &[&[u8]] = &[")
            .expect("the ALL list's declaration was renamed — fix this guard")
            .1
            .split_once("];")
            .expect("the ALL list is unterminated")
            .0;
        for name in declared {
            assert!(
                all_block.contains(name),
                "sig-domain tag `{name}` is declared but absent from ALL, so it inherits none \
                 of the distinctness / prefix-freeness / NUL-termination guarantees this module \
                 asserts — add it to ALL (and to APPLIED if a live signer prepends it)"
            );
        }
    }

    #[test]
    fn domain_separated_prepends_the_context() {
        let m = domain_separated(CERT_BINDING_V1, b"payload");
        assert!(m.starts_with(CERT_BINDING_V1));
        assert_eq!(&m[CERT_BINDING_V1.len()..], b"payload");
        // A different context yields a different signed message for the same body.
        let other = domain_separated(OUTBOX_FORWARDED_POST_V1, b"payload");
        assert_ne!(m, other);
    }
}
