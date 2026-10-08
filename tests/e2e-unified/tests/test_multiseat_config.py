"""tier_1 unit: the pure run-id arithmetic for the three-machine live filesync
test (``helpers/multiseat_config.py``), plus the machine→seat mapping the
announce step uses to decide which client it must build before printing an id.
No nest, no driver — just the "largest YYYYMMDD-NN written today, plus one"
function the announce step feeds the set's Media-listed file basenames into."""

import sys

import pytest

from helpers import multiseat_config as cfg

pytestmark = pytest.mark.tier_1

TODAY = "20260718"


def test_empty_set_is_01():
    assert cfg.next_run_id_from_names([], TODAY) == "20260718-01"


def test_prior_days_do_not_count():
    names = ["20260717-05-hello-linux.txt", "20260716-09-shared.txt"]
    assert cfg.next_run_id_from_names(names, TODAY) == "20260718-01"


def test_max_plus_one_across_kinds_and_seats():
    # The shared file (no seat suffix) and per-seat files all carry the id prefix
    # and all count; the highest today-NN wins regardless of kind or seat.
    names = [
        "20260718-01-hello-linux.txt",
        "20260718-01-shared.txt",
        "20260718-02-hello-macos.txt",
        "20260718-02-done-windows.txt",
        "20260717-09-done-windows.txt",  # yesterday — ignored
    ]
    assert cfg.next_run_id_from_names(names, TODAY) == "20260718-03"


def test_two_digit_roll_over():
    assert cfg.next_run_id_from_names(["20260718-09-shared.txt"], TODAY) == "20260718-10"


def test_non_matching_basenames_ignored():
    names = [
        "r1807-hello-linux.txt",          # manual FAUNA_MULTISEAT_RUN_ID override
        "hello.txt",                      # unrelated file
        "20260718-shared.txt",            # date but no NN segment
        "2026071-01-hello.txt",           # short (7-digit) date
        "  20260718-04-hello-linux.txt ",  # whitespace tolerated
    ]
    assert cfg.next_run_id_from_names(names, TODAY) == "20260718-05"


# ── machine → seat (the announce's build gate) ────────────────────────────────
#
# The announce prints one run id that commits EVERY machine to `go`, so it must
# not print until this machine's own SEAT client is built. That means the
# announce — which runs on the READER client (tui) — has to know which seat this
# machine plays without a seat driver in hand. `_seat_of(driver)` cannot answer
# that; this pure mapping can.


@pytest.mark.parametrize(
    "platform,seat",
    [
        ("linux", "linux"),
        ("darwin", "macos"),
        ("win32", "windows"),
    ],
)
def test_seat_for_platform_maps_the_three_dev_machines(platform, seat):
    assert cfg.seat_for_platform(platform) == seat


@pytest.mark.parametrize("platform", ["freebsd13", "aix", "emscripten", ""])
def test_seat_for_platform_is_none_off_the_three(platform):
    """A non-seat platform announces without a seat build rather than failing —
    the reader is cohort-agnostic and any box may compute the id."""
    assert cfg.seat_for_platform(platform) is None


def test_local_seat_matches_this_machine():
    assert cfg.local_seat() == cfg.seat_for_platform(sys.platform)


def test_local_seat_is_a_real_seat_on_a_dev_machine():
    """The three dev machines (Linux, macOS, Windows) each play exactly one seat, and the
    name must be one the cohort actually rendezvouses on."""
    seat = cfg.local_seat()
    if seat is None:
        pytest.skip(f"{sys.platform} is not one of the three seat platforms")
    assert seat in ("linux", "macos", "windows")


# ── which CLIENT may drive this machine's seat ────────────────────────────────
#
# The seat identifies the MACHINE (one machine, one seat); the client is a
# `--client` PARAMETER, so a seat can be driven by its native desktop app or
# by the terminal client. Not every pairing has a local sync engine behind it,
# and a pairing that silently has none is the exact failure mode that cost this
# track a day — so the unsupported ones must fail loudly, not skip quietly.


@pytest.mark.parametrize(
    "client,platform",
    [
        ("linux", "linux"),
        ("macos", "darwin"),
        ("windows", "win32"),
        # tui drives a seat wherever its agent provisioner is compiled in —
        # `apps/fauna-tui/src/sync_agent.rs` is `cfg(any(unix, windows))`, i.e.
        # all three seat platforms (see the next test).
        ("tui", "linux"),
        ("tui", "darwin"),
        ("tui", "win32"),
    ],
)
def test_seat_client_supported_pairings(client, platform):
    assert cfg.seat_app_error(client, platform) is None


@pytest.mark.parametrize("platform", ["linux", "darwin", "win32"])
def test_tui_drives_a_seat_on_every_seat_platform(platform):
    """tui is a seat client everywhere, windows included (
    2026-07-24: `sync_agent.rs` went `cfg(unix)` -> `cfg(any(unix, windows))`
    with a `WindowsDetachedSpawner`, and `drivers/tui.py` gained the windows
    per-launch pipe + data-dir isolation arm).

    This is what lets the win seat skip the ~25 min WinUI MSBuild: the round's
    windows seat is `--client tui`, driving the same `fauna-sync-agent` the
    native app would, over `fauna_ipc::endpoint::AgentEndpoint`'s pipe arm.
    """
    assert cfg.seat_app_error("tui", platform) is None


@pytest.mark.parametrize("client", ["web", "android", "ios"])
def test_clients_without_a_local_sync_engine_are_rejected(client):
    err = cfg.seat_app_error(client, "linux")
    assert err is not None


def test_native_client_must_match_its_machine():
    """`--client macos` on the linux box is an operator slip, not a seat."""
    assert cfg.seat_app_error("macos", "linux") is not None


@pytest.mark.parametrize(
    "platform,client",
    [("linux", "linux"), ("darwin", "macos"), ("win32", "windows")],
)
def test_seat_client_defaults_to_the_native_desktop_client(platform, client, monkeypatch):
    monkeypatch.delenv("FAUNA_MULTISEAT_SEAT_CLIENT", raising=False)
    assert cfg.seat_app(platform) == client


def test_seat_client_env_override_wins(monkeypatch):
    """So a tui-seat operator does not pay for the native build (on Windows that is a
    ~25 min WinUI MSBuild for a client the run never launches)."""
    monkeypatch.setenv("FAUNA_MULTISEAT_SEAT_CLIENT", "tui")
    assert cfg.seat_app("darwin") == "tui"


def test_seat_client_blank_override_falls_back_to_native(monkeypatch):
    monkeypatch.setenv("FAUNA_MULTISEAT_SEAT_CLIENT", "   ")
    assert cfg.seat_app("linux") == "linux"


# ── settle_listing: an unloaded read must never pass as an empty set ──────────
#
# The bug this pins (observed live 2026-07-24): the announce reader's settle loop
# accepted two consecutive EMPTY snapshots as "stable" and returned [] after ~1 s.
# An empty listing is indistinguishable from a not-yet-loaded one by sameness
# alone, and next_run_id_from_names([]) then yields `-01` regardless of what is
# really on the nest — silently handing machines a run id that cannot rendezvous.
#
# The rule: only a NON-EMPTY listing settles early; an empty one polls to the
# deadline and is then returned as-is (a genuinely empty set is legitimate — the
# caller warns rather than fails). An earlier revision raised on empty when the
# set_filter select had succeeded, on the theory that the filter only offers
# non-empty sets; that premise is FALSE (the select succeeds against an empty set)
# and it false-positived on a correct read. Do not reintroduce it.


class _FakeClock:
    """Deterministic clock — the test pays no wall-clock time (convention 14)."""

    def __init__(self) -> None:
        self.t = 0.0

    def monotonic(self) -> float:
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds


def _snapshots(*frames):
    """A snapshot callable yielding each frame once, then repeating the last."""
    seq = list(frames)

    def _next():
        return seq.pop(0) if len(seq) > 1 else seq[0]

    return _next


def test_a_slow_load_is_not_mistaken_for_an_empty_set():
    """THE REGRESSION. Two identical empty reads are not a settled listing."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots([], [], [], ["20260724-05-hello-linux.txt"]),
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == ["20260724-05-hello-linux.txt"]


def test_an_empty_listing_polls_to_the_deadline_before_being_believed():
    """The invariant that replaces the unsound filter floor: empty is returned
    (a genuinely empty set is legitimate) but never settled EARLY — the loop
    must spend the full window giving a slow load its chance to appear."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots([]),
        timeout_s=20.0,
        poll_s=1.0,
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == []
    assert clock.t >= 20.0, (
        f"gave up after {clock.t}s — an empty listing must poll to the deadline, "
        "not settle on two identical empty reads"
    )


def test_a_growing_listing_settles_on_the_full_set():
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots(["a.txt"], ["a.txt", "b.txt"], ["a.txt", "b.txt"]),
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == ["a.txt", "b.txt"]


def test_a_stable_non_empty_listing_settles_early():
    """The common case must stay fast — no paying the full window when loaded."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots(["a.txt"]), sleep=clock.sleep, monotonic=clock.monotonic
    )
    assert names == ["a.txt"]
    assert clock.t < 20.0


def test_the_settled_listing_feeds_the_run_id():
    """End-to-end of the two pure pieces: waiting out the empty read is what
    makes the id right. Settling early on [] would yield `-01` instead of `-05`."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots([], [], ["20260724-04-hello-windows.txt"]),
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert cfg.next_run_id_from_names(names, "20260724") == "20260724-05"


# ── `loaded`: the real signal replaces the wall-clock inference ───────────────
#
# `media-empty-state` (ui.yaml `media` page, user-approved 2026-08-05) is what an
# app paints once its first read has RETURNED and found nothing. With it, empty
# stops being a guess: a loaded-and-empty page settles at once instead of paying
# the full window, and a page that never loads FAILS LOUDLY instead of returning
# `[]` and silently handing three machines a run id that cannot rendezvous.
# convention 14 — the deadline stops being the thing the verdict is inferred FROM
# and becomes a bound that a green run never pays.


def test_a_loaded_empty_page_settles_immediately():
    """The genuine empty set is now PROVEN, not waited out."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots([]),
        loaded=lambda: True,
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == []
    assert clock.t == 0.0, (
        f"paid {clock.t}s for an answer the app had already given — an app that "
        "paints media-empty-state has settled the question"
    )


def test_a_page_that_never_loads_fails_loudly_instead_of_returning_empty():
    """THE POINT. Returning `[]` from an unloaded read is what produced a wrong
    run id in silence; on an app that can tell us it never loaded, that is now a
    diagnosed failure."""
    clock = _FakeClock()
    with pytest.raises(RuntimeError, match="never finished loading"):
        cfg.settle_listing(
            _snapshots([]),
            loaded=lambda: False,
            timeout_s=20.0,
            sleep=clock.sleep,
            monotonic=clock.monotonic,
        )
    assert clock.t >= 20.0, "it must give the load its full window before failing"


def test_a_slow_load_that_arrives_still_settles_on_the_names():
    """The signal is a floor, not a tripwire: a page that loads late and non-empty
    settles normally."""
    clock = _FakeClock()
    states = iter([False, False, True, True])

    names = cfg.settle_listing(
        _snapshots([], [], ["a.txt"], ["a.txt"]),
        loaded=lambda: next(states, True),
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == ["a.txt"]


def test_a_probe_that_cannot_answer_keeps_the_old_wall_clock_settle():
    """A probe returning ``None`` (no signal — e.g. it raised) must behave
    exactly as before — never raise, never settle empty early. Reading
    'cannot answer' as 'still loading' would hang every genuinely empty set."""
    clock = _FakeClock()
    names = cfg.settle_listing(
        _snapshots([]),
        loaded=lambda: None,
        timeout_s=20.0,
        sleep=clock.sleep,
        monotonic=clock.monotonic,
    )
    assert names == []
    assert clock.t >= 20.0


# --- The live admin seed resolves PER BOX (testing.md § Default app and nest
# mode, *Live mode*): FAUNA_LIVE_SECRET_HEX > the staging box's own provisioning
# identity for the live URL's host > ~/.fauna-id. A live run against
# dev.example.com that signed in with example.com's ~/.fauna-id was refused
# `not_registered` and skipped the whole run as "box not ready" — the
# misdiagnosis this pins away.

_ENV_SEED = "11" * 32
_BOX_SEED = "22" * 32
_HOME_SEED = "33" * 32


@pytest.fixture
def _seed_home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))  # Path.home() under Windows
    monkeypatch.delenv("FAUNA_LIVE_SECRET_HEX", raising=False)
    monkeypatch.delenv("FAUNA_LIVE_NEST_URL", raising=False)
    return tmp_path


def _write_box(home, domain, body=None):
    import json

    d = home / ".config" / "fauna" / "staging-box"
    d.mkdir(parents=True, exist_ok=True)
    if body is None:
        body = json.dumps({"domain": domain, "secret_hex": _BOX_SEED})
    (d / f"{domain}.json").write_text(body)


def test_the_env_seed_beats_the_box_file(_seed_home, monkeypatch):
    _write_box(_seed_home, "dev.example.com")
    (_seed_home / ".fauna-id").write_text(_HOME_SEED)
    monkeypatch.setenv("FAUNA_LIVE_SECRET_HEX", _ENV_SEED)
    assert cfg.resolve_secret("https://dev.example.com") == (_ENV_SEED, "FAUNA_LIVE_SECRET_HEX")


def test_the_box_file_for_the_live_host_beats_the_home_seed(_seed_home):
    _write_box(_seed_home, "dev.example.com")
    (_seed_home / ".fauna-id").write_text(_HOME_SEED)
    assert cfg.resolve_secret("https://dev.example.com") == (
        _BOX_SEED,
        "~/.config/fauna/staging-box/dev.example.com.json",
    )


def test_another_hosts_box_file_is_never_used(_seed_home):
    _write_box(_seed_home, "other.example.com")
    (_seed_home / ".fauna-id").write_text(_HOME_SEED)
    assert cfg.resolve_secret("https://dev.example.com") == (_HOME_SEED, "~/.fauna-id")


@pytest.mark.parametrize("body", ['{"domain": "dev.example.com"}', "{not json", '["x"]'])
def test_a_box_file_without_a_usable_seed_falls_through(_seed_home, body):
    _write_box(_seed_home, "dev.example.com", body)
    (_seed_home / ".fauna-id").write_text(_HOME_SEED)
    assert cfg.resolve_secret("https://dev.example.com") == (_HOME_SEED, "~/.fauna-id")


def test_the_dotted_home_alias_is_still_read(_seed_home):
    (_seed_home / ".fauna.id").write_text(_HOME_SEED + "\n")
    assert cfg.resolve_secret("https://dev.example.com") == (_HOME_SEED, "~/.fauna.id")


def test_no_seed_anywhere_is_none(_seed_home):
    assert cfg.resolve_secret("https://dev.example.com") == (None, None)
    assert cfg.load_secret("https://dev.example.com") is None


def test_without_a_url_the_default_live_nest_is_the_host(_seed_home, monkeypatch):
    """The multiseat test calls `load_secret()` bare: its box is `nest_url()`."""
    _write_box(_seed_home, "box.example")
    monkeypatch.setenv("FAUNA_LIVE_NEST_URL", "https://box.example")
    assert cfg.load_secret() == _BOX_SEED


def test_a_live_url_with_a_port_resolves_by_host(_seed_home):
    _write_box(_seed_home, "dev.example.com")
    assert cfg.load_secret("https://dev.example.com:8443/") == _BOX_SEED
