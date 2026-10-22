//! The TLS server name the nest's own TLS layer accepted a connection under —
//! the SNI — carried from the accept loop into the handler that signs the
//! channel binding. One connection, one served name, read nowhere else.
//!
//! Why it exists: a nest serves different certs to different dials (the
//! per-SNI rule of `nest/tls-certificates.md` § A, and § B-IP's bridge cert,
//! which every **no-SNI** dial is served for up to 160 h after a provision), so
//! "the SPKI of the cert the nest serves" is only well-defined per connection
//! (`transport-trust.md` § Two independent axes → Axis 1, *the cert it served
//! this connection*). The accept loop (`lib.rs`'s `serve_admitted`) reads the
//! name off the accepted `rustls::ServerConnection` and its `WithConnectInfo`
//! middleware inserts it as the [`ServedName`] request extension beside
//! `ConnectInfo`; the three WS upgrades that build a dispatch sink
//! (`routes::ws_handler`, `routes::ws_anonymous_handler`,
//! `federation_channel::federation_ws_handler`) extract it with
//! [`OptionalServedName`] and keep it on the connection, and the dispatcher
//! scopes it into `dispatch_core::CallerRef` so a binding producer asks
//! `dispatch_core::current_served_name` and reads
//! `acme::ServedCertSpki::spki_for_served_name`.
//!
//! `None` on a plain-HTTP listener (no TLS, no name — the dev/e2e nest, the
//! in-container bridge hop) and on a no-SNI dial (an IP dial: RFC 6066 forbids
//! a literal address in SNI); every producer then keeps the no-SNI read, the
//! bridge while it is installed, else the default. The name is the nest's own
//! TLS state, never a wire field — a client cannot name a cert it was not
//! served, which is what keeps Axis 1's "never a client-reported value" true.
//!
//! A `rustls` server name is ASCII, lowercase and already validated as a DNS
//! name by the TLS layer, so it is safe to key `cert_for_sni` on and to log.

use std::sync::Arc;

/// The request extension the accept loop inserts: the accepted server name,
/// `None` when the connection carried none (plain HTTP or a no-SNI dial).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedName(pub Option<Arc<str>>);

/// Extractor twin of [`crate::registration::OptionalConnectInfo`]: the served
/// name when the accept loop recorded one, else `None` (a router driven without
/// the accept loop in a test, a plain listener, a no-SNI dial).
pub struct OptionalServedName(pub Option<Arc<str>>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for OptionalServedName {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let name = parts
            .extensions
            .get::<ServedName>()
            .and_then(|s| s.0.clone());
        Ok(Self(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::FromRequestParts;

    fn parts() -> axum::http::request::Parts {
        axum::http::Request::new(()).into_parts().0
    }

    #[tokio::test]
    async fn extracts_the_name_the_accept_loop_recorded() {
        let mut parts = parts();
        parts
            .extensions
            .insert(ServedName(Some(Arc::from("nest.example"))));
        let OptionalServedName(name) = OptionalServedName::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(name.as_deref(), Some("nest.example"));
    }

    #[tokio::test]
    async fn none_without_the_extension_and_none_for_a_no_sni_dial() {
        let mut parts = parts();
        let OptionalServedName(name) = OptionalServedName::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(name, None, "a router driven without the accept loop");

        parts.extensions.insert(ServedName(None));
        let OptionalServedName(name) = OptionalServedName::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(name, None, "a no-SNI dial records an empty name");
    }
}
