//! WebFinger response construction (RFC 7033).

use crate::types::{WebFingerLink, WebFingerResponse};
use anyhow::{Result, bail};

/// Build a WebFinger response for a local user (path mode).
pub fn build_webfinger_response(username: &str, domain: &str) -> WebFingerResponse {
    WebFingerResponse {
        subject: format!("acct:{username}@{domain}"),
        aliases: vec![format!("https://{domain}/ap/users/{username}")],
        links: vec![WebFingerLink {
            rel: "self".into(),
            r#type: Some("application/activity+json".into()),
            href: Some(format!("https://{domain}/ap/users/{username}")),
            template: None,
        }],
    }
}

/// Build a WebFinger response for the nest's **instance actor**.
///
/// Its account name is the bare domain — `acct:<domain>@<domain>` — matching
/// both `instance_actor_person`'s `preferredUsername` and Mastodon's own
/// instance-actor shape. A peer that WebFingers a fetched actor back to check
/// it agrees with its `keyId` therefore resolves, instead of 404ing on
/// `/ap/users/<domain>` (a username no account can ever hold, since a handle
/// may not contain a dot).
pub fn build_instance_webfinger_response(domain: &str) -> WebFingerResponse {
    let actor_url = crate::translate::instance_actor_url(domain);
    WebFingerResponse {
        subject: format!("acct:{domain}@{domain}"),
        aliases: vec![actor_url.clone()],
        links: vec![WebFingerLink {
            rel: "self".into(),
            r#type: Some("application/activity+json".into()),
            href: Some(actor_url),
            template: None,
        }],
    }
}

/// Parse an `acct:user@domain` URI.
/// Returns (username, domain) or an error.
pub fn parse_acct_uri(resource: &str) -> Result<(String, String)> {
    let acct = resource
        .strip_prefix("acct:")
        .ok_or_else(|| anyhow::anyhow!("resource must start with 'acct:'"))?;
    let (user, domain) = acct
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("resource must contain '@'"))?;
    if user.is_empty() || domain.is_empty() {
        bail!("user and domain must not be empty");
    }
    Ok((user.to_string(), domain.to_string()))
}

/// A Fediverse handle as people write it — `@user@host`, the two-`@` form —
/// split into `(user, host)`, the host lowercased. `None` for anything else: a
/// bare `user@host` is an email address to every app's address probe, so the
/// leading `@` is what marks the string as a Fediverse account
/// (`fauna_conversations::address::try_parse_typed_address`).
#[must_use]
pub fn parse_fediverse_handle(s: &str) -> Option<(String, String)> {
    let (user, host) = s.trim().strip_prefix('@')?.split_once('@')?;
    let user_ok = !user.is_empty()
        && !user
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '@' | '/' | ':' | '?' | '#' | '%'));
    let labels_ok = host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    (user_ok && labels_ok).then(|| (user.to_string(), host.to_ascii_lowercase()))
}

/// The WebFinger query that asks `host` which actor `acct:user@host` is
/// (RFC 7033 § 4).
#[must_use]
pub fn acct_query_url(user: &str, host: &str) -> String {
    let mut url = url::Url::parse(&format!("https://{host}/.well-known/webfinger"))
        .unwrap_or_else(|_| url::Url::parse("https://invalid.invalid/").expect("static URL"));
    url.query_pairs_mut()
        .append_pair("resource", &format!("acct:{user}@{host}"));
    url.to_string()
}

/// The actor URI a WebFinger answer names: the href of its `self` link typed
/// as an ActivityStreams document. `None` when it names none.
#[must_use]
pub fn self_actor_href(resp: &WebFingerResponse) -> Option<&str> {
    resp.links
        .iter()
        .filter(|l| l.rel == "self")
        .find(|l| {
            l.r#type.as_deref().is_some_and(|t| {
                t == "application/activity+json"
                    || (t.starts_with("application/ld+json")
                        && t.contains("https://www.w3.org/ns/activitystreams"))
            })
        })
        .and_then(|l| l.href.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_webfinger_response_path_mode() {
        let resp = build_webfinger_response("alice", "nest.fauna.social");
        assert_eq!(resp.subject, "acct:alice@nest.fauna.social");
        assert_eq!(resp.links.len(), 1);
        assert_eq!(resp.links[0].rel, "self");
        assert_eq!(
            resp.links[0].r#type.as_deref(),
            Some("application/activity+json")
        );
        assert_eq!(
            resp.links[0].href.as_deref(),
            Some("https://nest.fauna.social/ap/users/alice")
        );
    }

    /// `acct:<domain>@<domain>` resolves to the instance actor, not to a user
    /// route. A peer that WebFingers a fetched actor back to confirm it agrees
    /// with the `keyId` it was handed depends on this pointing at the same
    /// document.
    #[test]
    fn instance_webfinger_resolves_to_the_instance_actor() {
        let resp = build_instance_webfinger_response("nest.fauna.social");
        assert_eq!(resp.subject, "acct:nest.fauna.social@nest.fauna.social");
        assert_eq!(
            resp.links[0].href.as_deref(),
            Some("https://nest.fauna.social/ap/instance"),
        );
        assert_eq!(
            resp.links[0].r#type.as_deref(),
            Some("application/activity+json"),
        );
        assert_eq!(
            resp.aliases,
            vec!["https://nest.fauna.social/ap/instance".to_string()],
        );
    }

    #[test]
    fn parse_webfinger_resource_valid() {
        let (user, domain) = parse_acct_uri("acct:alice@nest.fauna.social").unwrap();
        assert_eq!(user, "alice");
        assert_eq!(domain, "nest.fauna.social");
    }

    #[test]
    fn parse_webfinger_resource_invalid() {
        assert!(parse_acct_uri("invalid").is_err());
        assert!(parse_acct_uri("acct:nodomain").is_err());
    }

    /// Only the two-`@` spelling is a Fediverse handle; a bare `user@host` is
    /// an email address, and a host must be a dotted DNS name.
    #[test]
    fn a_fediverse_handle_is_the_two_at_form() {
        assert_eq!(
            parse_fediverse_handle(" @Bob@Social.Example "),
            Some(("Bob".to_string(), "social.example".to_string()))
        );
        for not_one in [
            "bob@social.example",
            "@bob",
            "@@social.example",
            "@bob@",
            "@bob@localhost",
            "@bob@social.example/x",
            "@bob@-x.example",
            "@b ob@social.example",
            "@bob@social.example@x",
            "@bob@social.example:8443",
        ] {
            assert_eq!(parse_fediverse_handle(not_one), None, "{not_one}");
        }
    }

    #[test]
    fn the_acct_query_asks_the_host_for_the_account() {
        assert_eq!(
            acct_query_url("bob", "social.example"),
            "https://social.example/.well-known/webfinger?resource=acct%3Abob%40social.example"
        );
    }

    /// The `self` link of an ActivityStreams type names the actor; a profile
    /// page or an untyped self link does not.
    #[test]
    fn the_self_link_names_the_actor() {
        let link = |rel: &str, ty: Option<&str>, href: &str| WebFingerLink {
            rel: rel.into(),
            r#type: ty.map(Into::into),
            href: Some(href.into()),
            template: None,
        };
        let mut resp = WebFingerResponse {
            subject: "acct:bob@social.example".into(),
            aliases: vec![],
            links: vec![
                link(
                    "http://webfinger.net/rel/profile-page",
                    Some("text/html"),
                    "https://social.example/@bob",
                ),
                link("self", None, "https://social.example/untyped"),
                link(
                    "self",
                    Some(r#"application/ld+json; profile="https://www.w3.org/ns/activitystreams""#),
                    "https://social.example/users/bob",
                ),
            ],
        };
        assert_eq!(
            self_actor_href(&resp),
            Some("https://social.example/users/bob")
        );
        resp.links.truncate(2);
        assert_eq!(self_actor_href(&resp), None);
    }
}
