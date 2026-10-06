//! The historical chunk-plane pair audit and its device re-seal are **gone**.
//!
//! Until 2026-09-24 the GC walk folded every sealed manifest looking for one
//! plaintext chunk resting under two ciphertexts of one (key, nonce) — the
//! pre-2026-09-03 WebDAV MDA's raw seal beside the sync engine's framed seal —
//! and handed the owner's device the store keys over `fauna.sync.chunk_pairs.list`
//! so it could re-record the file. The raw writer was retired on 2026-09-03, and
//! under the 2026-09-24 baseline reset no chunk it wrote rests anywhere, so the
//! audit, the kind, the device pass and the dashboard counts left together
//! (`version-compatibility.md` § Dimension 2, the fourth ratified exception;
//! `mls-group-key-material.md` § Per-chunk file-sync key keeps the history).
//!
//! This pins the refusal: a peer that still sends the kind meets the router's
//! `unknown_kind`, and no caller class is permitted it.

use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::rpc_router::RpcRouter;

const RETIRED_KIND: &str = "fauna.sync.chunk_pairs.list";

#[test]
fn the_chunk_pairs_list_kind_does_not_dispatch() {
    let mut b = RpcRouter::builder();
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();
    assert!(
        router.kind_meta("fauna.sync.changes.list").is_some(),
        "non-vacuity: the sync handlers really are registered on this router"
    );
    assert!(
        router.kind_meta(RETIRED_KIND).is_none(),
        "the retired kind must not dispatch — a raw twin is no longer scanned for or re-sealed"
    );
}

#[test]
fn no_caller_class_is_permitted_the_retired_kind() {
    for class in [CallerClass::User, CallerClass::Admin] {
        assert!(
            !is_permitted(class, RETIRED_KIND),
            "{class:?} must not be permitted {RETIRED_KIND}"
        );
    }
}
