//! ATProto (Bluesky) bridge library for Fauna.
//!
//! Provides OAuth authentication, XRPC client, content translation,
//! DM bridging, feed proxying, interaction bridging, and media proxying
//! for integrating Bluesky into a Fauna node.
//!
//! Feature layout: the default-on `client`
//! feature carries the heavy consume-side (reqwest/tokio/atrium/dashmap);
//! with `default-features = false` only the pure, synchronous host-direction
//! core (`outbound` + `types` + `authz`) compiles — the surface fauna-ffi
//! exposes to the Go atproto.pds bridge. The optional `uniffi` feature adds
//! the FFI derives to that core and nothing else.

/// D8 — the single per-request authorization decision point for the hosted
/// PDS. Pure + wasm-clean, in the always-on core: the Go atproto.pds bridge
/// consumes it over the tracked fauna-ffi Go binding.
pub mod authz;
/// F4 — confidential-client authentication (`private_key_jwt`): does this
/// caller hold a key the client's own published metadata declares? Pure +
/// wasm-clean, in the always-on core beside [`dpop`], and for the same reason —
/// the Go atproto.pds bridge consumes it over the tracked fauna-ffi binding,
/// and it decides everything about an assertion except the signature and the
/// replay memory.
pub mod client_assertion;

#[cfg(feature = "client")]
pub mod chat;
pub mod db;
/// F4 — DPoP proof validation (RFC 9449): does this caller hold the key it
/// claims to? Pure + wasm-clean, in the always-on core beside [`oauth_par`].
/// It decides everything about a proof except the three things that need a
/// key, a clock or a store — the signature, the nonce's provenance, and the
/// `jti`'s novelty — which are Go's, per the same ruling that put signing
/// there.
pub mod dpop;
/// The Fauna scope family (`fauna:<plane>:<verb>[:<qualifier>]`) — the closed
/// arm table [`authz::scope_grants_something`] and [`authz::describe_scope`]
/// dispatch to. Pure + wasm-clean, in the always-on core beside [`authz`].
pub mod fauna_scope;
/// The SSRF guard for attacker-directed outbound fetches (F3 service
/// proxying, F4 client-metadata resolution). Pure + wasm-clean, in the
/// always-on core beside [`authz`], for the same reason: the policy is one
/// decision point both callers share.
pub mod fetch_guard;
/// One compact-JWS decomposition, shared by every ES256 token F4 reads off a
/// caller's request — the DPoP proof and the `private_key_jwt` client
/// assertion. Crate-internal (a parsing primitive, not policy), and in the
/// always-on core because both its callers are.
mod jws;

/// Consume-side feed ingestion — an AppView **view** of a post → the Fauna
/// `Post` the nest stores, built on [`reverse_translate`]'s record half plus
/// the facts only the view knows (`bridges.md` § Unified feed ingestion).
/// `client`-gated with its inputs.
#[cfg(feature = "client")]
pub mod ingest;
#[cfg(feature = "client")]
pub mod keypair;
#[cfg(feature = "client")]
pub mod media;
/// D2/D6 — the per-lexicon membership policy deciding whether an external
/// repo write round-trips into Fauna, journals verbatim, or is refused. Pure
/// + wasm-clean, in the always-on core beside [`authz`] and [`fetch_guard`]:
/// one decision point, table-driven-testable.
pub mod membership;
#[cfg(feature = "client")]
pub mod oauth;
/// F4 — turning an OAuth `client_id` URL into a validated client-metadata
/// document (the loopback dev client included). Pure + wasm-clean, in the
/// always-on core: Go performs the fetch through [`fetch_guard`], this module
/// decides what the fetched bytes mean.
pub mod oauth_client;
/// F4 — the two OAuth discovery documents this PDS publishes, and the AS
/// endpoint paths they name. Pure + wasm-clean, in the always-on core beside
/// [`authz`]: the wire field names are vocabulary the policy owns, so the
/// bodies are rendered here and cross to Go as strings.
pub mod oauth_metadata;
/// F4 — pushed authorization requests (RFC 9126): may this request be stored
/// at all? Pure + wasm-clean, in the always-on core beside [`oauth_client`];
/// Go owns the bridge-memory store and the `request_uri` minting.
pub mod oauth_par;
pub mod outbound;
/// F4 — permission sets (`include:<NSID>?aud=…`): the `include:` grammar and
/// the expansion of a resolved set's document into ordinary granular scopes.
/// Pure + wasm-clean, in the always-on core beside [`authz`], which is the
/// ratified split — Go only ever fetches; expansion is this module's.
pub mod permission_set;
/// F2.2 slice 4b — the AT-URIs an external write refers to (reply parent,
/// quoted post), so the bridge can resolve them against `post_map` before the
/// nest call. Pure + wasm-clean, in the always-on core beside [`membership`]:
/// it walks raw dag-cbor with the same decoder [`reverse_translate`] uses, so
/// exporting it over FFI costs the cdylib none of `atrium_api`'s tree.
pub mod record_refs;
/// F2 reverse translation — an external app's ATProto record back into the
/// intermediate Fauna shape the D10 authoring sub-key then signs. The mirror
/// of [`outbound`] (Fauna → record); `client`-gated because it parses via
/// `atrium_api`'s record types, and nest-side only (no FFI export).
#[cfg(feature = "client")]
pub mod reverse_translate;
#[cfg(feature = "client")]
pub mod store;
#[cfg(feature = "client")]
pub mod translate;
pub mod types;
#[cfg(feature = "client")]
pub mod xrpc;

/// Test-only fixture helpers shared by [`record_refs`]'s and
/// [`reverse_translate`]'s own test modules, and by fauna-nest's bluesky
/// fixtures cross-crate (enable via the `test-helpers` feature). All three
/// build fixtures the same way — the Go bridge's `JSONRecordToDagCBOR`
/// encoding of a `serde_json::Value`, dag-json `{"/": …}` links included.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support {
    use ipld_core::ipld::Ipld;
    use std::str::FromStr;

    /// Encode a JSON record the way the Go bridge does before either side
    /// sees it (indigo's `JSONRecordToDagCBOR`), so fixtures exercise the
    /// real byte shape.
    pub fn dag_cbor(record: serde_json::Value) -> Vec<u8> {
        serde_ipld_dagcbor::to_vec(&json_to_ipld(record)).expect("fixture encodes")
    }

    /// Convert fixture JSON to IPLD, honouring the dag-json `{"/": …}` link
    /// spelling: it must reach dag-cbor as a **tag-42 link**, not a one-key
    /// map, or the record types would not deserialize.
    pub fn json_to_ipld(v: serde_json::Value) -> Ipld {
        match v {
            serde_json::Value::Null => Ipld::Null,
            serde_json::Value::Bool(b) => Ipld::Bool(b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(|i| Ipld::Integer(i as i128))
                .unwrap_or_else(|| Ipld::Float(n.as_f64().expect("fixture number"))),
            serde_json::Value::String(s) => Ipld::String(s),
            serde_json::Value::Array(a) => Ipld::List(a.into_iter().map(json_to_ipld).collect()),
            serde_json::Value::Object(o) => {
                if o.len() == 1
                    && let Some(serde_json::Value::String(cid)) = o.get("/")
                {
                    return Ipld::Link(
                        ipld_core::cid::Cid::from_str(cid).expect("fixture CID parses"),
                    );
                }
                Ipld::Map(o.into_iter().map(|(k, v)| (k, json_to_ipld(v))).collect())
            }
        }
    }

    /// A genuinely valid CIDv1 (raw codec, sha2-256) per `seed` — a base32
    /// CID string carries a length-checked multihash, so invented literals
    /// do not parse.
    pub fn test_cid(seed: u8) -> String {
        use ipld_core::cid::Cid;
        use ipld_core::cid::multihash::Multihash;

        let mh = Multihash::<64>::wrap(0x12, &[seed; 32]).expect("sha2-256 multihash wraps");
        Cid::new_v1(0x55, mh).to_string()
    }

    /// A minimal `app.bsky.feed.post` record fixture — `extra`'s keys
    /// override/extend the base three (`$type`/`text`/`createdAt`).
    pub fn post_json(extra: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "$type": "app.bsky.feed.post",
            "text": "hello from an external app",
            "createdAt": "2026-07-24T10:30:00.000Z",
        });
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }
}

/// Re-export `atrium_api` so consumers (e.g. fauna-nest) can use XRPC types
/// without adding a direct dependency.
#[cfg(feature = "client")]
pub use atrium_api;

// Registers this crate as a UniFFI component so the `authz` types carry their
// derives here (one definition, no mirror type in fauna-ffi). Gated: every
// other consumer — nest, wasm — compiles without uniffi in the graph.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_bridge_atproto");
