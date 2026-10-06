//! The unknown arms of fauna-core's signed and sealed record enums
//! (`docs/goal/architecture/transport.md` § Schema and forward-compat
//! discipline → *Rule 3 in full*; the per-enum answers are
//! `tools/check-additive-evolution/enum_ledger.txt`).
//!
//! Each test models the newer writer as a test-only twin enum with one extra
//! variant, encodes a record holding the twin's value, decodes it with the
//! real type, and asserts (1) the containing record decodes, (2) the unknown
//! value behaves as the restrictive reading its ledger line names, and (3) for
//! a carrying arm, re-encoding gives back the newer writer's bytes.

use fauna_cbor::Value;
use fauna_core::custody_grant::{CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet};
use fauna_core::data::{
    AccountLoadHint, DelegationConfig, Facet, FacetFeature, InboxMode, MailConfig, MailCredential,
    MailCredentialKind, NestEntry, NestRole, ParticipantRef, Post, PostBody, PostId, Profile,
    Reference, TaskAssignment, Timestamp,
};
use fauna_core::delegation::{
    HeavyTaskCapability, KIND_INDEX, ParticipantClass, ParticipantDescriptor, current_candidates,
    delegation_rows,
};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::crypto::{
    DecryptError, create_key_blob_entry, decrypt_key_blob_entry_for,
};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob};
use serde::{Deserialize, Serialize};

/// A value as the dag-cbor tree a record is spliced in.
fn tree<T: Serialize>(value: &T) -> Value {
    canonical_decode(&canonical_encode(value).unwrap()).unwrap()
}

/// The node at `path` (map keys, or list indices as decimal strings).
fn at<'a>(mut node: &'a mut Value, path: &[&str]) -> &'a mut Value {
    for key in path {
        node = match node {
            Value::Map(map) => map.get_mut(*key).unwrap_or_else(|| panic!("no key {key}")),
            Value::List(list) => &mut list[key.parse::<usize>().unwrap()],
            other => panic!("cannot step into {other:?} at {key}"),
        };
    }
    node
}

/// Encode a spliced tree as the newer writer's canonical bytes, decode them
/// with the real type, and pin that re-encoding gives the same bytes back.
fn decode_carrying<T: Serialize + for<'de> Deserialize<'de>>(newer: &Value) -> T {
    let bytes = canonical_encode(newer).unwrap();
    let older: T = canonical_decode(&bytes).expect("the record decodes");
    assert_eq!(
        canonical_encode(&older).unwrap(),
        bytes,
        "re-encoding a carried value gives back the newer writer's bytes"
    );
    older
}

fn post_id(n: u8) -> PostId {
    PostId::of_raw(&[n])
}

// ── Post: FacetFeature and Reference ─────────────────────────────────────

#[test]
fn a_post_with_an_unknown_facet_and_reference_decodes_and_carries_them() {
    #[derive(Serialize)]
    enum NewerFacetFeature {
        Highlight { color: String, weight: u8 },
    }
    #[derive(Serialize)]
    enum NewerReference {
        Bookmark { post_id: PostId, folder: String },
    }

    let post = Post {
        author: ActorId([1; 32]),
        created_at: Timestamp(7),
        body: PostBody::Text {
            content: "#rust and more".into(),
            facets: vec![
                Facet {
                    byte_start: 0,
                    byte_end: 5,
                    feature: FacetFeature::Tag {
                        name: "rust".into(),
                    },
                },
                Facet {
                    byte_start: 6,
                    byte_end: 9,
                    feature: FacetFeature::Tag {
                        name: "placeholder".into(),
                    },
                },
            ],
        },
        references: vec![
            Reference::Reply {
                post_id: post_id(1),
            },
            Reference::Upvote {
                post_id: post_id(2),
            },
        ],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let mut newer = tree(&post);
    *at(&mut newer, &["body", "Text", "facets", "1", "feature"]) =
        tree(&NewerFacetFeature::Highlight {
            color: "amber".into(),
            weight: 3,
        });
    *at(&mut newer, &["references", "1"]) = tree(&NewerReference::Bookmark {
        post_id: post_id(3),
        folder: "later".into(),
    });

    let older: Post = decode_carrying(&newer);
    let PostBody::Text { facets, .. } = &older.body else {
        panic!("body kind preserved");
    };
    assert!(matches!(facets[1].feature, FacetFeature::Unknown(_)));
    assert_eq!(
        older.tags(),
        vec!["rust".to_string()],
        "an unknown facet is no tag: its range renders as plain text"
    );
    assert!(matches!(older.references[1], Reference::Unknown(_)));
    assert_eq!(
        older.references[0],
        Reference::Reply {
            post_id: post_id(1)
        },
        "the known references still decode as themselves"
    );
}

// ── Profile: NestRole, AccountLoadHint, InboxMode ────────────────────────

#[test]
fn a_profile_with_an_unknown_role_hint_and_mode_decodes_and_carries_them() {
    #[derive(Serialize)]
    enum NewerNestRole {
        Relay,
    }
    #[derive(Serialize)]
    enum NewerAccountLoadHint {
        Window(u32, u32),
    }
    #[derive(Serialize)]
    enum NewerInboxMode {
        Moderated,
    }

    let profile = Profile {
        actor_id: ActorId([2; 32]),
        display_name: Some("Ada".into()),
        bio: None,
        avatar: None,
        banner: None,
        links: vec![],
        nests: vec![NestEntry {
            nest_id: vec![9; 32],
            url: "https://home.example".into(),
            roles: vec![NestRole::Social, NestRole::Mls],
        }],
        admin_nests: vec![],
        load_hint: Some(AccountLoadHint::Full),
        inbox_mode: InboxMode::Open,
        recovery_head: None,
        updated_at: Timestamp(3),
    };
    let mut newer = tree(&profile);
    *at(&mut newer, &["nests", "0", "roles", "1"]) = tree(&NewerNestRole::Relay);
    *at(&mut newer, &["load_hint"]) = tree(&NewerAccountLoadHint::Window(10, 20));
    *at(&mut newer, &["inbox_mode"]) = tree(&NewerInboxMode::Moderated);

    let older: Profile = decode_carrying(&newer);
    assert_eq!(
        older.nests[0].roles,
        vec![NestRole::Social, NestRole::Other("Relay".into())]
    );
    assert!(
        !older.nests[0].roles.contains(&NestRole::Mls),
        "an unknown role routes nothing"
    );
    assert!(matches!(older.load_hint, Some(AccountLoadHint::Unknown(_))));
    assert_eq!(older.inbox_mode, InboxMode::Other("Moderated".into()));
    assert_eq!(
        older.inbox_mode.effective(),
        InboxMode::Closed,
        "an unknown inbox mode acts as Closed"
    );
    assert_eq!(older.inbox_mode.to_wire(), None, "and has no stored token");
}

// ── MailConfig and DelegationConfig: MailCredentialKind, ParticipantRef ──

/// A newer build's mail credential and delegation pin each holding a new
/// variant decode whole on an older build, which carries the unknown arm
/// instead of refusing the record.
#[test]
fn newer_mail_and_delegation_records_with_unknown_variants_decode_whole() {
    #[derive(Serialize)]
    enum NewerMailCredentialKind {
        Passkey,
    }
    #[derive(Serialize)]
    enum NewerParticipantRef {
        Relay {
            #[serde(with = "serde_bytes")]
            relay_id: Vec<u8>,
        },
    }

    let mail = MailConfig {
        credentials: vec![MailCredential {
            credential_id: "default".into(),
            display_name: "Default".into(),
            kind: MailCredentialKind::Plain,
            secret: b"correct horse".to_vec().into(),
            created_at: 1_700_000_000,
            updated_at: Default::default(),
            wrapped_under: None,
            revoked_at_unix: None,
            burned: None,
        }],
        ..MailConfig::default()
    };
    let delegation = DelegationConfig {
        assignments: vec![TaskAssignment {
            task_kind: KIND_INDEX.into(),
            pinned_to: Some(ParticipantRef::Device {
                device_id: "aa".repeat(32),
            }),
        }],
    };
    let mut newer_mail = tree(&mail);
    *at(&mut newer_mail, &["credentials", "0", "kind"]) = tree(&NewerMailCredentialKind::Passkey);
    let mut newer_delegation = tree(&delegation);
    *at(&mut newer_delegation, &["assignments", "0", "pinned_to"]) =
        tree(&NewerParticipantRef::Relay {
            relay_id: vec![7; 16],
        });

    let older_mail: MailConfig = decode_carrying(&newer_mail);
    assert_eq!(
        older_mail.credentials[0].kind,
        MailCredentialKind::Other("Passkey".into())
    );
    let older_delegation: DelegationConfig = decode_carrying(&newer_delegation);
    let pin = older_delegation.assignments[0].pinned_to.clone();
    assert!(matches!(pin, Some(ParticipantRef::Unknown(_))));

    // An unknown pin matches no participant this build knows: the kind waits.
    let me = ParticipantRef::Device {
        device_id: "bb".repeat(32),
    };
    let participants = [ParticipantDescriptor {
        reference: me.clone(),
        class: ParticipantClass::PluggedInDesktop,
        holds_grant: false,
    }];
    assert!(
        current_candidates(KIND_INDEX, &participants, &older_delegation).is_empty(),
        "an unknown pin means nobody is eligible"
    );
    // …and the delegation surface withholds the row rather than rendering a
    // pin it cannot name.
    let rows = delegation_rows(
        &me,
        &HeavyTaskCapability::runner_for([KIND_INDEX]),
        &older_delegation,
        &[],
        u64::MAX,
    );
    assert!(rows.iter().all(|row| row.task_kind != KIND_INDEX));
}

// ── Delegation: ParticipantClass ─────────────────────────────────────────

#[test]
fn an_unknown_participant_class_is_carried_and_treated_as_battery_mobile() {
    #[derive(Serialize)]
    enum NewerParticipantClass {
        SolarRelay,
    }
    let bytes = canonical_encode(&NewerParticipantClass::SolarRelay).unwrap();
    let older: ParticipantClass = canonical_decode(&bytes).unwrap();
    assert_eq!(older, ParticipantClass::Other("SolarRelay".into()));
    assert_eq!(canonical_encode(&older).unwrap(), bytes);
    assert_eq!(older.effective(), ParticipantClass::BatteryMobile);

    let unknown = ParticipantRef::Device {
        device_id: "cc".repeat(32),
    };
    let participants = [ParticipantDescriptor {
        reference: unknown,
        class: older,
        holds_grant: true,
    }];
    assert!(
        current_candidates(KIND_INDEX, &participants, &DelegationConfig::default()).is_empty(),
        "an unknown class never runs a heavy task kind"
    );
}

// ── Custody: CustodyScopeSet ─────────────────────────────────────────────

#[test]
fn a_custody_grant_with_an_unknown_scope_set_decodes_and_carries_it() {
    #[derive(Serialize)]
    enum NewerCustodyScopeSet {
        Group { group_id: String },
    }
    let grant = CustodyGrant {
        grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
        owner: ActorId([5; 32]),
        custodian_key: [6; 32],
        scopes: CustodyScopeSet::Account,
        minted_at: Timestamp(1_000),
        expires_at: Timestamp(5_000),
        removed_devices: vec![[0xD1; 32]],
    };
    let mut newer = tree(&grant);
    *at(&mut newer, &["scopes"]) = tree(&NewerCustodyScopeSet::Group {
        group_id: "g1".into(),
    });

    let older: CustodyGrant = decode_carrying(&newer);
    assert!(matches!(older.scopes, CustodyScopeSet::Unknown(_)));
    assert_eq!(
        older.removed_devices,
        vec![[0xD1; 32]],
        "the exclusion list survives with the witness"
    );
}

// ── Subscription: KemSuiteId (collapse) ──────────────────────────────────

#[test]
fn a_key_blob_with_an_unknown_suite_entry_opens_its_other_entries() {
    let alice = ActorKeypair::from_secret([0x11; 32]);
    let bob = ActorKeypair::from_secret([0x22; 32]);
    let period_key = [0x77; 32];
    let blob = KeyBlob {
        author: ActorId([8; 32]),
        tier: "tier1".into(),
        rotated_at: Timestamp(9),
        entries: vec![
            create_key_blob_entry(&alice.actor_id(), &period_key),
            create_key_blob_entry(&bob.actor_id(), &period_key),
        ],
        signer: [8; 32],
        key_commitment: [0; 32],
    };
    let mut newer = tree(&blob);
    let Value::Map(entry) = at(&mut newer, &["entries", "1"]) else {
        panic!("an entry is a map");
    };
    entry.insert("suite".into(), Value::String("MlKem1024".into()));

    let older: KeyBlob = canonical_decode(&canonical_encode(&newer).unwrap())
        .expect("one unknown-suite entry never fails the blob");
    assert_eq!(older.entries[1].suite, KemSuiteId::Unknown);
    assert_eq!(
        decrypt_key_blob_entry_for(&alice, &older.entries[0]).unwrap(),
        period_key,
        "the other entries still open"
    );
    assert_eq!(
        decrypt_key_blob_entry_for(&bob, &older.entries[1]),
        Err(DecryptError::UnknownSuite),
        "that one entry is unopenable here"
    );
    assert!(
        canonical_encode(&older).is_err(),
        "a collapsed suite is never written back: re-encoding fails loudly"
    );
}
