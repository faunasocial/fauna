"""Nest build/start helpers for Fauna E2E tests."""

import functools
import hashlib
import json
import os
import shutil
import socket
import sqlite3
import ssl
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

CLAIM_CODE = "TEST42"


class _OwnDialAuthority:
    """Sentinel `handle_domain_seed`: *this nest's own dial authority*.

    A handle domain that is an IP-literal authority (`127.0.0.1:<port>` —
    `testing.md` § Default app and nest mode, ruling (3)) is a fact about a nest
    that does not exist yet: the port is allocated by whoever starts it. Four
    fixtures used to reach for it by allocating the port THEMSELVES and handing
    it back in through `port=`, and that is precisely what kept them out of the
    mode provider — a caller that must pick the port cannot let a provider pick
    it, so the fixture had to spawn its own binary and stayed in the
    `nest_binary` closure with no named reason.

    Passing this sentinel says the same thing as a property of the nest instead
    of a value the caller computes: *whatever authority a client dials to reach
    you, advertise that as your handle domain*. `start_nest` resolves it to
    `f"{dial_host}:{port}"` — the same two values its `url` is composed from, so
    the seed and the URL cannot disagree — and publishes the resolved string
    under `handle_domain_seed`, which is what a re-spawn (`start_nest_in_place`)
    then carries.

    It stays standalone-only, and permanently: `handle_domain_seed` is the
    `--handle-domain` boot flag, which ruling (3) declares a class (4) absence in
    every other mode. The sentinel does not change that — it changes only who
    computes the value, which is what lets the four fixtures route through
    `_start_dedicated_nest` and be excluded by a *named* option rather than by an
    anonymous binary closure.
    """

    __slots__ = ()

    def __repr__(self) -> str:  # so a refusal message reads as the name
        return "OWN_DIAL_AUTHORITY"


OWN_DIAL_AUTHORITY = _OwnDialAuthority()


def resolve_handle_domain_seed(handle_domain_seed, dial_host, port):
    """`OWN_DIAL_AUTHORITY` → `"<dial_host>:<port>"`; anything else unchanged.

    A named function rather than two lines inline so the fact is pinnable
    without starting a nest — `test_nest_mode_axis.py` asserts it directly, and
    an `is`-identity sentinel resolved in the middle of a 200-line spawn is
    otherwise witnessed only by whatever integration test happens to read the
    seed back.

    `None` (no seed) and a plain string (an explicitly named domain) both pass
    through untouched, which is what keeps this a pure widening: the ~40 existing
    `handle_domain_seed="fauna.test"` sites cannot notice it.
    """
    if handle_domain_seed is OWN_DIAL_AUTHORITY:
        return f"{dial_host}:{port}"
    return handle_domain_seed

# Default `wait_for_node` budget — a GENEROUS CEILING for one operation class
# ("a debug nest binary boots and serves /health"), sized far above any
# non-pathological boot rather than tuned to observed latency (e2e-conventions.md
# convention 14, the same discipline as `helpers/budgets.py`; compare its
# SERVICE_BOOT_S = 180.0, which guards the same kind of wait).
#
# Uniform across platforms since 2026-08-03. It used to be `180 if win32 else 30`,
# justified as "non-Windows keeps the fast 30s default so a genuinely dead nest is
# reported quickly" — but that justification does not survive inspection: the poll
# loop below ALREADY raises the moment `proc.poll()` shows the process gone
# ("exited early"), so a genuinely dead nest is reported in milliseconds at any
# ceiling. The short budget therefore bought nothing, and bounded only the
# alive-but-slow case — which on these boxes is the FALSE-positive case, not a
# real one: a dev VM routinely runs 3–15 checkouts building in parallel (load average 45 was
# measured while this constant was being changed), and a debug nest that has
# logged its sidecar tokens can then need well past 30s to bind and serve health.
# A green run pays nothing for the larger ceiling — the loop returns as soon as
# health answers.
DEFAULT_NODE_TIMEOUT = 180.0


def get_repo_root() -> Path:
    # Use this file's location to find the repo root, so it works
    # regardless of the shell's cwd (important for linked checkouts).
    here = Path(__file__).resolve().parent
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True,
        cwd=here,
    )
    return Path(result.stdout.strip())


def load_and_rewrite_config(config_name: str, tmp_dir: str) -> str:
    """Load a config from config/ dir, rewrite /data/ paths to tmp_dir, write to tmp_dir.

    Args:
        config_name: Filename in config/ directory (e.g., "default.toml").
        tmp_dir: Target directory for rewritten config.

    ⚠ There is deliberately no ``registration_mode`` seed here. The posture is an
    admin choice, so the harness sets it the way an admin's app does — one
    ``common.auth.set_registration_mode`` call after the claim (``testing.md``
    § Default app and nest mode, ruling (3)). A seed written into this file was
    the same knob wearing a config file's clothes, which the invariant bans
    outright (``principles.md`` § One configuration surface; owner:
    ``public-mode.md`` § Registration Modes).

    Returns the path to the rewritten config file.
    """
    repo = get_repo_root()
    src = repo / "config" / config_name
    content = src.read_text()
    # Rewrite /data/ paths to tmp_dir (forward slashes for TOML compatibility)
    tmp_normalized = tmp_dir.replace("\\", "/")
    content = content.replace("/data/", f"{tmp_normalized}/")
    config_path = os.path.join(tmp_dir, "config.toml")
    with open(config_path, "w") as f:
        f.write(content)
    return config_path


def _cargo_cmd(repo: Path) -> list[str]:
    """Return the cargo command for the current platform.

    On Windows, uses scripts/cargo-win.cmd to set up MSVC/LLVM environment.
    On Unix, uses bare cargo.
    """
    if sys.platform == "win32":
        wrapper = repo / "scripts" / "cargo-win.cmd"
        if wrapper.exists():
            return ["cmd", "/c", str(wrapper)]
    return ["cargo"]


def _build_slot_cmd(repo: Path) -> list[str]:
    """Prefix routing a heavy build through the machine-wide `build` slot pool.

    `cargo build -p fauna-nest` compiles most of the workspace — exactly the
    "workspace-scale command outside `just`" the pool exists for. Before
    2026-07-29 this helper's build ran completely unslotted, so N concurrent
    pytest sessions each compiled the nest with no load bound at all (the
    inverse of the fixture slot-wait inversion; build-system.md § Build/e2e
    slot locks). Reentrancy via FAUNA_SLOT_HELD_BUILD means an externally
    slot-wrapped caller runs directly instead of double-queueing.

    The slot script itself is fleet-only tooling and does not ship (a solo
    checkout has no sibling builds to queue against, so there is nothing for
    it to arbitrate) — degrades to an empty prefix, same shape as
    `_cargo_cmd`'s win wrapper above.
    """
    wrapper = repo / "scripts" / "build-slot.py"
    if not wrapper.exists():
        return []
    return [sys.executable, str(wrapper), "--pool", "build", "--"]


def _cargo_target_dir(repo: Path) -> Path:
    """CARGO_TARGET_DIR env (the primary dev VM's shell profile always sets
    it), else the cargo default `<repo>/target` (Windows/macOS)."""
    env = os.environ.get("CARGO_TARGET_DIR")
    return Path(env) if env else repo / "target"


@functools.cache
def _load_build_if_stale():
    """Import scripts/build-if-stale.py (hyphenated filename → importlib by
    path) so the builders below share ONE freshness scanner — and one
    exclusion semantics — plus one stamp-dating rule (`run_for_stamp`, the
    build-slot grant instant) with the justfile gates. Resolved beside this
    file: the same checkout `get_repo_root` names, without a git call."""
    import importlib.util

    path = Path(__file__).resolve().parents[2] / "scripts" / "build-if-stale.py"
    spec = importlib.util.spec_from_file_location("fauna_build_if_stale", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# Same exclusions as the justfile's libs-keyed gates: wasm-pack out-dirs are
# build OUTPUTS living inside libs/, and libs/fauna-mail-go is the mail-bridge
# pipeline's own generated tree — none of them is a cargo input, and watching
# any of them would re-stale this gate on every unrelated build.
_NEST_SOURCE_EXCLUDES = ["*/pkg/*", "*/pkg-test/*", "*/fauna-mail-go/*"]


def _nest_newest_source_mtime(repo: Path) -> float:
    """Newest mtime across fauna-nest's build inputs (bins/fauna-nest + libs +
    Cargo.lock; every fauna-nest path dep points into libs/)."""
    bis = _load_build_if_stale()
    return max(
        bis._source_newest(repo / rel, _NEST_SOURCE_EXCLUDES)
        for rel in ("bins/fauna-nest", "libs", "Cargo.lock", "rust-toolchain.toml")
    )


def _nest_variant_paths(target_dir: Path, features: str, profile: str) -> tuple[Path, Path]:
    """(pinned exe copy, freshness stamp) for one (features, profile) variant."""
    tag = hashlib.sha1(f"{features}|{profile}".encode()).hexdigest()[:8]
    suffix = ".exe" if sys.platform == "win32" else ""
    profile_dir = target_dir / profile
    return (
        profile_dir / f"fauna-nest-e2e-{tag}{suffix}",
        profile_dir / f".fauna-nest-e2e-{tag}.stamp",
    )


def _variant_build_is_fresh(built: Path, stamp: Path, newest_source_mtime) -> bool:
    """Freshness of one build shape (nest's per-(features, profile) variant, or
    a single-shape binary), stamp-keyed. Shared by every `build_*` here.

    The stamp — not the binary — carries freshness, for the same reason the
    justfile's cargo gates use `--stamp`: a cargo no-op leaves its artifact
    untouched, so an artifact-keyed gate goes permanently stale on any source
    mtime churn (every rebase). `built` is an existence check; `newest_source_mtime`
    is a callable so the libs/ scan is skipped when the files alone already decide.
    """
    if not built.exists() or not stamp.exists():
        return False
    return newest_source_mtime() <= stamp.stat().st_mtime


def build_node(release: bool = False, features: str = "test-hooks,nostr") -> str:
    """Build fauna-nest and return the binary path.

    Default `features="test-hooks,nostr"`:
      * `test-hooks` exposes the `/api/v1/test/outbound/*` admin endpoints
        (mock clock + scripted send-fn + state inspectors) for outbound-mail
        testing (tracked internally).
      * `nostr` registers `NostrProvider` (`#[cfg(feature = "nostr")]`,
        bins/fauna-nest/src/lib.rs) so `fauna.bridges.list` surfaces the Nostr
        bridge → `status.available` is true on a plaintext nest → the Nostr
        settings flows (`tests/test_nostr.py`, all apps) are exercisable.
        Without it `fauna.bridges.list` omits nostr and every app renders
        only the Phase-1 "unavailable" notice (nostr.md § Implementation status).
    `test-hooks` is on in no production cargo build (e2e convention 15), so the
    admin test endpoints are e2e-only. `nostr` is NOT in that class and this
    docstring claimed it was until 2026-08-06: the nest Docker image ships
    `--features fauna-nest/bluesky,fauna-nest/nostr,fauna-nest/activitypub`, so
    all three planes are production shapes — what makes this build e2e-only is
    `test-hooks`. Pass a different `features` string (e.g.
    `"test-hooks,activitypub"`) for a test that needs a different provider set —
    every registered provider adds startup surface + list-page noise the test
    doesn't want, so build only what the test exercises.

    ``release=True`` builds the optimized profile (default is the fast-to-compile
    debug profile every functional test uses). Only a *perf* measurement wants it
    — a debug nest's absolute latencies + mailbox-scaling are debug-amplified and
    not release-representative (used by the Phase-3 IMAP FETCH-latency benchmark).

    Load + freshness discipline (2026-07-29; build-system.md § Build/e2e slot
    locks):

    * **The cargo run holds a machine-wide `build` slot** (`_build_slot_cmd`) —
      it compiles most of the workspace and previously ran completely unslotted.
    * **A warm tree runs no cargo and takes no slot**: freshness is a per-variant
      stamp (`_variant_build_is_fresh`), so on the common warm path this returns in
      one libs/ mtime scan. The intended first call site is
      `pytest_collection_finish` (`_prebuild_binaries`), OUTSIDE every per-test
      timeout; the in-fixture call is then a warm no-op.
    * **The returned path is a per-variant pinned COPY**
      (`fauna-nest-e2e-<hash>`), not cargo's shared `target/<profile>/fauna-nest`:
      every feature set compiles to the same cargo path, so whichever variant
      built last used to own the file and a previously returned path could
      silently point at a nest with different features (the bluesky "unknown
      kind" trap — its fixture pioneered this copy). Pinning per variant also
      means spawned nests hold the copy open, never the base exe cargo relinks
      (the Windows "Access is denied" class).
    """
    repo = get_repo_root()
    profile = "release" if release else "debug"
    pinned, stamp = _nest_variant_paths(_cargo_target_dir(repo), features, profile)
    if _variant_build_is_fresh(pinned, stamp, lambda: _nest_newest_source_mtime(repo)):
        return str(pinned)

    # The stamp's instant is the build-slot GRANT (`run_for_stamp`, same rule
    # as `--stamp`): a source edited while cargo runs stays newer than the
    # stamp so the next call rebuilds, while one edited while the build only
    # queued was compiled by it — the warm pass's in-hold re-check must not
    # queue for `build` again over it.
    profile_args = ["--release"] if release else []
    result, instant = _load_build_if_stale().run_for_stamp(
        _build_slot_cmd(repo)
        + _cargo_cmd(repo)
        + ["build", "-p", "fauna-nest",
           "--features", features,
           *profile_args,
           "--message-format=json"],
        run=subprocess.run, capture_output=True, text=True, cwd=repo,
    )
    if result.returncode != 0:
        # On Windows, cargo build fails if the exe is locked by another process
        # (e.g., orphaned nest from a previous test session). Fall back to the
        # pinned variant copy if one exists (stale is better than nothing; the
        # stamp is deliberately NOT committed, so the next call retries).
        if sys.platform == "win32" and "Access is denied" in result.stderr:
            if pinned.exists():
                return str(pinned)
            fallback = repo / "target" / profile / "fauna-nest.exe"
            if fallback.exists():
                return str(fallback)
        raise RuntimeError(f"cargo build failed:\n{result.stderr}")

    # Parse JSON messages to find the fauna-nest executable
    for line in reversed(result.stdout.strip().split("\n")):
        try:
            msg = json.loads(line)
            exe = msg.get("executable")
            if exe and Path(exe).stem == "fauna-nest":
                pinned.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(exe, pinned)
                _load_build_if_stale().commit_stamp(
                    stamp, instant,
                    "fauna-nest e2e variant freshness stamp; mtime = build start (slot grant)\n"
                    f"features={features} profile={profile}\n",
                )
                return str(pinned)
        except json.JSONDecodeError:
            continue
    raise RuntimeError("Could not find fauna-nest binary in cargo output")


def _read_log_tail(log_path: str | None, n: int = 40) -> str:
    if not log_path:
        return "(no nest log captured)"
    try:
        lines = Path(log_path).read_text().splitlines()
    except OSError:
        return "(nest log unavailable)"
    return "\n".join(lines[-n:]) if lines else "(nest log empty)"


def wait_for_node(port: int, timeout: float | None = None, proc=None,
                  log_path=None, scheme: str = "http"):
    """Poll until the node's health endpoint responds.

    `timeout` defaults to `DEFAULT_NODE_TIMEOUT` (platform-aware: generous on
    Windows where debug-nest boot is ~60s, 30s elsewhere). When `proc`/`log_path`
    are supplied, fail fast on early process exit and include the nest's log tail
    in the error — so a startup failure is diagnosable instead of a blind "did
    not start" timeout. The per-request `timeout=2` keeps a single hung connect
    from eating the whole budget.

    `scheme` is `"http"` for the usual `FAUNA_INSECURE_DISABLE_TLS` plain-HTTP
    harness nest; pass `"https"` for a `serve_tls=True` nest that serves its
    self-signed floor cert on the API listener — the health poll then connects
    over TLS with verification OFF (the floor is self-signed, name-mismatched by
    design; client trust is channel-binding, not WebPKI — `security.md`
    § Transport trust).
    """
    if timeout is None:
        timeout = DEFAULT_NODE_TIMEOUT
    # A self-signed floor never validates against WebPKI; the health poll only
    # needs liveness, so accept any cert (the onboarding probe under test is what
    # exercises the real channel-binding trust path).
    ssl_ctx = None
    if scheme == "https":
        ssl_ctx = ssl.create_default_context()
        ssl_ctx.check_hostname = False
        ssl_ctx.verify_mode = ssl.CERT_NONE
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc is not None and proc.poll() is not None:
            raise RuntimeError(
                f"Nest on port {port} exited early (rc={proc.returncode}).\n"
                f"{_read_log_tail(log_path)}"
            )
        try:
            req = urllib.request.urlopen(
                f"{scheme}://127.0.0.1:{port}/api/v1/health",
                timeout=2, context=ssl_ctx)
            if req.status == 200:
                return
        except Exception:
            pass
        time.sleep(0.2)
    raise TimeoutError(
        f"Node on port {port} did not start within {timeout}s.\n"
        f"The process was still ALIVE at the deadline (a nest that had exited "
        f"would have been reported above as 'exited early'), so this is a nest "
        f"that booted but never served /health — check the log tail below for "
        f"where it stopped, and `uptime` for whether the box was saturated.\n"
        f"{_read_log_tail(log_path)}"
    )


def _sync_service_win_build_paths(target_dir: Path) -> tuple[Path, Path]:
    """(built exe, freshness stamp) for the windows e2e build of
    `fauna-sync-agent.exe` — the cross-platform `fauna-sync-agent` package
    built by itself. Always the implicit-host debug profile, one feature set,
    so there is only ever one cargo output path: no variant-clobber risk, just
    the stamp that lets a warm tree skip cargo (and the build slot) entirely."""
    debug_dir = target_dir / "debug"
    return (
        debug_dir / "fauna-sync-agent.exe",
        debug_dir / ".fauna-sync-agent-e2e.stamp",
    )


def _sync_service_win_newest_source_mtime(repo: Path) -> float:
    """Newest mtime across fauna-sync-agent's build inputs (its own crate
    dir + libs + Cargo.lock; every path dep points into libs/)."""
    bis = _load_build_if_stale()
    return max(
        bis._source_newest(repo / rel, _NEST_SOURCE_EXCLUDES)
        for rel in (
            "bins/fauna-sync-agent",
            "libs",
            "Cargo.lock",
            "rust-toolchain.toml",
        )
    )


def build_sync_service_win() -> str:
    """Build `fauna-sync-agent.exe` (the windows per-user sync agent) and
    return its path — the builder behind
    `tests/e2e-unified/conftest.py::sync_agent_binary`.

    Load + freshness discipline mirrors `build_node()`:
    the cargo run holds a machine-wide `build` slot — `cargo build -p
    fauna-sync-agent` compiles most of the workspace and previously ran
    completely unslotted, the last outlier `_PREBUILD_BY_FIXTURE`'s own
    comment named — and a warm tree (stamp newer than every source) runs no
    cargo and takes no slot at all.

    The package is `fauna-sync-agent` itself, the one package that produces
    this exe on every platform (the windows-only `fauna-sync-service` wrapper
    crate, and the name this function still carries, were retired 2026-10-02).
    It is named ALONE on purpose: one cargo invocation naming a second package
    (`-p fauna-tui`, say) unifies the two feature sets, and this build is the
    agent with the features it asks for and no others.

    fauna-sync-agent is a ROOT-workspace member built
    IMPLICIT-HOST (2026-08-23, row 60): win is arm64, so the host triple
    already IS aarch64-pc-windows-msvc, and an explicit `--target` would
    split this build onto a disjoint unit graph from the dev inner loop's
    `fauna-nest` build (measured: 0 of 66 workspace-local units shared,
    61 of 66 after dropping the flag).

    Never trusts cargo's own exit code — it lies on win-arm64 — so success
    is judged by the known exe path existing after the run, same as the
    fixture this replaces judged it before.
    """
    repo = get_repo_root()
    binary, stamp = _sync_service_win_build_paths(_cargo_target_dir(repo))
    if _variant_build_is_fresh(
        binary, stamp, lambda: _sync_service_win_newest_source_mtime(repo)
    ):
        return str(binary)

    # Stamp dated at the build-slot grant — see build_node's identical comment.
    result, instant = _load_build_if_stale().run_for_stamp(
        _build_slot_cmd(repo) + _cargo_cmd(repo) + ["build", "-p", "fauna-sync-agent"],
        run=subprocess.run, cwd=repo, capture_output=True, text=True,
    )
    if not binary.exists():
        tail = result.stderr[-3000:] if result.stderr else result.stdout[-3000:]
        raise RuntimeError(
            f"fauna-sync-agent.exe not found at {binary}\nBuild output tail:\n{tail}"
        )
    _load_build_if_stale().commit_stamp(
        stamp, instant,
        "fauna-sync-agent e2e freshness stamp; mtime = build start (slot grant)\n",
    )
    return str(binary)


def _recovery_fixture_build_paths(target_dir: Path) -> tuple[Path, Path]:
    """(built exe, freshness stamp) for the test-only `recovery_fixture`
    example — one debug shape, same single-shape reasoning as
    `_sync_service_win_build_paths`."""
    examples_dir = target_dir / "debug" / "examples"
    suffix = ".exe" if sys.platform == "win32" else ""
    return (
        examples_dir / f"recovery_fixture{suffix}",
        target_dir / "debug" / ".recovery-fixture-e2e.stamp",
    )


def _recovery_fixture_newest_source_mtime(repo: Path) -> float:
    """Newest mtime across the fixture's build inputs: it links the shared
    protocol/client crates, so a bare comparison against its own source (the
    pre-2026-09-29 rule) let a binary built against an older wire shape run
    against a newer nest — every path dep points into libs/."""
    bis = _load_build_if_stale()
    return max(
        bis._source_newest(repo / rel, _NEST_SOURCE_EXCLUDES)
        for rel in ("libs", "Cargo.lock", "rust-toolchain.toml")
    )


def build_recovery_fixture() -> str:
    """Build the `recovery_fixture` example (`libs/fauna-client-recovery`) and
    return its path — the builder behind
    `tests/e2e-unified/helpers/succession.py::recovery_fixture_binary`.

    Load + freshness discipline mirrors `build_sync_service_win()`: the cargo
    run holds a machine-wide `build` slot, goes through `_cargo_cmd` (bare
    `cargo` fails on Windows, where Git Bash's `link.exe` shadows the MSVC linker),
    and a warm tree — stamp newer than every workspace source — runs no cargo
    and takes no slot. The stamp, not the binary, carries freshness: the helper
    signs profiles and statements with the SHARED crates' encoders, so a binary
    older than any of them produced a body the nest's strict decode refused
    (`fauna.profile.invalid_request … SchemaMismatch`, measured 2026-09-29).

    Never trusts cargo's own exit code (it lies on win-arm64) — success is the
    known exe path existing after the run.
    """
    repo = get_repo_root()
    binary, stamp = _recovery_fixture_build_paths(_cargo_target_dir(repo))
    if _variant_build_is_fresh(
        binary, stamp, lambda: _recovery_fixture_newest_source_mtime(repo)
    ):
        return str(binary)

    # Stamp dated at the build-slot grant — see build_node's identical comment.
    result, instant = _load_build_if_stale().run_for_stamp(
        _build_slot_cmd(repo)
        + _cargo_cmd(repo)
        + ["build", "-p", "fauna-client-recovery", "--example", "recovery_fixture"],
        run=subprocess.run, cwd=repo, capture_output=True, text=True,
    )
    if not binary.exists():
        tail = result.stderr[-3000:] if result.stderr else result.stdout[-3000:]
        raise RuntimeError(
            f"recovery_fixture not found at {binary}\nBuild output tail:\n{tail}"
        )
    _load_build_if_stale().commit_stamp(
        stamp, instant,
        "recovery_fixture e2e freshness stamp; mtime = build start (slot grant)\n",
    )
    return str(binary)


def build_app() -> str:
    """Build the WinUI 3 desktop app and return the exe path."""
    repo = get_repo_root()
    csproj = repo / "apps" / "fauna-windows" / "FaunaApp" / "FaunaApp" / "FaunaApp.csproj"
    result = subprocess.run(
        ["dotnet", "build", str(csproj), "-c", "Debug"],
        capture_output=True, text=True, cwd=repo,
    )
    if result.returncode != 0:
        raise RuntimeError(f"dotnet build failed:\n{result.stderr}\n{result.stdout}")

    # The exe lands in bin/Debug/<tfm>/
    bin_dir = csproj.parent / "bin" / "Debug"
    # Find the target framework directory (e.g., net10.0-windows10.0.26100)
    for d in sorted(bin_dir.iterdir()):
        exe = d / "FaunaApp.exe"
        if exe.exists():
            return str(exe)
    raise RuntimeError(f"Could not find FaunaApp.exe under {bin_dir}")


def _is_loopback_host(host: str) -> bool:
    """Python twin of Rust `is_loopback_authority` (`fauna-anon-client/src/trust.rs`).

    Deliberately mirrors it exactly, DNS resolution included — i.e. NOT AT ALL:
    the literal name `localhost`, or an IP literal in `127.0.0.0/8` / `::1`. Any
    other hostname is non-loopback to a client *even if it resolves to loopback*.
    Keep the two in lockstep; this is what decides which trust branch a test drives.
    """
    import ipaddress
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host.strip("[]")).is_loopback
    except ValueError:
        return False


def lan_ipv4() -> str:
    """This box's primary NON-loopback IPv4, as a client on the LAN would dial it.

    The point is the *authority*, not the route: a client trusting a self-signed
    nest short-circuits to "accept" whenever the authority is loopback
    (`is_loopback_authority` — literal `localhost`, or an IP in `127.0.0.0/8` /
    `::1`; it never resolves DNS). So a loopback-dialled nest can NEVER exercise
    the SPKI-**pin** branch of that decision. Dialling this address instead makes
    the authority non-loopback — the pin branch is the only way trust can succeed
    — while the packets still never leave the box.

    Windows Firewall does not filter same-host traffic to the host's own IP, so
    this needs no elevation, no firewall rule, and no hosts-file entry (verified
    on Windows; the hosts-file alternative would need admin and so is CI-hostile).

    Uses a connectionless UDP socket: it sends nothing, it just asks the routing
    table which local address would source a packet to the outside.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))  # no traffic; selects the source iface
        ip = s.getsockname()[0]
    finally:
        s.close()
    if ip.startswith("127.") or ip == "0.0.0.0":
        raise RuntimeError(
            f"No non-loopback IPv4 on this box (got {ip!r}) — a pin-branch test "
            "needs one; it cannot fall back to loopback without silently testing "
            "the loopback short-circuit instead."
        )
    return ip


#: The nest's durable deployment-seed file, relative to its data dir. ONE name,
#: because three call sites need it and two of them used to spell it themselves:
#: this reader, `common.helpers.sign_as_nest`, and the docker provider.
NEST_DEPLOYMENT_KEY = "nest_deployment.key"


def nest_id_from_data_dir(data_dir) -> str:
    """The nest's identity — ``nest.info``'s ``nest_id`` — read off its data dir.

    The single-identity unification retired the separate ``nest_identity.key``;
    a nest now has ONE identity, the deployment seed in
    :data:`NEST_DEPLOYMENT_KEY` (a raw 32-byte Ed25519 seed), which backs
    ``nest.info``'s ``nest_id``, federation and the sync signer alike
    (``box-recovery.md`` § Single-identity unification).

    **Mode-agnostic by construction**, which is why it is a helper rather than
    two copies: docker bind-mounts its ``/data`` to a host directory, so the
    file the image writes at boot is the same file at the same relative path
    under the handle's ``tmp_dir``. A provider that could not answer ``nest_id``
    would strand every federation-channel test — the harness signs the
    ``fauna.federation.hello`` as the initiator nest, so it needs both nests'
    ids and the initiator's key off disk.

    Returns ``""`` when the file is not there — an unclaimed or not-yet-booted
    nest — rather than raising, matching what ``start_nest`` has always done.
    """
    key_path = os.path.join(str(data_dir), NEST_DEPLOYMENT_KEY)
    if not os.path.exists(key_path):
        return ""
    from nacl.signing import SigningKey as NaClSigningKey

    with open(key_path, "rb") as kf:
        return bytes(NaClSigningKey(kf.read()).verify_key).hex()


def start_nest(node_binary, tmp_dir, port, config_name="default.toml",
               unclaimed=False,
               claim_domain=None, handle_domain_seed=None, serve_tls=False,
               extra_env=None, dial_host=None, cors_origins=None,
               static_dir=None):
    """Start a nest instance using a canonical config file.

    Args:
        node_binary: Path to fauna-nest binary.
        tmp_dir: Temporary directory for this nest instance.
        port: Port to bind to.
        config_name: Config file from config/ directory (default: "default.toml").
        unclaimed: When True, do NOT claim the admin — leave the nest in its
            fresh, never-claimed state (``setup-status.claimed == false``) so a
            client-UI onboarding e2e can drive ``POST /api/v1/claim-admin``
            itself. ``admin`` is ``None`` in the returned dict; the one-time
            claim code is on disk at ``<tmp_dir>/claim-code`` for the test.
        claim_domain: The domain to claim this nest ONTO — carried as the
            claim's ``mail_domain``, so ``claim_core`` registers it as the
            primary ``mail_domains`` row and ``apply_primary_identity`` makes it
            the deployment identity that ``handle_domain()`` reads at top
            precedence. This is a **wire act**, not a boot knob, so every nest
            mode can honour it (``testing.md`` § Default app and nest mode,
            ruling (3)). Requires a claim to attach to: passing it with
            ``unclaimed=True`` is a contradiction and raises.

            ⚠ Do NOT pass this on a nest whose fixture or test registers the
            same domain itself. ``fauna.bridges.add_local_domain`` is idempotent
            *by domain name* and answers an already-active domain with
            ``skipped: true``, silently discarding that caller's
            ``mta_sts_cert_mode`` / catch-all / DKIM
            arguments — every mail-shaped caller in the suite asks for
            ``per_host`` where the claim hard-codes ``expand_primary``. Such a
            site passes NEITHER domain option: its own registration sets the
            identity.
        handle_domain_seed: When set, start the nest advertising this string as
            its registration handle domain (``--handle-domain``) — the
            conformance-harness boot seed ``domains-and-tls-bootstrap.md``
            § Env contract wants dead, kept for the two shapes no wire act can
            express. A cross-nest federation peer sets it to its own reachable
            authority (in tier_3 its loopback ``127.0.0.1:<port>``) so
            ``fauna.actor.by_handle`` replies with a domain the originating
            nest's relay can reach — the binary equivalent of
            ``RegistrationConfig { handle_domain: .. }`` in
            ``conformance_cross_nest_conversations_client.rs``; the claim gate
            registers no local target, so this one can never be a claim. The
            other shape is an ``unclaimed=True`` nest, which has no harness
            claim to carry a domain however registerable the value is — the
            *client* drives that claim through the app, which is the point of
            the fixture. Standalone-only in both cases: a class (4) declared
            absence in every other mode.

            ``OWN_DIAL_AUTHORITY`` is the sentinel for the first shape and the
            only way a caller should ever spell it: it resolves HERE to
            ``f"{dial_host}:{port}"``, so the caller never needs the port and
            can therefore let a mode provider allocate one.
        extra_env: Optional dict of extra environment variables for this nest's
            process only (bucket-2 IPC wiring, never a human-edited knob). The
            box-recovery case is ``{"FAUNA_DEPLOYMENT_SEED": "<64-hex>"}``: a
            *rebuilt* box adopts the admin's custodied deployment seed on a fresh
            data dir and so re-presents the SAME ``nest_id`` — the harness twin of
            cloud-init's env injection (``box-recovery.md`` § Mechanism — Restore).
            Preserved across ``factory_reset_and_restart`` / ``start_nest_in_place``
            so a re-spawn keeps the injected identity.
        serve_tls: When True, this nest serves REAL self-signed HTTPS on its API
            listener (the always-live floor cert) instead of the harness's
            default plain HTTP — by clearing the process-wide
            ``FAUNA_INSECURE_DISABLE_TLS`` escape for this nest only. The
            returned ``url`` is then ``https://…``. Pairs with ``unclaimed=True``
            for onboarding flows that drive claim over TLS through the UI; with
            ``unclaimed=False`` the harness auto-claim dials the same https base
            (a mail fixture that pre-provisions over Admin WS-RPC then drives the
            client onboarding over TLS — Pillar C uniform-https posture).
            See ``nest/domains-and-tls-bootstrap.md`` § Test posture.
        cors_origins: Browser origins this nest should allow cross-origin, seeded
            at boot through ``--cors-origin`` — the artifact's own boot seed for
            the client-set ``nest_cors_origins`` state (``provisioning/registry.md``
            § Health-poll CORS: "the ``--cors-origin`` CLI flag / TOML value is only
            a boot seed (artifact wiring), never the choice surface"). Bucket-1
            wiring, not a knob: the harness IS this nest's deployment artifact, and
            the origin it seeds is its own SPA proxy's, exactly as the Docker
            entrypoint seeds ``FAUNA_CORS_ORIGINS`` with the deployment's app origin.

            Only a nest a BROWSER dials **raw** needs it. Most web tests reach the
            nest through ``_serve_spa_proxy``, which is same-origin by construction,
            so they need nothing here. The fixtures that hand the wasm client a raw
            nest URL — ``provision_target_nest`` (the orchestrator's Online health
            poll ``GET {nest_base_url}/api/v1/health``) — do: without the seed the
            nest answers with no ``Access-Control-Allow-Origin`` at all (its empty
            list collapses to ``DEFAULT_CORS_ORIGIN``, ``https://app.fauna.social``),
            the browser blocks the response, and reqwest-wasm reports the generic
            "error sending request" forever. Native apps are outside a browser and
            never see this, which is what makes it a web-only failure class.
        dial_host: The host a CLIENT dials to reach this nest — i.e. the authority
            in the returned ``url``. Defaults to loopback ``127.0.0.1``. Pass
            ``lan_ipv4()`` to hand clients a NON-loopback authority, which is the
            only way to drive a client's SPKI-**pin** trust branch instead of its
            loopback short-circuit (see ``lan_ipv4``). The listen interface widens
            to ``0.0.0.0`` automatically so the nest is actually reachable there —
            setting one without the other would just yield a dead port.

    Returns dict with process info: proc, port, url, db_path, nest_id,
    tmp_dir, config_path, admin (None when unclaimed=True).
    """
    from common.auth import claim_admin

    if claim_domain and unclaimed:
        raise ValueError(
            "claim_domain needs a claim to attach to, and unclaimed=True is the "
            "harness declining to make one — the client drives that claim through "
            "the app. Such a nest advertises its domain with handle_domain_seed "
            "(testing.md § Default app and nest mode, ruling (3))."
        )

    tmp_dir = str(tmp_dir)
    os.makedirs(tmp_dir, exist_ok=True)

    # Load canonical config and rewrite paths
    config_path = load_and_rewrite_config(config_name, tmp_dir)

    db_path = os.path.join(tmp_dir, "nest.db").replace("\\", "/")
    blob_dir = os.path.join(tmp_dir, "blobs").replace("\\", "/")
    os.makedirs(blob_dir, exist_ok=True)

    # Write claim code file (nest binary also generates one if missing,
    # but writing it here avoids a race with the test trying to claim
    # before the nest has written the file)
    claim_code_path = os.path.join(tmp_dir, "claim-code")
    with open(claim_code_path, "w") as f:
        f.write(CLAIM_CODE)

    # Start server — --bind, --config, and --blob-dir as CLI args. blob_dir must
    # be passed via CLI because main.rs reads args.blob_dir directly (doesn't
    # honor nest_config.nest.blob_dir); without it /api/v1/blob returns 503.
    # The spawn + health-wait live in `_spawn_and_wait`, shared with
    # `factory_reset_and_restart` (which re-spawns the same binary after a wipe).
    log_path = os.path.join(tmp_dir, "nest.log")
    # A non-loopback `dial_host` must be backed by a non-loopback LISTEN, or the
    # client dials a dead port. Bind 0.0.0.0 in that case; keep the tight loopback
    # bind (the default) for every ordinary harness nest.
    dial_host = dial_host or "127.0.0.1"
    bind_host = "127.0.0.1" if _is_loopback_host(dial_host) else "0.0.0.0"
    # `OWN_DIAL_AUTHORITY` resolves HERE rather than at the call site, and the
    # placement is the whole point of the sentinel: `dial_host` has just taken
    # its default and `port` is settled, so the seed is composed from exactly the
    # two values the `url` below is composed from and the two cannot disagree.
    # Resolving before the returned dict is also what makes a re-spawn correct —
    # `start_nest_in_place` carries `handle_domain_seed` forward, and a sentinel
    # carried forward would re-resolve against whatever port that spawn got.
    handle_domain_seed = resolve_handle_domain_seed(
        handle_domain_seed, dial_host, port)
    cli_args = _build_nest_cli_args(
        node_binary, port, config_path, blob_dir,
        handle_domain=handle_domain_seed,
        bind_host=bind_host,
        cors_origins=cors_origins,
        static_dir=static_dir,
    )
    proc = _spawn_and_wait(cli_args, port, log_path, serve_tls=serve_tls,
                           extra_env=extra_env)

    # Claim admin via claim code — unless the caller wants a fresh UNCLAIMED
    # nest (a client-UI claim e2e drives POST /api/v1/claim-admin itself). The
    # one-time code is on disk at <tmp_dir>/claim-code for the test to read.
    # A `serve_tls` nest serves ONLY https, so the auto-claim must dial https too
    # (the floor cert is self-signed; `claim_admin`'s `_resolve_base` uses an
    # unverified context for an `https://` base — channel-binding trust, not WebPKI).
    # The harness's own claim call may dial loopback regardless of `dial_host` (it
    # is not the thing under test — the CLIENT's trust path is), but it MUST match
    # the scheme, and an https base needs the unverified context `_resolve_base`
    # picks for it.
    scheme = "https" if serve_tls else "http"
    claim_base = f"{scheme}://127.0.0.1:{port}" if serve_tls else None
    admin = (
        None if unclaimed
        else claim_admin(port, CLAIM_CODE, base_url=claim_base,
                         mail_domain=claim_domain)
    )

    nest_id = nest_id_from_data_dir(tmp_dir)

    return {
        "proc": proc,
        "port": port,
        # The authority a CLIENT dials. `dial_host` is what decides whether that
        # client's self-signed-trust decision takes the loopback short-circuit or
        # the SPKI-pin branch, so it belongs in the url, not just the bind.
        "url": f"{scheme}://{dial_host}:{port}",
        "db_path": db_path,
        "nest_id": nest_id,
        "tmp_dir": tmp_dir,
        "config_path": config_path,
        "admin": admin,
        # The one-time code this box will accept, for a fixture that leaves the
        # nest UNCLAIMED and lets a client drive the claim. Published on the
        # handle rather than left to the `CLAIM_CODE` constant because the
        # constant is a fact about *standalone*: docker's provider generates a
        # random code per container and publishes it under this same key, so a
        # test reading `nest["claim_code"]` stays honest in either local mode,
        # while one importing the constant silently presents `TEST42` to a
        # container that never heard of it. (Live answers neither — it starts
        # no nest at all.)
        "claim_code": CLAIM_CODE,
        # Launch params so `factory_reset_and_restart` can re-spawn this exact
        # binary itself — locally there is no s6 supervisor to restart the nest
        # into the boot-time wipe (`factory_reset.rs::maybe_run_factory_reset`).
        "node_binary": node_binary,
        "blob_dir": blob_dir,
        "log_path": log_path,
        # No `registration_open` here: it is not a start input any more, so a
        # re-spawn has nothing to carry. The posture lives in the nest's own DB
        # — which a factory reset wipes, so a fixture that resets and still
        # wants an open nest calls `common.auth.open_registration` again after
        # the re-claim (arm 4; no fixture needs that today).
        "handle_domain_seed": handle_domain_seed,
        # Carried because a factory reset wipes the DB, and the identity a
        # domained claim registered lives THERE (the primary `mail_domains` row),
        # not in the boot args — so a re-claim must carry the domain again or the
        # nest comes back on the `localhost` fallback. This is the same shape as
        # the `registration_open` note above, and the opposite conclusion: the
        # posture had no re-spawn input to carry, this one does.
        "claim_domain": claim_domain,
        "extra_env": extra_env,
        # Re-spawn inputs (`start_nest_in_place` / `factory_reset_and_restart`).
        # `serve_tls` was previously NOT carried here, so a restarted TLS nest came
        # back as PLAIN HTTP while its `url` still said https — a silent downgrade.
        "serve_tls": serve_tls,
        "bind_host": bind_host,
        "dial_host": dial_host,
        # Carried for the same reason `serve_tls` is: a re-spawn that dropped the
        # seed would come back refusing the browser origin it was started to
        # allow, and the only symptom would be the wasm client's generic
        # "error sending request" — the failure this option exists to prevent.
        "cors_origins": cors_origins,
        # Carried so a re-spawned nest keeps serving the SPA build it was
        # started over (its `/app/` and the share viewer page).
        "static_dir": static_dir,
    }


def _build_nest_cli_args(node_binary, port, config_path, blob_dir, *,
                         handle_domain=None,
                         bind_host="127.0.0.1",
                         cors_origins=None,
                         static_dir=None):
    """Build the fauna-nest CLI args. Shared by `start_nest` (first boot) and
    `factory_reset_and_restart` (re-spawn after a wipe) so both launch the binary
    identically — the re-spawn must reuse the SAME data dir for the boot-time
    wipe (`factory_reset.rs::maybe_run_factory_reset`) to run.

    `bind_host` is the listen interface. It stays loopback for every ordinary
    harness nest; `start_nest(dial_host=...)` widens it to `0.0.0.0` when a test
    needs the nest reachable on a NON-loopback authority (see `lan_ipv4`)."""
    cli_args = [node_binary, "--bind", f"{bind_host}:{port}",
                "--config", config_path,
                "--blob-dir", blob_dir]
    # The registration posture is NOT here, in any shape — not a flag (the nest
    # pins that itself: `main.rs::registration_posture_has_no_cli_flags`) and no
    # longer a config seed either. It is an admin choice, so the harness makes it
    # over the wire after the claim: `common.auth.set_registration_mode`.
    if handle_domain:
        cli_args += ["--handle-domain", handle_domain]
    # The CORS allow-list seed, one flag per origin (`registry.md` § Health-poll
    # CORS). Boot wiring the artifact sets, like `--bind` — the live surface stays
    # `fauna.admin.set_cors_origins`, whose DB row wins over this seed for good
    # (`node_policy_core::resolve_cors_origins`).
    for origin in cors_origins or ():
        cli_args += ["--cors-origin", origin]
    # The SPA build this nest serves at `/app/` (and the private share link
    # viewer page at `/share/<token>`): boot wiring the deployment artifact
    # sets — the image points it at its bundled build — never a knob.
    if static_dir:
        cli_args += ["--static-dir", static_dir]
    return cli_args


def _spawn_and_wait(cli_args, port, log_path, *, serve_tls=False, extra_env=None):
    """Spawn a fauna-nest process, register it for cleanup, and block until it is
    healthy. Returns the process. On a startup failure the process is killed and
    the error carries the nest log tail (via `wait_for_node`).

    Output is redirected to `log_path` rather than an unread subprocess.PIPE: a
    PIPE's ~64 KB buffer can fill while the nest runs (e.g. the backup/GC
    scheduler logging in a loop), blocking the nest on write(). A file never
    blocks, and the log survives for post-mortem when startup fails.

    `serve_tls=True` makes this one nest serve REAL self-signed HTTPS on its API
    listener: the e2e harness sets `FAUNA_INSECURE_DISABLE_TLS=1` process-wide
    (`conftest.py` `pytest_sessionstart`) so every nest serves plain HTTP, but a
    test that needs the always-live self-signed floor on the nest's own listener
    (e.g. the onboarding-probe self-signed-TLS regression) clears the escape for
    its nest only — the nest then runs `prepare_listener_tls` and serves the
    floor. See `nest/domains-and-tls-bootstrap.md` § Test posture.

    `extra_env` sets **bucket-2 IPC env** on this nest only (the artifact-set
    wiring class, never a human-edited knob — a product invariant: the
    client UI is the one user-config surface, not env/config-file knobs).
    The box-recovery case is `FAUNA_DEPLOYMENT_SEED`: a rebuilt box is provisioned
    with the admin's custodied deployment seed so it re-presents the SAME
    `nest_actor_id` (`box-recovery.md` § Mechanism — Restore; the same env var
    cloud-init and both self-hosted installers render). This is the harness half
    of the "provision a box with a caller-supplied deployment seed" primitive.
    """
    log_fh = open(log_path, "w")
    # Accelerate the nest's background-expiry tick for tests (default 1 h →
    # a few seconds) so time-driven jobs — notably the scheduled DKIM
    # rotation-mint (`run_scheduled_dkim_rotation_mint`) — fire within a test's
    # lifetime. Harmless to shorten: the contact/knock TTLs are 30/90 days and
    # the soft-delete GC is 30 days, so every job but the due-gated DKIM
    # rotation stays a no-op at any cadence. `setdefault` lets a specific run
    # override it; production never sets it (→ 1 h).
    nest_env = dict(os.environ)
    nest_env.setdefault("FAUNA_EXPIRY_INTERVAL_SECS", "3")
    if serve_tls:
        # Drop the process-wide plain-HTTP escape for THIS nest so it serves the
        # self-signed floor over TLS on its API listener (the var is read once at
        # boot — `main.rs` `force_plain_http`).
        nest_env.pop("FAUNA_INSECURE_DISABLE_TLS", None)
    if extra_env:
        nest_env.update(extra_env)
    # The port's scheme is this nest's, not a previous tenant's: a port outlives
    # its nest, and a stale TLS mark sends this nest's own claim over TLS at a
    # plaintext listener (`ssl.SSLError: RECORD_LAYER_FAILURE`).
    from common.auth import mark_tls_nest, unmark_tls_nest

    if serve_tls:
        mark_tls_nest(port)
    else:
        unmark_tls_nest(port)
    # Die-with-parent spawn where the e2e harness is available: the nest
    # becomes a reapable group leader and (on Linux) gets PDEATHSIG, so it
    # cannot outlive a SIGKILLed pytest (testing.md § conventions, point 9).
    reap_kwargs = {}
    try:
        from drivers.port_util import popen_group_kwargs
        reap_kwargs = popen_group_kwargs()
    except ImportError:
        pass  # standalone usage outside e2e-unified
    if os.name == "nt":
        # Its own process group, so `stop_nest(graceful=True)` can reach THIS nest
        # alone with a Ctrl+Break — Windows has no SIGTERM, and `terminate()` there
        # is TerminateProcess, a hard kill that skips the 1001 + drain entirely.
        reap_kwargs["creationflags"] = (
            reap_kwargs.get("creationflags", 0) | subprocess.CREATE_NEW_PROCESS_GROUP
        )
    proc = subprocess.Popen(
        cli_args, stdout=log_fh, stderr=subprocess.STDOUT, env=nest_env, **reap_kwargs
    )
    log_fh.close()  # child holds its own dup'd fd; the parent's copy is done

    # The Windows half of that same guarantee, which this spawn site was missing.
    # There are no process groups there, so `popen_group_kwargs()` is `{}` by
    # construction and the kernel-level mechanism is a kill-on-close job object,
    # necessarily armed AFTER the spawn. Without it the nest's only protection is
    # the cooperative atexit sweep below — which a killed, crashed, or
    # hard-timed-out run never reaches, so the nest outlives it. The orphan then
    # holds its own pinned `fauna-nest-e2e-<hash>.exe` open, and because that name
    # is deterministic per (features, profile) the NEXT run's `shutil.copy2` in
    # `build_node` fails with `[WinError 32] … used by another process` — i.e. a
    # failing run silently arms the following run's failure, and the second
    # failure looks like a build break rather than a leak (found on Windows
    # 2026-08-02). No-op off Windows (returns False).
    try:
        from drivers.port_util import reap_descendants_of
        reap_descendants_of(proc.pid)
    except ImportError:
        pass  # standalone usage outside e2e-unified

    # Register for atexit cleanup immediately — if wait_for_node (or a later
    # claim) raises, the process would otherwise be orphaned.
    try:
        from drivers.port_util import track_process
        track_process(proc)
    except ImportError:
        pass  # standalone usage outside e2e-unified

    try:
        wait_for_node(port, proc=proc, log_path=log_path,
                      scheme="https" if serve_tls else "http")
    except Exception:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        raise
    return proc


def _wait_nest_fresh(url, timeout=120.0):
    """Poll `fauna.setup.status` until the nest is back AND fresh (`claimed` and
    `admin_exists` both False), tolerating the mid-restart window. Shared with
    the live believable-mail test's `_wait_nest_fresh`."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            with WsRpcAnonClient(url) as anon:
                st = anon.call("fauna.setup.status", {})
            last = st
            if st.get("claimed") is False and st.get("admin_exists") is False:
                return
        except Exception:
            pass  # nest mid-restart — keep polling
        time.sleep(2.0)
    raise RuntimeError(
        f"nest did not return to fresh/unclaimed within {timeout:.0f}s after "
        f"factory_reset (last setup.status: {last!r})"
    )


def factory_reset_and_restart(nest, *, reclaim=True, timeout=120.0):
    """Factory-reset a locally-spawned binary nest and bring it back fresh.

    Mirrors the production restart-wipe (`docs/goal/architecture/nest/common.md`
    § Factory reset): the Admin-gated `fauna.admin.factory_reset` WS-RPC stages a
    marker, replies with the post-reset claim code, then `exit(0)`s — expecting an
    s6 supervisor to restart it into the boot-time wipe. A `start_nest` binary has
    NO supervisor, so this helper BECOMES the supervisor: it drives the reset,
    waits for the exit, re-spawns the same binary with the same data dir (so
    `maybe_run_factory_reset` runs the wipe), waits for the nest to come back
    fresh/unclaimed, and (when `reclaim`) re-claims admin with the SAME identity —
    matching the live CalDAV/mail tests' "re-claim (same identity)" cycle.

    The `nest` dict is mutated in place (new `proc`, new `admin`) and returned, so
    existing references stay valid. The port is preserved, so a client's
    `node_url` survives the cycle. The next claim code is pinned to the module
    `CLAIM_CODE` so the re-claim reuses the standard local code.

    NOTE: this wipes ALL deployment state — registered users, storage mode, mail
    enablement. Callers re-establish what they need (re-register users, re-commit
    storage, re-login) after this returns; only the admin identity is restored.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import claim_admin

    admin = nest.get("admin")
    if admin is None:
        raise RuntimeError("factory_reset_and_restart requires a claimed nest")
    admin_sk = admin["signing_key"]  # PyNaCl SigningKey from claim_admin

    # 1) Drive the reset over the authenticated WS-RPC connection, pinning the
    #    next claim code so the re-claim below reuses the standard local code.
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    ) as adm:
        reply = adm.call("fauna.admin.factory_reset", {"new_claim_code": CLAIM_CODE})
    assert reply.get("claim_code") == CLAIM_CODE, (
        f"factory_reset should echo the pinned claim code: {reply!r}"
    )

    # 2) The handler exits ~750ms after replying (reply flush + DB flush).
    proc = nest["proc"]
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        # It should self-exit; force it so the restart is deterministic.
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()

    # 3) BE the supervisor: re-spawn the same binary with the same args + data
    #    dir. At boot, `maybe_run_factory_reset` sees the marker and wipes
    #    deployment state before opening the DB, then installs the staged code.
    cli_args = _build_nest_cli_args(
        nest["node_binary"], nest["port"], nest["config_path"], nest["blob_dir"],
        handle_domain=nest["handle_domain_seed"],
        bind_host=nest.get("bind_host", "127.0.0.1"),
        cors_origins=nest.get("cors_origins"),
        static_dir=nest.get("static_dir"),
    )
    # Re-apply `serve_tls`, else the post-wipe nest comes back on plain HTTP while
    # `nest["url"]` still says https (and `_wait_nest_fresh` below polls that url).
    nest["proc"] = _spawn_and_wait(cli_args, nest["port"], nest["log_path"],
                                   serve_tls=nest.get("serve_tls", False),
                                   extra_env=nest.get("extra_env"))

    # 4) Wait until the wiped nest is back AND fresh/unclaimed.
    _wait_nest_fresh(nest["url"], timeout=timeout)

    # 5) Re-claim admin with the SAME identity (the live "re-claim (same
    #    identity)"). Without reclaim the caller drives onboarding itself.
    nest["admin"] = (
        claim_admin(nest["port"], CLAIM_CODE, signing_key=admin_sk,
                    mail_domain=nest.get("claim_domain"))
        if reclaim else None
    )
    return nest


def restart_nest(nest, *, graceful=True, timeout=30.0):
    """Restart a locally-spawned binary nest IN PLACE on the SAME data dir,
    WITHOUT factory-reset and WITHOUT re-claiming — the *benign flip*.

    Mirrors a Watchtower redeploy: Watchtower stops the old container (SIGTERM,
    then SIGKILL after `stop_grace_period`) and starts a fresh one from the new
    image on the SAME `fauna-data:/data` volume. The nest's Ed25519 identity
    (`nest_deployment.key`), SQLite DB, blobs, storage mode, and registered users
    all persist on the data dir; only the process is replaced. In-memory state —
    crucially the bearer `token_store` — is wiped, so a connected client's cached
    bearer is 401'd at the next WS upgrade and silently re-minted from its
    persisted key (`transport.md` § Connection lifecycle).

    Contrast `factory_reset_and_restart`, which WIPES deployment state (the
    deliberately-NON-seamless identity-rotation path, `nest/common.md` § Factory
    reset). This helper is the SEAMLESS path the reconnect machinery is supposed
    to make invisible — the one `test_nest_flip_resilience.py` regression-guards.

    Args:
        graceful: when True (the production Watchtower default now that the
            graceful-shutdown work landed), SIGTERM the old proc so it runs the
            graceful path — broadcast WS 1001 (Going Away) + drain in-flight +
            `db.flush()` — before exit (`transport.md` § Graceful shutdown,
            `main.rs` SIGTERM handler). When False, SIGKILL it for an abrupt drop
            with no close frame (the client notices via TCP FIN / keepalive and
            reconnects with backoff) — the path Track 1's 1001 close improves on.
        timeout: max seconds to wait for the old proc to exit before forcing it.

    Mutates `nest` in place (new `proc`); the port + data dir are preserved so a
    connected client's `node_url` survives the cycle. Returns `nest`. The new nest
    is healthy (port bound, `wait_for_node` passed) by the time this returns; the
    CLIENT may still be mid-reconnect (backing off), which is exactly the in-gap
    window the resilience test exercises.

    Process safety: only the fixture's OWN `proc` handle is signalled
    — never `pkill`/`killall`/name-match, which would SIGTERM sibling sessions'
    nests.
    """
    stop_nest(nest, graceful=graceful, timeout=timeout)
    return start_nest_in_place(nest)


def stop_nest(nest, *, graceful=True, timeout=30.0):
    """Stop a locally-spawned binary nest and leave it DOWN (no respawn) — the
    first half of `restart_nest`, split out so a test can observe the client's
    behaviour *while the nest is gone* (e.g. the global `connection-status`
    indicator flipping off "Connected"), then bring it back with
    `start_nest_in_place`.

    `graceful=True` SIGTERMs the proc (→ WS 1001 + drain + flush, the Watchtower
    path); `False` SIGKILLs it (abrupt drop, no close frame). The data dir —
    identity, SQLite DB, blobs, claim — is untouched, so a later
    `start_nest_in_place` resumes the SAME deployment on the SAME port (a
    connected client's `node_url` survives). Mutates `nest` in place (the `proc`
    has exited); returns `nest`.

    On Windows, where there is no SIGTERM and `terminate()` is a hard
    TerminateProcess, the graceful stop is a Ctrl+Break to the nest's own process
    group (`_spawn_and_wait` starts it in one; the nest treats Ctrl+Break as its
    shutdown signal). A hard kill there turned every "graceful" stop into an
    abrupt one, so a read on the wire at that instant failed as the transport
    says an in-flight read must on an ABRUPT drop — and a passing-gap journey saw
    an error banner one run in four. If the event cannot be delivered (the
    console-less caller case: Ctrl events travel only within a console), the stop
    degrades to the hard kill and says so.

    Process safety: only the fixture's OWN `proc` handle is signalled
    — never `pkill`/`killall`/name-match, which would SIGTERM sibling sessions' nests.
    The Ctrl+Break names the nest's own group id (its pid), never group 0.
    """
    proc = nest["proc"]
    if graceful and os.name == "nt":
        import signal

        try:
            proc.send_signal(signal.CTRL_BREAK_EVENT)
        except OSError as e:
            print(
                f"stop_nest: Ctrl+Break could not reach nest pid {proc.pid} ({e}); "
                "falling back to a HARD kill — the stop is not graceful",
                flush=True,
            )
            proc.terminate()
    elif graceful:
        proc.terminate()  # SIGTERM → graceful shutdown (WS 1001 + drain + flush)
    else:
        proc.kill()  # SIGKILL → abrupt drop, no close frame
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    return nest


def start_nest_in_place(nest):
    """Re-spawn a stopped nest on the SAME port + data dir (the second half of
    `restart_nest`; pairs with `stop_nest`). No factory-reset marker is staged, so
    `maybe_run_factory_reset` is a no-op at boot: the DB, identity, and claim all
    survive — the admin is NOT re-claimed. Blocks until the new nest is healthy
    (`wait_for_node`). Mutates `nest` in place (new `proc`); returns `nest`.

    **Mode-agnostic by delegation, not by branching.** A nest whose provider knows
    how to bring it back answers a `start_in_place` key, and that provider is asked
    rather than second-guessed — docker restarts its own container, standalone
    re-spawns the binary below. The fallback is not a guess either: a nest carrying
    no such key came from one of the dedicated-nest fixtures, which spawn a local
    binary directly and are therefore standalone by construction (testing.md
    § Default app and nest mode — the nest-binary closure). Probed with an explicit
    default, which `NestHandle.get` documents as the deliberate-probe form.
    """
    starter = nest.get("start_in_place", None)
    if starter is not None:
        starter()
        return nest

    cli_args = _build_nest_cli_args(
        nest["node_binary"], nest["port"], nest["config_path"], nest["blob_dir"],
        handle_domain=nest["handle_domain_seed"],
        bind_host=nest.get("bind_host", "127.0.0.1"),
        cors_origins=nest.get("cors_origins"),
        static_dir=nest.get("static_dir"),
    )
    # `serve_tls` must be re-applied: without it the re-spawn silently comes back
    # as plain HTTP while `nest["url"]` still says https, and the health-wait then
    # polls the wrong scheme.
    nest["proc"] = _spawn_and_wait(cli_args, nest["port"], nest["log_path"],
                                   serve_tls=nest.get("serve_tls", False),
                                   extra_env=nest.get("extra_env"))
    return nest


def resume_after_self_exit(nest, *, timeout=30.0):
    """Get a nest that EXITS ITSELF back on its feet, in this run's mode.

    The production factory-reset path is "stage the marker, reply, `exit(0)`,
    and let the supervisor restart me into the boot-time wipe"
    (`factory_reset.rs::maybe_run_factory_reset`). Who plays the supervisor is
    the one thing that differs per mode, and it differs in opposite directions:

    * **standalone** has no supervisor at all, so the harness BECOMES one —
      wait for the self-exit, then bring the same binary back on the same data
      dir, which is what runs the wipe;
    * **docker** has a real one. s6 restarts the nest inside a container that
      never went anywhere, so the harness must do NOTHING — a `proc.wait()`
      here would be waiting on the container's exit and would time out against
      a nest that came back in seconds.

    Mode-agnostic by delegation, exactly like `start_nest_in_place` above and
    for the same reason: the provider that knows how this nest is supervised
    answers the key, rather than generic code sniffing the handle for a
    container name. The fallback is the standalone act, and it is not a guess —
    a handle carrying no such key came from a fixture that spawned a local
    binary directly.

    **The witness lives in the caller, in both modes alike.** This helper is an
    ACTION, not an assertion: what proves the box came back — and came back
    wiped — is the caller's own fresh/unclaimed convergence poll
    (`_wait_nest_fresh` + `fauna.setup.status`), which is latency-independent
    and strictly stronger than "some process exited within 30 s".
    """
    resume = nest.get("resume_after_self_exit", None)
    if resume is not None:
        resume(timeout)
        return nest

    nest["proc"].wait(timeout=timeout)
    return start_nest_in_place(nest)


def stamp_schema_meta(db_path: str, schema_version: int, min_reader_version: int) -> None:
    """Force the on-disk `schema_meta` row, simulating a DB a newer nest wrote.

    The host-side half of the degraded-"needs-update" boot recipe
    (`version-compatibility.md` § 2.2): boot a fresh nest (records
    `schema_meta=(1,1)`), `stop_nest(graceful=False)`, call this to stamp a
    reader floor above the binary's `CURRENT_SCHEMA_VERSION`, then
    `start_nest_in_place` — the restarted binary detects the incompatible
    verdict and boots DEGRADED, answering the typed `fauna.nest.outdated` to
    every WS-RPC call (including the connect handshake).

    Checkpoints + truncates the WAL on both sides of the write so the restarted
    binary reads the mutated value from the main db file, not a stale WAL frame
    (mirrors the tier_4 schema-upgrade test's host-side mutation).
    """
    conn = sqlite3.connect(db_path)
    try:
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
        updated = conn.execute(
            "UPDATE schema_meta SET schema_version = ?, min_reader_version = ? WHERE id = 1",
            (schema_version, min_reader_version),
        ).rowcount
        assert updated == 1, "fresh boot should have recorded exactly one schema_meta row"
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
    finally:
        conn.close()


def read_service_intent(nest_info: dict) -> dict:
    """Read the service intent file from a running nest's data directory.

    Args:
        nest_info: The dict returned by start_nest().

    Returns:
        Dict with service flags, e.g. {"bridge": False, "dns": False, "algorithm": False}
    """
    services_path = os.path.join(nest_info["tmp_dir"], "services.json")
    with open(services_path) as f:
        data = json.load(f)
    return data["services"]


def build_macos_app() -> str:
    """Build the macOS desktop app via SPM and wrap in a .app bundle.

    Returns the path to the .app bundle.
    """
    repo = get_repo_root()
    apple_dir = repo / "apps" / "fauna-apple"

    # Build the FFI xcframework if it doesn't exist
    xcframework = apple_dir / "FaunaFFI.xcframework"
    if not xcframework.exists():
        raise RuntimeError(
            "FaunaFFI.xcframework not found. Run 'just apple-ffi' (5 slices) or "
            "'just apple-ffi-host' (the host slice alone — what mac-debug uses)"
        )

    # Build the app
    result = subprocess.run(
        ["swift", "build", "--package-path", str(apple_dir), "--product", "FaunaMacOS"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"swift build failed:\n{result.stderr}")

    # Locate the built binary
    exe = apple_dir / ".build" / "arm64-apple-macosx" / "debug" / "FaunaMacOS"
    if not exe.exists():
        raise RuntimeError(f"FaunaMacOS binary not found at {exe}")

    # Create .app bundle (SwiftUI requires a proper bundle to run)
    app_bundle = exe.parent / "Fauna.app"
    contents = app_bundle / "Contents"
    macos_dir = contents / "MacOS"
    macos_dir.mkdir(parents=True, exist_ok=True)

    # Copy Info.plist
    info_plist_src = apple_dir / "Fauna-macOS" / "Resources" / "Info.plist"
    info_plist_dst = contents / "Info.plist"
    if not info_plist_dst.exists() or info_plist_src.stat().st_mtime > info_plist_dst.stat().st_mtime:
        import shutil
        shutil.copy2(info_plist_src, info_plist_dst)

        # Inject CFBundleExecutable if missing
        plist_text = info_plist_dst.read_text()
        if "CFBundleExecutable" not in plist_text:
            plist_text = plist_text.replace(
                "<key>CFBundlePackageType</key>",
                "<key>CFBundleExecutable</key>\n  <string>FaunaMacOS</string>\n  "
                "<key>NSPrincipalClass</key>\n  <string>NSApplication</string>\n  "
                "<key>CFBundlePackageType</key>",
            )
            info_plist_dst.write_text(plist_text)

    # Symlink the executable
    exe_link = macos_dir / "FaunaMacOS"
    if exe_link.is_symlink():
        exe_link.unlink()
    exe_link.symlink_to(exe)

    # Copy Sparkle framework
    sparkle_src = exe.parent / "Sparkle.framework"
    frameworks_dir = contents / "Frameworks"
    sparkle_dst = frameworks_dir / "Sparkle.framework"
    if sparkle_src.exists() and not sparkle_dst.exists():
        frameworks_dir.mkdir(parents=True, exist_ok=True)
        import shutil
        shutil.copytree(sparkle_src, sparkle_dst, symlinks=True)

    return str(app_bundle)
