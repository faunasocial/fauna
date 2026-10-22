//! The mail import's **remembered source** — the old provider's server, port,
//! connection security and username the wizard keeps between visits
//! (`mailbox-migration.md` § Credential handling, the "Source server hostname
//! / port / TLS mode" row: client persistent storage, until the user re-uses
//! or deletes the import setting). Never the password: that lives in client
//! memory only, for one connection.
//!
//! The value rests as the account's `fauna.state.mail-import-source` row
//! (`fauna_protocol::merge_policy::KIND_MAIL_IMPORT_SOURCE`, one row at
//! [`MAIL_IMPORT_SOURCE_ROW_KEY`], whole-record latest-wins); the door is
//! `fauna_account_plane::mail_import_source_rows`. Forgetting it is a
//! tombstone on that row.
//!
//! The provider and the TLS mode are the wizard's own labels as strings
//! (`fauna_client_mail_settings::import` maps them to its enums), so this
//! crate needs nothing from the wizard; a label this build does not know
//! reads as nothing remembered, never as a guess.

use serde::{Deserialize, Serialize};

/// The one logical key a `fauna.state.mail-import-source` row lives at.
pub const MAIL_IMPORT_SOURCE_ROW_KEY: &str = "self";

/// The longest host a record remembers — a DNS name's own ceiling.
pub const MAX_REMEMBERED_HOST_BYTES: usize = crate::web::MAX_HOSTNAME_BYTES;

/// The longest username a record remembers — far past any real address
/// (RFC 5321 caps a mailbox at 254), and what bounds the row's size.
pub const MAX_REMEMBERED_USERNAME_BYTES: usize = 1024;

/// What the import wizard remembers about the source it last connected to.
/// Tolerant decode (`#[serde(default)]`, unknown fields ignored): no merge arm
/// reads the value — the plane orders two writes by the entry's stamp alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RememberedImportSource {
    /// The provider, as the session's `source_descriptor` names it —
    /// `gmail`, `outlook`, `icloud` or `imap` (the Generic source).
    pub provider: String,
    /// The IMAP server's hostname.
    pub host: String,
    /// The IMAP server's port.
    pub port: u16,
    /// The connection security — `implicit` or `starttls`.
    pub tls_mode: String,
    /// The account's username on the old provider (usually its address).
    pub username: String,
}

impl RememberedImportSource {
    /// Whether the record is within the bounds the row is sized for — the
    /// door writes nothing that is not (the protocol crate's size pin).
    pub fn is_bounded(&self) -> bool {
        self.host.len() <= MAX_REMEMBERED_HOST_BYTES
            && self.username.len() <= MAX_REMEMBERED_USERNAME_BYTES
            && self.provider.len() <= 16
            && self.tls_mode.len() <= 16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field a newer writer added is decoded past, and a missing one
    /// defaults: the row is whole-record latest-wins, so this reader must
    /// never refuse a sibling's write.
    #[test]
    fn the_remembered_source_decodes_tolerantly() {
        let known = RememberedImportSource {
            provider: "imap".into(),
            host: "mail.example.org".into(),
            port: 143,
            tls_mode: "starttls".into(),
            username: "someone@example.org".into(),
        };
        let bytes = crate::encoding::canonical_encode(&known).unwrap();
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&bytes).unwrap() {
            fauna_cbor::Value::Map(m) => m,
            other => panic!("the remembered source encodes as a map, got {other:?}"),
        };
        map.insert("a_newer_field".into(), fauna_cbor::Value::Integer(7));
        map.remove("port");
        let newer = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();
        let read: RememberedImportSource = crate::encoding::canonical_decode(&newer).unwrap();
        assert_eq!(read, RememberedImportSource { port: 0, ..known });
    }
}
