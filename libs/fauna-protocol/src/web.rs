//! Web-content-publishing WS-RPC payload types. A behavior-preserving transport
//! migration of the 5 bearer-authed web-content-hosting HTTP routes
//! (`web_content::{publish_routes, domain}`) onto the per-actor WS-RPC
//! connection — Track B18 of the WS-RPC-everywhere migration
//! (tracked internally). The handlers reuse the
//! existing core fns exactly (no shared core needed).
//!
//! Six kinds (api-layers.md § Web Content Publishing):
//!
//! - `fauna.web.publish.set` ≡ POST `/api/v1/web/publish/{post_id}` — publish a
//!   post under an optional slug (default = the post-id hex). Upsert.
//! - `fauna.web.publish.unset` ≡ DELETE `…/{post_id}` — take a post down
//!   (idempotent).
//! - `fauna.web.publish.list` ≡ GET `/api/v1/web/publish` — the caller's
//!   published posts.
//! - `fauna.web.domain.set` ≡ PUT `/api/v1/web/domain` — register a custom
//!   domain; returns the DNS verify token + the TXT record name to set.
//! - `fauna.web.domain.get` ≡ GET `/api/v1/web/domain` — the caller's domains.
//! - `fauna.web.domain.delete` — deregister a custom domain the caller owns
//!   (idempotent). No HTTP twin — added with the custom-domain TLS lifecycle.
//!
//! Plus two **Admin-class** apex-hosting kinds (web-content-hosting.md § Admin
//! apex hosting — the direct analogue of `fauna.bridges.set_catch_all_actor`, but
//! a nest-wide singleton instead of a per-`mail_domains` row):
//!
//! - `fauna.web.set_apex_actor` — designate (`Some`) / clear (`None`) the actor
//!   whose `web` content serves `https://<node-domain>/`.
//! - `fauna.web.get_apex_actor` — read the current apex designation.
//!
//! These ride the **bearer** connection and ARE actor-scoped — the connection
//! `actor_id` is the data scope (the twin's `bearer.0.0`). The publish/domain
//! kinds gate `User | Admin`; the apex kinds gate `Admin` only.
//!
//! Wire convention (matching `files.rs` / `account.rs`): `post_id` rides as raw
//! bytes ([`ByteBuf`]; the twin parsed a hex path param); every numeric field is
//! `i64` (the dag-cbor wire forbids floats); optionals are plain `Option`;
//! `#[serde(flatten, default)] extra` carries forward-compat fields. The
//! publicly-served HTML pages stay HTTP (residue) — they are for non-Fauna
//! browsers, not Fauna apps.
//!
//! Kind registry: `kind.rs::register_web_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{ByteBuf, Value};

// ── fauna.web.publish.set (≡ POST /api/v1/web/publish/{post_id}) ─────────────

/// Publish a post as a web page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebPublishSetRequest {
    /// 32-byte post id (the twin's `{post_id}` hex path param, here raw bytes).
    pub post_id: ByteBuf,
    /// Optional URL slug; `None`/empty → the post-id hex (the twin's default).
    pub slug: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The effective slug the post is published under (the twin's `{slug}` body).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebPublishSetReply {
    pub slug: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.publish.unset (≡ DELETE …/{post_id}) ──────────────────────────

/// Take a published post down (idempotent — no error if it was not published).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebPublishUnsetRequest {
    pub post_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success (the twin returned 204 No Content).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebPublishUnsetReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.publish.list (≡ GET /api/v1/web/publish) ──────────────────────

/// List the caller's published posts. No parameters — the actor scope is the
/// connection actor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebPublishListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One published post: its id, public slug, and (additively) the tier gating
/// it.
///
/// `Default` exists for struct-update fixtures (`..Default::default()`) so two
/// branches independently growing this wire type merge cleanly instead of
/// colliding on hand-listed fields (the growing-wire-type fixture convention).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PublishedPost {
    pub post_id: ByteBuf,
    pub slug: String,
    /// Tier name of a gated-to-tier post, joined from `content_meta.gated_tier`
    /// at list time so the `web-settings` management rows know which of them
    /// carry the *Copy paywall link* affordance (`behavior/monetization.md`
    /// § Pillar 2 → *Creator comp-link surface*: published **and** gated only).
    /// `None` = ungated — additive + `None`-default
    /// (`behavior/web-content-hosting.md` § Published-post management).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_tier: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebPublishListReply {
    pub posts: Vec<PublishedPost>,
    /// `true` while the caller's **rendered** site is dark because a render
    /// failed and the nest cleared it rather than keep serving withdrawn
    /// content — the nest's `web_restore_owed` state. The nest restores the
    /// site by itself, so this asks nothing of the author; it exists so the
    /// `web-settings` page can say the pages are down instead of looking
    /// healthy. Synced static files are unaffected and keep serving.
    /// Additive: `false` sends no key at all, and an absent key reads as `false` (`behavior/web-content-hosting.md` § Routing,
    /// render, serving → *A blanked site tells its author*).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rendered_pages_down: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.domain.set (≡ PUT /api/v1/web/domain) ─────────────────────────

/// Register a custom domain for published web content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainSetRequest {
    pub domain: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The freshly-registered domain (status `pending`) plus the DNS verification
/// token and the TXT record name the user must set (the twin's CREATED body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainSetReply {
    pub domain: String,
    pub verify_token: String,
    /// The TXT record name to set, `_fauna-verify.{domain}`.
    pub txt_record: String,
    /// Always `pending` on a fresh registration.
    pub status: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.domain.get (≡ GET /api/v1/web/domain) ─────────────────────────

/// List the caller's registered domains. No parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebDomainGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One registered domain (the twin's per-row projection).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainInfo {
    pub domain: String,
    pub verify_token: String,
    pub txt_record: String,
    /// `pending` / `verified` / `active`.
    pub status: String,
    pub created_at: i64,
    /// Unix-seconds the domain was verified; `None` while still pending.
    pub verified_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainGetReply {
    pub domains: Vec<WebDomainInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.domain.delete ─────────────────────────────────────────────────

/// Deregister a custom domain the caller owns. Idempotent — deleting a domain
/// that is not registered (or already deleted) succeeds with `ok: false`. The
/// nest's per-domain cert lifecycle then drops the domain's TLS cert from the
/// live resolver and removes its `acme_dir/<domain>/` on its next pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainDeleteRequest {
    pub domain: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `ok` is `true` if a registration was removed, `false` if there was nothing
/// to remove (idempotent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebDomainDeleteReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.set_apex_actor (Admin) ────────────────────────────────────────

/// Designate (or clear) the deployment's apex web-content actor — the actor
/// whose `web` content serves `https://<node-domain>/`. `Some` designates;
/// `None` clears the designation and reverts the apex to the built-in nest info
/// page. **Admin-class**, the nest-wide-singleton analogue of
/// `fauna.bridges.set_catch_all_actor` (`SetCatchAllActorRequest`): there the
/// admin designates a per-domain catch-all *mail* actor; here a single apex
/// *web* actor for the one node domain. See `web-content-hosting.md` § Admin
/// apex hosting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSetApexActorRequest {
    /// 32-byte actor id to designate; `None` clears the apex designation.
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Echoes the resulting designation after the set/clear (`None` ⇒ cleared).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSetApexActorReply {
    pub actor_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.get_apex_actor (Admin) ────────────────────────────────────────

/// Read the current apex web-content actor designation. No parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebGetApexActorRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The current apex actor, or `None` if none is designated (apex serves the
/// built-in info page).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebGetApexActorReply {
    pub actor_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.set_subdomain_enabled (User) ──────────────────────────────────

/// Opt into (or out of) per-user subdomain hosting — `enabled` makes the
/// **caller's own** `web` content serve `https://<handle>.<node-domain>/` over a
/// per-subdomain HTTP-01 cert; `false` drops the mapping + cert. Caller-scoped
/// (NO `actor_id` — the connection actor is the subject, exactly like
/// `fauna.web.publish.*`), so an actor can only toggle its own hosting. Default
/// OFF (privacy / user-controls-their-data). See `web-content-hosting.md`
/// § Routing (subdomain) + Architectural rule 8.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSetSubdomainEnabledRequest {
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Echoes the resulting opt-in state after the set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSetSubdomainEnabledReply {
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.get_subdomain_enabled (User) ──────────────────────────────────

/// Read the caller's own subdomain-hosting opt-in state. No parameters — the
/// connection actor is the subject.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebGetSubdomainEnabledRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The caller's current opt-in state (`false` ⇒ not hosting a subdomain).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebGetSubdomainEnabledReply {
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.web.paywall.mint_token (User | Admin) ──────────────────────────────

/// Mint a short-lived web-paywall capability URL token for the **caller's
/// own** paywalled resource (`monetization.md` § Pillar 2 — the creator
/// comp-link surface; the Pillar-3 entitlement engine mints through the same
/// nest-side mint). Caller-scoped: the connection actor is the content owner,
/// so an actor can only open its own paywalled resources.
///
/// Exactly one of the two targets is given:
///
/// - `slug` — a published **post**, gated to a tier (the `fauna.web.publish.set`
///   slug). The token is scoped to its rendered `post/{slug}.html`.
/// - `path` — a **file** in a paywalled `web` folder (the folder half). The
///   token is scoped to that path verbatim; the `(owner, path)` token scope
///   covers file paths unchanged.
///
/// `path` is additive (a slug mint sends none; an empty `slug` with no `path`
/// → `invalid_request`, never a wrong mint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebPaywallMintTokenRequest {
    #[serde(default)]
    pub slug: String,
    /// The paywalled file's path within the actor's web content (e.g.
    /// `downloads/report.pdf`). Mutually exclusive with `slug`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The minted bearer token (the `?token=` URL value), its expiry (unix
/// seconds; short TTL, freely re-mintable), and the rendered path it is
/// scoped to (`post/{slug}.html`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebPaywallMintTokenReply {
    pub token: String,
    pub expires: u64,
    pub path: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.web.files.prune_sealed` — the owner's client declaring the **complete**
/// live path set of one folder's website corpus, so the nest can drop the
/// **sealed** `web_files` rows that are no longer in
/// it.
///
/// Why this exists at all: `web_files` is a projection of the folder's live sync
/// heads, and the nest reconciles it itself for **plaintext**-resting heads. A
/// *sealed* head rests no plaintext name (S9), so the nest cannot see that class
/// — its projection rows are maintained only by client re-records, which are
/// additive by construction. The missing sentence was *"and nothing else"*, and
/// it can only be spoken by the client. Without it, a sealed path deleted while
/// the website toggle was OFF kept serving after re-enable: a delete that does
/// not take effect on a published surface.
///
/// **`paths` must be COMPLETE**, and is only meaningful straight after a fully
/// successful re-record walk — a truncated set deletes live content. The client
/// sends it after the walk, never before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebFilesPruneSealedRequest {
    /// The folder whose projection is being reconciled. Owner-only: the nest
    /// resolves it against the calling actor's own rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Every path that should still have a row — folder-relative, exactly as
    /// the sync change records spell them.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// How many sealed rows the prune removed. Zero is the steady state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebFilesPruneSealedReply {
    pub dropped: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The wire budget for [`WebFilesPruneSealedRequest::paths`].
///
/// The declaration is one message and must fit one 2 MiB WS frame
/// ([`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`]) — the same reasoning
/// (and the same headroom shape) as [`crate::mls_replica::MAX_MLS_REPLICA_BYTES`].
/// A corpus whose paths do not fit is not an error: the client **skips** the
/// prune and logs, which degrades to the pre- behaviour (the stale row
/// survives) rather than declaring a truncated set, which would delete live
/// content.
pub const MAX_PRUNE_SEALED_PATH_BYTES: usize =
    fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE - 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// The prune declaration round-trips, and a request sending no `paths`
    /// at all must decode as an EMPTY set, never as
    /// "unspecified", because the handler reads it as "keep nothing".
    /// That is exactly why the client, not the wire, decides whether to send.
    #[test]
    fn prune_sealed_round_trips_and_defaults_to_an_empty_set() {
        assert_round_trips(&WebFilesPruneSealedRequest {
            folder: "site".into(),
            paths: vec!["index.html".into(), "assets/app.css".into()],
            ..Default::default()
        });
        assert_round_trips(&WebFilesPruneSealedReply {
            dropped: 3,
            extra: Default::default(),
        });

        #[derive(serde::Serialize)]
        struct BareFolder {
            folder: String,
        }
        let bytes = encode_canonical(&BareFolder {
            folder: "site".into(),
        })
        .unwrap();
        let decoded: WebFilesPruneSealedRequest = decode(&bytes).unwrap();
        assert!(decoded.paths.is_empty());
    }

    #[test]
    fn publish_set_round_trips_with_and_without_slug() {
        assert_round_trips(&WebPublishSetRequest {
            post_id: ByteBuf::from(vec![0x11; 32]),
            slug: Some("hello-world".into()),
            extra: BTreeMap::new(),
        });
        // No slug → null → round-trips to None.
        let bare = WebPublishSetRequest {
            post_id: ByteBuf::from(vec![0x11; 32]),
            slug: None,
            extra: BTreeMap::new(),
        };
        let decoded: WebPublishSetRequest = decode(&encode_canonical(&bare).unwrap()).unwrap();
        assert_eq!(bare, decoded);
        assert!(decoded.slug.is_none());
        assert_round_trips(&WebPublishSetReply {
            slug: "hello-world".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn publish_unset_and_list_round_trip() {
        assert_round_trips(&WebPublishUnsetRequest {
            post_id: ByteBuf::from(vec![0x22; 32]),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebPublishUnsetReply {
            ok: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebPublishListReply {
            posts: vec![
                PublishedPost {
                    post_id: ByteBuf::from(vec![0x01; 32]),
                    slug: "first".into(),
                    ..Default::default()
                },
                PublishedPost {
                    post_id: ByteBuf::from(vec![0x02; 32]),
                    slug: "second".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let empty = WebPublishListReply::default();
        let decoded: WebPublishListReply = decode(&encode_canonical(&empty).unwrap()).unwrap();
        assert!(decoded.posts.is_empty());
    }

    #[test]
    fn published_post_gated_tier_is_additive_both_directions() {
        // Present: a gated row carries the tier the management surface reads to
        // decide whether the paywall-link affordance renders.
        assert_round_trips(&WebPublishListReply {
            posts: vec![PublishedPost {
                post_id: ByteBuf::from(vec![0x03; 32]),
                slug: "gated".into(),
                gated_tier: Some("gold".into()),
                ..Default::default()
            }],
            ..Default::default()
        });

        // An ungated post answers without the key at all → `None`, never a decode
        // error (`web-content-hosting.md` § Published-post management, the
        // additive-wire-evolution ruling). Modelled by a field-less struct.
        #[derive(serde::Serialize)]
        struct UngatedPublishedPost {
            post_id: ByteBuf,
            slug: String,
        }
        let ungated_row = UngatedPublishedPost {
            post_id: ByteBuf::from(vec![0x04; 32]),
            slug: "legacy".into(),
        };
        let bytes = encode_canonical(&ungated_row).unwrap();
        let decoded: PublishedPost = decode(&bytes).unwrap();
        assert_eq!(decoded.slug, "legacy");
        assert!(
            decoded.gated_tier.is_none(),
            "an ungated row must decode with gated_tier absent, not fail"
        );
        assert!(
            decoded.extra.is_empty(),
            "the known keys must bind to fields, never spill into `extra`"
        );

        // A row carrying a key this decoder does not know is modelled by
        // the field landing in `extra` — proven here by the key surviving a
        // round-trip through the untyped map.
        let new_row = PublishedPost {
            post_id: ByteBuf::from(vec![0x05; 32]),
            slug: "fresh".into(),
            gated_tier: Some("silver".into()),
            ..Default::default()
        };
        let as_map: BTreeMap<String, Value> = decode(&encode_canonical(&new_row).unwrap()).unwrap();
        assert!(
            matches!(as_map.get("gated_tier"), Some(Value::String(t)) if t == "silver"),
            "the additive key is on the wire under its own name, got {:?}",
            as_map.get("gated_tier")
        );

        // Absent tier is omitted from the encoding entirely (no null key).
        let public_row = PublishedPost {
            post_id: ByteBuf::from(vec![0x06; 32]),
            slug: "public".into(),
            gated_tier: None,
            ..Default::default()
        };
        let as_map: BTreeMap<String, Value> =
            decode(&encode_canonical(&public_row).unwrap()).unwrap();
        assert!(
            !as_map.contains_key("gated_tier"),
            "an ungated row must not emit the key at all"
        );
    }

    #[test]
    fn rendered_pages_down_is_additive_both_directions() {
        assert_round_trips(&WebPublishListReply {
            rendered_pages_down: true,
            ..Default::default()
        });

        // A healthy site never sends the key → never a decode
        // error (`web-content-hosting.md` § Routing, render, serving → *A
        // blanked site tells its author*). Modelled by a field-less struct.
        #[derive(serde::Serialize)]
        struct HealthyListReply {
            posts: Vec<PublishedPost>,
        }
        let bytes = encode_canonical(&HealthyListReply { posts: vec![] }).unwrap();
        let decoded: WebPublishListReply = decode(&bytes).unwrap();
        assert!(!decoded.rendered_pages_down);
        assert!(decoded.extra.is_empty());

        // A healthy site emits no key at all, so the reply carries no extra
        // key; a dark one carries it under its own name.
        let healthy: BTreeMap<String, Value> =
            decode(&encode_canonical(&WebPublishListReply::default()).unwrap()).unwrap();
        assert!(!healthy.contains_key("rendered_pages_down"));
        let dark: BTreeMap<String, Value> = decode(
            &encode_canonical(&WebPublishListReply {
                rendered_pages_down: true,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            dark.get("rendered_pages_down"),
            Some(Value::Bool(true))
        ));
    }

    #[test]
    fn domain_set_round_trips() {
        assert_round_trips(&WebDomainSetRequest {
            domain: "example.com".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebDomainSetReply {
            domain: "example.com".into(),
            verify_token: "fauna-verify-0123456789abcdef".into(), // gitleaks:allow
            txt_record: "_fauna-verify.example.com".into(),
            status: "pending".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn domain_delete_round_trips() {
        assert_round_trips(&WebDomainDeleteRequest {
            domain: "example.com".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebDomainDeleteReply {
            ok: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebDomainDeleteReply {
            ok: false,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn domain_get_round_trips_with_and_without_verified_at() {
        assert_round_trips(&WebDomainGetReply {
            domains: vec![WebDomainInfo {
                domain: "active.example.com".into(),
                verify_token: "fauna-verify-abc".into(),
                txt_record: "_fauna-verify.active.example.com".into(),
                status: "active".into(),
                created_at: 1_700_000_000,
                verified_at: Some(1_700_001_000),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        // Pending domain → verified_at None → null → round-trips to None.
        let pending = WebDomainGetReply {
            domains: vec![WebDomainInfo {
                domain: "pending.example.com".into(),
                verify_token: "fauna-verify-def".into(),
                txt_record: "_fauna-verify.pending.example.com".into(),
                status: "pending".into(),
                created_at: 1_700_000_000,
                verified_at: None,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        let decoded: WebDomainGetReply = decode(&encode_canonical(&pending).unwrap()).unwrap();
        assert_eq!(pending, decoded);
        assert!(decoded.domains[0].verified_at.is_none());
    }

    #[test]
    fn set_apex_actor_round_trips_designate_and_clear() {
        // Designate: Some(32-byte actor id) round-trips.
        assert_round_trips(&WebSetApexActorRequest {
            actor_id: Some(ByteBuf::from(vec![0xAB; 32])),
            extra: BTreeMap::new(),
        });
        // Clear: None → null → round-trips to None.
        let clear = WebSetApexActorRequest {
            actor_id: None,
            extra: BTreeMap::new(),
        };
        let decoded: WebSetApexActorRequest = decode(&encode_canonical(&clear).unwrap()).unwrap();
        assert_eq!(clear, decoded);
        assert!(decoded.actor_id.is_none());
        // Reply echoes the resulting designation, both arms.
        assert_round_trips(&WebSetApexActorReply {
            actor_id: Some(ByteBuf::from(vec![0xAB; 32])),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebSetApexActorReply {
            actor_id: None,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn get_apex_actor_round_trips_with_and_without_designation() {
        assert_round_trips(&WebGetApexActorRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&WebGetApexActorReply {
            actor_id: Some(ByteBuf::from(vec![0x07; 32])),
            extra: BTreeMap::new(),
        });
        let none = WebGetApexActorReply {
            actor_id: None,
            extra: BTreeMap::new(),
        };
        let decoded: WebGetApexActorReply = decode(&encode_canonical(&none).unwrap()).unwrap();
        assert_eq!(none, decoded);
        assert!(decoded.actor_id.is_none());
    }

    #[test]
    fn subdomain_enabled_round_trips_both_states() {
        for enabled in [true, false] {
            assert_round_trips(&WebSetSubdomainEnabledRequest {
                enabled,
                extra: BTreeMap::new(),
            });
            assert_round_trips(&WebSetSubdomainEnabledReply {
                enabled,
                extra: BTreeMap::new(),
            });
            assert_round_trips(&WebGetSubdomainEnabledReply {
                enabled,
                extra: BTreeMap::new(),
            });
        }
        assert_round_trips(&WebGetSubdomainEnabledRequest {
            extra: BTreeMap::new(),
        });
    }
}
