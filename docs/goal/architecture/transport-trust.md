# Transport trust — authenticating a nest's TLS without a public CA — target state

Owns: channel-binding
Status: ratified — split verbatim out of `security.md` on 2026-10-14; implemented for the WS-RPC client↔nest leg and the HTTP content API on every app (both axes, the cross-connection pin, the disk-backed pin store), the remaining gaps named in § Implementation status today
Authority: **client-side transport trust** — how an app knows the TLS connection it opened reached the real nest and not a man-in-the-middle when the nest has no public CA: the two independent axes (the nest's identity key bound into the channel, and the TOFU/identity pin), the connection-teardown rule, the cross-connection binding that pins the bound SPKI onto the bearer connection, post-auth and pre-claim surfacing of an identity change, and pin custody across processes (one install-scoped store, one interactive minter) — plus this concept's implementation status and each app's durability record. Defers the app-layer crypto discipline (signed envelope, verification recipe, decode-strictness, key management invariants) and the co-resident process trust boundary to [`security.md`](security.md); the channel-binding handshake's *field layout* to [`transport.md`](transport.md) (this doc owns *why* and *what is proven*); server-side cert policy to [`nest/tls-certificates.md`](nest/tls-certificates.md); key taxonomy to [`key-material-hierarchy.md`](key-material-hierarchy.md).

Last verified: 2026-10-14 | Sources: `libs/fauna-anon-client/src/trust.rs`, `libs/fauna-client/src/ws_challenge_bearer.rs`, `bins/fauna-nest/src/self_signed_cert.rs`

Split verbatim out of [`security.md`](security.md) on 2026-10-14. That doc had reached
222,127 B, about ten days from the 262,144 B whole-file read ceiling; its
§ Transport trust and that section's status entries were 144,703 B of it and carried
most of its growth in the week before the split. Transport trust was already a concept of
its own there — the section opened by setting it apart from everything before it as the
orthogonal *transport* question — so it moved whole, with its status.

**Citations.** The section keeps its original heading, `## Transport trust — authenticating a
nest's TLS without a public CA`, every child keeps its `###` heading, and the status entries sit
under `## Implementation status today` as they did, so a `security.md § <child>` citation
resolves here by swapping the filename alone. Unqualified § citations in the moved text that
name a section not found here — § Signed-envelope shape, § Key management invariants,
§ Co-resident process trust boundary, § On-screen secret exposure, § Third-party verification
recipe — resolve in [`security.md`](security.md). The one line edited in the move is the
section's first sentence, whose *above* pointed at the app-layer sections that stayed; it now
names `security.md`.

## Section map

- **§ Transport trust** — the model, then its children: *Two independent axes*; *Connection-teardown rule*; *Cross-connection binding*; *Post-auth surfacing*; *Pre-claim surfacing*; *Pin custody across processes*.
- **§ Implementation status today** — what is built: the WS-RPC leg and the HTTP content API,
  graduation on both legs, the nest half, the client half (Axis 1 + Axis 2 + the
  cross-connection pin), and the per-app durability + web hardening record.

## Transport trust — authenticating a nest's TLS without a public CA

Everything in [`security.md`](security.md) is *app-layer* (sign-over-CID): it proves a payload's
authorship once it's in hand. This section is the orthogonal *transport*
question: when a Fauna **client** opens the WS-RPC TLS connection to a
**nest**, how does it know it reached the real nest and not a
man-in-the-middle — for a self-hosted, LAN, or `.local` nest that has no
publicly-trusted (CA-issued) certificate?

A public-domain nest answers this the boring way: it serves an ACME /
CA-issued cert and WebPKI validates it. The hard case is the
**Pi-on-the-LAN / `.local` / bare-IP** nest, which serves a **self-signed**
cert (`bins/fauna-nest/src/self_signed_cert.rs`,
`fauna.bridges.provision_self_signed_cert`; the nest also auto-self-signs at
bootstrap before ACME can run). The **server-side** policy for *which* cert the
nest serves per connection — the always-live self-signed floor, the tiered
trusted-cert acquisition (HTTP-01 / client-published DNS-01), and how a cert
stays alive and trusted — is owned by
[`nest/tls-certificates.md`](nest/tls-certificates.md); this section is its
**client-side** counterpart (how a client authenticates whatever cert it
receives). The two are additive: the server keeps a cert always live and as
trusted as obtainable, the client pins the *identity* the channel binding
proves. The client *was* all-or-nothing — WebPKI-valid, or the
`FAUNA_INSECURE_TLS=1` escape hatch that accepted *any* cert (**MITM-open**,
dev/dogfood only). That flag is **removed** and the path this section
specifies is **implemented** on every bearer-carrying connection (see
`## Implementation status today`); the prose below is the normative
statement of what now runs.

> **Scope — the *Fauna app* ↔ *nest WS-RPC* leg only.** A generic MUA
> reaching a nest's IMAP/CalDAV listener cannot verify a Fauna identity
> signature and gets the weaker network-trust posture — owner:
> `nest/deployment-home-with-public-relay.md` § MUA reach. The Fauna
> client *can* (it knows the nest's `actor_id`), so it gets the strictly
> stronger guarantee below. The two are additive, not contradictory.

> **A third-party principal's session is outside this section as well (ratified 2026-10-01; built 2026-10-02).** It authenticates the nest by ordinary WebPKI on the issuer's domain — the trust the client already placed when it fetched discovery there; a nest with no claimed domain has no issuer and so no principal sessions at all — and authenticates *itself* with a DPoP-bound access token at the upgrade, never a bearer. Owner: [`transport-connection.md`](transport-connection.md) § Connection lifecycle → *The principal session*.

> **The login signature binds the nest it is addressed to (2026-09-23).** The channel binding below authenticates the *nest to the client*; the complementary direction — that the client's login signature cannot be relayed by the nest it signs into to mint a bearer elsewhere — is owned by [`../behavior/login.md`](../behavior/login.md) § Binding the nest: every `fauna.auth.{handshake,verify,device_handshake,custody_handshake}` signature names the identity `fauna.auth.nest_handshake` proved on the same connection, SPKI-compared on native TLS and pin-checked before signing on web.

### Two independent axes

Trust here is **not** a bind-vs-TOFU either/or. It is two orthogonal axes
that always both apply:

**Axis 1 — Cert ↔ identity binding (the mechanism; always on).** A
**channel binding** that fuses the TLS channel to the nest's Fauna identity,
RFC 5929 `tls-unique` in spirit:

1. The client accepts the self-signed cert **provisionally** — encrypt-only,
   *not yet authenticated* — and records the **SPKI fingerprint** of the cert
   it actually received (`SHA-256` over the cert's
   `SubjectPublicKeyInfo` — the *public-key* fingerprint, **not** the
   whole-cert fingerprint).
2. In the WS-RPC handshake the client sends a fresh random `client_nonce`.
   (The same nonce is also folded into the client's own auth signature —
   `handshake_signed_message(actor_id, timestamp, client_nonce)` — so two
   legitimate same-actor clients signing in the same millisecond don't collide
   on the nest's deterministic-signature replay guard; `login.md` § direct auth,
   the auth-handshake fix. One field, two uses.)
   The nest signs `(SPKI_fp_of_the_cert_the_nest_itself_serves ‖
   client_nonce)` with its `nest_signing_key` (the stable Ed25519
   deployment-identity key, which survives a factory reset so the pinned
   `nest_actor_id` stays stable — custody, reset survival, and off-box
   recovery: `key-material-hierarchy.md` § Roots → Deployment Ed25519
   signing key + `nest/box-recovery.md`) and returns
   `(signature, nest_actor_id)`.
3. The client verifies the signature over
   `(SPKI_fp_of_the_cert_THE_CLIENT_received ‖ client_nonce)` against the
   public key of `nest_actor_id`.

   **Load-bearing subtlety:** the nest signs the SPKI of the cert *it* serves
   (known locally), never an SPKI the client reports. The client checks that
   signature against the SPKI *it* saw. The two SPKIs are equal iff no
   middlebox substituted the cert. A LAN MITM terminates TLS with its own
   consistent cert, so plain TOFU-on-cert is fooled — but the MITM saw a
   *different* SPKI than the nest serves and **cannot forge the nest's
   Ed25519 signature over the SPKI the client saw**. So the substitution is
   exposed exactly when it matters. This is why the binding is *necessary*,
   not optional, even when an out-of-band fingerprint was confirmed.

   **"The cert it serves" is the cert it served *this connection* (ruled
   2026-10-18).** A nest serves different certs to
   different dials — the per-SNI rule of
   [`nest/tls-certificates.md`](nest/tls-certificates.md) § A, and the § B-IP
   bridge every **no-SNI** dial is served for up to 160 h after a provision — so
   the SPKI is scoped to the connection's served name: the server name the
   nest's own TLS layer accepted rides the connection into the handler
   (`served_name.rs` → `dispatch_core::current_served_name`), and **every**
   binding producer — the four bearer-minting handshakes, the verify fold, the
   pre-identity `nest_handshake`, and the federation listener's hello compare —
   signs `served_cert_spki_sha256(name)` for a named dial and the no-SNI cert
   for an IP dial. The name is the nest's TLS state, never a wire field: a
   client-supplied `sni` would have been harmless (the nest holds every cert it
   could be asked to sign) but redundant, and would have made correctness rest
   on a field eight client stacks carry; reading it off the connection keeps
   "never a client-reported value" literally true, costs the wire nothing, and
   covers web (the browser sets SNI from the URL host) and the nest↔nest leg
   alike. The read stays live, as before: a rotation between a long-lived
   connection's TLS handshake and its binding read signs the cert *now* served
   for that name — the window the single-SPKI read already had.

This axis is a pure mechanism — it proves "the encrypted channel I'm on
terminates at the holder of `nest_actor_id`'s key." It says nothing about
whether `nest_actor_id` is the nest I *meant* to reach. That is Axis 2.

**Axis 2 — Identity root of trust (where the *expected* `nest_actor_id`
comes from; differs by deployment).**

| Deployment | Root of trust | First-connect UX |
|---|---|---|
| **Public domain** (`alice@example.com`) | DNS `_fauna.{domain}` TXT `self=<actor_id>` (`libs/fauna-core/src/resolve.rs`; DNSSEC is the ideal hardening). The expected `actor_id` is fully resolved before connecting. | **None** — fully verified, no warning. The login records the verified identity as the host's pin (*The login's pin*, below), so the escrow-holder set is never empty here; a later identity change the zone and a rotation chain do not carry warns like the TOFU row. |
| **LAN IP / `.local`** (`alice@192.168.1.57`, `alice@pi.local`) | **TOFU** — there is no DNS authority to ask. Pin `nest_actor_id` on first successful channel binding; on a *later* change, warn loudly (the SSH `known_hosts` model). | First connect pins silently; optional out-of-band hardening (the Pi prints its fingerprint / a QR at setup → the user confirms it, closing the first-connect trust window). |
| **Client-provisioned box** (first contact, pre-DNS) | the client's own **injected `deployment_seed`** — the client minted+injected the box's identity at provision, so it knows `nest_actor_id` a priori. This is the LAN-IP row with the *optional* out-of-band hardening made **mandatory and automatic** (the client is the out-of-band channel). | **None** — verified against the injected identity from first connect; **no TOFU window**. |
| **Self-hosted public domain, pre-claim** (hand-deployed internet nest — `nest-internet-setup.md`; domainless until the claim names it, so no DNS `self=` exists yet and no seed was injected) | the **console-printed `fauna://claim` URI** — the boot banner prints the claim code beside a URI carrying `nest_actor_id` (`fauna_core::claim_code::claim_uri`; banner `bins/fauna-nest/src/claim.rs`), and pasting the URI makes the onboarding machine hold that identity as the first-contact root for the claim host *before the code is sent* (`wizard_submit_claim_code` → `hold_first_contact_identity`). The console is the out-of-band channel the code already travels on — the LAN-IP row's optional hardening made available here, where it guards a claim-time admin-takeover secret. | **Native apps:** paste the URI → verified from first connect, **no TOFU window**, and every later wizard call to that host self-verifies too — `WsNestApi::core`'s native arm runs `graduate_first_contact` against the held root before any wizard call rides the connection. Type the bare code → **no root** (the pre-fix state, kept working for old consoles/transcription): the claim rides the provisionally-accepted channel — a documented residual, closed for the account's later life by the post-claim DNS `self=` TXT. **Web consults the pin too, but proves less** (built 2026-08-16): the pasted identity is held the same way (`hold_first_contact_identity` is not cfg-gated), and the wasm arm of `WsNestApi::core` now verifies it before any wizard call rides the connection — `fauna_client_core::nest_trust::prove_first_contact_identity_possession` makes the box sign a fresh **client-chosen** nonce as the pasted identity, and a wrong identity, an absent binding, or a refusal of the handshake kind each hard-fail with nothing sent (as every row does since 2026-09-24 — the admin pasted *this* box's identity, so a refusal is hostile-or-broken). **It remains strictly weaker than native, and the difference is structural, not a gap to close later:** a browser exposes no received certificate to WASM, so there is no SPKI to compare the signature against and Axis 1's channel binding cannot be completed — the proof establishes *possession of the key*, not that *this channel* terminates at its holder. Concretely, web now refuses any box that cannot sign as the pasted identity (it previously accepted every box: the pin was stored and never consulted, a silent no-op), while a **compromised or coerced CA** stays exploitable by an attacker who both mints a browser-trusted certificate for the domain **and** relays the handshake to the real nest. Native's SPKI compare defeats that relay; matching it on web needs a received-certificate primitive browsers do not offer. |
| **Federation-granted** (cross-nest byte plane) | the **home nest's `nest_actor_id`, delivered WITH the routing** — a cross-nest reader dials the folder's home nest's HTTPS byte plane directly (chunk/manifest transfers), holding no account there, so it cannot graduate a pin the ordinary way. Whatever carried `home_nest_url` also carries the identity, and the row has **three arms of different provenance — never generalize one onto another** (the *Mechanism* paragraph below splits them): **(shared set)** the inviter's grant — the MLS Welcome relay + every `caller_access` federated read reply stamp `home_nest_actor_id` beside the URL (`../architecture/federation-shared-folders.md` § Cross-nest → *Recipient-side access discovery*; `../behavior/file-sync.md` § Multi-writer shared sets); **(followed public folder)** no grant and no inviter — the follower typed the owner's address, the client derived `home_nest_url` from that domain, and the home nest stamps its own `nest.info` identity on the `fauna.folders.public.fetch` reply, relayed through the follower's own nest (`../behavior/folders.md` § Publicly-synced follow; `../ui/media.md` § Followed public folders), anchored by the domain→key binding TLS gives the relaying nest's dial (`../architecture/federation.md` § Security → *Domain↔key binding rides TLS*) and held against no prior expectation; **(post bytes)** a post homed elsewhere (ruled 2026-10-19) — the reader's own nest stamps the identity it dial-proved for the home it relayed from, `home_nest_actor_id` beside `home_nest_url` on the `fauna.posts.get_remote` reply and on a delivered post's feed item (`feed-read-model.md` § The read model → *A post homed on another nest*; `../behavior/federated-follow.md` § Decision 3), and a room's Welcome carries its home's the same way for the room-attachment leg (`../behavior/conversation-rooms.md` § The home nest → *Attachment bytes*), and the `fauna.profile.get_remote` reply carries it for a relayed profile's pictures (ruled 2026-10-20; `../ui/profile.md` § Where logic lives → *The remote read*): never the home's own word, held against no prior expectation. `IdentityRoot::PreResolved` on all three arms. | **None — no TOFU window on any arm**: the identity arrives before the first byte-plane contact, which verifies against it (a self-signed home works; a WebPKI home is unchanged and the root is belt-and-braces). The follow arm's residual is the absence of any expectation *older than the reply*: an attacker who owns the typed domain's resolution *and* a WebPKI-valid certificate for it (the bar the relaying nest's dial enforces since 2026-09-25 — `../architecture/federation.md` § Peer-auth model → *Discovery trust rule*) owned the whole follow by construction, and beneath the pin that arm has no seal (its content is public by the owner's choice) — CID verification still runs, rooted in the same relayed listing. The post arm's residual is the reader's own nest, which chose the home and relayed the envelope: compromised, it names any post already, so the stamp adds no adversary. |

**The login's pin — a login graduation leaves the pin store naming the identity it verified, whatever root proved it (ruled 2026-10-05; built the same day).** The pin is not only the TOFU row's record: it is *this machine's statement of which nest stands behind an authority*, and the escrow-holder set the account runtime trusts is defined as exactly that pin ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *A holder change re-receipts and never mints*, clause (1): "the one identity pinned for the bound nest … never the nest's own claim about itself"). So the rule is one sentence with three consequences, each closing a path on which a login verified the nest's identity and recorded nothing — which left every GenerationTip write (`fauna.state.mail`, `.backup`, `.folder-keys`, …) refused for good with *the escrow receipt is signed by a holder this account does not trust*, on exactly the deployments the project ships (a public-CA cert plus a published `self=`), for every user who signed in without having CLAIMED the box through onboarding (the claim ceremony's own seed, `seed_claimed_identity_pin`, hid the gap from every admin). **(1) A binding already in hand is never waived.** The WebPKI waiver (`trust::webpki_waives_binding`, the one decision point) answers "may a public-CA nest go *unbound*?" — the pre-identity first contact's question, asked before spending a round trip on `fauna.auth.nest_handshake`. A login never asks it: `read_login_binding` demanded a binding before anything was signed ([`../behavior/login.md`](../behavior/login.md) § Binding the nest), and every TLS nest's bearer-mint reply carries one. WebPKI says the client reached the name it dialed; the binding says *which nest* that is, and the stronger statement is kept — the full core runs (SPKI compare, the DNS `self=` root where the zone publishes one, else TOFU), the bound SPKI is pinned and re-pinned at every graduation, which is the mechanism a claimed § B-IP box already rides and what carries the app across an ACME rotation. **(2) The pre-resolved rows record the root they verified as the pin, authoritatively** (`trust::record_login_identity`, run by the two login wrappers after the core): a root outranks any TOFU pin, the same reasoning as the claim seed, and on the public-domain row "the client accepts whatever head the zone names" ([`nest/box-recovery.md`](nest/box-recovery.md) § Client acceptance) — so a pin that differs from a verified DNS root is replaced, not warned about; recording the identity the pin already names is a no-op, so an accepted rotation's seq survives every login. The core itself still never TOFU-pins a pre-resolved root. **(3) The scope is the LOGIN — the bearer-mint handshake and the silent-challenge launch wrappers — and nothing else.** `graduate_first_contact`'s callers record nothing new: the wizard's claim ceremony seeds its own pin; the byte-plane's grant-carried root stays on its own row (the federation-granted row's arms are fenced — *never generalize one onto the other* — and a durable pin for a home nest would be exactly that generalization; the post arm, ruled 2026-10-19, rides the same per-process bound-SPKI map and records nothing in the pin store either — a home nest is never a login pin); the consumer's never-minting heal and a read-only store stay never. **What this is, and is not.** It is native matching web: the SPA has always pinned the possession-proved identity on first contact over any origin (`check_web_nest_identity`, the NT-1 analysis below) and read its escrow trust from that pin. It is not a weakening: a pin adds the binding requirement, the SPKI pin and change detection *on top of* WebPKI, never in place of it; the first-connect UX on a public-CA nest stays silent. The one visible change is the public-domain row's "no pin": a box rebuilt with a new identity that neither the zone's `self=` nor a committed rotation chain carries now warns on native as it always has on web — which is the generation machinery's holder-change signal (clause (1): *trust follows the pin, every pass*), not a regression. Standalone e2e never saw the gap because its plaintext rig is seeded (`e2e-gated-seeds.md` § The e2e trust seed); live mode deliberately is not, which is how the first tui sweep against a real public-CA box exposed it. The standalone witness of rule (1) rides the nest-leg trust seed (`e2e-gated-seeds.md` § The nest-leg trust seed), since no rig can mint a publicly-chained certificate for a loopback or LAN address: `test_nest_identity_pin_webpki.py` (tui), red first against the waiver. Witnesses: `trust::tests::{a_webpki_login_with_a_binding_in_hand_pins_the_identity_it_proved, a_login_records_the_pre_resolved_root_it_verified_as_the_pin, both_login_wrappers_pin_a_never_claimed_public_ca_nest}` (red first against the waiver), beside the unchanged waiver table and the § B-IP bridged-box test.

**Mechanism for the client-provisioned row — the pre-identity nest-identity
handshake `fauna.auth.nest_handshake`.** The bearer handshakes can't serve this
window (`fauna.auth.handshake` requires a *registered* actor; nothing is
registered before the claim), so the nest proves its identity on demand,
pre-identity: the request carries a fresh `client_nonce`; the reply carries the
same Axis-1 `cert_binding` shape as `HandshakeReply` — the nest signs
`served_SPKI ‖ client_nonce` with `nest_signing_key` (no actor, no DB;
replay-moot since each signature is nonce-unique and nothing mutates). The
onboarding machine holds the injected seed's **derived public identity** for the
provisioned domain (`hold_first_contact_root`; the seed itself stays under
box-recovery custody rules), persists that public identity — never the seed —
beside the claim code in the pending-provision slot so a Retry or a relaunch
re-holds the same root for the same box (`../behavior/onboarding.md` § 6 *The
pending-provision slot*), and runs the handshake as the **opening step of
every fresh pre-identity connection** to that host — probe, claim,
NAT-mode, mail-enable each self-verify (the wizard opens a fresh connection
per call, so a graduate-once model would leave the later calls blind). The
verification core is shared with the bearer path
(`graduate_handshake_with_root`; `IdentityRoot::PreResolved` covers both DNS
`self=` and the injected seed). A mismatch on a box we just provisioned is
MITM/bug, never benign → hard-fail per § Connection-teardown rule — **and that
hard-fail includes a nest that *refuses the kind or withholds the binding***:
with an injected root held the client knows the box it just provisioned (from a
current image) answers the handshake, so a rejection is a first-contact MITM's
cheapest move (terminate TLS, refuse the kind, ride the downgrade), not a benign
old nest (safe
failure direction — re-provisioning from the current `:latest` clears it). The
"no TOFU window" cell above is therefore unconditional on this row — **and since
2026-09-24 on every row: a kind-rejection hard-fails everywhere.** Past the
WebPKI waiver the binding is demanded outright, and a box that will not prove
its identity is not spoken to (`trust::TrustError::BindingRequired`, nothing
sent). The pre-Track-2 compat fallback that once let a rejection on the pin-less
DNS/TOFU ladder — and on a pinned host on the *self-signed floor* — proceed
ungraduated (design + A/B analysis ratified 2026-07-07; the "held covers a root
*or* a pin" tightening ruled 2026-09-02) was a backwards-compatibility remnant
for nests predating `fauna.auth.nest_handshake` (2026-07-07), and the
compat-remnant sweep removed it in place with no accept path
([`version-compatibility.md`](version-compatibility.md) § Dimension 2, the
fourth user-ruled write-off). With it went the residual that fallback carried
(an active MITM could mimic the rejection) and the *open population question* it
raised (a nest that minted a TOFU pin over the bearer handshake but predated the
kind, and had since acquired a WebPKI-valid cert — a population the user confirmed
empty on 2026-09-26, no such alpha nest existing): every nest a current client
meets answers the kind, and a box that does not is updated, not tolerated. The
failure direction is safe (refuse, nothing sent).

**Mechanism for the federation-granted row — the cross-nest byte plane.** A
cross-nest reader's byte plane (direct HTTPS chunk/manifest transfers to the
folder's **home** nest) has no other trust root: the reader holds no account on
the home nest, so no bearer handshake runs there, and the control plane is
relayed through the reader's own nest, not dialed. The same pre-identity
`fauna.auth.nest_handshake` fills the gap — `graduate_first_contact` runs it
against the home nest with `IdentityRoot::PreResolved(home_nest_actor_id)` (the
identity the record carries — whose provenance differs per arm, below) and pins
the bound SPKI, which the byte-plane reqwest
client (`store_pinned_reqwest_tls`, read **per-handshake**) then accepts. Run
**before** the byte plane is dialed, by one shared body —
`fauna_client::graduate_home_nest_pin` — from both native byte-plane dials:
`fauna-sync-agent`'s bind (`engine_driver`), and every app's Media download of a
foreign set or a **followed public folder** homed elsewhere
(`fauna_client::ForeignPublicChunkFetcher::for_home`, fed the record's
`home_nest_actor_id` through the media machine's foreign-fetcher factory —
built 2026-09-22; before that the Media dial was plain WebPKI and a self-signed
home was simply refused). `https` homes only — a plain-`http` loopback has no
TLS to pin. **Refusal after a root was delivered is a hard-fail
→ the folder is left inert (fail-closed + loud), retried on the next bind**: a
home nest serving cross-nest sets postdates Track 2 and answers the kind, so a
rejection is the MITM's cheapest move, not a benign old nest — and a WebPKI home
graduates by short-circuit, so a refusal is never a benign-old-nest case here.
**Absent** `home_nest_actor_id` (a relay-unaware home) skips graduation and keeps
the `RequireWebPki` floor — never weaker (a WebPKI home works, a self-signed one
fails loud at TLS). **The root's provenance and what sits beneath the pin differ
per arm — a sentence true of one arm is not licensed on the other.** *Shared
set:* the root is the **grant-delivered** identity (Welcome relay /
`caller_access` reply). Why it may travel the (own-nest-mediated) grant relay
rather than inviter-authenticated E2E material: the member's own nest is
**already routing-trusted** (it controls `home_nest_url` today), so relaying adds
no adversary; and content stays sealed E2E (the content key came over MLS) +
CID-verified regardless, so the pin is a second line behind the seal. A
strictly-separable future hardening moves `home_nest_url` + `home_nest_actor_id`
inside the MLS Welcome payload — both fields or neither. (Design ratified
2026-07-23.) *Followed public folder:* there is no grant and no inviter. The
root is the home nest's own `nest.info` identity, **stamped by the home nest on
the `fauna.folders.public.fetch` reply** and relayed by the follower's
already-routing-trusted own nest, which resolved that identity over
authenticated TLS to the domain the follower typed — anchored by the domain→key
binding TLS provides (`federation.md` § Security → *Domain↔key binding rides
TLS*, enforced at that dial by § Peer-auth model → *Discovery trust rule* since
2026-09-25: a handle domain is a request-named global target, so neither of the
rule's carve-outs reaches this arm) and held against **no prior expectation**
(`federation_pool::originate`'s
own caveat: the dial proves the peer holds the key it advertises, not that it is
the peer the caller intended). That missing older expectation is the honest
residual, not a missing anchor — the arm is not the LAN-IP row's TOFU, since a
CA's domain→key binding, not blind first contact, is what the stamp rests on.
And beneath the pin there is **no seal**: the owner's own `audience=public`
choice rests the content unsealed (`../principles.md` § The user always
controls their data), and the followed read is structurally keyless
(`../ui/media.md` § Followed public folders). CID verification still runs
(manifest against the address asked for, file bytes against the manifest's
hash), but its root is the same relayed listing — it defends against byte-plane
substitution, not against a lying home nest. So on the follow arm the graduated
pin is the byte plane's only transport line, with no seal beneath it to fall
back on; a session reasoning about what a compromised home nest can do to a
follower starts from that, not from the shared-set arm's sentences.

**The post arm — a post's own bytes from its home (ruled 2026-10-19; the relay arm, the delivered-post and room carriers and
the room-attachment read BUILT natively 2026-10-19 — § Implementation status
today).** A post homed
on another nest reaches a reader as an envelope — relayed by the reader's own
nest through `fauna.posts.get_remote`, or delivered to it by a federated follow
— and its sealed body, media and thumbnails are fetched by the app straight from
the home's public `GET /api/v1/blob/{hash}` (`feed-read-model.md` § The read model →
*A post homed on another nest* owns the legs; `../behavior/federated-follow.md`
§ Decision 3 the delivered case; the room-attachment read of
`../behavior/conversation-rooms.md` § The home nest → *Attachment bytes* is the
same leg one rail over). That plane is this row's: the reader holds no account
on the home, so no bearer handshake graduates a pin there, and a home serving
the self-signed floor (every LAN, loopback or domainless box — and every
two-nest e2e nest) is refused by WebPKI alone. So `fauna_client::post_blob`'s
foreign plane is the pin-or-WebPKI reqwest plane every other native byte leg
dials through (`pinned_http_client` → `store_pinned_reqwest_tls`, the pin read
per handshake, `RequireWebPki` beneath it), and before the first byte to a home
whose identity the record carries it runs `graduate_home_nest_pin` once per home
per process — the one shared body, with the lazy once-per-home graduation
`ForeignPublicChunkFetcher` holds lifted into one shared piece the chunk-store
fetcher, the post plane and the room-attachment read all use (priority #2), so
no leg can be the one that forgot it. **The root's provenance is the arm's own:
the reader's own nest stamps the identity it dial-proved for the home.** The
relaying nest originated the read on a channel it dialed *as* a `peer_nest_id` —
`federation_pool::originate` resolves it (`resolve_peer_nest_id`, the resolution
the Welcome relay already records a foreign member's home by) and the federation
hello binds the channel to it (Axis 1 at the nest↔nest dial; `federation.md` §
Peer-auth model) — so `posts_get_remote_handler` stamps that identity on the
reply as `home_nest_actor_id` beside `home_nest_url`; a delivered post's feed
item carries the delivery's verified `origin_nest_id` the same way; and the
receiving nest of a cross-nest Welcome stamps the verified origin it resolved
the channel's home URL from — the relaying nest IS the channel's home — for a
room's attachment leg and a folder's chunk plane alike, never the peer's own
`home_nest_actor_id` field. `libs/fauna-feed` folds the field onto
`PostSummary.home_nest_actor_id`, and `blob_home` names the host and its
identity together, so no app re-derives either. It is never the home's own
`nest.info` word relayed (the follow arm's stamp), and it is stronger than that:
the relaying nest's *verified* knowledge of the channel it used. Why the
reader's own nest may stand for it: that nest is already routing-trusted — it
chose which home a post resolves from and relays the envelope itself — so a
compromised own nest names any post and any home already, and the stamp adds no
adversary; held, like the follow arm, against **no prior expectation** (the
residual is the reader's own nest, not a missing anchor). **The room-attachment
upload rides the same plane on the same stamp (ruled 2026-10-19):** the write leg's `POST /api/v1/blob` to the home
carries a short-lived, write-only token the member's own nest relayed from that
home — the same routing-trusted nest that stamped the identity, so neither the
token nor the stamp adds an adversary — and graduating the pin before the token
leaves only narrows which endpoint may receive it; beneath the pin the bytes are
sealed under the room's key and content-addressed, the shared-set posture, so a
refusal fails the send loudly, the existing posture for a failed upload, never a
retry on the floor. **A relayed profile's pictures ride the same plane on the
same stamp (ruled 2026-10-20):** a profile
relayed through `fauna.profile.get_remote` (`../ui/profile.md` § Where logic
lives → *The remote read*) names its `avatar` and `banner` by content hash,
and the app fetches them from the home's public blob route exactly as a
relayed post's media; so the carrier is the relay's reply — `home_nest_actor_id`
beside the home the read was relayed to, stamped from the same
`resolve_peer_nest_id` the post relay stamps from, absent on a same-nest
`fauna.profile.get`, folded by `fauna_client_profile::HeaderRead` beside
`relayed_from` so no app re-derives it. This is the post arm's own provenance
— the reader's own nest's verified knowledge of the channel it relayed over,
the same routing-trusted nest, the same residual — and therefore NOT a fourth
arm: the fence between the arms is a fence between *provenances* (an inviter's
grant, a home's own word relayed, a relaying nest's dial-proof), and a second
carrier of one provenance generalizes nothing; the room-attachment leg above is
the precedent. The actor-signed `Profile.nests[0].nest_id` was weighed and is
not the anchor, for four reasons: it is the actor's *routing* word — where
they live (`../ui/profile.md` § Where logic lives → *The actor's home*) — not a
transport proof; it is empty on every profile published before a registration
mirrored it and on every nest-side editor's base, so most relayed profiles
would stay on the floor; it is a claim about a key the actor does not control,
which may hold stale across a home's identity rotation
([`nest/box-recovery.md`](nest/box-recovery.md)) and would then fail the plane
closed against a legitimate home until the actor re-publishes; and it names
`nests[0].url`, which the relay did not necessarily dial — `relayed_from` is
the open target's carried home — so pinning it onto that dial would bind one
URL's identity to another. A third provenance would also be a genuine fourth
arm with sentences of its own, for a weaker availability story, when the
dial-proved stamp is already in hand on the very reply that carried the bytes.
`nests[0].nest_id` stays routing: never a transport anchor, and never a
cross-check either — the arm holds against no prior expectation, and a stale
profile must not empty a header. Beneath the pin the pictures take the public
post's posture below: unsealed by the owner's choice (`audience = Everyone` is
the opt-in), CID-verified against a hash the actor's own verified signature
names, the pin the plane's only transport line. The floor and the refusal are
the arm's: no stamp keeps `RequireWebPki`; a refusal paints the element empty
and never `error-message` (`../ui/profile.md` § Errors & edge cases). **Beneath
the pin the legs differ, and a session reasons per leg:** a gated body is sealed end to
end (a room post under the room's key the device holds, a broadcast-gated one
under the `KeyBlob` read through the follower's own nest) and CID-verified — the
shared-set arm's posture, the pin a second line behind the seal; a public post's
media is unsealed by the author's choice and CID-verified — the follow arm's
posture, the pin the byte plane's only transport line, CID verification
defending against substitution and not against a lying home. **The fence holds
by construction** (*The login's pin*, rule (3)): the graduation writes the
per-process bound-SPKI map `store_pinned_reqwest_tls` reads and nothing else —
`graduate_first_contact` never TOFU-pins a pre-resolved root and
`record_login_identity` never runs on this plane — so a home nest never becomes
a login pin, and neither sibling arm's sentences are licensed on this one. **The
floor is the floor:** absent `home_nest_actor_id` (a relay-unaware home; a post
reached by a path with no carrier) the plane keeps `RequireWebPki` — a WebPKI
home works, a self-signed one fails loud at TLS, never weaker; a plain-`http`
home has no TLS to pin and skips graduation, as the sync agent's bind does. **A
refusal after a root was delivered is fail-closed and quiet on the card:** the
byte leg fetches nothing and the post stays teased — the app's existing posture
for a byte leg that fails, since the reader may simply not be entitled — never a
retry on the floor. **What this rules out.** Not the nest-leg trust seed
(`e2e-gated-seeds.md` § The nest-leg trust seed) as the plane's answer: the seed
widens which roots the harness trusts, so it would make a two-nest journey green
while a self-signed home stayed refused in production, and it would need its
term 4 widened and the founder's login pin recorded against a harness CA; the
seed stays on its one journey, and a two-nest post journey needs no harness-CA
leaf and no seed — `home` serves its floor and the reading seat graduates
against the stamp, which IS the production self-signed-home branch (a fetch with
the stamp withheld fails at TLS, the floor). Not content addressing alone: the
blake3 check is integrity, already run on every leg; transport authentication is
what keeps an unauthenticated plane from showing a path adversary which sealed
blobs a reader opens and from withholding them unseen, and *never weaker than
the floor* is this doc's standing rule, not a per-plane choice.

**The browser's leg — the SPA reads the home's blob route direct over the
open-CORS byte router, WebPKI only, and no relay through the reader's nest
(ruled 2026-10-21; BUILT 2026-10-21 — §
Implementation status today).** Web is the one app
with no plane, and the first reason is structural, not a trust one:
`GET /api/v1/blob/{hash}` sits on the nest's main router under the credentialed
allowlist CORS layer, so a browser on nest A's app origin is refused by every
foreign home B, WebPKI or not. **The ruling: the blob GET (and its HEAD) joins
the open, credential-less `public_bytes` router** — `Access-Control-Allow-Origin:
*`, GET/HEAD only, `Authorization` admitted, `x-c2pa` exposed — beside the
chunk, manifest, share and segment reads, because it is the same class of route
and the 2026-07-18 S6 analysis that opened that router
(`federation-shared-folders.md` § Cross-nest shared folders + channel append)
already rules the class: unauthenticated by possession (a content hash is the
capability; `blob_routes::download_blob` takes no bearer), content-addressed,
and read cross-origin by a web app on an origin the home's allowlist cannot
know. Open CORS adds no exposure a non-browser client lacked, and nothing the
own-nest reads rest on is lost: no own-nest blob read carries a cookie — the
SPA's bearer is a script-held `Authorization` header, which the open layer
admits and the browser preflights, the posture the bearer-gated segment route
already rides on that router (a script-held bearer is no ambient authority) —
and the `?thumb=1` lookup and the `Range` answer are the handler's, unchanged.
The `PUT` on the same path (`put_blob_by_cid`, a nest-side caller) rides along:
CORS binds browsers only, and no browser PUTs by CID. The wasm face then
mirrors the UniFFI one — the same cap (`INLINE_BLOB_BODY_LIMIT`), the same
digest check (`content_address_matches`), no bearer to a foreign home, the home
read from `blob_home` — with the one difference the browser imposes: **no
pin.** A browser's fetch is WebPKI only: it hands WASM no certificate, so there
is no SPKI to graduate against (the *Self-hosted public domain, pre-claim*
row's web column above — "structural, not a gap to close later"), and
`home_nest_actor_id` is carried but cannot be used. **The browser residual,
declared and not cured: a home serving the self-signed floor (a LAN, loopback,
bare-IP or domainless box) is unreachable from the SPA, and its post stays
teased on web** — the fail-closed-and-quiet posture every refused byte leg
already takes — while a WebPKI home's post reads in full. **Rejected: a relay
through the reader's own nest** (a nest route fetching `home/blob/{hash}` over
a nest-side pinned dial and streaming it back), alone or as the self-signed
half of a hybrid. It would reach a self-signed home for web, and that is all
it buys. Against it: it contradicts three standing rulings at once — bulk
bytes never ride the nest↔nest leg (`federation.md` § Federation residue
surface), the reader's nest holds none of a foreign post's bytes
(`feed-read-model.md` § The read model → *A post homed on another nest*) and
caches none (`../behavior/federated-follow.md` § Decision 3: a cache is a
second copy the owner cannot delete at the home); it opens a fetch-on-behalf
surface whose bounds would need ruling anew — an SSRF classification toward a
caller-named host, an amplification bound, a bandwidth bill every foreign read
puts on the reader's nest — beside the two the nest already has for *public
web* targets only (the link-preview fetcher and the media proxy over
`ssrf_safe_https_client`: WebPKI, unpinned); it needs a nest-side
pin-graduated HTTPS plane the nest does not have (`graduate_first_contact` runs
on clients; the nest reaches a peer over the federation hello, a WS channel,
never a pinned HTTP GET); and the residual it cures is one the six native apps
already cover on every self-signed home. Revisit only on a measured population
of web readers whose followed authors live on self-signed boxes, and then as a
hybrid that leaves this leg as it is. **The witness splits the mile:** the web
two-nest journey runs with `ignore_https_errors` on every browser context (the
harness's standing web setting, which every `wss://` to an e2e nest needs —
not a plane answer, and not the nest-leg trust seed the paragraph above rules
out), so it witnesses the router move, the face, the cap and the digest check
against a home serving the floor; the self-signed residual is the browser's
own behaviour with no Fauna code in its path, declared here and in the status
bullet rather than tested.

**Key principle: pin the *identity* (`nest_actor_id`), never the cert.**
Certs rotate (90-day self-signed expiry, reinstall, ACME self-heal), so
raw-cert pinning would false-alarm on every benign rotation. The
per-connection channel-binding signature (Axis 1) re-binds *whatever cert is
current* to the stable identity each time, so the only thing worth pinning
is the identity the binding proves. "Bind cert→Ed25519" is the always-on
mechanism; "DNS / TOFU / out-of-band" is the per-deployment identity root.
A *deliberate* change of the pinned identity is the deployment-seed rotation
ceremony — a pinned client re-pins on a verified rotation chain instead of
surfacing the identity-changed warning, and refuses superseded ancestors
afterward; owner: `nest/box-recovery.md` § Deployment-seed rotation (ratified
2026-08-11; client acceptance BUILT 2026-08-12 in the shared
`fauna_client_core::nest_trust` core — an updated client re-pins silently; a
pre-acceptance client still surfaces the warning, the § Version skew arm).

**The mail/CalDAV serving path honors the LAN-IP row too.** A domainless / bare-IP nest serves IMAP + CalDAV at the same locator a Fauna app uses; Fauna apps authenticate that endpoint by Axis 1 exactly as above (address/CA-independent), while a third-party MUA — which cannot do channel binding — falls back to plain TOFU-on-cert with the out-of-band fingerprint as optional hardening. Owner: `../behavior/caldav-server.md` § Authentication + § Network exposure.

### Connection-teardown rule

TLS completes **before** the WS-RPC handshake, so the cert is accepted only
*provisionally* and is *retroactively* authenticated by the in-band
signature. If channel-binding verification fails — bad signature, wrong
`actor_id` vs. the DNS/TOFU root, or a TOFU pin-change the user rejects — the
client **tears the connection down immediately** and never sends the bearer
or any request over it. A provisional connection that fails to graduate is
indistinguishable from an attacker's connection and is treated as one.

**Tearing down is dropping the client, so dropping the client must actually
close the socket.** The rule is carried out by releasing the connection object
(`AnonymousNestClient`), which is the only teardown affordance the pre-identity
path has. That makes the type's `Drop` load-bearing for this rule rather than
mere hygiene: it aborts the task driving the connection, because the driver owns
the whole adapter and **dropping a `JoinHandle` detaches its task instead of
aborting it**. While that abort was missing the rule was silently a no-op — a
refused connection stayed open and `ESTABLISHED` for the life of the process,
and since every bearer mint opens one such client per refresh, a client
reconnecting in a loop accumulated one unreleased socket per attempt until the
host could open no new outbound connection at all. Pinned by
`libs/fauna-anon-client/tests/connection_teardown.rs`.

### Cross-connection binding — pinning the bound SPKI onto the bearer connection

Axis 1 verifies the binding on the connection that carried
`fauna.auth.handshake`. But a Fauna app's **bearer** rides a *different* TLS
connection — the authenticated `GET /api/v1/ws/{actor_id}` (the bearer is a
subprotocol header at connect time), and historically a separate HTTPS
`POST /auth/token`. Binding connection *A* while sending the bearer over
connection *B* would leave *B* MITM-able: an attacker who passes the
pre-identity handshake through cleanly but intercepts the authenticated WS would
capture the bearer. **The binding must therefore extend to every connection
that carries the bearer.** It does so by *pinning the SPKI it just authenticated*:

1. A successful handshake binding yields a trusted pair `(nest_actor_id,
   bound_spki)` — the channel binding proved `bound_spki` belongs to
   `nest_actor_id`, and Axis 2 proved `nest_actor_id` is the nest meant.
2. The client **pins `bound_spki`** and requires every bearer-carrying TLS
   connection to that host to present a leaf cert whose SPKI equals
   `bound_spki`. This is sound *on its own*, without a second signature: TLS
   requires the server to prove possession of the cert's private key, so a MITM
   **cannot complete a handshake as a cert whose SPKI it doesn't hold the key
   for**. The per-connection signed binding's role is purely to *bootstrap*
   which SPKI to pin (via DNS or TOFU); ordinary SPKI-pinning secures the rest.
3. **Cert rotation** (90-day self-signed expiry, ACME self-heal) changes the
   served SPKI, so a pinned-SPKI mismatch is *expected and benign*. On mismatch
   the client re-runs `fauna.auth.handshake` to re-bind; if the freshly-bound
   `nest_actor_id` still matches the pin, it accepts the new SPKI and re-pins.
   This is why the durable pin is the **identity** (`nest_actor_id`), never the
   cert (the key principle above): the SPKI pin is a per-session cache the
   binding refreshes, the identity pin is permanent.

A direct consequence: for a self-hosted-trust nest the bearer is obtained over
the **WS `fauna.auth.handshake`** (the binding-verified path that mints the
token), not a separate unauthenticated HTTP `POST /auth/token`. That legacy
HTTP token-fetch — exactly what `FAUNA_INSECURE_TLS` made accept-any — has been
**deleted** (the last `deprecated_http` control-plane twin, dropped at the
WS-RPC-everywhere endgame), so bearer acquisition now rides the bound WS
connection as its sole path: both the secure shape and the realized
`docs/goal/architecture/transport.md` "WS-RPC everywhere" end-state.

**The dialers — one implementation, every bearer-carrying WS (2026-08-02).**
This policy is code exactly once — `fauna_anon_client::tls_dial` (graduated-pin
lookup → SPKI-pinned or strict-WebPKI dial, plus the § Pin custody
graduate-and-retry arm) — and every bearer-carrying native WS dial consumes it:
the WS-RPC bearer channel (`fauna-client::ws_adapter`), the leaf-crate bearer
connect (`fauna-anon-client`), and — until the daemon that dialed it was
removed 2026-10-02 — the **sync `/sync/ws` data plane** (`bins/fauna-sync`;
the route itself was removed the same day). The sync plane is named deliberately: it shipped for weeks
as a fourth hand-rolled dial that never adopted the policy (plain strict
WebPKI), so a fresh nest serving the self-signed floor was undialable by the
headless daemon — first-boot LAN file sync broken exactly when a new user first
tries it. The residual HTTP legs share the same policy via
`store_pinned_reqwest_tls`. A new bearer-carrying connection must consume
`tls_dial`, never re-derive the policy inline. A WebPKI-valid graduation that
holds **no root** also **clears** any SPKI a prior self-signed graduation
cached for that host — the floor→ACME rotation otherwise strands a long-lived
process on a stale pin no re-bind refreshes (rotation rule 3 above; the pin is
a session cache, and a WebPKI cert with no root needs none). Once a root *is*
held the clearing never runs: the graduation verifies the binding in full and
**re-pins** the served SPKI, which serves the same rotation — and the
[`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP bridge cert's
~4-day rotation — without ever downgrading a pinned host.

**No session resumption on a pinned config (ruled 2026-10-08; built the same day).** Rule 2 above says every bearer-carrying connection presents the pinned SPKI, and the reqwest legs read the pin *per handshake* — but a TLS 1.3 session resumed from a cached ticket runs no certificate verifier at all, so with rustls' default client-side session cache "per handshake" silently meant "per *full* handshake": a process holding a ticket from a host's OLD identity could keep resuming to that box on every reqwest leg (the content API, chunk fetch, the ranged reader) for the ticket's lifetime after the user approved a re-pin to a NEW identity (a replaced box — or a compromise the admin answered by replacing it), and a `forget_identity_pin` re-trust would not refuse the un-graduated self-signed host it exists to refuse until the cache aged out. The accept-it reading — the resumed session was itself verified under the pin of its day, and the server proves PSK possession — was weighed and refused: the pin store is this machine's statement of which nest stands behind an authority *now* (*The login's pin*, above), the re-pin is the one user act that changes it, a connection reaching the previous holder after it is exactly the connection the pin exists to refuse, and a ticket lifetime (RFC 8446 allows seven days) is not a window the user approved. Flushing the session store on re-pin was refused too: a second mechanism every pin writer across three crates must remember to call, and it would leave the capturing configs — whose connect paths read the captured SPKI *after* the handshake, which a resumed session never writes — correct only by the accident that each dial builds a fresh config. So the ruling makes the state unrepresentable: **every config `tls_verify::config_with` builds — capturing, SPKI-pinned and dynamically pinned alike — disables client session resumption (`ClientConfig::resumption = Resumption::disabled()`)**, so every connection is a full handshake, the verifier is the only path to a connection, and *per-handshake* means *per connection*. The cost is one cert parse and one signature verification per new TCP connection, behind reqwest's connection pool (TLS 1.3 resumption saves no round trip without early data, which these configs never enable); on a `Connection: close` server the ranged reader pays it per read, beside the TCP and TLS round trips it already pays. Witness: `libs/fauna-client/tests/reqwest_pinned_tls.rs::a_re_pin_bites_on_the_next_request_even_when_the_server_offers_resumption` against the shared fixture with resumption ENABLED (`common::ServerSessions::Resumable`), red first under the default cache; the fixture's default rig stays full-handshake-only so the other trust assertions observe the verifier alone.

**The native-player leg — a native app's media player never dials the nest (ruled 2026-10-08, a design pass; the shared reader BUILT 2026-10-08, windows' adapter 2026-10-09, linux' and android's 2026-10-10, apple's UNBUILT — `## Implementation status today`).** Every leg above is a client the app builds, which is why the policy reaches it: `tls_dial` for a WS, `store_pinned_reqwest_tls` for a reqwest leg, and the three platform HTTP legs through their `NestCertTrust` shims over the shared FFI classifiers. A platform media player handed a URL — windows' `MediaPlayer`, `AVPlayer` on macos + ios, android's framework `MediaPlayer` / `VideoView`, GTK's GStreamer-backed `gtk::Video` on linux — is none of these: it dials through the OS media stack's own HTTP client, which validates against the OS trust store alone. No WebPKI waiver the app decided, no loopback carve-out, no SPKI pin reaches it, so it refuses the always-live self-signed floor ([`nest/tls-certificates.md`](nest/tls-certificates.md) § The two-layer model) that every nest serves before ACME succeeds, that a LAN / `.local` / bare-IP nest serves for good — **and that the same-box install serves on `https://127.0.0.1:3000`** ([`installers/windows.md`](installers/windows.md)): the OS stack knows nothing of the loopback carve-out either, so the failure is not a remote-nest edge case but the windows default. The rule, for every native app: **a nest URL is handed to no OS component — not a media player, not an image loader, not a download manager. The component reads from a byte source the app owns, and that source is fed by the one pinned leg: the shared-Rust ranged reader (`fauna_client::RangedNestReader` over `pinned_http_client`, `Range` GETs only, UniFFI-faced for windows / apple / android and consumed directly on linux).** Where it stands in this section's model: it is a reqwest leg, so it is `store_pinned_reqwest_tls`'s — the pin is armed whenever a feed exists (*The login's pin*, above: the login graduated it), and `fauna_ffi` is statically linked into every native app, so the reader reads the same process-global store the WS handshake wrote. It carries no bearer and needs no session (the blob route is unauthenticated by design, the proxy's playback ticket rides the query — [`render-model.md`](render-model.md) § D6c → *Inline playback* answer (4)), which is also why nothing kept it platform-side the way the bearer-carrying content API legs are: the reader is a pure function of the nest URL and the pin store. What the player is, how the app wraps the reader into each platform's byte-source seam, and the `Sealed` arm (unchanged — whole blob through the app's existing pinned fetch) are [`render-model.md`](render-model.md) § D6c → *Inline playback* answer (1)'s. Three alternatives were rejected, each a fifth hand-rolled trust path of exactly the sync-plane kind the paragraph above records: **installing the floor cert into the OS trust store** — a per-install ceremony the user would perform against a 90-day-rotating key (configuration theatre under [`../principles.md`](../principles.md) § One configuration surface; on ios and android a visible profile install; and a system-wide trust grant for a per-app need); **the platforms' static pin manifests** (`NSPinnedDomains`, android's network-security-config `<pin-set>`, a `HttpBaseProtocolFilter` on windows — which only the HLS / DASH `AdaptiveMediaSource` accepts) — a pin here is graduated at runtime per nest and never known at build time; **a loopback relay the player dials** (`http://127.0.0.1:<port>` → the pinned client) — a listener any co-resident process can read (§ Co-resident process trust boundary), bearer-less by construction, and the sealed arm's plaintext on a socket. Outside this leg: web (the SPA is same-origin with the nest, so the browser's `<video>` already is the pinned peer) and tui (its handoff downloads whole files through the content API — [`apps/tui.md`](apps/tui.md) § External media handoff).

**The proven identity is remembered per connection (ruled 2026-10-10; built the same day in shared Rust).** Every client decision keyed on "which nest is this" reads the identity the connection is **bound** to and refuses a nest whose own `fauna.nest.info` claim disagrees (`## Implementation status today`, the bound-identity bullet) — a proof of two round trips, the possession proof or pin read plus the cross-check. That proof is a fact about a **connection**, not about a read: it cannot change for the life of a bound connection, and a reconnect — to the same address, to a different box behind it, or by a client bound to another nest — is a new connection that proves afresh. So the one shared resolver (`LinkedNestsMachine::bound_nest_id`, behind every app's `resolve_this_nest_id` / FFI / wasm twin) runs the proof **once per connection** and serves it from the transport's own memo for that connection only (`fauna_protocol::ProvenIdentityMemo`, owned by the native `NestClient` and the wasm `WsRpcClient`, keyed on the live dispatcher's process-unique `connection_id`, which both transports mint per (re)connect). Three rules keep it a memo of a *proof*, never a cache of a *claim*: (1) nothing is remembered until the cross-check passed over this very connection, and a refusal remembers nothing; (2) the resolver takes the live connection's number BEFORE its first round trip and remembers only if that number is still live after the last — a proof that straddled a reconnect is attributed to no connection; (3) a seam with no connection of its own (a port-built web chunk, a test fake) proves on every read as before. **Why now:** every reader — the backups page's reads and writes, the custody leg at both its edges, the ledger aftermath, the DNS cert target — re-proved per call, and the `fauna.nest.info` leg rides the nest's per-actor discovery budget (`anonymous_rate_limit::DISCOVERY_MAX_EVENTS`, 60 events / 60 s on a release nest); a user signing in and enrolling this device as a custodian within the minute was refused `rate_limited` on the enroll itself, and the destination they asked for was never created (measured on tui against a release image, 2026-10-10). The budget stays at 60: raising it would weaken the harvesting bound it exists for, and the lever is the client's volume. Witnesses: `fauna_client_pair::tests::{the_proven_identity_is_remembered_per_connection_and_a_rebind_re_proves, a_proof_that_straddled_a_reconnect_is_not_remembered, a_seam_without_a_memo_proves_on_every_read}` and the memo's own `fauna_protocol::requester::proven_identity_tests`.

**The nest's self-description is read once per connection (2026-10-11).** The node leg (`domain`, `version`), the feature-limits capability set (`capabilities`) and the sign-in update look (`newest_release`) all fold the one `fauna.nest.info` reply, so the transport remembers it per connection beside the proven identity: `fauna_protocol::NestInfoMemo`, owned by the native `NestClient` and the wasm `WsRpcClient`, keyed on the same `connection_id`, and read only through `fauna_protocol::read_nest_info` — which `StatusClient::nest_info`/`node`, `FeaturesClient::node_capabilities` (behind `rows` and `authored_surface`) and `update_look::look_at_sign_in` call, so every app that reaches those shared readers (directly, over UniFFI or over wasm) draws one read per connection with no app-side memo. The rules are the proof memo's, plus two: concurrent readers on one connection share the first one's round trip (the slot is an async lock held across the read), and an error is never remembered. **Which readers may share it:** a reader whose answer is fixed for a connection's life or is read once per connection anyway — a nest restart or upgrade drops the socket, the one runtime-gated capability (`relay`) gates no feature-limits row, and the sign-in look reads `newest_release` once per sign-in. A reader that needs `newest_release` or `relay` as they are now, mid-connection, reads `fauna.nest.info` itself; so does the identity cross-check, whose proof is its own memo's. Witnesses: `fauna_protocol::nest_info_memo::tests` and the beacon counts in `tests/e2e-unified/tests/test_nest_info_draw.py`.

### Post-auth surfacing — a mid-session identity change blocks, on the same surface (ratified 2026-07-23)

Identity verification does not end at launch; three structural re-check
points keep running for the life of a session, so a nest whose identity
changes *mid-session* is detected without any new mechanism:

1. **Bearer re-mint.** Bearers are short-lived (the nest's TTL, ~1 hour,
   with a 60 s pre-expiry refresh), and every native re-mint opens a fresh
   anonymous connection and runs the full Axis-1 graduation over the silent
   challenge (`fauna_anon_client::mint_bearer_over_silent_challenge`), consulting the
   identity pin. Web's re-mint — wasm `challengeVerify` since 2026-09-23, a
   possession-checked handshake from 2026-08-19 (closing the residual
   this bullet used to record) — possession-verifies the reply's
   `cert_binding` over the challenge nonce folded with its own client nonce
   and rejects before the token is returned.
2. **SPKI-pin mismatch → re-handshake.** Every bearer-carrying connection
   requires the pinned SPKI (§ Cross-connection binding); a served-cert
   change forces a re-bind, which re-runs the full identity verdict.
3. **Background silent challenges / token refresh.** A post-auth
   `fauna.auth.challenge`/`verify` re-check yields the same
   `SilentChallengeOutcome::IdentityChanged`, and the launch machine's
   token-refresh seam the same `TokenRefreshOutcome::IdentityChanged`. The
   re-checks today are the session mint's own reply (every bearer re-mint,
   whose identity the app reads off the client's identity watch and whose
   verdict stops the supervisor — `transport-connection.md` § The dial
   budget) and the e2e bridge command's forced challenge (`silent_sign_in`).

The *security* invariant therefore already holds mid-session: a changed
identity fails graduation, the connection is torn down, and no bearer is
sent over it (§ Connection-teardown rule). What this section ratifies is
the **surface**. A post-auth `IdentityChanged` verdict, from *any* of the
three channels, is routed to the **same blocking `launch_identity_changed`
surface the launch path renders** — the warning, the explicit re-trust
affordance (`trust_nest_identity()` — forget the pin, re-TOFU), and the
fallthrough, with **no retry CTA** — and the session's bearer is dropped;
the session blocks until the user chooses (the launch-surface row in
`../behavior/onboarding.md` already states this for the machine's
token-refresh path: a mid-session identity change is the same MITM signal).
No app invents a second, softer per-app shape — no banner, badge, or
toast: a possible-MITM signal gets the one uniform surface all seven
apps already render (priority #1/#3; and the session is already
de-facto dead — its connections can no longer graduate — so a soft surface
over a broken session would be strictly worse, a generic-error mystery
instead of the honest verdict). Background refreshes stay silent for every
*other* failure class: transient/network errors remain logged-and-swallowed;
only the identity verdict escalates.

The plumbing rule that makes the routing implementable: **the typed verdict
must survive every seam between detection and surface.** The shared error
taxonomy on the bearer/connection path carries the identity verdict as a
distinguishable variant end-to-end; a client's glue is then only "on that
variant, drop the bearer and navigate to the existing surface."

The failure mode this rule exists to prevent, kept here because it is the
one a future seam will re-introduce by accident: a verdict flattened into a
stringly transport error reaches clients as a *generic* failure, which
reconnect supervisors treat as **retryable** — a transient-retry loop on a
MITM signal, the launch path's pre-2026-07-13 dead-Retry bug reborn one
layer down. So a new seam on this path either carries the variant or is a
bug; "it still compiles" is not the test, because the flattening arm is
usually a catch-all that compiles fine. Corollary, learned from the four
seams the 2026-08-15 leg fixed: **a side channel hung off one concrete
bearer cannot serve this**, because linux and tui supply their own
`BearerSource` — the property that makes the taxonomy the right carrier and
the `SupersededLatch` shape the wrong one here.

### Pre-claim surfacing — the wizard's anonymous path carries the verdict too (ratified 2026-08-30)

§ Post-auth surfacing's plumbing rule is scoped post-auth; this section
extends it to the **pre-identity wizard path** — every `WsNestApi` seam
(probe, silent challenge, claim, invite, register, age-nonce, storage/NAT
commits, escrow restore), whose fresh per-call connections each run
first-contact graduation (§ Transport trust). The *security* half already
held (teardown, nothing sent — § Connection-teardown rule); what this
ratifies is the *telling*: **the typed identity verdict must survive every
pre-claim seam.** A graduation failure the trust layer classifies as an
identity verdict is never mapped to a per-endpoint transient/retryable
variant; a seam that flattens it is a bug — the § Post-auth surfacing
failure mode one layer earlier. The measured shape was exactly that: eleven
of twelve seams mapped `AnonClientError::Trust` into per-endpoint
`Transient`s, so the "Almost ready" surface rendered a detected mismatch on
a just-provisioned box as an indefinite, error-free "waiting for DNS"
spinner.

The classification lives in ONE place — `WsNestApi::core`'s
`classify_core_failure` — never re-derived per seam:

1. **Pin-related failures** (via the shared `classify_identity_changed`
   table: a changed or forked TOFU pin, or a withheld binding while a pin
   exists) → the verdict.
2. **ANY graduation failure while a pre-resolved first-contact root is held
   for the host** (the injected deployment seed, or a pasted `fauna://claim`
   URI) → the verdict — the § Transport trust client-provisioned row's
   unconditional hard-fail, refusals and withheld bindings included.
3. **Rootless, pin-free trust trouble** (a first-contact binding failure on
   the TOFU ladder) stays transient: nothing was pinned or injected, so
   there is no identity to have mismatched, and a false-terminal is worse
   there.

The surface is deliberately NOT the post-auth blocking surface: pre-claim
there is no session to block, no bearer to drop — and **no re-trust
affordance**: with a held root the remedy is re-provisioning (§ Transport
trust: safe failure direction), never forget-the-pin, so
`launch_identity_changed`'s re-trust must not be offered anywhere in the
wizard. Instead each wizard consumer routes the verdict to its **existing
machine-shared terminal error state** — one shape per page, rendered
identically by all 7 apps; this is plumbing into pages the machine already
owns, not the "second, softer per-app shape" § Post-auth surfacing forbids.
In particular the three consumers that retry *without a human in the loop*
STOP: the "Almost ready" poll lands on the terminal error instead of the
DNS resting message, the provisioning run's claiming substep fails the
run, and the pending-invite poll's registered-probe (the not-found recheck)
lands on its terminal `Rechecking` error instead of restoring the pending
state — only a probe that cannot reach the nest keeps that poll going
(surface specs: `../behavior/onboarding.md` § "Almost ready" surface and
§ The pending-invite surface; witnesses:
`awaiting_manual_dns.rs::recheck_probe_identity_mismatch_is_terminal_not_the_dns_resting_message`,
`machine_lifecycle.rs::pending_poll_typed_verdicts_end_on_the_terminal_error`).

### Pin custody across processes — one install-scoped store, one interactive minter

An app installation can put several **processes** on the wire to the same
nest: the interactive app, and headless helpers with no user attached — the
apple File Provider extension, the desktop background sync agents. Pin custody
across them follows two rules (ratified 2026-07-22):

1. **The identity pin store is scoped to the app *install*, not to any
   process.** The pinned identity is a fact about "the nest this installation
   trusts" — the `known_hosts` of the host — so every process of one install
   reads the **same** store, rooted wherever that platform's processes share
   client state (iOS: the app-group container's `trust/` dir, beside the
   credentials and sync state the extension already shares; **macOS, since
   2026-08-25: the user-domain `~/Library/Application Support/Fauna/trust`**
   — the ONE Rust derivation `install_scoped_trust_home` the sync agent and
   fauna-tui read, because a launchd-spawned process is TCC-prompted for the
   app-group container on every instance with no user decision ever binding
   the next one (`installers/macos.md` § Identifier domain, item 5) — with
   the app keeping a **read replica** at the container's `trust/` for the
   sandboxed File Provider extension, which can reach nothing else; the
   replica is the writer's own file, copied on every persist by the store
   itself (`DiskPinStore::open_in_dir_with_mirror`), so rule 2 is untouched
   and a stale replica fails closed exactly like a missing pin; platforms
   whose processes share a plain config dir — linux, windows — already
   satisfy this with the dir itself). A per-process store is a bug twice over: the helper
   starts empty (it can never authenticate a TOFU-rooted nest the user already
   trusts), and any pin it *did* learn would drift from the app's.
2. **Only the interactive app mints or removes pins; every other process is a
   read-only consumer.** First-trust and re-trust are user decisions, made
   where a user is present — the SSH model: an interactive session prompts,
   batch mode (`StrictHostKeyChecking=yes`) refuses unknown hosts. A consumer
   process reaching a TOFU-rooted nest with no pin **fails the connect and
   retries** (`IdentityError::PinRequired` — benign: the app just hasn't pinned
   yet) rather than silently trusting whatever it reached; a writable store in
   a background process would let a MITM be pinned with no user in the loop and
   would forfeit the pin-change warning this section promises. Consumer stores
   read the file per lookup (uncached), so a pin the app mints after the
   consumer launched is honored on its next retry without a process relaunch.

Mechanism: `NestIdentityPinStore::read_only()` marks a consumer backend
(`cert_binding::ReadOnlyDiskPinStore`); graduation then takes the strict TOFU
arm (`IdentityRoot::TofuStrict` — verify/warn exactly like the interactive arm,
but never mint). The store relocation once shipped with a one-time
**copy-never-destroy** adoption of the legacy per-process pin file into the
install-scoped home (`cert_binding::adopt_legacy_pin_file`); it was retired
2026-09-24 by the compat-remnant sweep
([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program
4) — no pin file predating the relocation exists anywhere.

**The consumer's dial needs its own graduation step — and that step never
mints.** The § Cross-connection binding SPKI pin is an ephemeral
**per-process** cache populated by a handshake graduation *in that process* —
and a consumer process holds a bearer, not the identity key, so it never runs
the signed `fauna.auth.handshake` mint that graduates one; a shared identity
pin alone still leaves its bearer dial failing strict WebPKI against a
self-signed nest. The shared trusted dial (`fauna_anon_client::tls_dial`,
consumed by `fauna_client::ws_adapter`, and by the removed `/sync/ws` plane until 2026-10-02)
therefore carries a **graduate-and-retry fallback**: a failed dial with no
SPKI pin over `wss` runs one pre-identity `fauna.auth.nest_handshake`
graduation (`AnonymousNestClient::graduate_transport_trust`) and retries once
with the SPKI it cached. That graduation is **always the strict arm above**
(`PinMinting::Never`), whatever pin store the process installed: it verifies
a held identity pin or a DNS `self=` root and refuses everything else, so an
unpinned TOFU nest stays refused until the interactive path pins it. The
strictness cannot be left to the store, because the fallback runs on a
*failed* dial with a bearer in hand and no user in the loop, and every dialer
reaches it, not only consumers: a public-CA nest is unpinned until this
machine's first login completes (since the login's pin ruling above; before
it, a public-CA nest's own bearer mint took the WebPKI waiver and pinned
nothing, for good), so an interactive app (or, until its removal
2026-10-02, the standalone daemon) meets any dial error against such a nest
before that login with no pin at all. There, a
store-decides graduation TOFU-minted whichever box answered the pre-identity
handshake with a binding-valid self-signed cert — an on-path attacker needs
no CA compromise to be that box, only to make the first dial fail — and the
retry then carried the real bearer to it. Witness: tier_3
`bins/fauna-nest/tests/tls_bearer_dial_fallback_never_mints.rs` (a
writable-store process with no pin refuses and sends no retry; a held pin
still heals), beside the consumer-store twin `tls_pin_consumer_fallback.rs`.

**The derivation of the trust home is shared code, and tui is a writer
(2026-08-02).** The per-platform install-scoped home resolves through one
function — `fauna_anon_client::cert_binding::install_scoped_trust_home` (unix
`$XDG_CONFIG_HOME/fauna`; macOS the user-domain trust dir rule 1 names;
windows `%LOCALAPPDATA%\Fauna`) — consumed by writers (the linux app, tui) and
consumers (the sync agent's read-only store) alike, which makes rule 1's
alignment structural rather than a table each crate transcribes. It exists
because tui drifted: it wrote a per-app `$XDG_CONFIG_HOME/fauna-tui` store no
consumer read, so a tui-provisioned sync agent could never authenticate a
TOFU-rooted (self-signed) nest — its graduate-and-retry ran `TofuStrict`
against a store the app never wrote and failed `PinRequired` forever. (The
one-time adoption of pins pre-fix tui builds minted into the per-app dir was
retired 2026-09-24 with the adoption above.)

**The standalone headless daemon was its own install (ratified 2026-08-02;
the daemon was removed 2026-10-02 — `apps/sync-agent.md` § Headless
deployment — and this paragraph is kept as the record).**
`bins/fauna-sync` — the legacy NAS/server deployment (`file-sync.md`
§ deployments) — had no interactive app beside it, so rule 2's "only the
interactive app mints" resolved to the daemon itself: it installed a
**writable** `DiskPinStore` beside its hand-edited config (the same
seed-custody carve-out that config's `secret_key` already lives under) and
TOFU-pins on first connect — the SSH `accept-new` model, first start being the
provisioning moment. A later identity change hard-fails the graduation;
recovery is deleting the pin file beside the config and restarting the daemon.
Wherever the daemon meets that verdict — its start-up mint, its register hold
(`file-sync.md` § 1 Device Registration), a `/sync/ws` redial — it **parks**:
it stops attempting the nest, logs the verdict and the pin file's path at
`error`, and stays up until stopped. Not a retry: that is § Post-auth
surfacing's retry loop on a possible MITM. Not an exit either: the daemon's own
service units restart an exited process within seconds, the same loop only
faster. The prior state — the process-default in-memory store — was not a
lesser version of this but rule 2's exact failure: an empty, writable store at
every start silently re-TOFUs whatever answers, with no user in the loop and no
pin-change warning ever able to fire.

## Implementation status today

The transport trust model (§ Transport trust) is now **implemented for the
WS-RPC client↔nest leg** (and, from 2026-08-02 until its removal, the sync `/sync/ws` data
plane: `bins/fauna-sync` adopted the shared trusted dial + a writable
per-install pin store; before that the headless daemon dialed plain strict
WebPKI and could not reach a self-signed-floor nest at all; the daemon was
removed 2026-10-02, and the route with it) — both axes
(including the DNS `self=` root for self-signed *public* domains), the
cross-connection pin, and the **complete removal of `FAUNA_INSECURE_TLS`**:
not just the WS paths but the residual HTTP content API + `POST /register`
reqwest leg now share the WS handshake's pinned SPKI (below). The disk-backed pin store is now installed at startup on every
app (linux / windows / android / macOS / iOS all now build-verified — the
mac binding regen landed and iOS is additionally live-run-verified against a
real self-signed dev nest, see below).

- **Graduation now covers BOTH legs.** The *bearer-carrying* leg
  (`fauna_anon_client::graduate_handshake` on `fauna-launch-machine` +
  `fauna-client::ws_challenge_bearer`, via `fauna.auth.handshake`) and — since
  2026-07-07 (Track 2, Option B ratified + built) — the **pre-identity
  onboarding leg**: `fauna-onboarding-machine`'s `WsNestApi::core` runs
  `AnonymousNestClient::graduate_first_contact` (the `fauna.auth.nest_handshake`
  mechanism, § Two independent axes above) on every fresh pre-claim connection,
  graduating against the injected-seed root on the client-provisioned path and
  the DNS-`self=`/TOFU ladder otherwise. Since 2026-07-29 the same held-root
  seam has a **third feeder** — the self-hosted pre-claim row above: pasting
  the console's `fauna://claim` URI holds the printed identity via
  `hold_first_contact_identity` before the claim call opens its connection
  (machine-level, so all apps get it through their existing claim input; the
  graduation is native — web cannot reach a self-signed nest anyway). Guarded
  by the tier_3 `tls_channel_binding_roundtrip.rs` (`nest_handshake_*` +
  `graduate_first_contact_*` tests: no-actor answer, injected-root exact match,
  wrong-root + substituted-SPKI hard-fail, and — since the compat-remnant sweep
  retired the legacy-nest fallback 2026-09-24 — a kind rejection hard-failing
  on every host) and the
  machine/codec units (`claim_code::` URI round-trip + loud malformed-pin
  refusal; `machine::` pin-before-send, bare-code-no-pin). **Live-validated
  2026-07-07** on a real Hetzner box (`just e2e-live-provision`, production image): the wizard's bridge-driven claim rode the graduated first-contact
  path against the injected-seed root — self-signed pre-ACME cert +
  handler-answering nest make exact-match graduation the only green path — with
  every downstream gate (storage-mode, mail-enable, ACME TLS, firewall,
  25/587/465/993 banners, DKIM) green.

The macOS/iOS binding-regen verification (below) has since landed too.

- **Implemented (nest half):** the nest self-signs (`self_signed_cert.rs`) and
  holds a stable `nest_signing_key`; DNS publishes `self=<actor_id>` for public
  domains (`resolve.rs`); the WS-RPC auth challenge proves the *client* to the
  nest (`auth_core.rs`). The nest→client channel-binding leg signs the SPKI of
  the cert *it served this connection* (read live via `acme::ServedCertSpki`
  for the connection's served name — `MultiDomainCertResolver::cert_for_sni`'s
  own selection, [`nest/tls-certificates.md`](nest/tls-certificates.md) § A —
  the no-SNI cert for an IP dial) over `spki_sha256 ‖ client_nonce` with
  `nest_signing_key` (`auth_handlers::build_cert_binding`).
- **Built 2026-10-18 — the SNI-scoped binding** (Axis 1's *"the cert it served
  this connection"* paragraph). Measured that day on dev.fauna.build: every
  producer read `cert_for_sni(None)`, so while the § B-IP bridge was installed
  (up to 160 h after a provision —
  [`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP *Lifetime*) a
  client dialling **by name** was served the domain cert but handed a binding
  over the bridge, and refused it (`served SPKI differs from received cert`):
  no native sign-in by name for that window. Now the accepted TLS server name
  rides each connection (`served_name.rs`; `ws::RpcConnection::served_name`,
  the federation listener's `served_name`) into every producer. Witness:
  `bins/fauna-nest/tests/tls_sni_scoped_binding.rs` — a real nest over
  `serve_tls` behind a real two-cert `MultiDomainCertResolver` (domain cert +
  bridge), a real client dialling by name through the reach override: the
  identity binding, the bearer handshake, the verify fold and the federation
  hello each bind the domain cert, and a dial by IP keeps the bridge.
- **The post arm of the federation-granted row — RULED 2026-10-19; the plane, the relay stamp and the summary BUILT
  2026-10-19, the delivered-post and room carriers
  and the room-attachment leg BUILT natively the same day** (§ Transport trust → *Mechanism for the federation-granted row*
  → *The post arm*). Built:
  `posts_handlers::posts_get_remote_handler` stamps
  `PostGetReply.home_nest_actor_id` with the `peer_nest_id`
  `resolve_peer_nest_id` resolved for the dial (absent on a same-nest
  `fauna.posts.get`); `FeedManager::resolve_post_from` folds it onto
  `PostSummary.home_nest_actor_id`, and `PostSummary::blob_home` names the host
  and the identity together (`fauna_feed::BlobHome`); the foreign plane
  `fauna_client::post_blob::pinned_plane` dials each home over
  `pinned_http_client` and graduates its pin once per home through
  `fauna_client::GraduatedHome` — the lazy once-per-home graduation lifted out
  of `ForeignPublicChunkFetcher`, which holds one too; tui's four byte legs hand
  the plane the home with its identity. Witnesses: the relay case of
  `bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs`
  (`a_post_homed_on_another_nest_relays_through_the_readers_own_nest`), the
  `post_blob` unit cases (a malformed identity fails closed before dialing; a
  plain-`http` home skips graduation), and the tui two-nest journey
  `test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_searches_the_room_and_opens_a_room_post_through_the_relay`,
  whose member on another nest unseals a room post's words from a home serving
  the self-signed floor, with no trust seed and no harness-CA leaf. The native
  app face is BUILT 2026-10-19: `fauna_ffi::FfiPostBlobHosts`
  (`FfiNestClient::post_blob_hosts`, one per session, so a home graduates once
  for its life) over the same plane, with `post_blob_home` the UniFFI face of
  `blob_home`; the content-address check it keeps bytes behind is
  `fauna_core::encoding::content_address_matches`, reachable from `fauna-wasm`
  too. Witnesses: `fauna_ffi::post_blob` unit cases (the plane is handed the
  identity the post carries; substitute bytes cross as an error). **The web's
  leg — RULED 2026-10-21, BUILT 2026-10-21** (§ The post arm → *The browser's leg*):
  `GET /api/v1/blob/{id}` (and its HEAD) sits in the open `public_bytes` group,
  `x-c2pa` exposed on that layer, and the wasm face — `postBlobHome` beside
  `resolvePostFrom`, and `WsRpcClient::postBlobHosts` → `WasmPostBlobHosts`
  (`get`, `headHasC2pa`) — reads over `fauna_core::post_blob`: anonymous,
  capped at `INLINE_BLOB_BODY_LIMIT`, kept only when `content_address_matches`.
  Witnesses: `bins/fauna-nest/tests/blob_route_open_cors.rs` (a foreign origin's
  GET and HEAD answer `*` with `x-c2pa` exposed and no credentials, a bearer
  preflight is admitted, the own origin gets the open answer too) and
  `fauna_core::post_blob`'s unit cases (substitute bytes are an error). The
  SPA's adoption — every byte leg through the face — is the web lift.
  The browser residual stands by ruling: a browser fetch is WebPKI only, so a
  self-signed home's post stays teased on web, and no relay through the
  reader's nest cures it. The delivered post's carrier: `follow_delivery::
  deliver_handler` stores the delivery's verified `origin_nest_id` as
  `content.origin_nest_id` (schema 155, `CacheDb::put_delivered_post_index_only`),
  `db::feeds::project_home_nest_urls` projects it beside the URL (never for a
  discovery stub, which no verified channel delivered), `FeedPostItem.
  home_nest_actor_id` carries it and `fauna_feed`'s `map_post` folds it onto
  `PostSummary.home_nest_actor_id`, so the same `blob_home` hands every native
  app's byte legs the delivered post's home with its identity. The room's
  carrier and leg: the receiving nest's `welcome.deliver` handler stamps
  `home_nest_actor_id` from the verified origin for every cross-nest Welcome;
  `FaunaMlsBackend` records it beside `ChannelHome::Foreign` and the history
  slice persists it; `ConversationsRpc::blob_get` and `blob_put` carry it, and
  the native read and upload dial over `fauna_client::post_blob::PinnedHomes` — the clients and
  graduations `pinned_plane` itself now rests on, behind the
  `ForeignHomeHttp` seam (`conversation-rooms.md` § Implementation status
  today names the witnesses). Witnesses for the delivered post: the
  `db::feeds` case `a_delivered_envelope_lists_only_to_the_reader_who_follows_its_author`,
  `fauna_feed`'s `a_delivered_items_home_identity_rides_onto_the_summary` and
  `conformance_cross_nest_conversations_client.rs::a_followed_authors_gated_post_reaches_only_the_follower_and_no_off_box_read`
  (each delivered item names the home's identity). Still unbuilt beyond the
  web gap: a delivered envelope stored before schema 155 carries no identity
  until its home re-delivers it. **The profile relay's carrier — RULED and
  BUILT 2026-10-20** (§ The post arm → *A relayed
  profile's pictures*): `profile_handlers::profile_get_remote_handler` stamps
  `ProfileGetReply.home_nest_actor_id` from the same `resolve_peer_nest_id`
  (absent on a same-nest `fauna.profile.get`), `fauna_client_profile::HeaderRead`
  carries it beside `relayed_from`, and tui's header read hands `PostBlobHosts`
  the home with its identity; the other six apps' header reads take it in
  their trickle-down. Witnesses:
  `fauna_protocol::profile`'s additive round-trip
  (`profile_get_reply_home_nest_actor_id_round_trips_and_is_additive`), the
  stamp assertions of
  `conformance_cross_nest_conversations_client.rs::a_profile_homed_on_another_nest_relays_only_with_the_actors_consent`,
  `fauna-client-profile`'s `the_header_read_names_the_home_a_relayed_profile_came_from`,
  tui's `the_other_header_relays_to_the_carried_home_on_a_local_not_found`,
  and the tui two-nest journey `test_profile_remote_read_cross_nest.py`'s
  picture leg — the reader's header paints an avatar fetched from a home
  serving the self-signed floor.
- **Implemented (client half — Axis 1 + Axis 2 + cross-connection):**
  - A **capturing TLS verifier** (`fauna_anon_client::tls_verify`) provisionally
    accepts the cert (encrypt-only), records its SPKI + WebPKI validity, and
    (for a bearer connection) hard-fails unless the served SPKI matches a pin.
  - The bearer-minting silent challenge on **both** native bearer paths
    (`fauna_client::ws_challenge_bearer::WsChallengeBearer` for the UniFFI
    apps — and `bins/fauna-sync` until its removal 2026-10-02 — and
    `fauna-launch-machine` for linux + tui)
    sends `client_nonce`, then `fauna_anon_client::trust::graduate_verify_path`
    verifies the binding against the *received* SPKI, checks the identity against
    its Axis-2 root (`cert_binding::{verify_cert_binding,check_identity_root}`),
    and pins the bound SPKI. WebPKI-valid certs take the boring path (no binding
    required) **only when no Axis-2 root is held and no binding was offered**
    (the second condition since 2026-10-05 — § Transport trust → *The login's
    pin*: the login paths always have one in hand, so on them the full core
    runs and the identity it verifies is recorded as the pin, pre-resolved
    roots included); non-WebPKI (self-signed/LAN)
    certs require the binding, and so does *any* cert once a root is held
    (tightened 2026-09-02 — WebPKI answers "is this the
    address I dialed", the root answers "is this the nest I provisioned", so the
    stronger authenticator is always used when the caller has one; before this,
    the WebPKI arm returned before the root was read, which was safe only while a
    domainless box served an untrusted floor — the premise
    [`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP deletes).
    **"Held" covers both a root the caller pre-resolved and an identity pin the
    install-scoped store already names for the host** — the claim-seeded pin on
    a client-provisioned box (`seed_claimed_identity_pin`), or an ordinary TOFU
    pin — so the posture outlives first contact on the bearer-mint and launch
    paths, which never see the seed; `trust::webpki_waives_binding` is the one
    decision point all four graduation entrypoints share (ruled 2026-09-02, after the bridged-box pin was found to last exactly
    one connection: the wrappers' unconditional WebPKI arm cleared it on the
    very next bearer mint). **`graduate_first_contact`'s rejection arm hard-fails
    on every host** (since 2026-09-24; the intermediate `trust::rejection_is_hostile`
    predicate, ruled 2026-09-02, had first made that
    arm read the waiver's own "held" — it previously hard-failed on an explicit
    root alone, so one function carried two definitions of "a root is held"
    twenty lines apart and the box on the wire chose which ran — and the
    compat-remnant sweep then removed the pre-Track-2 fallback the predicate's
    other arm still allowed, § *Compat* above). The
    Axis-2 root is resolved by `trust::resolve_dns_self_root` (the only async step,
    delegating to the sync `graduate_handshake_with_root` core): a registrable
    **public** domain that publishes `_fauna.{host}` TXT `self=` gets the strong
    DNS root (exact identity match, no pin — `lookup_fauna_txt` +
    `probe::public_dns_host`, which shares `resolve_handle_domain`'s `.local`/IP
    classification); LAN/`.local`/IP hosts and public domains that publish no
    `self=` fall to TOFU-on-host.
  - **The login's pin (ruled and built 2026-10-05).**
    The two login wrappers (`graduate_handshake`, `graduate_verify_path`) never
    waive a binding they hold, and record the identity the core verified
    (`trust::Graduated::Verified` → `trust::record_login_identity`) as the
    host's pin, pre-resolved roots included; `graduate_first_contact` and the
    never-minting fallback are unchanged. Unit witnesses in `trust::tests`
    (named at the ruling). **Verified live against the public-CA staging box
    (2026-10-05 and 2026-10-10, tui, no e2e trust seed, no `FAUNA_LIVE_*`):**
    the rerun of the first live sweep's reds on a build containing the fix
    turned the 103 escrow-trust reds green, and its logs carry zero
    `does not trust` and zero `no candidate generation tip resolves` lines
    (the pre-fix sweep carried 344 and 350); a fresh 2026-10-10 run of the
    live-selectable `test_backups.py` and `test_mail_serving_toggle.py`
    (17 passed) repeats the zero `does not trust` count. The two tests first named as the live proof are
    not live witnesses by construction: `test_backup_destination_crud` takes a
    second nest and the `archive_nest` journeys need a harness-started box, so
    live mode deselects both at collection (`testing.md` § Default app and nest
    mode → *What only a harness-started nest gives is deselected*). The
    standalone witness of rule (1) is the nest-leg trust seed's harness-CA
    login, `test_nest_identity_pin_webpki.py` (tui, built 2026-10-13): red
    at its "the login leaves the pin" half with the waiver's
    `binding.is_none()` clause removed, green on current code, with a
    self-signed control arm green on both (`e2e-automation-surface-gating.md`
    § Implementation status today records the runs).
  - **The TOFU-mint decision is atomic** (`NestIdentityPinStore::pin_if_absent`,
    fixed 2026-08-17): `check_identity_root`'s `Tofu` arm used to check-then-act
    (`get()` a pin, decide, then a separate `set()`) — a genuine race between two
    concurrent graduations of the *same* host (a client's synchronous
    login-time bearer mint racing its own fire-and-forget background silent
    challenge, both TOFU-checking the same host — tui's
    `session::spawn_domain_refresh` was one such caller, until it was folded
    onto the session mint's identity watch on 2026-10-08). Under load, one
    graduation's stale, still-in-flight mint could land **after** a second,
    newer pin decision and silently clobber it back with no warning —
    `test_nest_identity_pin_post_auth.py` caught it intermittently. Every
    `NestIdentityPinStore` backend a multi-threaded native runtime can call
    concurrently (`MemoryPinStore`, `DiskPinStore`) now performs the whole
    check-and-mint under one lock acquisition; a concurrency unit test
    (`nest_trust::tests::pin_if_absent_is_atomic_under_concurrent_first_contact`)
    races 32 threads against the same host and pins the winner-agreement
    property.
  - **Every client decision keyed on "which nest is this" reads the identity
    the connection is bound to, never the nest's own `fauna.nest.info` claim.**
    `LinkedNestsNest::bound_nest_id` (the origin's pin, else a possession proof
    over the connection) surfaces as `LinkedNestsMachine::bound_nest_id()`,
    which also **refuses** a nest whose claim disagrees with it — a
    disagreement is hostile or broken, and a silent substitution would hide it.
    The blessing decisions moved first; the
    deployment-seed custody comparand (BR-2), the then seed-map fan-out's
    owned-box guard (the fan-out itself is retired since — `nest/box-recovery.md`
    § Implementation status today), the co-admin self-heal, the predecessor entry a
    rotation supersedes and both ids a both-ends link writes followed on
    2026-09-27 — on all 7 apps
    through that one resolver (linux/tui `resolve_this_nest_id`, the `fauna-ffi`
    and wasm twins). **The DNS cert `target_nest_id` moved on all 7 apps:** every app seals the cert to the connection-bound id, `LinkedNestsMachine::bound_nest_id()`, never the claim. Before it, a box the user was a mere user of could answer
    `nest.info` with an owned sibling's id and receive the admin's whole sealed
    custody map past the fan-out's blast-radius guard, or have that sibling's
    custody entry marked superseded by a rotation it ran. Per-consumer
    mechanics: [`nest/box-recovery.md`](nest/box-recovery.md) § Implementation
    status today (the BR-2 bullet). **Proven once per connection since
    2026-10-10** (§ Transport trust → *The proven identity is remembered per
    connection*): the resolver serves a passed proof from the transport's
    `ProvenIdentityMemo` for the rest of that connection; a re-bind re-proves.
    Measured 2026-10-10 on tui (`test_nest_info_draw.py`, counting the
    nest's dispatch beacon for one actor from sign-in through the
    hosted-custodian Backups journey): **10 `fauna.nest.info` reads under
    the memo, 19 with it bypassed** — the nine it removes are the custody
    leg's second edge, the ledger aftermath and the Backups page's own
    re-proofs. Six of the ten that remained were capability reads that prove
    nothing (the tui settings landing's node, feature-limits and own-limits
    legs, one each, on each of the sign-in's two landing loads); since the
    same day the landing reads the reply **once per connection** and hands it
    to all three legs (`FeaturesClient::rows_with` / `authored_surface_with`,
    `StatusClient::nest_info`), and the journey measured **5**. The tui's
    sign-in update look (`newest_release`) then took the same reply instead
    of a read of its own (2026-10-10), and the journey measures **4** —
    pinned at 8 or fewer: the shared reply, the custody leg's one proof, and
    two sync-engine peer-leg passes (one read per pump pass, the ruled relay
    refresh). Since 2026-10-11 the memo lives in the transport
    (§ Transport trust → *The nest's self-description is read once per
    connection*): the shared readers read through it on all 7 apps and the
    tui's app-held memo is retired. The settings landing's re-loads on one
    connection draw no `fauna.nest.info` (measured 0 across three re-loads on tui, linux and web, 2026-10-12; pinned at 2 or fewer). macOS and
    windows still run the interim release-host sign-in look, which reads no
    nest, until they move to the FFI's `look_for_newer_release_at_sign_in`
    (it reads the shared reply); web, iOS and android have no sign-in look.
  - **Cross-connection propagation:** `ws_adapter` requires the pinned SPKI on
    the authenticated WS that carries the bearer; absent a pin it falls to strict
    WebPKI. The **residual HTTP content API + `POST /register` reqwest leg** shares
    the same pin: its reqwest client (`fauna-client`'s `AuthClient::new`, the linux
    `build_http_client`, the windows hydration host) is built with
    `trust::store_pinned_reqwest_tls(nest_url)`, whose verifier reads
    `pinned_spki(authority)` **per-handshake** — so it pins the moment the WS
    handshake graduates one (and re-reads it across a cert rotation), refusing a
    non-WebPKI cert with no pin rather than accepting any. `FAUNA_INSECURE_TLS` no
    longer touches **any** path (WS or reqwest); the env var is gone. **Never
    resumed (2026-10-08):** every `config_with`-built config — this one, the
    capturing and the SPKI-pinned WS configs — disables client session resumption,
    so the per-handshake read is per connection (§ Transport trust → *No session
    resumption on a pinned config*; witness `reqwest_pinned_tls::
    a_re_pin_bites_on_the_next_request_even_when_the_server_offers_resumption`).
  - **Windows `DirectNestClient` — the *second* residual-HTTP leg (C#), pinned
    via shared-Rust FFI**
    (`apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/DirectNestClient.cs`).
    The "windows hydration host" above is the Rust `fauna-sync-agent` (its reqwest
    client *is* `store_pinned_reqwest_tls`-built); but the desktop app's own health /
    blob / snapshot calls go through a .NET `HttpClient`, which the Rust rustls pin
    store cannot govern. That leg shares the same pinned identity via a
    `ServerCertificateCustomValidationCallback` that accepts a served cert iff it is
    WebPKI-valid, OR the host is loopback (same-box install — the sanctioned
    `danger_accept_invalid_certs` prior art, also covering the unauthenticated health
    check that can precede the pin-graduating handshake), OR its SPKI matches the pin.
    The loopback and pin carve-outs are the *configured nest's*, so the callback grants them
    only to a request for that nest's own host and port (`NestCertTrust.IsForNest`, host
    compared case- and bracket-blind, port defaulted per scheme): `HttpClientHandler`
    follows a 30x by default and calls back for the redirected request, and a redirect
    anywhere else — or a request naming no host — falls to strict WebPKI alone, so a
    loopback nest's redirect target is never accepted unverified. The pin key stays the
    port-as-written authority Rust pinned; the guard compares host and port numerically and
    never re-derives it. The Apple leg below grants the same.
    The SPKI computation, pin lookup, and loopback classification are the *same shared
    Rust* the handshake uses, exported as `fauna_ffi::{spki_sha256_of_cert_der,
    pinned_spki_for_host, authority_of, is_loopback_authority}` (gated `nest-trust`), so
    the C# value is byte-identical to the pin — no .NET re-encoding divergence — and the
    `host[:port]` loopback test (including the IPv6-bracket parse) is one tested impl
    rather than a hand-rolled per-app parser. This is what lets a fresh same-box app reach its installed
    nest at `https://127.0.0.1:443` and a remote `test@<ip>` self-signed nest serve it
    media, with no public CA.
    - **The authority is the host a URL parser would dial — userinfo is never part
      of it.** `authority_of` drops any `userinfo@` (`fauna_core::web::strip_userinfo`,
      splitting on the *last* `@` per WHATWG) before the string becomes either the pin key
      or the loopback classifier's input, and `is_loopback_authority` strips again on its
      own input because it is a UniFFI export any app's cert callback can reach. Both
      matter: the *same-box* carve-out above must be granted only when the connection
      really goes to the local box (granting it to a remote nest would disable WebPKI
      **and** the pin at once), and a TOFU pin must be keyed on the host actually dialed,
      or that host escapes the identity-change warning. Malformed
      authorities stay non-loopback in **both** the bracketed and unbracketed camps
      (`[::1]:junk` no more loopback than `localhost:junk`); an unparseable port suffix is
      never discarded to yield a clean literal. Landed 2026-08-01 with the
      `is_loopback_authority` fix; the same `strip_userinfo` backs
      `fauna_onboarding_machine::helpers::nest_host` so the two host extractors that read a
      nest URL cannot disagree about which side of an `@` is the host.
    - **Every nest-URL host extractor splits the authority the same way** —
      `strip_userinfo` then `split_host_port`, keeping IPv6 brackets and stripping them only
      where an `IpAddr` parse follows (`fauna_core::resolve::strip_ipv6_brackets`). The last
      hand-rolled splitter, `fauna_core::resolve::parse_node_address`, joined the family
      2026-08-01, replacing a private splitting rule of its own that could misclassify a
      host and so let it through `is_public_dns_name`. That parser is load-bearing in three
      user-visible places — the SRV **reconnect** self-heal, `fauna_client_dns`'s dial
      classification, and the IMAP/SMTP/CalDAV endpoints shown for a user to paste into
      their MUA — so all three now read a nest URL's authority through the one shared
      splitter.
      - **The authority itself ends at the first of four bytes — `/` `\` `?`
        `#` — never just `/`, or `/` and `\`.** A raw
        `?`/`#` in a nest URL is not a metacharacter these extractors used to
        stop at, so a phished URL like `https://nest.example.com?@[::1]`
        read the query's `[::1]` as the host while every WHATWG parser — the
        real dialer included — resolves `nest.example.com`. This is the
        *third* variant of the same drift, after userinfo and `\` alone: each fix landed in
        one extractor and left the others narrower. The terminator set now
        lives in exactly one place, `fauna_core::web::authority_len`, behind
        two public callers: `authority_of` (its own fixed
        `http`/`https`/`ws`/`wss` scheme set, for the TLS-trust pin key and
        loopback classifier above) and `fauna_core::web::generic_authority`
        (any `scheme://`, for the display/resolve extractors —
        `fauna_core::format::url_host_opt`,
        `fauna_onboarding_machine::helpers::nest_host`,
        `parse_node_address` below, `fauna_core::data::url_host` (the
        peer-anchor nest-URL extractor),
        `fauna_launch_machine::auth::hint_authority_url` (the wasm reach-hint
        dial, joined 2026-09-14, which also composes
        `split_host_port` to keep the URL's own port when substituting the
        hint IP), and `fauna_provisioning::probe::split_host_port` (joined
        2026-09-16, cutting at the terminator before
        `strip_userinfo` — below) — which never restricted the scheme the way
        `authority_of` does).
      `fauna_provisioning::probe::split_host_port` joined the family
      2026-09-03: it classified `split_host_port(domain)` without
      stripping userinfo first, so `resolve_handle_domain` read one authority (the
      unstripped host) but composed its `base_url` from a *different* one (the whole
      `domain` string) — a userinfo-carrying domain like
      `nest.example.com@attacker.example` classified by the wrong half of the `@` and
      then composed the URL from the un-stripped whole. Both are fixed: the classifier
      strips userinfo first, and the public-domain branch composes `base_url` from the
      same `host`/`port` it just classified, never from the raw input. The one
      caller-reachable through this path — the Bluesky OAuth `client_id` derivation
      (`bins/fauna-nest/src/bluesky/mod.rs::oauth_public_url`) plus the claim-time and
      admin add-domain identity doors — additionally reject a domain that fails
      `fauna_core::web::is_hostname_syntax` (userinfo, a path, a query, a fragment, or
      whitespace) before it ever reaches the classifier, so a malformed candidate is
      refused rather than silently normalized.
      **Fixed 2026-09-16, filed:**
      `probe::split_host_port` used to read to the end of the
      WHOLE string handed to it, not to the end of an authority the way a URL
      parser ends one — so a string carrying a URL metacharacter *past* a
      `.local`/loopback-looking suffix (`x.example#.local`) misclassified as
      non-public while `base_url` composed an authority a URL parser would
      resolve differently, the same terminator-set gap a sibling finding fixes
      for `authority_of`. Fixed by joining the `generic_authority`
      family above rather than special-casing `.local` inside the classifiers:
      `split_host_port` now cuts at the first authority terminator *before*
      `strip_userinfo` — not after, since a terminator preceding the last `@`
      would otherwise be read as part of the userinfo half — so classify and
      compose both name the authority a URL parser would actually dial
      (`probe.rs::a_terminator_suffixed_local_string_names_one_authority`).
      `fauna_core::resolve::is_public_dns_name` and `is_private_network_target`
      are unchanged; the terminator set still lives solely in
      `fauna_core::web::authority_len`. Not reachable before the fix, same as
      stated at filing: every identity-writing door gates on `is_hostname_syntax`
      first, and the un-gated federation/peer-domain resolvers
      (`ConversationsClient::actor_by_handle_remote`,
      `AnonAttendeeDiscovery::resolve_actor`) consult only `base_url`, never this
      bool — the real TLS trust decision is keyed off the dialed connection's own
      parsed authority, not this classifier.
    **Live-verified end-to-end on BOTH branches — no residue.** Until this work no automated
    test opened a socket on this path at all; the unit suites (`NestCertTrustTests`,
    `NestCertTrustFfiTests`, and `DirectNestClientCertCallbackTests` — the callback driven
    with a hand-built request for another host) only pin the decision function. Two tier_3 suites now drive a real
    FaunaApp against a nest serving its self-signed floor over real HTTPS, and prove the same
    C# legs land on each branch: **health** (`IsAvailableAsync`) plus **blob upload *and*
    download** (media).
    - **Loopback branch (2026-07-11)** — `test_self_signed_nest_client_legs.py`, via the
      `self_signed_nest` fixture (the one e2e nest that drops the process-wide plain-HTTP
      escape). This is the same-box install: `https://127.0.0.1:443`.
    - **SPKI-pin branch (2026-07-12)** — `test_spki_pinned_nest_client_legs.py`, via
      `spki_pinned_nest`: the same self-signed floor, dialled on the box's own **LAN IP**. The
      authority is therefore non-loopback (`is_loopback_authority` classifies IP literals and
      never resolves DNS) and the cert is not WebPKI-valid for it — a negative-control test
      asserts a *verifying* TLS client is rejected — so `spki == pin` is the **only** term of
      the decision that can accept it. A green is a proof of the pin branch **by elimination**.
      This is the remote `test@<ip>` case, and it also proves the bearer mint graduates the pin
      *before* the unauthenticated health check runs (on loopback that ordering is masked by
      the loopback term, which exists partly to cover a pre-pin health check).
  - **Apple `APIClient` — the *third* residual-HTTP leg (Swift `URLSession`), the same
    policy through the same shared-Rust FFI (2026-09-24)**
    (`apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/NestCertTrust.swift`). The macOS and
    iOS apps' blob upload / download / `HEAD`, chunk and manifest calls go through one
    `URLSession` per `APIClient`; it used to be `URLSession.shared`, which makes **no** trust
    decision — the OS trust store refused the self-signed floor cert
    (`NSURLErrorServerCertificateUntrusted`, -1202) and media failed against a same-box nest
    while every Rust leg accepted it. The session now carries a `URLSessionDelegate` whose
    server-trust answer is windows' `NestCertTrust.ShouldTrust` member for member — accept
    iff `SecTrustEvaluateWithError` passes (WebPKI), OR the nest authority is loopback, OR
    the served leaf's SPKI equals the pin — with the SPKI, pin lookup and loopback
    classification read from the same four `fauna_ffi::trust` exports the C# callback uses.
    The carve-outs are granted only to a challenge for the configured nest's own host and
    port (`challengeIsForNest`, the twin of the C# leg's `NestCertTrust.IsForNest`, pinned
    by the same eight rows on both sides — a redirect anywhere else falls to the system
    default, strict WebPKI). The one refinement over the C# leg, also fail-closed: a refusal
    answers `.performDefaultHandling` rather than cancelling — the system re-evaluates the
    same trust it just failed, so the request still dies, but as -1202 rather than a
    `.cancelled` the UI would misread as the user's own cancellation. Pinned by `NestCertTrustTests.swift` (the pure decision, the
    real FFI classifiers, and the delegate's verdict over a real self-signed certificate:
    accepted on loopback, refused off loopback with no pin) and live by
    `test_self_signed_nest_client_legs.py` on `--app macos` and `--app ios`.
  - **Android `ApiClient` — the *fourth* residual-HTTP leg (Kotlin OkHttp), the same
    policy through the same shared-Rust FFI (2026-09-26)**
    (`apps/fauna-android/app/src/main/java/com/fauna/app/core/NestCertTrust.kt`). The
    android app's blob upload / download / `HEAD` calls go through one OkHttp client; it
    used to be the plain injected one, which makes **no** trust decision beyond the OS
    trust store and so refused the self-signed floor cert (`SSLHandshakeException`).
    `ApiClient` now derives its client with `NestCertTrust.makeClient`, whose verdict is
    windows' `NestCertTrust.ShouldTrust` member for member over the same four
    `fauna_ffi::trust` exports, the carve-outs granted only to a peer at the configured
    nest's own host and port (`NestCertTrust.isForNest`, the same eight rows as the C# and
    Swift legs). JSSE splits what the C# callback and the Swift delegate answer at once, so
    the one verdict runs on both halves: an `X509ExtendedTrustManager` answers for the
    chain (its socket and engine variants name the peer; the two-argument variant names
    none and gets strict WebPKI alone) and a `HostnameVerifier` answers for the name — a
    connection stands iff the chain AND the name are valid, OR the peer is the nest and a
    carve-out holds. The nest URL is read at handshake time (`ApiClient` is one instance
    across sign-ins, and signed out it names no nest, so no carve-out exists), a refusal
    rethrows the platform's own exception, and the injected base client
    (`AppModule.provideOkHttpClient`) stays strict WebPKI for any other consumer. Android
    carries no Kotlin WebSocket: the bearer-carrying dial is shared Rust's alone. Pinned
    by `NestCertTrustTest.kt` (the pure decision, the real FFI classifiers, the trust
    manager over a real self-signed certificate — accepted on loopback, refused off
    loopback with no pin, accepted on a matching pin and refused on a disagreeing one —
    and what a peer that is not the nest is granted). **Not yet live-verified:**
    `test_self_signed_nest_client_legs.py` carries `pytest.mark.android`, but no android
    e2e run exists until the run venue's harness lands (`testing.md` § Default app and
    nest mode → *Android's run venue*).
  - **The native-player leg — reader BUILT 2026-10-08, windows' adapter BUILT 2026-10-09,
    linux' and android's BUILT 2026-10-10, apple's UNBUILT (ruled 2026-10-08, § Transport trust → *The native-player
    leg*).** The shared reader exists: `fauna_client::RangedNestReader`
    (`libs/fauna-client/src/ranged_nest_reader.rs` — a `bytes=0-0` probe for `len` /
    `content_type`, then one `Range` GET per `read_at`, over `pinned_http_client`) and its
    sync UniFFI face `FfiRangedNestReader` (`libs/fauna-ffi/src/ranged_nest_reader.rs`,
    gated `nest-trust`), with tier_1 `libs/fauna-client/tests/ranged_nest_reader.rs` over
    a self-signed ranged fixture (slices, EOF, a non-ranging server, 416, an unknown
    total, and pinned / unpinned / disagreeing-pin). **windows reads through it:**
    `VideoPlaybackResolver.ResolveAsync` opens the reader on the client's configured nest
    URL (`INestHttpClient.NestUrl` — the string the WS login minted on, so the pin key
    matches) + the projection's nest-relative path, off the UI thread, for both `Url`
    arms (the public blob and the ticketed proxy path); `VideoThumbnailView` plays
    `MediaSource.CreateFromStream` over `RangedNestStream`, a WinRT `IRandomAccessStream`
    shell over `FaunaApp.Core`'s `RangedNestCursor`, and releases the reader with the
    player; nothing on windows hands the OS media stack a nest URL. **linux reads through
    it directly from Rust:** `views/feed/video_thumbnail.rs` opens `RangedNestReader` on
    `FaunaClient::node_url` (the WS login's string) + the projection's path, and
    `gtk::MediaFile::for_input_stream` plays `feed/ranged_stream.rs`'s `RangedNestStream`
    — a `gio::InputStream` + `gio::Seekable` subclass whose reads `block_on` the reader on
    GStreamer's streaming thread. **android reads through the UniFFI face:**
    `InlineVideoPlayback.resolve` opens `FfiRangedNestReader` on `ApiClient.nodeUrl` off
    the main thread, and the framework `MediaPlayer` reads it through
    `RangedNestDataSource` (`setDataSource(MediaDataSource)`). Still open: apple's `InlineVideoPlayer.start` hands `AVPlayerItem(url:)` the
    nest URL, so there every public-`Video` stream (and every ticketed `ProxiedVideo`
    stream, once apple wires its tap) is dialed by the OS media stack, unpinned, and fails
    against a nest on its self-signed floor, the same-box install included. The `Sealed` arm is unaffected on every app (its bytes ride the pinned
    fetch). **The e2e:** `test_feed_video_playback.py`'s ordinary legs run on the
    plain-HTTP session nest (`FAUNA_INSECURE_DISABLE_TLS`) and cannot see this, so its
    TLS twin `test_a_video_plays_against_a_self_signed_nest` runs the same journey on
    `self_signed_logged_in_app` (the loopback floor) and `spki_pinned_logged_in_app` (the
    pin branch) — windows' and linux' legs green; android's owed with its run venue
    (`InlineVideoPlaybackTest` pins the data source over a fake reader meanwhile); macos +
    ios declared unbuilt until apple's adapter lands, joining it red first.
  - A disk-backed identity pin store (`cert_binding::DiskPinStore`) survives
    restarts; the in-memory `MemoryPinStore` is the default until an app
    installs the disk store (`trust::install_pin_store`). **Linux installs it**
    at startup (`apps/fauna-linux/src/client.rs::install_disk_pin_store`, called
    from `main.rs`'s `connect_startup` before the first authenticated connect)
    rooted at `~/.config/fauna/nest_identity_pins.json` — the XDG config dir its
    MLS / P2P / window-state stores already use, *not* the nest sidecar's
    `service_watcher::resolve_data_dir()` (which is `None` for a pure client, so
    pins would never persist on a laptop). `fauna-client` re-exports
    `fauna_anon_client::trust` so every app installs it the same way.
  - **Apple pin custody is install-scoped (closed 2026-07-22 — was the "appex
    installs an EMPTY pin store" KNOWN GAP, measured on an iOS simulator as
    `ws connect failed: invalid peer certificate: UnknownIssuer` retrying
    forever).** Per § Pin custody across processes: the app's
    `NestTrust.installPinStore()` roots the store at the app-group
    `trust/` dir on iOS, and — **since 2026-08-25, on macOS** — at the
    user-domain `~/Library/Application Support/Fauna/trust` taken from the
    `install_scoped_trust_home` FFI export (the one-time adoption of the
    per-process file and the 2026-07-22..2026-08-25 container primary was
    retired 2026-09-24 by the compat-remnant sweep, § Dimension 2 program 4 of
    [`version-compatibility.md`](version-compatibility.md))
    with the container's `trust/` maintained as the extension's read replica
    (`install_nest_identity_pin_store_with_mirror`; tier_1-pinned in
    `fauna-anon-client`'s `cert_binding` tests); the extension installs the
    container dir **read-only** on both OSes
    (`NestTrust.installPinStoreReadOnly()` →
    `install_nest_identity_pin_store_read_only`) — it consumes the pin the
    user's onboarding accept minted and can never TOFU-mint on its own. E2E
    launches keep the legacy per-process path (the app-group container is
    machine-global state a test launch must never touch — e2e-launch-isolation.md
    § conventions point 10, same branch as `FaunaClient.syncStateDir`).
    The pin store alone was necessary but not sufficient: the appex mints no
    bearer in-process, so nothing graduated a bound SPKI and its dial failed
    strict WebPKI regardless — closed by the `ws_adapter` graduate-and-retry
    fallback (§ Pin custody across processes, *the consumer's dial needs its
    own graduation step*). Consumer-side strictness + live pickup proven by
    `libs/fauna-anon-client/tests/read_only_pin_store_consumer.rs`;
    the full consumer dial (refuse-unpinned → connect-once-the-app-pins, over a
    real self-signed-TLS nest) by tier_3
    `bins/fauna-nest/tests/tls_pin_consumer_fallback.rs`; the fallback's
    graduation never minting in a *writable*-store process either
    (`PinMinting::Never`, 2026-09-28) by its twin
    `tls_bearer_dial_fallback_never_mints.rs`.
  - **The desktop background sync agents are read-only consumers too (closed
    2026-07-22 — was the "installs no pin store at all" gap).**
    `bins/fauna-sync-agent` ran on the writable `MemoryPinStore` default, which
    is wrong twice over: empty at every start (a TOFU-rooted nest was simply
    unreachable from an agent process) and *writable* (it would mint a pin for
    whatever it reached, with no user in the loop). It now installs
    `ReadOnlyDiskPinStore` at startup (`crate::trust::install_consumer_pin_store`,
    called from `service::run_agent_with_store` before the capability restore
    can dial), rooted at the install-scoped trust dir — which per rule 1 is a
    *sibling* of the agent's own root, never under it: `<app-group>/trust` on
    macOS (the same dir `NestTrust.sharedTrustDir()` returns),
    `$XDG_CONFIG_HOME/fauna` on linux and `%LOCALAPPDATA%\Fauna` on windows (the
    dirs those apps already pass to `install_nest_identity_pin_store`). An
    explicit `--data-dir` keeps the store inside that root, so a test launch
    never touches the box's real one (e2e-launch-isolation.md § conventions point 10). One
    call covers all three platforms: the windows `fauna-sync-agent.exe` shim is
    a thin `main()` over the same `run_main`. Proven by
    `bins/fauna-sync-agent/tests/agent_pin_consumer.rs` (reads the app's pin,
    sees a later mint with no relaunch, cannot mint or remove one) plus the
    per-platform dir derivations in `trust::tests`.
  - Proven end-to-end by tier_3 `bins/fauna-nest/tests/tls_channel_binding_roundtrip.rs`
    (genuine self-signed nest connects over `wss://`; substituted cert rejected
    before the bearer).
- **Per-app durability + web hardening record (remaining gap: the macOS/iOS
  binding regen):**
  1. **Per-app durability wiring — wired on every app.** Each disk-backed
     app installs the store at startup so TOFU pins survive restarts.
     - **Linux** (native Rust): `client::install_disk_pin_store` →
       `~/.config/fauna/`. **Verified** — `libs/fauna-anon-client/tests/disk_pin_store_durability.rs`
       (a TOFU pin survives a simulated restart; a changed identity is then
       rejected).
     - **tui** (native Rust): `session::install_disk_pin_store` → the
       install-scoped trust home (`install_scoped_trust_home`, § Pin custody
       across processes rule 1 — never a per-app `fauna-tui/` dir), called once
       from `main.rs` before `launch::start` runs the silent challenge. Same
       shared seam as Linux (`DiskPinStore::open_in_dir` →
       `trust::install_pin_store`, canonical `NEST_IDENTITY_PIN_FILE`), so on
       Linux the two apps read and write the one pin file — the install's, not
       the app's. **Verified
       end-to-end** by tier_3 `tests/e2e-unified/tests/test_nest_identity_pin.py`
       `[tui]` (a seeded pin survives a real force-quit + relaunch over
       `self_signed_nest` TLS and drives `LaunchPhase::IdentityChanged`). Until
       this landed tui alone kept the volatile `MemoryPinStore`, so a nest-identity
       change across a restart — the pin's whole point — was never caught on tui.
     - **UniFFI apps (windows / macOS / iOS / android):** one shared exported
       Rust fn `fauna_ffi::install_nest_identity_pin_store(data_dir)` (which calls
       `DiskPinStore::open_in_dir` → `trust::install_pin_store`), called once at
       each native startup with that app's config dir — macOS/iOS
       `FaunaKit.NestTrust.installPinStore()` (the app-group `trust/` dir since
       2026-07-22, with the File Provider extension as a read-only co-reader —
       § Pin custody across processes; e2e launches keep application-support
       `Fauna/`), Windows `App.OnLaunched` (`%LocalAppData%\Fauna`), Android
       `FaunaApp.onCreate` (`filesDir`). The canonical pin filename
       (`cert_binding::NEST_IDENTITY_PIN_FILE`) lives in one place, so every
       app's on-disk layout is identical. **Windows compile-verified**
       (2026-05-31, on Windows): `just windows-ffi` regen → generated
       `FaunaFfiMethods.InstallNestIdentityPinStore(string dataDir)` → WinUI
       MSBuild builds `FaunaApp.dll` clean against it. **Android compile-verified**
       (2026-06-01, on a Linux dev machine): `just android-ffi` regen → generated
       `fun installNestIdentityPinStore(dataDir: String)` (`fauna_ffi.kt`) →
       `just android-debug` builds the debug APK clean against `FaunaApp.onCreate`'s
       call (NDK 28 — see the internal dev-setup notes § Android NDK on the Linux dev machine; the verify
       runs here, not on the emulator host). **macOS / iOS compile-verified on macOS**
       (`just apple-ffi` regen + Xcode build, both targets — the shared
       `FaunaKit.NestTrust.installPinStore()` builds clean into both the app
       and the File Provider extension, each `rc=0`). **iOS additionally
       live-run-verified**: the pin store install runs during a real
       onboarding flow against a self-signed dev nest (2026-07-20 iOS
       pixel-check session, tracked internally), and — after
       the 2026-07-22 app-group relocation — its pin is read back by the File
       Provider extension read-only (§ Pin custody across processes). Adding
       the export changes the UniFFI contract checksum, so every native
       app must regen its bindings.
     - **Web** is exempt from the *disk* pin store — the browser validates TLS
       itself and wasm has no `DiskPinStore` (it keeps a localStorage pin instead;
       part a-ii below). The **residual** of that exemption is
       hardened by review: the SPA now refuses to navigate
       (`window.location` / `window.open`) to a non-`https:` **nest-supplied**
       destination — the bridge OAuth `redirect_url` and the subscription
       `payment_url` are scheme-allowlisted via `#lib/safe-url.ts` `isSafeNavUrl`
       (`routes/bridges/+page.svelte`, `routes/profile/[[actorId]]/+page.svelte`),
       so a spoofed/compromised nest can't relay the user to an attacker scheme
       (the anti-phishing-redirect class, not XSS — the browser already
       blocks `javascript:` here and `noopener` is set on the payment open).
       **The `payment_url` half of this guard is cross-app, not web-only**
       (native apps have no OAuth `redirect_url` flow, only the `payment_url`
       open): `fauna_core::subscription::is_safe_payment_url` (UniFFI-exported)
       is the single native-app definition, found 2026-08-10 to have drifted —
       tui duplicated it at two call sites instead of sharing it, linux and
       windows each guarded only the feed-side `payment_url` open and missed
       the profile-side one, and apple had no guard at all — fixed the same day
       across tui/linux (verified) and apple (native-toolchain build-verified
       2026-08-10 on macOS: `just apple-ffi` regen + `mac-debug`/`swift-test`
       360/360 green, the generated `isSafePaymentUrl(url:)` signature matching
       the call site exactly, no toolchain delta); windows' leg stays
       source-verified against generated UniFFI bindings pending its own
       native-toolchain build on Windows.
       **part (a-i) — done:** the `fauna_node_url` localStorage override is
       validated by `#lib/safe-url.ts` `isSafeNodeUrl` before `#lib/api.ts`
       `nodeUrl()` honours it — accepted iff it parses as an absolute `https:` URL
       (the production / LAN-TLS nest scheme) or an `http:` URL to a loopback host
       (`localhost`, `*.localhost`, 127.0.0.0/8, `::1` — the browser
       secure-context dev/e2e carve-out: `just web-dev` and the e2e suite serve the
       nest at `http://127.0.0.1:<port>`). Any other value — notably `http:` to a
       non-loopback host (the phishing/MITM-relay residual), or a malformed /
       relative / `javascript:` / `data:` string — is ignored and `nodeUrl()`
       falls back to `window.location.origin`, so a spoofed override can't silently
       steer the client to an attacker-controlled origin.
       **part (a-ii) — done (2026-06-26):** web adopts the SSH `known_hosts`
       model. The silent-challenge launch path (`fauna.auth.verify`) now carries the
       same Axis-1 proof the direct handshake does — `VerifyReply.cert_binding`, the
       nest signing `served_SPKI ‖ challenge_nonce ‖ client_nonce` (reusing
       `auth_handlers::build_cert_binding`; the `client_nonce` is the NT-1 hardening
       described below), symmetric with `HandshakeReply`. On each
       launch the web SPA possession-verifies that proof — **key-possession only**,
       since a browser can't read the served cert's SPKI to do the native channel
       binding — and TOFU-pins `(origin → nest_actor_id)` in localStorage, **warning
       loudly** when a later connect's possession-verified identity *differs*
       (`Changed`) **or** when the nest can no longer present a valid proof for an
       origin it already pinned (`Withdrawn` — downgrade protection, so an attacker
       can't bypass the pin by simply omitting the binding). The warning surface
       (`launch_identity_changed` → `nest-identity-changed-warning` + a "trust
       this nest" re-pin button that forgets the pin and re-TOFUs) is the
       **uniform all-app model**, carried by the shared `LaunchMachine`: a
       changed/withdrawn pin is `SilentChallengeOutcome::IdentityChanged` /
       `TokenRefreshOutcome::IdentityChanged` →
       `LaunchPhase::IdentityChanged { pinned_hex, seen_hex }` (auto-entry
       blocked, bearer dropped, retry refused), and the explicit recovery is
       `LaunchMachine::trust_nest_identity()` — forget the pin through the
       `AuthConnector` trust seam, then re-challenge and re-TOFU. *(Ratified
       2026-07-13; supersedes the earlier "hard connection error, no soft UI,
       web-exclusive warning" stance — the hard error routed natives to a
       retry spinner on a MITM signal with no recoverable affordance.)*
       Native detection stays the connect-time channel binding (full SPKI
       compare — strictly stronger than web's possession-only check); since
       2026-07-13 the native **silent-challenge (launch) path graduates too**
       (`fauna_anon_client::graduate_verify_path` over `served_SPKI ‖
       challenge_nonce ‖ client_nonce` — that exact message and no other since
       2026-09-24, when the compat-remnant sweep removed the challenge-nonce-only
       retry and the downgrade-harvest residual it carried) — before
       that, the launch path minted and *used* a bearer with **no identity
       check at all**, so an impersonated self-signed/LAN nest was spoken to
       until the first token refresh or content connect caught it. The
       pin/compare + possession-verify logic is **shared**
       (`fauna_client_core::nest_trust`: `check_web_nest_identity`,
       `verify_cert_binding_possession`, the `run_pinned_silent_challenge` ceremony the machine's wasm
       connector runs, the `NestIdentityPinStore` trait — `get`/`set`/`remove`,
       `remove` being the user-approved re-trust seam — + `MemoryPinStore` —
       lifted there from `fauna-anon-client` so native and wasm call one core,
       priority #2); only the pin-store backend diverges (web's
       `LocalStoragePinStore` over localStorage — also in `nest_trust`, ONE
       store shared by the SPA's `challenge_verify_inner` path and the
       machine's wasm connector, keyed by the nest URL — vs. native's
       `DiskPinStore`). **The reach hint keys on the domain, never on the
       dial (built 2026-08-30; supersedes the 2026-08-29 carry-over target).** A
       session that reaches a freshly-provisioned box through its reach hint
       (`behavior/onboarding.md` § Reach hint — `https://<ip>` on web, the
       resolve override on native) pins under the **domain**, exactly as a
       domain dial does: natively for free, since `connect_resolving` moves the
       socket address alone and leaves the URL — hence SNI, `Host`, the demanded
       cert and the pin key — the domain's; on wasm by keeping the dial URL out
       of the identity argument, since there the *authority* is what moves. So
       the hint dial and the later domain dial resolve the **same** pin: the
       switch opens no TOFU window and can never read as `IdentityChanged`,
       there is only ever one key per box, and a stale hint pointing at a
       recycled address is refused by the pin the account already holds.
       **The earlier target — copy the pin from the hint authority to the domain
       authority — is retired as unsafe, not merely unnecessary:** it promotes
       whatever the IP dial TOFU-accepted into the pin that guards every later
       domain dial, which is strictly weaker than pinning the domain in the
       first place. **The claim seeds the pin from the wizard's
       possession-proven first-contact root (ratified + built 2026-08-30).** Where a hint dial is the *first*
       contact with a box (the common case at wizard exit, since the domain
       does not yet resolve), it used to TOFU under the domain key — the
       ordinary NT-1 first-contact residual — even though the client had
       possession-proven that box's identity minutes earlier (every pre-claim
       dial graduates against the injected-seed / pasted-URI root) and then
       discarded the proof at wizard exit. Now **every claim success persists
       the held root as the domain's durable pin** through the shared
       `NestIdentityPinStore` seam (`OnboardingMachine::seed_identity_pin_at_claim`,
       called from all three claim-success arms: `claim_provisioned_box`'s
       fresh claim, its already-claimed-ours recovery edge, and
       `wizard_submit_claim_code`'s URI-pinned claim), each arm writing the key
       its launch reader resolves — native `authority_of(nest_url)` into the
       installed store (`fauna_anon_client::trust::seed_claimed_identity_pin`),
       wasm the nest URL verbatim into `LocalStoragePinStore` — so the first
       post-claim dial, hint or domain, **verifies instead of TOFU-ing**, on
       both arms. Native is seeded too, deliberately: the SPKI channel binding
       defeats a *relay*, not a *stranger* — a first-contact hint dial at a
       recycled address would TOFU-pin whatever box answers with a valid
       self-binding on native exactly as on web. The write is authoritative
       (`set`, never `pin_if_absent`): the claim is a user-initiated ceremony
       over a connection that just possession-proved the root — evidence
       strictly stronger than any TOFU pin it replaces — so a "start over onto
       a new box, same domain" installs the new box's root rather than wedging
       the first launch on an `IdentityChanged` the stale pin would force (the
       wizard's own pre-claim dials never consult the pin — a pre-resolved
       root is exact-match — so a stale pin cannot wedge the wizard either).
       Rotation is untouched — and, since the 2026-08-30 same-root-guard
       hardening, that is a
       property of the **write path**, not only the value: a seed whose root
       the pin ALREADY NAMES is a no-op — `set` replaces the whole entry, so
       an unguarded re-seed on the repeat-claim path would erase a
       chain-accepted `rotation_seq` and silently disarm both of
       `evaluate_rotation_bridge`'s fork clauses, downgrading a hard
       `PinForked` to a re-trustable `IdentityChanged`. A fresh seed lands an
       ordinary pin (`rotation_seq = None`) that `try_rotation_repin` bridges
       like any other, and a DIFFERING root still overwrites authoritatively —
       the old chain's seq belongs to the old box. Pinned by
       `fauna-anon-client`'s
       `a_reseed_of_the_same_root_keeps_the_chain_accepted_seq`
       (red-verified against the guard removed) on native; the wasm arm's
       identical guard property is witnessed separately, through a real
       browser claim rather than `FakeNestApi`, by
       `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py`
       (red-verified 2026-09-13 by making its `store.set` unconditional,
       which reddens exactly the same-root case's `rotation_seq`
       assertion). What remains TOFU is exactly
       a claim with **no** held root — a
       bare typed claim code, or a resumed slot from before `nest_actor_id`
       existed — where nothing was proven and nothing may be invented. Pinned
       by `fauna-onboarding-machine/tests/claim_seeds_identity_pin.rs` (seed,
       no-root, stale-overwrite, and recovery-arm cases; red-verified against
       the seeding removed).

       **What the web pin does and does NOT catch (NT-1; reviewed 2026-06-26,
       tracked internally).** Web does
       **possession-only** verification — a browser can't read the served cert's SPKI,
       so there is no received-cert compare (the leg that gives the *native* channel
       binding its anti-MITM strength). The pin therefore rests entirely on nonce
       freshness, and it does **not** catch an attacker who has broken WebPKI for the
       origin (the spec's own premise — without a browser-valid cert the TLS handshake
       fails and no wasm runs) **and** is willing to obtain one genuine binding: such
       an attacker presents the **real** `nest_actor_id` via a binding **live-relayed**
       from the real nest (the **offline-harvested** binding — any one registered
       actor yielding a reusable, actor-independent, non-expiring binding — was the
       *legacy* nest's shape, one that signed over the server-chosen `challenge_nonce`
       alone; a client that folds its own nonce accepts no such binding since
       2026-09-24) — so the pin matches and does *not* warn. The earlier draft of
       this section claimed the pin "catches a DNS-hijack + attacker-valid-WebPKI-cert
       where the attacker lacks the nest deployment key"; that is **false** — an
       attacker lacking the deployment key still presents the real `nest_actor_id` via
       a harvested or relayed genuine binding. The pin's genuine residual value is
       narrow: (1) **benign** identity-change detection — it warns on a legitimate
       redeploy / key rotation (a different `nest_actor_id` → `Changed`); and (2) it
       catches only the **lazy** attacker who breaks WebPKI but presents *no* binding
       (`Withdrawn` — downgrade protection) or *their own* key (`Changed`). Against a
       network attacker willing to relay one binding it adds **no** meaningful
       protection over plain WebPKI. This *broadens* — does not contradict — the
       phishing/MITM-relay residual below: the uncaught case is a live
       TLS-terminating relay (an offline harvest sufficed only against the retired
       legacy shape).

       **A refused proof and an absent one collapse to the same outcome when
       unpinned — declared, accepted (reviewed 2026-08-26, tracked internally).**
       `check_web_nest_identity`'s `(pinned=None, seen=None) → Unprovable, proceed`
       row does not distinguish *why* `seen` is `None`: a plaintext/dev nest that sent
       no `cert_binding` at all reads identically to a nest that sent one whose
       signature failed to verify (`fauna_client_core::nest_trust`'s three web call
       sites all discard that `Err` into `None` via `.ok()` before it reaches the
       check). Once a pin exists this does not matter — `(pinned=Some, seen=None)`
       always warns (`Withdrawn`), refused or absent alike — the gap is narrow to
       **first contact with no prior pin**. This is the same reasoning as the
       former DNS/TOFU kind-rejection residual above (§ *Compat*, retired
       2026-09-24 with the rest of the transition remnants): with no root to authenticate
       *any* first-contact signal against, a corrupted proof is exactly as cheap for
       an active attacker to produce as an absent one, so refusing on it buys nothing
       against this section's own threat model while growing web a new hard-fail
       surface on a connection that is unauthenticated by construction. Accepted as a
       residual rather than closed; pinned by
       `fauna_client_core::nest_trust::tests::a_refused_verify_path_binding_collapses_to_unprovable_when_unpinned`.

       **Per-site witness status (added 2026-09-02).** The pin above covers the *rule* in isolation — it
       calls `verify_cert_binding_possession`/`check_web_nest_identity` directly and
       never drives a production call site, so it cannot see a regression at any
       of the three. One of the three now has a call-site witness; two are
       **declared absences**:
       - **`fauna_client_core::nest_trust::run_pinned_silent_challenge`** (the
         launch machine's wasm connector's entry point) — witnessed by
         `nest_trust::rotation_tests::the_web_launch_path_pins_the_identity_the_opening_read_proved`,
         which drives the real function against a mock nest serving a genuine
         opening proof and a present-but-forged verify-reply `tagged_sig`, and
         asserts the `Success` outcome and that the pin names the identity the
         **opening read** proved. **Since the login nest binding (2026-09-23,
         [`../behavior/login.md`](../behavior/login.md) § Binding the nest) the
         web verdict rides that opening read, not the verify reply:** the
         identity is possession-verified over a fresh client nonce and checked
         against the origin's pin *before* the login is signed, and a corrupted
         or absent opening proof is a hard refusal — no identity, no login
         signature — so the residual above is structurally closed on the login
         path; the verify reply's own proof is no longer consulted on web (it
         still is natively, where `graduate_verify_path` pins the served SPKI
         for the bearer connection).
       - **`fauna-wasm`'s `challenge_verify_inner`** (`libs/fauna-wasm/src/lib.rs`; its
         former sibling `mint_bearer_inner` was deleted 2026-09-23 when the SPA's re-mint
         moved onto `challengeVerify`) — **declared absence, not oversight.** It calls a
         live `fauna_rpc_wasm::AnonymousWsRpcClient`
         concretely (unlike `run_pinned_silent_challenge`, which is generic over
         `RpcRequester` precisely so a mock nest can stand in); reaching the
         refused-proof arm through either wasm32-only function needs a live
         WS-RPC mock reachable from a `wasm-bindgen-test` browser run (`libs/fauna-wasm`
         already runs pure-logic `#[wasm_bindgen_test]`s, e.g. `mint_key_blob_inner_roundtrip`
         in `tests/upload_sidecar.rs`, but none of them drive a live connection —
         no such harness exists in this crate today) or a refactor to inject the
         requester the way the native path does. Either is real, follow-on-worthy
         infrastructure work, not a witness — out of scope for this residual.
         Cross-referenced inline at both call sites.

       **NT-1 hardening (client-nonce fold) — done.** To shrink the residual to the
       handshake path's already-accepted *live-relay* case, `VerifyRequest` carries a
       fresh **client**-chosen `client_nonce` and the nest folds it into the binding —
       `served_SPKI ‖ challenge_nonce ‖ client_nonce` (symmetric with the handshake
       path's client-chosen nonce; `auth_handlers::build_cert_binding`). A binding
       harvested under one client nonce no longer possession-verifies against a
       victim's fresh nonce, so the *offline* harvest is closed against a current nest
       — only a *live* relay remains. A client that folds its nonce verifies the
       3-part message and nothing else: the challenge-only retry that once accepted a
       legacy nest's `served_SPKI ‖ challenge_nonce` binding (the
       verify-path fallback) and its
       residual *downgrade*-harvest (an attacker harvesting a 2-part binding by
       omitting its own client nonce) were removed 2026-09-24 by the compat-remnant
       sweep. The master secret
       never leaves the browser regardless (`challengeVerify` signs in
       wasm; only the signed bearer is sent), so the residual is impersonation, not
       raw-key theft. (The bearer re-mint runs the same possession check — see
       § Post-auth surfacing, channel 1. Since 2026-09-23 it IS this ceremony
       (`getAuthToken` → `challengeVerify`, `login.md` § When to use which); from
       2026-08-19 until then it was a direct handshake verifying over that
       ceremony's own client nonce. The *launch* identity pin rides the same
       challenge/verify, where the warning can surface on the launch screen
       before the app mounts.)
       **Implementation status today:** built — the identity pin runs on the web
       silent-challenge ceremony (`challenge_verify_inner`,
       `libs/fauna-wasm/src/lib.rs`) — launch and bearer re-mint alike — through one verdict
       helper (`fauna_wasm::nest_identity::check_pin_and_maybe_repin`) and all
       backed by the shared `fauna_client_core::nest_trust` core; tier_2 web e2e
       covers the warn-and-recover wiring, and the tier_3
       `test_nest_identity_pin_post_auth.py` covers the mid-session verdict. The NT-1 client-nonce hardening is also built —
       `VerifyRequest.client_nonce` (additive wire) + the nest fold in
       `auth_handlers::build_cert_binding` + the native client's `fauna_anon_client::graduate_verify_path`
       (one leg per request), pinned on that production door by
       `a_two_part_verify_path_binding_is_refused_by_the_production_door` (a 2-part
       binding, and a binding harvested under another client nonce, are refused by a
       client that folded a nonce). The transition residual
       — a *downgrade*-harvest against the legacy fallback — closed 2026-09-24 when
       the fallback was dropped. The **shared `LaunchMachine` identity-pin seam is built**
       (2026-07-13): `SilentChallengeOutcome::IdentityChanged` /
       `TokenRefreshOutcome::IdentityChanged` → `LaunchPhase::IdentityChanged`
       + `trust_nest_identity()`, native launch-path graduation
       (`graduate_verify_path`) and the wasm pinned ceremony
       (`nest_trust::run_pinned_silent_challenge` over the shared
       `LocalStoragePinStore`), `fauna_ffi::forget_nest_identity_pin`, all
       transition-tested in `fauna-launch-machine/tests/{silent_challenge,
       token_refresh}.rs`. **Consumed by web (2026-07-13)**: web's
       silent-challenge launch row now runs through `LaunchMachine::start()`
       (`routes/onboarding/+page.svelte`), so `launch_identity_changed` renders
       off `LaunchPhase::IdentityChanged` and the trust button calls
       `trust_nest_identity()`. Web's own pre-machine classifier is deleted; the
       pin store is unchanged (`LocalStoragePinStore`, `fauna_nest_pins`, keyed by
       nest URL), so what is pinned and what verdict a pin produces are the same
       before and after — the tier_3 `tests/test_nest_identity_pin.py`
       warn-and-recover journey (its `[web]` arm) is the regression pin. The `mintBearer` re-mint
       that used to follow the launch is gone too: the machine's already
       pin-verified bearer is handed to the SPA's token cache
       (`api.ts primeTokenCache`), so the app no longer mints a second, *unpinned*
       bearer on its first request.
       **Native, today:** `linux` + `tui` render the **full** surface for
       `LaunchPhase::IdentityChanged` — the localized
       `onboarding.launch.identity_changed_warning` in its own
       `nest-identity-changed-warning` element, the re-trust button
       (`nest-identity-changed-trust-button` → `trust_nest_identity()`) and
       `launch-fallthrough-button`, with **no retry CTA**
       (`apps/fauna-linux/src/views/launch.rs` § `IdentityChanged`;
       `apps/fauna-tui/src/launch.rs`). *(Until 2026-07-13 both fell into their
       launch-phase catch-all and rendered a `TransientRetry` surface: a **dead**
       Retry button — `retry_silent_challenge()` no-ops outside
       `Offline{transient:true}` — over a raw Rust debug dump. That is fixed; a
       retry CTA on a possible-MITM signal is precisely what this section
       forbids.)*
       **`android` (2026-07-13): the full uniform surface**, matching web — a
       new `AppLaunchVM.NavTarget.IdentityChanged` arm (`ui/viewmodel/AppLaunchVM.kt`)
       + `LaunchIdentityChangedScreen.kt` render the localized warning
       (`nest-identity-changed-warning`), the re-trust button
       (`nest-identity-changed-trust-button` → `AppLaunchVM.trustIdentity()` →
       `machine.trustNestIdentity()` on the *same* `LaunchMachine` instance that
       produced the verdict, not a fresh one) and `launch-fallthrough-button` — no
       retry CTA. Before this landed, `LaunchPhase.IdentityChanged` had **no**
       `when` arm at all in `AppLaunchVM.navTargetFor`, a non-exhaustive-`when`
       *compile error* that blocked all of android from building (discovered
       2026-07-13 rebasing the android Nests-trust-facet development branch
       onto the already-widened ui.yaml + shared enum).
       **`macOS` + `iOS` (2026-07-13): the full uniform surface**, completing the
       seven-app set. Apple was the last app whose launch never called the
       shared machine at all — it hand-rolled the four-case routing table in Swift,
       so `LaunchPhase::IdentityChanged` was not merely unrendered but
       *unreachable*, and a changed pin fell into the bespoke classifier's
       catch-all and painted the **transient retry** surface: the same dead-Retry-on-a-
       MITM-signal shape linux and tui had. Both targets now construct
       `LaunchMachine` over the shared `RegistryLaunchPersistence`
       (`FaunaAccounts.bootLaunchPersistence()`, from `FaunaMacApp.runLaunch()` /
       `FaunaApp.runLaunch()`) and render the phase through one **shared FaunaKit**
       `LaunchIdentityChangedView` — warning, re-trust button
       (→ `trustNestIdentity()` on the *held* machine, not a fresh one) and
       `launch-fallthrough-button`, no retry CTA. The trust button deliberately
       carries no `.keyboardShortcut(.defaultAction)` either: making "trust this
       nest" the Enter-key default on a possible-MITM warning is the same silent
       re-pin this section forbids, by another name. The fallthrough does **not**
       forget the pin — walking away from an untrusted nest must not un-pin it.
       **E2E, today:** the warn-and-recover journey is pinned by **one cross-app
       module** `tests/test_nest_identity_pin.py` (web + tui + linux + **macOS +
       iOS**; the separate
       web module merged in on 2026-07-15 once web grew a `common.launch_harness`
       leg — web has no `app_path`, and its only state-preserving relaunch is a
       `hard_reload()`, so a `teardown()` discards the whole browser profile). The
       arm is the **Changed** arm on every app (a pin a proven identity
       *disagrees* with); only how the identity is proven differs: **tui** +
       **linux** + **macOS** + **iOS** ride `self_signed_nest`'s SPKI-compared
       binding, and **web** rides its plain-HTTP origin, whose cert-optional
       identity binding (`fauna.auth.nest_handshake`) every web channel reads
       before signing (`../behavior/login.md` § Binding the nest) —
       possession-only, so after "trust this nest" web re-pins the genuine
       identity exactly as native does. (Web reached the **Withdrawn** arm — a pin
       that can't be *confirmed* — until that binding landed on 2026-09-23; the
       e2e assertion was corrected 2026-10-12. Withdrawn stays unit-pinned in
       `fauna_client_core::nest_trust`.)
       **web, tui, macOS and iOS pass; linux skips headlessly** — the seeded pin must
       survive a
       relaunch (the native drivers otherwise hand a *fresh* store, so the harness
       calls `driver.preserve_state_across_relaunch()`), and linux's real-keyring
       backend needs an unlocked desktop Secret Service, whereas tui's, macOS's and
       iOS's file backend and web's localStorage are headless-safe.
       **apple joined 2026-08-01**, and needed no new harness *mechanism*: the
       `AppleFileCredStore` adapter, `make_launch_harness`'s native leg, both apple
       drivers' `preserve_state_across_relaunch()`, and the value-returning
       `machine_method_result` reader path the pin *reader* half needs were all
       already built — joining was the module's client list plus iOS's `ios_setup`
       fixture shape. Only the self-signed nest was new ground for apple, and it
       needed no cert plumbing: `self_signed_nest` binds `127.0.0.1`, so both apple
       clients accept it on the **loopback** branch of the trust posture above.
       ⚠ **One real harness bug surfaced, and it is the durability trap to know:**
       iOS's `preserve_state_across_relaunch()` pinned only the *credential dir*
       while `drivers/ios.py::launch()` kept `simctl uninstall`-ing the app data
       container — so the identity trio survived a relaunch but everything rooted in
       `Library/Application Support` did **not**, including the E2E pin store
       (`FaunaKit` `NestTrust.installPinStore()` roots the e2e launch at
       `<Application Support>/Fauna`). The method returned `True` while keeping half
       its promise, which reads as a product bug at the assertion. Fixed by
       `_preserve_container` (skip the uninstall when a test pinned state; `simctl
       install` upgrades in place and keeps the container) — the iOS counterpart of
       macOS's `_preserved_home`. Any iOS test asserting at-rest durability across a
       relaunch (MLS db, SwiftData, pins) depended on this.
       ⚠ **The same trap a third time, on tui-on-macOS (found + fixed
       2026-09-21).** tui's `DiskPinStore` rests at `install_scoped_trust_home()`
       (§ Pin custody across processes rule 1): `$XDG_CONFIG_HOME/fauna` on linux,
       under the `xdg_base` its `preserve_state_across_relaunch()` always pinned —
       but `<HOME>/Library/Application Support/Fauna/trust` on macOS, where every
       launch relocates HOME into a throwaway dir the pin never named. The seed
       assertion passed (same process, same HOME); the relaunch met an empty trust
       dir, TOFU-pinned afresh and entered as if nothing had changed, so the
       missing **Changed** arm read as a launch-routing bug (`count=0`, never
       rendered). The leg's only recorded green was linux's, and on macOS it could
       not have passed since the 2026-08-02 store move — until the driver pinned
       `home` too (`drivers/tui.py` `_relocates_home` / `_remember_launch_store`,
       the tui counterpart of macOS's `_preserved_home`).
       ⚠ The native cases **must** ride the TLS-serving `self_signed_nest` fixture:
       both native graduation points are scheme-gated
       (`fauna-launch-machine/src/auth.rs` and `fauna-anon-client/src/bearer.rs`,
       each `if nest_url.starts_with("https://")`), so against a plain-HTTP nest a
       pin is **never consulted** and this surface is simply unreachable. Both
       modules seed the pin through the one shared E2E-bridge name
       `set_nest_identity_pin_for_test`, which writes whichever pin store the client
       installed (`onboarding.md` § E2E bridge contract).
       **`windows` (2026-07-13): the full uniform surface, completing all seven
       apps.** `App.xaml.cs:DispatchLaunchSnapshotAsync`'s `LaunchPhase.IdentityChanged`
       case navigates to `Views/LaunchIdentityChangedPage.xaml` — the localized warning
       (`nest-identity-changed-warning`), the re-trust button
       (`nest-identity-changed-trust-button` → `machine.TrustNestIdentity()` on the same
       machine instance that produced the verdict) and `launch-fallthrough-button`, no
       retry CTA — the same lift linux/tui/android/apple already did. Rendering-wise every
       app is done; **e2e-verification-wise the `cred_store.py` adapter gap is CLOSED**
       (`tests/common/cred_store.py` wires `WindowsFileCredStore` since 2026-08-09 and
       `AppleFileCredStore` for macos/ios — `make_cred_store` covers six of seven apps
       (web, linux, tui, macos, ios, windows; `cred_store.py:389`'s `raise ValueError`
       is the unhandled-client fallthrough); android has no host-side adapter by
       design — its credential file is written on-device by the bridge with no
       host-readable path (`attach_cred_store`'s `NotImplementedError`,
       `cred_store.py:422-429`) — the "only linux/tui/web are wired" framing here was
       stale by the time row 87 read it, 2026-08-26).
     - **Post-auth surfacing (§ Post-auth surfacing, ratified 2026-07-23):
       the shared taxonomy is DONE (2026-08-15); linux (2026-07-23), tui
       (2026-07-30), macOS/iOS (2026-08-01), web (2026-08-19), android
       (2026-08-21) and windows (2026-08-26) route the verdict — all 7
       apps.** All seven apps render the launch surface and now route a
       *post-auth* verdict to it. Windows' leg is
       `App.RunTtlRefreshLoopAsync` — the
       app's one post-auth "keep the bearer fresh" cadence, the same universal
       hook other apps' legs route through: its per-iteration phase check now
       catches `LaunchPhase.IdentityChanged` (parked there by
       `LaunchMachine.RefreshToken()` rather than thrown) and re-enters
       `DispatchLaunchSnapshotAsync` on the SAME `LaunchMachine` instance that
       produced the verdict — never a synthesized one, since `TrustNestIdentity()`
       reads secret + nest_url off that specific instance and no-ops on any
       other. **windows' e2e leg is CLOSED (2026-08-31)**: `TestAgent.cs`'s `call_machine_method` case falls back to a
       throwaway `OnboardingMachine` for `set_nest_identity_pin_for_test` /
       `nest_identity_pin_for_test` specifically (the two names the shared
       `call_machine_free_method` dispatcher answers with no machine at all —
       every instance's dispatcher tries them first regardless of `self`) when
       `OnboardingViewModel.Current` is null post-auth, and a new `silent_sign_in`
       command drives `App.RunSilentSignInForTestAsync` — `LaunchMachine.RefreshToken()`
       on the SAME live machine `RunTtlRefreshLoopAsync` holds, then the identical
       phase-check-and-dispatch the loop's own tick performs.
       `test_nest_identity_pin_post_auth.py`'s `_POST_AUTH_APPS` now includes
       windows. Android's leg is
       `AppLaunchVM.performPostAuthSilentSignIn()`, called at the same
       universal post-auth hook `FaunaNavHost` fires the mail/CalDAV/backup
       glue from: catches `FfiException.NestIdentityChanged` from
       `silentChallenge`, tears the session down without erasing credentials
       (`ApiClient.clearAuth()`), and re-enters the launch flow via
       `appState.isOnboarding = true` — the same reconnect-no-relaunch path
       `AccountSettingsVM.switchAccount` already uses, so the SAME
       `LaunchMachine` singleton re-derives the verdict and the trust button
       has a live machine behind it. ⚠ **e2e is not wired**: android's
       `androidTest` bridge has neither the launch-time
       `set_nest_identity_pin_for_test` seam nor a `silent_sign_in` agent
       command yet (`test_nest_identity_pin_post_auth.py`'s `_POST_AUTH_APPS`
       still excludes android), and android e2e is emulator-host-gated on this
       fleet regardless — the code leg is done, the test leg is tracked
       separately.
       **web is the one app that covers TWO channels, and the only one whose
       leg is not a port of the native glue.** `fauna-anon-client` is
       native-only (its wasm twin is `fauna_rpc_wasm::AnonymousWsRpcClient`),
       so none of the four seams the shared taxonomy leg fixed exist on wasm;
       web instead runs the check at both of its own re-check points, over the
       browser-side possession primitive (`verify_cert_binding_possession` —
       no received-cert compare, since a browser cannot read the served SPKI):
       the **bearer re-mint** (channel 1, the handshake then — closing the
       "the `mintBearer` path ignores its `cert_binding` reply"
       residual, which is why web could not detect a re-mint-channel change at
       all) and the **background silent challenge** (`fauna.auth.verify`,
       channel 3). Both share one verdict helper
       (`fauna_wasm::nest_identity::check_pin_and_maybe_repin`, rotation-repin
       bridge included) so the two channels cannot drift. Web's handler is the
       apple shape rather than linux's: drop the bearer cache, count the
       teardown, and re-enter the real launch flow by navigating to the
       onboarding route — never a surface painted in place, because the
       re-trust button drives `trustNestIdentity()` on the `LaunchMachine` that
       produced the verdict and a synthesised phase would render an identical
       surface whose button did nothing.
       **e2e coverage of web's channel 1, closed 2026-08-22
:** a
       new web-only `bearer_force_refresh` command drives the real
       `getAuthToken(secret, undefined, true)` re-mint over the SAME
       plain-HTTP harness `test_nest_identity_pin_post_auth.py` already
       uses for web's channel 3 — plain HTTP serves the cert-optional
       identity binding the re-mint reads before signing, so a poisoned pin
       is *contradicted* by a possession-proven identity (the Changed
       verdict, possession-only), the same verdict class reaching the same
       surface. **Ruling on the SPKI-compared Changed arm (real TLS) for
       web e2e: not pursued, and not needed.** Reaching it would
       need a TLS-terminating upstream leg in the e2e harness's SPA proxy
       (`tests/e2e-unified/conftest.py`'s `_proxy_websocket`/
       `_proxy_to_nest`, today a bare `socket.create_connection` + an
       unverified `urllib` context) — real harness work, not a browser-cert
       question, since the browser only ever talks to the plain-HTTP proxy.
       It buys no new coverage: a browser compares no SPKI whatever the
       transport, so `check_pin_and_maybe_repin` runs the same
       possession-only Changed path over TLS as over plain HTTP (the
       re-pin tail is asserted by `test_nest_identity_pin_post_auth.py`'s
       `repinned is not None`), and the SPKI-compared Changed arm itself is
       already proven — over the real `self_signed_nest` fixture — by the
       other four wired native apps.
       **The classification rule is SHARED** — the identity verdict is the
       ONE outcome that escalates, every other failure class stays swallowed
       — as `fauna_launch_machine::classify_silent_challenge` →
       `SilentSignInVerdict`, with its unit pins beside it. It was app-local
       on linux until tui became the second consumer; the remaining five
       consume the shared fn rather than re-deriving the rule, which is what
       stops it drifting per-app. `SilentSignInVerdict` is a four-variant
       enum rather than a `Result` on purpose: the identity verdict must be
       reachable only through its own arm, since folded into an error it is
       indistinguishable at the call site from a network blip — exactly how
       it used to be swallowed.
       **Both legs then share one shape:** the verdict reaches the app loop
       as a dedicated payload-free `DataMessage::NestIdentityChanged`, never
       the error/toast path; the handler tears the session down **without
       erasing credentials** (nothing is wrong with the identity; the *nest*
       changed — and both exits need it in hand) and **re-enters the real
       launch flow** rather than painting the surface in place, because the
       re-trust button drives `trust_nest_identity()` on the machine that
       produced the verdict and a synthesized phase would render an
       identical surface whose button silently did nothing.
       Per-app detail: **linux** classifies in `client.rs` and tears down via
       `settings::trigger_nest_identity_changed` (pump, backup, sync-agent
       capability, windows destroyed, client `shutdown()` — which is what
       drops the bearer). **tui**'s channel is `session::silent_refresh`, the
       body behind its post-auth background refresh — which until 2026-07-30
       was a `let Success(..) else { return }` that swallowed the verdict
       along with everything else; its handler is `session::sign_out` +
       `launch::start`. **macOS/iOS** go over UniFFI instead of native Rust:
       `APIClient.silentSignIn(secret:)` throws a distinguishable
       `FfiError.NestIdentityChanged` (`libs/fauna-ffi/src/auth.rs`'s
       `silent_outcome_to_ffi`, the same classification linux/tui's
       `classify_silent_challenge` performs, just surfaced as a Swift error
       rather than a Rust enum); `performPostAuthSilentSignIn()` (one-shot at
       authenticated launch, plus the `#if DEBUG` test command's trigger)
       catches only that case and calls the same `tearDownSessionForSwitch()`
       + `runLaunch()` pair account-switch already uses, so the fresh
       `LaunchMachine` re-challenges the still-poisoned pin and lands on
       `LaunchPhase::IdentityChanged` with a live machine behind the trust
       button (avoiding the dead-button trap the design guards against).
       ⚠ **e2e:** `test_nest_identity_pin_post_auth.py` (tier_3) drives all
       three. The machine-free bridge arms (the nest-identity pin seed/read)
       are a shared free dispatcher
       (`fauna_onboarding_machine::call_machine_free_method`) each agent
       consults *before* requiring an onboarding machine (absent post-auth, so
       the pin CAN be re-seeded on a live session), and a `silent_sign_in`
       bridge command triggers the production refresh — the real one, not a
       faked verdict. ⚠ **Not every arm is equally observable:** linux's GREEN
       run is gated on an unlocked Secret Service (its launch injects the
       identity into libsecret), so it SKIPS on a headless box — run it on a
       Linux desktop session or via the real-session path; **tui's and
       apple's stores are file-backed and those arms run green headless**, so
       they're the arms that actually verify this path in CI-shaped
       environments.
       `test_nest_identity_pin.py` covers the *launch*-time surface.
       **The shared taxonomy leg is DONE (2026-08-15).** The bearer re-mint's
       verdict is no longer flattened: `ApiError::NestIdentityChanged` and
       `NestClientError::NestIdentityChanged` carry it — with the same
       `host` + two-fingerprint field set `FfiError::NestIdentityChanged`
       renders — through all four seams that used to stringify it
       (`ws_challenge_bearer::map_anon_err`, `AuthClient::map_api_err`,
       `ws_adapter`'s bearer arm, and `LaunchMachineBearer`, the last being
       the one linux and tui ride). Two consequences worth naming:
       `ClientChannel::connect_error_is_terminal` now stops the reconnect
       loop on it, ending the transient-retry-on-a-MITM-signal the plumbing
       rule describes; and `fauna-ffi`'s single `stringify` helper raises the
       verdict instead of `FfiError::General`, so every authenticated FFI
       call routes it without a per-call-site decision. **Which failure IS
       the verdict is one shared table**, `fauna_anon_client::
       classify_identity_changed` (pin changed / rotation-chain fork / a
       binding withdrawn on a host that has a pin) — lifted out of the launch
       machine when the mint became its second consumer, so the mint and both
       launch-machine channels cannot drift apart.
       **Still open:** per-app glue ×1 — windows — on that variant (or the
       machine's `TokenRefreshOutcome::IdentityChanged`), drop the bearer and
       navigate to the existing `launch_identity_changed` surface; linux's,
       tui's and android's legs are the reference, and the step is now mostly
       the handler, since both the classifier and the taxonomy are shared.
       (Web and android are both done — web's leg additionally had to add the
       possession check its direct `mintBearer` re-mint was skipping, before it could route the
       verdict at all.) Tracked internally as a cross-app track.

  (The former "residual HTTP surface" gap — the reqwest content API + `POST /register`
  still honouring `FAUNA_INSECURE_TLS` — is **closed**: that leg is now SPKI-pinned
  to the WS handshake's bound identity, see the Cross-connection-propagation bullet
  above. Byte-bulk content stays HTTP/1.1 (Bucket C — `transport.md` § HTTP residue;
  it is *not* migrated to WS-RPC, just secured), but it is no longer MITM-open.)
