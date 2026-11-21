"""A file too large for one inline manifest, between two app seats — paged
when the nest can store it, refused onto the review list when it cannot.

Owner docs: ``docs/goal/behavior/file-sync.md`` § Paged manifests (a file
whose chunk list outgrows the inline bound is cut into manifest pages under
one root; *Compatibility*: a nest that does not advertise ``paged-manifests``
gets no paged manifest, and the file is refused before any of its chunks is
sent) and ``docs/goal/behavior/conflicts.md`` § Refused uploads reach the
review list (the refusal is an unresolved, candidate-free
``too_large_for_nest`` conflict reported to the nest and rendered on the
device's review list through the shared fold, while its other files keep
syncing). Catalog: ``docs/features/local-folder-sync.md`` outcomes 7 and 29.

The production paging rule is reached only by files of many gigabytes, so
both seats run under the engine's compile-gated ``FAUNA_E2E_PAGING_RULE``
seam (``helpers.sync_seats.make_seat(paging_rule=...)``): ``2,2,3,1`` pages
any file of more than two chunks, and a 24 MiB incompressible file is about
twelve. The refusal leg is the proof the seam reached the engine — only a
paged manifest is ever refused — so the round trip under the same rule is a
paged one.

The flow::

    seat a's user writes a large file into the bound folder
      -> a's engine cuts a paged manifest under the e2e rule
      -> nest advertises `paged-manifests`: chunks, pages, root go up; seat b
         pulls and writes the file byte-identical
      -> nest withholds it (`/api/v1/test/capability-withhold/paged-manifests`,
         before the seats connect — the set is read once per connection):
         a's engine refuses before any chunk, files a local
         `too_large_for_nest` row, reports it (the nest's `conflicts.list`),
         and a's Folders review list shows it; a small file beside it still
         reaches b

Every wait is a deadline poll on observed state (convention 14).
"""

from __future__ import annotations

import contextlib
import secrets
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path

import pytest

from common import register_user, user_create_folder
from common.accounts import actor_id_hex
from common.auth import (
    UNBINDING_MAX_DEVICES,
    sealed_path,
    set_tier_caps,
    sync_conflicts_list,
)
from helpers import sync_seats
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; the other desktops join through the same `make_seat("native")`
    # seam and the same shared fold renders their review list.
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.linux,
    pytest.mark.windows,
]

# Owed legs, in `sync_seats.UNBUILT_MODES`' shape. Landing a leg deletes its
# entry.
PAGED_OWED: dict = {
    "win32": {
        "native": (
            "a windows seat is an on-demand (cfapi) root; no windows run has read "
            "the paged round trip or the too-large refusal yet"
        ),
    },
}

# `inline,page_min,page_max,cut_bits` — the engine's `PagingRule::with_thresholds`
# (`fauna_sync_engine::engine::initial_paging_rule`); the same rule the tier_3
# Rust pin uses (`conformance_paged_manifest_two_engines.rs`).
PAGING_RULE = "2,2,3,1"
# About twelve chunks at the chunker's 2 MiB average — well past the inline two.
LARGE_SIZE = 24 * 1024 * 1024

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 300.0


def _seat_pairs() -> list:
    return [
        pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
        for pair in sync_seats.seat_sets_for_platform(sys.platform, unbuilt=PAGED_OWED)
    ]


_SEAT_PAIRS = _seat_pairs()


def _withhold_paged_manifests(nest, withhold: bool) -> None:
    """Make the nest stop (or resume) advertising `paged-manifests` — the real
    running nest standing in for one too old to store a paged manifest."""
    suffix = "" if withhold else "/clear"
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/capability-withhold/paged-manifests{suffix}",
        data=b"",
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"capability-withhold returned {resp.status}"


@contextlib.contextmanager
def _two_seats(nest, request, folder: str, seat_pair):
    """Two app seats of one account, both under :data:`PAGING_RULE`, bound to
    a custody-first set. Yields ``(a, b, secret_key)``."""
    from tests.test_filesync_seats import handle_for, harness_sign_in

    port, url = nest["port"], nest["url"]
    admin_sk = nest["admin"]["signing_key"]
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    run_token = sync_seats.new_run_token(seats=2)
    sign_in = harness_sign_in(url, secret_key, actor_id, handle)
    try:
        with contextlib.ExitStack() as stack:
            seats = sync_seats.start_seats(
                stack,
                seat_pair,
                make_one=lambda mode, name: sync_seats.make_seat(
                    mode,
                    name=name,
                    run_token=run_token,
                    root=tmp / name,
                    node_url=url,
                    node_port=port,
                    folder=folder,
                    sign_in=sign_in,
                    request=request,
                    paging_rule=PAGING_RULE,
                ),
                startup_window=STARTUP_WINDOW,
            )
            a, b = seats
            yield a, b, secret_key
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _await_bytes(seat, rel: str, body: bytes, what: str) -> None:
    target = seat.path / rel

    def landed() -> bool:
        try:
            return target.stat().st_size == len(body) and target.read_bytes() == body
        except OSError:
            return False

    wait_until(landed, WINDOW, interval=2.0,
               diagnose=lambda: f"{what}: {rel} never reached seat {seat.name} "
                                f"byte-identical\n{seat.diagnostics()}")


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 2 * WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("local-folder-sync")
def test_a_paged_file_reaches_the_other_seat_byte_identical(
    nest_instance, request, seat_pair
):
    """A file whose chunk list outgrows the inline bound goes up as a paged
    manifest and lands on the other device byte-identical
    (`file-sync.md` § Paged manifests)."""
    with _two_seats(nest_instance, request, "paged-docs", seat_pair) as (a, b, _):
        large = secrets.token_bytes(LARGE_SIZE)
        (a.path / "large.bin").write_bytes(large)
        _await_bytes(b, "large.bin", large, "the paged round trip")


def _too_large_rows(nest, secret_key: str) -> list[dict]:
    rows = sync_conflicts_list(nest["port"], secret_key=secret_key,
                               base_url=nest["url"])["conflicts"]
    return [c for c in rows if c["conflict_type"] == "too_large_for_nest"]


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 4 * WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("local-folder-sync")
def test_a_file_too_large_for_the_nest_is_refused_onto_the_review_list(
    nest_instance, request, seat_pair
):
    """The nest stops advertising `paged-manifests`; the large file is refused
    before any chunk, reported to the nest as an unresolved candidate-free
    `too_large_for_nest` conflict, shown on the writing seat's review list —
    and a small file written beside it still reaches the other seat
    (`conflicts.md` § Refused uploads reach the review list)."""
    folder = "too-large-docs"
    _withhold_paged_manifests(nest_instance, True)
    try:
        with _two_seats(nest_instance, request, folder, seat_pair) as (a, b, secret_key):
            large = secrets.token_bytes(LARGE_SIZE)
            small = b"the other files keep syncing"
            (a.path / "large.bin").write_bytes(large)
            (a.path / "small.txt").write_bytes(small)

            # The device's other files keep syncing.
            _await_bytes(b, "small.txt", small, "the file beside the refused one")

            # Reported: one unresolved, candidate-free row for the large file's
            # sealed path.
            seal = sealed_path(secret_key, "large.bin")
            got: list = []

            def reported() -> bool:
                got[:] = [c for c in _too_large_rows(nest_instance, secret_key)
                          if bytes(c.get("path_sealed") or b"") == seal]
                return bool(got)

            wait_until(reported, WINDOW, interval=2.0,
                       diagnose=lambda: "the refusal never reached conflicts.list: "
                                        f"{_too_large_rows(nest_instance, secret_key)!r}\n"
                                        f"{a.diagnostics()}")
            assert len(got) == 1, f"one report for the refused file: {got!r}"
            assert not got[0].get("candidates") and got[0].get("resolved_at") is None, got

            # The user sees it: the writing seat's Folders review list shows the
            # row with the too-large badge, naming the file.
            rows: list = []

            def rendered() -> bool:
                rows[:] = [
                    (badge, info) for badge, info in (a.review_rows() or [])
                    if badge == S.devices.conflicts.type_too_large_for_nest
                    and info.startswith(f"{folder}: ")
                ]
                return len(rows) == 1

            wait_until(rendered, WINDOW, interval=2.0,
                       diagnose=lambda: f"no Too large row on seat a's review list: "
                                        f"{a.review_rows()!r}\n{a.diagnostics()}")
            assert "large.bin" in rows[0][1], rows

            # Refused before any chunk: the other seat never receives it. Safe
            # as a bare absence only behind the small file's landing and the
            # report — the pass that would have sent it has provably run.
            assert not (b.path / "large.bin").exists(), (
                "a file the nest cannot store must never reach the other device"
            )
    finally:
        _withhold_paged_manifests(nest_instance, False)
