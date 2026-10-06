//! The `fauna.state.succession-ledger` size pins (`config-dissolution.md` §
//! Phases and gates → *Bounded rows*): every row family, at the widest a
//! production writer produces it, seals under HALF the plane's per-entry cap
//! — measured with the writer door's own `sealed_envelope_len`, generation-
//! sealed as the kind is, and printed so a regression shows its bytes.
//!
//! The one row that grows — `chain` — is pinned at 512 successions: it grows
//! by 34 B per succession *ceremony*, an identity-lifetime event under a
//! recovery key, never a use-time count.

use fauna_client_capabilities::custody_grants::custody_event_scopes;
use fauna_client_capabilities::grant_log::{bounded_mail_labeler_event_scope, build_mint_event};
use fauna_core::account_entry_crypto::{EntryPlaintext, sealed_envelope_len};
use fauna_core::custody_grant::CustodyScopeSet;
use fauna_core::data::{
    FilterUnattestedMark, GrantUnattestedMark, MemberUnattestedItem, MemberUnattestedReason,
    UnattestedVerdict,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::succession_ledger::{ChainState, SuccessionLedgerRecord};
use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
use fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER;

/// The sealed length of `record`'s row, asserted under half the cap.
fn assert_row_fits(what: &str, record: &SuccessionLedgerRecord) -> usize {
    let key = record.key().expect("key").render();
    let plaintext = EntryPlaintext {
        kind: KIND_SUCCESSION_LEDGER.to_string(),
        key: key.clone(),
        merge_meta: None,
        value: record.encode().expect("encode").into(),
        tombstone: false,
    };
    let sealed = sealed_envelope_len(&plaintext, true).expect("measure");
    eprintln!(
        "size pin: the {what} row seals to {sealed} B (half cap {} B)",
        MAX_STATE_ENTRY_BYTES / 2
    );
    assert!(
        sealed <= MAX_STATE_ENTRY_BYTES / 2,
        "the {what} row {key} seals to {sealed} B, over half the {MAX_STATE_ENTRY_BYTES} B cap"
    );
    sealed
}

/// A few hundred bytes of a newer build's verdict — the widest a mark
/// carries (`UnattestedVerdict::Other` is preserved verbatim).
fn wide_verdict() -> UnattestedVerdict {
    UnattestedVerdict::Other("v".repeat(300))
}

#[test]
fn succession_ledger_rows_seal_under_half_the_entry_cap() {
    // The chain after 512 successions.
    let chain = ChainState {
        actor_id: ActorId([0xFF; 32]),
        prior_actor_ids: (0u16..512)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..2].copy_from_slice(&i.to_be_bytes());
                ActorId(id)
            })
            .collect(),
    };
    let chain_len = assert_row_fits(
        "512-succession chain",
        &SuccessionLedgerRecord::Chain(chain),
    );

    // The widest event a mint produces: a per-labeler bounded mail grant's
    // two factor-folded tuples plus a scope-subset custody grant's tuples.
    let owner = ActorKeypair::from_secret([0x11; 32]);
    let mut scope = bounded_mail_labeler_event_scope(&ActorId([0x22; 32]));
    scope.extend(custody_event_scopes(&CustodyScopeSet::Scopes(
        (0..16)
            .map(|i| format!("content:{}", format!("{i:02x}").repeat(32)))
            .collect(),
    )));
    let event = build_mint_event(
        [0x33; 16],
        [0x44; 32],
        scope,
        1_700_000_000,
        1_800_000_000,
        1_700_000_000,
    )
    .sign(owner.signing_key())
    .expect("sign");
    assert_row_fits("widest event", &SuccessionLedgerRecord::Event(event));

    let predecessor = ActorId([0x55; 32]);
    assert_row_fits(
        "grant-mark",
        &SuccessionLedgerRecord::GrantMark(GrantUnattestedMark {
            grant_id: vec![0x33; 16],
            predecessor,
            verdict: wide_verdict(),
        }),
    );
    assert_row_fits(
        "member-item",
        &SuccessionLedgerRecord::MemberItem(MemberUnattestedItem {
            person: ActorId([0x66; 32]),
            predecessor,
            reason: MemberUnattestedReason::Other("r".repeat(300)),
            verdict: wide_verdict(),
        }),
    );
    assert_row_fits(
        "filter-mark",
        &SuccessionLedgerRecord::FilterMark(FilterUnattestedMark {
            filter_id: i64::MIN,
            predecessor,
            verdict: wide_verdict(),
        }),
    );

    // The chain grows by one 32-byte id (+ its 2-byte CBOR header) per
    // succession — the 34 B the ruling names.
    let chain_513 = ChainState {
        actor_id: ActorId([0xFF; 32]),
        prior_actor_ids: (0u16..513)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..2].copy_from_slice(&i.to_be_bytes());
                ActorId(id)
            })
            .collect(),
    };
    let grown = SuccessionLedgerRecord::Chain(chain_513)
        .encode()
        .unwrap()
        .len()
        - SuccessionLedgerRecord::Chain(ChainState {
            actor_id: ActorId([0xFF; 32]),
            prior_actor_ids: (0u16..512)
                .map(|i| {
                    let mut id = [0u8; 32];
                    id[..2].copy_from_slice(&i.to_be_bytes());
                    ActorId(id)
                })
                .collect(),
        })
        .encode()
        .unwrap()
        .len();
    assert_eq!(grown, 34, "one succession adds one 34-byte id to the chain");
    assert!(
        chain_len > 512 * 34,
        "the 512-link chain is really measured"
    );
}
