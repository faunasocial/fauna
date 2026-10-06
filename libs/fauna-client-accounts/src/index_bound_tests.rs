//! The account index is bounded against one credential item
//! (`long-term-store.md` § Multi-account evolution → *The index is bounded*).
//!
//! Every test here runs over [`CappedStore`], which behaves as the tightest
//! production backend does: a value over the cap is dropped without a word
//! and the row keeps what it had. A registry that only *asked* the store
//! whether its index landed would be testing the fake; these pin that the
//! registry never hands the store such a write in the first place.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

/// A store that keeps a row's previous value when handed one over
/// [`MAX_INDEX_VALUE_BYTES`] — Windows Credential Manager's behaviour behind
/// the best-effort `keyring_set`.
#[derive(Default)]
struct CappedStore {
    map: Mutex<HashMap<String, String>>,
    sets: Mutex<Vec<String>>,
}

impl CappedStore {
    fn keys(&self) -> Vec<String> {
        self.map.lock().unwrap().keys().cloned().collect()
    }
    fn set_count(&self) -> usize {
        self.sets.lock().unwrap().len()
    }
}

impl SecretStore for CappedStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map.lock().unwrap().get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) {
        self.sets.lock().unwrap().push(key.to_string());
        if value.len() > MAX_INDEX_VALUE_BYTES {
            return;
        }
        self.map.lock().unwrap().insert(key.into(), value.into());
    }
    fn delete(&self, key: &str) {
        self.map.lock().unwrap().remove(key);
    }
}

/// Fixed, distinct secrets — the capacity is a number only when the ids are
/// not minted at random (every actor id is 64 hex chars either way).
fn secret(n: usize) -> String {
    format!("{:064x}", n + 1)
}

fn actor(n: usize) -> String {
    ActorKeypair::from_secret_hex(&secret(n))
        .unwrap()
        .actor_id_hex()
}

fn capped() -> (AccountRegistry, Arc<CappedStore>) {
    let store = Arc::new(CappedStore::default());
    (AccountRegistry::new(store.clone()), store)
}

/// A signed-in account's cache, at a representative size: an 8-character
/// handle on a 16-character domain.
fn sign_in(reg: &AccountRegistry, n: usize) -> Result<(), AccountError> {
    reg.update_cache(
        &actor(n),
        Some(&format!("person{n:02}")),
        Some("nest.example.org"),
        Some("free"),
    )
}

/// Every secret slot in the store is named by the index, and the index the
/// registry reads is the one the store holds.
fn assert_no_orphan_secret(reg: &AccountRegistry, store: &CappedStore) {
    let listed: Vec<String> = reg.list().into_iter().map(|a| a.actor_id).collect();
    for key in store.keys() {
        if let Some(id) = key
            .strip_prefix("fauna/")
            .and_then(|rest| rest.strip_suffix("/secret"))
        {
            assert!(
                listed.iter().any(|a| a == id),
                "secret slot {key} is held but the index does not name it"
            );
        }
    }
}

/// Add bare accounts until the registry refuses; returns how many landed.
fn fill_bare(reg: &AccountRegistry) -> usize {
    for n in 0..100 {
        match reg.add_account(&secret(n), None, None) {
            Ok(_) => {}
            Err(AccountError::IndexFull { .. }) => return n,
            Err(e) => panic!("unexpected refusal at account {n}: {e:?}"),
        }
    }
    panic!("the registry never refused");
}

/// The measured capacity, pinned: how many accounts one credential item
/// holds. Fixed ids and a fixed cache, so these are numbers and not a
/// distribution; a change to the entry's serialized shape moves them, and
/// `long-term-store.md` quotes them.
#[test]
fn the_account_capacity_of_one_credential_item_is_measured() {
    // Bare rows — added, never signed in.
    let (reg, store) = capped();
    let bare = fill_bare(&reg);
    assert_no_orphan_secret(&reg, &store);

    // Signed-in rows — each carries its handle/domain/tier cache.
    let (reg, store) = capped();
    let mut signed_in = 0;
    for n in 0..100 {
        if reg.add_account(&secret(n), None, None).is_err() || sign_in(&reg, n).is_err() {
            break;
        }
        signed_in += 1;
    }
    assert_no_orphan_secret(&reg, &store);

    // Signed-in rows that each succeeded one predecessor whose retired row
    // this device still holds: two rows and both halves of the link apiece.
    let (reg, store) = capped();
    let mut succeeded = 0;
    for n in 0..100 {
        let (old, new) = (2 * n, 2 * n + 1);
        let landed = reg.add_account(&secret(old), None, None).is_ok()
            && sign_in(&reg, old).is_ok()
            && reg.add_account(&secret(new), None, None).is_ok()
            && reg.record_succession(&actor(old), &actor(new)).is_ok()
            && sign_in(&reg, new).is_ok();
        if !landed {
            break;
        }
        succeeded += 1;
    }
    assert_no_orphan_secret(&reg, &store);

    eprintln!("capacity: bare={bare} signed_in={signed_in} succeeded={succeeded}");
    assert_eq!(
        (bare, signed_in, succeeded),
        (12, 11, 4),
        "bare / signed-in / signed-in-with-a-held-predecessor"
    );
}

/// The row's invariant: an add that would not land writes NOTHING — above
/// all not the secret slot, which with the index unchanged would be an
/// identity this device holds and cannot reach.
#[test]
fn an_add_past_the_cap_is_refused_and_writes_nothing() {
    let (reg, store) = capped();
    let landed = fill_bare(&reg);
    // `fill_bare` already met one refusal; meet it again, watched.
    let index_before = store.get(INDEX_KEY).expect("an index was written");
    let sets_before = store.set_count();

    let err = reg
        .add_account(&secret(landed), Some("https://a.example"), Some("dev-1"))
        .expect_err("an add the index cannot hold must not read as added");

    assert!(
        matches!(err, AccountError::IndexFull { needed, limit }
            if limit == MAX_INDEX_VALUE_BYTES && needed > limit),
        "got {err:?}"
    );
    assert_eq!(
        store.set_count(),
        sets_before,
        "a refused add writes nothing"
    );
    assert_eq!(store.get(&secret_key(&actor(landed))), None);
    assert_eq!(store.get(&nest_url_key(&actor(landed))), None);
    assert_eq!(store.get(INDEX_KEY).as_deref(), Some(index_before.as_str()));
    assert_eq!(reg.list().len(), landed);
    assert_no_orphan_secret(&reg, &store);
}

/// The bound sits on the index write itself, so every mutator that grows the
/// blob meets it — not only the add.
#[test]
fn every_growing_mutator_is_refused_at_the_cap() {
    let (reg, store) = capped();
    let landed = fill_bare(&reg);
    assert!(landed >= 2);
    let before = store.get(INDEX_KEY).unwrap();
    let wide = "h".repeat(MAX_INDEX_VALUE_BYTES);

    assert!(matches!(
        reg.update_cache(&actor(0), Some(&wide), None, None),
        Err(AccountError::IndexFull { .. })
    ));
    let wide_chain: Vec<String> = (200..240).map(actor).collect();
    assert!(matches!(
        reg.record_predecessors(&actor(0), &wide_chain),
        Err(AccountError::IndexFull { .. })
    ));
    // Fill the last few bytes the bare rows left, so a single link is too much.
    let room = MAX_INDEX_VALUE_BYTES - before.len();
    reg.update_cache(
        &actor(0),
        Some(&"h".repeat(room.saturating_sub(2))),
        None,
        None,
    )
    .expect("growth up to the cap lands");
    let full = store.get(INDEX_KEY).unwrap();
    assert!(full.len() <= MAX_INDEX_VALUE_BYTES);
    assert!(matches!(
        reg.record_succession(&actor(0), &actor(1)),
        Err(AccountError::IndexFull { .. })
    ));

    assert_eq!(
        store.get(INDEX_KEY).as_deref(),
        Some(full.as_str()),
        "a refused mutation leaves the index byte-for-byte as it was"
    );
}

/// A full index is not a stuck one: everything that does not grow it still
/// lands, and removing an account makes room for the next.
#[test]
fn a_full_index_still_switches_and_removes() {
    let (reg, store) = capped();
    let landed = fill_bare(&reg);

    reg.set_active(&actor(landed - 1))
        .expect("switching rewrites the index at the same size");
    assert_eq!(reg.active(), Some(actor(landed - 1)));
    reg.set_require_confirm(&actor(0), false)
        .expect("a flag flip that does not grow the index lands");

    reg.remove(&actor(0)).expect("removing always lands");
    reg.add_account(&secret(landed), None, None)
        .expect("the freed room takes the next account");
    assert_eq!(reg.list().len(), landed);
    assert_no_orphan_secret(&reg, &store);
}

/// An index already past the cap — written by a build that predates the
/// bound, on a store roomy enough to have taken it — is not bricked by the
/// bound: it may shrink or hold its size, and only growth is refused.
#[test]
fn an_index_already_past_the_cap_may_shrink_but_never_grow() {
    let store = Arc::new(InMemorySecretStore::new());
    let idx = AccountIndex {
        active: Some(actor(0)),
        accounts: (0..30).map(|n| AccountEntry::new(actor(n))).collect(),
        ..AccountIndex::default()
    };
    let raw = serde_json::to_string(&idx).unwrap();
    assert!(raw.len() > MAX_INDEX_VALUE_BYTES);
    store.seed(INDEX_KEY, &raw);
    for n in 0..30 {
        store.seed(&secret_key(&actor(n)), &secret(n));
    }
    let reg = AccountRegistry::new(store.clone());

    assert!(matches!(
        reg.add_account(&secret(30), None, None),
        Err(AccountError::IndexFull { .. })
    ));
    assert_eq!(store.get(&secret_key(&actor(30))), None);

    reg.set_active(&actor(7))
        .expect("a same-size rewrite lands");
    reg.remove(&actor(3)).expect("a shrinking rewrite lands");
    assert_eq!(reg.list().len(), 29);
    assert_eq!(reg.active(), Some(actor(7)));
}

/// The add every seat runs at sign-in (`persist_logged_in`) on a full list:
/// refused with the line the seats paint, and the account the user was on
/// stays active.
#[test]
fn a_sign_in_on_a_full_list_paints_the_list_full_line_and_keeps_the_active_account() {
    let (reg, _store) = capped();
    let landed = fill_bare(&reg);
    reg.set_active(&actor(0)).expect("a bare account activates");

    let err = persist_logged_in(&reg, &secret(landed), "https://a.example", None, None)
        .expect_err("a sign-in the index cannot hold must not read as added");

    let line = add_refused_copy(&err)
        .expect("a full list is a refusal the user can act on")
        .resolve(fauna_i18n::strings::lookup);
    assert!(
        line.contains("full") && !line.starts_with("settings."),
        "a seat must paint shipped copy, not a key: {line}"
    );
    assert_eq!(reg.active(), Some(actor(0)));
    assert_eq!(reg.list().len(), landed);
}
