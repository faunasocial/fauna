"""tier_3 e2e (apple): the signed-in session runs on the ACTOR-SCOPED photo-backup store.

`account-scoping.md` § The scoping taxonomy makes the photo-backup SwiftData store a
class-4 replica that still takes account-scoped *placement*, and § Serialized switching
requires the swap to happen per account on all seven apps. Both apple shells build the
scoped container at login — `PhotoBackupRecord.buildModelContainer(actorIdHex:)` →
`AccountStateDir.photoBackupStoreURL`, i.e. `<Application Support>/Fauna/<actor>/
photo-backup.store` rather than the pre-auth flat `<…>/Fauna/photo-backup.store`.

**Building it is not using it, and that is the whole point of this file.** The e2e
login path (`applySessionPatch`, the `set_state` arm of the test-agent command handler)
*called* `buildModelContainer(actorIdHex:)` — so the scoped file appeared on disk, and
any test that checked for the file would have passed — and then assigned the result to
the App struct's `@State modelContainer`. That handler runs on a `self` the App captured
BY VALUE at `init()`, where a `@State` write is silently dropped
(`FaunaMacApp.swift`'s `liveFaunaClient` doc comment states the rule: `appState.liveClient`
is the only slot such a write cannot lose). So the container the app actually read —
the view injection, the `FaunaClient`'s `modelContext`, the agent's own state read —
stayed the pre-auth FLAT one for the whole session, which is exactly the
cross-account bleed the scoping exists to prevent, and it was invisible.

**What this asserts, and why it is a state read rather than a file check.** The app
reports the store backing the context it is really using (`data.photo_backup_store`, off
`ModelContext.container.configurations`), so the assertion is about the container in
USE, not one that merely exists. A file-existence check cannot tell the two apart — the
bug built the scoped file every time. The read is latency-independent (convention 14):
it polls the already-settled post-login state, asserting a value, never a delay.
"""
from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios]

STORE_LEAF = "photo-backup.store"


def test_the_signed_in_session_uses_the_actor_scoped_photo_backup_store(logged_in_app):
    driver = logged_in_app.driver

    state = driver.get_state(
        wait_for=lambda s: bool(s.get("session", {}).get("actor_id"))
        and s.get("data", {}).get("photo_backup_store") is not None,
        timeout=15,
    )
    session = state.get("session", {}) if state else {}
    actor = session.get("actor_id")
    assert actor, f"no signed-in actor to scope against; state.session={session!r}"

    store = (state.get("data") or {}).get("photo_backup_store")
    assert store, (
        "the app did not report which photo-backup store its context is on — "
        "`data.photo_backup_store` is what makes the container in USE observable "
        f"(a built-but-unused one is indistinguishable without it); state.data keys="
        f"{sorted((state.get('data') or {}).keys())!r}"
    )

    assert store.endswith(f"/{actor}/{STORE_LEAF}"), (
        f"the session is reading the WRONG photo-backup store: {store!r}.\n"
        f"Expected it to end with '/{actor}/{STORE_LEAF}' — the actor-scoped container "
        f"(`AccountStateDir.photoBackupStoreURL(actorIdHex:)`). Ending in the bare "
        f"'{STORE_LEAF}' means the app is on the pre-auth FLAT container: the login "
        f"built the scoped one and then lost the reference, so every account in this "
        f"process shares one unscoped store (`account-scoping.md` § Serialized switching)."
    )
