"""tier_3: the admin's headless reports pass resolves a report on a local nest.

``org-nests.md`` § fauna.build → *Disposable* (ruled 2026-11-01) admits one
client to the acceptance nest during an open store review without spending its
one complex connection: a headless, shared-Rust-only client that lists the open
reports and resolves them, and nothing else (``bins/fauna-reports-pass``; duty 1 of ``installers/store-review.md`` § During a
review). This drives the SAME binary against a locally-built nest: a user
reports another account, the pass lists the report under the admin's key read
from stdin, resolves it, and the reporter's own ledger reads the outcome.

The guard half (admitted only inside an open review of the served version) is
tier_1 in ``tests/test_staging_box_plan.py``; this file proves the client.
"""

from __future__ import annotations

import subprocess

import pytest

from conftest import _make_user
from helpers.helper_bins import helper_bin
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

#: The client makes one sign-in and one call; a budget, not a wait.
CLIENT_BUDGET_S = 120


def _pass(nest, admin_seed: bytes, *verb: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(helper_bin("fauna-reports-pass")), "--nest", nest["url"], *verb],
        input=admin_seed.hex() + "\n", capture_output=True, text=True,
        timeout=CLIENT_BUDGET_S, check=False,
    )


def _rows(listing: subprocess.CompletedProcess) -> dict[str, list[str]]:
    assert listing.returncode == 0, (
        f"list failed ({listing.returncode}): {listing.stderr.strip()}")
    lines = listing.stdout.splitlines()
    assert lines and lines[-1].startswith("open reports: "), listing.stdout
    rows = {line.split("\t")[0]: line.split("\t") for line in lines[:-1]}
    assert lines[-1] == f"open reports: {len(rows)}", listing.stdout
    return rows


def test_the_headless_pass_lists_and_resolves_an_open_report(nest_instance):
    nest = nest_instance
    admin_seed = bytes(nest["admin"]["signing_key"])
    reporter, author = _make_user(nest), _make_user(nest)
    author_hex = author["actor_id_bytes"].hex()

    # The reporter's own act through the nest API — the seed, not the journey.
    # The note carries a bidi override: box-sourced text the pass must print inert.
    filed = ws_api._client_for(ws_api._base_url(nest["port"]), reporter).call(
        "fauna.moderation.abuse_report.submit",
        {"subject": {"kind": "actor", "actor_id": author_hex}, "reason": "spam",
         "note": "probe‮end"},
    )
    report_id = filed["report_id"]

    rows = _rows(_pass(nest, admin_seed, "list"))
    assert report_id in rows, f"{report_id} not in the pass's listing: {sorted(rows)}"
    row = rows[report_id]
    assert row[1:3] == ["spam", f"account {author_hex}"], row
    assert "note=probe\\u{202e}end" in row, f"the note was not printed inert: {row}"
    assert "‮" not in "\t".join(row)

    done = _pass(nest, admin_seed, "resolve", report_id, "dismissed")
    assert done.returncode == 0, f"resolve failed: {done.stderr.strip()}"
    assert done.stdout.strip() == f"resolved {report_id}: dismissed", done.stdout

    assert report_id not in _rows(_pass(nest, admin_seed, "list")), (
        "a resolved report is still listed as open")
    mine = ws_api._client_for(ws_api._base_url(nest["port"]), reporter).call(
        "fauna.moderation.abuse_report.mine", {})
    entry = next(r for r in mine["reports"] if r["report_id"] == report_id)
    assert (entry["status"], entry.get("outcome")) == ("resolved", "dismissed"), entry


def test_a_non_admin_key_is_refused_and_a_bad_verb_never_connects(nest_instance):
    nest = nest_instance
    user = _make_user(nest)
    refused = _pass(nest, bytes(user["signing_key"]), "list")
    assert refused.returncode == 1, (refused.returncode, refused.stdout, refused.stderr)
    assert "listing refused" in refused.stderr, refused.stderr

    usage = _pass(nest, bytes(user["signing_key"]), "resolve", "not-hex!", "acted")
    assert usage.returncode == 2 and "not a report id" in usage.stderr, usage.stderr
