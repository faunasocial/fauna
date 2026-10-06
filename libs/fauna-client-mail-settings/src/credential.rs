//! In-memory credential type + credential-id derivation.
//!
//! The on-disk shape is `fauna_core::data::MailCredential`; this
//! module owns the in-flight Zeroizing-wrapped form the state machine
//! constructs from `MailSettingsAction` and the dispatch path
//! consumes.

use crate::state::CredentialKind;
use fauna_mls::wrapped_blob::CredentialInput;
use zeroize::Zeroizing;

/// In-memory credential. `Plain` and `OAuthBearer` carry the same
/// byte shape but route through different KDFs at wrap-time per
/// `docs/goal/behavior/mail-credentials.md` § KDF choice.
#[derive(Debug, Clone)]
pub enum Credential {
    Plain(Zeroizing<Vec<u8>>),
    OAuthBearer(Zeroizing<Vec<u8>>),
}

impl Credential {
    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::Plain(_) => CredentialKind::Plain,
            Self::OAuthBearer(_) => CredentialKind::OAuthBearer,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Plain(b) | Self::OAuthBearer(b) => b,
        }
    }

    /// Adapter into the form `fauna_mls::wrapped_blob::seal_*`
    /// accepts. Lifetime of the returned `CredentialInput` is bound
    /// by `self`.
    pub fn as_input(&self) -> CredentialInput<'_> {
        match self {
            Self::Plain(b) => CredentialInput::Plain(b),
            Self::OAuthBearer(b) => CredentialInput::OauthBearer(b),
        }
    }

    /// Build a `Credential` from a kind + raw secret bytes — used by
    /// the rotation path, which reads
    /// `MailConfig::credentials[].secret` and re-wraps under the new
    /// MSEK without prompting the user.
    pub fn from_kind_bytes(kind: CredentialKind, secret: Vec<u8>) -> Self {
        let z = Zeroizing::new(secret);
        match kind {
            CredentialKind::Plain => Self::Plain(z),
            CredentialKind::OAuthBearer => Self::OAuthBearer(z),
        }
    }

    /// The wrap-side credential for a persisted row's kind + secret, or
    /// `None` when the row's kind is one a newer build wrote and this one
    /// does not name — such a credential is never wrapped under here
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*: an unknown value never grants).
    pub fn from_stored(
        kind: &fauna_core::data::MailCredentialKind,
        secret: Vec<u8>,
    ) -> Option<Self> {
        CredentialKind::of_stored(kind).map(|k| Self::from_kind_bytes(k, secret))
    }

    /// [`Self::from_stored`], refusing a kind this build does not name.
    pub(crate) fn of_row(
        row: &fauna_core::data::MailCredential,
    ) -> Result<Self, crate::error::DispatchError> {
        Self::from_stored(&row.kind, row.secret.to_vec()).ok_or_else(|| {
            crate::error::DispatchError::InvalidState(format!(
                "credential {} is of a kind this version of the app does not know; \
                 update the app to use it",
                row.credential_id
            ))
        })
    }
}

/// Kebab-case `credential_id` derivation — **re-export**; the implementation
/// and its tests live in `fauna_client_bridges::credential_id`, shared with the
/// ATProto app-credential machine (lifted 2026-07-22, priority #4). Mail's own
/// semantics are unchanged, including the `mail-credentials.md` rule that the
/// literal id `"default"` renders without a suffix in a MUA username (RFC 5233
/// sub-addressing) — that is a render-time concern the shared function
/// deliberately does not encode.
pub use fauna_client_bridges::credential_id::derive_credential_id;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_input_round_trip() {
        let c = Credential::Plain(Zeroizing::new(b"hunter2".to_vec()));
        match c.as_input() {
            CredentialInput::Plain(b) => assert_eq!(b, b"hunter2"),
            _ => panic!("wrong variant"),
        }
        let c = Credential::OAuthBearer(Zeroizing::new(b"tok".to_vec()));
        match c.as_input() {
            CredentialInput::OauthBearer(b) => assert_eq!(b, b"tok"),
            _ => panic!("wrong variant"),
        }
    }
}
