//! `fauna.admin.web_app_origin.*` — what this nest's own `/app/` answers: the
//! SPA it bundles, or a redirect to the central origin with this nest
//! pre-filled.
//!
//! Owner doc: `docs/goal/behavior/web-content-hosting.md` § Same-origin
//! security model → *The nest-served `/app/` and the central origin*; the trust
//! claim the redirect serves is `docs/goal/architecture/front-door.md` § Which
//! origin a user loads the app from.
//!
//! **Where logic lives.** The mode, the serving decision and the redirect
//! address are derived HERE, once: the nest's `/app` router serves from
//! [`WebAppOriginServing::resolve`] and [`central_redirect_location`], and the
//! same derivation is what the nest projects onto `fauna.admin.web_app_origin.get`
//! and `fauna.setup.status`, so an app's status text shows the exact address a
//! user is sent to without re-deriving it. Nothing here is specific to `/app`:
//! the share-link viewer (`GET /share/<token>` on a navigation request —
//! `share-links.md` § The private-file extension) follows the same choice by
//! the same rule and calls the same two functions with its own path.
//!
//! **On the wire.** `setup.status` carries the mode as a required field and
//! the redirect target as an optional one (`None` while `/app/` serves the
//! bundled SPA; `discovery.rs`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Value;

/// The central origin the association runs — the one place the web app can be
/// loaded from without trusting any nest's operator (`front-door.md`). Also the
/// nest's built-in CORS origin (`node_policy_core::DEFAULT_CORS_ORIGIN` is this
/// constant), so no second knob or literal names it.
pub const CENTRAL_APP_ORIGIN: &str = "https://app.fauna.social";

/// The query parameter the central app reads as a pre-fill of the handle-entry
/// page's domain part (`onboarding.md` § 2 Handle entry → *Nest hint*).
pub const NEST_HINT_PARAM: &str = "nest";

/// The admin's choice of what this nest's `/app/` answers. Absent (never set)
/// is [`WebAppOrigin::Bundled`] — the works-out-of-the-box default: a fresh
/// nest cannot know whether the central origin is reachable from its users.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebAppOrigin {
    /// Serve the SPA this nest ships.
    #[default]
    Bundled,
    /// Answer a `302` to [`CENTRAL_APP_ORIGIN`] with this nest pre-filled.
    Central,
}

impl WebAppOrigin {
    /// The wire / at-rest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            WebAppOrigin::Bundled => "bundled",
            WebAppOrigin::Central => "central",
        }
    }

    /// Parse the wire / at-rest spelling; `None` for anything else (a newer
    /// peer's mode, or a corrupt row).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bundled" => Some(WebAppOrigin::Bundled),
            "central" => Some(WebAppOrigin::Central),
            _ => None,
        }
    }
}

/// What a reserved navigation path (`/app`, and later the share viewer)
/// actually answers — the chosen mode folded with whether this nest has a
/// handle domain to pre-fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAppOriginServing {
    /// The bundled SPA, because that is the choice.
    Bundled,
    /// The bundled SPA although the choice is central: a domainless box has
    /// nothing to pre-fill, and a redirect that cannot name the nest would hand
    /// the user to the central origin with no way back here.
    BundledDomainless,
    /// A `302` to the central origin with `nest=<domain>`.
    Central {
        /// This nest's handle domain — the `nest=` value.
        nest_domain: String,
    },
}

impl WebAppOriginServing {
    /// Fold the chosen mode with this nest's handle domain — the caller passes
    /// the domain only when one is SET (never a `localhost` placeholder).
    pub fn resolve(mode: WebAppOrigin, handle_domain: Option<&str>) -> Self {
        match (mode, handle_domain.filter(|d| !d.is_empty())) {
            (WebAppOrigin::Bundled, _) => WebAppOriginServing::Bundled,
            (WebAppOrigin::Central, None) => WebAppOriginServing::BundledDomainless,
            (WebAppOrigin::Central, Some(d)) => WebAppOriginServing::Central {
                nest_domain: d.to_string(),
            },
        }
    }

    /// The redirect address for `path_and_query`, or `None` when the path is
    /// served bundled.
    pub fn redirect_for(&self, path_and_query: &str) -> Option<String> {
        match self {
            WebAppOriginServing::Central { nest_domain } => {
                Some(central_redirect_location(path_and_query, nest_domain))
            }
            _ => None,
        }
    }
}

/// The central-origin address a reserved navigation path redirects to: the
/// central origin + the same path and query + `nest=<nest_domain>`.
///
/// A `nest` pair already in the query is dropped first — the nest's own domain
/// is the only pre-fill it vouches for, and two `nest=` values would leave the
/// central app to guess. The path is kept verbatim (it came off a request line,
/// so it is already a valid URI path); one that does not start with `/` gets
/// one, so the result can never re-point the authority.
pub fn central_redirect_location(path_and_query: &str, nest_domain: &str) -> String {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let mut out = String::with_capacity(
        CENTRAL_APP_ORIGIN.len() + path_and_query.len() + nest_domain.len() + 8,
    );
    out.push_str(CENTRAL_APP_ORIGIN);
    if !path.starts_with('/') {
        out.push('/');
    }
    out.push_str(path);
    out.push('?');
    if let Some(q) = query {
        for pair in q.split('&').filter(|p| !p.is_empty()) {
            let key = pair.split_once('=').map_or(pair, |(k, _)| k);
            if key == NEST_HINT_PARAM {
                continue;
            }
            out.push_str(pair);
            out.push('&');
        }
    }
    out.push_str(NEST_HINT_PARAM);
    out.push('=');
    percent_encode_query_value(nest_domain, &mut out);
    out
}

/// RFC 3986 query-value encoding: unreserved characters pass, everything else
/// is `%XX`. A handle domain is plain DNS text, so this is belt and braces.
fn percent_encode_query_value(value: &str, out: &mut String) {
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

/// `fauna.admin.web_app_origin.get` — no parameters; the answer is about this
/// deployment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminWebAppOriginGetRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The chosen mode and what `/app/` answers because of it. The same three
/// facts ride `fauna.setup.status` as its `web_app_origin*` fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminWebAppOriginGetReply {
    /// The chosen mode's wire spelling ([`WebAppOrigin::as_str`]) — a string,
    /// not the enum, so a newer nest's mode reaches an older app as an
    /// unrecognised value it renders read-only rather than a decode failure.
    pub mode: String,
    /// The exact address a user who opens this nest's `/app/` is sent to;
    /// absent when `/app/` serves the bundled SPA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_target: Option<String>,
    /// The choice is central but this nest has no handle domain, so it serves
    /// bundled regardless.
    #[serde(default)]
    pub domainless: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl AdminWebAppOriginGetReply {
    /// The projection of `mode` as served by a nest whose handle domain is
    /// `handle_domain` — the one builder the nest's reply and its
    /// `setup.status` fields share.
    pub fn project(mode: WebAppOrigin, handle_domain: Option<&str>) -> Self {
        let serving = WebAppOriginServing::resolve(mode, handle_domain);
        AdminWebAppOriginGetReply {
            mode: mode.as_str().to_string(),
            redirect_target: serving.redirect_for("/app/"),
            domainless: serving == WebAppOriginServing::BundledDomainless,
            extra: Default::default(),
        }
    }
}

/// `fauna.admin.web_app_origin.set` — a whole-value replace of the choice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminWebAppOriginSetRequest {
    pub mode: WebAppOrigin,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.web_app_origin.set` — what `/app/` answers now, so
/// the save needs no second read to render its status.
pub type AdminWebAppOriginSetReply = AdminWebAppOriginGetReply;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    #[test]
    fn the_redirect_keeps_path_and_query_and_appends_the_nest() {
        assert_eq!(
            central_redirect_location("/app/", "example.org"),
            "https://app.fauna.social/app/?nest=example.org"
        );
        assert_eq!(
            central_redirect_location("/app/feed/x?tab=2&q=a%20b", "example.org"),
            "https://app.fauna.social/app/feed/x?tab=2&q=a%20b&nest=example.org"
        );
        assert_eq!(
            central_redirect_location("/app", "example.org"),
            "https://app.fauna.social/app?nest=example.org"
        );
    }

    #[test]
    fn an_incoming_nest_pair_never_survives_beside_ours() {
        assert_eq!(
            central_redirect_location("/app/?nest=evil.example&x=1&nest", "example.org"),
            "https://app.fauna.social/app/?x=1&nest=example.org"
        );
        // A key merely starting with `nest` is someone else's parameter.
        assert_eq!(
            central_redirect_location("/app/?nestling=1", "example.org"),
            "https://app.fauna.social/app/?nestling=1&nest=example.org"
        );
    }

    #[test]
    fn a_path_cannot_re_point_the_authority() {
        let loc = central_redirect_location("@evil.example/app", "example.org");
        assert!(loc.starts_with("https://app.fauna.social/@evil.example/app?"));
        let loc = central_redirect_location("//evil.example/app", "example.org");
        assert!(loc.starts_with("https://app.fauna.social//evil.example/app?"));
    }

    #[test]
    fn the_nest_value_is_query_encoded() {
        assert_eq!(
            central_redirect_location("/app/", "a b&c"),
            "https://app.fauna.social/app/?nest=a%20b%26c"
        );
    }

    #[test]
    fn a_domainless_box_serves_bundled_whatever_the_choice() {
        assert_eq!(
            WebAppOriginServing::resolve(WebAppOrigin::Central, None),
            WebAppOriginServing::BundledDomainless
        );
        assert_eq!(
            WebAppOriginServing::resolve(WebAppOrigin::Central, Some("")),
            WebAppOriginServing::BundledDomainless
        );
        assert_eq!(
            WebAppOriginServing::resolve(WebAppOrigin::Bundled, Some("example.org")),
            WebAppOriginServing::Bundled
        );
        assert_eq!(
            WebAppOriginServing::resolve(WebAppOrigin::Central, Some("example.org"))
                .redirect_for("/app/x"),
            Some("https://app.fauna.social/app/x?nest=example.org".to_string())
        );
        assert_eq!(
            WebAppOriginServing::BundledDomainless.redirect_for("/app/"),
            None
        );
    }

    #[test]
    fn the_projection_names_mode_target_and_the_domainless_reason() {
        let p = AdminWebAppOriginGetReply::project(WebAppOrigin::Central, Some("example.org"));
        assert_eq!(p.mode, "central");
        assert_eq!(
            p.redirect_target.as_deref(),
            Some("https://app.fauna.social/app/?nest=example.org")
        );
        assert!(!p.domainless);

        let p = AdminWebAppOriginGetReply::project(WebAppOrigin::Central, None);
        assert_eq!(
            (p.mode.as_str(), p.redirect_target, p.domainless),
            ("central", None, true)
        );

        let p = AdminWebAppOriginGetReply::project(WebAppOrigin::Bundled, Some("example.org"));
        assert_eq!(
            (p.mode.as_str(), p.redirect_target, p.domainless),
            ("bundled", None, false)
        );
    }

    #[test]
    fn the_mode_spelling_round_trips_and_absent_is_bundled() {
        for m in [WebAppOrigin::Bundled, WebAppOrigin::Central] {
            assert_eq!(WebAppOrigin::parse(m.as_str()), Some(m));
            let req = AdminWebAppOriginSetRequest {
                mode: m,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).expect("encode");
            assert_eq!(
                decode_strict::<AdminWebAppOriginSetRequest>(&bytes).expect("decode"),
                req
            );
        }
        assert_eq!(WebAppOrigin::parse("elsewhere"), None);
        assert_eq!(WebAppOrigin::default(), WebAppOrigin::Bundled);
    }

    #[test]
    fn an_unknown_mode_is_refused_at_decode() {
        #[derive(Serialize)]
        struct Loose {
            mode: String,
        }
        let bytes = encode_canonical(&Loose {
            mode: "elsewhere".into(),
        })
        .expect("encode");
        assert!(decode_strict::<AdminWebAppOriginSetRequest>(&bytes).is_err());
    }
}
