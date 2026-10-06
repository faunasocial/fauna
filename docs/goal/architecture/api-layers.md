# API Layers — target state

Owns: api-layers, push-relay
Status: ratified
Authority: endpoint-layer classification + the single HTTP-residue inventory + the caller-class authorization model at the WS-RPC gate; defers the per-domain enumeration of Layer 1's kinds to core-client-kind-catalog.md, defers WS-RPC frame mechanics to transport.md, wire bytes to serialization.md, per-feature behavior to behavior/*.md.

> **Audience:** every per-app area and the nest.
> **Purpose:** the canonical inventory of every fauna-nest endpoint classified by its intended consumer layer (Core Client, Protocol Client, Bridge Management, Admin, Federation, Internal). Calling the wrong layer is how duplicate work happens. This doc owns the API-surface story — which protocols, which paths, which auth surfaces, which layer each endpoint belongs to, and which previously-JSON endpoints have a WS-RPC successor. Defers wire-byte / canonicalization / sign-over-CID to `docs/goal/architecture/serialization.md`, WS-RPC frame structure (Request/Reply/Push/Cancel, idempotency, cancellation, push semantics) to `docs/goal/architecture/transport.md`, on-disk segment storage to `docs/goal/architecture/data-flow.md`, and per-feature endpoint behavior to `docs/goal/behavior/*.md` (e.g. `bridges.md`, `conversations.md`).
> **Current state:** the WS-RPC-everywhere migration of authenticated UI control-plane traffic is **complete** — every authenticated request/reply rides the per-actor WS-RPC connection; HTTP survives only on the § HTTP residue inventory below plus the open items in § Implementation status today. Code-verified 2026-07-07 (cluster #2 goal-docs review) against `bins/fauna-nest/src/lib.rs` `build_router`.

## Goal

Every fauna-nest endpoint belongs to exactly one consumer layer. Apps (web, android, iOS, macOS, windows, linux, tui) consume Layer 1 (Core Client) for the unified experience and Layer 2 (Protocol Client) only for genuinely protocol-specific features; Layer 3 (Bridge Management) is the uniform path for managing external-protocol bridges; Layer 5 (Admin) is for the admin panel (admin is a Fauna app); Layer 6 (Federation) is server-to-server (ActivityPub, CalDAV, MTA-STS); Layer 7 (Internal) is intra-deployment plumbing. (Layer 4, the bridge proxy, was **deleted** at the I6 mail-bridge cutover — bridges are peer clients of nest, `architecture/apps/bridges.md`.) Apps reach for Layer 1 first, fall through to Layer 2 only when the feature is truly protocol-unique, and never call Layer 6/7 directly. This separation is the contract that prevents duplicate work and lets WS-RPC migrations happen layer-by-layer without breaking clients.

## The four API protocols

Fauna's network surface is exactly four protocols. Every endpoint
classified by Layer below rides on one of them.

| Protocol | Purpose | Where |
|---|---|---|
| WS-RPC | All authenticated UI traffic (Layer 1, 3, 5; admin is a Fauna app per the product invariant) | one WebSocket per actor session at `GET /api/v1/ws/{actor_id}`, and a third-party principal's session at `GET /api/v1/principal/ws` (`docs/goal/architecture/transport-connection.md` § Connection lifecycle → *The principal session*); framing per `docs/goal/architecture/transport.md` |
| HTTP residue | External-standard-mandated only | the residue list below; never grows for non-external reasons |
| Federation HTTP | ActivityPub / WebFinger / NodeInfo (Layer 6) | matches the federation spec; JSON-LD shape per RFC; classified under Layer 6 below |
| Blob HTTP | CID-addressed bytes | `GET\|PUT /api/v1/blob/{cid_b32}` with `application/octet-stream`; base32-lower CIDv1 in URL path (`b…` prefix per `docs/goal/architecture/serialization.md` § CID shape); server-side `blake3(body) == cid.digest()` verification on both directions (`bins/fauna-nest/src/blob_routes.rs`). **The canonical shape for new callers** — every current client/feature (incl. FaunaMls conversation attachments) uploads/downloads through the hex-keyed multipart pair instead, which is equally permanent (§ Remaining HTTP; § Blobs & Media below owns the full inventory) |

Bytes on every WS-RPC frame are canonical IPLD dag-cbor per
`docs/goal/architecture/serialization.md`. Signed payloads ride as
embed-as-bytes envelopes (raw canonical bytes alongside a
`SignedEnvelope { cid, sig }`); receivers verify by hash + signature
without re-encoding.

## HTTP residue — the single inventory

**This section is the single owner of the HTTP-residue inventory**
(one-owner rule, cluster #2 ratification 2026-07-07):
`docs/goal/architecture/transport.md` § HTTP residue defers here, and
the former second copy at the bottom of this doc is gone. Every entry
is externally forced (RFC-mandated, an off-the-shelf convention, a
non-Fauna far end, or byte-bulk content) or a deliberately-public
unauthenticated read; nothing here is migration-pending. Every other
endpoint that previously existed in this doc has a WS-RPC successor
(`fauna.<area>.<verb>` per transport.md § Namespace policy); see the
per-group notes in Layers 1, 3, and 5 below. Reconciled against
`bins/fauna-nest/src/lib.rs` `build_router` 2026-07-07.

**JSON- or HTML-bearing residue:**

| Endpoint | Why |
|---|---|
| `/ap/*` | ActivityPub JSON-LD (the far end is a third-party AP server) |
| `/.well-known/webfinger` | WebFinger, RFC 7033 |
| `/.well-known/nodeinfo`, `/nodeinfo/2.1` | NodeInfo discovery + the document it links to |
| OAuth callbacks — bluesky `auth/callback`, `/.well-known/atproto-oauth-client` | OAuth 2.0 spec (the far end is Bluesky's OAuth server) |
| `GET /health`, `GET /version` | Off-the-shelf monitoring convention (not used by Fauna apps) |
| `GET /api/v1/posts/{post_id}` | Public-byte cross-nest post-body fetch — the discovery-feed `fetch_url` `peer_query.rs` builds; a client→nest public read, not a federation call. Local clients use `fauna.posts.get`. |
| `GET /api/v1/subscriptions/tiers/{author_id}`, `GET /api/v1/subscriptions/delegate/{author_id}`, `GET /api/v1/nest/info` | Deliberately public, unauthenticated subscription reads (Pillar-2 web-paywall / external consumers — `monetization.md` § Pillar 1). Authenticated clients read the same data over `fauna.subscriptions.{offers.list,tiers.list}`. |
| `POST /api/v1/payments/webhook/{author_id}/{provider}` | Payment-provider webhook ingress — the far end is a third-party payment provider that cannot speak WS-RPC (`monetization.md` § Pillar 3 owns the adapter/verification model). Compiled in only under the `payments` cargo feature — a store-safe build does not have the route at all, not merely a refusal (`dynamic-features.md` § What "completely compiled away" means). |
| `/.well-known/oauth-authorization-server`, `/.well-known/openid-configuration`, `/oauth/jwks`, `/oauth/{par,authorize,authorize/poll,token,revoke}` — **built on the nest (TP5 slices S1–S2, complete)**; `/oauth/userinfo` — **built on the nest (TP6)**; `/oauth/{device_authorization,bc-authorize}` — **built on the nest (TP9)** | OAuth 2.1 / OIDC / RFC 8628 / CIBA — the far end is a third-party OAuth client (`behavior/authorization-server.md` § The issuer owns the surface; this row classifies it). The residue rule's external-standard-mandated arm, exactly as the bluesky callbacks above. |
| `/api/v1/records/{kind}[/{key}]`, `/api/v1/folders/{id}/deposit`, `/api/v1/events` — **ratified 2026-09-05; the deposit door built 2026-10-03, the record door 2026-10-05, the events long-poll and its webhook 2026-10-05** | The third-party integration chain's three remote-server doors: the far end is a third-party server that cannot speak WS-RPC (the payment-webhook arm, one row up), bearer = a DPoP-bound access token. Device apps and hosted code use the WS-RPC kinds instead; the HTTP door never grows for a Fauna app. Owners: `architecture/third-party-kinds.md` § The record doors (records), `behavior/file-sync.md` § Third-party deposit ingress (deposit), `architecture/transport.md` § Push events → *Third-party event doors* (events). **Re-decided here, per this section's own rule:** admitted as residue because the far end is external and cannot ride WS-RPC — never because HTTP was convenient; a fourth door needs the same argument. |
| `GET\|POST /list/unsubscribe?t=<token>` | RFC 8058 mailing-list unsubscribe — GET renders a confirm page, POST is the One-Click action; consumed by external MUAs/browsers, no Fauna app (`lib.rs` `list_unsubscribe_{get,post}`; `mail-mass-mailing.md` § The HTTPS endpoint) |
| `GET /nostr` (WebSocket), `GET /nostr/info`, `GET /.well-known/nostr.json` | Nostr relay + NIP-05 identity — NIP-01 relay WS, NIP-11 relay info, and NIP-05 `?name=` handle→pubkey lookup (`bins/fauna-nest/src/nostr/mod.rs`); its own protocol for non-Fauna Nostr clients, not Fauna's WS-RPC |
| `/share/{token}` | Public HTML landing pages for non-Fauna recipients (revoked → `410 Gone`; control plane is `fauna.share.*`); a fragment-keyed private link adds `/share/{token}/manifest` and `/share/{token}/chunk/{index}`, ciphertext only (`../behavior/share-links.md` § The private-file extension) |
| Published web-content pages | Public HTML served to non-Fauna browsers (`web-content-hosting.md`) |

**Non-JSON (byte-bulk / XML) residue:**

- `/caldav/*` + `/.well-known/caldav` — XML, CalDAV protocol-required.
- `/.well-known/carddav` — CardDAV discovery redirect (301 to the mail-bridge MDA's CardDAV store, or 503 until enabled), the CalDAV apex's sibling (`bins/fauna-nest/src/carddav_bridge.rs::wellknown_carddav`; `carddav-server.md` is the authority).
- `/.well-known/webdav` — WebDAV files-apex discovery redirect (same 301/503 shape, `bins/fauna-nest/src/webdav_bridge.rs::wellknown_webdav`; `webdav-server.md` is the authority) — not an RFC-numbered convention like the other two, but the same external-client-discovery necessity (no SRV record for WebDAV).
- `/.well-known/host-meta` — XML per RFC 6415.
- `/.well-known/mta-sts.txt` — MTA-STS policy text.
- `GET\|PUT /api/v1/blob/{cid_b32}` — `application/octet-stream`; base32-lower CIDv1 in URL path (`b…` prefix); canonical byte-source surface, server-side `blake3(body) == cid.digest()` verification on both directions (`bins/fauna-nest/src/blob_routes.rs`). The hex shape (`POST /api/v1/blob`, `GET /api/v1/blob/{hex}`) shares the same `BlobStoreBackend` and is **permanent** (ruled 2026-10-01): every app uploads and downloads through exactly this shape today, so it is a public URL from the 2026-10 baseline on and could only be retired at a major (§ Blobs & Media below owns the full inventory).
- `GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}[/meta]` — the permanent segment byte route (ruled 2026-10-01, `segment-backup-protocol.md` § Byte-source endpoint); the base route serves the CARv2 `.dat`, the `/meta` sibling its dag-cbor `record_order` sidecar (an adopting replica's bootstrap needs it, a backup pass does not) — both owner-or-custodian-authed (`bins/fauna-nest/src/segments/segment_route.rs`; `crate::custody_admission`).
- `GET /api/v1/export` — streaming `application/zip` of all user data; no JSON control plane to split.
- `GET /api/v1/export/{session_id}` — one completed mailbox export's sealed blob, streamed as `application/octet-stream` (`bins/fauna-nest/src/export_routes.rs::handle_export_session_blob`; `behavior/mail-export.md` § Download flow is the authority). Session-bearer authed and owner-scoped — the session lookup filters by the authenticated actor, so a foreign or non-`completed` session answers 404 rather than confirming it exists; the bearer's standing and the owner's download notice are § Download flow's (*Standing and the owner's notice*). Byte-bulk because the per-session ceiling is 10 GiB; the bytes are framed ciphertext the nest cannot read, so there is no control plane to split off.
- `GET /api/v1/admin/export/all` — admin bulk export (zstd tar).
- File-sync byte plane — `/api/v1/chunks/*` and `/api/v1/manifests/*` (`file-sync.md` § Chunk size and transport contract). The `GET /api/v1/sync/ws` data-plane WebSocket was **removed 2026-10-02** (`file-sync.md` § Relay serving → *The `/sync/ws` data plane leaves with the daemon*). The server-side single-file reassembly route (`GET /api/v1/sync/file/{*path}`) was **deleted 2026-07-14** — never a production caller; the unconditional owner-only chunk seal makes nest-side plaintext reassembly impossible (`backup-restore.md` § 3).
- Snapshot bytes have no route of their own: they are the file-sync byte plane above, reassembled client-side where the keys live (`backup-restore.md` §§ 3–4). The two legacy-plaintext routes (`POST /api/v1/snapshots/{id}/restore` streaming ZIP, `GET /api/v1/snapshots/{id}/file/{path}`) were **deleted 2026-09-27** in the compat-remnant sweep (`core-client-kind-catalog.md` § Snapshots records them).
- Video/media bytes — `/api/v1/video/*` (HLS), `/api/v1/media/proxy`, bluesky `media?url=` CDN proxy.
- Moderation model bytes — `GET /api/v1/moderation/{model,vocab}` (ONNX binary + tokenizer vocab) — **retired 2026-10-02** (`content-scoring.md` § The placement matrix → *Deployment-wide content models at the client position*); the routes are deleted, so the residue list holds no moderation entry.

Byte-bulk surfaces exceed the WS-RPC frame budget (transport.md
§ Wire format); they never become WS-RPC payloads.

## Remaining HTTP — verified snapshot (2026-05-23; re-verified 2026-06-19, 2026-07-07, and 2026-07-24)

Code-verified against `bins/fauna-nest/src/lib.rs` `build_router` + the
registered kind groups (`build_rpc_router`, `lib.rs:1313-1404`). Every authenticated UI
request/reply *not* below is already on WS-RPC. What remains HTTP, by
**why** (this is the migration-status axis the residue list above does
not capture — the residue list is bucket 1 only):

**Re-verified 2026-06-19:** the client↔nest
**control-plane** rip is complete. The seven vestigial `NestClient` HTTP methods
in `libs/fauna-client` (`node_info`, `check_handle`, `get_inbox`, `get_account`,
`get_groups`, `get_knocks`, `get_contacts`) had **zero callers** — every app
reads the successor kinds via the per-feature crates (`fauna-anon-client`,
`fauna-client-{inbox,account,contacts,conversations}`) — and were **deleted**;
their HTTP routes were already gone. The `search` / `spam` / mutating-`subscriptions`
twins are likewise **deleted** (no route remains). What stays HTTP: the permanent
residue (bucket 1 below), the deliberate **public unauthenticated** subscription
reads (§ Subscriptions). The one **admin-track** laggard this rip left —
`/api/admin/bridges/{pending,approve,revoke}` (AdminBearerAuth bridge-enrollment) —
is **removed** too: bridge enrollment is `fauna.bridges.request_enrollment` plus
the admin's `approve_pending_bridge` / `reject_pending_bridge` / `revoke_service_user`
(`mail-bridge-lifecycle.md` § The retired pre-registration path).

1. **Permanent residue** — the two lists above (byte-bulk; ActivityPub /
   WebFinger / NodeInfo / OAuth; CalDAV / host-meta / MTA-STS; health).
   Never migrates.
2. **Auth bootstrap + discovery — kind shipped, HTTP twin removed (Track A
   complete; S4 retirement COMPLETE).**
   `/api/v1/{auth/token,auth/challenge,auth/verify,register,node-info,
   handle-available,resolve-node,actor/by-handle}` ride the **anonymous** WS
   (`GET /api/v1/ws`). **No auth HTTP twin remains** — `auth/token`, the last
   `deprecated_http` control-plane twin, was **deleted at the rip-out endgame**
   once every app minted its bearer over the `fauna.auth.handshake` kind (the
   shared FFI `mint_bearer` / `mint_bearer_over_handshake` — a faithful 1:1 of
   the old HTTP route, the tagged handshake sign + lockout /
   unregistered-actor-reject side effects): **android**,
   **apple** (`APIClient.authenticate` → `mintBearer`, 2026-06-13), and
   **windows** (`DirectNestClient` → `MintBearer`, 2026-06-13) all landed; the
   apple tier_3 `SyncIntegrationTests`/`FFICompatTests` helpers migrated as part
   of the route-delete commit; the Go mail-bridge never POSTed it (it rides
   `fauna.auth.{challenge,verify}`). The `auth/{challenge,verify}` twins were
   **deleted** earlier once their last consumer (apple `APIClient.silentSignIn`)
   migrated to the WS-RPC kinds. Everything else here is **WS-only, HTTP twin deleted**:
   `register` (S4f) → `fauna.account.register`; discovery
   (`node-info`/`handle-available`/`resolve-node`/`actor/by-handle`) →
   `fauna.{nest.info,handle.available,nest.resolve,actor.by_handle}`; the
   `setup-status` + `storage-mode` twins (S4c2), `invite-code/verify` (S4b),
   `invite-requests*` (S4a2), and `claim-admin` (S4d) → their `fauna.setup.*` /
   `fauna.account.*` / `fauna.auth.claim_admin` kinds. (The RFC-8628
   device-code/-token bootstrap was likewise removed — replaced by the
   challenge-auth flow.)
3. **Content kinds shipped, HTTP twin DELETED.** bridges, email (send +
   filters), conversations, feed, posts, contacts, knocks, inbox-mode,
   notifications, account, config (config has no twin — WS-RPC-only).
   `search` / `spam` / mutating-`subscriptions` twins were since **deleted**
   (verified 2026-06-19 — no `search`/`spam` HTTP route remains; only the
   public *unauthenticated* `subscriptions` reads survive, § HTTP residue).
   **All seven apps have adopted the kinds** (re-verified 2026-07-07; the
   per-cluster adoption notes in the sections below carry the per-app
   detail — the former "Apple/Windows/Android broken against current nest"
   state is resolved).
   The cross-nest federation `keypackage` fetch (`GET /api/v1/keypackage/{actor}`),
   `welcome` deliver, and `feeds/query` were the previous bucket-5 federation
   residue; they were **retired in Spec Y2 slice 5 (2026-06-03)** and now ride
   solely the nest↔nest WS-RPC federation channel (`federation.md` is the
   authority). The one cross-nest read that **stays HTTP** is `posts/{id}`
   (public-byte residue — the client's discovery-feed post-body fetch, not a
   nest↔nest federation call; see bucket 5).
4. **Migratable NOW — rides the existing per-actor bearer WS; needs only a
   kind (or twin-deletion), no new transport.** sessions; `context`;
   **file-sync** metadata (`sync/*`, `folders/*`, `snapshots` control,
   `sync/conflicts`); moderation; **labels** (`fauna.labels.{attach,list}`
   shipped; twins **deleted** — no consumer — in the residue sweep (tracked
   internally)) — the `engagement` and `context` entries in this bucket are
   **obsolete**: `fauna.engagement.{record,list}` and the interest-profile
   `context` read were deleted outright (D9, 2026-07-12), never migrated;
   `access-grants` (B10a done, and the kinds themselves **deleted** in the same
   D9 pass;
   `algorithm/*` + `reputation/{exchange,export}` reclassified **out** of this
   bucket — `algorithm/*` is service-to-service residue, `reputation/{exchange,export}`
   is bucket-5 federation (retired in Spec Y2 slice 5 onto the federation channel as
   `fauna.federation.reputation.{exchange,export}`), B10b); calendars
   + events; `web/{publish,domain}`;
   `storage/migrate*`; pending-actions; `stats`; `files/{h}/versions`
   (list); **`wireguard/peer`** (client + `fauna-sync` register — both; the kinds that replaced these twins were themselves deleted 2026-08-23,
   bearer-authed); admin `/admin/api/*` (Layer 5 — admin is a Fauna app
   per the product invariant). The bucket-3 twin-deletion cleanup
   (search / spam / subscriptions mutating twins) is **DONE** — those HTTP
   routes were deleted (verified 2026-06-19; only the public unauthenticated
   `subscriptions` reads survive). `nostr` /
   `bluesky` = **Layer 2, deferred** (protocol-specific).

   **Residue-sweep close-out (2026-06-03, tracked internally):** the migrated twins with *no remaining consumer* — `labels`, `engagement`, `access-grants`, `pending-actions` (+ admin `/admin/api/pending-actions`), `storage/migrate*`, `stats`, `files/{h}/versions`, `web/{publish,domain}` — had their HTTP routes **deleted** (the WS-RPC kinds are the sole surface; `web/*` keeps its transport-agnostic cores). The other bucket-4 twins then stayed until their lagging client migrated (S3b); since then **every one of them has been deleted too** (re-verified 2026-07-07): `wireguard/peer`, the moderation control-plane, the snapshot control-plane, **sessions** (`fauna.sessions.*` sole surface), the **file-sync control plane** (`fauna.sync.*` / `fauna.folders.*` sole surface; only the byte plane stays, § HTTP residue), the **push-subscription twins** (`fauna.push.*` sole surface), and the whole **calendars/events plaintext plane** (retired outright — Decision-B § 4c, see § Calendars & Events). Bucket 4 is **empty**: no `#[deprecated]` twin remains anywhere in `build_router`.
5. **Federation surface — Spec Y2 nest↔nest WS-RPC channel (BUILT; HTTP interim
   RETIRED in slice 5, 2026-06-03).**
   Peer-auth model + surface inventory: **`docs/goal/architecture/federation.md`**
   (the authority); design history ratified 2026-05-30 (tracked internally).
   The auth model (mutual nest-key sign-over-CID) is settled and is an orthogonal
   axis from the carrier. The carrier migration is now **complete**: the long-lived
   nest↔nest WS-RPC federation channel (`GET /api/v1/federation/ws`, subprotocol
   `fauna.federation.v1`, kinds `fauna.federation.*`) is the **sole carrier** for
   every cross-nest Fauna↔Fauna call. The HTTP interim that carried them during the
   migration was **retired in Spec Y2 slice 5 (2026-06-03)** — these HTTP routes are
   now **deleted**, not deprecated twins:
   `feeds/query` (`fauna.federation.feed.query`), `keypackage/{actor}` fetch
   (`fauna.federation.keypackage.fetch`), `welcome/{actor}` deliver
   (`fauna.federation.welcome.deliver`), `forward` (`fauna.federation.post.forward`),
   `nest-sync/{pull,push,mls-pull,mls-ack}` (`fauna.federation.sync.{pull,push,mls_pull,mls_ack}`),
   `reputation/{exchange,export}` (`fauna.federation.reputation.{exchange,export}`),
   `calendar-invite-deliver/{actor}` (`fauna.federation.calendar.invite_deliver`),
   `remote-rsvp-deliver` (`fauna.federation.event.rsvp_deliver`). The pairing handshake
   (`pair[/revoke]`) was already retired under the per-user-pairing design (§ Nest Pairing
   & Sync); `pairings/{id}` is a client read (`fauna.pair.list`, kept as `deprecated_http`).
   These all carry **no actor** — the per-actor client WS-RPC channel is bearer-authed —
   so they ride the dedicated nest↔nest channel (peer auth = mutual nest-key), never the
   client surface. The encoder debt **D1** was closed (2026-05-25): the pairing + nest-sync
   wire signs/verifies over **canonical dag-cbor** of shared typed structs
   (`bins/fauna-nest/src/federation_sig.rs`), receiver re-deriving `blake3(bytes)==cid`.
   **The one cross-nest read that STAYS HTTP is `posts/{id}`** — a public-byte client→nest
   read (the discovery-feed `fetch_url` `peer_query.rs` builds, like `/api/v1/blob`), NOT a
   nest↔nest federation call, so the federation channel does not carry it. A
   `fauna.federation.post.get` channel kind exists for a future server-side originator, but
   none exists today. See `federation.md` and `transport.md` § Future directions.

(External / local-sidecar HTTP — rspamd `POST /checkv2`,
TLSRPT report POST, the push relay (§ Push relay below),
DNS-provider provisioning, GitHub-releases updater — is not client↔nest and
is out of WS-RPC scope. **The nest↔algorithm-sidecar mesh** — `/api/v1/algorithm/{classify,reputation,trust-context}`
nest→sidecar + `/internal/algorithm/{reputation,report,reports}` sidecar→nest — was
classified here as out-of-scope "service-to-service residue" through 2026-06-08; the
**user ruled it IN-scope for the rip on 2026-06-08** (it is loopback co-located sandboxed
IPC, but it is still non-bulk-binary production HTTP). It migrated to a bidirectional
**internal sidecar WS-RPC channel** — see § Algorithm & Reputation and
`transport.md` § Future directions; **both ends ride the channel (the live carrier) as of 2026-06-08 (tracked internally — steps 1–5 + tier_3); the internal mesh twins (`http_algorithm_client` + `/internal/algorithm/{reputation,report,reports}`) were deleted 2026-06-09 (step-6 part A)**. ⚠ The public `/api/v1/algorithm/{reputation,trust-context}` routes were the algorithm `service`'s **only** consumer — built-ahead scaffolding, no ranking/moderation reader. The user kept them as scaffolding 2026-06-09 (the sidecar-only dead handlers + `[algorithm].url` were cleaned up, step-6 part B); the 2026-10-01 survey's dead-shape ruling then **removed all four public routes 2026-10-01** ([`core-client-kind-catalog.md`](core-client-kind-catalog.md)), so the service and its channel were left booting with **no consumer at all**, and the rest of the mesh was **removed whole the same day** (2026-10-01; the ruling, what went and what stays are [`core-client-kind-catalog.md`](core-client-kind-catalog.md) § Algorithm & Reputation's). The same intra-deployment principle has since consumed the other two surfaces once flagged for a user decision: the worker's `/internal/proxy-status` no longer exists (the live route is `/internal/router-status`, plus the `/internal/{worker,relay}/ws` channels — § Layer 7), and the mail-bridge `/api/v1/email/{deliver,validate-recipient}` plumbing was deleted at the I6 cutover (the Go bridge talks WS-RPC directly).)

### Push relay

`bins/fauna-push-relay` is a standalone first-party service (not part of a nest
deployment) that wakes a roaming peer through APNs. It is external HTTP by
design — out of WS-RPC scope, like the rest of this parenthetical. Its routes:

| Route | Signed? | Purpose |
|---|---|---|
| `PUT /v1/push-token` | yes, `push-token/put` domain | Register/update an actor's push token — `actor_id`, `platform`, `push_token` |
| `DELETE /v1/push-token` | yes, `push-token/delete` domain | Unregister |
| `POST /v1/wake` | yes, `wake` domain | Ask the relay to wake a target actor; returns a nonce |
| `GET`/`POST /v1/endpoint/{nonce}` | no — the nonce is the capability (ruled 2026-08-15, below) | Poll for / report the woken peer's endpoint |

**Signing encoding (normative — any reimplementation must reproduce these bytes).**
Each signed message is the concatenation of its elements where **every element,
the leading domain tag included, is emitted as an 8-byte big-endian length
followed by its UTF-8 bytes**. Elements in order:

- `push-token`: `<domain> actor_id platform push_token timestamp`
  where `<domain>` is `fauna-push-relay/v1/push-token/put` or
  `…/delete`, and `timestamp` is the `u64` in canonical decimal.
  ⚠ **A `wg_public_key` element sat between `push_token` and `timestamp` until
  2026-08-26** — the last WireGuard carrier in the tree, outliving the stack's
  deletion by three days because the routes table's own parenthetical claimed
  it was gone while this spec (and `db.rs`/`api.rs`) still required it. Do not
  reintroduce it: whatever binds a push token to key material is mobile push's
  design to make, and it is unrelated to WireGuard.
  ([`version-compatibility.md`](version-compatibility.md) § Dimension 2 carries
  the removal's evidence — the route has never had a caller in any commit on
  any branch.)
- `wake`: `fauna-push-relay/v1/wake target_actor_id requester_actor_id requester_endpoint timestamp`.

The signature is Ed25519 over those bytes, verified against `actor_id`
(`requester_actor_id` for wake) as the public key, **`verify_strict`, with
small-order keys refused**. Two properties are load-bearing and must survive any
future change: the length prefixes make the encoding **injective** (a bare
concatenation of adjacent variable-length fields lets an observer re-split one
valid signature into a different field tuple), and the **per-operation domain tag**
keeps a `put` signature from authorizing a `delete`. Golden vector pinned on both
ends: `signing_bytes_is_the_documented_golden_vector`
(`bins/fauna-push-relay/src/api.rs`) and
`signing_bytes_matches_the_relays_golden_vector`
(`libs/fauna-peer/src/relay_client.rs`).

**Freshness (normative — a signature alone does not authorize a request).** Every
signed route refuses a request whose `timestamp` is more than **300 seconds**
from the relay's own clock **in either direction**; the boundary is
**inclusive**, so exactly 300 s of skew is accepted and 301 s is not. A
reimplementation of this wire must therefore stamp `timestamp` with real unix
seconds at send time and re-stamp (and re-sign) on retry — a request held and
replayed later is refused, which is the entire point. The refusal is `401` with
an explanatory body rather than a status of its own: a client whose clock has
drifted is the one caller that needs to tell this apart from a bad key, and a
replayer already knows the timestamp it captured.

Three things about that window are decisions, not defaults. **It is symmetric**
because a post-dated timestamp is the same attack with the sign flipped — sign
once at `now + 10 years` and the request replays until then. **It is 300 s
specifically** to match the wake rate limiter's nominal window
(`db.rs::has_recent_wake`), so the relay has one time constant rather than two
that drift apart. And **300 s is deliberately generous** as skew allowance — the
conventional figure for signed-request schemes — because a device whose clock is
minutes off must still be able to register for push; refusing it would break
out-of-the-box setup on exactly the devices least able to diagnose why. Checked
**after** signature verification, not before: an unverified timestamp is not a
statement about anything. Pinned by
`the_freshness_window_is_inclusive_and_symmetric` (the boundary, with no clock
involved), `the_freshness_window_matches_the_wake_rate_limit_window`, and one
refusal pin per signed route.

**Two retentions, deliberately different — and they were one until 2026-08-15.**
A wake produces two records, and they expire on their own schedules:

| Record | Lives | Because |
|---|---|---|
| the **nonce** (`pending_wakes`) | 60 s | it is a bearer capability — whoever holds it can read and set that wake's responder endpoint, so it wants the shortest life the handshake tolerates |
| the **rate-limit history** (`wake_history`) | 300 s | it is how long a target stays protected from a second wake, and must outlive the nonce |

They shared one table, which made the limiter silently
ineffective: the cleanup task deletes at the nonce's 60 s, so
`has_recent_wake`'s 300 s lookback only ever saw the last ~60–90 s of history —
**a ~60 s limiter wearing a 300 s doc comment**, and nothing observed it,
because the only test asserted that an *immediate* duplicate is refused, which
passes at either value. The ordering is now a **compile-time** assertion
(`NONCE_TTL_SECS < WAKE_RATE_LIMIT_SECS`): raising the nonce TTL to the
limiter's window would restore the coupling and quietly extend a capability's
life fivefold, so it does not build. Both planes stay bounded — history is a
rolling window collected by the same task, never a log of every wake the relay
has seen — and a wake's two rows are written in one transaction, since a crash
between them would either hand out a nonce the limiter never heard of or record
a limit against a wake that never happened.

**The endpoint pair is unauthenticated on purpose — the nonce IS the capability
(ruled 2026-08-15).** `GET`/`POST /v1/endpoint/{nonce}` carry no signature in
either direction: the requester polls with the nonce the wake returned, and the
woken target reports with the nonce its push payload carried. Three things make
that the right shape rather than a hole. The nonce is a **122-bit random**
UUIDv4 held only by those two parties. The value it guards is a **routing hint,
not a secret or an authorization** — the peer connection that follows is
authenticated by key, so a forged endpoint yields a failed handshake, never a
redirect an attacker can exploit; there is nothing here worth signing for. And
the capability is **short-lived by construction**: the nonce dies with its wake
at `NONCE_TTL_SECS`, which is precisely why that TTL is kept short and is now
compile-time-asserted to stay below the rate-limit window.

What must hold, and is pinned by `a_nonce_buys_nothing_about_another_wake`, is
that holding *a* nonce buys nothing about *another* wake — neither a read nor a
write. Writing that assertion is what surfaced the one real defect on this pair:
`POST` used to answer `200 OK` to a nonce matching no wake, so a reporter could
not tell a landed endpoint from one that went nowhere, and the capability
boundary was unobservable. It now answers **404** when no such wake exists
(unknown nonce, or one whose wake has expired). Anyone tempted to "harden" this
pair into signed requests should read this paragraph first: the asymmetry is
deliberate, and the fix for a nonce that leaks is a shorter TTL, not a signature
over a public IP:port.

⚠ **What freshness still does not close.** With the limiter honest, a captured
wake is rate-limited for the whole 300 s it remains fresh, so it buys an
attacker nothing on the wake route. What remains is the ordinary consequence of
allowing any skew at all: within its 300 s window a captured **`push-token`**
request can still be replayed, re-registering the same token the victim had
already registered. That is bounded and idempotent rather than harmful, and it
is the price of a skew allowance generous enough that a device with a drifting
clock can still register for push — see the three decisions above.

> **Implementation status today.** The relay ships as a release artifact
> (`release.yml`, in the public-files tree since 2026-08-31), but the wire has
> **no deployed producer**:
> nothing in-tree signs a `PUT`/`DELETE /v1/push-token` at all, and the only wake
> producer (`libs/fauna-peer` `WakeRequest::sign` via `DeliveryOrchestrator`) is
> constructed solely in that crate's unit tests. Gap (1) — no replay/freshness
> check — **closed 2026-08-15**: all three signed routes now enforce the window
> above, and the wake producer re-stamps on every call
> (`RelayClient::wake` builds a fresh `WakeRequest`), so nothing in-tree holds a
> pre-signed request across time. Gap (3) — the `/v1/endpoint/{nonce}` pair
> being unauthenticated by design but undeclared — **closed 2026-08-15**: the
> nonce-as-capability reading is now ruled and stated above, and pinned. Gap (2)
> — the nest's security-notification push channel — is **closed 2026-08-15**
> too, by deletion; see the block below. **All three gaps recorded here are now
> closed**; what remains true is the header above — the wire still has no
> deployed producer. The relay's sender is APNs (or a log-only stand-in when no
> credentials are set); the never-constructed FCM sender was deleted 2026-10-02
> — no owner doc calls for an FCM wake path — and no sender logs a push token.
>
> **Gap (2) is CLOSED 2026-08-15 — by deletion, and the reasoning binds any
> future nest→relay call.** The nest's security-notification channel 2
> (`bins/fauna-nest/src/security_notify.rs`) POSTed to `/v1/notify`, a route this
> relay has never implemented. It was removed rather than built, for two reasons.
> **The payload was unauthenticated** — plain JSON with no signature, which is
> reason enough not to serve it. **And its URL came
> from a `--push-relay-url` CLI flag no shipped artifact ever set**, so the
> channel never fired in any real deployment; because this relay is a first-party
> service and not part of a nest deployment, no user or admin ever chooses that
> URL, which made the flag the banned operator tier rather than a configuration
> surface (`nest/common.md` § Web Push). Nothing user-visible was lost: security
> notices reach the user through the guaranteed inbox message, the `notifications`
> row the apps render, and the sealed INBOX email — pinned by
> `every_security_notice_reaches_the_user_without_a_push_relay`. **The rule this
> leaves behind:** a nest that later needs to call this relay presents a *signed*
> message in the same shape as `push-token`/`wake` and reaches a compiled-in
> first-party URL — never an unsigned payload, never a flag. What identity a nest
> signs with is still open (the relay has no nest registry, knowing only actor
> pubkeys); that question is answered against a real requirement when a mobile
> push producer lands, not in advance.

## Migration narrative

Layer 1 surfaces (UI-authenticated HTTP) migrated per the
CBOR-DAG-everywhere rewrite § Layer sequencing (design ratified
2026-05-15; tracked internally). Layers 2–5 of that spec handled the actual flips
— canonical codec, CARv2 at-rest, WS-RPC content flip, kind catalog,
HTTP endpoint flip; this doc ratifies the target shape only. Each
previously-JSON authenticated UI endpoint noted in the per-group
sections below carries a `→ WS-RPC kind <kind.name>` pointer to its
successor; the kind names follow `fauna.<area>.<verb>` per
`docs/goal/architecture/transport.md` § Namespace policy. **Twin-removal
policy (ratified 2026-07-07, replacing the earlier telemetry-gate
wording): a deprecated HTTP twin is deleted once every app consumes
the WS-RPC kind (verified per-app at the rip-out); no telemetry
gate.** Per-feature, not big-bang — and as of 2026-07-07 the policy has
run to completion: no deprecated twin remains.

## Quick Reference

| Layer | Software | User | Path Pattern | Auth |
|-------|----------|------|-------------|------|
| Core Client API | Apps (web, android, iOS, macOS, windows, linux, tui) | End users | `/api/v1/*` (most routes) | Bearer token |
| Protocol Client API | Apps | End users | `/api/v1/bluesky/*` (residue — ActivityPub's twin was ripped 2026-07-16; Nostr's 2026-07-22) | Bearer token |
| Bridge Management API | Apps | End users | `/api/v1/bridges/*` | Bearer token |
| Bridge Proxy (deleted) | — | — | ~~`/api/v1/bridge/{bridge_name}/*`~~ deleted at the I6 cutover | — |
| Admin API | Apps (admin panel) | Nest admins | `fauna.admin.*` WS-RPC kinds (former `/admin/api/*` twins deleted) | Admin caller class |
| Federation API | Remote servers (AP, CalDAV) | None (server-to-server) | `/ap/*`, `/.well-known/*`, `/caldav/*` | Varies |
| Internal API | Co-located nest services | None (automated) | `/internal/{worker,relay}/ws`, `/internal/router-status` | Internal only |
| Public API | Any HTTP client | Anyone (unauthenticated) | `/share/*`, the public subscription reads, `GET /api/v1/posts/{id}` | None |

The "Path Pattern" column reflects the historical HTTP shape; per the
CBOR-DAG-everywhere migration, every authenticated layer (1, 3, 5)
rides the single per-actor WS-RPC connection at
`GET /api/v1/ws/{actor_id}` instead. The former HTTP twins are
**deleted** per the twin-removal policy above; § HTTP residue is the
complete inventory of what stays HTTP.

## Layer 1: Core Client API

These are the primary APIs every app should use. They provide the unified experience — all content normalized regardless of origin protocol.

**Caller-class authorization (`bridge_method_allowlist::is_permitted`).** Every WS-RPC kind is gated by the connection's caller class — `User`, `Admin`, `BridgeMta`, `BridgeMda`, `Custodian`, `ContentProcessor`, `BridgeAtprotoPds` (the bridge classes derive from a bridge's service-user enrollment; full catalogue owned by [`apps/bridges.md`](apps/bridges.md) § Capability-allowlist enforcement). `Custodian` is narrower than the others — a live custody-capability row, not an enrollment, and its only reach is `fauna.sync.changes.list` + `fauna.segments.list` + the segment-byte HTTP GET routes above, re-derived per request so a revoke severs a live session at its very next dispatch (`account-replica-posture.md` § The custody grant + ceremony owns the model). **`Admin ⊇ User`:** an admin is a user who additionally holds the (grantable/revocable) admin role, so a caller classified `Admin` **inherits every `User` permission** — `is_permitted` returns true for an `Admin` whenever it does for a `User`, and kinds name only the *minimum* class. This is what lets the claimer-admin of a personal single-user nest send/read its own mail, run conversations, manage its own bridges, etc. (memory `admin-is-a-user-with-extra-role`). It is safe because **every user-facing kind is caller-scoped** — it operates on the connection actor (`target == caller`), never an arbitrary `actor_id` from the request body — so an admin only ever reaches its OWN data, never another user's, even on a multi-user nest (and on an encrypted nest a user's data is sealed to their own keys regardless). The handful of kinds that *do* take a `target` (e.g. `provision_recipient_mls_pubkey`, `provision_calendar`) enforce `target == caller` for non-bridge callers in the handler; the bridge classes may act on behalf of a served user. **Never add a User kind that returns or mutates another actor's data by an `actor_id` param** — that would break the invariant the admin-inheritance rests on. The converse direction — that an `Admin` is never left *without* the `User` half — is enforced at **both ends of the role's life**, since `is_admin` reads `admin_actor_ids`, a table neither user-creation nor user-deletion touches: `fauna.admin.admins.add` refuses to promote a target that holds no `users` row (at the door, and again in the pending-action executor, which can outlive the upgrade that added the door), and every deletion path refuses an actor still holding the role (scope + mechanism: [`../behavior/admin.md`](../behavior/admin.md) § 2 Users → *Cutting a user off*). An admin with no account would otherwise be invisible to every guard that reasons over `users`, while still resolving to `Admin` here — the orphaned-admin brick.

**What `caller_class_for_actor` refuses.** The class is re-resolved on *every* RPC, so each of these bites at **dispatch**, on a connection the actor already holds — not merely at token mint: the all-zero (anonymous) actor; a `pending`/`revoked` bridge; an actor with **no `users` row** (never registered, or deleted); a **suspended** actor; and a **locked-out** actor (`fauna.sessions.lockout`, the emergency control). Admins resolve *before* the row lookup and so are exempt from the suspension and lockout checks — deliberately, because a suspended or locked-out sole admin is an off-box brick nobody could restore ([`nest/common.md`](nest/common.md) § Client-state recoverability); suspension keeps that unrepresentable via `require_not_admin`, and lockout is simply not extended to admins until an equivalent "cannot lock the last admin" guard exists. **The same question guards every other bearer door, at use (built 2026-09-23).** A bearer outlives the moment it was minted, so a validated token is not by itself admission: the authenticated WS upgrade (refused `401` before the connection registers, so a refused socket never receives a Push frame) and every HTTP route that accepts a session bearer (the `auth` extractors and the hand-rolled byte-plane validates) go through one nest validator, `auth::validate_bearer` / `validate_bearer_session`, which is the token-store check **plus** `caller_class_for_actor(…).is_some()` — this function, not a re-derivation of its order, so an approved bridge, a custodian holder and a locked-out admin keep exactly the doors dispatch grants them. A refused standing answers `401`, the same as the revoked token it would have been had the revocation reached it. No route calls the token store's `validate` directly (pinned by source shape in `auth.rs`); the bulk-byte-token arm of the bulk write extractor is a separate store and is out of that validator's scope.

**Refusal codes at the gate (ruled 2026-08-17).** The central gate's refusal answers three different questions, and the code says which. A **listed** kind refused on caller class — the only live refusal path for a served kind, since `every_registered_kind_is_gated` keeps every registered kind listed — answers with the kind's own family code **`fauna.<ns>.permission_denied`** (`<ns>` = the kind's leading segment, the `fauna.` prefix stripped first when present, derived by `bridge_method_allowlist::class_refusal_namespace`): the refusal is a statement about that family's contract, and apps branch per-family. The prefix is optional because four listed kinds are named outside the `fauna.<ns>.…` shape — `bluesky.feed.thread` and the three `nostr.…` kinds, which keep their upstream protocol's own namespace — and they are statements about *their* family just the same; deriving only after a mandatory `fauna.` strip sent exactly those four to the central bridges code instead (fixed 2026-08-17, caught by `every_registered_kind_has_a_refusal_family` — the companion sweep test that exists for this fallback). The defense-in-depth handler layer derives the code through the same seam — `bridge_method_allowlist::require_permission` builds the refusal itself (callers pass only their namespaced `internal`), so a gate bypass cannot change wire codes; per-module `permission_denied` helpers survive only for the finer within-class refusals (caller scope, authorship) the class gate cannot decide. The pinned spec is `tests/e2e-unified/tests/api/test_services.py::test_non_admin_caller_is_denied` + `test_invite_requests.py::test_admin_list_requires_admin`. The central **`fauna.bridges.permission_denied`** remains for the two refusals that are *not* statements about a kind's family: an **unlisted** kind (no allowlist arm — a nest-side defect the sweep test normally stops before it ships; the [`transport.md`](transport.md) add-a-kind recipe cites this case), and an **unknown/revoked actor**, denied on *every* kind — a load-bearing wire signal: the Go bridges' revocation detection (`bins/fauna-bridges/internal/wsrpc/reconnect.go`, `probeStanding`/`WhoamiIndicatesRevoked`) keys on exactly that every-kind shape, so that arm's code must never become per-family. History: from 2026-06-24 to 2026-08-17 the gate hard-coded the bridges code for all three cases — a silent wire regression against the pinned spec that no merge path could see (no gate runs tier_3 `tests/api/`; [`merge-gate-check.md`](merge-gate-check.md) accepted gap 6). Restoring the family code is the bug-fix direction, surveyed against every wire consumer: the Go bridges ride the unchanged revoked-actor arm, and `RpcError::action()` classifies every `*.permission_denied` as `Rejected` regardless of family.

**The per-domain kind catalog moved 2026-09-06 — it now lives in [`core-client-kind-catalog.md`](core-client-kind-catalog.md).** This heading keeps the *layer*: what Layer 1 is, who may call it, and what the gate refuses. What left is the *enumeration* underneath it — 30 domain sub-sections listing every Layer 1 kind, the HTTP twin each replaced and the per-route provenance of the migration, which was 54.8% of this doc without being any part of the classification rule it states. A citation of `§ Layer 1: Core Client API` still resolves here; a citation naming one of the domains resolves there, heading text unchanged: § Authentication & Registration, § Account & Profile, § Sessions, § Feeds & Posts, § Inbox & Messaging, § Notifications (unified), § Email (feature-gated), § Contacts & Knocks, § Channels (MLS Encryption), § Groups, § Blobs & Media, § File Sync, § Folders, § Snapshots, § Backup Destinations, § Calendars & Events, § Subscriptions (Paywalled Content), § Moderation & Spam, § Labels & Engagement, § Algorithm & Reputation, § Web Content Publishing, § Nest Pairing & Sync, § Bridge Feeds, § WireGuard *(removed 2026-08-23)*, § Pending Actions, § Storage Migration, § Share, § Stats, § File Versions, § Push Notifications.

## Layer 2: Protocol-Specific Client API

These endpoints expose protocol-native features. Some are genuinely protocol-specific (thread context, zaps, badges, account linking). Everything else is unified — **interactions** ride `fauna.posts.interact` and **notifications** `fauna.notifications.*` cross-protocol; the protocol-specific interact/notification/DM/control-plane surfaces were removed (per-protocol notes below), leaving only the genuinely protocol-unique kinds plus OAuth/byte residue.

> Migrating to WS-RPC: kinds in the form `bluesky.<area>.<verb>`, `nostr.<area>.<verb>`, `activitypub.<area>.<verb>` — staying scoped to genuinely protocol-unique features as the unified-API push retires the deprecated cross-protocol surfaces. OAuth callback endpoints (`auth/callback` under each protocol prefix) stay HTTP on the residue list. The Nostr relay WebSocket (`GET /nostr`) is its own protocol on its own URL — separate from Fauna's WS-RPC connection.

**Apps MUST use unified APIs** for interactions and notifications. Protocol-specific APIs are only appropriate for features that are genuinely unique to a protocol.

See `app-guidelines.md` for the exact rules on when protocol-specific APIs are appropriate vs. when unified APIs should be used instead.

### Bluesky (`#[cfg(feature = "bluesky")]`)

The consume-side Bluesky surface was reduced to WS-RPC / unified surfaces
(2026-06-05 — a two-agent flow-trace found only
the thread + auth twins had live consumers; design grounded in `docs/goal/behavior/bridges.md`
§ Bluesky bridge; tracked internally). What remains
under `/api/v1/bluesky/`:

| Group | Routes | Status |
|-------|--------|---------|
| Auth (OAuth residue) | `auth/callback`, `.well-known/atproto-oauth-client` | **HTTP residue** — OAuth 2.0 redirect + client-metadata served *to* Bluesky's OAuth server (far end is not Fauna) |
| Media (byte-bulk) | `media?url=` | **HTTP residue** — CDN image/video proxy (`application/octet-stream`) |

**Migrated to WS-RPC** (2026-06-05; tracked internally, Commit B): the **thread view** (`feed/thread/{uri}` + `thread?post_id=`) moved onto the kind **`bluesky.feed.thread`** — the one protocol-unique consume-side Bluesky kind. Request `BlueskyThreadRequest{AtUri|PostId}` (the `PostId` variant resolves its AT-URI through the `bluesky_posts` crosspost mapping), reply `BlueskyThreadReply{posts:[BlueskyPost], focal_index}` (flat list plus the nest-named focal post's index); wire types `libs/fauna-protocol/src/bluesky.rs`, kind `kind.rs::register_bluesky_kinds`, handler `bluesky::bluesky_handlers` (feature-gated), gate `bridge_method_allowlist.rs` (`User`), tier_3 `conformance_bluesky.rs`. The two HTTP twins were deleted; **android migrated** onto the kind via the shared `fauna-client-bluesky` crate + the `FfiBlueskyClient` UniFFI seam (2026-06-06; **`PostId` variant** — the post-detail surface navigates from a crossposted Fauna post, holding the hex `[u8;32]` Fauna id, not the AT-URI), and **linux migrated** (2026-06-06; `client.rs fetch_bluesky_thread` → `BlueskyClient::thread(PostId)` directly on the proto types, no FFI seam). Both post-detail surfaces are now on the kind, so the thread-view fan-out is **complete**. The nest names the focal post in the reply's `focal_index`; no client derives it.

**Deleted as unified-bridges dups** (2026-06-05; tracked internally, Commit C): `auth/start`, `auth/status`, `auth` (DELETE) — deprecated exact dups of `fauna.bridges.{link,list,unlink}` (each dispatched into the same `BlueskyProvider` trait methods). Apps link/status/unlink Bluesky through the unified bridge kinds; **linux is done** (2026-06-06 — the legacy account-page Bluesky-OAuth row that called these twins was deleted; linux already links Bluesky on the unified Bridges page via `fauna.bridges.link`, and `link_bridge` now opens the OAuth `redirect_url` like web), **apple remains** the follow-on. The coverage replacing the deleted auth-twin tests is **now complete**: the deterministic half — `fauna.bridges.list` (Bluesky available/unlinked + `oauth` link mode) and `fauna.bridges.link` unknown-mode → `invalid_mode` — in `tests/e2e-unified/tests/api/test_bluesky_oauth.py`; and the positive `link` path — `mode:"oauth"` → reply `redirect_url` — in the tier_3 `bins/fauna-nest/tests/conformance_bluesky_link.rs`, which drives `fauna.bridges.link` through the live `RpcRouter` against a fake atproto far end canned in-process at the `HttpClient::send_http` boundary (no network), exercising the real `BlueskyProvider` + `OAuthClient::authorize`. Both feature-gated bluesky conformance suites now run in CI (`--features bluesky` step).

**Removed** (2026-06-05; all zero-consumer): `profile` (→ `fauna.bridges.list` identity), `settings` GET/PUT (→ `fauna.bridges.{list,set_settings}`), `feed/{timeline,feed,author/{did}}` (standalone Bluesky-timeline view dropped per bridges.md), `interact/*` (→ unified `fauna.posts.interact`; `route_unified_interaction` kept as shared plumbing), `dm/*` (Bluesky DMs flow via the unified Conversations surface — `direct-messages.md`), `search/{actors,feeds}` + `feeds/{suggested,saved,save,unsave}` (custom-feed-subscription is the unified `fauna.bridges.feeds.*`). The `notifications*` routes listed in an earlier revision **never existed** in code — Bluesky notifications ride the `bluesky::notif_sync` background poller into the unified `fauna.notifications.*`.

### Nostr (`#[cfg(feature = "nostr")]`)

**No `/api/v1/nostr/*` routes remain** (the native-content rip completed the surface's deletion, 2026-07-22). The only Nostr HTTP left is the genuine residue whose far end is a non-Fauna Nostr client: `GET /nostr` (NIP-01 relay WS), `GET /nostr/info` (NIP-11), and `GET /.well-known/nostr.json` (NIP-05).

**Removed** (the native-content rip, 2026-07-22 — user ruling: no client-to-nest HTTP; `nostr.md` § WS-RPC migration contract): `zaps/{event_id}`, `badges/{pubkey}`, `publish-signed`. These now ride the **prefix-less protocol-content kinds** `nostr.{zaps.total,badges.list,events.publish_signed}` (User-class; the `bluesky.feed.thread` naming precedent), over the shared `fauna_client_nostr::NostrContentClient` + wasm faces.

**Removed** (the `nostr-dm-rail` track, 2026-06-13 — the Nostr **DM** rip, `nostr.md` § WS-RPC migration contract step 3): `dms` (GET conversations), `dms/{peer_pubkey}` (GET messages / POST send). DMs then rode three dedicated **User-class, caller-scoped** kinds — a **nest-backed Conversations feed** in the `fauna.` prefix family, **not** the prefix-less `nostr.<area>.<verb>` form reserved for the step-4 protocol-content kinds above — and since 2026-10-03 ride the bridged-conversation family (`fauna.bridges.conversation.*`, a room per peer on the in-process Nostr leg): the three kinds, the web Nostr-page DM list that last called them, and the per-rail `NostrBackend` first designed to consume them are all deleted (`../ui/nostr.md` § Implementation status today → DMs). See `../behavior/conversations-at-rest.md` § Receiving into the conversations view.

**Removed** (2026-06-08 — the Nostr control-plane rip, `nostr.md` § WS-RPC migration contract): `link` (POST/DELETE), `status`, `settings` (PUT), `follows` (GET/POST/DELETE). The control plane now rides the existing **`fauna.bridges.*`** kinds (`NostrProvider`, `bridge_id:"nostr"`) — `list` carries status (npub via `identity.value`, mode, the 5 content flags + `relay_list` via `settings[]`); `set_settings`/`link`/`unlink`/`list_follows`/`add_follow`/`remove_follow` cover the rest. `NostrProvider::update_settings` carries the NIP-65 relay-list republish the old HTTP handler did. App fan-out is complete on all 7 apps (`nostr.md` § Implementation status today).

Nostr relay endpoints: `GET /nostr` (WebSocket), `GET /nostr/info` (NIP-11 relay info) — external Nostr protocol, residue (stays HTTP/WS).

### ActivityPub (`#[cfg(feature = "activitypub")]`)

**No client-facing HTTP routes.** `/api/v1/activitypub/*` — `enable` / `disable` (POST), `status` (GET), `settings` (PUT) — was **removed 2026-07-16** (the ActivityPub control-plane rip, following the Nostr precedent above; `activitypub.md` § Control plane). It duplicated, over bearer-auth HTTP, a control plane the **`fauna.bridges.*`** kinds already served (`ActivityPubProvider`, `bridge_id:"activitypub"`): `link{mode:"enable"}`/`unlink` cover enable/disable, `list` carries status (the `@user@domain` handle via `identity.display`, the actor URL via `identity.value`, and `auto_accept_follows`/`default_visibility`/`backfill` via `settings[]`), and `set_settings` covers the rest. No app ever called the HTTP twin — its only consumer was the AP e2e suite, migrated with the rip.

The `/ap/*` + WebFinger + NodeInfo endpoints are unaffected: their far end is a third-party AP server, so they are permanent **Layer 6** residue, not a migration candidate (§ Layer 6 below).

## Layer 3: Bridge Management API

Generic bridge operations — **the correct way for apps to manage bridge connections**. These work the same regardless of protocol.

The surface is WS-RPC-only. HTTP twins (`/api/v1/bridges/*`, `/api/v1/bridge-feeds`, `/api/v1/email/filters/*`, `/api/v1/email/send`) deleted in the T9+T10 sweep — clients hit the typed `fauna.bridges.*` and `fauna.email.*` kinds via `libs/fauna-client-bridges::BridgesClient` and `libs/fauna-client-email::EmailClient`. Wire bytes are canonical dag-cbor per `docs/goal/architecture/transport.md` § Wire format.

| WS-RPC kind | Replay | Purpose |
|---|---|---|
| `fauna.bridges.list` | safe, 5 s | Installed bridge daemons and their connection status |
| `fauna.bridges.link` | **forbid**, 30 s | Start the OAuth or credential flow to connect a bridge to your external account |
| `fauna.bridges.link_challenge` | safe, 5 s | The proof-of-possession challenge an external signer signs before a `link` in that mode is accepted (Nostr `nip07`) |
| `fauna.bridges.unlink` | safe, 5 s | Disconnect a bridge and stop syncing from the external service |
| `fauna.bridges.set_settings` | safe, 5 s | Bridge-specific config: sync frequency, content filters, notification rules |
| `fauna.bridges.list_follows` | safe, 5 s | Accounts you follow through this bridge |
| `fauna.bridges.add_follow` | **forbid**, 5 s | Follow an external account through the bridge so their content appears in your feed |
| `fauna.bridges.remove_follow` | safe, 5 s | Stop following an external account through this bridge |
| `fauna.bridges.list_follow_requests` | safe, 5 s | Follow requests waiting on your account on this bridge (a bridge that declares `supports_follow_requests`) |
| `fauna.bridges.resolve_follow_request` | safe, 5 s | Approve or refuse one waiting follow request; idempotent |

Dynamic per-bridge fields (settings, link params, follow extras) ride as `fauna_cbor::Value` (the generic dag-cbor node) — the `BridgeProvider` trait in `bins/fauna-nest/src/bridge_management.rs` accepts them directly. The legacy `json_to_cbor`/`cbor_to_json` shim retired with the HTTP twins.

## Layer 4: Bridge Proxy (deleted)

> **Deleted at the I6 mail-bridge cutover (the row below is historical).** This layer proxied requests to a co-located Rust bridge daemon (`bins/fauna-bridge-daemon/`) over a Unix socket; both the proxy routes (`bridge_routes.rs`) and the daemon were removed. Bridges are now peer clients of nest: the mail bridge (`bins/fauna-bridges/`) talks to nest over WS-RPC + DAG-CBOR on `/api/v1/ws/{actor_id}` (see `docs/goal/architecture/apps/bridges.md`), with no proxy layer.

| Method | Path | Purpose |
|--------|------|---------|
| ANY | `/api/v1/bridge/{bridge_name}/*` | Transparent proxy: nest forwarded the request to the named bridge daemon process |

## Layer 5: Admin API

Formerly the HTTP routes under `/admin/api/`; now the `fauna.admin.*` WS-RPC kinds on the admin's per-actor connection (Track C complete — every `/admin/api/*` twin deleted). Admin caller class per § Layer 1's caller-class model.

> **All of Layer 5 migrates to WS-RPC** — admin is a Fauna app per the product invariant ("Nest configuration … is set from Fauna apps, not CLI args / env vars / hand-edited config files"). Each route group becomes an `admin.<area>.<verb>` RPC kind on the admin's per-actor WS-RPC connection (admin auth subprotocol shape per `docs/goal/architecture/transport.md`). Per the CBOR-DAG-everywhere rewrite (ratified 2026-05-15; tracked internally) § HTTP residue: "All `/admin/api/*` → admin RPC kinds." Bulk `/api/v1/admin/export/all` stays HTTP (octet-stream tar).
>
> **Migrated to WS-RPC (tracked internally, § Track C / C1) — the Users group + Evictions, full cluster:** the **first `fauna.admin.*` kinds** (establishing the namespace + Admin-only gate for the rest of Layer 5), all behavior-preserving and **Admin-only** in `bridge_method_allowlist::is_permitted` (matching the twins' `AdminBearerAuth`; `suspend`'s twin additionally gated on a role-tier extractor since removed as dead — the roster is single-role today, so `is_admin` was always the whole check). Kinds shipped (`libs/fauna-protocol/src/admin.rs`, `admin_ws_handlers::register_admin_handlers`):
> - **Reads:** `fauna.admin.users.list` (≡ GET `/admin/api/users?limit&offset`), `fauna.admin.users.get` (≡ GET `/admin/api/users/{actor_id}`), `fauna.admin.evictions.list` (≡ GET `/admin/api/evictions`).
> - **Mutations:** `fauna.admin.users.{create,update,delete,clear_handle}` (≡ POST `users`, PUT/DELETE `users/{id}`, DELETE `users/{id}/handle`). `delete` returns a queued `pending_actions` row (`AdminPendingActionReply`); `clear_handle` pushes `AccountUpdated`.
> - **Actions:** `fauna.admin.users.{evict,cancel_eviction,suspend}` (≡ POST `users/{id}/{evict,cancel-eviction,suspend}`). `evict`/`cancel_eviction` push `AccountUpdated` and manage eviction export tokens; `suspend` returns a queued pending action.
>
> No floats: actor ids ride as raw 32-byte `ByteBuf`; tier/label/eviction fields as `String`; byte counts + timestamps as `i64`. The audit-log writes, pending-action scheduling, and pushes are preserved exactly from the twins. The HTTP twins (`admin::{list_users_paginated, get_user, create_user, update_user, delete_user, clear_user_handle, start_eviction, cancel_eviction, suspend_user, list_evictions}`) have since been **deleted** in the WS-RPC-everywhere rip-out (`lib.rs`: "ALL of the deprecated `/admin/api/*` HTTP twins" deleted; re-verified 2026-07-07). C3–C6 (stats/audit/ops, pairings/nest/dns, wireguard/folders/services, email-domains) followed in the same track.

> **Migrated to WS-RPC (tracked internally, § Track C / C2) — admin management (tiers / invite codes / invite requests / admins):** behavior-preserving and **Admin-only** in `bridge_method_allowlist::is_permitted` (matching the twins' `AdminBearerAuth`). Named `fauna.admin.{tiers,invite_codes,invite_requests,admins}.*` — the `admin.` prefix keeps them distinct from the pre-identity public invite flow `fauna.account.invite_{request,code}.*` (Track A5, anonymous connection). Kinds shipped (`libs/fauna-protocol/src/admin.rs`, `admin_ws_handlers::register_admin_handlers`):
> - **Tiers:** `fauna.admin.tiers.{list,create,update}` (≡ GET/POST `/admin/api/tiers`, PUT `/admin/api/tiers/{name}`). Empty name → `fauna.admin.invalid_params`; duplicate → `fauna.admin.conflict`; missing (update) → `fauna.admin.not_found`. All five tier limits ride as `i64`, and **a negative one is refused `fauna.admin.invalid_params` on both `create` and `update`** (2026-08-17): the caps are non-negative by the rule [`../behavior/value-formatting.md`](../behavior/value-formatting.md) § Tier cap validation owns, where `0` is a valid admin-chosen "no allowance" — so the floor is `0`, never `1`. One shared `validate_tier_caps` serves both doors so they cannot drift.
> - **Invite codes:** `fauna.admin.invite_codes.{list,create,delete}` (≡ GET/POST `/admin/api/invite-codes`, DELETE `/admin/api/invite-codes/{code}`). `create` defaults `tier` to `"free"` and `uses` to `1` (the twin's defaults); empty code → `invalid_params`; duplicate → `conflict`; missing (delete) → `not_found`. **`uses < 1` → `invalid_params`** (2026-08-17): redemption requires `uses_left > 0`, so a lower value would mint a **born-dead code** the admin hands out believing it works.
>
>   **Why both refusals live at the door even though all 7 apps clamp** (the reasoning generalizes to every numeric admin field): the app clamps landed 2026-08-17, and a client may be **older** than the nest ([`version-compatibility.md`](version-compatibility.md)), so an app built before then still puts the unclamped value on the wire — the nest is the only surface that can refuse it. Both were silent rather than loud: an out-of-range value saved cleanly for the admin and surfaced only later, as unexplained behaviour for the tier's users. The precedent for the shape is the DNS-rename doors' `GRACE_DAYS_MIN..=GRACE_DAYS_MAX` check (`bridge_routing_handlers.rs`, `fauna.bridges.invalid_grace_days`), which is *why* `value-formatting.md` sweep could rule those app-side parses "correct as written — they delegate range validation to the nest". **An app-side parse may only claim that delegation where the door actually checks**; these two doors had no such delegate until now.
>
>   **The mail-policy doors take the same `0` boundary, and take it deliberately** (`fauna.bridges.put_{spam,auth,submission,imap,outbound,alias}_policy`; swept knob-by-knob 2026-08-18, `../behavior/value-formatting.md` sweep). Their 25 integer knobs ride as `Option<u32>`/`Option<u64>`, so a negative is unrepresentable and only `0` is in question — and **no unsigned mail knob refuses `0` at the door**. Which meaning `0` carries is a property of the knob, not of the door: a **protection threshold** (connection-rate cap, AUTH-failure lockout, spam-score tier, unlisted-recipient penalty) reads `0` as *protection off*, an **allowance** (submissions/day, mailbox bytes/messages, aliases) reads it as *no allowance* — the same boundary the tier caps keep above, floor `0` and never `1`. The per-knob table, and which consumer makes each verdict true, is owned by [`../behavior/mail-policy-config.md`](../behavior/mail-policy-config.md) § What `0` means on an unsigned knob. Two consequences for anyone touching these doors: **do not add a `> 0` floor to an allowance knob** (it would contradict the tier-cap ruling and delete a legitimate admin choice), and **a protection knob must disable at `0`, not reject at `0`** — `max_conn_per_min` did the latter until sweep, which made every inbound SMTP connection `421` at `NewSession` while the knob beside it in the same pane, `max_conn_per_ip`, had meant *admit all* at `0` by explicit written decision all along.
> - **Invite requests (admin side):** `fauna.admin.invite_requests.{list,approve,deny}` (≡ GET `/admin/api/invite-requests`, POST `.../{id}/{approve,deny}`). `approve` creates the user (actor + handle, `tier` default `"free"`), re-checks the handle, deletes the (terminal) request, and returns `{actor_id, handle, tier}`; `deny` marks it denied. Missing → `not_found`; non-pending / re-taken handle / duplicate actor → `conflict`.
> - **Admins:** `fauna.admin.admins.{list,add,remove}` (≡ GET/POST `/admin/api/admins`, DELETE `/admin/api/admins/{actor_id}`). `add`/`remove` schedule `AdminAdd`/`AdminRemove` `pending_actions` (returning `AdminPendingActionReply`); `remove` refuses the last superadmin (`conflict`). Wrong-length actor id → `invalid_params`.
>
> No floats: actor ids ride as raw 32-byte `ByteBuf`; names/handles/reasons as `String`; counts/timestamps/ids as `i64`. Audit-log writes preserved exactly from the twins. **`tiers.create` / `invite_codes.create` fix a latent twin error-mapping bug** — both twins matched only `e.to_string()` for `UNIQUE`, but `CacheDb::{create_tier, create_invite_code}` wrap the rusqlite error in `.context(...)`, so a duplicate `500`ed instead of returning the intended conflict; the kinds match the full chain (`{e:#}`) — the C1 `create_user` / B14 folders precedent. The HTTP twins (`/admin/api/{tiers,invite-codes,invite-requests,admins}*`) were **deleted** in the WS-RPC-everywhere rip-out — the WS-RPC kinds are now the sole admin-management surface (the `tests/api/` harness drives them via `clients.ws_rpc_admin_client.WsRpcAdminClient`; see `tests/e2e-unified/tests/api/test_invite_requests.py`). The shared cores (`admin::generate_invite_code`, the `CacheDb` writers) stay.

> **Migrated to WS-RPC (tracked internally, § Track C / C3) — stats / audit / ops:** behavior-preserving and **Admin-only** in `bridge_method_allowlist::is_permitted` (matching the twins' `AdminBearerAuth`). Kinds shipped (`libs/fauna-protocol/src/admin.rs`, `admin_ws_handlers::register_admin_handlers`):
> - **Stats / status:** `fauna.admin.stats` (≡ GET `/admin/api/stats` — `db::Stats` plus the live `ws_connections`), `fauna.admin.status` (≡ GET `/admin/api/status` — running version + any pending self-update).
> - **Audit:** `fauna.admin.audit.list` (≡ GET `/admin/api/audit?limit&before_id` — newest-first page; `limit` defaults to 100, clamped `1..=1000`), `fauna.admin.audit.integrity` (≡ GET `/admin/api/audit/integrity` — hash-chain summary).
> - **Cluster / GC:** `fauna.admin.cluster.status` (≡ GET `/admin/api/cluster/status` — blob-storage breakdown), `fauna.admin.gc` (≡ POST `/admin/api/gc` — orphan-blob GC; `grace_period_secs` default 1800, `dry_run` reports without deleting; writes the `gc.trigger` audit entry on a real run; backup-not-configured → `fauna.admin.invalid_params`, the twin's 400). **`grace_period_secs < 0` → `fauna.admin.invalid_params`, checked before the backup-service lookup** (2026-08-17): GC keeps a blob iff `created_at > now - grace_period_secs`, so a negative puts the cutoff in the **future** — no blob's `created_at` can exceed it, every unreferenced blob becomes deletable, and the fresh-blob grace is disabled outright rather than shortened (the superseded-sync pin unpins the same way). `0` remains valid — "no grace, collect everything unreferenced now" is a coherent admin choice, the same `0`-is-meaningful boundary the tier caps keep.
> - **Worker:** `fauna.admin.worker.status` (≡ GET `/admin/api/worker/status` — nest-link proxy worker connection / authorized key / replication count / storage figures).
> - **Pending actions:** `fauna.admin.pending_actions.list` (≡ GET `/admin/api/pending-actions` — the **cross-actor** list of every actor's queued destructive operations). Named `fauna.admin.pending_actions.list` to stay **distinct** from B20's user-scoped `fauna.pending_actions.list` (the calling actor's own queue); the admin row adds `actor_id` (the owning actor) and `ip_address`, and parses the stored `approvals` JSON-array into `Vec<String>` (the B20 typing precedent).
>
> No floats: every numeric field is `i64`/`bool` (the twins' replies were already integer counts / byte totals / wall-clock seconds — `db::Stats`, `db::AuditRow`, `db::BlobStorageStats`, `backup::gc::GcResult`, the nest-link `WorkerInfo`). The HTTP twins (`admin::{get_status, list_audit, audit_integrity, cluster_status, trigger_gc, worker_status}`) have since been **deleted** in the rip-out (re-verified 2026-07-07). The `/admin/api/stats` and `/admin/api/pending-actions` twins (`admin::stats`, `list_all_pending_actions`) were **deleted** in the orphaned-twin residue sweep (tracked internally, 2026-06-03); their `fauna.admin.{stats,pending_actions.list}` kinds are the sole surface. ⚠ The stats sweep **missed android's admin dashboard** (`AdminDashboardVM` → `ApiClient.fetchAdminStats`), which kept calling the deleted route (so the dashboard was broken against main from 2026-06-03) — **now migrated** onto `fauna.admin.stats` via the new `FfiAdminClient::stats` UniFFI seam (`libs/fauna-ffi/src/admin.rs`, the `FfiAdminStats`/`FfiAdminTierCount` mirrors; linux already reads the kind directly). C5 (wireguard/folders/services) and C4 (pairings) follow below and complete the track; C4's private-nest/dns-credentials and C6 (email-domains) are retired.

> **Migrated to WS-RPC (tracked internally, § Track C / C5) — wireguard / folders / services:** behavior-preserving and **Admin-only** in `bridge_method_allowlist::is_permitted` (matching the twins' `AdminBearerAuth`). The admin `fauna.admin.{wireguard,folders}.*` buckets are **distinct** from the user-class `fauna.wireguard.peer.*` (Track B21) and `fauna.folders.*` (Track B14) clusters. Kinds shipped (`libs/fauna-protocol/src/admin.rs`, `admin_ws_handlers::register_admin_handlers`):
> - **WireGuard:** `fauna.admin.wireguard.{status,peers,keygen}` (≡ GET `/admin/api/wireguard/{status,peers}`, POST `/admin/api/wireguard/keygen`) — **all three DELETED 2026-08-23** with the stack; the historical migration note is kept because the C5 batch it belonged to is described as a unit. `status` folds the twin's enabled/disabled shapes into one struct; `peers` wraps the twin's bare array; `keygen` mints a fresh Curve25519 keypair (the twin's `no-store` cache header is an HTTP artifact — the private key rides the sealed WS-RPC connection).
> - **Folders (admin):** `fauna.admin.folders.{create,get,add_member,add_destination}` (≡ POST `/admin/api/file-sets`, GET `/admin/api/file-sets/{name}`, POST `.../{name}/{members,destinations}`). `create` returns the **correct** `fauna.admin.conflict` on a duplicate name — `folders` is `UNIQUE(name, actor_id)` (per-actor; the pre-migration column was globally `UNIQUE(name)`) and the twin wrapped the rusqlite error in `.context(...)`, so a duplicate silently `500`ed (the C1/C2 latent-bug class); the kind matches the full chain (`{e:#}`). Because uniqueness is per-actor, a bare name can match several actors' sets on a multi-user nest: `get`/`add_member`/`add_destination` take an **optional `actor_id`** (additive, 2026-07-08) that scopes the by-name lookup to one owner; a bare ambiguous name errors `fauna.admin.invalid_params` instead of silently resolving to an arbitrary actor's set. Role / kind / sync_mode validation → `fauna.admin.invalid_params`; missing set → `fauna.admin.not_found`. **⚠ `add_destination` RETIRED 2026-08-18**: the phantom `folder_destinations` rail it wrote had no reader any app or daemon ever created a row for, so the whole kind — handler, registration, kind-registry row, offline-class row, allowlist arm, wire request/reply types — is deleted along with the table; `fauna.admin.folders` now spans only `{create,get,add_member}`. Delivery for every device is the remote-change nudge + catch-up pull, never a nest-forwarded destination — `../behavior/file-sync.md` § Remote-change nudge.
> - **Services:** `fauna.admin.services.{list,update}` (≡ GET `/admin/api/services`, PUT `/admin/api/services/{name}`). The sidecar-service intent file (`bridge`/`pairing` flags on the wire; `pairing` is the per-user-pairing admin knob — default on); unknown service name → `invalid_params`.
>
> No floats: actor / device ids ride as raw 32-byte `ByteBuf` (the twins emitted hex); `listen_port` / `persistent_keepalive` (`u16` on HTTP) and the service `version` (`u32`) ride as `i64`; peer/blob counts as `i64` — no handshake timestamps, transfer counters, or health ratios appear in any of these replies (the hand-off's float warnings were speculative). The HTTP twins (`wireguard::admin::{wg_status, list_peers, keygen}`, `folder_routes::{create_folder, get_folder, add_member, add_destination}`, `services::{get_services, update_service}`) have since been **deleted** in the rip-out (re-verified 2026-07-07).

> **Migrated to WS-RPC (tracked internally, § Track C / C4) — pairings:** behavior-preserving and **Admin-only** in `bridge_method_allowlist::is_permitted` (matching the twins' `AdminBearerAuth`). Kinds shipped (`libs/fauna-protocol/src/admin.rs`, `admin_ws_handlers::register_admin_handlers`): `fauna.admin.pairings.{list,approve}` (≡ GET `/admin/api/pairings`, POST `/admin/api/pairings/{nest_id}/approve`). `list` is **actor-scoped** — the caller's own pairings (`db::list_pairings_for_actor`), so the connection `actor_id` drives data selection (unlike the cross-actor C1/C3 admin lists). `approve` stores the pairing (`INSERT OR REPLACE`, idempotent), defaulting an empty capability list to `["sync", "federation"]`, and writes the `pairing.approve` audit entry; `nest_id` rides as raw `ByteBuf` (hex path param on HTTP). **⚠ RETIRED (per-user-pairing design, 2026-05-25):** `fauna.admin.pairings.{list,approve}` + the deprecated HTTP twins are removed — pairing is authorized/revoked by the **user** via the bearer `fauna.pair.{add,revoke}` kinds (§ Nest Pairing & Sync); the admin's only pairing control is the nest-level `pairing` service knob. See the per-user-pairing design (ratified 2026-05-25; tracked internally). No floats (`expires_at`/`created_at` are `i64`/`Option<i64>`; ids `ByteBuf`). **C4's `private-nest` and `dns-credentials` groups are RETIRED, not migrated** — both routes/handlers were deleted in the client-holds-keys removal (the nest never holds DNS provider credentials nor mints private nests; the client does — `docs/goal/behavior/dns-management.md`, memory `nest-never-writes-dns-handle-dns-deferred`). **C6 (email-domains legacy)** is likewise RETIRED: the `/admin/api/email-domains*` admin HTTP surface is gone from `lib.rs`, superseded by the mail-admin local-domains surface (`fauna.bridges.{add,remove,restore,list}_local_domain`). **Track C admin migration is therefore complete** — the only surviving admin HTTP is the bulk `/api/v1/admin/export/all` octet-stream (residue) (survivor table below; the `/api/admin/bridges/{pending,approve,revoke}` enrollment laggard is removed — `mail-bridge-lifecycle.md` § The retired pre-registration path); the bridge-proxy passthrough (Layer 4) was deleted at the I6 cutover.

**Former `/admin/api/*` route groups → `fauna.admin.*` kind clusters (all HTTP twins deleted — Track C complete; the per-cluster blockquotes above carry the per-kind detail):**

- **Users + evictions (C1):** `users*`, `users/{id}/{evict,cancel-eviction,suspend}`, `users/{id}/handle`, `evictions` → `fauna.admin.users.{list,get,create,update,delete,clear_handle,evict,cancel_eviction,suspend}` + `fauna.admin.evictions.list`.
- **Management (C2):** `tiers*`, `invite-codes*`, `invite-requests*`, `admins*` → `fauna.admin.{tiers,invite_codes,invite_requests,admins}.*`.
- **Stats / audit / ops (C3):** `stats`, `status`, `audit`, `audit/integrity`, `cluster/status`, `gc`, `worker/status`, `pending-actions` → `fauna.admin.{stats,status,audit.{list,integrity},cluster.status,gc,worker.status,pending_actions.list}`.
- **WireGuard / folders / services (C5):** `wireguard/{status,peers,keygen}` (the wireguard third deleted outright 2026-08-23), `folders*`, `services*` → `fauna.admin.{wireguard,folders,services}.*`.
- **Retired outright (no kind):** `pairings*` (per-user-pairing design 2026-05-25 — user-side `fauna.pair.{add,revoke}`; admin control is the nest-level `pairing` service knob), `private-nest` + `dns-credentials` (client-holds-keys), `email-domains*` (superseded by mail-admin local-domains `fauna.bridges.*_local_domain`).

The admin HTTP survivors:

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/admin/export/all` | Full nest export: zstd-compressed tar of all tables, blobs, and config (byte residue) |

## Layer 6: Federation API

Consumed by other servers, not by clients.

### ActivityPub
| Method | Path | Purpose |
|--------|------|---------|
| POST | `/ap/users/{username}/inbox` | Receive AP activities (Follow, Create, Update, Like, Announce, Undo, Accept, Delete) addressed to this user |
| POST | `/ap/inbox` | Shared inbox for activities not addressed to a specific user (public posts, announces) |
| GET | `/ap/users/{username}` | AP actor document: public key, endpoints, name, bio (JSON-LD) |
| GET | `/ap/users/{username}/followers` | Paginated OrderedCollection of accounts following this user |
| GET | `/ap/users/{username}/following` | Paginated OrderedCollection of accounts this user follows |
| GET | `/ap/users/{username}/outbox` | Paginated OrderedCollection of this user's public activities |
| GET | `/ap/users/{username}/notes/{post_id_hex}` | Single-note dereference — serves the same `ApNote` a `Create`-push or the outbox embeds (`activitypub.md` § The produce direction) |
| GET | `/ap/instance` | Nest-level instance actor — the signing identity for outbound remote-actor fetches, not a profile (`activitypub.md` § Architecture → The instance actor) |
| GET | `/ap/instance/outbox` | Instance actor's outbox — always an empty `OrderedCollection` |
| GET | `/.well-known/webfinger` | Resolve `user@domain` to AP actor URL (used by remote servers for federation) |
| GET | `/.well-known/nodeinfo` | NodeInfo discovery — carries no metadata itself, only the link to the document below (its href must name a route this nest serves; `activitypub.md` § Actor serving) |
| GET | `/nodeinfo/2.1` | NodeInfo 2.1 document: software name + `major.minor` version, protocols, AP-enabled user count, open registration |

### CalDAV
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/.well-known/caldav` | CalDAV service discovery redirect for standards-compliant calendar clients |
| PROPFIND | `/caldav/{actor_id}/` | CalDAV principal resource: list calendars and supported properties |
| ANY | `/caldav/{actor_id}/{calendar}/` | CalDAV collection operations: PROPFIND, REPORT, MKCALENDAR on a calendar |
| GET/PUT/DELETE | `/caldav/{actor_id}/{calendar}/{uid}` | Fetch, create/update, or delete a single calendar event by UID |

### Email
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/.well-known/mta-sts.txt` | MTA-STS policy file telling sending servers to require TLS for mail delivery |

### OAuth (Bluesky)
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/.well-known/atproto-oauth-client` | AT Protocol OAuth client metadata so Bluesky's PDS can validate this nest as a client |

## Layer 7: Internal API

Inter-service communication within a nest deployment.

> The user-facing `claim-admin` bootstrap **migrated** to the `fauna.auth.claim_admin` kind on the pre-identity WS-RPC connection (one-time bootstrap; admin is a Fauna app per the product invariant) — Track A4; the `POST /api/v1/claim-admin` HTTP twin was **removed** in S4d (tracked internally), so the kind is its sole transport. The live internal surface is the two loopback WS channels plus one health route (re-verified 2026-10-01 against `lib.rs` `build_router`). **Gone:** the worker `/internal/proxy-status` (superseded by `/internal/router-status`), the internal algorithm HTTP mesh (`/internal/algorithm/{reputation,report,reports}`, deleted 2026-06-09) and the `/internal/algorithm/ws` channel that replaced it (removed 2026-10-01 with the rest of the algorithm-service mesh — [`core-client-kind-catalog.md`](core-client-kind-catalog.md) § Algorithm & Reputation), and the mail-bridge `/api/v1/email/{deliver,validate-recipient}` plumbing (deleted at the I6 cutover — the Go bridge talks the bridge-class WS-RPC surface directly, `architecture/apps/bridges.md`).

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/internal/worker/ws` | WebSocket for the nest-link proxy worker to connect and receive replication tasks |
| GET | `/internal/relay/ws` | Loopback-gated, sidecar-token-authed WS channel the iroh relay sidecar dials (`sidecar_channel::relay_ws_handler`; `transport.md` § Future directions) |
| GET | `/internal/router-status` | Nest health and capacity for the SNI router / load balancer: user count, max users, version |
| — | (claim-admin) | One-time bootstrap moved off HTTP: submit the first-boot claim code via the pre-identity WS-RPC kind `fauna.auth.claim_admin` (A4); the `POST /api/v1/claim-admin` HTTP twin was removed in S4d |

## Implementation status today

- **The nest-hosted issuer endpoints classified in § HTTP residue are built (TP5 slices S1–S2, complete)** — discovery, JWKS, PAR, authorize/poll, token, revoke and (TP6) userinfo all answer on the nest's own apex; only `device_authorization`/`bc-authorize` remain target state (TP9, unbuilt anywhere). **Of the record / deposit / events doors classified alongside them, the deposit door is built (2026-10-03 — `POST /api/v1/folders/{id}/deposit`, `bins/fauna-nest/src/folder_deposit.rs`, admitting the DPoP-bound token through the principal session's own gate and dispatching `fauna.folders.deposit`), and so is the record door (2026-10-05 — `/api/v1/records/{kind}[/{key}]`, `bins/fauna-nest/src/records_door.rs`, the same gate, dispatching `fauna.account.state.put` / `fauna.sync.changes.list`; wire: `third-party-kinds.md` § The record doors), and so is the events long-poll (2026-10-05 — `GET /api/v1/events`, `bins/fauna-nest/src/events_doors.rs`, the same gate, dispatching `fauna.events.poll`), and so is the events webhook (2026-10-05 — `bins/fauna-nest/src/events_webhook.rs`: a security event token under the issuer key, POSTed to the manifest's `events_uri` through the guarded dial; which key is `key-material-hierarchy.md` § *Issuer signing key* → *What it signs*' ruling, the door `transport.md` § Push events → *Third-party event doors*).** Sequencing: `architecture/third-party.md` § Implementation status today; issuer detail: `behavior/authorization-server.md` § Implementation status today.

### WS-RPC migration status

Spec Y (design ratified 2026-05-04; tracked internally) shipped the per-actor WS-RPC channel at `GET /api/v1/ws/{actor_id}`. The CBOR-DAG-everywhere rewrite (design ratified 2026-05-15; tracked internally) takes that channel from "absorbs interactive traffic over time" to "is the authenticated UI surface" — JSON survives only on the externally-forced residue list.

- **Layer 1 — Core Client API:** migrating per-feature to WS-RPC kinds (`fauna.<area>.<verb>`). **Shipped:** the bridges + email-filters surface (`fauna.bridges.*` / `fauna.email.*`, HTTP twins deleted), and the **conversations** surface — `fauna.conversations.{channel,keypackage,welcome,room}.*` (channel send/fetch/list, key-package upload/fetch/count, same-nest welcome delivery, the room family — the earlier `group` create/send/invite/react/delete + read kinds were retired 2026-09-26); the HTTP twins (`/api/v1/{group,groups,channel,channels,keypackage,welcome}/*` local-user routes + the `/api/v1/groups/create` REST shortcut) were deleted; the two cross-nest federation routes `GET /api/v1/keypackage/{actor}` + `POST /api/v1/welcome/{actor}` were kept as HTTP federation residue through the interim and then **retired in Spec Y2 slice 5 (2026-06-03)** — they now ride the nest↔nest WS-RPC federation channel as `fauna.federation.{keypackage.fetch,welcome.deliver}` (`federation.md` is the authority). Plus `fauna.spam.*`, `fauna.search.query`, `fauna.posts.{create,get,interact}`, and the **10 client-facing `fauna.feed.*` kinds** (`list`/`create`/`get`/`update`/`delete`/`posts`/`local.posts`/`contributors.{list,grant,revoke}`) — kinds shipped, HTTP twins deleted, **app fan-out complete on all 7 apps** — linux + web (2026-05-25) + windows + android + macos + ios (the last two via the shared FaunaKit `FfiFeedClient`/`FfiPostsClient` seam + the shared `encodeFilterRule`/`decodeFilterRules` rule codec) + tui (native `fauna-feed`, parity 2026-07-19) (`feed.query` excluded — nest↔nest federation, not a client path; its HTTP route was retired in Spec Y2 slice 5 onto the federation channel as `fauna.federation.feed.query`; `context.get` — the `GET /api/v1/context` interest profile — was **deleted** as dead code, no consumer). Plus the **social-inbox cluster** — `fauna.notifications.{list,mark_read,count}` + `fauna.knocks.{list,accept,block,dismiss}` + `fauna.contacts.{list,confirm}` + `fauna.inbox.mode.{get,set}` (tracked internally) — kinds shipped, linux migrated via `fauna-client-notifications` / `fauna-client-contacts`, HTTP twins + `paths::{contacts,knocks,notifications}` + `inbox::mode_for_actor` deleted (pure same-nest routes, no federation residue). Plus the **account cluster** — `fauna.account.{get,delete,upgrade,am_i_admin}` + `fauna.quota.get` + `fauna.profile.handle.change` (tracked internally) — kinds shipped, linux migrated via `fauna-client-account`, HTTP twins (`/api/v1/{account,quota,am-i-admin}`, `PUT /api/v1/profile/handle`, `POST /api/v1/upgrade`) + the `paths::account` constants (all but `EXPORT`) deleted (pure same-nest routes, no federation residue); gated `User | Admin` (an admin manages their own account, and `am_i_admin` must be reachable by an admin). `GET /api/v1/export` stays HTTP (streaming `application/zip` byte download, residue). Plus the **Bluesky-native thread view** — `bluesky.feed.thread` (the one protocol-unique consume-side Bluesky kind; `libs/fauna-protocol/src/bluesky.rs`, feature-gated `bluesky::bluesky_handlers`, tier_3 `conformance_bluesky.rs`) — kind shipped + the two HTTP thread twins deleted (tracked internally, Commit B); android + linux app fan-out onto the kind **complete** (the only two post-detail consumers; 2026-06-06). (android + linux were the only post-detail consumers; no sibling app held a thread call-site.) **Remaining:** none — the file-sync metadata control plane rides `fauna.sync.*` / `fauna.filesync.*` kinds (only the byte plane — chunks / manifests — stays HTTP, per § HTTP residue).
- **Layer 2 — Protocol Client API:** reduced to residue — OAuth flows, byte transfer. The Bluesky thread view migrated to `bluesky.feed.thread`, nostr DMs to WS-RPC (dedicated kinds first; since 2026-10-03 the bridged-conversation family, `fauna.bridges.conversation.*`), the nostr control plane to `fauna.bridges.*` (twins deleted), and — 2026-07-22, the native-content rip — nostr's last 3 account routes (`zaps`/`badges`/`publish-signed`) to the prefix-less `nostr.{zaps.total,badges.list,events.publish_signed}` kinds, closing `/api/v1/nostr/*` entirely (§ Nostr above). The 4 ActivityPub account routes were ripped 2026-07-16 — the `ActivityPubProvider` on `fauna.bridges.*` had always been their twin, and no app called them.
- **Layer 3 — Bridge Management API:** **fully migrated.** T1–T6 shipped the bridges-management + email-filters subsurfaces; T7+T8 shipped `fauna.email.send` + deleted the HTTP `/api/v1/email/send` route; T9+T10 migrated linux typed end-to-end and deleted every remaining HTTP twin (`/api/v1/bridges/*`, `/api/v1/bridge-feeds`, `/api/v1/email/filters/*`) plus the `paths::bridges` constants and the `BridgeProvider` trait's `serde_json::Value` boundary (now typed `fauna_cbor::Value` end-to-end). **macos/ios migrated** via the `FfiBridgesClient` / `FfiEmailClient` UniFFI seam (`libs/fauna-ffi/src/{bridges,email_client}.rs` wrapping `fauna-client-bridges` / `fauna-client-email`, consumed by `apps/fauna-apple/.../APIClient.swift` over the shared `FfiNestClient` connection): bridge `Ffi*` replies map to the existing FaunaKit structs (BridgeFFIMapping.swift), email filters ride the typed `Ffi*` end-to-end (the old `[[String:Any]]`/`Any` Swift shapes had drifted from the protocol and were removed). With web, android, linux, and macos/ios done, **windows also adopted** via the C# `NestRpcClient` façade (`fauna.bridges.*` consumed by the Feed/Status VMs, `fauna.email.filters.*` — list/create/delete/get/update — over the same seam; `DirectNestClient.cs` carries no HTTP twin for either, per-method email coverage confirmed complete).
- **Layer 4 — Bridge Proxy:** **deleted at the I6 cutover** (the daemon-socket architecture is gone); no admin-side bridge HTTP remains — service-user enrollment is WS-RPC (`fauna.bridges.request_enrollment` + the admin approval kinds).
- **Layer 5 — Admin API:** **WS-RPC (complete)** — every `/admin/api/*` HTTP twin was deleted in the rip-out (Track C); admin tools are Fauna apps per the product invariant.
- **Layer 6 — Federation API:** Fauna↔Fauna rides the nest↔nest WS-RPC federation channel (sole carrier since Spec Y2 slice 5); HTTP stays only where protocol-required (ActivityPub, CalDAV, MTA-STS, webfinger/nodeinfo).
- **Layer 7 — Internal API:** the internal WS channels (`/internal/{worker,relay}/ws`); the internal HTTP mesh twins are gone, and so is the algorithm channel (removed 2026-10-01, [`core-client-kind-catalog.md`](core-client-kind-catalog.md) § Algorithm & Reputation).

**Per-kind migrations (selected; see route tables above for full set):**
- **Subscriptions** — migrated 2026-05-15. 15 authenticated routes
  (`tiers.{create,update,delete}`, `subscribe`, `unsubscribe`,
  `status.get`, `requests.{list,approve,reject}`, `key_blob.get`,
  `subscribers.{list,remove}`, `delegate.upload`) live as
  `fauna.subscriptions.*` WS-RPC kinds (the tier-MLS pair `epoch_secret.get` /
  `archival_blob.get` was retired with its plane on 2026-09-27 —
  [`../behavior/restricted-posts.md`](../behavior/restricted-posts.md) § Encryption at rest, room ruling 8).
  The mutating HTTP twins were **removed** in the WS-RPC-everywhere cutover
  (verified 2026-06-19 — matches § Subscriptions; they are no longer
  `#[deprecated]` twins). Three public
  routes (`GET /tiers/{author_id}`, `GET /delegate/{author_id}`,
  `GET /nest/info`) stay HTTP. A **16th, net-new** kind
  `fauna.subscriptions.tiers.list` was added 2026-06-18 (not a migrated
  route): the *authenticated own-read* of the caller's tier definitions
  powering the profile Tiers-tab SELF "My tiers" list. It complements the
  public `GET /tiers/{author_id}` (which is the unauthenticated /
  another-creator path); the WS-RPC reply is a superset — it also carries
  `auto_approve` (the HTTP twin previously omitted it; the JSON now
  includes it too). A **17th, net-new** kind `fauna.subscriptions.mine.list`
  was added 2026-06-18 (also not a migrated route): the *caller-scoped
  consumer enumeration* of the caller's subscriptions across all creators
  (active subscriber rows + pending subscribe requests, deduped; each with
  the creator's nest-resolved handle), powering the `subscription-settings`
  consumer page. Distinct from the per-creator `status.get`. Client seam built
  2026-05-22 (`libs/fauna-client-subscriptions::SubscriptionsClient` +
  `fauna-ffi`'s `FfiSubscriptionsClient`, the analogue of the bridges/email
  wrappers); the linux profile Tiers-tab SELF build + the `subscription-settings`
  consumer page (both 2026-06-18) are the first consumers (tracked
  internally). An **18th** net-new kind
  `fauna.subscriptions.offers.list` was added 2026-06-19 (not a migrated route):
  the *authenticated another-author* read of a target author's public tier
  offerings (request-supplied `author_id`, reusing the `TiersListReply` shape) —
  the WS-RPC path the in-app subscriber-browse on another profile uses, so no
  authenticated Fauna app reads the public `GET /tiers/{author_id}` HTTP route
  anymore (it survives only for Pillar-2 web-paywall / external consumers).
- **Segments** — migrated 2026-05-16. Manual compaction trigger
  (`fauna.segments.compact`) joins `fauna.segments.list` in the
  namespace. The HTTP twin `POST /api/v1/segments/compact` had no
  app consumers (added,
  never integrated), so the cleanest pre-prod path was a straight
  replacement: route registration deleted, no twin, no migration
  window.
- **Filesync (message-kind snapshots)** — migrated 2026-05-16; read
  surfaces extended 2026-05-22. Authenticated kinds
  `snapshot.create_message_kind`, `snapshot.delete_immediate`,
  `snapshot.restore_message_kind`, `snapshot.list_restore_history`,
  `snapshot.list_restore_divergence`, and `snapshot.list` (the
  forward-compatible / unified snapshot-list, message-kind-scoped today —
  added 2026-05-22 to give the Backups restore picker a local snapshot
  source; no HTTP twin) live as `fauna.filesync.*` WS-RPC kinds. The HTTP twins (the
  `?kind=mail|calendar` branch of `POST /api/v1/snapshots`, the
  `?immediate=true` branch of `DELETE /api/v1/snapshots/{id}`, and
  the message-kind branch of `POST /api/v1/snapshots/{id}/restore`)
  had no app consumers (added, never
  integrated), so they were deleted from the dispatcher rather than
  retained as twins. The folder ZIP-archive flow at
  `POST /api/v1/snapshots/{id}/restore` stayed HTTP until the route was
  deleted 2026-09-27 (compat-remnant sweep; folder restore is client-side).
- **Filesync (folder snapshot CRUD) + sync.backup_status — Linux
  adopted** 2026-05-31 (tracked internally). The Linux Backups
  page moved its folder CRUD off the deprecated `/api/v1/snapshots/*` +
  `/api/v1/sync/backup-status` HTTP onto the same shared
  `fauna-client-snapshots` (folder CRUD methods) + `fauna-client-sync`
  adapters web uses — over the native `Arc<NestClient>` (priority #2: the
  same kind-composition, lifted not reimplemented). `fetch_backup_status` →
  `sync.backup_status`; `fetch_snapshots` → `snapshot.list` folder mode
  (unknown set → `not_found` mapped to empty, mirroring the twin's 404);
  `fetch_snapshot_detail`/`delete_snapshot` parse the UI's string id to the
  kind's `i64`; `create_snapshot`/`prune_snapshots`/`check_integrity` →
  `snapshot.{create_folder,prune,check}`. The `DataMessage` payloads are
  now the typed protocol rows (was `serde_json::Value`), matching the
  already-typed message-kind siblings. Only the single-file byte download
  (`GET /api/v1/snapshots/{id}/file/{path}`) stayed HTTP, until its
  2026-09-27 deletion in the compat-remnant sweep. (The HTTP control
  twins have since been **deleted** — see § Snapshots; the "stays
  `#[deprecated]` until the remaining apps adopt" state this entry
  originally recorded is over.)
- **Filesync (folder snapshot CRUD) + sync.backup_status — web
  adopted** 2026-05-31 (tracked internally). The web Backups
  page moved off the deprecated `/api/v1/snapshots/*` +
  `/api/v1/sync/backup-status` HTTP onto the Track B15 folder CRUD
  kinds (`snapshot.{create_folder,get,delete,undelete,prune,check,
  diff}` + the `list` `folder` mode) and the Track B13
  `fauna.sync.backup_status` kind, via the shared
  `fauna-client-snapshots` (extended with the folder CRUD methods) +
  the new `fauna-client-sync` adapter, surfaced to the SPA through
  `libs/fauna-wasm`. (The HTTP control twins have since been **deleted** —
  see § Snapshots; android/apple followed web/linux onto the kinds, and
  windows lifts the same seam.) The snapshot **byte downloads** (single-file
  `GET /api/v1/snapshots/{id}/file/{path}`, ZIP restore
  `POST /api/v1/snapshots/{id}/restore`) stayed HTTP residue until their
  2026-09-27 deletion in the compat-remnant sweep.
- **Email.send (Layer 3 bridges)** — migrated 2026-05-15. The
  `fauna.email.send` WS-RPC kind shipped together with deletion of
  the HTTP `/api/v1/email/send` route (only consumers were a Linux UI
  stub and the dormant Rust bridge-daemon — both migrated or gutted
  in the same commit; no twin retained).
- **CBOR-DAG-everywhere Layer 3 — CARv2 at-rest + blob endpoint** — landed 2026-05-17. Segment files (`libs/fauna-segment-store`) and index manifests (`libs/fauna-index`) now use standard CARv2 framing via the new wrapper crate `libs/fauna-carv2`. New canonical byte-source surface `GET|PUT /api/v1/blob/{cid_b32}` (octet-stream + server-side `blake3(body) == cid.digest()` verification) ships in `bins/fauna-nest/src/blob_routes.rs`; the `GET /api/v1/segments/{kind}/{actor}/{segment_id}` byte route stays, serving CARv2 bytes (ruled permanent 2026-10-01 — `segment-backup-protocol.md` § Byte-source endpoint). `fauna_cbor::Cid` is now codec-parametric (`Cid::DAG_CBOR = 0x71`, `Cid::RAW = 0x55`); `ContentHash` collapsed to `pub type ContentHash = fauna_cbor::Cid;` (raw codec) — wire shape stays 36 bytes; every kind embedding ContentHash gains 4 bytes per field (intended per spec).

Layer-by-layer end state:

- **Layer 1 — Core Client API:** **WS-RPC** (complete; kinds `fauna.<area>.<verb>` per `docs/goal/architecture/transport.md` § Namespace policy). Bytes-bulk surfaces (CID-keyed blob `GET|PUT /api/v1/blob/{cid_b32}`, chunks, manifests, video, media, snapshot file content, file version content, segments byte-source) stay HTTP `application/octet-stream` (§ HTTP residue).
- **Layer 2 — Protocol Client API:** **WS-RPC for everything interactive** (`bluesky.feed.thread`, Nostr DMs on `fauna.bridges.conversation.*`, control plane on `fauna.bridges.*`, and — since 2026-07-22 — nostr native content `nostr.{zaps.total,badges.list,events.publish_signed}`); only OAuth callbacks + AP native account routes stay HTTP (§ HTTP residue) — nostr has none left; the Nostr relay WebSocket is its own protocol (out of scope here).
- **Layer 3 — Bridge Management API:** **WS-RPC** (`fauna.bridges.*`, complete).
- **Layer 4 — Bridge Proxy:** **deleted** at the I6 cutover; no admin-side bridge HTTP remains (enrollment is WS-RPC).
- **Layer 5 — Admin API:** **WS-RPC** (complete) — admin is a Fauna app per the product invariant. Bulk export stays HTTP `application/octet-stream`.
- **Layer 6 — Federation API:** Fauna↔Fauna on the **nest↔nest WS-RPC federation channel**; HTTP only where protocol-required (ActivityPub JSON-LD, CalDAV XML, MTA-STS, WebFinger, NodeInfo, OAuth callbacks).
- **Layer 7 — Internal API:** the internal WS channels (`/internal/{worker,relay}/ws`). The user-facing `claim-admin` bootstrap rides the pre-identity WS-RPC kind `fauna.auth.claim_admin`.

The complete endpoint-level inventory of what stays HTTP (JSON, XML, and byte residue alike)
lives in **§ HTTP residue — the single inventory** at the top of this doc — the one owner
copy (transport.md § HTTP residue defers here; ratified 2026-07-07, cluster #2 review).

### Rust client-side HTTP code (`libs/fauna-nest-http`)

The native-Rust consumers of these endpoints share one crate, `libs/fauna-nest-http` — the consolidated home for "native Rust talking HTTP to a fauna nest":

- the `ApiError` taxonomy (`Status { code, message }` / `Transport(String)`);
- the `paths::*` constants — the union of what the consumers call, grouped by feature, every module now PERMANENT HTTP residue (the WS-RPC-migratable and MIXED modules left with their routes as the Spec-Y content-kind migration completed);
- a `BearerSource` trait — its production default is `fauna-client`'s `WsChallengeBearer`, which mints over the WS-RPC silent challenge `fauna.auth.{challenge,verify}` + TTL-caches (the in-crate impls are `LaunchMachineBearer`, behind the `launch-machine` feature — reads the launch flow's silent-challenge-minted bearer + reactively re-mints on a 401; and `StaticBearer` for tests). The legacy `KeypairBearer` HTTP `/api/v1/auth/token` signer was **removed** once every Rust client minted over `fauna.auth.handshake`, and the route itself was **deleted** at the `auth/token` endgame;
- `ReqwestNestContentApi<B: BearerSource>` — the single chokepoint for the bearer attach, the 401-reactive one-retry refresh, and the structured-`{"error":…}` extraction.

Consumers: `apps/fauna-linux/src/nest_content_api/` (a re-export of this crate + the `LaunchMachineBearer` glue), and — follow-ups — `libs/fauna-client`'s `AuthClient` and `libs/fauna-onboarding-machine`'s `nest_api` (which share the error taxonomy + path constants but keep their own trait shapes). (The `bins/fauna-bridge-daemon` consumer was removed when the daemon was deleted at the I6 cutover.) It is **not** WS-RPC (that's `fauna-protocol` + `fauna-client`) and **not** the web/WASM layer (web keeps its TS `api.ts`); the non-Rust apps (web `api.ts`, Windows `DirectNestClient.cs`, Android `ApiClient.kt`, Swift FaunaKit) re-implement this *shape* in their own language, the crate as their reference. As WS-RPC kinds shipped per `transport.md`'s migration policy, the corresponding path constant + its trait method retired; no migratable constants remain. Design ratified 2026-05-11 (tracked internally).
