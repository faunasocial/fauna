# Nest TLS certificate lifecycle & selection

Owns: acme, dane-tlsa, cert-lifecycle, ip-bridge-cert
Status: ratified
Authority: server-side TLS — which cert the nest serves per SNI (self-signed floor + valid-else-floor resolver), how a trusted cert is acquired (tiered: nest HTTP-01, client-driven DNS-01 managed/manual/CNAME-delegated), and how it is kept alive (renewal lead, self-heal + rate budget, at-risk nudge, auto-renew decision), plus the cert-reality coupling rules (published MTA-STS mode, self-signed-MX TLSA); defers client-side trust to architecture/security.md § Transport trust, DNS mechanics/credential to behavior/dns-management.md, per-domain record bodies + cert-mode SAN topology to behavior/mail-multidomain.md, floor write-triggers to nest/domains-and-tls-bootstrap.md, bridge TlsCertBlob seal/fan-out to behavior/mail-bridge-lifecycle.md.

Design ratified with the user 2026-06-05 and since **built end-to-end** (see
§ Implementation status today). This doc is the home for the server-side
cert-selection, acquisition, and keep-alive policy its neighbours assume; the
detailed per-doc splits live in § Relationship to neighbouring docs. On
conflict in another doc's domain, raise it.

## Goal

A nest **always serves a certificate** — there is never a dead-TLS / no-cert
state on any listener — and it serves the **best trusted certificate obtainable
for the requested name**, falling back to a self-signed certificate only when no
trusted certificate is currently obtainable. This is transparent to native Fauna
clients (which authenticate the nest's *identity*, not its cert — `security.md`
§ Transport trust) and does not silently degrade the three audiences that
*cannot* authenticate a Fauna identity: the **web browser**, generic **MUAs**
(IMAP/CalDAV), and **inbound SMTP** senders.

## The four audiences (why self-signed alone is insufficient)

A nest's TLS is consumed by four audiences with four different trust mechanisms.
Self-signed fully satisfies only the first:

| Audience | Trust mechanism | Self-signed OK? |
|---|---|---|
| Native Fauna apps | identity-pin via channel binding (`security.md` § Transport trust) | ✅ — the cert is just an encrypted tunnel; identity proves the peer |
| Web app (browser) | WebPKI only | ❌ — needs a CA-issued cert (no click-through in a wasm SPA's fetch/WS) |
| MUAs (IMAP / CalDAV) | WebPKI, or click-through | ❌ — self-signed re-prompts the user on every cert change |
| Inbound SMTP (e.g. Gmail under MTA-STS enforce) | WebPKI, or **DANE/TLSA** | ❌ — a non-WebPKI MX is **refused** under MTA-STS `enforce` |

So the two layers compose: **self-signed is the robustness floor** (never-dead
TLS + native-app transport); **a trusted cert is the goal** for the browser /
MUA / inbound-MX audiences. Neither replaces the other.

## The two-layer model: self-signed floor + trusted-cert goal

Every nest maintains **both** at all times:

1. **A self-signed floor** — always live, auto-renewed, stable-key. Guarantees a
   listener never has *no* cert and gives native apps a working tunnel even
   when no CA cert exists or a renewal lapses.
2. **A trusted cert per served name** — acquired (tiered, below) when obtainable.
   The per-SNI resolver serves the trusted cert when it is valid and covers the
   requested name; **otherwise it serves the floor**.

The floor makes a trusted-cert lapse **non-fatal**: native apps keep working;
only the browser / MUA / inbound-mail-trust audiences degrade until a trusted
cert is restored. This non-fatality is what lets the trusted-cert acquisition be
**client-driven** (and therefore occasionally offline) without ever bricking the
Fauna app — see § C.

---

## A. The self-signed floor (per-SNI, always live)

> The floor is written **unconditionally on every nest** — including a
> **domainless** one (no configured domain, reached at any IP). When and why the
> floor is written (vs. *how* the cert works, this section) is owned by
> [`domains-and-tls-bootstrap.md`](domains-and-tls-bootstrap.md).

**Always maintain and auto-renew a self-signed certificate.** The boot bootstrap
writes one (`write_self_signed_bootstrap`, `bins/fauna-nest/src/self_signed_cert.rs`)
and — **built in Phase 2** — the **floor renew task** (`floor_renew_task`) keeps it
fresh unattended, re-synthesizing well before its `notAfter` (30-day lead, matching
the HTTP-01 cadence) and backing off without clobbering if ACME later swaps in a
real cert. (The admin re-call `fauna.bridges.provision_self_signed_cert` →
`synthesize_and_seal_self_signed_cert` remains the separate per-mail-domain path.)

**Use a stable keypair.** **Built in Phase 2:** the floor synthesizes from a
**persisted** keypair (`load_or_create_floor_key` → `floor-privkey.pem`), so the
SPKI changes only on a deliberate rotation, never on a routine renewal. (Before
this track, `synthesize_self_signed_pem` called `rcgen::KeyPair::generate()` on
every synthesis → **every renewal minted a new key → a new SPKI**.) SPKI churn is
tolerable for native apps (the channel binding re-binds whatever cert is
current — `security.md` § Cross-connection binding), but it is costly for two
other audiences:

- **MUAs** re-prompt the user on every key change (the floor's SPKI is what a
  click-through MUA pins).
- **DANE/TLSA** records pin the served key's SPKI; a key change forces a TLSA
  re-publish (§ D), and a stale TLSA hard-fails DANE-validating senders.

So the floor reuses a **persisted self-signed keypair** across validity refreshes:
the SPKI changes only on a genuine, deliberate key rotation, never on a routine
renewal. (The stable key is load-bearing for **both** the MUA-reprompt frequency
and TLSA stability — do not let a renewal regenerate it.)

**Per-SNI: serve a valid trusted cert if present, else a floor cert that covers
the requested name.** The resolver is `MultiDomainCertResolver`
(`bins/fauna-nest/src/acme.rs`); its `cert_for_sni` — **built in Phase 2** — now
performs both behaviors below (before this track it did a bare per-SNI lookup with
no validity/coverage check, falling back to the apex default):

1. **Validity/coverage check.** Before serving a per-SNI trusted cert, check it is
   unexpired (`notAfter`/`notBefore`) and a DNS SAN covers the requested name
   (`cert_valid_and_covers`). If it is missing, expired, or non-covering, fall
   through to the floor.
2. **A floor cert that carries the requested SNI.** Falling back to the bare apex
   default cert gives a web browser *both* a name-mismatch *and* an untrusted-CA
   error. So `cert_for_sni` mints (lazily, cached, signed by the stable floor key
   — `floor_for_sni`/`mint_floor_cert`) a **per-domain self-signed floor** carrying
   the requested SNI as its SAN — the floor then yields only the expected
   untrusted-CA warning, not a spurious name mismatch, and native apps on that
   name still get a working channel-bound tunnel. (The mint is bounded to SNIs the
   resolver already recognizes — those with a registered-but-invalid trusted cert
   — so an arbitrary-SNI flood can't grow the cache. A custom domain that never
   obtained any cert, so has no registered entry, still falls to the apex default
   — a transient cold-start gap until issuance first succeeds.)

   ⚠ **The bound has a second, less obvious consequence, spelled out here because
   it reads like a bug when rediscovered from the code (2026-08-13).** An active
   local **mail** domain is *never* registered at all — `add_domain`'s only
   production callers are the custom-web-domain loop in `web_content/cert.rs` — so
   a secondary's SNI never reaches the per-SNI mint either. Whether that is
   name-clean depends entirely on the **apex default's** own SANs, which splits by
   deployment: on an **ACME-healthy** box the secondary's apex joins the apex ACME
   order (`acme_http01::reachable_mail_domains` → `desired_san_domains`, because a
   secondary is a client entry point), so the default leaf covers it and there is
   **no gap** — this is the designed steady state and must not be re-filed as a
   defect. On a **floor-only** box the floor's SANs are apex-derived
   (`write_self_signed_bootstrap`), so a public secondary is served a cert that
   does not carry its name: a real but narrow residue, persistent rather than
   transient. Both halves are pinned by
   `acme::tests::cert_for_sni_covers_an_unregistered_secondary_only_via_the_default_cert`,
   so a future fix flips a red rather than editing prose.

   **That residue is ACCEPTED, deliberately and with the consumer census done
.** Enumerating who
   actually dials a secondary's own name settles it: **DAV and mail clients never
   do** — the deployment has one MX host and one DAV host, both `mail.<primary>`
   (`fauna_mail::dns::per_domain::build_caldavs_srv_record`'s `caldav_host`, the
   MDA serving every local domain there via SNI), and `mail.<primary>` is in the
   floor's SANs, so the click-through-vs-hard-fail distinction that would have made
   this urgent never arises for them. **A Fauna app does dial it, and does not
   care:** its verifier never name-checks (`AcceptProvisional` pre-identity, then
   SPKI-pinned), because floor acceptance rides the channel binding plus the
   identity root — which § E now publishes per domain. That leaves exactly one
   affected consumer: **a web browser** visiting `https://<secondary>` on a box with
   no trusted cert, which sees two warnings instead of one. Buying that back would
   need a floor-eligible-names seam on the resolver fed by the active local-domain
   set — machinery with a boot-ordering hazard (the floor is synthesized before the
   mail-domain table is necessarily readable) and an SPKI-stability constraint — and
   it is not worth it for that. Revisit only if a *new* consumer starts dialing a
   secondary's own name directly; the deferred per-secondary `mta-sts.<secondary>`
   SAN work (§ B) is the one on the horizon.

**The apex/default cert is the native-app floor.** `acme::ServedCertSpki`
reads the resolver's **default (apex) cert only** and feeds its SPKI to
`auth_handlers::build_cert_binding` (the channel-binding signer). Per-SNI
custom-domain certs serve *web/MUA/SMTP visitors*, not the channel binding — and
the Phase-2 per-SNI floor fallback (above) mints covering certs for *visitor* SNIs
only, so it **never** changes what `current_spki_sha256` reports (still the apex
default; a unit test pins this invariant). So the **apex floor** is what every
native Fauna app rides; keeping it always-live + stable-key keeps the channel
binding stable. (Native apps tolerate even apex SPKI churn by re-binding, so the
stable key is an optimization there, not a correctness requirement —
correctness-critical for MUA/DANE, optimization for native.)

---

## B. Trusted-cert acquisition — tiered (Decision A: 1+2+3 + floor; the always-on scoped acme-dns is DEFERRED)

A trusted cert is acquired by the first applicable tier; if none currently yields
a valid cert, the floor (§ A) covers the name.

1. **HTTP-01** — when the nest is **publicly reachable on :80**. **Needs no DNS
   control at all**, so it covers **registrars without an API** for any public
   nest, and the public nest holds no DNS key. This is the built path
   (`bins/fauna-nest/src/acme_http01.rs`, public-apex; `web_content/cert.rs`, the
   per-domain web-content loop → `MultiDomainCertResolver::add_domain`). **The
   generic HTTP-01 machinery (challenge state, port-80 router/listener,
   `obtain_certificate`, cert inspectors, the persisted retry budget, the
   hot-swap `ReloadableCertResolver`) was lifted 2026-08-21 into shared
   [`libs/fauna-acme-http01`](../front-door.md) — the nest's `acme_http01.rs`
   re-exports it unchanged (no call site moved) and keeps only nest policy
   (SAN derivation, resolve gates, `cert_lifecycle_task`); the crate is now
   also consumed by the `fauna.social` front-door binary
   (`front-door.md` § TLS policy), which defers all mechanics here. The CA is
   Let's Encrypt production (§ ACME settings — constants, not choices); the
   test-IPC **`[acme].directory_url`** override
   points the client at any RFC-8555 CA — a private/internal ACME CA (step-ca), a
   different public CA, or the in-network `pebble` of the tier_4 acceptance. The
   nest's `instant-acme` client trusts the directory's TLS via the **system trust
   store** (`with_native_roots`, honours `SSL_CERT_FILE`), so a non-public CA is
   trusted by adding its root to the OS trust store — no Fauna code change. Keep
   `acme.rs`'s **"no ACME on the private NAT axis"** gate (`build_acme_config`
   force-disables ACME on `NodeMode::Private`) — HTTP-01 cannot validate an
   unreachable A record, so a private nest never wastes attempts on it; the floor
   + DNS-01 cover private nests instead.

   **REUSED-AUTHORIZATION — a re-issue inside the CA's reuse window must not
   re-trigger already-valid challenges.** When `obtain_certificate` re-orders
   shortly after a prior success — canonically a `provision_self_signed_cert`
   clobber immediately followed by the self-heal (`mail-bridge-lifecycle.md`
   § Self-healing), or a renew inside the window — the CA returns the new order's
   authorizations already `valid`. The client **skips** `set_challenge_ready` for
   any authorization that is not `Pending` and finalizes on the reused ones:
   re-POSTing a `valid` challenge is rejected by the CA (*"Cannot update challenge
   with status valid, only status pending"*) and would fail the **whole** re-issue,
   silently stranding the box on the self-signed floor (the tier_4
   `test_domainless_add_domain_acme_serves_mail` assert-4 symptom before this was
   fixed, 2026-07-02). This mirrors the client DNS-01 path
   (`fauna_client_dns::acme_order::collect_dns01_challenges`), which has skipped
   already-valid authorizations since D6.

   **The same "no unreachable name in the order"
   principle gates every *infra-subdomain* SAN.** The apex managed cert's SAN set
   (`acme_http01::desired_san_domains`) is **all-or-nothing**: an ACME order fails
   *entirely* if any one identifier doesn't validate, which would take the apex
   cert down. Two names are in this class today — `relay.<apex>` (the SNI-routed
   P2P relay sidecar's HTTPS name — `../transport.md` § Future directions) and
   `pds.<apex>` (the SNI-routed ATProto PDS bridge's — `../../behavior/atproto-pds-full.md`
   § Wire & process topology); both are listed in `dns-management.md` § Records
   covered. Each is added to the order only when **its service is on AND its name
   actually resolves**: `acme_http01::relay_san_included` (a relay sidecar
   is connected to the nest) and `acme_http01::pds_san_included` (an approved `atproto.pds`
   bridge service user exists), each `&&`-ed with an `infra_host_resolves`
   best-effort A/AAAA pre-check run every cycle that fail-safes to *omit* on any
   miss/timeout. An admin who enables one of these before publishing its `A`
   record therefore never stalls apex renewal; that service just serves on the
   self-signed **floor** (§ A carries `relay.<apex>` and `pds.<apex>`
   unconditionally) until the record resolves, at which point the next cycle adds
   the SAN. The admin DNS matrix surfaces the expected `A` so they know to publish
   it (`dns-management.md` § Records covered). The gates are carried as **named
   fields** of `acme_http01::InfraSans` (never positional `bool`s — a swapped pair
   would gate one service's SAN on another's DNS state) and are shared with the
   at-risk cert nudge (§ Implementation status, At-risk renewal push), so the
   nudge can never flag a SAN at-risk that the resolve-gated issuer declines to
   add.

   **The same principle gates each additional
   local domain's SAN.** The apex managed cert covers the **one** MX host
   (`mail.<primary>` — every domain shares it, `mail-multidomain.md`
   § Architectural rules; there is **no** `mail.<secondary>` name, so
   `desired_san_domains` adds the mail host once, for the apex, never per-domain)
   plus each active domain's **apex** `<domain>` for client reachability
   (`mail-multidomain.md` § Client reachability of a secondary domain). Because
   the order is all-or-nothing, a **secondary** domain's apex joins it only once
   that apex actually resolves to this nest — `acme_http01::reachable_mail_domains`
   pre-checks each active mail domain against the persisted `nest_host_address`
   (strong: the `A`/`AAAA` must *serve the nest's own IP*, which also rejects a
   record pointed at some other host; weak "has any record" fallback when the host
   address isn't persisted yet) and fail-safes to *defer* on any miss/mismatch/
   timeout. **The primary is never gated** (it is the identity apex, the name the
   nest is reached by). So adding a second domain whose DNS is not yet published —
   or is pointed elsewhere — can no longer fail HTTP-01 for the whole order and
   take the **primary's** working cert down with it; the secondary simply serves on
   the self-signed floor until its `A` record points here, at which point the next
   cycle adds its SAN. This gate too is shared with the at-risk cert nudge (so it
   never nudges over a deliberately-deferred secondary). *(The deferred
   `expand_primary` cert-mode dispatcher adds each secondary's `mta-sts.<domain>`
   SAN — § Per-domain MTA-STS cert mode dispatcher, `mail-multidomain.md`.)*
2. **DNS-01 automated (managed mode)** — when an admin **client** holds the
   DNS-provider API credential (`dns-management.md` § Where the credential lives),
   the `DnsManagementMachine` publishes the `_acme-challenge.<domain>` TXT (a
   **new record type it manages**, alongside DKIM/SPF/DMARC/MTA-STS — see § The
   `_acme-challenge` record below) and reconciles renewals. **Needs DNS control,
   not reachability**, so it is the trusted-cert path for private/home nests —
   **and for any nest tier 1 cannot serve**, which is the point below.
   **DNS-01 issuance is built** — the order core runs on both drivers (native
   `acme_order` via instant-acme; the wasm-safe `acme_pure` for web) with managed,
   manual two-phase, and CNAME-delegated orchestration, all pebble-proven
   (§ Implementation status today).

   **DNS-01 IS AVAILABLE TO EVERY NEST, NOT ONLY PRIVATE ONES (ratified
   2026-07-22).** The tiers are ordered by *applicability*, not by NAT axis. A
   **public** nest normally uses tier 1 because it is cheapest (no DNS credential
   at all), but a public nest whose :80 is unreachable — a cloud firewall rule, an
   ISP block, a fronting proxy — has no tier-1 path, and before this ratification
   had no trusted-cert path *at all*: it sat on the floor until a human opened a
   port, which is off-box recovery and therefore forbidden by
   `nest/common.md` § Client-state recoverability. Tier 2 covers it, because the
   admin's client can always reach both its DNS provider and the nest.

   **How the issued cert reaches the nest — one seam, two topologies.** The client
   HPKE-seals the cert to **the identity key of the nest it is for** and delivers
   it over `fauna.tls.publish_cert`. The receiving nest installs it iff it can
   *open* it: **the seal target is the authorization**, so no NAT-axis gate is
   needed or wanted.
   - **Standalone** (one nest, public or private, no pair) — the client publishes
     to the nest the cert is for, and that nest installs it immediately.
   - **Relay + private** — the client publishes to the reachable relay, which
     stores the blob **opaquely** (it is sealed to the peer; the relay can neither
     read nor forge it) and the paired private nest installs it on its next
     namespace-sync pull over the **existing** `lan_cert` channel.

   **Who may sign it: an admin of the installing nest — on both routes (ruled
   2026-09-21).** The seal
   target authorizes the *nest*; it says nothing about the *signer*, and it
   cannot: the target is the nest's published identity key, so anybody can seal
   to it. The installed cert is the nest's listener cert — an admin's setting —
   so the one installer refuses an entry whose signer is not an admin in the
   installing nest's **own** role state, whichever route delivered it. The
   direct route's Admin-class kind already implied this; the relayed route is
   where it bites, because pairing is every user's own act (no admin approves
   it) and the relay stores whatever a paired user's nest pushes. **It fails
   safe:** a nest with no admin yet installs nothing by relay, and an entry
   refused once is not retried behind anyone's back — the pull cursor has
   passed it — so the admin's next publish (a fresh entry) is what installs.
   No off-box step is ever needed: the direct route to the nest itself is
   there all along.

   **The order covers the nest's SAN set, not the clicked domain.** An installed
   client-issued cert becomes the nest's **listener** cert, so ordering only the
   domain whose row the admin clicked would silently *narrow* a working cert on
   every renewal — dropping `mail.<primary>` and pushing every MUA onto the floor.
   So the nest reports its desired SAN set (`fauna.tls.cert_status` →
   `desired_sans`, the same `desired_san_domains` builder tier 1 orders) and the
   client requests it, minus two exclusions it alone can judge: names outside the
   zones its credential covers (an ACME order is all-or-nothing — an unpublishable
   name would fail the *whole* order, the domain the admin asked for included), and
   every sibling name when the domain is **CNAME-delegated** (S6b re-homes only its
   own `_acme-challenge`, so a sibling's challenge has nowhere to go). Unlike tier
   1 this set is **not** resolve-gated: DNS-01 validates by publishing a TXT into
   the zone, so a name with no `A` record yet still validates, and gating it would
   narrow the cert for nothing.
3. **DNS-01 manual (no-API registrar)** — `dns-management.md` manual mode already
   shows each record with a live red/green check. For ACME issuance: surface the
   `_acme-challenge` TXT to paste. **Issuance then works**, but **recurring manual
   renewal every ~60 days is unsustainable**, so offer a **one-time CNAME
   delegation**: `_acme-challenge.<domain>` CNAME → a zone the managed credential
   (or an acme-dns responder the admin runs) controls, after which renewals
   automate from the single manual step. If the admin declines to delegate,
   renewal falls to the **floor** (graceful — native apps keep working) plus
   the at-risk reminder (§ C.4).

   **Delegation-target naming — the re-homing convention (S6b, decided 2026-06-06).**
   The CNAME target is `_acme-challenge.<domain>.<controlled-zone>` — the
   challenge name **re-rooted** under a zone a held credential controls (e.g.
   `_acme-challenge.home.example.com` CNAME → `_acme-challenge.home.example.com.example.net`).
   On each renewal the client publishes the order's `_acme-challenge` TXT at that
   target name inside `<controlled-zone>` (via the covering credential's seam) and
   the CA follows the CNAME — so the renewal becomes a fully-automated
   *managed-style* order with the publish redirected, no further paste. This
   convention is chosen over a reserved `_acme`/`_acme-delegations` subdomain
   because the delegated TXT is the **same** `_acme-challenge.<name>` record shape
   every managed/manual record already uses (`fauna_mail::dns::per_domain::build_acme_challenge_txt_record`),
   just published into a different zone — one record type, one path. The target
   name is **persisted authoritative** in `DnsConfig.delegations`
   (`CnameDelegation { domain, target_name, target_zone }`) — it is what the
   admin's CNAME points at, so a future convention change never orphans an
   already-set delegation. Sub-case (b) — delegating to an **acme-dns responder
   the admin runs** — is out of scope (Fauna shows the CNAME but cannot drive that
   responder's publish); only sub-case (a), a zone a held credential controls, is
   automated.
4. **Self-signed floor** (§ A) when none of 1–3 currently yields a valid cert.

> **Constraint — registrars without an API are never a dead end.** A *public*
> nest uses **HTTP-01 (no DNS at all)**. A *private/NAT* nest on a no-API
> registrar uses **manual DNS-01** (issuance) + **CNAME delegation** (automated
> renewal), or the **floor** (graceful, if the admin declines delegation). The
> `DnsManagementMachine` managed/manual split is exactly this framework.

### Surviving an interrupted manual issuance

Tier 3 is the only issuance path with an **out-of-band human step in the middle**:
the admin leaves the client, pastes a TXT at their registrar, and comes back — and
since 2026-07-29 the propagation gate may then hold the completion for up to 45
minutes. So a manual issuance is *routinely* in flight across events that destroy
process-local state: a page navigation, an app restart, a move to another device.

**The rule: an in-flight manual issuance is client-persisted state, not
process-local state.** Beginning one writes a breadcrumb to
`DnsConfig.pending_manual_issue` (`dns-management.md` § Storage — the
tip-sealed, nest-opaque, synced record the machine is sole writer of):
the domain, the target nest id, and the exact `_acme-challenge` TXT(s) the admin
was asked to publish. The live `Dns01OrderInProgress` is a process-local handle
and is deliberately **not** persisted.

**The ACME account rides along too, in the same write, and this is load-bearing
— not an implementation detail.** `begin`'s `persist_pending_manual_issue` also
sets `DnsConfig.acme_account` to the account the order was just opened
against, the moment it exists — not only on a *successful completion*, which is
the only time the account used to get persisted. RFC 8555 §7.4 scopes a CA's
pending-authorization reuse to the *same account*, so a resume that restores a
fresh account instead of this one can never even be offered the same challenge,
whatever the CA's reuse policy is — this was a real gap (found + closed via the
pebble real-wire proof below, S4b), not a hypothetical one.

Three consequences, all shared-Rust so every app gets them by construction:

- **The paste card re-surfaces.** A machine built with no live order projects the
  breadcrumb onto `snapshot.pending_cert`, so the admin sees the record they were
  told to publish rather than an empty page. Client-side machine lifetime is
  therefore **not load-bearing** — a client may hold one machine per page or
  rebuild it per navigation without changing the outcome.
- **Completion resumes.** `CompleteManualIssueCert` with no live order re-opens a
  **fresh** order for the same (now-restored) account and domain, and proceeds if
  the CA asks for the same challenge value. **Whether that is "the usual case"
  is a CA policy question this codebase does not control** — measured against
  pebble (S4b), which never reuses a pending authorization across separate
  `new-order` calls even for the identical account, so a resume there hits the
  mismatch branch every time. If the value changed, the new record is surfaced
  and persisted and the admin is told to update it, rather than left waiting on
  a token no CA will ever ask about; the freshly re-opened order stays live in
  memory, so the *next* completion attempt (once the admin re-pastes) finalizes
  directly with no further CA round-trip.
- **Nothing is discarded silently.** A *failed* completion keeps both the card and
  the breadcrumb (the error rides `snapshot.error`), so the admin retries against
  the record they already published; only success — or an explicit
  `CancelManualIssueCert` — retires them.

Found live on 2026-07-29 (§ Live issuance): the admin's card read "Checking",
then card and status text both vanished with **no error**, because nothing on
that path throws — the state was never carried across the rebuild, not lost by a
failure.

### The `_acme-challenge` record (the one record type this track adds)

`dns-management.md` § Records covered enumerates the managed matrix (MX, SPF, DKIM,
DMARC, MTA-STS, TLSRPT, optional DANE/TLSA, A/AAAA, advisory PTR). This track adds
**`_acme-challenge.<domain>` TXT** to that matrix, with three properties that make
it unlike the steady-state records:

- **Transient.** It exists only during an ACME order; the value is the order's
  key-authorization digest, published immediately before `acme-challenge ready`
  and torn down after the CA validates. It is **not** a steady-state record the
  red/green page nags about — its absence between renewals is correct, not drift.
- **Published by the same one writer.** It routes through the
  `DnsManagementMachine` (the sole DNS writer — `dns-management.md` § Where logic
  lives) via the `DnsProvider` seam, exactly like every other managed record.
  **Do not build a parallel DNS path.** In managed mode the client publishes it
  automatically; in manual mode the page shows it to paste (or the CNAME
  delegation makes it automatic).
- **Does not violate "no nest holds DNS keys."** The nest never publishes
  `_acme-challenge`; the **admin's client** does (it holds the provider key), then
  the issued cert is sealed back to the nest over namespace-sync (§ B.2). The nest
  is out of the DNS-write path end-to-end.

---

## B-IP. The IP bridge cert — a trusted cert for the box's own public IP, until its domain is live (ratified 2026-08-29)

A nest that boots **domainless** on a **publicly-routable IP** — every
client-provisioned VPS, every hand-deployed internet box — can hold no trusted
cert for a name it does not yet know (§ B is derived-off until the claim names
it), so a **browser** cannot reach it at all: no fetch, no socket, and no
interstitial to click through from inside an app. Native apps do not care (§ A
is what they ride, under the channel binding), but the web app is a browser,
and the whole client-provisioned onboarding flow on web runs pre-claim
([`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 6 *Reaching
the box*). The bridge cert closes that gap without touching identity or the
domainless design:

- **What.** A publicly-trusted certificate whose SAN is the box's own public IP
  address(es) — an RFC 8738 `ip` identifier. Let's Encrypt issues them generally
  since 2026-01-15, IPv4 and IPv6, **only under the `shortlived` profile: 160 h
  (~6 days) of validity**
  ([announcement](https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability));
  validation is HTTP-01 (or TLS-ALPN-01) against the IP itself — DNS-01 does not
  exist for an IP. The nest orders it through the same HTTP-01 machinery as § B
  tier 1 (`libs/fauna-acme-http01`: the port-80 challenge router, which a
  provisioned box exposes — its cloud-init opens `80/tcp`,
  [`../installers/vps.md`](../installers/vps.md)), requesting the `shortlived`
  profile.
- **When (derived, bucket-1 — never a knob).** Enabled iff the nest finds a
  **global-unicast address directly attached to one of its own interfaces**
  (`fauna_core::resolve::is_global_ip` over the interface table — the only case
  an IP identifier can validate, since the CA must reach `:80` on that address);
  a NAT/LAN box has none and is unchanged. The order is placed from first boot,
  before any domain exists, independent of the domain-derived `[acme]` enable of
  § B (`build_acme_config` gains an IP arm; the private-NAT-axis gate still wins).
- **Lifetime — a bridge, not a permanent identity.** Renewed at one third of the
  remaining validity (~every 4 days) **for as long as the deployment has no
  trusted cert for its primary domain**; once the domain cert is live the IP cert
  is left to lapse, and the floor serves IP dials again. The IP is never the
  deployment's identity ([`domains-and-tls-bootstrap.md`](domains-and-tls-bootstrap.md)
  § Goal); a cert for it is worth carrying only while it is the one address a
  browser can trust. Rate budget: ~2 successful orders per box-week while
  bridging, under the persisted failed-validation pacing of § Keeping the cert
  alive (a box whose `:80` is blocked burns its 4/h and waits, as today).
- **Serving.** It is the resolver's **no-SNI default** while valid (an IP dial
  carries no SNI); named SNIs keep the per-SNI rule of § A step 2. The
  channel-binding SPKI (`ServedCertSpki`) follows **the cert a no-SNI dial is
  served** — so it is the bridge while one is installed and the default
  otherwise — and native apps re-bind across the bridge's ~4-day rotation and
  once more when it is dropped (§ A: SPKI churn is tolerable for them). The
  floor stays beneath it, unconditionally (§ A).

  *Corrected 2026-09-02: this bullet previously said the SPKI "follows the default cert as
  it always has" **and** that native apps re-bind across the bridge's rotation,
  which cannot both be true — if the binding names the default, the bridge's
  rotation moves nothing. The premise behind the first half was that native apps
  always carry SNI and so never ride the bridge; that is false by construction,
  since the bridge exists for boxes with **no domain to dial**, which the
  provisioned native path therefore reaches by IP — carrying no SNI (RFC 6066
  forbids a literal address there). Signing the default while serving the bridge
  would hand that dial a binding over a cert it was not served: an unconditional
  identity failure on the one path this section exists to unblock.*

- **What it does NOT authenticate.** WebPKI on an IP proves only that the box
  holds the address — there is no hostname in the cert, and a recycled cloud
  address is HTTP-01-certifiable in seconds. So a client that holds an **Axis-2
  root** (the injected deployment seed on a provisioned box, the pasted
  `fauna://claim` URI on a hand-deployed one) **verifies the channel binding
  against it even when the served cert is WebPKI-valid**; the bridge never
  substitutes for the seed. Before this section, first-contact graduation could
  short-circuit on `webpki_valid` alone, which was safe only because a
  domainless box served an untrusted floor — precisely the premise the bridge
  deletes. Owner of the graduation
  rule itself: [`../security.md`](../security.md) § Transport trust.
- **Who relies on it.** The web wizard's Online poll and pre-claim WS
  connection; the web session's reach hint after the claim
  ([`../../behavior/onboarding.md`](../../behavior/onboarding.md) § Reach hint);
  and any browser a human points at `https://<ip>` (the internet-setup guide's
  "accept the browser's warning" step becomes unnecessary on such a box — a
  follow-on, not this ratification). Trust in the *nest* is unchanged: WebPKI on
  the IP proves only that the box holds the IP; *which* nest it is stays the
  Axis-2 root ([`../security.md`](../security.md) § Transport trust — the
  injected deployment seed on a provisioned box, the pasted `fauna://claim` URI on
  a hand-deployed one).
- **When it cannot be had** (CA outage; `:80` firewalled at the provider; an
  IPv6-only box the CA cannot reach): nothing regresses — the box serves the floor
  as today, native apps proceed, and the web wizard falls back to the manual
  browser exception ([`../../behavior/onboarding.md`](../../behavior/onboarding.md)
  § "Almost ready" surface, *web fallback*).
- **What it announces, and why that is accepted** (recorded 2026-08-29 on a
  security review of this section).
  Every publicly-trusted certificate is logged to Certificate Transparency, so a
  box ordering one **from first boot** publishes its own IP to a public,
  append-only, queryable feed *during its unclaimed window* — and `ip`-identifier
  certs are rare enough to make a CT filter for them small and high-signal.
  Discovery of a fresh box therefore moves from *scan the internet* to *subscribe
  to a feed*. This is accepted, on two grounds: **takeover is not the exposure** —
  the claim code's keyspace under the global attempt throttle is unexhaustible
  on any human timescale (`libs/fauna-core/src/claim_code.rs`, which owns that
  reasoning and carries the arithmetic), so what an attacker gains is the
  ability to *delay* a claim by holding the global bucket open, an
  availability cost
  ([`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 6 already
  treats a failed claim as recoverable through the pending-provision slot); and
  **the alternative is no web onboarding at all** — a browser cannot reach a
  domainless box without this cert, so narrowing the window to "only when web
  needs it" is not available to a nest that cannot know which app will claim it.
  The consequence for the claim code's own documented mitigation is scoped at its
  owner: firewalling the box to your own address while claiming stays available
  on the native path, and is traded away wherever the CA must reach `:80`.
- **Testing.** Pebble implements RFC 8738 IP identifiers, so the tier_4
  acceptance that already runs the nest against pebble can order an IP cert for
  the container's own address; the LE staging environment issues IP certs under
  the same profile for the live suite.

---

## Keeping the cert alive — issuance lifecycle, self-heal, rate budget, switch-back

The nest-side HTTP-01 path (§ B tier 1) is kept alive by cooperating pieces in
`bins/fauna-nest/src/acme_http01.rs` (the generic order/renew machinery
re-exported from the shared `libs/fauna-acme-http01` crate — § B tier 1); the
cert and key land in `acme_dir` as `fullchain.pem` / `privkey.pem` (`/data/acme`
in the Docker image — `installers/docker.md`).

- **Issuance / renewal — `cert_lifecycle_task`.** Polls every 5 min and
  (re-)issues **only** when the on-disk cert is missing, no longer covers the
  desired SAN set (§ B — apex + the shared `mail.<primary>` + the gated
  relay/secondary SANs), or is < 30 days from expiry (`CERT_RENEWAL_LEAD_SECS`)
  — a healthy unchanged cert never burns issuance budget.
- **Hot-reload — `cert_watcher_task`.** Watches `acme_dir` and atomically swaps
  a new chain into the live resolver (existing connections keep the old cert;
  new handshakes get the new one), then converges via `store_acme_material`
  (write PEM + seal/fan out the `TlsCertBlob` to approved bridges — the
  bridge-facing half is owned by `mail-bridge-lifecycle.md` § TLS provisioning).
- **The relay sidecar is told at once.** The relay (`bins/fauna-iroh-relay`) serves this same cert at `relay.<apex>` from a copy it fetched sealed over its sidecar channel, so a change on disk must reach it or it keeps serving the old one — and the common case is not a renewal but the first real cert landing minutes after claim, under a relay already serving the floor — or the claim itself, which gives a relay that has been standing by (handed no cert while the nest had no public name) its first one. `cert_watcher_task` bumps `AppState::relay_cert_changed` on every change it reloads, and `identity_domain_core::apply_primary_identity` bumps it when the nest gets its name; the relay's channel turns that into a `fauna.relay.cert_changed` push, and the relay re-fetches and hot-swaps (no connection drop). The push carries no key material — the sealed fetch stays the only way the cert leaves the nest. A push that is lost costs only promptness: the relay also re-fetches whenever its channel reconnects, every twelve hours, and on SIGHUP. Channel owner: [`../transport.md`](../transport.md) § Future directions.
- **Cold boot — no restart, ever.** The unconditional floor (§ A) means the
  listener has a cert from first listen; under ACME the floor serves first and
  the self-heal below swaps in the real cert the moment issuance succeeds (a
  certless *pending* resolver also exists as the no-floor edge fallback — the
  watcher swaps the first issued cert in with no process restart).

**Self-healing: a self-signed cert auto-upgrades to a real one (no admin
action).** A self-signed cert on disk — the boot floor, or an admin-synthesized
`provision_self_signed_cert` — is a *stopgap* wherever ACME can issue. The
lifecycle task treats a self-signed on-disk cert (`acme::pem_is_self_signed`,
issuer == subject) as "needs a real cert" and obtains one on its next poll,
**overwriting** the self-signed PEM (obtain-then-write, never delete-first) so
the self-signed keeps serving until the real cert lands — no outage, no admin
step, no client RPC. A deployment that genuinely wants a self-signed cert has
ACME derived-off (`build_acme_config`), so the task isn't running and the cert
is left untouched. One correctness prerequisite is the REUSED-AUTHORIZATION
skip (§ B tier 1) — a re-issue inside the CA's authorization-reuse window must
finalize on already-`valid` authorizations, not re-trigger them.

**The rate budget is real-limit-aware and survives restarts.** On issuance
*failure* (DNS not yet pointing at the box, port 80 firewalled) the task keeps
a rolling log of recent failed attempts in `acme_dir/acme-retry-state.json` and
paces the next attempt against Let's Encrypt's failed-validation budget
(5 / account / hostname / hour, one slot reserved → an effective **4 / hour**):
budget free → retry on the next 5-min poll (the common case the instant a
misconfiguration clears); failing → attempts spaced evenly across the window
(≈ 15 min); budget spent → wait exactly until the oldest failure ages out.
The log is **persisted**, so a restart loop *resumes* the spent budget rather
than firing a fresh attempt that could blow the limit (`next_attempt_delay`);
a corrupt/missing state file reads as healthy and never wedges issuance. The
other limits the design respects (always test against LE **staging**):
duplicate-certificate 5 / week / exact SAN set (protected by the re-issue-only
conditions above) and new-orders 300 / account / 3 h.

**Wake accelerators (within budget).** Two admin actions expedite the
self-heal by waking the lifecycle task (`AppState::acme_retry_notify`) instead
of waiting out the steady poll: `fauna.bridges.restore_real_tls_cert` (the
"switch back to a real cert" action) and `fauna.bridges.provision_self_signed_cert`
— the latter fires **only on a real→self-signed transition**
(`provision_should_wake_acme`), because a repeat provision over an
already-self-signed cert would otherwise drive a fresh *successful* re-issue
per call against LE's success-side weekly limits (duplicate-certificate),
which the failed-validation budget does not model; the transition gate
coalesces repeats into the single in-flight self-heal. A wake arriving
mid-backoff is ignored — the accelerator can never fire an attempt that would
blow the failed-validation budget (`cert_lifecycle_loop`).

**Instant switch-back from a preserved cert (optimization).** Because a
self-signed provision overwrites the nest's own listener PEM,
`write_acme_pem_atomic` preserves the existing real cert+key to a backup slot
(`fullchain.real-backup.pem` / `privkey.real-backup.pem`) whenever a
self-signed cert is about to overwrite a **real** one (only that transition —
real→real renewal and self-signed→self-signed do not). `fauna.bridges.restore_real_tls_cert`
(Admin, parameterless, **non-destructive**) copies the backup back and the
cert watcher hot-reloads it within ~2 s (`{restored_immediately: true,
method: "backup"}`) — an instant switch-back that skips the self-heal wait.
No backup → no-op (`method: "self_heal"`); the self-heal already handles it.
Restore is purely an optimization; correctness never depends on it, and there
is no destructive cert-deletion path.

**Prompt bridge re-serve.** When a real cert lands, `cert_lifecycle_loop`
fires a `config_changed`/`"tls"` push so every running bridge re-fetches its
sealed cert blob without waiting out its 12 h timer — the bridge-facing half
(seal-on-read, blob shapes, the push) is owned by `mail-bridge-lifecycle.md`
§ TLS provisioning.

## ACME settings — constants, not choices

**Ruled 2026-10-01: the certificate
authority and the account contact are constants. No human chooses either, in
a file or in an app.** A nest orders from Let's Encrypt's production
directory and registers its ACME account with no contact address — the two
constants the apps' own DNS-01 orders have always used
(`Dns01OrderConfig::lets_encrypt` with an empty contact, `libs/fauna-client-dns`),
so the nest-driven HTTP-01 path and the client-driven DNS-01 path are one
policy. The principles test (`principles.md` § One configuration surface —
*would a user or admin ever want to choose this?*) answers no for both. A
contact address is optional in RFC 8555, and renewal is the certificate
lifecycle loop's job, not a mailbox's. A different CA is not a setting a nest
could honour alone: a CA outside the public roots needs its root installed on
every app and mail client that connects, and most public alternatives need an
external account binding the nest has nowhere to hold. A deployment that must
use another CA is a feature to design whole — the trust root, the account
binding and the admin page that carries them — and until that design exists
no file, variable or flag stands in for it. This reverses the earlier
recording of both as admin choices awaiting an admin page; staging is not a
choice either (it is a CA nobody trusts, useful only to a test).

One piece of test wiring remains by design: `[acme] directory_url`, which the
Docker entrypoint writes from `FAUNA_ACME_DIRECTORY_URL`, points the HTTP-01
client at the in-network `pebble` of the tier_4 acceptance. It is
artifact-set IPC for the harness — no deployment path sets it — in the same
class as `FAUNA_INSECURE_DISABLE_TLS`. `installers/docker.md` § Environment
Variables classifies the env rows and points here, as does
`mail-bridge-lifecycle.md`'s ACME-managed path row. The old contact and
staging knobs are gone from the nest (`[acme] email` / `staging` in an old file
parse and do nothing; the flags and the entrypoint's contact variable no longer
exist). The one other consumer of the shared HTTP-01 library, the hosted
`fauna.social` front door (`front-door.md`), is the project's own service, not
a nest: it keeps its hostmaster role address as the account contact and maps
its `FAUNA_ACME_STAGING` onto the directory override.

---

## C. Client-driven DNS + the mobile-offline expiry hazard (mitigations 1–4; mitigation 5 DEFERRED)

The product invariant holds: **the nest never holds the DNS-provider key; the
admin's client publishes** (`dns-management.md` § Where the credential lives;
`deployment-home-with-public-relay.md` § Architectural rules). A direct
consequence: DNS-01 renewal needs an admin client online at renewal time, and an
**iOS/Android-only admin who does not open the app** could miss the window. The
mitigations, in order — and why this is safe without weakening the invariant:

1. **The floor makes expiry NON-FATAL for the Fauna app.** Native apps keep
   working on the floor via identity-pin (`security.md` § Transport trust); only
   the web browser, MUAs, and inbound-mail-trust degrade until the admin reopens
   the app and renewal completes. This is the cert-side analog of
   `dns-management.md`'s "a record drifts → shows red until a client next checks"
   — **no nest daemon required**.
2. **Generous renewal lead.** Begin renewal attempts well before expiry (**≥30
   days**, matching the built HTTP-01 lead in `acme_http01.rs`); the floor covers
   any gap until a client completes the renewal.
3. **Any synced admin device renews.** The credential lives in `fauna.state.dns`
   (`dns-management.md`), synced to laptop / phone / web, so a desktop or web
   client covers a mobile-only gap. Renewal is **not** pinned to one device. The
   DNS-01 **ACME account credentials** are persisted the same way — a
   field of the same tip-sealed `fauna.state.dns` record, nest-opaque, synced alongside the
   DNS-provider credential and owned by the same `DnsManagementMachine` — so every
   device renews against the **same** ACME account rather than minting a fresh
   per-device/per-renewal account (account reuse avoids new-account churn against
   the CA's rate limits and keeps one stable account identity for the deployment).
   A device with no persisted account creates one on first issuance and writes it
   back for the others. *(Decision recorded as D6 in the Phase-3 plan; the order
   core threads the credentials in/out, the orchestration slice persists them.)*
   **This renewal is automatic by default.** A per-domain **auto-renew** toggle
   (`admin-dns-domain-auto-renew`) defaults **on** for every managed/delegated
   domain — the only kind a client *can* auto-issue (a held credential publishes
   `_acme-challenge`, or a one-time CNAME delegation re-homes it). The decision of
   *which* domains a synced device should re-issue right now is the shared
   projection `DnsSnapshot::domains_needing_auto_renew` (at-risk per the
   cert-status row ∧ auto-renew on). **The whole background tick is shared, not
   per-app glue**: `DnsManagementMachine::auto_renew_scan` (refresh the record
   matrix + cert-status, then ask that decision) and
   `DnsManagementMachine::auto_renew_issue` (dispatch
   `IssueCert { domain, target_nest_id }` per domain — each failure non-fatal —
   then re-read cert-status), on the one shared cadence `auto_renew_poll_secs`,
   with **no admin tap**. A native app owns only its timer, the `target_nest_id`
   resolution (linked-nests state, D7 — deliberately outside the DNS machine), its
   log sink, and shipping the refreshed snapshot to an open `admin-dns` page; an
   empty scan means it resolves no nest id and issues nothing. The admin only ever
   touches the toggle to
   *disable* hands-off renewal; the opt-out persists in
   `DnsConfig.auto_renew_off` (empty ⇒ all managed/delegated domains
   auto-renew). A manual-non-delegated domain can't auto-renew, so the control is
   shown only for managed/delegated domains. *(Truly device-less renewal — with no
   client ever online — would need the nest to hold a challenge-scoped DNS
   credential; that is mitigation 5, deferred below.)*
4. **Push reminder when due AND at-risk.** When renewal is due and no client has
   renewed within the lead window, the nest pushes the admin ("open your Fauna app
   to renew your certificate") — this reaches a closed mobile app. The admin DNS
   page surfaces a **cert-status row** next to the existing red/green DNS checks:
   `valid-trusted` / `on-floor — renew needed` / `expiring`. (This resolves the
   "ACME/cert-health status badge whose home is deferred" note in
   `dns-management.md` § App surface — its home is the `admin-dns` page.)
5. **~~Always-on scoped acme-dns hands-off renewal~~ — DEFERRED (user decision
   2026-06-05).** A challenge-only credential (an acme-dns responder, or a
   challenge-scoped delegation the relay nest holds) would let a mobile-only admin
   renew with no client online — but it weakens the strict "no nest holds any DNS
   capability" invariant. The user chose to keep the strict invariant and rely on
   the floor + reminders (mitigations 1–4). **Do not build this in this track.**
   It is recorded here as an explicitly-deferred future option, to revisit only if
   real usage shows mobile-only admins are common; it is **not** a TODO.

---

## D. MTA-STS honesty + DANE/TLSA for the self-signed MX (mail)

The two mail-trust mechanisms each need the cert reality reflected honestly in
published DNS — otherwise the nest advertises a guarantee its served cert cannot
keep, and senders **refuse** mail.

- **MTA-STS honesty.** A mail domain currently serving the **floor** on its MX
  must **not** publish `_mta-sts.<domain>` mode `enforce` — a sender that fetches
  an `enforce` policy and then sees a non-WebPKI MX cert refuses delivery
  (`smtp-server.md` outbound-MTA-STS describes the symmetric refusal we apply as a
  *sender*). So couple the **published** MTA-STS mode to cert reality: while a
  domain is on the floor, the published mode drops to `testing`; it
  restores to `enforce` once a trusted cert is live for that MX. `mail-multidomain.md`
  § Per-domain MTA-STS owns the mode values + the `mail_domains.mta_sts_mode`
  storage; this doc owns the **coupling rule** (published mode follows cert
  reality, never advertises a trust the floor can't satisfy).
- **DANE/TLSA for a self-signed MX.** Publishing a
  `_25._tcp.mail.<primary-domain>` **TLSA** record (`3 1 1` — DANE-EE / SPKI /
  SHA-256) that pins the floor's **stable-key** self-signed MX lets
  DANE-validating senders (Gmail does DANE) accept the MX **without a CA** — a
  self-signed-but-trusted MX. One host-level record covers the whole deployment
  (every local domain MXes to the single `mail.<primary-domain>` host). This
  complements MTA-STS (DANE > MTA-STS at receivers that do both) and **requires
  the stable floor key** (§ A): a key change invalidates the TLSA, so the pin
  follows the served floor leaf's SPKI (which reuses the stable key), never
  churned by a routine renewal. Publication is **unconditional** — no DNSSEC
  probe: a non-DNSSEC sender treats the unsigned TLSA as absent (RFC 7672
  §2.1.1), so it is harmless on non-DNSSEC domains and does its job on DNSSEC
  ones. This is the *inbound-advertised* side of DANE (pinning **our** MX for
  senders); Fauna's **outbound** DANE (validating *recipient* MX hosts —
  `fetch_tlsa`, `LiveTlsaResolver`, `dane_chain_matches`) is already built
  (`smtp-server.md` § Architectural rules) and is unaffected. `mail-multidomain.md`
  § Per-domain DNS records owns the TLSA record body; this doc owns the
  **coupling rule**: the self-signed-MX TLSA is **automatic and cert-coupled**
  (not an admin opt-in) — present, pinning the stable floor key, only while the
  MX is on the floor, and **withdrawn** the moment a trusted cert covers it (a
  floor-key TLSA against a trusted cert would DANE-fail), exactly mirroring the
  published-MTA-STS-mode coupling above.

> **Fauna↔fauna federation is out of scope here.** Federation authenticates on the
> Fauna identity channel, not MTA-STS/DANE; the MTA-STS coupling and the
> self-signed-MX TLSA are **only** for non-Fauna inbound SMTP.

---

## E. DNS `self=` published per domain (fresh-client floor acceptance)

A **fresh** native app connecting to a **public** domain that is serving the
floor needs the domain's `_fauna.{domain}` TXT `self=<actor_id>` published, so the
client resolves the expected nest identity and accepts the floor via the DNS root
(`security.md` § Transport trust, Axis 2 — public-domain root). An **already-pinned**
client tolerates the floor via TOFU without `self=`. So `self=` must be part of the
managed record matrix the `DnsManagementMachine` publishes; this doc only records
that the floor depends on it for fresh-client acceptance, so it must not be dropped
when a domain is on the floor.

**Status (2026-08-13): built, and the scope now matches this section's title.** This
paragraph previously said "(it already is)", which was false at code level from the day
it was written — nothing emitted a `_fauna` row, so the record had never been published
on any deployment and only the *read* side existed. That gap closed on both halves
2026-08-12: the nest emits the row (`dns_handlers::append_fauna_self_txt`, from the live
deployment key) and a managed-mode client publishes it through a **withdraw-aware
converge pass**, so a deployment-seed rotation moves the value instead of leaving the
superseded identity resolvable beside the successor.

That first build was **primary-only**, which left this section stating a requirement the
code declined to meet for every secondary domain. Ruled 2026-08-13 in favour of this
section's reading: the row is emitted on **every public active local domain**, so the
"per domain" in the title is now literal. The deciding facts — recorded here because
they are what a re-litigation would have to overturn:

- **Acceptance here is name-independent.** It authenticates the nest *identity*, not the
  cert's name, and one deployment has one identity — so the correct answer is the same
  at every domain, and scoping it to the primary made a user's trust posture depend on
  which of the box's domains their handle sat on.
- **It bites exactly where this section aims.** `self=` is *required* only when the domain
  serves the floor: a WebPKI-valid cert short-circuited before the DNS lookup
  (`fauna-anon-client::graduate_handshake`) until 2026-10-05, and since the login's pin
  ruling (`security.md` § Transport trust → *The login's pin*) a login consults it on every
  cert — a binding in hand is never waived — and records the root it verifies as the
  host's pin. A secondary on the floor was the weakest case in the
  tree: the floor cert's SANs are apex-derived (`self_signed_cert::write_self_signed_bootstrap`)
  and a secondary mail domain gets no per-SNI floor (`MultiDomainCertResolver::cert_for_sni`
  mints one only for names registered via `add_domain`, i.e. custom *web* domains), so a
  fresh app arriving there met a name-mismatched cert **and** no identity root, and fell
  back to TOFU. Widening fixes the half that governs identity.
- **A secondary is a client entry point by construction**, so this is not a hypothetical
  arrival path: it gets an apex `A`/`AAAA` → nest, and its apex joins the apex ACME order,
  both explicitly for client reachability.

Placement, the per-domain public-ness gate, the custom-web-domain scope boundary, and the
per-slot withdrawal rationale are owned by
[`dns-management.md`](../../behavior/dns-management.md) § Records covered + § Fauna-managed
→ *Withdraw-aware convergence*; the rotation consumer is
[`box-recovery.md`](box-recovery.md) § Deployment-seed rotation → *DNS row and the
propagation window*. ⚠ Two things still gate end-to-end coverage: **tui and web dispatch
`DnsAction::Publish` from nowhere**, so no managed row — this one included — is published
from those two apps until that gap closes; and on a **floor-only** box (one that never
obtained a trusted cert) a public secondary is additionally served a *name-mismatched*
cert — analysed and **accepted** in § A's ⚠ paragraph on the mint's bound (only a web
browser is affected; DAV/mail clients dial `mail.<primary>`, and a Fauna app never
name-checks). On an ACME-healthy box that second problem does not arise at all: the
secondary's apex joins the apex ACME order, so the default leaf covers it. This ruling
makes the connection *identity*-verifiable in every case.

---

## Where logic lives

Per priority #2, shared Rust by default; the nest owns the cert-selection and
acquisition it is in the path for, and **never** the DNS-provider credential.

- **Nest (`bins/fauna-nest`)** — owns: the always-live floor (synthesize +
  auto-renew + stable key), the per-SNI valid-else-floor resolver
  (`MultiDomainCertResolver`), HTTP-01 acquisition (`acme_http01.rs` — the
  generic order/challenge/retry-budget machinery is the shared
  `libs/fauna-acme-http01` crate, also consumed by the `fauna.social`
  front-door binary, `front-door.md` § TLS policy — re-exported here
  unchanged; nest-only policy (SAN derivation, resolve gates,
  `cert_lifecycle_task`) stays in this module, `web_content/cert.rs`), the
  cert-install convergence (`store_acme_material` —
  seal listener PEM + fan out the sealed `TlsCertBlob` to bridges) + reload
  (`cert_watcher_task`), the **single installer for a client-issued cert**
  (`lan_cert::install_client_issued_cert` → `store_acme_material`, called by both
  the `fauna.tls.publish_cert` handler and the namespace-sync pull — § B tier 2),
  the **desired-SAN-set report** a client-driven order requests
  (`cert_status` → `desired_sans`), the cert-status projection on the admin read
  surface, and the at-risk renewal push. The nest **never** holds a DNS-provider
  key, **never** writes DNS, and **never** runs a DNS-01 order itself.
- **Client (shared Rust — `libs/fauna-client-dns` / `libs/fauna-provisioning`)** —
  owns the DNS-01 **order** (it holds the provider key): drive the ACME order,
  publish `_acme-challenge` via the `DnsManagementMachine` (managed) or surface it
  to paste (manual), obtain the cert, then **seal it to the nest** as the
  namespace-sync producer (`fauna_mls::wrapped_blob::lan_cert` —
  `seal_lan_tls_cert_entry`, the producer side the Slice-4 consumer awaits). The
  cert-status surface + the renew-reminder UI render on `admin-dns` (priority #1,
  uniform across all 7 apps — tui's `admin-dns` cert lifecycle landed 2026-07-29).
  Also owns the **background auto-renew tick** (§ C.3): `auto_renew_poll_secs`
  (the one cadence), `DnsManagementMachine::auto_renew_scan` and
  `::auto_renew_issue` (the whole sequence — refresh-then-ask order,
  skip-if-empty, per-domain non-fatal issuance, trailing health re-read).
- **Per-app shells** — render the cert-status row + dispatch the renew action;
  no cert logic of their own. A native shell's auto-renew loop owns only its
  timer, the `target_nest_id` resolution (D7), its log sink, and shipping the
  refreshed snapshot to an open page — never the tick's own policy.

## Relationship to neighbouring docs

- **`security.md` § Transport trust** owns the **client side** (identity-pin,
  channel binding, "pin the identity, never the cert", `DiskPinStore`). This doc
  owns the **server-side cert-selection policy** that side assumes; the nest
  signs the SPKI of the cert *it serves* over the channel binding, so the floor
  is transparent to native apps.
- **`deployment-home-with-public-relay.md`** owns the **home/relay topology** —
  LAN self-signed default, the client-published DNS-01 path for a non-routable
  `home.example.com`, and the Slice-4 cert-over-namespace-sync distribution
  (DONE). This doc owns the **general** floor + tiered-acquisition policy that
  topology specializes; that doc references it for the acquisition tiers.
- **`dns-management.md`** owns the **DNS-record management mechanism** (the
  `DnsManagementMachine`, the client-held `fauna.state.dns` credential, managed vs
  manual, live verification). This doc adds the **`_acme-challenge` TXT** to its
  managed matrix and homes the **cert-status badge** on `admin-dns`; it never
  re-defines the credential/publish model.
- **`smtp-server.md`** owns the **per-port inbound TLS posture** and the
  **outbound** MTA-STS/DANE enforcement (we as a sender). This doc owns the
  **published** MTA-STS-mode↔cert coupling (we as a receiver advertising honestly).
- **`mail-multidomain.md`** owns the **per-domain record bodies** (MTA-STS policy,
  optional TLSA) + the `mta_sts_mode` / `mta_sts_cert_mode` storage. This doc owns
  the rule that the published mode follows cert reality and that an enabled
  self-signed-MX TLSA is tied to the stable floor key. (The § MTA-STS HTTPS cert
  `expand_primary`/`wildcard`/`per_host` modes describe *which SANs* a per-domain
  cert carries; the **issuer mechanism** is this doc's tiered acquisition via the
  nest's own ACME client + `MultiDomainCertResolver`, **not** an external
  `certbot`.)
- **`front-door.md`** owns the **`fauna.social` public-web box** — the
  HTTP-01-only, DNS-01-never posture appropriate to a box that holds no
  DNS-provider key, and its own self-signed-floor-then-hot-swap serving. It
  shares the generic HTTP-01 order/renew **implementation** with the nest
  (`libs/fauna-acme-http01`, lifted 2026-08-21) but defers all **mechanics**
  here — this doc owns the seam both binaries consume.

## Implementation status today

**The IP bridge cert (§ B-IP) is BUILT and CA-proven** (ratified 2026-08-29; the
order landed 2026-09-02). A domainless box on a public
address orders, installs and renews the bridge from first boot, and serves it to
no-SNI dials. The order is proven on the wire against a real ACME CA
(`libs/fauna-acme-http01/tests/pebble_http01.rs::ip_identifier_flow_issues_a_real_certificate_against_pebble`,
green 2026-09-02 against pebble 2.9.0). What remains is a **live** witness — a
real provisioned box actually reached at `https://<ip>` by a browser — which
rides the live runs, not this section:

- **Built.** The derived enable — `acme::ip_bridge_addresses` (global-unicast
  addresses attached to this box's own interfaces via
  `acme::attached_interface_addresses`, classified by
  `fauna_core::resolve::is_global_ip`; the private-NAT-axis gate still wins; no
  knob of any kind, and independent of the domain-derived `[acme]` enable). The
  lifetime rule — `acme::ip_cert_needs_renewal` (one third of the *observed*
  validity, so a CA issuing something other than LE's 160 h `shortlived` gets a
  proportionate lead) and `acme::ip_bridge_should_renew` (the bridge is dropped
  once a trusted primary-domain cert is live). The serving rule —
  `MultiDomainCertResolver::{set_ip_cert, clear_ip_cert, current_ip_cert}`: the
  no-SNI default while valid, falling through to the floor/default otherwise,
  with named SNIs and the channel-binding SPKI deliberately untouched. The CSR
  side needs no work at all: `fauna_acme_core`'s shared finalize tail already
  emits an `iPAddress` GeneralName for an IP literal (unit-pinned there, since
  the classification is invisible at the call site and a silent flip to `dNSName`
  would fail only against a live CA). `IP_CERT_FILENAME` / `IP_KEY_FILENAME`
  reserve the separate on-disk material.
- **Built 2026-09-02 — the order and its lifecycle.** The blocker was
  `instant-acme` 0.7.2, which had **neither an `Identifier::Ip` variant nor a
  `NewOrder.profile` field**, so no RFC 8738 `ip` identifier could be expressed
  and the `shortlived` profile could not be requested. The user-approved
  semver-major bump to **0.8.5** supplies both (`Identifier::Ip(IpAddr)`,
  `NewOrder::profile`), keeping ONE native order core rather than growing a
  second. On top of it: `fauna_acme_http01::obtain_ip_certificate` — the same
  account, port-80 challenge router and finalize tail the domain order uses,
  differing only in identifier kind, profile and filenames (`OrderSubject`); and
  the lifecycle arm inside `cert_lifecycle_loop`, which orders **one** cert over
  the whole address set (the resolver can serve exactly one cert to a no-SNI
  dial, so per-family certs would be unservable — a dual-family failure narrows
  to v4-only instead), installs it into the resolver directly, reconciles a
  still-fresh chain across a restart without respending the CA's
  duplicate-certificate budget, re-orders when the interface set changes, and
  drops the bridge once the primary-domain cert is trusted. It shares the domain
  arm's `RetryState` — one owner of `acme-retry-state.json`, per § B-IP
  *Lifetime*'s "no second budget".
- **Fixed 2026-09-02 — settling after a
  v4-only narrowing.** "Re-orders when the interface set changes" above is
  `acme::ip_bridge_addrs_changed`, comparing the live derived set against
  `IP_BRIDGE_ATTEMPTED_FILENAME` (`ip-bridge-attempted.json`, persisted beside
  the chain) — **not** against what the installed chain's own SANs cover. A
  dual-stack box whose IPv6 `:80` the CA cannot reach narrows every order to
  v4-only by design, so the chain never covers the live v6 address; comparing
  coverage against the *live* set instead of the last-*attempted* one read that
  permanent, expected gap as a fresh interface change on every tick and
  re-ordered forever, unpaced (a narrowed success returns `false`, so it never
  reaches the failed-validation budget that would otherwise slow it down).
- **Proven against a CA 2026-09-02 — the pebble acceptance.** An RFC 8738
  `ip`-identifier order for `127.0.0.1` under the `shortlived` profile, driven
  through the production machinery, issues a real certificate whose leaf carries
  an `iPAddress` SAN and **no** `dNSName` — the `rcgen` classification that is
  invisible at the call site and would otherwise fail only against a live CA.
  It lives in `fauna-acme-http01`'s own pebble test rather than the tier_4 nest
  suite on purpose: validating an IP identifier needs no DNS (the CA dials the
  address itself), so loopback plus the already-bound challenge listener is the
  whole fixture — whereas the tier_4 acme network's container address is RFC 1918,
  which the derive rightly refuses, and giving that network a global-unicast
  subnet would be working around the derive rather than testing it. The derive
  keeps its own unit pins; this test owns the order.
- **Not proven — the live witness.** No real provisioned box has yet been reached
  at `https://<ip>` by a browser. That rides the live provisioning runs
  (`../../behavior/onboarding.md` § Implementation status, the web bullet), not
  this section.

Everything else in this section predates that ratification and is unaffected by
it.

Everything below is **built and proven** (re-verified against code in the
2026-07-09 cluster-#4 review); the target-state prose above is the operating
state, and the admin-dns cert UI is complete on **all 7 apps** as of 2026-07-29
(tui was the last — its `admin-dns` page sits outside `navigation.admin_pages`,
so the M8 admin-shell build hadn't covered it; `apps/fauna-tui/src/admin/dns.rs`,
`tests/e2e-unified/ui-actual-tui.yaml`). The recorded gaps are the ACME-settings
configuration surface (bottom of this section) and the live-issuance gap below. Landing history lives in git, not here.

| Mechanism | Symbols (anchor) | Proven by |
|---|---|---|
| Self-signed floor — unconditional, stable key, auto-renew, per-SNI valid-else-floor (§ A) | `write_self_signed_bootstrap`, `load_or_create_floor_key` (`floor-privkey.pem`), `floor_renew_task` (spawned on every nest), `MultiDomainCertResolver::cert_for_sni` / `cert_valid_and_covers` / `floor_for_sni` / `mint_floor_cert` (`bins/fauna-nest/src/{self_signed_cert,acme}.rs`) | resolver + floor unit tests; the per-SNI floor never changes the channel-binding SPKI (apex-only — unit-pinned) |
| Domainless-capable bootstrap (no domain → SANs `localhost`, `127.0.0.1`, CN `fauna-nest`) | `prepare_listener_tls` calls the bootstrap unconditionally; `synthesize_self_signed_pem_with_key`; test-only escape `FAUNA_INSECURE_DISABLE_TLS` | when/why owned by `domains-and-tls-bootstrap.md` |
| HTTP-01 acquisition + hot-reload + issuance lifecycle (§ Keeping the cert alive) | `acme_http01.rs` (30-day lead; generic order/challenge/retry-budget machinery now lives in shared `libs/fauna-acme-http01`, re-exported unchanged — lifted 2026-08-21, `front-door.md`; the finalize/CSR/poll tail once the challenge is satisfied is now shared further, in `libs/fauna-acme-core::finalize_order_and_fetch_certificate` — lifted 2026-08-26, also consumed by the DNS-01 driver below), `web_content/cert.rs` (per-domain web-content loop), `cert_lifecycle_task`, `cert_watcher_task`, `store_acme_material`, `acme-retry-state.json` rate budget, `acme_retry_notify` + `provision_should_wake_acme` wakes, `restore_real_tls_cert` switch-back | unit tests; tier_4 `test_acme_http01_pebble_issuance.py` (a real pebble-CA issuance through the image's own client + challenge listener + watcher); the shared crate's own real-wire proof is `libs/fauna-acme-http01/tests/pebble_http01.rs` (`just e2e-pebble-http01`) |
| ACME enable is **derived**, never configured (no `[acme].enabled` field) | `build_acme_config` — enabled iff public NAT axis ∧ a real orderable domain; a private or domainless/`localhost` box serves only the floor | unit-pinned |
| CA-agnostic directory override | `[acme].directory_url` → `acme_http01::acme_directory_url` (the override, else Let's Encrypt production); Docker maps `FAUNA_ACME_DIRECTORY_URL`; trusts via system roots (`SSL_CERT_FILE`) | `directory_url_override_else_lets_encrypt_production`, `build_acme_config_reads_toml_directory_url` |
| REUSED-AUTHORIZATION skip + infra/secondary SAN gates (§ B) | `obtain_certificate` skips non-`Pending` authorizations (fixed 2026-07-02); `desired_san_domains` over `InfraSans`, `relay_san_included` / `pds_san_included` → `infra_host_resolves`, `reachable_mail_domains` vs the persisted `nest_host_address` | unit tests; tier_4 assert-4 XPASS; gates shared with the at-risk nudge |
| DNS-01 order core — client-side, **both drivers** (§ B tier 2) | `fauna_client_dns::obtain_certificate_dns01`; native `acme_order` (instant-acme + rcgen, finalize/CSR/poll tail shared with the HTTP-01 driver via `libs/fauna-acme-core` since 2026-08-26) and wasm-safe `acme_pure` (RustCrypto ES256 JWS + `x509-cert` CSR + reqwest via the credential-blind `proxy.fauna.social` CORS proxy); shared `acme_shared` — which owns the **propagation gate** (2026-07-24): an active `Dns01ResolvabilityProbe` poll of the publish zone's authoritative NS (45-min deadline then best-effort) replaces the fixed wait before CA validation. **One mechanism, two transports (2026-08-22):** the query is `fauna_core::authoritative_dns::authoritative_txt_visible` (raw non-recursive hickory over UDP, every authoritative NS must serve the exact value), run in-process by `AuthoritativeNsProbe` on the 5 native apps + tui, and one Admin RPC hop away by `NestRelayedProbe` → `fauna.dns.probe_txt_visible` on **web**, which has no raw DNS in the browser. Web is deliberately not given a recursive/DoH approximation: a recursive resolver negative-caches the miss the gate's own polling plants. A nest that predates the kind errors the call, which the probe contract reads as "not visible yet", so such a pairing degrades to poll-to-deadline-then-best-effort with no version branch. The gate runs on **both** order paths — managed via `with_published_challenges` (publish → gate → CA dance), and **manual via `complete_dns01_order`** (`PropagationGate::manual`, gate → CA dance; nothing to publish or tear down, and the gate's zone is the admin's own domain). Extended to manual 2026-07-29: it had validated immediately on the admin's say-so, which is a claim about their registrar's *control plane*, not about what the authoritative NS serves — see § Live issuance | S4 unit tests + behavioural fake-probe gate tests (`acme_shared::tests::gate_*`, `manual_gate_*`); probe responder tests (`resolvability::tests`); S4b pebble acceptance `libs/fauna-client-dns/tests/pebble_dns01.rs` — all four flows (managed fresh, managed account-reuse D6, manual two-phase, CNAME-delegated) × both drivers + cross-driver D6 account interop (`just e2e-pebble-dns01`, pebble 2.9.0 pin) |
| DNS-01 managed orchestration | `DnsAction::IssueCert { domain, target_nest_id }`; persisted ACME account `DnsConfig.acme_account` (tip-sealed `fauna.state.dns`, D6 — every synced device renews the same account); seal `seal_lan_tls_cert_entry` + deliver over `fauna.tls.publish_cert` | machine unit tests |
| DNS-01 manual two-phase (S6a) + CNAME renewal-delegation (S6b) | `begin_dns01_order` / `complete_dns01_order` (the latter runs the propagation gate before the CA dance — row above); `BeginManualIssueCert` → `snapshot.pending_cert` (plain `admin-dns-record` rows) → `CompleteManualIssueCert` / `CancelManualIssueCert`; `DelegateRenewal { domain, target_zone }` → persisted `CnameDelegation`, `resolve_issuance_target` + `Dns01OrderConfig::challenge_publish_names` | pebble manual + delegation phases, both drivers (the manual phase injects a late-visible probe, asserting the gate polled before signalling ready) |
| Interrupted manual issuance survives a machine rebuild (§ Surviving an interrupted manual issuance) — built 2026-07-29, real-CA-verified 2026-08-19 | `DnsConfig.pending_manual_issue` (`PendingManualIssue` + `PendingChallengeRecord`, additive `#[serde(default)]`) **and** `DnsConfig.acme_account`, both written by `persist_pending_manual_issue` at **begin** time (not only on completion — the account must survive the same gap the breadcrumb does, or a resume can never even be offered the same challenge); `apply_config` re-projects the breadcrumb onto `snapshot.pending_cert` whenever the machine holds no live order; `resume_manual_issue` re-opens a fresh order against the restored account and gates on `challenge_values_match`; a failed completion keeps card + breadcrumb, and the freshly re-opened order stays live for the next completion attempt | machine tests (`a_fresh_machine_resurfaces_the_interrupted_paste_card`, `a_refresh_clears_a_card_whose_issuance_ended_elsewhere`, `cancel_manual_issue_clears_the_persisted_breadcrumb`, `persisting_an_in_flight_issuance_records_the_record_to_paste`, `resuming_a_breadcrumb_with_a_bad_target_errors_before_any_ca_work`, `challenge_values_match_*`) + `fauna-core` at-rest round-trip / decodes-without-the-field + the pebble real-wire proof (S4b) `manual_resume_survives_a_machine_rebuild_against_pebble` — begins through a `DnsManagementMachine` carrying `ManualOrderTestSeam`, DROPS it, REBUILDS a fresh one over the same config store, and completes against a real CA, which is what found the missing-account-persistence gap this row's mechanism column now describes |
| Client-issued cert install — one installer, both topologies (§ B tier 2) | client producer `seal_lan_tls_cert_entry` → `fauna.tls.publish_cert`; nest `lan_cert::install_client_issued_cert` (opens with its **own** identity x25519 secret ⇒ installs; cannot open a peer's blob ⇒ stores it opaquely) called from BOTH `tls_handlers::publish_cert_handler` (standalone) and `nest_sync_worker` on the namespace-sync pull (relay→private). No NAT-axis gate — the seal target authorizes the nest, and the installer itself requires the **signer** to be an admin of this nest, on both routes (built 2026-09-21 — the relayed route had no signer check at all). `PublishCertReply.installed` reports which happened | tier_3 `conformance_tls_publish_cert` (self-install on the sealed-to nest; relay stores-but-does-not-install; forged sig rejected; non-admin denied) + `conformance_cross_nest_lan_cert` (admin-signed relay install; a paired non-admin's entry pulled but not installed; a nest with no admin installs nothing until its admin publishes; forged sig rejected) |
| Client order covers the nest's listener SAN set (§ B tier 2) | nest `tls_handlers::desired_listener_sans` (reuses `acme_http01::desired_san_domains`, flag-gated not resolve-gated) → `CertStatusReply.desired_sans`; client `fauna_client_dns::order_san_set` filters to credential-covered zones and stays single-name when delegated | unit `order_san_set_covers_the_listener_but_only_what_the_credential_can_publish` + `issue_cert_reads_the_listener_san_set_before_ordering` |
| Live app-UI DNS-01 renewal against a real CA + real DNS | `tests/e2e-unified/tests/live/test_dns01_cert_renewal_hetzner.py` — admin types the token into `admin-dns-add-credential-*`, flips `admin-dns-domain-mode`, clicks `admin-dns-cert-issue-button`, and the `admin-dns-cert-status` badge's expiry must **advance**; credential + mode flip are undone in a `finally` | tier_4, opt-in (`FAUNA_E2E_LIVE=1` + `HETZNER_API_TOKEN`) |
| Cert-status read + client projection (§ C.4) | `fauna.tls.cert_status` (Admin-only, `tls_handlers.rs`); `served_cert_facts` → `cert_health_state` (three wire states over the shared `CERT_RENEWAL_LEAD_SECS`); client seam `DnsNest::cert_status` → `DnsAction::RefreshCertStatus` → `snapshot.cert_statuses` (crosses UniFFI + wasm) | resolver unit tests; tier_3 `conformance_tls_cert_status.rs`; `refresh_cert_status_*` machine tests |
| At-risk renewal push (§ C.4) | `cert_nudge::cert_at_risk_nudge_task` — shares the issuer's SAN gates; gated to HTTP-01-off ∧ real public domain ∧ configured `PushService`; 24 h de-dup persisted in `acme_dir` | `cert_nudge` unit + loop tests (web-push generic-content caveat; an admin whose only device is android, linux, windows or a terminal has no push transport *built* yet — [`../apps/common.md`](../apps/common.md) § Push Notifications ruled theirs 2026-09-26 (`ws-device` through the sync agent for the desktops; android over UnifiedPush on the existing `web-push` transport, embedded-FCM fallback), so the nudge reaches them once those build rows land; this nudge is push-only with no WS twin, which is why the desktop stand-in is a real transport rather than the agent reading existing events) |
| Auto-renew setting + auto-issue decision (§ C.3) | opt-out set `DnsConfig.auto_renew_off` (default **on**, managed/delegated only), `DnsAction::SetAutoRenew`, shared decision `DnsSnapshot::domains_needing_auto_renew` (+ the machine-level FFI wrapper the native cadences reuse) | machine tests (default-on, opt-out persists, manual-non-delegated never auto-renews) |
| The background auto-renew **tick** itself (§ C.3) — shared, not per-app glue | `auto_renew_poll_secs` (6 h), `DnsManagementMachine::auto_renew_scan` / `::auto_renew_issue` (`libs/fauna-client-dns`), exported over UniFFI. **Consumed by tui + linux today; windows / macOS / iOS / android still run their own hand-rolled copy of the tick** and are owed the swap (their loops are behaviourally identical today — this is drift-prevention, not a live bug) | `fauna-client-dns` machine tests: scan refreshes health before deciding; empty scan when nothing at risk; the issue loop attempts every domain after a failure; the trailing re-read reaches the snapshot even when every domain failed. All three mechanism assertions mutation-verified |
| MTA-STS↔cert coupling + multi-domain live-route serving (§ D) | shared pure `MtaStsMode::coupled_to_cert` (`libs/fauna-mail`); `mta_sts_handler` (`lib.rs`) matches Host against active `mail_domains`, reads stored `mta_sts_mode`, downgrades `enforce → testing` while the MX serves the floor | `coupled_to_cert` + handler unit tests; tier_3 `conformance_mta_sts_serving.rs`; tier_4 `test_mta_sts_serving.py` |
| Self-signed-MX DANE/TLSA — emit, verify, managed reconcile, continuous withdraw (§ D) | nest `append_floor_mx_tlsa` (`dns_handlers.rs`) gated on `served_cert_facts(...).is_floor`, SPKI via `served_cert_spki_sha256`; shared `fauna_mail::dns::host::build_mail_tlsa_record` + `verify::compare_tlsa`; client `reconcile_floor_mx_tlsa` (primary-only, best-effort, per-provider TLSA: Cloudflare structured body, Hetzner/Porkbun/Gandi verbatim, Namecheap unsupported) + `withdraw_floor_mx_tlsa_on_trust` (edge-triggered on the cert-status observation, regardless of who issued) | unit + per-provider conformance tests; tier_3 `conformance_dns_dane_coupling.rs`; tier_4 `test_dane_tlsa_cert_coupling.py` |
| Per-app `admin-dns` cert UI — **all 7 apps complete** (tui 2026-07-29, the last) | badge `admin-dns-cert-status`; issue `admin-dns-cert-issue-button` (managed/delegated → `IssueCert`, manual → two-phase paste flow); delegation `admin-dns-cert-delegate-*` / `-remove-delegation-button`; auto-renew `admin-dns-domain-auto-renew`; native background auto-issue cadences (the tick is the shared `auto_renew_scan` / `auto_renew_issue` on the shared `auto_renew_poll_secs` — see the row above for which apps have adopted it; web issues natively on page-open — a browser SPA runs no background timer); per-page `DnsManagementMachine` — **held** for the page's lifetime on linux, **rebuilt per navigation** on windows (`AdminDnsPage` sets no `NavigationCacheMode`), which is no longer load-bearing now that an in-flight manual issuance is persisted (§ Surviving an interrupted manual issuance); `target_nest_id` = `LinkedNestsMachine::this_nest()` passthrough — on **tui** the machine is held for the session (built credentialed at `admin::init`), the badge/issue/delegate/auto-renew affordances are per-row inline reveals (a terminal has no modal), and the native cadence is `admin::spawn_auto_renew_cadence`, which (like linux's) now only schedules the shared tick | `test_admin_dns_cert{,_issue,_delegate,_auto_renew}.py` per app (android e2e host-emulator-gated; Robolectric 24 green); Linux is the ratified prior art the fan-out lifted |
| Client-side transport trust (the counterpart, not owned here) | `security.md` § Transport trust — identity-pin, channel binding, `DiskPinStore`; `acme::ServedCertSpki` signs the apex default cert's SPKI | — |
| Outbound DANE / MTA-STS (we as sender; not owned here) | `smtp-server.md` § Architectural rules (`fetch_tlsa`, `LiveTlsaResolver`, `dane_chain_matches`) + outbound MTA-STS enforcement | — |

**Remaining: none mechanism-side** — mitigation 5 (always-on scoped acme-dns)
stays deferred by user decision 2026-06-05 (§ C.5; an explicitly-recorded future
option, not a TODO), and the transient `_acme-challenge` TXT is deliberately
**not** a steady-state `list_records` row (built-by-design, § B — its absence
between renewals is correct). But see the live-issuance gap below.

**Live client-driven DNS-01 issuance: CONFIRMED 2026-07-24** — the live tier_4
renewal test passed end to end against production example.com (46 min run, ~37 of
them the propagation gate absorbing Hetzner's zone-publish batch): the linux
app drove the whole flow through `admin-dns` UI gestures, Let's Encrypt
validated, and the nest installed + hot-reloaded the renewed cert (badge and
served `notAfter` advanced 2026-08-28 → 2026-10-22, SAN set intact
`[example.com, mail.example.com]`, challenge TXT torn down clean). History of the
failure it closed: both prior live runs (2026-07-23) went `Invalid` at CA
validation; the order core had been CA-proven only against pebble (all flows,
both drivers — the S4b acceptance above — plus the failure-path reproduction
`dns01_order_goes_invalid_when_published_txt_is_not_yet_resolvable`, same file).
The root cause was **two stacked defects**, both fixed 2026-07-24:
(1) **the owner-name relativization miss** — `RpcDnsProvider` passed
fully-qualified owner names to the provider adapters whose contract is
zone-relative owners (`DnsProvider::record_names_relative_to_zone`), so
Hetzner's Cloud RRset API stored/served every record at a **doubled name**
(`_acme-challenge.example.com.example.com.`) — API-GET-visible, never resolvable at
the name the CA queries; fixed by relativizing in the seam (the orchestrator
always did), with wiremock wire-shape pins; (2) **fixed propagation waits can't
cover Hetzner's zone publish**, which is batchy and irregular (measured 15 min
/ >21 min / one ~5 h outlier from write-action success to authoritative-NS
serving); replaced by the **active resolvability poll** — `PropagationGate` +
`Dns01ResolvabilityProbe` in `acme_shared`, shared by both drivers, native
`AuthoritativeNsProbe` querying the publish zone's authoritative NS directly
(non-recursive, no negative-cache shadow), 45-min deadline then best-effort.
Wasm kept the fixed-wait fallback until a browser-safe probe existed; it has one
since 2026-08-22 — see § The browser's arm of the propagation gate below.

**The manual path was left ungated until 2026-07-29 — the hardened path was the
one nobody used.** Every domain is manual mode today (§ Implementation status),
so the gate above covered only `with_published_challenges` (managed), while
`complete_dns01_order` told the CA to validate **immediately** on the reasoning
that "the admin confirmed the record is live". That confirmation describes the
admin's *registrar control plane*, not what their authoritative NS serves, so the
first manual attempt raced the registrar's publish and lost. Found live on
2026-07-29 during the second-ever client-driven DNS-01 issuance (first through the
manual paste path): attempt 1 died, attempt 2 succeeded only because Let's Encrypt
**reuses a pending authorization** — the same challenge token came back and the
already-published TXT had propagated by then. Fixed by running the same
`PropagationGate` on the manual completion (`PropagationGate::manual` — identical
45-min/15-s bounds; the gate's zone for a manual domain is the domain itself).
Native gained the real probe then; the probe-less wasm path kept its existing
behavior, so the browser's complete-button did not silently acquire a blind
sleep — the wasm probe gap was left as a separate track, closed 2026-08-22 by
§ The browser's arm of the propagation gate below.

**§ The browser's arm of the propagation gate (2026-08-22).** The wasm gap was
worse than "wasm takes the 180 s fixed wait": on the **managed** path wasm took
that wait, but on the **manual** path — which is every domain today — the caller
passes `Duration::ZERO`, so a probe-less wasm order signalled ready with *no wait
and no check at all*. That is the exact 2026-07-29 race, unmitigated, on the one
driver that could not detect it.

Closed by making the probe's *mechanism* uniform and only its *transport*
per-target. `fauna_core::authoritative_dns::authoritative_txt_visible` — the raw
non-recursive per-NS UDP query, requiring every authoritative NS of the publish
zone to serve the exact value — is the single implementation. The 5 native apps
and the tui reach it in-process through `AuthoritativeNsProbe`; **web** reaches
it through `NestRelayedProbe`, which asks the nest over the Admin kind
`fauna.dns.probe_txt_visible`, whose handler runs that same function. Both wasm
order paths (`with_published_challenges` and `complete_dns01_order`) now carry a
probe, so neither can signal ready blind.

Three properties worth stating, because each was a live fork in the design:

- **Not DoH, and not the nest's recursive verifier.** A browser *can* reach a
  public DoH resolver, and the tree already has a wasm-proven DoH client
  (`fauna_provisioning::probe`). Both it and the nest's existing
  `fauna.dns.verify_records` (`state.dns_verifier`) are **recursive**, so they
  negative-cache a miss for up to the zone's SOA minimum — and a 15-second poll
  loop is precisely what plants that entry, blinding every later poll in the same
  gate. Authoritative-direct is the whole point of the mechanism, so the relay
  runs *that*, not the recursive path that happened to already exist.
- **Nest-side resolution is this surface's ratified shape, not a locality
  breach.** `dns-management.md` § Live verification already puts `admin-dns`'s
  live public-DNS resolution nest-side by design, because the client may sit
  behind split-horizon DNS that lies. The probe carries no credential and mutates
  nothing — DNS-provider credentials remain client-held — so the client stays the
  issuer; only a public, read-only question is delegated.
- **Old-nest degradation needs no version branch.** `Dns01ResolvabilityProbe`'s
  contract makes *every* failure `false` ("not visible yet"), so a nest that does
  not know the kind yields a probe that never confirms: the gate polls to its
  45-min deadline and proceeds best-effort — the fixed wait's behavior, bounded
  by a deadline sized from real provider-publish measurements rather than a
  blind guess.

Not reachable in production yet for an unrelated upstream reason: web's ACME
order routes through `proxy.fauna.social`, whose deployment is postponed
(`../provisioning/registry.md` § Implementation status). This closes the
*client-side* blocker, so web issuance is correct the day that proxy lands.

**The same live walkthrough exposed a second, quieter defect: the pending order
evaporated with no error at all.** Between the paste and the completion the admin
navigated away; the windows `AdminDnsPage` rebuilds its `DnsManagementMachine` on
every navigation, and the order lived only in that machine's memory, so the card
read "Checking" and then card and status text simply vanished. Nothing on the path
throws — the state was never carried across, which is why no error appeared and
why three plausible failure theories (a thrown dispatch, an implicit cancel, a CA
rejection) were all ruled out by the page's own structure. Fixed 2026-07-29 by
persisting the issuance rather than lengthening any client's machine lifetime
(§ Surviving an interrupted manual issuance) — a shared-Rust fix, because the
lost state lived in the shared machine and every app that rebuilds one loses it
identically. The gate change above raises the stakes on exactly this: the window
in which a rebuild can land now runs to 45 minutes.

**Gap — private-nest DNS-01 delivery has never been driven live.** The
2026-07-24 confirmation renewed a *public* nest's listener cert. For
**private** nests the seal-and-deliver leg (`fauna.tls.publish_cert` to a
paired home box) has never run outside fixtures: the private-relay live e2e
leaves the home box on the self-signed floor, and the cert-install conformance
tests use a fixture-sealed producer (`deployment-home-with-public-relay.md`
§ Producer is a client concern). Owed alongside the first production
home-with-public-relay install.

## Reading list

In priority order:

1. `docs/goal/architecture/security.md` § Transport trust — the client side this
   server policy is the counterpart to.
2. `docs/goal/behavior/dns-management.md` — the `DnsManagementMachine`, the
   client-held credential, managed/manual modes, live verification.
3. `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § MUA reach
   / § Cert provisioning summary — the home/relay specialization + Slice-4
   cert-over-namespace-sync.
4. `docs/goal/behavior/mail-multidomain.md` § Per-domain MTA-STS / § Per-domain
   DNS records — the record bodies the honesty coupling and self-signed-MX TLSA
   act on.
5. `docs/goal/behavior/smtp-server.md` § TLS posture per port / § DANE — the mail
   TLS posture + outbound DANE.
6. `docs/goal/architecture/front-door.md` § TLS policy — the `fauna.social`
   public-web box that now shares the HTTP-01 order/renew seam with the nest;
   mechanics stay owned here.
7. `bins/fauna-nest/src/{acme.rs,acme_http01.rs,self_signed_cert.rs}` +
   `libs/fauna-acme-http01/src/lib.rs` + `web_content/cert.rs` — the
   implementation this doc's server half describes.
