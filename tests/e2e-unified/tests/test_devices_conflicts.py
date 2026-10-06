"""Conflict REVIEW LIST on the Folders page (auto-resolve) — cross-app e2e.

file-sync.md § Conflicts (ratified 2026-07-10): conflicts auto-resolve on the
detecting device and the per-set surface is a REVIEW LIST, not a blocking
chooser. The page renders resolved conflicts off the shared `DevicesMachine`
snapshot (fetched with `include_resolved`): the resolution badge
(`conflict-type-badge`), the file + winning head (`conflict-file-info`), and a
one-tap "use the other version" (`conflict-resolve-button`) that re-points the
file at the retained losing version via `DevicesMachine::use_other_version` —
an ordinary restore record (`fauna.sync.changes.record`), itself reversible.
A still-unresolved row (the degraded report the daemon sends when the candidate upload failed) renders
informationally with NO button — resolution happens on the detecting device.

tier_3: a real nest binary records the pre-resolved conflict (the slice-3a
transactional propagate: loser retained + winner head in the report
transaction) and serves the review list; the re-point round-trip is verified
against the nest's change log. Conflicts are seeded the way the resolving
daemon reports them — `fauna.sync.conflicts.report` carrying the resolution —
rather than driving a real two-device divergence (that runs in
tests/platform/sync/test_text_merge.py).
"""

import os
import secrets
import time

import pytest

import fauna_ffi

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _report_conflict(
    nest_url,
    signing_key,
    folder,
    path,
    candidates,
    *,
    resolution=None,
    winning=None,
    reporter_device=None,
):
    """Seed a conflict the way the daemon reports it — unresolved (the degraded
    report: no resolution) or PRE-RESOLVED (the auto-resolve report: resolution + winner;
    the nest transactionally retains the loser and propagates the winner as
    change rows).

    Sent through ``fauna_ffi.harness_report_conflict``: a pre-resolved report
    mints a winner head row, which since writer-signed change records the nest
    refuses unless it is signed — so the shared ``RecordSigning::sign_report``
    signs it under the nonce the owner's custody holds for the set (the app's
    folder wizard created it custody-first).

    ``reporter_device`` is the reporting device's id. A resolved report retains
    only the reporter's OWN losing candidate, signed by the reporter as a
    version (writer-signed change records, ruling (10)(d)) — so the candidate a
    test expects the review list to re-point to must carry this id, as a real
    reporter's losing version does."""
    payload = {
        "folder": folder,
        "device_id": reporter_device or os.urandom(32).hex(),
        "path": path,
        # Post-S9-flip a report on a sealed plane is refused
        # `fauna.sync.path_seal_required`, and the review row renders its path
        # sealed-first — so seed the REAL envelope (owner root, via the shared
        # `label_custody::seal_path` funnel), not a synthetic one: an
        # unopenable seal degrades to `Omit`, which drops the row rather than
        # blanking it (`file-sync.md` § Sealed names & paths).
        "path_sealed": fauna_ffi.seal_path(signing_key, path),
        "conflict_type": "content",
        "details": None,
        "candidates": candidates,
    }
    if resolution is not None:
        payload["resolution"] = resolution
        payload["winning_manifest_hash"] = winning["manifest_hash"]
        payload["winning_size_bytes"] = winning["size_bytes"]
    fauna_ffi.harness_report_conflict(nest_url, signing_key, payload)


def _candidate(size_bytes, created_at=0):
    return {
        "manifest_hash": os.urandom(32).hex(),
        "device_id": os.urandom(32).hex(),
        "size_bytes": size_bytes,
        "created_at": created_at,
    }


class TestConflictReviewList:
    @pytest.fixture(autouse=True)
    def setup(self, logged_in_app, nest_instance, test_user):
        self.app = logged_in_app
        self.nest_url = nest_instance["url"]
        # The handle's own port: a live box's URL carries none to parse.
        self.port = nest_instance["port"]
        self.signing_key = bytes(test_user["signing_key"])
        self.secret_hex = bytes(test_user["signing_key"]).hex()

    def _refresh_folders(self):
        """Force a Folders-page refresh: the page reloads its snapshot whenever
        it becomes visible, so navigate away and back."""
        self.app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(0.3)
        self.app.backups.navigate_folders()

    def _make_set(self):
        name = f"conf-set-{secrets.token_hex(4)}"
        self.app.backups.navigate_folders()
        self.app.backups.create_folder_via_wizard(name)
        return name

    @pytest.mark.feature("folders")
    def test_resolved_conflict_renders_review_row_and_repoints(self):
        name = self._make_set()

        # Seed a PRE-RESOLVED latest-wins conflict: cand_b won, cand_a is the
        # retained loser the one-tap re-points to — the reporter's own losing
        # version, which its report signs and the nest retains as a version.
        # The re-point below lands only because the file's verified version
        # history lists it (the review list signs nothing the nest alone names).
        cand_a = _candidate(100, created_at=10)
        cand_b = _candidate(200, created_at=20)
        _report_conflict(
            self.nest_url,
            self.signing_key,
            name,
            "/docs/report.txt",
            [cand_a, cand_b],
            resolution="latest_wins",
            winning=cand_b,
            reporter_device=cand_a["device_id"],
        )

        # The review row renders — resolution badge + file info + exactly ONE
        # re-point button (never a per-candidate chooser).
        self._refresh_folders()
        self.app.driver.wait_for("conflict-type-badge", timeout=15)
        assert self.app.driver.is_visible("conflict-file-info"), (
            "review row should render the file-info block: "
            f"{self.app.driver.diagnose('conflict-file-info')} "
            f"error={self.app.error_text()!r}"
        )
        # ONE re-point per resolved review row, never one per candidate. Counted
        # against the nest's resolved rows rather than a literal 1: `test_user`
        # is session-scoped, so on a multi-app run the page also lists the
        # rows an earlier app's leg of this test resolved.
        from common import sync_conflicts_list

        resolved = [
            c
            for c in sync_conflicts_list(
                self.port,
                secret_key=self.secret_hex,
                include_resolved=True,
            )["conflicts"]
            if c.get("resolution")
        ]
        deadline = time.time() + 15
        buttons = self.app.driver.count("conflict-resolve-button")
        while buttons != len(resolved) and time.time() < deadline:
            time.sleep(0.5)
            buttons = self.app.driver.count("conflict-resolve-button")
        assert buttons == len(resolved), (
            "the review list carries ONE 'use the other version' re-point per "
            f"resolved row, not a per-candidate chooser: {buttons} button(s) for "
            f"{len(resolved)} resolved row(s)"
        )

        # One-tap re-point: the retained loser (cand_a) becomes the new head
        # via an ordinary restore record on the nest change log.
        from common import sync_changes_list

        self.app.driver.click("conflict-resolve-button", index=0)
        deadline = time.time() + 15
        head = None
        while time.time() < deadline:
            rows = sync_changes_list(
                self.port, secret_key=self.secret_hex, folder=name, since=0
            )["changes"]
            # The re-point's change row rests `path: None` (S9 flip), so match
            # its convergent `path_sealed`. Both spellings, because the leading
            # slash the conflict was reported under may or may not survive the
            # machine's normalization — whichever it sealed, one of these is it.
            wanted = {
                fauna_ffi.seal_path(self.signing_key, p)
                for p in ("/docs/report.txt", "docs/report.txt")
            }
            path_rows = [
                c for c in rows if bytes(c.get("path_sealed") or b"") in wanted
            ]
            head = path_rows[-1] if path_rows else None
            if head and head.get("manifest_hash") == cand_a["manifest_hash"]:
                break
            time.sleep(0.5)
        assert head is not None and head.get("manifest_hash") == cand_a["manifest_hash"], (
            "the re-point must record the losing candidate as the new head "
            f"(an ordinary modify): head={head!r} "
            f"error={self.app.error_text()!r}"
        )
        # The review row remains (resolved history, itself re-reviewable) —
        # nothing was destroyed.
        assert self.app.driver.count("conflict-type-badge") >= 1

    @pytest.mark.feature("folders")
    def test_unresolved_conflict_row_is_informational(self):
        name = self._make_set()

        # Baseline BEFORE seeding: earlier tests' resolved rows (and their
        # re-point buttons) legitimately persist on the shared nest — the new
        # unresolved row must add a badge but NO button.
        self._refresh_folders()
        time.sleep(1)
        badges_before = self.app.driver.count("conflict-type-badge")
        buttons_before = self.app.driver.count("conflict-resolve-button")

        # The daemon's degraded report on a failed candidate upload: unresolved (no resolution fields).
        _report_conflict(
            self.nest_url,
            self.signing_key,
            name,
            "/docs/legacy.txt",
            [_candidate(100), _candidate(200)],
        )

        self._refresh_folders()
        deadline = time.time() + 15
        while time.time() < deadline:
            if self.app.driver.count("conflict-type-badge") > badges_before:
                break
            time.sleep(0.5)
        assert self.app.driver.count("conflict-type-badge") > badges_before, (
            "the unresolved row should render a badge (its conflict type): "
            f"error={self.app.error_text()!r}"
        )
        # Informational only: no re-point button, no per-candidate chooser —
        # resolution happens on the detecting device.
        assert self.app.driver.count("conflict-resolve-button") == buttons_before, (
            "an unresolved row must not offer any resolve affordance "
            f"(the chooser is retired): error={self.app.error_text()!r}"
        )
