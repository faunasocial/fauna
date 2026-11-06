//! The oracle's ActivityPub class — `activitypub.http_sig` — as policy.
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the table row). A third-party
//! principal never holds the account's actor key; it asks the nest's
//! custodian for an HTTP Signature over one outbound request. This module is
//! the class's pure half — what the custodian checks about the REQUEST
//! before it signs, the same bytes in, the same verdict out, wherever it
//! runs. Who may ask (the live grant, the declared peer set read off the
//! roster row, the rate ceiling) is the nest custodian's half.
//!
//! The predicate:
//!
//! * [`parse_target`] — the request target is an `https` URL on the default
//!   port with no credentials and no fragment; its host is what the declared
//!   peer set is matched against, exactly (lowercase, no wildcard).
//! * [`parse_method`] — `GET` (an authorized fetch: no body) or `POST` (an
//!   inbox delivery: a body, at most [`MAX_BODY_BYTES`]).
//! * [`activity_admitted`] — the body is never opaque: it is an AS2 activity
//!   whose `actor` is this account's actor, and it is none of the sovereign
//!   shapes — `Move`, a `Delete` of the actor, an `Update` of the actor.
//!   The body is JSON-LD and a receiver may expand it, so the deny list runs
//!   only over a CANONICAL body, one every reader reads alike: a `@context`
//!   drawn from [`CONTEXT_IRIS`] alone, no `@`-keyword but `@context`, every
//!   property name and every `type` a plain term, a single-valued `object`, no duplicate key;
//!   anything else is refused, never interpreted.
//!   Those change *who the user is* on the fediverse (`Move` re-homes the
//!   followers, `Update{Person}` rewrites the profile and its
//!   `alsoKnownAs`/key), so they are the class's half of the deny list,
//!   exactly as the Nostr custodian never signs kinds 1776/1777. Because the
//!   custodian sees the body, it computes the `Digest` itself; the caller
//!   never supplies one.

use serde_json::Value;

/// The largest body the oracle signs for — the inbox's own cap
/// (`bins/fauna-nest/src/activitypub/inbox_routes.rs::MAX_BODY_BYTES`): a
/// delivery no inbox of ours would accept is no delivery to sign.
pub const MAX_BODY_BYTES: usize = 1_048_576;

/// The AS2 context IRI.
pub const AS2_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";

/// The only `@context` values a signed body may carry, alone or as an array
/// of these strings — what [`crate::types::default_context`] spells. An
/// inline object can rename a type, an unknown IRI can define anything.
pub const CONTEXT_IRIS: &[&str] = &[AS2_CONTEXT, "https://w3id.org/security/v1"];

/// The AS2 actor types. An `Update` whose object is one of these is a
/// profile rewrite — sovereign, whatever its `id` says.
const ACTOR_TYPES: &[&str] = &["Person", "Service", "Application", "Group", "Organization"];

/// The request method a signature may cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleMethod {
    /// An authorized fetch — no body.
    Get,
    /// An inbox delivery — a body.
    Post,
}

impl OracleMethod {
    /// The lowercase spelling the `(request-target)` pseudo-header carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            OracleMethod::Get => "get",
            OracleMethod::Post => "post",
        }
    }
}

/// `get` or `post`, either case; anything else signs nothing.
#[must_use]
pub fn parse_method(method: &str) -> Option<OracleMethod> {
    if method.eq_ignore_ascii_case("get") {
        Some(OracleMethod::Get)
    } else if method.eq_ignore_ascii_case("post") {
        Some(OracleMethod::Post)
    } else {
        None
    }
}

/// A request target the oracle may sign for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleTarget {
    /// The host — lowercase, the value of the covered `host` header and what
    /// the declared peer set is matched against.
    pub host: String,
    /// The `(request-target)` path: path and query
    /// ([`crate::http_signatures::request_target_path`]).
    pub path: String,
}

/// Why the oracle refuses a request. The custodian answers every one with
/// the same coarse refusal; the variant is for its log and its tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleRequestRefusal {
    /// Not an `https` URL on the default port with a host, no credentials
    /// and no fragment.
    BadTarget,
    /// A method other than `get` / `post`, a `get` with a body, or a `post`
    /// without one.
    BadMethod,
    /// A body over [`MAX_BODY_BYTES`].
    TooLarge,
    /// A body that is not an AS2 activity of this account's actor.
    NotThisActorsActivity,
    /// A body a JSON-LD processor could read differently from this
    /// predicate: a `@context` outside [`CONTEXT_IRIS`] or carrying
    /// an inline term definition, an `@`-keyword other than `@context`, a
    /// property name or a `type` that is not a plain term (a compact or
    /// absolute IRI), an array-valued `object`, or a duplicate key.
    NonCanonical,
    /// A sovereign activity shape — never delegable.
    Sovereign,
}

/// Parse the target URL.
pub fn parse_target(target: &str) -> Result<OracleTarget, OracleRequestRefusal> {
    let url = url::Url::parse(target).map_err(|_| OracleRequestRefusal::BadTarget)?;
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(OracleRequestRefusal::BadTarget);
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        // An IP literal is never a declared peer (the manifest refuses one).
        _ => return Err(OracleRequestRefusal::BadTarget),
    };
    Ok(OracleTarget {
        host,
        path: crate::http_signatures::request_target_path(&url),
    })
}

/// Check the method against the body: a `get` carries none, a `post` one of
/// at most [`MAX_BODY_BYTES`].
pub fn check_body_shape(
    method: OracleMethod,
    body: Option<&[u8]>,
) -> Result<(), OracleRequestRefusal> {
    match (method, body) {
        (OracleMethod::Get, None) => Ok(()),
        (OracleMethod::Post, Some(b)) if b.len() > MAX_BODY_BYTES => {
            Err(OracleRequestRefusal::TooLarge)
        }
        (OracleMethod::Post, Some(_)) => Ok(()),
        _ => Err(OracleRequestRefusal::BadMethod),
    }
}

/// The `id` an AS2 value names: the string itself, or an object's `id`.
fn id_of(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Object(o) => o.get("id").and_then(Value::as_str),
        _ => None,
    }
}

/// The `type` values of an AS2 object (one string, or an array of them).
fn types_of(v: &Value) -> Vec<&str> {
    match v.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// A JSON value with no object holding the same key twice. `serde_json`
/// keeps the last of two equal keys; another parser may keep the first, so a
/// body carrying both is one the predicate and a receiver read apart.
struct NoDuplicateKeys;

impl<'de> serde::Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = NoDuplicateKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with no duplicate object key")
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<NoDuplicateKeys>()?.is_some() {}
                Ok(NoDuplicateKeys)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut seen = std::collections::HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key) {
                        return Err(serde::de::Error::custom("duplicate key"));
                    }
                    map.next_value::<NoDuplicateKeys>()?;
                }
                Ok(NoDuplicateKeys)
            }
        }
        d.deserialize_any(V)
    }
}

/// A plain AS2 term: a string with no `:`, so neither a compact nor an
/// absolute IRI.
fn is_plain_term(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.contains(':'))
}

/// Is this JSON object (the activity, or its embedded `object`) in the
/// canonical shape every reader reads alike? `@context`, when present, is one
/// of [`CONTEXT_IRIS`] or an array of them; no other key starts with `@`;
/// every other key is a plain term (no `:` — a compact or absolute IRI key
/// expands to the same property as its plain term, which the deny list never
/// reads); `type`, when present, is a plain term or an array of
/// plain terms.
fn canonical_node(node: &serde_json::Map<String, Value>) -> bool {
    let context_ok = match node.get("@context") {
        None => true,
        Some(Value::String(s)) => CONTEXT_IRIS.contains(&s.as_str()),
        Some(Value::Array(a)) => a
            .iter()
            .all(|c| c.as_str().is_some_and(|s| CONTEXT_IRIS.contains(&s))),
        Some(_) => false,
    };
    let keywords_ok = node.keys().all(|k| !k.starts_with('@') || k == "@context");
    let names_ok = node.keys().all(|k| k == "@context" || !k.contains(':'));
    let type_ok = match node.get("type") {
        None => true,
        Some(Value::Array(a)) => a.iter().all(is_plain_term),
        Some(t) => is_plain_term(t),
    };
    context_ok && keywords_ok && names_ok && type_ok
}

/// The canonical-shape check, run before the deny list: the body
/// has no duplicate key, the activity and its `object` are canonical nodes,
/// and `object` is a string or one object — never an array.
fn canonical_shape(body: &[u8], activity: &serde_json::Map<String, Value>) -> bool {
    serde_json::from_slice::<NoDuplicateKeys>(body).is_ok()
        && canonical_node(activity)
        && match activity.get("object") {
            None | Some(Value::String(_)) => true,
            Some(Value::Object(o)) => canonical_node(o),
            Some(_) => false,
        }
}

/// Is the `post` body an activity the oracle signs for `actor_url`? It must
/// be a JSON object in the canonical shape
/// ([`OracleRequestRefusal::NonCanonical`]) whose `actor` is `actor_url`,
/// and none of the sovereign shapes (module doc).
pub fn activity_admitted(body: &[u8], actor_url: &str) -> Result<(), OracleRequestRefusal> {
    let activity: Value =
        serde_json::from_slice(body).map_err(|_| OracleRequestRefusal::NotThisActorsActivity)?;
    let Some(node) = activity.as_object() else {
        return Err(OracleRequestRefusal::NotThisActorsActivity);
    };
    if !canonical_shape(body, node) {
        return Err(OracleRequestRefusal::NonCanonical);
    }
    if activity.get("actor").and_then(id_of) != Some(actor_url) {
        return Err(OracleRequestRefusal::NotThisActorsActivity);
    }
    let types = types_of(&activity);
    let object = activity.get("object");
    let object_is_actor = object.and_then(id_of) == Some(actor_url);
    let object_is_an_actor_type =
        object.is_some_and(|o| types_of(o).iter().any(|t| ACTOR_TYPES.contains(t)));
    for t in types {
        let sovereign = match t {
            "Move" => true,
            "Delete" => object_is_actor,
            "Update" => object_is_actor || object_is_an_actor_type,
            _ => false,
        };
        if sovereign {
            return Err(OracleRequestRefusal::Sovereign);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACTOR: &str = "https://nest.example/ap/users/alice";

    fn act(kind: Value, object: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({ "type": kind, "actor": ACTOR, "object": object })).unwrap()
    }

    #[test]
    fn the_target_is_an_https_host_on_the_default_port() {
        assert_eq!(
            parse_target("https://Mastodon.Example/users/bob/inbox?x=1").unwrap(),
            OracleTarget {
                host: "mastodon.example".into(),
                path: "/users/bob/inbox?x=1".into()
            }
        );
        for bad in [
            "http://mastodon.example/inbox",
            "https://mastodon.example:8443/inbox",
            "https://u:p@mastodon.example/inbox",
            "https://mastodon.example/inbox#frag",
            "https://10.0.0.1/inbox",
            "https://[::1]/inbox",
            "not a url",
        ] {
            assert_eq!(
                parse_target(bad),
                Err(OracleRequestRefusal::BadTarget),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_get_carries_no_body_and_a_post_a_bounded_one() {
        assert_eq!(parse_method("POST"), Some(OracleMethod::Post));
        assert_eq!(parse_method("delete"), None);
        assert!(check_body_shape(OracleMethod::Get, None).is_ok());
        assert!(check_body_shape(OracleMethod::Get, Some(b"{}")).is_err());
        assert!(check_body_shape(OracleMethod::Post, None).is_err());
        let big = vec![b' '; MAX_BODY_BYTES + 1];
        assert_eq!(
            check_body_shape(OracleMethod::Post, Some(&big)),
            Err(OracleRequestRefusal::TooLarge)
        );
    }

    #[test]
    fn ordinary_activities_of_this_actor_are_admitted() {
        for ok in [
            act(
                json!("Create"),
                json!({ "type": "Note", "id": "https://nest.example/n/1" }),
            ),
            act(json!("Follow"), json!("https://mastodon.example/users/bob")),
            act(json!("Like"), json!("https://mastodon.example/n/2")),
            act(
                json!("Delete"),
                json!({ "id": "https://nest.example/n/1", "type": "Tombstone" }),
            ),
            act(
                json!("Update"),
                json!({ "id": "https://nest.example/n/1", "type": "Note" }),
            ),
            act(json!("Undo"), json!({ "type": "Follow", "actor": ACTOR })),
        ] {
            assert_eq!(
                activity_admitted(&ok, ACTOR),
                Ok(()),
                "{}",
                String::from_utf8_lossy(&ok)
            );
        }
    }

    /// The class's half of the deny list.
    #[test]
    fn the_sovereign_shapes_are_refused() {
        for bad in [
            act(json!("Move"), json!(ACTOR)),
            act(
                json!(["Move"]),
                json!("https://elsewhere.example/users/alice"),
            ),
            act(json!("Delete"), json!(ACTOR)),
            act(json!("Delete"), json!({ "id": ACTOR, "type": "Tombstone" })),
            act(json!("Update"), json!({ "id": ACTOR, "type": "Person" })),
            act(
                json!("Update"),
                json!({ "id": "https://nest.example/x", "type": "Service" }),
            ),
            act(json!(["Update", "Note"]), json!(ACTOR)),
        ] {
            assert_eq!(
                activity_admitted(&bad, ACTOR),
                Err(OracleRequestRefusal::Sovereign),
                "{}",
                String::from_utf8_lossy(&bad)
            );
        }
    }

    /// A JSON-LD processor reads an AS2 body as a graph, so a
    /// shape that names `Move` or an actor `Update`/`Delete` other than by
    /// the literal `type` key and a single-valued `object` is refused before
    /// the deny list runs — the five probe shapes, each admitted before the
    /// canonical-shape check.
    #[test]
    fn json_ld_aliases_of_the_sovereign_shapes_are_refused() {
        let body = |v: Value| serde_json::to_vec(&v).unwrap();
        for bad in [
            // 1. `@type` with no `type` key.
            body(json!({ "@type": "Move", "actor": ACTOR, "object": ACTOR })),
            // 2. A compact IRI.
            body(json!({ "type": "as:Move", "actor": ACTOR, "object": ACTOR })),
            // 3. A body-local term redefinition.
            body(json!({
                "@context": [AS2_CONTEXT, { "Relocate": "https://www.w3.org/ns/activitystreams#Move" }],
                "type": "Relocate", "actor": ACTOR, "object": ACTOR
            })),
            // 4. An array-valued `object` holding the actor.
            body(json!({
                "type": "Update", "actor": ACTOR,
                "object": [{ "id": ACTOR, "type": "Person", "alsoKnownAs": ["https://evil.example/u/x"] }]
            })),
            // 5. `Delete` of `[<actor>]`.
            body(json!({ "type": "Delete", "actor": ACTOR, "object": [ACTOR] })),
        ] {
            assert_eq!(
                activity_admitted(&bad, ACTOR),
                Err(OracleRequestRefusal::NonCanonical),
                "{}",
                String::from_utf8_lossy(&bad)
            );
        }
    }

    /// The rest of the canonical-shape rule: `@context` only from the fixed
    /// IRI list, no `@`-keyword but `@context` (top level and in `object`),
    /// plain-term types in `object` too, and no duplicate key anywhere (a
    /// parser that keeps the first of two `type` keys reads another type).
    #[test]
    fn non_canonical_json_ld_is_refused() {
        let body = |v: Value| serde_json::to_vec(&v).unwrap();
        let note = json!({ "type": "Note", "id": "https://nest.example/n/1" });
        for bad in [
            body(json!({ "@context": "https://evil.example/ctx", "type": "Create", "actor": ACTOR, "object": note })),
            body(json!({ "@context": { "@vocab": "https://www.w3.org/ns/activitystreams#" }, "type": "Create", "actor": ACTOR, "object": note })),
            body(json!({ "@context": [AS2_CONTEXT, 7], "type": "Create", "actor": ACTOR, "object": note })),
            body(json!({ "@id": "https://nest.example/a/1", "type": "Create", "actor": ACTOR, "object": note })),
            body(json!({ "type": "Create", "actor": ACTOR, "object": { "@type": "Person", "id": ACTOR } })),
            body(json!({ "type": "Update", "actor": ACTOR, "object": { "type": "as:Person", "id": "https://nest.example/x" } })),
            body(json!({ "type": "Update", "actor": ACTOR, "object": { "type": "https://www.w3.org/ns/activitystreams#Person", "id": "https://nest.example/x" } })),
            body(json!({ "type": ["Create", 7], "actor": ACTOR, "object": note })),
            body(json!({ "type": "Create", "actor": ACTOR, "object": { "@context": { "Note": "as:Person" }, "type": "Note", "id": ACTOR } })),
            format!(r#"{{"type":"Move","type":"Create","actor":"{ACTOR}","object":"{ACTOR}"}}"#).into_bytes(),
            format!(r#"{{"type":"Create","actor":"{ACTOR}","object":{{"id":"{ACTOR}","type":"Person","type":"Note"}}}}"#).into_bytes(),
            // A property name that is a compact or an absolute IRI expands to
            // the same property as its plain term, which the deny list never
            // reads: a Delete of the actor, through each spelling.
            body(json!({ "@context": AS2_CONTEXT, "type": "Delete", "actor": ACTOR, "as:object": ACTOR })),
            body(json!({ "@context": AS2_CONTEXT, "type": "Delete", "actor": ACTOR, "https://www.w3.org/ns/activitystreams#object": ACTOR })),
            // A plain `object` beside an `as:object` expands to a Delete of
            // `[note, actor]`.
            body(json!({ "@context": AS2_CONTEXT, "type": "Delete", "actor": ACTOR, "object": "https://nest.example/n/1", "as:object": ACTOR })),
            // An Update of the actor.
            body(json!({ "@context": AS2_CONTEXT, "type": "Update", "actor": ACTOR, "as:object": { "id": ACTOR, "type": "Person" } })),
            // The same rule inside the embedded `object`.
            body(json!({ "type": "Create", "actor": ACTOR, "object": { "type": "Note", "id": "https://nest.example/n/1", "as:attributedTo": ACTOR } })),
        ] {
            assert_eq!(
                activity_admitted(&bad, ACTOR),
                Err(OracleRequestRefusal::NonCanonical),
                "{}",
                String::from_utf8_lossy(&bad)
            );
        }
        // The nest's own shape — the two-IRI default context, on the
        // activity and on an embedded object — is canonical.
        for ok in [
            body(
                json!({ "@context": crate::types::default_context(), "type": "Create", "actor": ACTOR,
                         "object": { "@context": crate::types::default_context(), "type": "Note", "id": "https://nest.example/n/1" } }),
            ),
            body(
                json!({ "@context": AS2_CONTEXT, "type": "Follow", "actor": ACTOR, "object": "https://m.example/u/bob" }),
            ),
        ] {
            assert_eq!(
                activity_admitted(&ok, ACTOR),
                Ok(()),
                "{}",
                String::from_utf8_lossy(&ok)
            );
        }
    }

    #[test]
    fn a_body_that_is_not_this_actors_activity_is_refused() {
        let other = serde_json::to_vec(&json!({
            "type": "Create", "actor": "https://elsewhere.example/users/eve", "object": {}
        }))
        .unwrap();
        let actor_object = serde_json::to_vec(&json!({
            "type": "Like", "actor": { "id": ACTOR }, "object": "https://m.example/n/1"
        }))
        .unwrap();
        assert_eq!(activity_admitted(&actor_object, ACTOR), Ok(()));
        for bad in [
            other,
            b"not json".to_vec(),
            b"[1,2]".to_vec(),
            b"{}".to_vec(),
        ] {
            assert_eq!(
                activity_admitted(&bad, ACTOR),
                Err(OracleRequestRefusal::NotThisActorsActivity)
            );
        }
    }
}
