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
}
