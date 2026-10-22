//! Which of the user's aliases a received message arrived on — the client-side
//! read behind the "Delivered to: `<alias>` [`<label>`]" tag
//! (`docs/goal/behavior/mail-aliases.md` § Label).
//!
//! Pure over the opened message and the user's own alias rows
//! (`fauna.bridges.list_account_aliases`), so every app resolves it the same
//! way with no new nest read. The address comes from the trusted
//! [`HEADER_DELIVERED_TO`] delivery stamp — the envelope `RCPT TO` — and the
//! alias kind from the `X-Fauna-Address-*` stamp the resolver chose beside it.
//! Both sit in the `X-Fauna-` namespace every filing door strips from inbound
//! mail before stamping, so neither can be forged by a sender. `To:`,
//! `Delivered-To:` and `Cc:` are deliberately NOT consulted: the sender writes
//! them, and they name the wrong address for Bcc'd and list mail.

use serde::{Deserialize, Serialize};

use super::{
    ALIAS_KIND_DISPOSABLE, ALIAS_KIND_EXACT, ALIAS_KIND_WILDCARD_PREFIX, HEADER_ADDRESS_SUFFIX,
    HEADER_CATCHALL, HEADER_CATCHALL_DOMAIN, HEADER_DELIVERED_TO, HEADER_DISPOSABLE,
    HEADER_WILDCARD_SUFFIX, SUBADDRESS_SEPARATOR, first_header_value,
};

/// The fields of one of the user's alias rows the resolution reads — copied out
/// of `fauna_protocol::bridge_routing::AliasRow`, which this crate's `aliases`
/// feature does not depend on. Owned, so the UniFFI and WASM boundaries can
/// carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "delivered-uniffi", derive(uniffi::Record))]
pub struct AliasLabelRow {
    /// `account_aliases.kind` ([`ALIAS_KIND_EXACT`], …).
    pub kind: String,
    pub local_domain: String,
    /// Exact: the local-part. Wildcard: the prefix incl. its trailing `-`.
    /// Disposable: the base32 token.
    pub pattern: String,
    pub label: String,
}

/// How the message reached the user — the resolver step that matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "delivered-uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeliveredAliasKind {
    Exact,
    /// `+suffix` sub-addressing on one of the user's exact aliases.
    Subaddress,
    WildcardPrefix,
    Disposable,
    /// The domain's catch-all; `domain` is the one the catch-all fired on.
    Catchall {
        domain: String,
    },
}

/// The resolved "Delivered to" tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "delivered-uniffi", derive(uniffi::Record))]
pub struct DeliveredAlias {
    /// The lower-cased address the message was delivered to.
    pub address: String,
    pub kind: DeliveredAliasKind,
    /// The matched row's `pattern`; `None` for catch-all (no row) and for an
    /// alias row deleted since delivery.
    pub pattern: Option<String>,
    /// The matched row's label; empty when unlabelled or unmatched — the UI
    /// then falls back to the alias kind + pattern (§ Label).
    pub label: String,
}

/// Resolve the alias `raw_message` was delivered on against the user's `rows`.
///
/// `None` when the message carries no [`HEADER_DELIVERED_TO`] stamp (never
/// went through the resolver — the user's own sent copy, an import), and for
/// an unstamped delivery no exact row names: a role-address delivery
/// (`postmaster@` routed to the admin owns no alias) or an exact alias deleted
/// since. List and forwarder addresses never deliver a local copy, so they
/// never reach here. A stamped kind whose row has since been deleted still
/// resolves, with no pattern and an empty label: the stamp is the record.
pub fn delivered_alias(raw_message: &[u8], rows: &[AliasLabelRow]) -> Option<DeliveredAlias> {
    let address = first_header_value(raw_message, HEADER_DELIVERED_TO)?.to_ascii_lowercase();
    let (local, domain) = address.rsplit_once('@')?;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    let on_domain = |r: &&AliasLabelRow| r.local_domain.eq_ignore_ascii_case(domain);
    let row_of_kind = |kind: &str, pattern: &str| {
        rows.iter()
            .filter(on_domain)
            .find(|r| r.kind == kind && r.pattern.eq_ignore_ascii_case(pattern))
    };

    let (kind, row) = if first_header_value(raw_message, HEADER_ADDRESS_SUFFIX).is_some() {
        let base = local
            .split_once(SUBADDRESS_SEPARATOR)
            .map_or(local, |(b, _)| b);
        (
            DeliveredAliasKind::Subaddress,
            row_of_kind(ALIAS_KIND_EXACT, base),
        )
    } else if let Some(token) = first_header_value(raw_message, HEADER_DISPOSABLE) {
        (
            DeliveredAliasKind::Disposable,
            row_of_kind(ALIAS_KIND_DISPOSABLE, token),
        )
    } else if let Some(suffix) = first_header_value(raw_message, HEADER_WILDCARD_SUFFIX) {
        let suffix = suffix.to_ascii_lowercase();
        let row = rows
            .iter()
            .filter(on_domain)
            .filter(|r| r.kind == ALIAS_KIND_WILDCARD_PREFIX)
            .filter(|r| {
                let prefix = r.pattern.to_ascii_lowercase();
                local.strip_prefix(prefix.as_str()) == Some(suffix.as_str())
            })
            .max_by_key(|r| r.pattern.len());
        (DeliveredAliasKind::WildcardPrefix, row)
    } else if first_header_value(raw_message, HEADER_CATCHALL).is_some() {
        let caught = first_header_value(raw_message, HEADER_CATCHALL_DOMAIN)
            .filter(|d| !d.is_empty())
            .map_or_else(|| domain.to_string(), str::to_ascii_lowercase);
        (DeliveredAliasKind::Catchall { domain: caught }, None)
    } else {
        // No alias-kind stamp: an exact match, or the role-address route.
        (
            DeliveredAliasKind::Exact,
            Some(row_of_kind(ALIAS_KIND_EXACT, local)?),
        )
    };

    Some(DeliveredAlias {
        address: address.clone(),
        kind,
        pattern: row.map(|r| r.pattern.clone()),
        label: row.map_or_else(String::new, |r| r.label.clone()),
    })
}

/// [`delivered_alias`] taking its arguments by value — the form the UniFFI
/// boundary exports.
#[cfg_attr(feature = "delivered-uniffi", uniffi::export)]
pub fn delivered_alias_of(
    raw_message: Vec<u8>,
    rows: Vec<AliasLabelRow>,
) -> Option<DeliveredAlias> {
    delivered_alias(&raw_message, &rows)
}

/// The stamp headers [`delivered_alias`] reads, in the order they appear.
const STAMP_HEADERS: [&str; 6] = [
    HEADER_DELIVERED_TO,
    HEADER_ADDRESS_SUFFIX,
    HEADER_DISPOSABLE,
    HEADER_WILDCARD_SUFFIX,
    HEADER_CATCHALL,
    HEADER_CATCHALL_DOMAIN,
];

/// The delivery stamps of `raw_message` as a header section of their own —
/// everything [`delivered_alias`] reads, so a client that keeps only this
/// (not the whole message) still resolves the tag the same way once its alias
/// rows arrive. Fields keep their order (the resolver takes the first match).
/// `None` when the message carries no [`HEADER_DELIVERED_TO`] stamp: the
/// resolver would answer `None` for it whatever the rows.
pub fn delivery_stamps(raw_message: &[u8]) -> Option<Vec<u8>> {
    use crate::header_walk::{find_body_offset, parse_header};
    first_header_value(raw_message, HEADER_DELIVERED_TO)?;
    let body_offset = find_body_offset(raw_message).unwrap_or(raw_message.len());
    let header_section = &raw_message[..body_offset];
    let mut out = Vec::new();
    let mut i = 0;
    while i < header_section.len() {
        let (header_end, field_name) = parse_header(&header_section[i..]);
        if header_end == 0 {
            break;
        }
        let name = field_name.trim_ascii();
        if STAMP_HEADERS
            .iter()
            .any(|h| name.eq_ignore_ascii_case(h.as_bytes()))
        {
            let field = &header_section[i..i + header_end];
            out.extend_from_slice(field);
            if !field.ends_with(b"\n") {
                out.extend_from_slice(b"\r\n");
            }
        }
        i += header_end;
    }
    out.extend_from_slice(b"\r\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, local_domain: &str, pattern: &str, label: &str) -> AliasLabelRow {
        AliasLabelRow {
            kind: kind.into(),
            local_domain: local_domain.into(),
            pattern: pattern.into(),
            label: label.into(),
        }
    }

    fn rows() -> Vec<AliasLabelRow> {
        vec![
            row(ALIAS_KIND_EXACT, "example.com", "bob", "Primary"),
            row(ALIAS_KIND_EXACT, "example.com", "bob.smith", ""),
            row(ALIAS_KIND_WILDCARD_PREFIX, "example.com", "bob-", "Shops"),
            row(
                ALIAS_KIND_DISPOSABLE,
                "example.com",
                "a2b3c4",
                "Amazon - Kindle order receipts",
            ),
            // Same pattern on another domain must not cross-match.
            row(ALIAS_KIND_EXACT, "other.example", "bob", "Other domain"),
        ]
    }

    fn msg(stamps: &[(&str, &str)]) -> Vec<u8> {
        let mut m = String::new();
        for (n, v) in stamps {
            m.push_str(&format!("{n}: {v}\r\n"));
        }
        m.push_str(
            "From: a@sender.example\r\nTo: someone@else.example\r\nSubject: hi\r\n\r\nbody\r\n",
        );
        m.into_bytes()
    }

    #[test]
    fn exact_resolves_from_the_envelope_stamp_alone() {
        let got =
            delivered_alias(&msg(&[(HEADER_DELIVERED_TO, "Bob@Example.com")]), &rows()).unwrap();
        assert_eq!(
            got,
            DeliveredAlias {
                address: "bob@example.com".into(),
                kind: DeliveredAliasKind::Exact,
                pattern: Some("bob".into()),
                label: "Primary".into(),
            }
        );
    }

    #[test]
    fn exact_without_a_label_resolves_with_an_empty_label() {
        let got = delivered_alias(
            &msg(&[(HEADER_DELIVERED_TO, "bob.smith@example.com")]),
            &rows(),
        )
        .unwrap();
        assert_eq!(got.kind, DeliveredAliasKind::Exact);
        assert_eq!(got.pattern.as_deref(), Some("bob.smith"));
        assert_eq!(got.label, "");
    }

    #[test]
    fn exact_matches_only_on_its_own_domain() {
        let got =
            delivered_alias(&msg(&[(HEADER_DELIVERED_TO, "bob@other.example")]), &rows()).unwrap();
        assert_eq!(got.label, "Other domain");
    }

    #[test]
    fn subaddress_labels_with_its_base_exact_alias() {
        let got = delivered_alias(
            &msg(&[
                (HEADER_ADDRESS_SUFFIX, "receipts"),
                (HEADER_DELIVERED_TO, "bob+receipts@example.com"),
            ]),
            &rows(),
        )
        .unwrap();
        assert_eq!(
            got,
            DeliveredAlias {
                address: "bob+receipts@example.com".into(),
                kind: DeliveredAliasKind::Subaddress,
                pattern: Some("bob".into()),
                label: "Primary".into(),
            }
        );
    }

    #[test]
    fn wildcard_labels_with_the_prefix_row() {
        let got = delivered_alias(
            &msg(&[
                (HEADER_WILDCARD_SUFFIX, "amazon"),
                (HEADER_DELIVERED_TO, "bob-amazon@example.com"),
            ]),
            &rows(),
        )
        .unwrap();
        assert_eq!(got.kind, DeliveredAliasKind::WildcardPrefix);
        assert_eq!(got.pattern.as_deref(), Some("bob-"));
        assert_eq!(got.label, "Shops");
    }

    #[test]
    fn disposable_labels_with_the_token_row() {
        let got = delivered_alias(
            &msg(&[
                (HEADER_DISPOSABLE, "a2b3c4"),
                (HEADER_DELIVERED_TO, "bob-temp-A2B3C4@example.com"),
            ]),
            &rows(),
        )
        .unwrap();
        assert_eq!(got.address, "bob-temp-a2b3c4@example.com");
        assert_eq!(got.kind, DeliveredAliasKind::Disposable);
        assert_eq!(got.pattern.as_deref(), Some("a2b3c4"));
        assert_eq!(got.label, "Amazon - Kindle order receipts");
    }

    #[test]
    fn catchall_carries_the_stamped_domain_and_no_row() {
        let got = delivered_alias(
            &msg(&[
                (HEADER_CATCHALL, "true"),
                (HEADER_CATCHALL_DOMAIN, "Example.com"),
                (HEADER_DELIVERED_TO, "anything@example.com"),
            ]),
            &rows(),
        )
        .unwrap();
        assert_eq!(
            got,
            DeliveredAlias {
                address: "anything@example.com".into(),
                kind: DeliveredAliasKind::Catchall {
                    domain: "example.com".into()
                },
                pattern: None,
                label: String::new(),
            }
        );
    }

    #[test]
    fn a_stamped_kind_whose_row_was_deleted_still_resolves_unlabelled() {
        let got = delivered_alias(
            &msg(&[
                (HEADER_DISPOSABLE, "zzzzzz"),
                (HEADER_DELIVERED_TO, "bob-temp-zzzzzz@example.com"),
            ]),
            &rows(),
        )
        .unwrap();
        assert_eq!(got.kind, DeliveredAliasKind::Disposable);
        assert_eq!(got.pattern, None);
        assert_eq!(got.label, "");
    }

    #[test]
    fn role_address_delivery_resolves_none() {
        // The role-address route stamps no alias kind and owns no row.
        assert_eq!(
            delivered_alias(
                &msg(&[(HEADER_DELIVERED_TO, "postmaster@example.com")]),
                &rows()
            ),
            None
        );
    }

    #[test]
    fn unstamped_message_resolves_none_even_when_to_names_an_alias() {
        // `To:`/`Delivered-To:` are sender-written: never consulted.
        let raw = b"Delivered-To: bob@example.com\r\nTo: bob@example.com\r\n\r\nbody\r\n";
        assert_eq!(delivered_alias(raw, &rows()), None);
    }

    #[test]
    fn a_stamp_in_the_body_is_not_a_stamp() {
        let raw = b"To: bob@example.com\r\n\r\nX-Fauna-Delivered-To: bob@example.com\r\n";
        assert_eq!(delivered_alias(raw, &rows()), None);
    }

    #[test]
    fn the_kept_stamps_resolve_exactly_as_the_whole_message() {
        let cases = [
            msg(&[(HEADER_DELIVERED_TO, "Bob@Example.com")]),
            msg(&[
                (HEADER_ADDRESS_SUFFIX, "receipts"),
                (HEADER_DELIVERED_TO, "bob+receipts@example.com"),
            ]),
            msg(&[
                (HEADER_WILDCARD_SUFFIX, "amazon"),
                (HEADER_DELIVERED_TO, "bob-amazon@example.com"),
            ]),
            msg(&[
                (HEADER_DISPOSABLE, "a2b3c4"),
                (HEADER_DELIVERED_TO, "bob-temp-A2B3C4@example.com"),
            ]),
            msg(&[
                (HEADER_CATCHALL, "true"),
                (HEADER_CATCHALL_DOMAIN, "Example.com"),
                (HEADER_DELIVERED_TO, "anything@example.com"),
            ]),
        ];
        for raw in cases {
            let kept = delivery_stamps(&raw).expect("stamped");
            assert!(
                !String::from_utf8_lossy(&kept).contains("Subject"),
                "only the stamps are kept: {kept:?}"
            );
            assert_eq!(
                delivered_alias(&kept, &rows()),
                delivered_alias(&raw, &rows())
            );
        }
    }

    #[test]
    fn an_unstamped_message_keeps_no_stamps() {
        let raw = b"To: bob@example.com\r\n\r\nX-Fauna-Delivered-To: bob@example.com\r\n";
        assert_eq!(delivery_stamps(raw), None);
    }

    #[test]
    fn malformed_envelope_stamp_resolves_none() {
        for bad in ["", "bob", "@example.com", "bob@"] {
            assert_eq!(
                delivered_alias(&msg(&[(HEADER_DELIVERED_TO, bad)]), &rows()),
                None,
                "{bad:?}"
            );
        }
    }
}
