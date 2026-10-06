"""The macOS DEFAULT launch is isolated from the real user profile.

Regression gate for the unconditional ``CFFIXED_USER_HOME`` flip
(`drivers/macos.py::launch`): every launch — not just one with a pinned
``home``/``seed_app_support`` — must relocate the whole CoreFoundation home
(Application Support with the MLS db and the SwiftData store, UserDefaults,
Caches) into the per-launch throwaway home.

Why this is load-bearing: ``HOME`` alone moves nothing CF-resolved, so a
default-path launch that skips ``CFFIXED_USER_HOME`` puts every run's state in
the REAL profile's shared ``Fauna/`` base — cross-test contamination, and
account-blind erasure (sign-out sweeps every actor dir, by design: erasure
follows scope) lets one run destroy a concurrent run's MLS state. Isolation
must come from the launch, never from account identity
(``docs/goal/architecture/apps/account-scoping.md`` § Testing interplay).
"""

from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.macos]

# The real profile's install-scoped base — what the flip keeps e2e out of.
_REAL_FAUNA_BASE = Path.home() / "Library" / "Application Support" / "Fauna"


def test_default_launch_keeps_app_state_out_of_the_real_profile(logged_in_app, test_user):
    app = logged_in_app
    aps = app.driver.app_support_dir()
    assert aps, (
        "app_support_dir() is None: the default launch did not relocate the "
        "store — CFFIXED_USER_HOME is not being set unconditionally"
    )
    aps = Path(aps)
    assert Path.home() / "Library" not in aps.parents, (
        f"isolated Application Support resolves under the real profile: {aps}"
    )

    # App-side proof (not just the driver's own bookkeeping) that the store
    # resolved into the isolated home. Two independent witnesses, both written
    # by the app at launch/login:
    #  * the account registry's cross-process mutation lock, created when the
    #    app constructs its registry over AccountStateDir.base;
    #  * the SwiftData PhotoBackupRecord store, whose per-actor
    #    ModelConfiguration (AccountStateDir.photoBackupStoreURL) resolves through the same CoreFoundation home — the
    #    surface that used to land in the real profile even when HOME was
    #    relocated. (Before that change this was an install-wide
    #    `<Application Support>/default.store`; that location is now
    #    permanently abandoned — CachedAccount/Snapshot, its other tenants,
    #    were deleted as dead code in the same change.)
    isolated_base = aps / "Fauna"
    assert isolated_base.is_dir(), (
        f"the app never materialized {isolated_base} — its state went "
        "somewhere else (the real profile?)"
    )
    assert (isolated_base / "account-registry.lock").exists(), (
        "the app's registry lock is not under the isolated base — "
        "AccountStateDir.base resolved elsewhere"
    )
    actor_hex = test_user["actor_id_hex"]
    assert (isolated_base / actor_hex / "photo-backup.store").exists(), (
        "the per-actor SwiftData store is not under the isolated Application "
        "Support — AccountStateDir.photoBackupStoreURL resolved elsewhere"
    )

    # And THIS login's state is absent from the real profile. Keyed by actor
    # id so an unrelated concurrent run on the machine cannot flake it.
    assert not (_REAL_FAUNA_BASE / actor_hex).exists(), (
        f"actor dir leaked into the real profile: {_REAL_FAUNA_BASE / actor_hex}"
    )
