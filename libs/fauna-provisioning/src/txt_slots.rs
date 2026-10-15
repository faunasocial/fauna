//! Fauna's marker-scoped `TXT` slots — the shared half of the withdraw-aware
//! converge pass (`docs/goal/behavior/dns-management.md` § Fauna-managed →
//! *Withdraw-aware convergence*).
//!
//! A slot is a `TXT` value family Fauna publishes, identified by a **marker**
//! substring every Fauna-published value carries. Withdrawal is scoped to
//! values carrying the marker, at a name Fauna already knows — never a name
//! sweep, so a third party's record at the same name survives. Two callers
//! share this: `fauna-client-dns`'s publish-time converge (desired ∪
//! remembered) and the retire machine's DNS cleanup (remembered only, an empty
//! desired set — `nest-retirement.md` § DNS cleanup — scope and order).

/// One Fauna-exclusive TXT slot the withdraw-aware converge pass can manage.
#[derive(Clone, Copy, Debug)]
pub struct TxtSlot {
    /// Log prefix identifying the pass in `fauna_dns` traces.
    pub label: &'static str,
    /// Substring every Fauna-published value at this slot carries; a published
    /// value without it is a foreign record and is never withdrawn.
    pub marker: &'static str,
}

/// Fauna's DKIM TXT slot — withdrawal scoped to published `p=` values.
pub const DKIM_SLOT: TxtSlot = TxtSlot {
    label: "DKIM converge",
    marker: "p=",
};

/// Fauna's ATProto handle-verification TXT slot — withdrawal scoped to
/// published `did=` values.
pub const ATPROTO_SLOT: TxtSlot = TxtSlot {
    label: "ATProto handle converge",
    marker: "did=",
};

/// Fauna's public-domain identity-root TXT slot — withdrawal scoped to
/// published `self=` values. The marker scoping is what makes this slot safe at
/// a name Fauna does **not** exclusively own.
pub const FAUNA_SELF_SLOT: TxtSlot = TxtSlot {
    label: "Fauna identity-root converge",
    marker: "self=",
};

/// Fauna's DMARC policy slot (`_dmarc.<domain>`) — withdrawal scoped to
/// published `v=DMARC1` values.
pub const DMARC_SLOT: TxtSlot = TxtSlot {
    label: "DMARC policy converge",
    marker: "v=DMARC1",
};

/// Fauna's MTA-STS announcement slot (`_mta-sts.<domain>`) — withdrawal scoped
/// to published `v=STSv1` values (RFC 8461 § 3.1).
pub const MTA_STS_SLOT: TxtSlot = TxtSlot {
    label: "MTA-STS policy converge",
    marker: "v=STSv1",
};

/// Fauna's TLS-RPT slot (`_smtp._tls.<domain>`) — withdrawal scoped to
/// published `v=TLSRPTv1` values (RFC 8460 § 3).
pub const TLSRPT_SLOT: TxtSlot = TxtSlot {
    label: "TLS-RPT policy converge",
    marker: "v=TLSRPTv1",
};

/// Fauna's SPF slot — the apex TXT carrying `v=spf1`; only the marker makes
/// withdrawal safe at the shared apex name.
pub const SPF_SLOT: TxtSlot = TxtSlot {
    label: "SPF policy converge",
    marker: "v=spf1",
};

/// Does a published `TXT` value belong to `slot` — i.e. is it Fauna's to
/// withdraw? The one marker test both callers apply, so a foreign value at a
/// Fauna-known name is never touched by either.
pub fn is_slot_value(slot: TxtSlot, published_value: &str) -> bool {
    published_value.contains(slot.marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_marker_carrying_values_are_the_slots() {
        assert!(is_slot_value(DKIM_SLOT, "v=DKIM1; k=rsa; p=AAAA"));
        assert!(is_slot_value(FAUNA_SELF_SLOT, "self=xyz"));
        assert!(!is_slot_value(FAUNA_SELF_SLOT, "subhandles=bob"));
        assert!(!is_slot_value(ATPROTO_SLOT, "v=spf1 -all"));
    }
}
