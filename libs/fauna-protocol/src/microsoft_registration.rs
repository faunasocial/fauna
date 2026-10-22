//! The deployment's own Microsoft app registration for the Outlook import —
//! its wire forms and the one validation every writer runs.
//!
//! Owner: `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the
//! device-code grant, paragraph *An admin-supplied registration — bucket 2*.
//! An admin may enter their own tenant's Application (client) ID and,
//! optionally, its directory; both are public identifiers (they travel in every
//! authorisation URL), so they rest in nest state as the
//! `mail_microsoft_registration` singleton, are written by
//! `fauna.bridges.set_microsoft_registration` and reach every app on
//! `fauna.setup.status` ([`crate::discovery::SetupStatusReply::microsoft_registration`]).
//!
//! The precedence (the admin's pair over the project's constant) lives one
//! layer up, in `fauna_mail::imap_client::oauth_device::MicrosoftRegistration`,
//! beside the constant it falls back to. The validation lives HERE, below both
//! the nest and that crate, so the admin's app and the nest refuse the same
//! values with the same rule.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Value;

/// The tenant an admin who entered only a client ID gets: Microsoft's
/// `common` endpoint — work, school and personal accounts alike, which is what
/// a multitenant registration serves.
pub const DEFAULT_TENANT: &str = "common";

/// Microsoft's three named endpoints a tenant may be instead of a directory's
/// ID or verified domain.
pub const TENANT_WORDS: [&str; 3] = ["common", "organizations", "consumers"];

/// The admin-set registration as `fauna.setup.status` carries it. Present only
/// when the admin has set one; its absence is the project's registration.
/// Already validated and normalised by [`normalize`] before it was stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicrosoftRegistrationStatus {
    /// The registration's Application (client) ID — a GUID.
    pub client_id: String,
    /// The directory: a tenant ID, a verified domain, or one of
    /// [`TENANT_WORDS`]; [`DEFAULT_TENANT`] when the admin set only the ID.
    pub tenant: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.set_microsoft_registration` — the whole pair, replaced.
/// Both strings empty clears the setting back to the project's registration.
/// Admin-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetMicrosoftRegistrationRequest {
    pub client_id: String,
    pub tenant: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The reply carries only `{ ok }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetMicrosoftRegistrationReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Why an entered pair is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    /// The client ID is not a GUID.
    #[error("the application (client) ID is not a GUID")]
    ClientIdNotGuid,
    /// The tenant is neither a GUID, a domain name, nor one of the three words.
    #[error("the directory is not a tenant ID, a domain name, common, organizations or consumers")]
    TenantInvalid,
    /// A tenant was entered without a client ID — there is nothing to sign in
    /// with.
    #[error("a directory needs an application (client) ID")]
    TenantWithoutClientId,
}

/// The entered pair, validated and normalised: `Ok(None)` is the clear (both
/// empty — back to the project's registration), `Ok(Some((client_id,
/// tenant)))` the pair to store. Surrounding whitespace is dropped and both
/// are lowercased (GUIDs, domain names and the three words are all
/// case-insensitive to Microsoft), and an empty tenant is [`DEFAULT_TENANT`].
pub fn normalize(
    client_id: &str,
    tenant: &str,
) -> Result<Option<(String, String)>, RegistrationError> {
    let client_id = client_id.trim().to_ascii_lowercase();
    let tenant = tenant.trim().to_ascii_lowercase();
    match (client_id.is_empty(), tenant.is_empty()) {
        (true, true) => return Ok(None),
        (true, false) => return Err(RegistrationError::TenantWithoutClientId),
        _ => {}
    }
    if !is_guid(&client_id) {
        return Err(RegistrationError::ClientIdNotGuid);
    }
    if tenant.is_empty() {
        return Ok(Some((client_id, DEFAULT_TENANT.to_string())));
    }
    if !is_tenant(&tenant) {
        return Err(RegistrationError::TenantInvalid);
    }
    Ok(Some((client_id, tenant)))
}

/// `8-4-4-4-12` hexadecimal digits — the form Microsoft shows an Application
/// (client) ID and a Directory (tenant) ID in.
pub fn is_guid(s: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == GROUPS.len()
        && parts
            .iter()
            .zip(GROUPS)
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A tenant ID, one of [`TENANT_WORDS`], or a domain name (two or more
/// labels of letters, digits and inner hyphens, at most 63 bytes each and 253
/// in all).
pub fn is_tenant(s: &str) -> bool {
    is_guid(s) || TENANT_WORDS.iter().any(|w| w.eq_ignore_ascii_case(s)) || is_domain_name(s)
}

fn is_domain_name(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    s.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c";

    #[test]
    fn both_empty_clears_to_the_project_registration() {
        assert_eq!(normalize("", ""), Ok(None));
        assert_eq!(normalize("  ", "\t"), Ok(None));
    }

    #[test]
    fn an_id_alone_gets_the_common_tenant() {
        assert_eq!(
            normalize(ID, ""),
            Ok(Some((ID.to_string(), "common".to_string())))
        );
    }

    #[test]
    fn the_pair_is_trimmed_and_lowercased() {
        assert_eq!(
            normalize(&format!(" {} ", ID.to_uppercase()), " Contoso.Example "),
            Ok(Some((ID.to_string(), "contoso.example".to_string())))
        );
    }

    #[test]
    fn the_validation_table() {
        let tenant_ok = [
            "common",
            "organizations",
            "consumers",
            "Organizations",
            "9d6e2a1b-1c2d-4e3f-9a8b-0c1d2e3f4a5b",
            "contoso.example",
            "mail.example-org.no",
        ];
        for t in tenant_ok {
            assert!(normalize(ID, t).is_ok(), "{t:?} is a tenant");
        }
        let tenant_bad = [
            "contoso",
            "-bad.example",
            "bad-.example",
            "has space.example",
            "a..example",
            "https://login.microsoftonline.com",
            "common/oauth2",
        ];
        for t in tenant_bad {
            assert_eq!(
                normalize(ID, t),
                Err(RegistrationError::TenantInvalid),
                "{t:?} is not a tenant"
            );
        }
        let id_bad = [
            "not-a-guid",
            "00000000000000000000000000000000",
            "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2",
            "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2g",
            "{0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c}",
        ];
        for id in id_bad {
            assert_eq!(
                normalize(id, ""),
                Err(RegistrationError::ClientIdNotGuid),
                "{id:?} is not a client ID"
            );
        }
        assert_eq!(
            normalize("", "common"),
            Err(RegistrationError::TenantWithoutClientId)
        );
    }

    #[test]
    fn setup_status_form_round_trips_and_keeps_unknown_keys() {
        let status = MicrosoftRegistrationStatus {
            client_id: ID.into(),
            tenant: "common".into(),
            extra: BTreeMap::from([("future".into(), Value::Bool(true))]),
        };
        let bytes = crate::encode_canonical(&status).unwrap();
        let back: MicrosoftRegistrationStatus = crate::decode_strict(&bytes).unwrap();
        assert_eq!(back, status);
    }
}
