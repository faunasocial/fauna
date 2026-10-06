"""The nest-mode axis: `--nest standalone|docker[:IMAGE]|live[:URL]`.

`testing.md` § Default app and nest mode — *Mode mechanics (ratified 2026-08-01)*.
This file pins **slice 1** (seam + flag): mode resolution, the guarded-capability
nest handle, and the provider registry. Slices 2/3 add the Docker and Live
providers.

Three properties are load-bearing enough to be worth stating outright, because
each has a documented failure mode the axis exists to avoid:

  * **Standalone must pay nothing.** Mode 1 is the red-green debugging loop, so
    resolution is a pure string parse — no docker probe, no network, no import
    of anything heavy — and the standalone handle declares every capability, so
    the guard is a no-op on the default path (`testing.md:44`).

  * **A capability mismatch fails loudly, never silently.** `nest["db_path"]`
    against a mode that has no local filesystem must raise a self-diagnosing
    error naming the mode and the fix — never a bare `KeyError` (unreadable),
    and never a `.get()` that answers `None` and lets the test skip or assert
    against nothing. The hand-maintained capability table is the documented
    failure mode here (convention 7's `app_capabilities.py` lesson: a table
    that answers "not implemented" for an app it never heard of).

  * **An unbuilt mode refuses; it does not fall back.** `--nest docker` before
    the DockerProvider exists must fail the run. Falling back to standalone
    would report a green certification run that never touched the artifact.
"""

import contextlib
import pathlib
import re
import ssl
import urllib.error
import urllib.request

import pytest

from helpers import nest_mode as nm

pytestmark = pytest.mark.tier_1


# ── Mode resolution ─────────────────────────────────────────────────────────

def test_default_is_standalone_with_no_argument():
    mode = nm.resolve_nest_mode(None, environ={})
    assert mode.name == nm.STANDALONE
    assert mode.argument is None
    assert mode.is_standalone


@pytest.mark.parametrize("raw,name,argument", [
    ("standalone", nm.STANDALONE, None),
    ("docker", nm.DOCKER, None),
    ("docker:ghcr.io/faunasocial/nest:sha-abc123", nm.DOCKER,
     "ghcr.io/faunasocial/nest:sha-abc123"),
    ("live", nm.LIVE, None),
    ("live:https://example.com", nm.LIVE, "https://example.com"),
    # Surrounding whitespace is a `just`-recipe artifact, not user intent.
    ("  docker  ", nm.DOCKER, None),
])
def test_parses_mode_and_its_optional_argument(raw, name, argument):
    mode = nm.parse_nest_mode(raw)
    assert (mode.name, mode.argument) == (name, argument)


def test_a_url_argument_keeps_its_own_colons():
    """`live:https://host:8443` — split once, or the port is lost."""
    mode = nm.parse_nest_mode("live:https://box.example:8443")
    assert mode.argument == "https://box.example:8443"


def test_an_unknown_mode_names_the_valid_ones():
    with pytest.raises(nm.NestModeError) as exc:
        nm.parse_nest_mode("compose")
    message = str(exc.value)
    assert "compose" in message
    for known in nm.MODES:
        assert known in message


def test_an_empty_mode_is_rejected_rather_than_defaulted():
    """`--nest ''` is a mistake in a recipe, not a request for the default."""
    with pytest.raises(nm.NestModeError):
        nm.parse_nest_mode("")


def test_an_empty_FLAG_is_rejected_too_and_does_not_fall_through_to_the_default():
    """The absent/empty distinction, which a truthiness check silently loses.

    `--nest "$UNSET_VAR"` reaches pytest as `""`, not as an absent flag. Falling
    back to standalone there is the exact failure this axis must not have: a
    recipe that meant to request docker runs the default and reports green.
    """
    with pytest.raises(nm.NestModeError):
        nm.resolve_nest_mode("", environ={})
    # ...while a genuinely absent flag still defaults.
    assert nm.resolve_nest_mode(None, environ={}).name == nm.STANDALONE


def test_an_empty_env_var_is_ignored_rather_than_fatal():
    """`E2E_NEST=` exported empty is ambient noise, not an explicit request —
    the opposite call from the flag, because nobody types an env var per run."""
    assert nm.resolve_nest_mode(None, environ={"E2E_NEST": ""}).name == nm.STANDALONE


def test_the_env_var_is_an_override_and_the_flag_still_wins():
    assert nm.resolve_nest_mode(None, environ={"E2E_NEST": "docker"}).name == nm.DOCKER
    # An explicit flag beats the environment — same precedence as `--app`.
    chosen = nm.resolve_nest_mode("live", environ={"E2E_NEST": "docker"})
    assert chosen.name == nm.LIVE


def test_the_test_id_stamp_is_the_bare_mode_name():
    """`test_x[tui-docker]`, never `test_x[tui-docker:ghcr.io/...]`.

    The stamp exists for per-mode distinguishability in logs and flake history
    (`testing.md:44`); an image ref in a node id would make that id unstable
    across runs and unusable as a baseline key.
    """
    mode = nm.parse_nest_mode("docker:ghcr.io/faunasocial/nest:sha-abc123")
    assert mode.id == "docker"
    assert str(mode) == "docker"


def test_the_default_mode_is_not_stamped_so_default_path_ids_never_churn():
    """`[tui]` means standalone; `[tui-docker]` means docker.

    Stamping the default would rename ~1400 node ids, including 1001 of the
    1936 keys in `baselines/apple-baseline.json` — a gate that classifies a
    changed id as added+removed and never as `NEW RED`, so a real regression in
    that window reads as a benign addition. `conftest.pytest_generate_tests`
    holds the decision and the full reasoning; this pins the predicate it uses,
    which is the part a later refactor could quietly drop.
    """
    assert nm.parse_nest_mode("standalone").is_standalone
    assert not nm.parse_nest_mode("docker").is_standalone
    assert not nm.parse_nest_mode("live").is_standalone


# ── The guarded-capability nest handle ──────────────────────────────────────

def _handle(absent=()):
    return nm.NestHandle(
        {"url": "http://127.0.0.1:1", "port": 1, "db_path": "/tmp/x.db",
         "proc": object(), "admin": {"signing_key": b""}},
        mode=nm.parse_nest_mode("live"),
        absent=absent,
    )


def test_a_present_capability_reads_exactly_like_a_dict():
    handle = _handle()
    assert handle["port"] == 1
    assert handle.get("port") == 1
    assert "port" in handle
    # A key that was never a declared capability keeps plain-dict semantics —
    # the guard must not change how `nest.get("reclaim_cycled")` behaves.
    assert handle.get("reclaim_cycled") is None
    with pytest.raises(KeyError):
        handle["no_such_key"]


def test_an_absent_capability_raises_a_self_diagnosing_error():
    handle = _handle(absent=["db_path"])
    with pytest.raises(nm.NestCapabilityError) as exc:
        handle["db_path"]
    message = str(exc.value)
    assert "db_path" in message
    assert "live" in message          # which mode could not answer
    assert nm.MODE_MARKERS[nm.LIVE] in message   # and the marker that excludes it


def test_an_absent_capability_is_not_a_bare_KeyError():
    """A `KeyError('db_path')` in a 900-line traceback diagnoses nothing, and
    `except KeyError` around a capability probe would swallow it entirely."""
    handle = _handle(absent=["db_path"])
    with pytest.raises(nm.NestCapabilityError):
        handle["db_path"]
    assert not issubclass(nm.NestCapabilityError, KeyError)


def test_get_on_an_absent_capability_raises_rather_than_answering_None():
    """The silent-skip shape: `if nest.get("db_path"): ...` would quietly do
    nothing on live, and the test would pass having asserted nothing."""
    handle = _handle(absent=["db_path"])
    with pytest.raises(nm.NestCapabilityError):
        handle.get("db_path")


def test_get_with_an_explicit_default_is_a_deliberate_probe_and_is_allowed():
    handle = _handle(absent=["db_path"])
    assert handle.get("db_path", None) is None
    assert handle.get("db_path", "fallback") == "fallback"


def test_membership_answers_False_so_a_test_can_branch_on_the_capability():
    handle = _handle(absent=["db_path"])
    assert "db_path" not in handle
    assert "port" in handle


def test_standalone_declares_every_capability_so_the_guard_is_a_no_op():
    """Zero behavior change on the default path — the whole point of slice 1."""
    assert nm.absent_capabilities(nm.parse_nest_mode("standalone")) == frozenset()


# ── The provider registry ───────────────────────────────────────────────────

def test_standalone_has_a_provider():
    assert nm.provider_for(nm.parse_nest_mode("standalone")) is not None


def test_docker_has_a_provider():
    """Slice 2."""
    assert nm.provider_for(nm.parse_nest_mode("docker")) is not None


def test_a_mode_with_no_provider_refuses_the_run_and_never_falls_back():
    """All three modes ship providers as of slice 3, so this drives the refusal
    with an unregistered mode rather than a real one — the behaviour it pins is
    what happens to the NEXT mode added, and it must never be a silent fallback
    to standalone (which would report a green certification run against an
    artifact it never started)."""
    ghost = nm.NestMode("staging")
    with pytest.raises(nm.NestModeError) as exc:
        nm.provider_for(ghost)
    message = str(exc.value)
    assert "staging" in message
    assert "not built" in message.lower()
    # ...and it names the modes that DO work, so the reader's next step is in
    # the message rather than in a doc.
    for built in nm.MODES:
        assert built in message


# ── Docker's declared capabilities (slice 2) ────────────────────────────────

def test_docker_keeps_the_state_capabilities_and_drops_the_local_process_ones():
    """The bind-mounted `/data` is what makes this split real: `db_path` and
    friends survive the mode switch (SQLite-poking fixtures keep working), while
    the things that only exist for a locally-spawned binary do not."""
    absent = nm.absent_capabilities(nm.parse_nest_mode("docker"))
    assert absent == frozenset({"config_path", "node_binary"})
    for kept in ("db_path", "blob_dir", "tmp_dir", "proc", "log_path"):
        assert kept not in absent, f"{kept} is a real docker capability"


def test_docker_answers_log_path_because_the_absence_was_MISFILED():
    """`log_path` was a declared docker absence until 2026-09-02, and the entry
    was in the wrong table: `DOCKER_ABSENT`'s own contract is *a fact about the
    image, not a harness limitation to be lifted later*, and this one was the
    second kind wearing the first one's clothes.

    Its stated reason — "the nest logs to the container's stdout under s6, read
    with `docker logs`, not to a file the harness owns" — describes the
    TRANSPORT and, read closely, names the door rather than a wall. Measured
    against `ghcr.io/faunasocial/nest:latest` before the lift: the container log
    carries `fauna_nest` tracing at DEBUG when `RUST_LOG` asks for it (the
    catalogued `extra_env` key), which is exactly the level the dispatch beacon
    `NestLogWatch` waits on is emitted at. The lines were always there; what was
    missing was a harness that opened the door.

    The two survivors are the genuine article and stay: `config_path` (the
    entrypoint authors the config itself — there is no file to point at) and
    `node_binary` (it lives in the image; re-spawning a local one would be
    testing standalone while reporting docker).
    """
    assert "log_path" not in nm.absent_capabilities(nm.parse_nest_mode("docker"))
    # ...and the reason table must not keep a dangling entry, or
    # `test_every_docker_absence_carries_a_reason` would be grading a key no
    # mode declares.
    assert "log_path" not in nm.DOCKER_ABSENT
    # Live still declares it, and for the reason that makes it a real absence
    # there: the nest is on another machine, so there is no `docker logs` — or
    # any other local door — to run at all.
    assert "log_path" in nm.absent_capabilities(nm.parse_nest_mode("live"))


def test_every_docker_absence_carries_a_reason():
    """A capability error that says only *that* sends the reader hunting; the
    reason is what lets them decide between dropping the dependency and marking
    the test."""
    for key in nm.absent_capabilities(nm.parse_nest_mode("docker")):
        assert nm.DOCKER_ABSENT[key].strip(), f"{key} needs a stated reason"


def test_a_tls_serving_handle_registers_its_port_so_port_keyed_dials_use_https():
    """A mode whose nest serves TLS must say so in the ONE place the port→scheme
    fact lives (`common.auth.mark_tls_nest`), or every helper keyed by *port*
    rather than by URL dials plain `ws://`/`http://` at a TLS listener.

    This is not hypothetical: it is why the nest-mode axis could never record a
    docker run. `_DockerProvider` hands back `url: https://127.0.0.1:<port>` and
    `serve_tls: True` (the image synthesizes a self-signed floor cert at boot and
    has no plain-HTTP posture), but nothing registered the port — so
    `_apply_r14_trust_env`'s `ws_api.nest_info(port)` resolved to `http://` via
    `port_base_url`, the TLS listener closed the connection, and EVERY app-fixture
    test errored at setup before its first assertion. Measured 2026-08-28: the
    whole fleet ledger held 0 docker records and 1158 standalone ones.

    Pinned on the handle rather than on the provider so it holds for any future
    TLS-serving mode, and so it needs no docker daemon to run.
    """
    from common.auth import port_base_url

    port = 59_231  # not dialled; only the scheme resolution is under test
    assert port_base_url(port) == f"http://127.0.0.1:{port}", "unmarked default"

    conftest = pytest.importorskip("conftest")
    conftest._as_nest_handle(
        {"url": f"https://127.0.0.1:{port}", "port": port, "serve_tls": True},
    )

    assert port_base_url(port) == f"https://127.0.0.1:{port}", (
        "a handle declaring serve_tls must register its port with "
        "mark_tls_nest, or port-keyed dials speak plain HTTP to a TLS listener"
    )


def test_a_plain_http_handle_does_not_register_its_port():
    """The converse, so the registration cannot creep onto the default path: a
    standalone nest serves plain HTTP (`FAUNA_INSECURE_DISABLE_TLS`) and must stay
    `http://` — marking it would break every tier_3 dial in the suite."""
    from common.auth import port_base_url

    port = 59_232
    conftest = pytest.importorskip("conftest")
    conftest._as_nest_handle({"url": f"http://127.0.0.1:{port}", "port": port})

    assert port_base_url(port) == f"http://127.0.0.1:{port}"


def test_a_live_handle_registers_its_host_so_port_keyed_dials_reach_the_box():
    """The port→scheme fact is not enough on its own: a live box is reached at a
    real HOST, and a helper keyed by port alone used to build
    `https://127.0.0.1:443` for it.

    Measured 2026-10-04 against `dev.example.com`:
    all 19 tests of `test_account_instance_lock_tui.py` and
    `test_account_switcher_tui.py` died at account seeding with
    `ConnectionRefusedError`, because `create_actor_and_register(port, …)` with
    no `base_url` resolved through `port_base_url(443)` to loopback. The handle
    records the authority beside the scheme, so every port-keyed dial
    (`_resolve_base`, `ws_api._base_url`, the blob helpers) reaches the box.

    The host is recorded, never trusted: a non-loopback authority is still no
    floor-cert authority, so the real box stays certificate-verified (the
    opener's and `ws_sslopt`'s conjunction is untouched).
    """
    from common import auth
    from tests.api import ws_api

    port = 59_234  # not dialled; only the resolution is under test
    conftest = pytest.importorskip("conftest")
    try:
        conftest._as_nest_handle(
            {"url": "https://box.example", "port": port, "serve_tls": True},
        )
        assert auth.port_base_url(port) == f"https://box.example:{port}", (
            "a handle must register its host, or a port-keyed dial reaches loopback"
        )
        assert auth._resolve_base(port, None)[0] == f"https://box.example:{port}"
        assert ws_api._base_url(port) == f"https://box.example:{port}"
        assert auth.port_base_url(port, "10.0.0.9") == f"https://10.0.0.9:{port}", (
            "an explicit host still wins"
        )
        assert not auth._is_floor_cert_authority(f"box.example:{port}"), (
            "a registered real box must stay certificate-verified"
        )

        # A later loopback nest on the same port takes the authority back.
        conftest._as_nest_handle({"url": f"http://127.0.0.1:{port}", "port": port})
        auth.unmark_tls_nest(port)
        assert auth.port_base_url(port) == f"http://127.0.0.1:{port}"
    finally:
        auth.unmark_tls_nest(port)
        auth.forget_nest_authority(port)


class _SpawnedStandIn:
    """What `_spawn_and_wait` needs of the process it spawned, and nothing more."""

    pid = 0


def test_a_plain_nest_on_a_port_a_tls_nest_used_is_dialled_plain(monkeypatch, tmp_path):
    """A port outlives the nest on it: `find_free_port` hands a port back once its
    nest has stopped, so the port→scheme fact must follow the nest that serves
    the port NOW, not the first one that ever did.

    Measured in a whole-suite linux sweep (2026-09-22): `box-recovery-b` served
    TLS on port 53843 and marked it; four minutes later a plain-HTTP dedicated
    mail nest was spawned on 53843, and its own admin claim — dialled through
    `port_base_url` — opened TLS against a plaintext listener and died with
    `ssl.SSLError: RECORD_LAYER_FAILURE` at fixture setup.
    """
    from common import auth
    from common import nest as nest_mod

    import drivers.port_util as port_util

    port = 59_233
    auth.mark_tls_nest(port)  # the earlier, TLS-serving tenant of this port
    monkeypatch.setattr(nest_mod.subprocess, "Popen", lambda *a, **k: _SpawnedStandIn())
    monkeypatch.setattr(nest_mod, "wait_for_node", lambda *a, **k: None)
    monkeypatch.setattr(port_util, "track_process", lambda proc: None)
    try:
        nest_mod._spawn_and_wait(["fauna-nest"], port, str(tmp_path / "nest.log"), serve_tls=False)
        assert auth.port_base_url(port) == f"http://127.0.0.1:{port}", (
            "a plain-HTTP nest spawned on a port a TLS nest used must clear the "
            "stale mark, or its own claim dials TLS at a plaintext listener"
        )

        nest_mod._spawn_and_wait(["fauna-nest"], port, str(tmp_path / "nest.log"), serve_tls=True)
        assert auth.port_base_url(port) == f"https://127.0.0.1:{port}", (
            "a TLS nest spawned on a port must mark it before its own claim dials"
        )
    finally:
        auth.unmark_tls_nest(port)


def test_every_port_keyed_api_helper_resolves_its_scheme_from_the_one_place():
    """The two pins above prove the port→scheme fact is REGISTERED. This one
    proves the helpers actually READ it — which is a different claim, and the
    half that was missing.

    `common.auth.port_base_url` is documented as "the one place the port→scheme
    fact lives", and `tests/api/ws_api.py::_base_url` is simply a call to it. But
    a helper is free to spell its own URL instead, and one did: `conv_api`'s
    `_base_url(port, scheme="http")` hard-defaulted to plaintext and made every
    caller responsible for remembering a keyword. Against a TLS-serving nest the
    forgotten keyword sends `ws://` at a TLS listener, which drops the connection
    **below any application logging** — so the nest's own log is clean, and the
    failure surfaces in the harness as a bare
    `WebSocketConnectionClosedException: Connection to remote host was lost.`
    raised out of the handshake, naming nothing.

    Measured 2026-08-30: `test_custody_ceremony_two_accounts` and
    `test_fauna_mls_two_client_inbox_drain` both ERRORed at setup that way under
    `--nest docker` in 19 seconds, while passing in standalone, because
    `_launch_second_real_faunamls_app` calls `conv_api.accept_contact(port, ...)`
    with no `scheme=`. The nest was healthy throughout; its whole WARN inventory
    was ACME/GC noise.

    Derived from the package rather than hand-listed (the lesson
    `test_dedicated_mail_nest_consumers_rebind.py` records): a third helper
    module that grows its own `_base_url` is covered the day it appears.

    **Complementary to `test_no_two_nest_consumer_composes_a_nest_url_by_hand`
    below, which `conv_api` fell through three ways at once**: that pin reads
    only `test_*.py` files (never a helper module), only modules consuming
    `two_nodes`, and matches only a *literal* `"://127.0.0.1:"` prefix — so a
    scheme carried in an interpolation (`f"{scheme}://127.0.0.1:{port}"`) is
    invisible to it. Its own docstring says widening it is the next family's
    job; this is that widening, done from the other side. A textual scan asks
    "is a scheme spelled here?"; this asks "what does the helper actually RETURN
    for a TLS port?", which no spelling can fake.
    """
    import importlib
    import pkgutil

    from common.auth import port_base_url

    import tests.api as api_pkg

    port = 59_233
    conftest = pytest.importorskip("conftest")
    conftest._as_nest_handle(
        {"url": f"https://127.0.0.1:{port}", "port": port, "serve_tls": True},
    )
    assert port_base_url(port) == f"https://127.0.0.1:{port}", "precondition"

    checked = []
    for mod_info in pkgutil.iter_modules(api_pkg.__path__):
        if mod_info.name.startswith("test_") or mod_info.name == "conftest":
            continue
        mod = importlib.import_module(f"tests.api.{mod_info.name}")
        base = getattr(mod, "_base_url", None)
        if base is None:
            continue
        checked.append(mod_info.name)
        assert base(port) == f"https://127.0.0.1:{port}", (
            f"tests/api/{mod_info.name}.py::_base_url spells its own scheme "
            f"instead of reading common.auth.port_base_url, so a port-keyed "
            f"dial speaks plain HTTP to a TLS listener — got {base(port)!r}. "
            "The listener drops such a connection with no HTTP response, which "
            "reaches the test as a bare WebSocketConnectionClosedException from "
            "the WS handshake, with nothing in the nest's log."
        )

    assert checked, (
        "the derivation found no port-keyed API helper at all — it has gone "
        "vacuous (a rename or a package move); fix the derivation, do not "
        "delete the test"
    )


def test_no_port_keyed_dial_hard_codes_a_plaintext_scheme():
    """The pin above proves the helper's DEFAULT reads the one place. This one
    proves no caller overrides it back to plaintext — a different claim, and the
    half that was missing.

    When `conv_api._base_url`'s plaintext default was removed on 2026-08-30, the
    audit that accompanied it concluded "every one of these call sites passes no
    `scheme=`". It was run as a `scheme=` search, so it could not see a
    **positional** argument — and one call site had one:
    `test_mls_channels.py::test_commit_upload_auto_registers_a_non_member_sender`
    dialled `conv_api._base_url(port_a, "http")`.

    The cost of that blind spot is the reason this pin exists. The test failed
    under `--nest docker` with the bare
    `WebSocketConnectionClosedException: Connection to remote host was lost.`
    the docstring above describes, and because it was one of six `two_nodes`
    failures in a 203-test slice it was read as a *fleet* symptom: an
    accumulating host resource, chased across three passes through docker-proxy,
    ephemeral ports, fd counts and the per-IP connection ceiling. The
    "dose-response curve" that framing rested on compared *different tests* —
    the solo cell was `test_dm_roundtrip`, which genuinely does pass alone. Run
    alone, the actual failing test fails in 22 seconds.

    So the rule is enforced on the SPELLING, where the defect lives, and at
    every level: a literal plaintext scheme handed to a port-keyed helper, as a
    positional or as a keyword, in a test module or a helper module. A dial that
    truly must pin plaintext (a stub that is not a nest) names a non-`scheme`
    argument or builds its own URL, and says why.
    """
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    offenders = []
    for source in sorted(root.rglob("*.py")):
        for node in ast.walk(ast.parse(source.read_text())):
            if not isinstance(node, ast.Call):
                continue
            target = node.func
            name = (
                target.attr if isinstance(target, ast.Attribute)
                else target.id if isinstance(target, ast.Name)
                else None
            )
            if name is None:
                continue
            # A port-keyed base-URL helper takes the scheme second; anything
            # else must name it, so both spellings are read.
            candidates = [node.args[1]] if (
                name.endswith("_base_url") and len(node.args) >= 2
            ) else []
            candidates += [kw.value for kw in node.keywords if kw.arg == "scheme"]
            for arg in candidates:
                if isinstance(arg, ast.Constant) and arg.value == "http":
                    rel = source.relative_to(root).as_posix()
                    offenders.append(f"{rel}:{node.lineno} — {name}(..., 'http')")

    assert not offenders, (
        "a port-keyed dial hard-codes the plaintext scheme, so it speaks HTTP "
        "to a TLS listener under `--nest docker` and the connection is dropped "
        "below any application logging:\n  " + "\n  ".join(offenders) + "\n"
        "Drop the argument and let `common.auth.port_base_url` resolve it."
    )


def test_a_docker_capability_error_states_the_reason_not_just_the_key():
    handle = nm.NestHandle(
        {"url": "https://127.0.0.1:1"}, nm.parse_nest_mode("docker"),
        absent=nm.absent_capabilities(nm.parse_nest_mode("docker")),
    )
    with pytest.raises(nm.NestCapabilityError) as exc:
        handle["node_binary"]
    message = str(exc.value)
    assert "node_binary" in message
    assert "inside the image" in message
    assert "docker_only" in message, "the message must name the marker that admits it"


# ── Classified exclusion (slice 2) ──────────────────────────────────────────

def test_the_local_nest_fixture_list_matches_conftest():
    """`LOCAL_NEST_FIXTURES` is the module's one hand-written table, so it is
    pinned to conftest's actual AST rather than trusted.

    Transitive on purpose: seven of these reach `_make_nest` through
    `_dedicated_mail_nest_impl` / `_bench_mda_impl`, and the first hand-read of
    the direct call sites missed every one of them.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    source = Path(__file__).resolve().parents[1] / "conftest.py"
    tree = ast.parse(source.read_text())
    funcs = {
        n.name: n for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }

    def called_names(node):
        out = set()
        for sub in ast.walk(node):
            if isinstance(sub, ast.Call):
                target = sub.func
                if isinstance(target, ast.Name):
                    out.add(target.id)
                elif isinstance(target, ast.Attribute):
                    out.add(target.attr)
        return out

    def is_fixture(node):
        return any("fixture" in ast.unparse(d) for d in node.decorator_list)

    # Rooted at `_make_nest` ALONE, unlike the options table below: this list
    # answers "does this fixture spawn a LOCAL binary", and a fixture routed
    # through the provider seam no longer does — which is precisely what routing
    # it was for.
    reaches = {n for n, f in funcs.items() if "_make_nest" in called_names(f)}
    grew = True
    while grew:
        grew = False
        for name, node in funcs.items():
            if name not in reaches and called_names(node) & reaches:
                reaches.add(name)
                grew = True

    derived = {n for n in reaches if is_fixture(funcs[n])}
    assert derived == set(ns.LOCAL_NEST_FIXTURES), (
        "conftest's dedicated-nest fixtures and nest_surface.LOCAL_NEST_FIXTURES "
        "have diverged.\n"
        f"  only in conftest: {sorted(derived - set(ns.LOCAL_NEST_FIXTURES))}\n"
        f"  only in the list: {sorted(set(ns.LOCAL_NEST_FIXTURES) - derived)}\n"
        "A fixture that spawns a local fauna-nest binary is standalone-only by "
        "construction — in a docker run it would serve a local binary while the "
        "report said 'docker'. Add it to LOCAL_NEST_FIXTURES."
    )


#: Conftest fixtures that spawn their nest by calling `common.nest.start_nest`
#: DIRECTLY, bypassing both nest-start entry points — and the reason each has to.
#:
#: This third shape is why "arm 1's conftest half" needed a definition rather than
#: a `_make_nest` grep. A fixture on this path is invisible to *both* AST pins —
#: `LOCAL_NEST_FIXTURES` roots at `_make_nest` and `FIXTURE_START_OPTIONS` roots at
#: the two entry points — so it neither shows as standalone-only nor declares the
#: options it passes. Four fixtures sat here; `handled_nest` was one of them, the
#: most widely shared dedicated nest in the suite, asking for nothing at all and
#: therefore routable all along. It also meant the guarded-capability wrapper
#: `_as_nest_handle` had a way around it.
#:
#: ⚠ **The three IP-literal ones LEFT this record on 2026-09-02, and the reason
#: they were in it was a false premise worth keeping written down.** It said: the
#: nest's own handle domain IS its port (`127.0.0.1:<port>`), so the authority has
#: to be composed BEFORE the start, and both entry points allocate the port
#: internally on purpose — "routing them would take a way to ask a provider for a
#: nest whose authority is known in advance". The mistake is in the last clause.
#: The authority never had to be known in ADVANCE; it only had to be known to
#: whoever composes it, and that is the nest, not the caller. `handle_domain_seed
#: = common.nest.OWN_DIAL_AUTHORITY` says *advertise whatever authority a client
#: dials to reach you*, and `start_nest` resolves it from the same `dial_host`
#: and `port` its own `url` is composed from. The caller stops needing the port
#: at all, so it stops needing to allocate one, so it can let a provider start
#: the nest — which is the whole of what kept these four in this hole.
#:
#: Two lessons, because the shape recurs. **(a) A "structural reason" that names
#: an ORDERING is usually a data-flow one wearing a costume** — "X must exist
#: before Y" invites a check of who actually needs X, and here only the nest did.
#: **(b) Being right about the destination hid being wrong about the road**: the
#: entry was correct that all three are permanent class (4) residents (an
#: IP-literal authority is a `handle_domain_seed`, which no provider but
#: standalone honours), and that correct conclusion made its incorrect premise
#: comfortable to leave alone for a month. Routing them changes no test's mode —
#: it changes the REASON the audit gives from the MIXED `nest_binary` closure to
#: the named option, which is the difference between seven residents counted as
#: outstanding work and seven counted as settled.
#:
#: `unclaimed_caldav_nest` is here for a different and stronger reason: it is
#: already class (5) (`BRIDGE_SPAWN_FIXTURES`), so unlike the four this test was
#: written to expose it was never invisible — the bridge class grades it whether
#: or not it routes. It is recorded rather than exempted because the pin asks a
#: structural question ("did this bypass both entry points?"), and answering it
#: with "yes, and here is why that is harmless" is the honest shape.
#: ⚠ The keys are `<conftest path relative to tests/e2e-unified>::<fixture>`, and
#: the qualification is not cosmetic. This pin scanned only the ROOT conftest until
#: 2026-08-30, so a fixture one directory down was invisible to it for exactly the
#: reason its own docstring gives — and `tests/api/conftest.py::two_nodes`, the
#: single most-consumed second-nest fixture in the suite (18 consumer files), sat
#: in that blind spot the whole time, duplicated verbatim in
#: `tests/platform/conftest.py`. A pin that grades one directory is a pin that
#: teaches the next author to add the fixture to another.
_DIRECT_START_NEST_FIXTURES = {
    "conftest.py::unclaimed_caldav_nest": (
        "three per-variant nests in one fixture, one of them an IP-literal "
        "authority; already class (5) — it spawns a host MDA"
    ),
}


def test_no_conftest_fixture_spawns_a_nest_behind_both_entry_points():
    """A conftest fixture may only call `common.nest.start_nest` directly for a
    reason recorded in `_DIRECT_START_NEST_FIXTURES` — never by habit.

    The two AST pins above are each rooted at a nest-start entry point, so a
    fixture that reaches `start_nest` around both is graded by neither: it does
    not appear standalone-only and it declares no start options. That is a hole
    exactly one shape wide, and it had four fixtures in it.

    **Every conftest in the tree is scanned, not just the root one.** pytest
    composes fixtures from every `conftest.py` on the path to a test, so a
    sub-directory conftest is the same hole with a narrower audience — and it is
    where the largest instance actually lived.
    """
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    funcs = {}
    for source in sorted(root.rglob("conftest.py")):
        rel = source.relative_to(root).as_posix()
        tree = ast.parse(source.read_text())
        for n in tree.body:
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)):
                funcs[f"{rel}::{n.name}"] = n

    def calls_start_nest(node):
        for sub in ast.walk(node):
            if isinstance(sub, ast.Call):
                target = sub.func
                name = (
                    target.id if isinstance(target, ast.Name)
                    else target.attr if isinstance(target, ast.Attribute)
                    else None
                )
                if name == "start_nest":
                    return True
        return False

    def is_fixture(node):
        return any("fixture" in ast.unparse(d) for d in node.decorator_list)

    # `_make_nest` is the entry point itself, not a bypass of it.
    direct = {
        name for name, node in funcs.items()
        if not name.endswith("::_make_nest")
        and is_fixture(node) and calls_start_nest(node)
    }
    assert direct == set(_DIRECT_START_NEST_FIXTURES), (
        "conftest's direct `start_nest` callers and the recorded set have "
        "diverged.\n"
        f"  only in conftest: {sorted(direct - set(_DIRECT_START_NEST_FIXTURES))}\n"
        f"  only in the record: {sorted(set(_DIRECT_START_NEST_FIXTURES) - direct)}\n"
        "A fixture here is graded by NEITHER AST pin — it is not in "
        "LOCAL_NEST_FIXTURES and it declares no start options — so it spawns a "
        "local binary that nothing in the mode axis can see. Route it through "
        "`_start_dedicated_nest` unless it genuinely cannot, and if it cannot, "
        "record the reason here."
    )


#: Modules whose hand-composed loopback authorities are REVIEWED as not naming a
#: nest's API listener — so the port→scheme fact they spell is not the one
#: `common.auth.port_base_url` owns, and the pin below would be flagging the
#: wrong thing rather than catching a defect.
#:
#: A reviewed map rather than a smarter predicate, and deliberately so: nothing
#: in the syntax distinguishes a nest's authority from a test-owned server's, so
#: any rule that tried would be inferring intent from a variable called `port`.
#: The same shape as `_PEER_FIELDS_REVIEWED_AS_CLIENT_DIALS` above and for the
#: same reason.
_COMPOSED_AUTHORITY_IS_NOT_A_NEST = {
    "tests/api/test_activitypub_federation.py":
        "a STUB remote fediverse actor this test serves itself "
        "(`actor_uri`/`inbox_url` on its own HTTP server), not a fauna nest",
    "tests/test_box_recovery_two_nest.py":
        "`_FirstConnectionRefusingProxy` — a proxy the test owns and puts in "
        "front of the nest; its scheme is the proxy's fact, not the nest's",
    "tests/test_caldav_admin_port_rebind.py":
        "the nest's CalDAV listener, which serves TLS in every mode — and the "
        "test is standalone-only anyway, because s6 owns the rebind in the "
        "image (testing.md § Default app and nest mode, arm 6)",
    "tests/test_caldav_autoschedule_mailbox_less.py":
        "the mail bridge's own CalDAV listener (`handle.caldav_port`, the "
        "MDA's self-signed HTTPS endpoint the stock organizer client dials) — "
        "a bridge authority, never the nest's API listener",
    "tests/test_carddav_nest_outcomes.py":
        "the mail bridge's own DAV listener (`handle.caldav_port` / "
        "`venue.caldav_port`, the MDA's self-signed HTTPS endpoint a stock "
        "contacts app dials) — a bridge authority, never the nest's API "
        "listener, which the module reaches only as `nest['url']`",
    "tests/api/test_backup_lease_handback.py":
        "a synthetic `destination_nest_url` on a `find_free_port()` port "
        "nothing serves — deliberately unreachable so the sweep fails to "
        "CONNECT, never a fixture's own nest authority",
}


def test_no_routed_nest_consumer_composes_a_nest_url_by_hand():
    """A module whose nest comes from the mode provider may not spell that
    nest's authority itself.

    This is the pin that makes routing a fixture *safe* rather than merely
    possible, and the distinction was the whole finding of the `two_nodes`
    slice. The row that commissioned the work recorded the blocker as "53 bare
    port reads must become threaded `base_url`s"; measured, the bare port is
    fine and the count is not the risk. `common.auth.port_base_url` is the ONE
    place the port→scheme fact lives, and `_as_nest_handle` registers a
    `serve_tls` nest's port there — so every helper that takes a bare port (116
    such reads in that family alone) already dials `https://` against a docker
    container with no edit at all.

    What breaks is the *hand-composed* URL: an `f"http://127.0.0.1:{port}"` is a
    hard-coded scheme, and against a container that serves only TLS it fails —
    the "red against working product code" outcome the arm ordering exists to
    avoid.

    **Scope widened 2026-09-02** from the `two_nodes` consumers to the consumers
    of every provider-routed dedicated-nest fixture, which its own docstring had
    flagged as the next family's job. The widening cost six reviewed sites, not
    a sweep: one real offender (`test_web_authoring.py`'s blob upload, fixed in
    the same commit) and three modules whose composed authorities name something
    other than a nest API listener, mapped above with the reason. Modules under
    `tests/platform/docker/` are outside the scope by construction: that venue
    brings its OWN image through `docker_build` rather than the mode provider,
    so its authorities are not the mode axis's to grade.
    """
    import ast
    from pathlib import Path

    routed = {q.split("::", 1)[1] for q in _PROVIDER_ROUTED_MODULE_LOCAL_NESTS}
    routed |= set(_PROVIDER_ROUTED_NESTS) | {"two_nodes"}

    root = Path(__file__).resolve().parents[1] / "tests"
    offenders = []
    for source in sorted(root.rglob("test_*.py")):
        rel = source.relative_to(root.parent).as_posix()
        if source.name == "test_nest_mode_axis.py":
            continue
        if "tests/platform/docker/" in rel:
            continue
        if rel in _COMPOSED_AUTHORITY_IS_NOT_A_NEST:
            continue
        tree = ast.parse(source.read_text())
        consumes = any(
            isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
            and any(a.arg in routed for a in n.args.args)
            for n in ast.walk(tree)
        )
        if not consumes:
            continue
        for node in ast.walk(tree):
            if not isinstance(node, ast.JoinedStr):
                continue
            parts = node.values
            for i, part in enumerate(parts[:-1]):
                if not isinstance(part, ast.Constant):
                    continue
                # A literal ending in an authority, immediately interpolated:
                # `f"http://127.0.0.1:{port}"`. The interpolation is what makes
                # it a composed nest URL rather than a constant peer address.
                if not isinstance(parts[i + 1], ast.FormattedValue):
                    continue
                text = str(part.value)
                if text.rstrip().endswith(("://127.0.0.1:", "://localhost:")):
                    offenders.append(f"{rel}:{node.lineno}")

    assert offenders == [], (
        "a provider-routed nest's consumer composes a nest URL by hand:\n  "
        + "\n  ".join(sorted(set(offenders)))
        + "\n\nThe scheme is not the caller's to spell. Use "
        "`common.auth.port_base_url(port)` — the one place the port→scheme fact "
        "lives — or the handle's own `nest_a['url']`. A hard-coded `http://` "
        "dials plain text at a docker nest, which serves only TLS, and the test "
        "reds against working product code (testing.md § Default app and nest "
        "mode, ruling (1)). If the authority is NOT a nest's API listener, say "
        "so in _COMPOSED_AUTHORITY_IS_NOT_A_NEST with the reason."
    )


def test_the_composed_authority_exemptions_all_still_apply():
    """The reviewed map's own staleness guard, mirroring the peer-field list's.

    An exemption that no longer names a real module, or names one that has
    stopped composing anything, is a licence nobody is using — and the next
    reader would take it as evidence the shape is fine there."""
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1] / "tests"
    for rel in sorted(_COMPOSED_AUTHORITY_IS_NOT_A_NEST):
        source = root.parent / rel
        assert source.exists(), f"exempted module no longer exists: {rel}"
        tree = ast.parse(source.read_text())
        composes = any(
            isinstance(n, ast.Constant)
            and str(n.value).rstrip().endswith(("://127.0.0.1:", "://localhost:"))
            for node in ast.walk(tree)
            if isinstance(node, ast.JoinedStr)
            for n in node.values
        )
        assert composes, (
            f"{rel} no longer composes a loopback authority — drop its "
            "exemption, so the map keeps meaning what it says"
        )


def test_every_standalone_only_conftest_fixture_has_a_structural_reason():
    """Arm 1's finish line, made enforceable: a conftest dedicated-nest fixture
    may only still spawn a LOCAL binary if something about its SHAPE stops it
    routing — never merely because nobody has routed it yet.

    Two structural reasons exist, and this derives both from the tree rather
    than reading them off the comment beside the list:

    * **It spawns a host bridge.** The nest is the easy half of a mail fixture;
      the MTA/MDA beside it are exclusion class (5), and the docker shape of a
      mail nest is the image's own s6 bridges enabled through the product's
      toggle (`testing.md` § Default app and nest mode, ruling (3)) — a build,
      not a routing edit.
    * **It asks for an option docker cannot honour.** Today that is
      `handle_domain_seed`, the `--handle-domain` boot flag. Routing such a
      fixture would trade a clean collection-time exclusion for a setup-time
      `NestModeError` from the provider's backstop — strictly worse, and the
      outcome the arm ordering exists to avoid.

    Anything else in `LOCAL_NEST_FIXTURES` is a fixture whose options every
    provider already honours and which is therefore one edit from collecting in
    a container. That is the state this test refuses to let return: before arm
    1's conftest half finished, four such fixtures sat in the list
    (`atproto_hosted_nest`, `atproto_localhost_nest`, `web_hosting_nest`,
    `registration_posture_nest`), indistinguishable — to any reader, and to the
    mode audit — from the seven that belong there.

    It also guards the direction nobody watches: a NEW dedicated-nest fixture
    written against `_make_nest` out of habit. The existing AST pin would happily
    accept it into the list; this one asks whether it had any business being
    there.
    """
    import ast
    from pathlib import Path

    import conftest
    from helpers import nest_surface as ns

    source = Path(__file__).resolve().parents[1] / "conftest.py"
    tree = ast.parse(source.read_text())
    funcs = {
        n.name: n for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }

    def called_names(node):
        out = set()
        for sub in ast.walk(node):
            if isinstance(sub, ast.Call):
                target = sub.func
                if isinstance(target, ast.Name):
                    out.add(target.id)
                elif isinstance(target, ast.Attribute):
                    out.add(target.attr)
        return out

    # Transitive, for the same reason the list's own pin is: every one of these
    # fixtures reaches its bridge spawn through a private impl helper, never
    # directly.
    spawners = {"_spawn_mta_bridge", "_spawn_mda_bridge"}
    reaches_bridge = {n for n, f in funcs.items() if called_names(f) & spawners}
    grew = True
    while grew:
        grew = False
        for name, node in funcs.items():
            if name not in reaches_bridge and called_names(node) & reaches_bridge:
                reaches_bridge.add(name)
                grew = True

    # Docker is the comparator, not the intersection over all three providers:
    # live declares no options because it will not START a nest at all, so
    # intersecting with it would call every option unhonourable and excuse
    # everything. What routing buys is collection in a container, so what decides
    # is what a container can honour.
    honourable = conftest._DockerProvider().supported_options

    unexplained = sorted(
        name for name in ns.LOCAL_NEST_FIXTURES
        if name not in reaches_bridge
        and ns.FIXTURE_START_OPTIONS.get(name, frozenset()) <= honourable
    )
    assert not unexplained, (
        "these fixtures are standalone-only for no structural reason:\n"
        f"  {unexplained}\n"
        "Each spawns no host bridge and asks only for options the docker "
        "provider already honours, so routing it through `_start_dedicated_nest` "
        "(the seam `second_nest` and friends already use) is the whole change — "
        "after which it drops out of LOCAL_NEST_FIXTURES and its tests collect "
        "in a container. If one genuinely cannot route for a reason not modelled "
        "here, add that reason to this test rather than adding the name to the "
        "list: an unexplained entry is how arm 1 stalled at 'mostly done'."
    )


def _start_option_vocabulary(conftest_source):
    """The option names a nest start understands, read off `_make_nest`'s own
    signature past the three positional parameters every start takes — so a new
    knob joins the table's domain by being added there, not by anyone
    remembering a test."""
    import ast

    tree = ast.parse(conftest_source)
    make_nest = next(
        n for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
        and n.name == "_make_nest"
    )
    return frozenset(a.arg for a in make_nest.args.args[3:])


def _derive_start_options(source, options, imported_helpers=frozenset()):
    """`{fixture name: frozenset(options it asks a nest for)}` for ONE file.

    Rooted at both nest-start entry points and closed over local helpers, so a
    fixture that reaches a root through one of its own module's functions is
    graded on what that helper asks for.

    ``imported_helpers`` carries the same closure ACROSS files: helper names
    that reach a root in the module that defines them, so a fixture calling one
    it imported is graded too. Third instance of the same widening — the pins
    below record the first two — and the case that forced it was
    ``tests/scenarios/``, where two module-scoped ``scenario`` fixtures reach the
    provider through ``conftest.create_scenario``, one file over. A single-file
    walk sees the fixture, sees a call it cannot resolve, and concludes the
    fixture starts no nest at all.
    """
    import ast

    tree = ast.parse(source)
    funcs = {
        n.name: n for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }

    def called_names(node):
        out = set()
        for sub in ast.walk(node):
            if isinstance(sub, ast.Call):
                target = sub.func
                if isinstance(target, ast.Name):
                    out.add(target.id)
                elif isinstance(target, ast.Attribute):
                    out.add(target.attr)
        return out

    def is_fixture(node):
        return any("fixture" in ast.unparse(d) for d in node.decorator_list)

    def own_params(node):
        a = node.args
        return {p.arg for p in (*a.posonlyargs, *a.args, *a.kwonlyargs)}

    roots = {"_make_nest", "_start_dedicated_nest"}
    reaches = {
        n for n, f in funcs.items()
        if called_names(f) & (roots | set(imported_helpers))
    }
    grew = True
    while grew:
        grew = False
        for name, node in funcs.items():
            if name not in reaches and called_names(node) & reaches:
                reaches.add(name)
                grew = True

    def asked_here(node):
        """The options this function itself asks for, at every call it makes
        into the chain — forwards and spelled-out defaults excluded."""
        # A FIXTURE's parameters are other fixtures, resolved by pytest, never
        # a caller's value: `static_dir=static_dir` in `share_viewer_nest` asks
        # its nest for the SPA build. Only an impl helper's parameter forwards.
        mine = set() if is_fixture(node) else own_params(node)
        out = set()
        for sub in ast.walk(node):
            if not isinstance(sub, ast.Call):
                continue
            target = sub.func
            name = (
                target.id if isinstance(target, ast.Name)
                else target.attr if isinstance(target, ast.Attribute)
                else None
            )
            if name not in roots and name not in reaches \
                    and name not in imported_helpers:
                continue
            for kw in sub.keywords:
                if kw.arg not in options:
                    continue
                if isinstance(kw.value, ast.Name) and kw.value.id in mine:
                    continue      # a forward: the caller's value decides
                if isinstance(kw.value, ast.Constant) and not kw.value.value:
                    continue      # a spelled-out default is not a request
                out.add(kw.arg)
        return out

    memo = {}

    def needs(name, seen=()):
        if name in memo:
            return memo[name]
        if name in seen:
            return set()
        out = set(asked_here(funcs[name]))
        for callee in called_names(funcs[name]):
            if callee in reaches and callee != name:
                out |= needs(callee, seen + (name,))
        memo[name] = out
        return out

    return {
        n: frozenset(needs(n)) for n in reaches if is_fixture(funcs[n])
    }


def _nest_starting_helpers(source):
    """Plain (non-fixture) function names in ONE file that reach a nest-start
    entry point — the helpers a fixture in ANOTHER file may call."""
    import ast

    tree = ast.parse(source)
    funcs = {n.name: n for n in tree.body
             if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}

    def called(node):
        return {
            sub.func.id if isinstance(sub.func, ast.Name) else sub.func.attr
            for sub in ast.walk(node) if isinstance(sub, ast.Call)
            and isinstance(sub.func, (ast.Name, ast.Attribute))
        }

    roots = {"_make_nest", "_start_dedicated_nest"}
    reaching = {n for n, f in funcs.items() if called(f) & roots}
    grew = True
    while grew:
        grew = False
        for name, node in funcs.items():
            if name not in reaching and called(node) & reaching:
                reaching.add(name)
                grew = True
    # `_start_dedicated_nest` itself lives in the root conftest; naming it here
    # would be circular, and it is already a root.
    return reaching - roots


def _walkable_sources(root):
    """`{path: source}` for every `.py` under the e2e tree worth parsing."""
    sources = {}
    for path in sorted(root.rglob("*.py")):
        if any(part in {".venv", "__pycache__", "node_modules"}
               for part in path.parts):
            continue
        try:
            sources[path] = path.read_text()
        except OSError:
            continue
    return sources


def _tree_nest_starting_helpers(root):
    """Every nest-starting helper NAME the tree defines, for the cross-file
    closure. Computed once and passed to `_derive_start_options`, so a per-file
    call made anywhere else grades the same way this one does — the two used to
    differ, and a fixture reaching the provider through an imported helper then
    existed for the tree walk and not for the collision pin."""
    helpers = set()
    for source in _walkable_sources(root).values():
        try:
            helpers |= _nest_starting_helpers(source)
        except SyntaxError:
            continue
    return helpers


def _derive_start_options_tree():
    """`{fixture: (options, [where it is defined])}` over the WHOLE tree.

    Every `.py` under `tests/e2e-unified` is walked, not just the root conftest,
    and that widening is the whole point rather than thoroughness for its own
    sake — see the pin below. A first pass collects the nest-starting helper
    NAMES each file defines, so the second pass can follow a fixture into a
    helper it imported from a sibling module.
    """
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    options = _start_option_vocabulary((root / "conftest.py").read_text())

    sources = _walkable_sources(root)
    helpers = _tree_nest_starting_helpers(root)

    derived = {}
    for path, source in sources.items():
        try:
            found = _derive_start_options(source, options, helpers)
        except SyntaxError:
            continue
        for name, asked in found.items():
            slot = derived.setdefault(name, (asked, []))
            derived[name] = (slot[0] | asked, slot[1])
            derived[name][1].append(str(path.relative_to(root)))
    return derived


def test_the_fixture_start_options_table_matches_the_tree():
    """`FIXTURE_START_OPTIONS` is pinned to the kwargs each fixture LITERALLY
    passes, the same hand-table-plus-AST-equality shape `LOCAL_NEST_FIXTURES`
    uses — ruling (3) names that pin by name for exactly this table.

    Two derivation rules are load-bearing, and getting either wrong mis-gates a
    real fixture:

    * **A falsy literal is not a request.** `_make_nest` defaults to
      `False`/`None`, so a fixture spelling one out wants the default — the
      rule `_OptionAwareProvider._refuse_unsupported` already applies at
      runtime. Counting those would gate the zero-option fixtures that are the
      first able to run in a container.
    * **A parameter FORWARD belongs to the caller.** The case that taught it
      was `_dedicated_mail_nest_impl` passing `handle_domain=handle_domain`
      through to `_make_nest`, which read as a need made `dedicated_mail_nest` —
      passing no knobs at all — look like it wanted two options it did not want.
      That forward is gone (arm 4 deleted the parameter); the rule stays,
      because it is a property of how impl helpers are written here.

    The derivation is rooted at BOTH nest-start entry points, and that is
    load-bearing rather than tidy. `LOCAL_NEST_FIXTURES` asks "does this fixture
    spawn a local binary", so `_make_nest` alone is its whole root; this table
    asks "what does this fixture ask a nest for", which is just as true once arm
    1 routes the fixture through `_start_dedicated_nest`. Rooting only at
    `_make_nest` would have meant a routed fixture silently left the table — and
    a fixture the table cannot see cannot be class (4), so its unhonourable
    option would surface as a setup-time `NestModeError` from the provider's
    backstop instead of a collection-time declared absence. That is exactly the
    outcome the arm ordering exists to avoid.

    ⚠ **And the walk is the whole TREE, not the root conftest.** Rooting the
    derivation at one file was the same blind spot that hid `two_nodes` from the
    direct-`start_nest` pin — that one scanned `parents[1] / "conftest.py"`, so
    the suite's most-consumed second-nest fixture sat one directory below its
    gaze. Here the consequence is worse than a missed duplicate: a MODULE-LOCAL
    fixture is exactly what arm 1 routes, `classify` grades it by the same bare
    name as any other, and a conftest-only derivation would have let the first
    option-passing one route with the classifier unable to see the option at
    all. The failure would then be a setup-time `NestModeError` — a ❌ against
    working product code, which is the one outcome this arm's ordering exists to
    prevent.
    """
    from helpers import nest_surface as ns

    derived = {n: asked for n, (asked, _) in _derive_start_options_tree().items()}

    def _shown(mapping, name):
        # `sorted(mapping.get(name, ()))` renders an ABSENT key and a
        # zero-option one identically as `[]` — and "the table has never heard
        # of this fixture" is the failure this pin exists to report, so it must
        # not be spelled the same as "it asks for nothing".
        return sorted(mapping[name]) if name in mapping else "ABSENT"

    assert derived == dict(ns.FIXTURE_START_OPTIONS), (
        "the tree's per-fixture start options and "
        "nest_surface.FIXTURE_START_OPTIONS have diverged.\n"
        + "\n".join(
            f"  {name}: tree={_shown(derived, name)} "
            f"table={_shown(ns.FIXTURE_START_OPTIONS, name)}"
            for name in sorted(set(derived) | set(ns.FIXTURE_START_OPTIONS))
            if derived.get(name) != ns.FIXTURE_START_OPTIONS.get(name)
        )
        + "\nA fixture asking for an option its mode's provider does not declare "
        "is a collection-time class (4) declared absence; the table is what the "
        "classifier subtracts the provider's declaration from."
    )


def test_no_two_nest_fixtures_share_a_name_while_asking_for_different_options():
    """The table is keyed by BARE fixture name because that is all `classify`
    ever sees — `item.fixturenames` carries no path. So two nest fixtures of the
    same name in different modules collapse to one row, and if they ask for
    different options the row is a lie about at least one of them.

    Pinned rather than assumed: the tree already has repeated nest-fixture names
    (`nest`, `head_nest`), and the moment one of a repeated pair grows an option
    the other does not, the classifier starts gating a test on a knob its own
    fixture never asked for — or, worse, stops gating one that did.
    """
    collisions = {
        name: where
        for name, (_, where) in _derive_start_options_tree().items()
        if len(where) > 1
    }
    conflicting = {}
    for name, where in collisions.items():
        from pathlib import Path

        root = Path(__file__).resolve().parents[1]
        options = _start_option_vocabulary((root / "conftest.py").read_text())
        helpers = _tree_nest_starting_helpers(root)
        per_file = {
            rel: _derive_start_options(
                (root / rel).read_text(), options, helpers
            )[name]
            for rel in where
        }
        if len(set(per_file.values())) > 1:
            conflicting[name] = per_file
    assert not conflicting, (
        "two nest fixtures share a name but ask a nest for different things, so "
        "the bare-name table cannot describe both:\n"
        + "\n".join(
            f"  {name}: " + ", ".join(
                f"{rel}={sorted(opts)}" for rel, opts in sorted(per_file.items())
            )
            for name, per_file in sorted(conflicting.items())
        )
        + "\nRename one of them, or give the classifier a qualified key."
    )


def test_the_option_table_covers_every_local_nest_fixture():
    """The two tables were the same fixtures seen from two angles until arm 1
    started routing; now the options table is the SUPERSET, and the direction of
    that containment is the property worth pinning.

    A fixture leaves `LOCAL_NEST_FIXTURES` the moment it stops spawning a local
    binary, which is the whole point of routing it — but it goes on asking its
    nest for the same things, so it must never leave the options table on the
    way. The subset assertion is what catches a routing that took the options
    with it: the fixture would still start, still ask, and no longer be
    classifiable.

    Kept exhaustive rather than sparse on purpose, and arm 4 is what it was
    written for: `registration_open` and then `handle_domain` left the table, and
    each departure shows as an EDIT to a recorded fact rather than as an absence
    nobody can tell from a fixture the table never learned.
    """
    from helpers import nest_surface as ns

    assert set(ns.LOCAL_NEST_FIXTURES) <= set(ns.FIXTURE_START_OPTIONS), (
        "a local-nest fixture the options table cannot see: "
        f"{sorted(set(ns.LOCAL_NEST_FIXTURES) - set(ns.FIXTURE_START_OPTIONS))}"
    )


class _FakeItem:
    """The attributes `nest_surface.classify` reads off a pytest item.

    `name` is the third: live's body scan looks the test's function name up in
    the call graph. Defaulting it to a name no module defines keeps every
    fixture-and-marker case above testing exactly what it did before.
    """

    def __init__(self, fixturenames=(), markers=(), name="test_not_a_real_test_fn"):
        self.fixturenames = list(fixturenames)
        self._markers = list(markers)
        self.name = name
        self.originalname = name

    def iter_markers(self):
        return iter(self._markers)


class _FakeMarker:
    def __init__(self, name):
        self.name = name


def test_standalone_excludes_nothing():
    """Eligibility is a default, not an opt-in — and the inner loop must pay
    nothing for the axis existing."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["reclaimable_nest", "app"])
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None


def test_a_local_nest_fixture_excludes_the_test_from_docker():
    """The exemplar is a fixture that still spawns a binary ITSELF.

    It used to be `second_nest`, which arm 1 routed through the mode provider —
    so it is no longer a local-nest fixture, and this rule declining to fire for
    it is the arm working rather than a regression. A fixture that genuinely
    spawns its own binary still cannot run in a container.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["restartable_mda_nest", "app"])
    verdict = ns.classify(item, nm.parse_nest_mode("docker"))
    assert verdict is not None
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "restartable_mda_nest" in reason


def test_a_nest_binary_in_the_closure_excludes_the_test_from_every_other_mode():
    """The general form of the rule above, and the one that closes the hole the
    hand-list could not see: ~100 dedicated-nest fixtures live in TEST MODULES
    (`sell_nest`, `subs_nest`, `two_nodes`, …), not in conftest. `fixturenames`
    is the transitive closure, so the binary they request shows up here.
    """
    from helpers import nest_surface as ns

    # What a module-local `sell_nest` fixture's test actually looks like.
    item = _FakeItem(fixturenames=["sell_nest", "nest_binary", "logged_in_app"])
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    for other in ("docker", "live"):
        verdict = ns.classify(item, nm.parse_nest_mode(other))
        assert verdict is not None, f"{other} must not serve a local binary"
        klass, reason = verdict.klass, verdict.reason
        assert klass == ns.DECLARED_ABSENCE
        assert "nest_binary" in reason
        assert other in reason


def test_an_ordinary_nest_using_test_does_not_carry_the_binary():
    """The rule above is only safe because `nest_instance` resolves the binary
    LAZILY per mode. If it declared `nest_binary` again, every nest-using test
    would land in the closure rule and the whole suite would deselect itself out
    of docker and live — so this pins the shape, not just the name.
    """
    import ast
    from pathlib import Path

    source = Path(__file__).resolve().parents[1] / "conftest.py"
    tree = ast.parse(source.read_text())
    fn = next(
        n for n in tree.body
        if isinstance(n, ast.FunctionDef) and n.name == "nest_instance"
    )
    params = {a.arg for a in fn.args.args}
    assert "nest_binary" not in params, (
        "nest_instance must NOT declare nest_binary — resolve it with "
        "request.getfixturevalue() under nest_mode.builds_local_nest(), or every "
        "nest-using test becomes standalone-only by closure."
    )


def test_every_fixture_that_COMPILES_a_nest_is_named_in_the_binary_set():
    """Derived, not hand-read — the closure rule is only as good as the names in it.

    `NEST_BINARY_FIXTURES` catches a module-local dedicated-nest fixture because
    that fixture *requests* `nest_binary`, so the binary's name lands in the test's
    transitive closure. A fixture that compiles the nest **itself** — through a
    helper rather than through the fixture — puts no such name in the closure and
    slips the net entirely.

    That is not hypothetical. `tests/api/test_activitypub_federation.py::ap_binary`
    calls `helpers.ap_nest.build_ap_nest_binary()` directly, so under `--nest docker`
    the whole ActivityPub federation suite was SELECTED, spent two minutes running
    `cargo build -p fauna-nest --features activitypub,test-hooks` inside the run,
    and would have served a locally-built nest while the run — and every ledger
    record it wrote — reported `docker` (measured 2026-08-28, the first docker-mode
    sweep). `fediverse` is a real catalog feature, so that is a false ✅ against an
    image that never served the test.

    So the rule is scanned rather than listed: any fixture that reaches a local nest
    build must be named in the set that excludes it from the modes it would lie to.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]

    def builds_directly(fn: ast.FunctionDef) -> bool:
        for node in ast.walk(fn):
            if not isinstance(node, ast.Call):
                continue
            fname = getattr(node.func, "id", None) or getattr(node.func, "attr", "")
            if fname.startswith("build_") and fname.endswith("_nest_binary"):
                return True
            # `common.nest.build_node` — the SHARED nest build, and the one the
            # first version of this scan missed: it matched only the
            # `build_*_nest_binary` naming, so a fixture calling the helper every
            # other nest-building fixture ultimately calls slipped through.
            # Found by measurement 2026-08-28, not by re-reading: a docker-mode
            # run of `tests/api/` stalled ~13 minutes inside
            # `test_namespace_sync.py::binary`, which is a `cargo build -p
            # fauna-nest` running *inside a docker run* — the exact false-✅
            # shape this scan exists to prevent, one rename away from the
            # pattern it did recognise.
            if fname in ("build_node", "build_nest"):
                return True
            # a raw `cargo build -p fauna-nest` argv
            for arg in node.args:
                if not isinstance(arg, ast.List):
                    continue
                parts = [e.value for e in arg.elts if isinstance(e, ast.Constant)]
                if "cargo" in parts and "-p" in parts and "fauna-nest" in parts:
                    return True
        return False

    def calls(fn: ast.FunctionDef) -> set:
        return {
            getattr(n.func, "id", None) or getattr(n.func, "attr", "")
            for n in ast.walk(fn) if isinstance(n, ast.Call)
        }

    def compiles_a_nest(fn: ast.FunctionDef, module_fns: dict) -> bool:
        """Transitively, within the module — a fixture usually reaches the build
        through a private `_bring_up_stack`-style helper rather than calling it
        itself (the same indirection `LOCAL_NEST_FIXTURES` had to follow through
        `_dedicated_mail_nest_impl`)."""
        seen, stack = set(), [fn]
        while stack:
            cur = stack.pop()
            if builds_directly(cur):
                return True
            for name in calls(cur):
                if name in module_fns and name not in seen:
                    seen.add(name)
                    stack.append(module_fns[name])
        return False

    def is_fixture(fn: ast.FunctionDef) -> bool:
        for dec in fn.decorator_list:
            target = dec.func if isinstance(dec, ast.Call) else dec
            if getattr(target, "attr", None) == "fixture":
                return True
        return False

    offenders = []
    for path in sorted(root.rglob("*.py")):
        try:
            tree = ast.parse(path.read_text())
        except SyntaxError:  # pragma: no cover - a broken file is another test's job
            continue
        module_fns = {
            n.name: n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)
        }
        for node in module_fns.values():
            if not is_fixture(node):
                continue
            if not compiles_a_nest(node, module_fns):
                continue
            # Two ways to satisfy the rule, because what the classifier actually
            # needs is a binary-shaped name in the TEST's fixture closure:
            # either the fixture is itself named in the set, or it REQUESTS one
            # that is — which puts the name in the closure just as well and is
            # the better shape for a module-local fixture with a generic name
            # (`binary`, `harness`), since adding those to the set would deselect
            # any test that happens to use the same argname.
            declares = {a.arg for a in node.args.args} & ns.NEST_BINARY_FIXTURES
            if node.name not in ns.NEST_BINARY_FIXTURES and not declares:
                offenders.append(f"{path.relative_to(root)}::{node.name}")

    assert not offenders, (
        "these fixtures compile a local fauna-nest but neither are named in "
        "NEST_BINARY_FIXTURES nor request a fixture that is, so a docker or "
        "live run selects their tests and "
        "serves a LOCAL binary while reporting the image: " + ", ".join(offenders)
    )


def test_the_image_feature_set_is_read_from_the_dockerfile_not_remembered():
    """`IMAGE_NEST_FEATURES` is the hinge of exclusion class (9): a fixture is a
    FACT when the artifact cannot carry what it pins, and *what the artifact
    carries* is a line in the `Dockerfile`, not a fact about the harness.

    Remembering it would be the same defect the whole module is written against
    — a hand-read table that is wrong the day it is written. Worse here, because
    both failure directions are silent: if the image dropped `activitypub`, the
    two AP fixtures would stay correctly excluded for a reason that had become
    false, and if it gained `test-hooks` (it must not — convention 15) the class
    would be excluding tests the artifact had started being able to serve.
    """
    import re
    from pathlib import Path

    from helpers import nest_surface as ns

    dockerfile = Path(__file__).resolve().parents[3] / "Dockerfile"
    # Comments stripped FIRST, and that is not tidiness: this file documents its
    # own build in prose, so the same feature string appears in a `#` block a few
    # lines above the RUN. A scan that reads both finds two answers to a question
    # with one, and would red on a correct Dockerfile.
    text = "\n".join(
        line for line in dockerfile.read_text().splitlines()
        if not line.lstrip().startswith("#")
    )

    # The shipped nest build, and only it: the same file also builds fauna-ffi
    # `--no-default-features --features labeler` and the relay `--features
    # fauna-iroh-relay/relay`, neither of which is the nest's feature set. The
    # nest's build is the `cargo build` naming `-p fauna-nest`, and its
    # features are the BARE names: `fauna-nest/<feat>` would bind to the
    # crate's own dev-dependency edge and activate nothing (the Dockerfile's
    # own comment, and `test_cli_features_never_name_a_self_dev_dep.py`).
    builds = [
        m.group(1)
        for m in re.finditer(
            r"cargo build[^\n&]*-p fauna-nest[^\n&]*--features\s+(\S+)", text
        )
    ]
    assert len(builds) == 1, (
        f"expected exactly one `cargo build … -p fauna-nest … --features …` in "
        f"{dockerfile}, found {len(builds)}: {builds}. Class (9) reads the "
        f"shipped nest's feature set off that line; two of them means the "
        f"question 'what does the image ship' no longer has one answer"
    )
    shipped = {f.split("/", 1)[-1] for f in builds[0].split(",")}

    assert shipped == set(ns.IMAGE_NEST_FEATURES), (
        f"the shipped nest image builds --features {sorted(shipped)} but "
        f"nest_surface.IMAGE_NEST_FEATURES says {sorted(ns.IMAGE_NEST_FEATURES)}. "
        f"Reconcile them, then re-decide every entry in "
        f"IMAGE_SERVABLE_BINARY_FIXTURES: each of those is exempt from class (9) "
        f"precisely because the image already carries the capability it pins "
        f"(testing.md § Default app and nest mode, exclusion class (9))"
    )

    assert "test-hooks" not in shipped, (
        "the shipped nest image is built WITH test-hooks. That is a convention 15 "
        "violation on its own (the automation surface must be compiled out of "
        "release artifacts), and it also makes exclusion classes (6) and (9) both "
        "false: they exclude tests on the grounds that no release artifact can "
        "answer a test-hooks-gated route or honour a test-hooks-gated guard"
    )


def test_every_fixture_pinning_a_feature_set_is_dispositioned():
    """Exclusion class (9) pinned by equality to what the fixtures actually build.

    The class's discriminator is NOT "builds a non-release feature set" — every
    binary fixture does, `nest_binary` included (`build_node` defaults to
    `test-hooks,nostr`, the image ships `bluesky,nostr,activitypub`), so that
    reading classifies the suite out and dissolves the routing arm. What decides
    is whether the TEST depends on the difference, which is a judgment, and a
    judgment is exactly the thing that rots silently.

    So the scan makes it un-rottable in the only way available: every fixture
    that pins an EXPLICIT feature set must appear in one of the two tables — the
    class itself, or the exemption table that says why the run's image can serve
    it anyway — and the recorded feature string must equal the one it builds. A
    new such fixture reds until someone decides; a changed feature string reds
    until someone re-decides. Neither table can quietly describe a build that
    stopped happening.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]

    def explicit_features(fn: ast.FunctionDef) -> str | None:
        """The feature string this function pins, if it pins one directly.

        Two spellings, because the tree has both: `build_node(features=...)` (the
        shared builder, keyword-only for this purpose — a positional first arg is
        `release`) and a raw `cargo build -p fauna-nest --features X` argv inside
        a bespoke `build_*` helper.
        """
        for node in ast.walk(fn):
            if not isinstance(node, ast.Call):
                continue
            fname = getattr(node.func, "id", None) or getattr(node.func, "attr", "")
            if fname in ("build_node", "build_nest"):
                for kw in node.keywords:
                    if kw.arg == "features" and isinstance(kw.value, ast.Constant):
                        return kw.value.value
            for arg in node.args:
                if not isinstance(arg, ast.List):
                    continue
                parts = [e.value for e in arg.elts if isinstance(e, ast.Constant)]
                if "cargo" not in parts or "fauna-nest" not in parts:
                    continue
                if "--features" in parts:
                    return parts[parts.index("--features") + 1]
        return None

    # Every function in the tree that pins a set, by name — the resolution has to
    # cross modules (`ap_binary` -> `helpers.ap_nest.build_ap_nest_binary`), and
    # a name is what the call site gives us. Names are unique enough here that
    # collisions would be a different bug; assert that rather than assume it.
    pinners: dict[str, str] = {}
    trees: dict[Path, ast.Module] = {}
    for path in sorted(root.rglob("*.py")):
        try:
            trees[path] = ast.parse(path.read_text())
        except SyntaxError:  # pragma: no cover - a broken file is another test's job
            continue
        # MODULE-LEVEL functions only. `explicit_features` walks each one's
        # whole body, so a nested helper's build is still seen — attributed to
        # the enclosing name, which is the only name a call site can write. Take
        # nested defs as pinners too and the map collides immediately: both
        # `_ensure_bluesky_nest_built` and `_ensure_bridges_nest_built` wrap
        # their build in an inner function literally called `build`.
        for node in trees[path].body:
            if not isinstance(node, ast.FunctionDef):
                continue
            features = explicit_features(node)
            if features is None:
                continue
            assert pinners.get(node.name, features) == features, (
                f"two functions named {node.name} pin different feature sets "
                f"({pinners.get(node.name)!r} and {features!r}); this scan "
                f"resolves cross-module calls by name, so disambiguate them"
            )
            pinners[node.name] = features

    def calls(fn: ast.FunctionDef) -> set:
        return {
            getattr(n.func, "id", None) or getattr(n.func, "attr", "")
            for n in ast.walk(fn) if isinstance(n, ast.Call)
        }

    def is_fixture(fn: ast.FunctionDef) -> bool:
        for dec in fn.decorator_list:
            target = dec.func if isinstance(dec, ast.Call) else dec
            if getattr(target, "attr", None) == "fixture":
                return True
        return False

    found: dict[str, str] = {}
    for path, tree in trees.items():
        module_fns = {
            n.name: n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)
        }
        for node in module_fns.values():
            if not is_fixture(node):
                continue
            # Transitively: `peer` reaches the build through `_bring_up_stack`,
            # the same module-local indirection `LOCAL_NEST_FIXTURES` had to
            # follow through `_dedicated_mail_nest_impl`.
            seen, stack = set(), [node]
            while stack:
                cur = stack.pop()
                direct = explicit_features(cur)
                if direct is not None:
                    found[node.name] = direct
                    break
                hit = next((n for n in calls(cur) if n in pinners), None)
                if hit is not None:
                    found[node.name] = pinners[hit]
                    break
                for name in calls(cur):
                    if name in module_fns and name not in seen:
                        seen.add(name)
                        stack.append(module_fns[name])

    declared = dict(ns.FEATURE_SET_BINARY_FIXTURES)
    for name, (features, _why) in ns.IMAGE_SERVABLE_BINARY_FIXTURES.items():
        assert name not in declared, (
            f"{name} is in BOTH class (9) and its exemption table; it cannot be "
            f"a fact about the artifact and closable debt at once"
        )
        declared[name] = features

    assert found == declared, (
        "the fixtures that pin an explicit nest feature set have drifted from "
        "the tables that disposition them.\n"
        f"  builds but undeclared: {sorted(set(found) - set(declared))}\n"
        f"  declared but no longer builds: {sorted(set(declared) - set(found))}\n"
        "  features changed: "
        + str({k: (declared[k], found[k])
               for k in set(found) & set(declared) if found[k] != declared[k]})
        + "\nEach one is a decision, not a rename: does the TEST depend on the "
        "difference between that set and the shipped image's? Yes -> "
        "FEATURE_SET_BINARY_FIXTURES (a FACT, cells honestly blank forever). "
        "No -> IMAGE_SERVABLE_BINARY_FIXTURES with the reason (ordinary "
        "nest_binary debt). testing.md § Default app and nest mode, class (9)."
    )


def test_class_9_members_pin_a_feature_the_image_cannot_ship():
    """The class's own premise, asserted rather than trusted.

    A member is a FACT only because the artifact cannot be what it needs. Today
    that is `test-hooks` in every case; if a member ever pinned nothing the image
    lacks, its exclusion would be excluding a test the image could serve — the
    over-claim direction, which hides closable work behind a FACT verdict where
    no future session will look for it.
    """
    from helpers import nest_surface as ns

    for name, features in ns.FEATURE_SET_BINARY_FIXTURES.items():
        wanted = {f.strip() for f in features.split(",") if f.strip()}
        unshippable = wanted - set(ns.IMAGE_NEST_FEATURES)
        assert unshippable, (
            f"{name} pins --features {features}, every one of which the image "
            f"already ships ({sorted(ns.IMAGE_NEST_FEATURES)}). It is not a "
            f"class (9) fact — move it to IMAGE_SERVABLE_BINARY_FIXTURES"
        )

    for name, (features, why) in ns.IMAGE_SERVABLE_BINARY_FIXTURES.items():
        assert why.strip(), f"{name} is exempt from class (9) with no reason given"


def test_a_feature_set_fixture_reports_class_9_instead_of_the_blunt_binary_rule():
    """The refinement, which is the whole point: the sharp reason REPLACES the
    blunt one rather than joining it.

    Reported alongside, the test would still count toward `nest_binary [MIXED]`
    — the bucket that reads as closable routing work — and the account this class
    exists to correct would be unchanged.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["ap_binary", "ap_nests"])
    verdicts = list(ns._verdicts(item, nm.parse_nest_mode("docker")))
    rules = {v.rule for v in verdicts}

    assert ns.RULE_FEATURE_SET_BINARY in rules, rules
    assert ns.RULE_NEST_BINARY not in rules, (
        f"a class (9) fixture still reports the blunt binary rule too: {rules}"
    )
    reason = next(v.reason for v in verdicts if v.rule == ns.RULE_FEATURE_SET_BINARY)
    assert "test-hooks" in reason and "activitypub,test-hooks" in reason, reason

    # ...and a test that stands up BOTH kinds of nest is genuinely blocked twice.
    both = _FakeItem(fixturenames=["ap_binary", "nest_binary"])
    assert {v.rule for v in ns._verdicts(both, nm.parse_nest_mode("docker"))} >= {
        ns.RULE_FEATURE_SET_BINARY,
        ns.RULE_NEST_BINARY,
    }


def test_a_permanently_unhonourable_option_is_a_fact_not_closable_debt():
    """The `unsupported_option` split — ruling (3) always implied it; the audit
    could not read it.

    "This mode does not turn that knob" was two facts in one sentence: a knob a
    provider could GROW (closable debt, which is why the class is graded MIXED)
    and one it can never turn (a cell honestly blank forever). Docker honours
    every option a fixture asks for except `handle_domain_seed`, which ruling (3)
    settles by name — so before the split the class was 100% permanent while
    grading as 100% closable, and it was the audit's last MIXED sole-blocker.
    """
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")

    # A fixture wanting only the permanent option reports the FACT rule, and
    # NOT the debt one — the whole point, exactly as class (9) subtracts from
    # `nest_binary` rather than reporting alongside it.
    seed_only = _FakeItem(fixturenames=["unclaimed_real_domain_nest"])
    rules = {v.rule for v in ns._verdicts(seed_only, docker)}
    assert ns.RULE_PERMANENT_OPTION in rules, rules
    assert ns.RULE_UNSUPPORTED_OPTION not in rules, (
        f"a permanently unhonourable option still reports as closable debt: {rules}"
    )

    reason = next(
        v.reason for v in ns._verdicts(seed_only, docker)
        if v.rule == ns.RULE_PERMANENT_OPTION
    )
    assert "handle_domain_seed" in reason and "NEVER" in reason, reason


def test_the_permanence_is_read_off_the_provider_not_restated():
    """`permanently_unsupported_options` is a provider declaration, like
    `supported_options` beside it, so the classifier holds no second list to
    fall out of step with the thing that actually starts nests.

    Both halves are asserted because both have a failure mode. A permanence that
    is NOT a subset of what the provider fails to support is a contradiction —
    the provider would be declaring it can never do something it does. And live's
    sentinel has to BE the sentinel: that module imports `nest_mode` lazily on
    purpose, so its value is spelled as a literal there.
    """
    import conftest
    from helpers import nest_surface as ns

    assert conftest._LiveProvider.permanently_unsupported_options == nm.ALL_OPTIONS, (
        "live spells the all-options sentinel as a literal because conftest "
        "imports nest_mode lazily; it has drifted from nest_mode.ALL_OPTIONS"
    )

    every_option = frozenset().union(*ns.FIXTURE_START_OPTIONS.values())
    for name in ("standalone", "docker", "live"):
        mode = nm.parse_nest_mode(name)
        supported = nm.supported_options(mode)
        permanent = nm.permanently_unsupported(mode, every_option)
        assert not (permanent & supported), (
            f"{name} declares it can NEVER honour "
            f"{sorted(permanent & supported)} while also declaring it supports "
            f"them; one of the two is wrong"
        )

    # And the substance, so a silent emptying of docker's declaration — which
    # would quietly re-grade the whole class as closable work — is caught here
    # rather than noticed in a report months later.
    assert nm.permanently_unsupported(
        nm.parse_nest_mode("docker"), every_option
    ) == {"handle_domain_seed"}
    assert nm.permanently_unsupported(
        nm.parse_nest_mode("live"), every_option
    ) == every_option
    assert not nm.permanently_unsupported(
        nm.parse_nest_mode("standalone"), every_option
    ), "standalone honours every option; nothing there can be permanently absent"


def test_an_ordinary_test_is_eligible_for_docker_by_default():
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["logged_in_app", "nest_instance"])
    assert ns.classify(item, nm.parse_nest_mode("docker")) is None


def test_a_mode_only_marker_excludes_the_other_modes():
    from helpers import nest_surface as ns

    item = _FakeItem(markers=[_FakeMarker("standalone_only")])
    verdict = ns.classify(item, nm.parse_nest_mode("docker"))
    assert verdict is not None
    assert verdict[0] == ns.DECLARED_ABSENCE
    assert "standalone_only" in verdict[1]
    # ...and does NOT exclude it from its own mode.
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None


def test_every_mode_marker_classify_honours_is_one_a_capability_error_names():
    """The `NestCapabilityError` message tells the reader to add
    `@pytest.mark.<mode>_only`. If `classify` did not honour exactly those
    markers, that advice would be a dead end."""
    from helpers import nest_surface as ns

    for mode_name, marker in nm.MODE_MARKERS.items():
        if mode_name == "docker":
            continue
        item = _FakeItem(markers=[_FakeMarker(marker)])
        assert ns.classify(item, nm.parse_nest_mode("docker")) is not None


def test_declared_absence_requires_a_citation():
    """A declared absence with no declaration is unbuilt debt wearing a better
    name — the same rule `app_surface.declared_absence` enforces."""
    from helpers import nest_surface as ns

    with pytest.raises(ValueError, match="citation"):
        ns.declared_absence(capability="a virgin box", doc="")


# ── Live's capabilities and provider (slice 3) ──────────────────────────────

def test_live_has_a_provider():
    assert nm.provider_for(nm.parse_nest_mode("live")) is not None


def test_live_declares_every_capability_absent():
    """Not harness debt to close later: each key is a fact about a machine that
    is not this one, so a live run reaching for it is asking the wrong question."""
    absent = nm.absent_capabilities(nm.parse_nest_mode("live"))
    assert absent == nm.CAPABILITY_KEYS


def test_every_live_absence_carries_a_reason():
    for key in nm.absent_capabilities(nm.parse_nest_mode("live")):
        assert nm.LIVE_ABSENT[key].strip(), f"{key} needs a stated reason"


def test_a_live_capability_error_states_the_reason_and_the_marker():
    """The docker twin of this test caught the reason table being docker-only;
    live's reasons must reach the message the same way."""
    mode = nm.parse_nest_mode("live")
    handle = nm.NestHandle(
        {"url": "https://example.com"}, mode, absent=nm.absent_capabilities(mode),
    )
    with pytest.raises(nm.NestCapabilityError) as exc:
        handle["db_path"]
    message = str(exc.value)
    assert "db_path" in message
    assert "remote box" in message
    assert "live_only" in message, "the message must name the marker that admits it"


def test_only_standalone_builds_a_local_nest():
    """docker's binary is in the image and live's is on the remote box, so both
    would pay a cold cargo build — plus its machine-wide build-slot wait — for a
    binary they never execute."""
    assert nm.builds_local_nest(nm.parse_nest_mode("standalone")) is True
    assert nm.builds_local_nest(nm.parse_nest_mode("docker")) is False
    assert nm.builds_local_nest(nm.parse_nest_mode("live")) is False


def test_an_unknown_mode_is_still_refused_at_parse_time():
    """The `_unbuilt_message` path now has no mode left to fire on, so the
    refusal that keeps a typo from silently becoming standalone is `parse`'s."""
    with pytest.raises(nm.NestModeError, match="unknown nest mode"):
        nm.parse_nest_mode("staging")


# ── Class (3): global-admin-mutating, live only (slice 3) ───────────────────

def test_the_global_admin_fixtures_live_only_exclusion_is_accounted_for():
    """Every class-(3) fixture is either subsumed by a structural class or named
    here as excluded by the shared-box rule alone — and the latter is pinned as
    LIVE-ONLY, so the list keeps deciding exactly what it claims to.

    From 2026-08-28 (the bridge-enrollment class) until 2026-09-25 every entry
    was subsumed: each spawned a host-side bridge or built a local nest, so an
    earlier structural class excluded it first. Reading `tests/common/` (where
    `open_registration` lives) and provider methods into the reach graph found
    fixtures that are not: they start a "dedicated" nest through the mode
    provider, and on live the provider hands back the real box itself — so
    their registration and mail-domain writes land on it.
    """
    from helpers import nest_surface as ns

    structural = ns.BRIDGE_SPAWN_FIXTURES | ns.LOCAL_NEST_FIXTURES | ns.NEST_BINARY_FIXTURES

    # Subsumption is a property of the CLOSURE, not of the name: `handled_nest`
    # requests `nest_binary` and the two `mda_*` fixtures request
    # `mail_bridge_mda`, so their tests carry a structural name transitively
    # even though their own is not listed. These are named here rather than
    # computed, so that a NEW class-(3) fixture trips this test and is sorted
    # into one of the two sets deliberately. `dav_user` joined on the same
    # terms: it requests `mail_bridge_mda`.
    by_closure_only = {"dav_user", "handled_nest", "mda_junk_train_sealed", "mda_spam_scoring"}
    by_shared_box_rule_only = {
        "caldav_cross_nest_peer",
        "cross_nest_foreign",
        "cross_nest_foreign_ephemeral",
        "dedicated_caldav_mailbox_less_nest",
        "dedicated_caldav_only_nest",
        "dedicated_mail_nest",
        "dedicated_mail_nest_handle_domain",
        "registration_posture_nest",
    }
    unaccounted = sorted(
        ns.GLOBAL_ADMIN_FIXTURES - structural - by_closure_only - by_shared_box_rule_only
    )
    assert not unaccounted, (
        "these class-(3) fixtures are in neither set: sort each into the "
        "structural or the shared-box-only one: " + ", ".join(unaccounted)
    )

    # The subsumption is real in both directions: standalone still admits
    # them (the inner loop pays nothing), while docker now refuses.
    item = _FakeItem(fixturenames=["mail_bridge_mta", "logged_in_app"])
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    assert ns.classify(item, nm.parse_nest_mode("docker")) is not None

    # And the shared-box-only ones are exactly that: live refuses them on
    # class (3), while standalone and docker (a throwaway nest) do not. An
    # earlier class may ALSO fire on a real closure (a start option live
    # cannot honour, say), so this asks for membership, not first place.
    for name in sorted(by_shared_box_rule_only):
        item = _FakeItem(fixturenames=[name, "logged_in_app"])
        assert ns.RULE_GLOBAL_ADMIN in ns.all_rules(item, nm.parse_nest_mode("live")), name
        assert ns.classify(item, nm.parse_nest_mode("standalone")) is None, name
        assert ns.RULE_GLOBAL_ADMIN not in ns.all_rules(item, nm.parse_nest_mode("docker")), name


def test_the_global_admin_marker_excludes_a_test_whose_own_body_mutates():
    """The fixture-closure inference cannot see a kind called in a test body;
    the marker is that half."""
    from helpers import nest_surface as ns

    item = _FakeItem(markers=[_FakeMarker(ns.GLOBAL_ADMIN_MARKER)])
    assert ns.classify(item, nm.parse_nest_mode("docker")) is None
    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None
    assert ns.GLOBAL_ADMIN_MARKER in verdict[1]


def test_live_ok_re_admits_a_class_three_test():
    """The ratified "individually re-admittable with proven revert-on-teardown".

    Demonstrated on the MARKER half of class (3): every class-(3) *fixture* is
    now also excluded by a structural class (see the subsumption pin above), and
    a marker must not be able to talk the harness past one of those — which the
    test directly below asserts.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(
        markers=[
            _FakeMarker(ns.GLOBAL_ADMIN_MARKER),
            _FakeMarker(ns.LIVE_READMIT_MARKER),
        ],
    )
    assert ns.classify(item, nm.parse_nest_mode("live")) is None


def test_live_ok_cannot_re_admit_a_test_that_spawns_a_host_bridge():
    """The same limit as the local-binary case, for the same reason.

    `live_ok` is a promise about *cleaning up* — "this test restores what it
    changed", which answers the shared-box rule. It says nothing about whether
    the bridge can enroll at all, and it cannot: nest's loopback gate refuses a
    bridge that is not on the box. A marker must not be able to re-admit a test
    that will simply hang on `/healthz`.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["mail_bridge_mta"],
        markers=[_FakeMarker(ns.LIVE_READMIT_MARKER)],
    )
    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None
    assert "loopback" in verdict[1].lower()


def test_live_ok_cannot_re_admit_a_test_that_needs_a_local_binary():
    """Class (3) is a policy about a shared box; the local-binary classes are
    structural. A marker must not be able to talk the harness into serving a
    local nest while the report says 'live'."""
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["restartable_mda_nest"],
        markers=[_FakeMarker(ns.LIVE_READMIT_MARKER)],
    )
    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None
    assert "restartable_mda_nest" in verdict[1]


def test_the_call_graph_reads_the_shared_common_tree():
    """`tests/common/` is where the harness's wire helpers live — `set_tier_caps`,
    `open_registration`, `factory_reset_and_restart` — and a graph rooted at
    `tests/e2e-unified/` alone never parsed it, so every fixture that reached a
    global kind through one of them reached nothing. That is how a live run came
    to lift the real box's `free` tier caps for every user on it: `test_user`
    called `set_tier_caps`, and `set_tier_caps` was invisible."""
    from helpers import kind_reach
    from helpers import nest_surface as ns

    assert "fauna.admin.tiers.update" in kind_reach.kinds_reached_by(
        "set_tier_caps", ns.KIND_VOCABULARY
    )
    assert "fauna.admin.set_registration_mode" in kind_reach.kinds_reached_by(
        "open_registration", ns.KIND_VOCABULARY
    )


def test_a_fixture_s_global_reach_excludes_its_tests_from_live():
    """The body half used to look up only the TEST FUNCTION's own name, so a
    global mutation made by a fixture in its closure was never attributed to the
    test. `collision_domain` is a module fixture in
    `test_admin_picker_label_collision.py`, on no list, that adds a mail domain
    to whatever nest the run targets — on live, the real box."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["collision_domain", "nest_instance"])
    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None
    assert verdict.rule == ns.RULE_GLOBAL_ADMIN
    assert "fauna.bridges.add_local_domain" in verdict.reason
    assert "collision_domain" in verdict.reason
    # Class (3) is live-only: a throwaway nest pays nothing for the mutation.
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None


def test_a_live_self_gated_fixture_s_reach_does_not_exclude():
    """The closure walk honours `LIVE_SELF_GATED_FIXTURES` — otherwise the
    session identity (in every `logged_in_app` closure) and the session nest
    would exclude the whole live axis for a mutation they skip on live."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["test_user", "nest_instance", "logged_in_app"])
    assert ns.RULE_GLOBAL_ADMIN not in ns.all_rules(item, nm.parse_nest_mode("live"))


def _kinds_test_user_sends(monkeypatch, mode: str, tiers: tuple) -> list:
    """Run the real `test_user` fixture body under `mode` against a stood-in
    wire whose tier list is `tiers`; return every `(kind, body)` it sent."""
    import conftest
    from common import auth
    from helpers import nest_mode as nm_mod
    from helpers import shared_identity

    sent = []
    caps = {"max_inbox_bytes": 1, "max_storage_bytes": 1, "max_devices": 2,
            "max_blob_size": 1, "max_feeds": 5}

    def wire(base, sk, kind, body):
        sent.append((kind, body))
        if kind == "fauna.admin.tiers.list":
            return {"tiers": [{"name": name, **caps} for name in tiers]}
        return {}

    monkeypatch.setattr(nm_mod, "run_mode", lambda: nm_mod.parse_nest_mode(mode))
    monkeypatch.setattr(auth, "_authed_call", wire)
    monkeypatch.setattr(auth, "_resolve_base", lambda port, url: (url, None))
    monkeypatch.setattr(auth, "_coerce_admin_signing_key", lambda sk: sk)
    monkeypatch.setattr(conftest, "_make_user", lambda nest: {"actor_id_hex": "ab"})
    monkeypatch.setattr(shared_identity, "remember_shared_actor", lambda _a: None)
    nest = {"port": 1, "url": "http://127.0.0.1:1", "admin": {"signing_key": "sk"}}
    conftest.test_user.__wrapped__(nest)
    return sent


def test_the_session_identity_leaves_the_shared_tier_alone_on_live(monkeypatch):
    """`test_user` lifts the `free` tier's caps so one identity can stand in for
    every test's account — on a throwaway nest. On live, `free` is the tier
    every real user of the box is on, and `tiers.update`/`tiers.create` are
    global with no delete to reap a harness tier, so the run sends neither: it
    moves its OWN account onto the admin-made `LIVE_HARNESS_TIER` when the box
    has one, and otherwise keeps `free`'s caps (`testing.md` § Default app and
    nest mode, *Live mode* (d))."""
    from common import auth
    from helpers import nest_surface as ns

    lifted = [k for k, _ in _kinds_test_user_sends(monkeypatch, "standalone", ("free",))]
    assert "fauna.admin.tiers.update" in lifted

    for tiers in (("free",), ("free", auth.LIVE_HARNESS_TIER)):
        sent = _kinds_test_user_sends(monkeypatch, "live", tiers)
        assert {k for k, _ in sent} <= ns.ACCOUNT_SCOPED_KINDS, sent
        moved = [b for k, b in sent if k == "fauna.admin.users.update"]
        if auth.LIVE_HARNESS_TIER in tiers:
            assert moved == [{"actor_id": bytes.fromhex("ab"),
                              "tier": auth.LIVE_HARNESS_TIER, "label": ""}]
        else:
            assert moved == []


def test_the_global_admin_fixture_list_matches_conftest():
    """`GLOBAL_ADMIN_FIXTURES` is derived from conftest's AST, not hand-read —
    the same pin `LOCAL_NEST_FIXTURES` gets, for the same reason.

    Deny-by-default over a closed vocabulary: any `fauna.admin.*`/
    `fauna.bridges.*` kind outside `ACCOUNT_SCOPED_KINDS` counts as global, so
    an unclassified new kind shrinks live's eligible set until someone looks.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    source = Path(__file__).resolve().parents[1] / "conftest.py"
    tree = ast.parse(source.read_text())
    funcs = {
        n.name: n for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }

    def is_fixture(node):
        return any("fixture" in ast.unparse(d) for d in node.decorator_list)

    # The same graph `classify` consults, so the list and the live verdict can
    # never disagree about what a fixture reaches. This pin used to walk
    # conftest's own functions only, which is how `test_user` reaching
    # `fauna.admin.tiers.update` through `common.auth.set_tier_caps` stayed
    # off every list. Membership in the classified vocabulary, not a
    # `fauna.admin.` prefix, for the body half's immunity to error codes.
    from helpers import kind_reach

    def reaches_a_global_kind(name):
        return bool(
            kind_reach.kinds_reached_by(name, ns.KIND_VOCABULARY)
            - ns.ACCOUNT_SCOPED_KINDS
        )

    reaches = {n for n in funcs if reaches_a_global_kind(n)}
    derived = {n for n in reaches if is_fixture(funcs[n])}
    # The ones already standalone-only are excluded from live by class (1)/(2)
    # anyway, so class (3) neither needs nor claims them.
    derived -= set(ns.LOCAL_NEST_FIXTURES)
    expected = set(ns.GLOBAL_ADMIN_FIXTURES) | set(ns.LIVE_SELF_GATED_FIXTURES)
    assert derived == expected, (
        "conftest's global-admin-mutating fixtures and nest_surface have "
        "diverged.\n"
        f"  only in conftest: {sorted(derived - expected)}\n"
        f"  only in the lists: {sorted(expected - derived)}\n"
        "A fixture that mutates state outside the run's own account must either "
        "join GLOBAL_ADMIN_FIXTURES (so live excludes its tests) or — if it "
        "disables itself on live, as the autouse primary-domain pin does — "
        "LIVE_SELF_GATED_FIXTURES, which needs that self-gate to actually exist."
    )


def _registered_admin_bridge_kinds() -> set[str]:
    """Every `fauna.admin.*`/`fauna.bridges.*` kind `kind.rs` registers.

    Cut at `#[cfg(test)]`: the tail is assertion literals, not registrations.
    A static parse is sound here because every registration is a plain
    `self.add("literal", meta)` — verified in both directions, and the reason
    this pin can be an equality rather than a ratchet.
    """
    import re
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    source = (root / "libs/fauna-protocol/src/kind.rs").read_text()
    head = source.split("#[cfg(test)]", 1)[0]
    return set(re.findall(r'"(fauna\.(?:admin|bridges)\.[a-z0-9_.]+)"', head))


def test_the_kind_partition_covers_exactly_what_kind_rs_registers():
    """The partition is pinned to the protocol vocabulary **by equality**.

    This is what makes "classify the kinds, not the tests" hold over time:
    registering a new admin/bridge kind fails here until somebody decides which
    side it belongs on. A ratchet would let the newest — least-reviewed — kind
    be the one nobody classifies.
    """
    from helpers import nest_surface as ns

    registered = _registered_admin_bridge_kinds()
    classified = set(ns.ACCOUNT_SCOPED_KINDS) | set(ns.GLOBAL_ADMIN_KINDS)

    assert classified == registered, (
        "the account-scoped/global kind partition and kind.rs have diverged.\n"
        f"  registered but unclassified: {sorted(registered - classified)}\n"
        f"  classified but not registered: {sorted(classified - registered)}\n"
        "A new fauna.admin.*/fauna.bridges.* kind must join "
        "ACCOUNT_SCOPED_KINDS (it touches only the caller's own account — see "
        "R1 (account-data-plane.md § The ratified decisions)/R2/R3 in nest_surface) or GLOBAL_ADMIN_KINDS (anything else, "
        "including every bridge-class kind), which decides whether tests "
        "reaching it may run against the shared live box."
    )


def test_the_two_halves_of_the_partition_are_disjoint():
    from helpers import nest_surface as ns

    overlap = set(ns.ACCOUNT_SCOPED_KINDS) & set(ns.GLOBAL_ADMIN_KINDS)
    assert not overlap, f"a kind cannot be both: {sorted(overlap)}"


def test_the_writes_on_read_kinds_stay_global():
    """The trap the partition exists to survive: kinds whose NAME reads like a
    query and whose handler writes. Each was confirmed at the handler this list
    was written — `check_greylist` upserts a nest-wide greylist row,
    `list_mailboxes` seeds the standard mailboxes. A future session tidying the
    partition by name would re-admit exactly these, so they are pinned by name."""
    from helpers import nest_surface as ns

    leaked = sorted(ns.WRITES_ON_READ_KINDS & set(ns.ACCOUNT_SCOPED_KINDS))
    assert not leaked, (
        f"these read-shaped kinds mutate and must stay global: {leaked}"
    )
    # And the list itself must not rot into naming kinds that no longer exist.
    unregistered = sorted(ns.WRITES_ON_READ_KINDS - _registered_admin_bridge_kinds())
    assert not unregistered, f"no longer registered in kind.rs: {unregistered}"


def test_a_test_whose_own_body_reaches_a_global_kind_is_excluded_from_live():
    """The half the fixture closure and the marker both miss — and the reason
    the scan is a call graph rather than a body grep: neither of these two real
    tests contains the kind literal, they reach it through an admin-client
    wrapper."""
    from helpers import nest_surface as ns

    item = _FakeItem(name="test_force_rotate_dkim_flips_active_selector_live")
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    assert ns.classify(item, nm.parse_nest_mode("docker")) is None

    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None, "a body-reached global kind must exclude on live"
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "force_rotate_dkim" in reason
    assert "shared-box" in reason


def test_a_test_that_reaches_only_account_scoped_kinds_still_runs_on_live():
    """The other direction, and the one that matters for coverage: the scan
    must not exclude a test just because it names an admin kind. Live's eligible
    set is the point of the axis."""
    from helpers import nest_surface as ns

    # A real test, and a representative one: the account-provisioning reach
    # (`users.delete`/`users.suspend` via the harness's own reap) is what nearly
    # every test in the suite carries, so misclassifying it would empty live.
    item = _FakeItem(name="test_enable_activitypub")
    from helpers import kind_reach

    reached = kind_reach.kinds_reached_by("test_enable_activitypub", ns.KIND_VOCABULARY)
    assert reached, "fixture picked a name the call graph does not know"
    assert not (reached - ns.ACCOUNT_SCOPED_KINDS)
    assert ns.classify(item, nm.parse_nest_mode("live")) is None


def test_a_protocol_slot_is_not_an_edge_in_the_reach_graph(tmp_path):
    """An explicit `x.__enter__()` must not inherit every context manager's body.

    Red-verified 2026-09-22 against the tree itself: `helpers/search_journeys.py`
    arms a test-hook route inside `FailingNestArm.__enter__`, and the name-keyed
    graph then attributed `/api/v1/test/rpc-hold/` to `_client_for` (which
    enters its cached ws-rpc client explicitly) and through it to most of
    `tests/api/` — the two eligibility pins above and the test-hook class's own
    pin went red, and the axis read the suite as excluded from docker and live.
    A `with` statement never surfaced as an edge to the slot in the first place,
    so the explicit call is now treated the same way: a dunder binds by receiver
    type, which a name-keyed graph cannot see.

    **Construction is the one place the type IS named** (2026-10-05): `Arm()`
    calls the class itself, so it reaches that class's own `__init__` /
    `__enter__` / `__exit__` and no other class's — keyed by the class name,
    not merged by the dunder's. Without it the three search journeys that arm
    the rpc-hold hook through `FailingNestArm.__enter__` reached nothing, and a
    live run recorded them as failed instead of deselecting them (class 6)."""
    from helpers import kind_reach

    tree = tmp_path / "e2e"
    (tree / "helpers").mkdir(parents=True)
    (tree / "helpers" / "arm.py").write_text(
        "class Arm:\n"
        "    def __init__(self):\n"
        "        post('/api/v1/test/rpc-hold/init')\n"
        "    def __enter__(self):\n"
        "        post('/api/v1/test/rpc-hold/x')\n"
        "        return self\n",
        encoding="utf-8",
    )
    (tree / "helpers" / "client.py").write_text(
        "def _client_for(url):\n"
        "    client = make(url)\n"
        "    client.__enter__()\n"
        "    return client\n"
        "def build(arm):\n"
        "    Arm.__init__(arm)\n"
        "def test_constructs(url):\n"
        "    with Arm():\n"
        "        pass\n"
        "def test_plain(url):\n"
        "    return _client_for(url)\n"
        "def test_named(url):\n"
        "    _client_for(url)\n"
        "    arm_it()\n"
        "def arm_it():\n"
        "    post('/api/v1/test/rpc-hold/y')\n",
        encoding="utf-8",
    )
    reach = kind_reach.build_reach(prefix="/api/v1/test/", root=tree)
    assert reach["test_plain"] == frozenset(), (
        "an explicit __enter__ call inherited a context manager's body — the slot "
        "binds by receiver type, which the name-keyed graph cannot see"
    )
    assert reach["build"] == frozenset(), "an explicit __init__ call is not an edge either"
    assert reach["test_constructs"] == frozenset({
        "/api/v1/test/rpc-hold/init", "/api/v1/test/rpc-hold/x",
    }), "constructing a class names its type, so its own protocol methods are reached"
    assert reach["test_named"] == frozenset({"/api/v1/test/rpc-hold/y"}), (
        "a named wrapper one call away must still reach — only the slot is cut"
    )


def test_no_protocol_slot_names_a_kind_route_or_key_directly():
    """The precondition of the cut above, held against the real tree: the graph
    follows no edge INTO a dunder, so a kind, route or key named inside one
    would be unreachable from everywhere. None does today; one that appears
    moves its literal into a named method the graph can see."""
    import ast

    from helpers import kind_reach
    from helpers import nest_surface as ns

    offenders = []
    for path in (p for r in kind_reach._ROOTS for p in kind_reach._iter_sources(r)):
        try:
            tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        except SyntaxError:
            continue
        for fn in kind_reach._functions(tree):
            if not kind_reach._is_dunder(fn.name):
                continue
            named = (
                kind_reach._kind_literals(fn, ns.KIND_VOCABULARY)
                | kind_reach._route_literals(fn, ns.TEST_HOOK_ROUTE_PREFIX)
                | kind_reach._key_reads(fn, ns.PEER_AUTHORITY_KEYS)
            )
            if named:
                offenders.append((path.name, fn.name, sorted(named)))
    assert not offenders, (
        f"a protocol slot names what the reach graph classifies on: {offenders} — "
        "move the literal into a named method; the graph follows no edge into a dunder"
    )


# ── Live account reaping (slice 3) ──────────────────────────────────────────

def test_the_ledger_only_records_live_accounts_once():
    from helpers import live_accounts

    live_accounts.reset()
    live_accounts.note("aa" * 32, "e2e-aaaa")
    live_accounts.note("aa" * 32, "e2e-aaaa")
    live_accounts.note("bb" * 32, None)
    assert [e["actor_id"] for e in live_accounts.created()] == ["aa" * 32, "bb" * 32]
    live_accounts.reset()
    assert live_accounts.created() == []


def test_the_reap_suspends_before_it_schedules_the_delete():
    """Suspension is the IMMEDIATE half — `fauna.admin.users.delete` only
    schedules a pending action (7-day delay, `pending_actions.rs`), so a reap
    that ran it first and then failed would leave the account able to connect."""
    from helpers import live_accounts

    live_accounts.reset()
    live_accounts.note("cc" * 32, "e2e-cccc")
    calls = []

    def fake_call(kind, payload):
        if kind == "fauna.admin.users.list":
            return {"users": [], "total": 0}
        calls.append(kind)
        return {"ok": True}

    residue = live_accounts.reap(
        "https://example.com", None, call=fake_call, log=lambda m: None
    )
    assert residue == []
    assert calls == ["fauna.admin.users.suspend", "fauna.admin.users.delete"]
    assert live_accounts.created() == [], "a reaped ledger must not re-warn"


def test_a_failed_reap_warns_loudly_and_never_raises():
    """It runs in teardown, often while the body's own failure is unwinding —
    raising there would replace the real diagnosis with this one."""
    from helpers import live_accounts

    live_accounts.reset()
    live_accounts.note("dd" * 32, "e2e-dddd")
    lines = []

    def fake_call(kind, payload):
        raise RuntimeError("nest said no")

    residue = live_accounts.reap(
        "https://example.com", None, call=fake_call, log=lines.append
    )
    assert len(residue) == 1
    assert "suspend failed" in residue[0]["problem"]
    assert "delete-schedule failed" in residue[0]["problem"]
    warning = "\n".join(lines)
    assert "RESIDUE" in warning
    assert "e2e-dddd" in warning, "the warning must name the account to clean up"


def _users_list_answer(users):
    """A scripted `fauna.admin.users.list` that pages like the nest does."""

    def answer(payload):
        offset = payload.get("offset", 0)
        limit = payload.get("limit", 50)
        return {"users": users[offset:offset + limit], "total": len(users)}

    return answer


def test_the_reap_follows_a_noted_handle_to_the_successor_that_holds_it():
    """A succession ceremony the APP runs (no harness dial sees it) moves the
    account — and its handle — to a fresh actor id; the retired id keeps a
    handle-less row the nest happily suspends. Measured 2026-10-04 on
    `dev.example.com`: a reap that suspended only
    the noted ids reported success and left both successors of
    `test_account_instance_lock_tui.py` ACTIVE. Handles are unique per box and
    this run minted the noted one, so whoever holds it now is this run's."""
    from helpers import live_accounts

    live_accounts.reset()
    retired, successor, foreign = "aa" * 32, "bb" * 32, "cc" * 32
    live_accounts.note(retired, "e2e-aaaa")
    users = [
        {"actor_id": bytes.fromhex(foreign), "handle": "someone-else"},
        {"actor_id": bytes.fromhex(successor), "handle": "e2e-aaaa"},
        {"actor_id": bytes.fromhex(retired), "handle": None},
    ]
    listing = _users_list_answer(users)
    suspended = []

    def fake_call(kind, payload):
        if kind == "fauna.admin.users.list":
            return listing(payload)
        if kind == "fauna.admin.users.suspend":
            suspended.append(bytes(payload["actor_id"]).hex())
        return {}

    lines = []
    residue = live_accounts.reap("https://example.com", None, call=fake_call, log=lines.append)
    assert residue == []
    assert sorted(suspended) == sorted([retired, successor]), (
        "the successor holding the run's handle must be reaped, and nobody else"
    )
    assert any("[live] reaped 2" in line for line in lines), lines


def test_a_reap_that_cannot_list_users_warns_about_unfollowed_successors():
    """No silent pass: if the handle lookup fails, the noted ids are still
    reaped and the summary says a successor may have been missed."""
    from helpers import live_accounts

    live_accounts.reset()
    live_accounts.note("dd" * 32, "e2e-dddd")

    def fake_call(kind, payload):
        if kind == "fauna.admin.users.list":
            raise RuntimeError("nest said no")
        return {}

    lines = []
    live_accounts.reap("https://example.com", None, call=fake_call, log=lines.append)
    text = "\n".join(lines)
    assert "[live] reaped 1" in text
    assert "successor" in text and "e2e-dddd" in text, text


def test_the_reap_report_reaches_the_terminal_summary_not_the_fixture_capture():
    """The reap runs in a fixture finalizer, and pytest discards a passing
    test's captured fixture output — so before this the `[live] reaped N` line
    (and the RESIDUE warning a human must act on) never reached any log. The
    default logger keeps the lines for `pytest_terminal_summary` to print."""
    from helpers import live_accounts

    live_accounts.reset()
    live_accounts.drain_report()
    live_accounts.note("ee" * 32, "e2e-eeee")

    def fake_call(kind, payload):
        if kind == "fauna.admin.users.list":
            return {"users": [], "total": 0}
        return {}

    live_accounts.reap("https://example.com", None, call=fake_call)
    report = live_accounts.drain_report()
    assert any("[live] reaped 1" in line for line in report), report
    assert live_accounts.drain_report() == [], "draining empties the report"


class _ScriptedSocket:
    """A socket that answers every request with one canned Reply frame."""

    def __init__(self, ok=True, payload=None):
        self._ok = ok
        self._payload = payload if payload is not None else {}
        self._corr = None

    def send_binary(self, frame):
        import cbor2

        self._corr = cbor2.loads(frame)[1]

    def settimeout(self, _):
        pass

    def recv(self):
        import cbor2

        return cbor2.dumps({0: 1, 1: self._corr, 4: self._payload, 7: self._ok})


def _wire_call(kind, payload, *, ok=True, reply=None):
    from clients import _ws_rpc_core as core

    client = core._WsRpcClientBase("https://box.example", reply_timeout=5)
    client._ws = _ScriptedSocket(ok=ok, payload=reply)
    return client._attempt(kind, payload, b"\0" * 16)


@pytest.mark.parametrize(
    "kind, payload",
    [
        ("fauna.admin.users.create", {"actor_id": bytes.fromhex("e1" * 32), "handle": "e2e-e1"}),
        ("fauna.account.register", {"actor_id": "e1" * 32, "handle": "e2e-e1"}),
    ],
)
def test_every_account_the_harness_creates_on_live_is_noted_at_the_wire(kind, payload):
    """Account-scoped isolation only holds if the reap knows every account the
    run made, and a per-call-site `live_accounts.note` did not: measured
    2026-10-04, one interrupted `dev.example.com`
    sweep leaked 53 accounts (`crowd*`, `collide*`, `admitted-*`, …) that test
    bodies and fixtures created outside `conftest._make_user`. So the note is
    taken where every harness dial passes — the WS-RPC core — on the successful
    reply of each account-creating kind, whatever helper or test body sent it."""
    from helpers import live_accounts

    saved = nm.run_mode()
    live_accounts.reset()
    try:
        nm.set_run_mode(nm.parse_nest_mode("live:https://box.example"))
        _wire_call(kind, payload)
        assert [(e["actor_id"], e["handle"]) for e in live_accounts.created()] == [
            ("e1" * 32, "e2e-e1")
        ]

        live_accounts.reset()
        with pytest.raises(Exception):
            _wire_call(kind, payload, ok=False, reply={"code": "fauna.x.refused"})
        assert live_accounts.created() == [], "a refused create made no account"

        nm.set_run_mode(nm.parse_nest_mode("standalone"))
        _wire_call(kind, payload)
        assert live_accounts.created() == [], (
            "standalone discards its whole nest; a ledger there would warn about "
            "accounts on a nest that no longer exists"
        )
    finally:
        nm.set_run_mode(saved)
        live_accounts.reset()


def test_an_invite_request_is_noted_pending_and_a_never_admitted_one_is_no_residue():
    """An invite request becomes an account only when an admin approves it —
    on live, through the app UI, which no harness dial observes. So the submit
    is noted as PENDING, and the reap reads the nest's `not_found` on such an
    entry as "never admitted": nothing to clean up, never a RESIDUE warning."""
    from helpers import live_accounts

    saved = nm.run_mode()
    live_accounts.reset()
    try:
        nm.set_run_mode(nm.parse_nest_mode("live:https://box.example"))
        _wire_call(
            "fauna.account.invite_request.submit",
            {"actor_id": "f2" * 32, "handle": "joiner1", "message": "hi"},
        )
        assert [e["pending"] for e in live_accounts.created()] == [True]

        class _NotFound(Exception):
            code = "fauna.admin.not_found"

        calls = []

        def fake_call(kind, payload):
            if kind == "fauna.admin.users.list":
                return {"users": [], "total": 0}
            calls.append(kind)
            raise _NotFound()

        lines = []
        residue = live_accounts.reap(
            "https://box.example", None, call=fake_call, log=lines.append,
        )
        assert residue == []
        assert calls == ["fauna.admin.users.suspend"], "nothing to delete"
        assert not any("RESIDUE" in line for line in lines)
    finally:
        nm.set_run_mode(saved)
        live_accounts.reset()


def test_only_the_last_handle_on_the_live_box_reaps_the_run(monkeypatch):
    """Every nest the harness "starts" on live is the SAME box, and the ledger
    is run-wide — so the reap belongs to the last handle that closes, never to
    a dedicated nest's module teardown.

    Measured 2026-10-05 on `dev.example.com`: `test_archive_import`'s
    module-scoped `_start_dedicated_nest` resolved to the live box, and its
    cleanup reaped the whole ledger — the session `test_user` included. The
    shared account came back `user is suspended` and every later
    `logged_in_app` test died at the connection barrier, ~18 h of sweep
    turned into one red per test.
    """
    import conftest
    from helpers import live_accounts

    reaps = []
    monkeypatch.setattr(live_accounts, "reap", lambda url, sk, **kw: reaps.append(url))
    monkeypatch.setattr(conftest._LiveProvider, "_admin",
                        lambda self, url: {"signing_key": None})
    saved = nm.run_mode()
    try:
        nm.set_run_mode(nm.parse_nest_mode("live:https://box.example"))
        provider = conftest._LiveProvider()
        _session, close_session = provider.start(None, None, "nest")
        _dedicated, close_dedicated = provider.start(None, None, "archive-import-nest")
        close_dedicated()
        assert reaps == [], (
            "a dedicated nest's teardown reaped the run's accounts while the "
            "session nest — and its test_user — was still in use"
        )
        close_session()
        assert reaps == ["https://box.example"], "the last close reaps, once"

        # A handle opened and closed on its own still reaps what it made.
        _alone, close_alone = provider.start(None, None, "solo-nest")
        close_alone()
        assert reaps == ["https://box.example"] * 2
    finally:
        nm.set_run_mode(saved)


# ── The admin contract, and the bridge-enrollment exclusion ─────────────────
# Both landed 2026-08-28 out of the second docker-mode sweep. The mode had been
# made to *run* at all by the TLS-port registration fix, but the mail half of the
# suite still failed wholesale — and the two causes turned out to be different in
# kind: one a harness contract that had quietly drifted, one a property of the
# product that no harness fix can reach.


def test_both_claim_admin_helpers_answer_one_admin_contract(monkeypatch):
    """`nest_instance["admin"]` must carry the same keys in every mode.

    Standalone and live claim through `common.auth.claim_admin`; docker claims
    through `tests/platform/docker/helpers.py::claim_admin_api`, a second
    implementation of the same `fauna.auth.claim_admin` round trip. Their return
    dicts had drifted: the docker one answered `secret_hex` / `domain` but NOT
    `actor_id_bytes` or `deployment_seed`.

    That is not a cosmetic difference. `_spawn_mta_bridge` reads
    `nest_instance["admin"]["actor_id_bytes"]` (the admin's MLS pubkey, so a
    role address can receive mail), so under `--nest docker` every mail-bridge
    fixture died at setup with a bare `KeyError: 'actor_id_bytes'` — before the
    bridge was even spawned, and with nothing in the message to suggest that the
    MODE was what differed (measured 2026-08-28, the second docker-mode sweep).

    Compared as key SETS rather than by asserting the two names that happened to
    be missing: the next key to drift is the one nobody thought to list.
    """
    import clients.ws_rpc_anon_client as anon_mod

    class _FakeAnon:
        def __init__(self, base, *a, **kw):
            self.base = base

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def call(self, kind, payload):
            assert kind == "fauna.auth.claim_admin"
            return {
                "token": "fake-bearer",
                "deployment_seed": "ab" * 32,
                "domain": "example.test",
            }

    monkeypatch.setattr(anon_mod, "WsRpcAnonClient", _FakeAnon)

    from common.auth import claim_admin
    from tests.platform.docker.helpers import claim_admin_api

    shared = claim_admin(4321, "claim-code", base_url="http://127.0.0.1:4321")
    docker = claim_admin_api(4321, "claim-code")

    assert set(shared) == set(docker), (
        "the two claim_admin helpers answer different key sets, so "
        "nest_instance['admin'] means something different under --nest docker "
        "than in standalone/live: "
        f"only in shared={sorted(set(shared) - set(docker))}, "
        f"only in docker={sorted(set(docker) - set(shared))}"
    )


def test_a_host_spawned_bridge_fixture_is_excluded_from_docker():
    """A bridge the harness spawns on the HOST cannot enroll against a container.

    `fauna.bridges.request_enrollment` is loopback-gated at the dispatcher
    (`routes.rs` step (1c) → `pre_identity_allowlist::requires_loopback_peer`),
    and that gate fails CLOSED on `peer_addr = None`. A bridge process on the
    host reaching the container's published port is not a loopback peer — nest
    sees the docker gateway address, the very fact
    `test_caldav_sni_router.py::test_external_request_enrollment_refused_over_router`
    relies on — so it is refused and never opens its listeners.

    Measured 2026-08-28 by spawning the built bridge by hand against a container
    started from `ghcr.io/faunasocial/nest:latest`::

        "self-enrolling over anonymous WS" ...
        await bridge approval: enrollment poll: request_enrollment:
        wsrpc: server returned ok=false
        (code=fauna.bridges.remote_enrollment_unsupported)

    This is the product behaving as designed — cross-IP bridge enrollment is
    deliberately deferred (`mail-bridge-lifecycle.md` § Cold boot step 3) — so
    the exclusion is a CLASSIFICATION, not a bug to fix. The faithful docker
    shape is the image's own s6-supervised bridges over the container's
    loopback, which `tests/platform/docker/` already drives.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["mail_bridge_mta", "nest_instance"])
    verdict = ns.classify(item, nm.parse_nest_mode("docker"))

    assert verdict is not None, (
        "a host-spawned mail bridge cannot enroll against a containerised "
        "nest, so its tests must not be selected in docker mode"
    )
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "mail_bridge_mta" in reason
    assert "loopback" in reason.lower(), (
        f"the reason must say WHY, so a reader is not left guessing: {reason}"
    )


def test_a_host_spawned_bridge_fixture_is_excluded_from_live_too():
    """Same gate, same verdict: a live nest is on another host entirely.

    Live already excluded these through `GLOBAL_ADMIN_FIXTURES` (they mutate
    nest-global mail state, which the shared-box rule forbids), so this pins the
    *reason* rather than the outcome — the enrollment gate is the more
    fundamental of the two, and it must not be possible to re-admit these tests
    to live by relaxing the shared-box class alone.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["mail_bridge_mda", "nest_instance"])
    verdict = ns.classify(item, nm.parse_nest_mode("live"))

    assert verdict is not None
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "loopback" in reason.lower(), (
        f"live must refuse these for the enrollment reason too: {reason}"
    )


def test_every_fixture_that_SPAWNS_A_BRIDGE_is_named_in_the_bridge_set():
    """Derived, not hand-read — the same rule the nest-binary set lives under.

    A fixture that spawns a host-side bridge process is docker/live-ineligible by
    construction (the loopback gate above). Hand-listing them would rot the day
    someone adds a CardDAV or ATProto bridge fixture, so the set is re-derived
    from the AST: any fixture whose transitive call graph within its own module
    reaches one of conftest's `_spawn_*_bridge` helpers must be named in
    `BRIDGE_SPAWN_FIXTURES`.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]

    def calls(fn: ast.FunctionDef) -> set:
        return {
            getattr(n.func, "id", None) or getattr(n.func, "attr", "")
            for n in ast.walk(fn) if isinstance(n, ast.Call)
        }

    def spawns_directly(fn: ast.FunctionDef) -> bool:
        return any(
            name.startswith("_spawn_") and name.endswith("_bridge")
            for name in calls(fn)
        )

    def spawns_a_bridge(fn: ast.FunctionDef, module_fns: dict) -> bool:
        seen, stack = set(), [fn]
        while stack:
            cur = stack.pop()
            if spawns_directly(cur):
                return True
            for name in calls(cur):
                if name in module_fns and name not in seen:
                    seen.add(name)
                    stack.append(module_fns[name])
        return False

    def is_fixture(fn: ast.FunctionDef) -> bool:
        for dec in fn.decorator_list:
            target = dec.func if isinstance(dec, ast.Call) else dec
            if getattr(target, "attr", None) == "fixture":
                return True
        return False

    offenders = []
    for path in sorted(root.rglob("*.py")):
        try:
            tree = ast.parse(path.read_text())
        except SyntaxError:  # pragma: no cover - a broken file is another test's job
            continue
        module_fns = {
            n.name: n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)
        }
        for node in module_fns.values():
            if not is_fixture(node):
                continue
            # Already-excluded is already correct: most of these also stand up a
            # nest of their own, and that reason is both true and more basic, so
            # `BRIDGE_SPAWN_FIXTURES` carries only the ones no other class
            # catches. What must never happen is a bridge-spawning fixture in
            # NONE of the three.
            classified = (
                ns.BRIDGE_SPAWN_FIXTURES
                | ns.LOCAL_NEST_FIXTURES
                | ns.NEST_BINARY_FIXTURES
            )
            if spawns_a_bridge(node, module_fns) and node.name not in classified:
                offenders.append(f"{path.relative_to(root)}::{node.name}")

    assert not offenders, (
        "these fixtures spawn a host-side bridge process but are in none of "
        "BRIDGE_SPAWN_FIXTURES / LOCAL_NEST_FIXTURES / NEST_BINARY_FIXTURES, so "
        "a docker or live run selects their tests and the bridge is refused "
        "enrollment by nest's loopback gate: " + ", ".join(offenders)
    )


# ── Class (5)'s THIRD door: the test that runs the bridge ITSELF ────────────
#
# The two doors above are both FIXTURE-shaped: a fixture named in
# `BRIDGE_SPAWN_FIXTURES` spawns the bridge, or a fixture-less body drives
# `request_enrollment` over an anonymous WS client. Measured 2026-09-02, a
# third shape has been sitting in the tail the whole time and is caught by
# neither: seven ATProto/Bluesky test bodies `subprocess.Popen` the built
# bridge binary INLINE (`test_atproto_bridge_enroll.py:78`), so there is no
# spawning fixture to name and the kind is never reached from Python at all —
# the bridge, a separate process, is the one that calls it.
#
# The consequence is an accounting lie rather than a wrong exclusion: every one
# of these also requests `nest_binary`, so `classify`'s first match already
# excludes them and the RUN is correct. But the mode audit reads the account,
# and the ratified order puts `nest_binary` (MIXED — closable where the binary
# is incidental) ahead of `bridge_spawn` (a FACT), so all seven were filed as
# closable routing work. They are not closable: routing them would move seven
# clean collection-time exclusions into seven runtime ❌ against product code
# behaving exactly as ratified.
#
# The signal is the BINARY fixture. A test asks for a built bridge binary in
# order to run one — there is nothing else to do with it — so the request is
# the spawn, one AST hop earlier than the `Popen` it leads to.


def _self_enrolling_bridge_cmds() -> set:
    """The `bins/fauna-bridges/cmd/<x>` binaries that self-enroll, from source.

    Pinned to the ONE door — `wsrpc.EnrollAndAwaitApproval`, the function that
    dials anonymous and calls `fauna.bridges.request_enrollment` — rather than
    to the kind string, which appears in both mains only inside a comment and
    would make this pin a prose check. `_test.go` files are excluded: the
    atproto main's own unit test signs an enrollment payload without ever
    performing one, and a binary that cannot enroll at runtime is not a host
    bridge no matter what its tests construct.
    """
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    cmds = set()
    for directory in sorted((root / "bins/fauna-bridges/cmd").iterdir()):
        if not directory.is_dir():
            continue
        sources = (
            p for p in directory.rglob("*.go") if not p.name.endswith("_test.go")
        )
        if any(
            "wsrpc.EnrollAndAwaitApproval(" in p.read_text(errors="replace")
            for p in sources
        ):
            cmds.add(directory.name)
    return cmds


def test_the_bridge_BINARY_fixture_set_matches_the_bridges_that_SELF_ENROLL():
    """Class (5)'s third door is pinned to the product, not hand-listed.

    A prebuild fixture is a bridge-binary fixture exactly when the binary it
    builds is one that self-enrolls. Both directions are asserted, so the set
    can neither miss a new bridge (a CardDAV bridge fixture would go red here
    rather than quietly re-admitting its tests to docker) nor over-claim one:
    `seal_helper_binary` builds out of the very same `bins/fauna-bridges/`
    tree and is deliberately NOT in the set, because the seal helper performs
    a client-side seal step and never enrolls.
    """
    import ast
    from pathlib import Path

    import conftest as ct
    from helpers import nest_surface as ns

    cmds = _self_enrolling_bridge_cmds()
    assert cmds, "no self-enrolling bridge cmd found — the pin's door moved"

    root = Path(__file__).resolve().parents[1]
    tree = ast.parse((root / "conftest.py").read_text())
    builders = {
        n.name: n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)
    }

    def builds_a_bridge(builder_name: str) -> bool:
        fn = builders.get(builder_name)
        if fn is None:
            return False
        doc = ast.get_docstring(fn)
        literals = {
            n.value
            for n in ast.walk(fn)
            if isinstance(n, ast.Constant) and isinstance(n.value, str)
        } - {doc}
        return any(cmd in lit for lit in literals for cmd in cmds)

    derived = {
        fixture
        for fixture, builder, _args in ct._PREBUILD_BY_FIXTURE
        if builds_a_bridge(builder)
    }

    assert derived == set(ns.BRIDGE_BINARY_FIXTURES), (
        "BRIDGE_BINARY_FIXTURES must name exactly the prebuild fixtures whose "
        f"binary self-enrolls ({sorted(cmds)}): derived {sorted(derived)}, "
        f"declared {sorted(ns.BRIDGE_BINARY_FIXTURES)}"
    )


def test_a_test_that_RUNS_the_bridge_binary_itself_is_a_bridge_spawn_FACT():
    """The audit's account, not the run's exclusion — and that is the point.

    `classify` answers `nest_binary` here exactly as it did before (these
    tests build a local nest too, and the ratified order is unchanged), so no
    test changes mode. What changes is `all_rules`: the FACT is now in the
    account, so the audit stops offering these pages as closable routing work.
    """
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    item = _FakeItem(fixturenames=["nest_binary", "atproto_bridge_binary"])

    assert ns.classify(item, docker).rule == ns.RULE_NEST_BINARY, (
        "the run's own exclusion and its reason must not move"
    )
    rules = ns.all_rules(item, docker)
    assert ns.RULE_BRIDGE_SPAWN in rules, (
        "a test that Popens the built bridge in its own body is a "
        f"host-spawned bridge, whatever spawned it: {rules}"
    )
    assert rules.index(ns.RULE_NEST_BINARY) < rules.index(ns.RULE_BRIDGE_SPAWN)


def _reason_for(item, mode, rule) -> str:
    """The reason attached to ONE rule, where `classify` answers the first.

    `all_rules` drops the reasons and `classify` keeps only the first match, so
    a pin over a later rule's wording has no public reader. Walking the one
    generator both public entries are built over keeps this from becoming a
    second classifier.
    """
    from helpers import nest_surface as ns

    for verdict in ns._verdicts(item, mode):
        if verdict.rule == rule:
            return verdict.reason
    raise AssertionError(f"no {rule} verdict: {ns.all_rules(item, mode)}")


def test_the_bridge_binary_reason_says_the_TEST_runs_it():
    """A reason a reader can act on: which fixture, and who spawns.

    The fixture-half reason reads "depends on X, which spawns a bridge process
    on the HOST" — false of a binary fixture, which only BUILDS. Getting this
    wrong would send the next reader looking for a spawning fixture that does
    not exist.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_binary", "mail_bridge_binary"])
    reason = _reason_for(item, nm.parse_nest_mode("docker"),
                         ns.RULE_BRIDGE_SPAWN)

    assert "mail_bridge_binary" in reason
    assert "loopback" in reason.lower()
    assert "own body" in reason or "its own test body" in reason, (
        f"the reason must say the TEST runs the bridge, not a fixture: {reason}"
    )


def test_both_bridge_doors_at_once_report_the_class_ONCE():
    """`disposable_mta_bridge` is exactly this shape — it requests both.

    Two evidence sources for one fact must not become two entries in the
    account: the audit tallies rule ids, and a page counted twice under
    `bridge_spawn` would overstate the class as surely as missing it
    understates it.
    """
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["mail_bridge_mta", "mail_bridge_binary"])
    rules = ns.all_rules(item, nm.parse_nest_mode("docker"))

    assert rules.count(ns.RULE_BRIDGE_SPAWN) == 1, rules
    reason = _reason_for(item, nm.parse_nest_mode("docker"),
                         ns.RULE_BRIDGE_SPAWN)
    assert "mail_bridge_mta" in reason and "mail_bridge_binary" in reason, (
        f"one verdict, but both sources named: {reason}"
    )


# ── The floor cert and raw urllib dials ─────────────────────────────────────
#
# `test_a_tls_serving_handle_registers_its_port_...` above fixed the URL a
# port-keyed dial resolves to. This section fixes the layer immediately inside
# it: having reached the right `https://` authority, a raw `urllib` dial then
# has to survive the handshake, and the image's bootstrap floor cert is
# self-signed by construction — it chains to no public root and never will.
#
# Measured 2026-08-28 in the `tests/api/` docker-mode profile: ten of the
# fifteen failures were one cause, six modules deep —
#
#     ssl.SSLCertVerificationError: [SSL: CERTIFICATE_VERIFY_FAILED]
#         certificate verify failed: self-signed certificate
#
# — every one of them a raw `urllib.request.urlopen(req)` that built the right
# URL and supplied no SSL context. `common.auth._resolve_base` had already
# ratified the rule for the dials it owns ("For any https:// base we skip cert
# verification"); these modules each roll their own request and inherit nothing.


@contextlib.contextmanager
def _floor_cert_listener():
    """A throwaway loopback HTTPS listener serving an UNTRUSTED leaf cert.

    The shape of the nest image's self-signed bootstrap cert, minus the nest:
    `EphemeralCA` is not in any trust store, so a verifying client rejects the
    leaf exactly as it rejects the real floor cert. Yields `(port, url)`.
    """
    import http.server
    import ssl as _ssl
    import tempfile
    import threading

    from helpers.ephemeral_ca import EphemeralCA

    leaf = EphemeralCA().issue("127.0.0.1")
    with tempfile.TemporaryDirectory() as tmp:
        cert_path, key_path = leaf.write(tmp, "floor")
        ctx = _ssl.SSLContext(_ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert_path, key_path)

        class _Quiet(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 - stdlib callback name
                self.send_response(200)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"ok")

            def log_message(self, *_args):  # keep pytest output readable
                pass

        srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Quiet)
        srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
        port = srv.socket.getsockname()[1]
        thread = threading.Thread(target=srv.serve_forever, daemon=True)
        thread.start()
        try:
            yield port, f"https://127.0.0.1:{port}/"
        finally:
            srv.shutdown()
            srv.server_close()
            thread.join(timeout=5)


def _assert_verification_refused(url, **kwargs):
    """The dial reached a verifying client and was refused for the cert.

    `urlopen` wraps the handshake failure in `URLError`, so the refusal has to be
    read off `.reason` — asserting `URLError` alone would also pass for a refused
    connection, i.e. for a listener that never came up.
    """
    with pytest.raises(urllib.error.URLError) as excinfo:
        urllib.request.urlopen(url, timeout=10, **kwargs)
    assert isinstance(excinfo.value.reason, ssl.SSLCertVerificationError), (
        f"expected a certificate refusal, got {excinfo.value.reason!r}"
    )


def test_a_raw_urlopen_to_a_registered_tls_nest_survives_the_floor_cert():
    """The fix: once a nest is marked TLS-serving, a context-free `urlopen` to it
    stops verifying — so the six `tests/api/` modules that roll their own request
    inherit the posture `_resolve_base` already grants the dials it owns.

    Pinned end-to-end against a real handshake rather than on the host predicate,
    because the two defects this file already records were both *wiring* gaps
    (a gate one fixture too late; a guard that matched the wrappers and missed
    the helper they wrap), not decision gaps.
    """
    from common.auth import mark_tls_nest

    with _floor_cert_listener() as (port, url):
        mark_tls_nest(port)
        with urllib.request.urlopen(url, timeout=10) as resp:
            assert resp.read() == b"ok"


def test_an_unregistered_loopback_tls_port_still_verifies():
    """The converse, so the relaxation cannot creep onto every loopback dial: a
    TLS listener this harness never declared as a nest keeps full verification.
    A test that wants otherwise says so — by marking the port, or by passing its
    own context."""
    with _floor_cert_listener() as (_port, url):
        _assert_verification_refused(url)


def test_a_registered_non_loopback_nest_still_verifies():
    """The live-box guard, and the reason the rule is a conjunction.

    `_LiveProvider` also hands back `serve_tls: True`, so `mark_tls_nest` fires
    for a real box at `https://example.com` holding a real ACME cert. Keying the
    relaxation on the port ALONE would silently stop verifying it — turning the
    one mode where a cert failure is a genuine production incident into a green
    run. Host classification is asserted directly here: binding a non-loopback
    interface is not a thing an isolated test may do (conventions point 10).
    """
    from common import auth

    port = 59_233
    auth.mark_tls_nest(port)
    assert auth._is_floor_cert_authority(f"127.0.0.1:{port}")
    assert not auth._is_floor_cert_authority(f"example.com:{port}")


def test_an_explicit_context_still_wins_over_the_installed_opener():
    """The escape hatch every negative control needs: a dial that passes its own
    context bypasses the opener entirely, so a test asserting the floor cert is
    NOT WebPKI-valid (`test_spki_pinned_nest_client_legs.py::
    test_the_floor_cert_is_not_webpki_valid`) can still be written over urllib."""
    from common.auth import mark_tls_nest

    with _floor_cert_listener() as (port, url):
        mark_tls_nest(port)
        _assert_verification_refused(url, context=ssl.create_default_context())


def _ws_dial(url: str, **kwargs):
    """Dial ``url`` with a raw ``websocket.create_connection``, as the modules
    that bypass the WS-RPC clients do. Returns the exception it raised.

    The listener answers a plain ``200 ok``, never a ``101``, so a dial that gets
    *through* TLS still ends in ``WebSocketBadStatusException`` — and that is the
    point: the two outcomes separate cleanly at the layer under test. A cert
    refusal never reaches the HTTP status, and a status error proves the
    handshake did.
    """
    import websocket

    with pytest.raises(Exception) as excinfo:  # noqa: PT011 - the class IS the assertion
        websocket.create_connection(url, timeout=10, **kwargs)
    return excinfo.value


def _is_cert_refusal(exc: BaseException) -> bool:
    """True when ``exc`` (or anything it wraps) is a certificate refusal.

    ``websocket-client`` re-raises the handshake failure as-is on some paths and
    wrapped in ``WebSocketException`` on others, so the chain is walked rather
    than the top type asserted.
    """
    seen = exc
    while seen is not None:
        if isinstance(seen, ssl.SSLCertVerificationError):
            return True
        seen = seen.__cause__ or seen.__context__
    return "CERTIFICATE_VERIFY_FAILED" in repr(exc)


def test_a_raw_websocket_dial_to_a_registered_tls_nest_survives_the_floor_cert():
    """The WS half of the floor-cert posture: `common.auth.ws_sslopt`.

    The installed opener is a *urllib* mechanism, so a module reaching the nest
    over a raw `websocket.create_connection` inherited nothing from it and met
    the self-signed floor cert bare. Measured 2026-08-30 under `--nest docker`:
    one dialer had survived the opener that way
    (`tests/api/test_nest_rotation_chain.py`), and it was the whole remaining SSL
    failure in that slice.

    Pinned end-to-end against a real handshake, for the same reason the urllib
    twin above is: both defects this file records were *wiring* gaps, not
    decision gaps — and this fix's own risk is exactly that shape, a helper that
    decides correctly while some dialer never calls it.
    """
    from common.auth import mark_tls_nest, ws_sslopt

    with _floor_cert_listener() as (port, url):
        mark_tls_nest(port)
        ws_url = "wss://" + url.partition("://")[2]
        exc = _ws_dial(ws_url, sslopt=ws_sslopt(ws_url))
        assert not _is_cert_refusal(exc), (
            f"the floor cert was still refused over WS: {exc!r}"
        )


def test_an_unregistered_loopback_tls_port_still_verifies_over_websocket():
    """The converse, so the WS relaxation cannot creep onto every loopback dial
    any more than the urllib one may: a TLS listener this harness never declared
    as a nest keeps full verification."""
    from common.auth import ws_sslopt

    with _floor_cert_listener() as (_port, url):
        ws_url = "wss://" + url.partition("://")[2]
        exc = _ws_dial(ws_url, sslopt=ws_sslopt(ws_url))
        assert _is_cert_refusal(exc), (
            f"an undeclared TLS listener must still be verified, got {exc!r}"
        )


def test_ws_sslopt_keeps_verifying_a_registered_non_loopback_nest():
    """The live-box guard — the reason `ws_sslopt` reuses the opener's
    CONJUNCTION rather than keying on the scheme the way the WS-RPC clients'
    `_ssl_opt` does.

    `_LiveProvider` declares `serve_tls`, so a real box at `https://example.com` on
    a real ACME cert is `mark_tls_nest`-marked too. A scheme-only rule would
    silently stop verifying it — turning the one mode where a cert failure is a
    genuine production incident into a green run.
    """
    from common import auth

    port = 59_234
    auth.mark_tls_nest(port)
    assert auth.ws_sslopt(f"wss://127.0.0.1:{port}/api/v1/ws") == {"cert_reqs": ssl.CERT_NONE}
    assert auth.ws_sslopt(f"wss://example.com:{port}/api/v1/ws") is None


@contextlib.contextmanager
def _floor_cert_websocket_listener():
    """A throwaway loopback TLS listener that COMPLETES a WebSocket upgrade.

    `_floor_cert_listener` answers `200 ok` and is enough to separate a cert
    refusal from a reached handshake. This one goes one step further because the
    hop under test is a *splice*: the SPA proxy replays the browser's upgrade
    request upstream verbatim and pumps bytes both ways, so the only observable
    that proves the hop spoke TLS is a `101` arriving back at the browser side.
    Serves the same untrusted `EphemeralCA` leaf; yields `(port, handshakes)`,
    where `handshakes` records, per accepted connection, whether the TLS
    handshake completed (`True`) or the peer spoke something that was not TLS
    (`False`) — the plaintext-into-a-TLS-listener signature.
    """
    import base64
    import hashlib
    import socket
    import tempfile
    import threading

    from helpers.ephemeral_ca import EphemeralCA

    leaf = EphemeralCA().issue("127.0.0.1")
    handshakes: list[bool] = []
    with tempfile.TemporaryDirectory() as tmp:
        cert_path, key_path = leaf.write(tmp, "floor")
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert_path, key_path)
        srv = socket.create_server(("127.0.0.1", 0))
        port = srv.getsockname()[1]

        def _serve_one(conn):
            try:
                with ctx.wrap_socket(conn, server_side=True) as tls:
                    raw = b""
                    while b"\r\n\r\n" not in raw:
                        chunk = tls.recv(4096)
                        if not chunk:
                            return
                        raw += chunk
                    key = ""
                    for line in raw.split(b"\r\n"):
                        name, _sep, value = line.partition(b":")
                        if name.strip().lower() == b"sec-websocket-key":
                            key = value.strip().decode("latin-1")
                    accept = base64.b64encode(hashlib.sha1(
                        (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                    ).digest()).decode()
                    tls.sendall(
                        b"HTTP/1.1 101 Switching Protocols\r\n"
                        b"Upgrade: websocket\r\nConnection: Upgrade\r\n"
                        b"Sec-WebSocket-Accept: " + accept.encode() + b"\r\n\r\n"
                    )
                    handshakes.append(True)
                    while tls.recv(4096):
                        pass
            except OSError:
                # `ssl.SSLError` is an `OSError`: a peer that sent plaintext
                # (or a verifying client that walked away from the cert) lands
                # here, and that is the fact the pin reads.
                handshakes.append(False)
            finally:
                try:
                    conn.close()
                except OSError:
                    pass

        def _accept_loop():
            while True:
                try:
                    conn, _peer = srv.accept()
                except OSError:
                    return
                threading.Thread(target=_serve_one, args=(conn,), daemon=True).start()

        thread = threading.Thread(target=_accept_loop, daemon=True)
        thread.start()
        try:
            yield port, handshakes
        finally:
            srv.close()
            thread.join(timeout=5)


@contextlib.contextmanager
def _spa_proxy_to(nest_url: str, tmp_path):
    """`conftest._serve_spa_proxy` in front of `nest_url`, torn down after.
    Yields the `ws://` URL of its `/api/v1/ws` splice."""
    import conftest

    base, server = conftest._serve_spa_proxy(str(tmp_path), nest_url)
    try:
        yield "ws://" + base.partition("://")[2] + "/api/v1/ws"
    finally:
        server.shutdown()
        server.server_close()


def test_the_spa_proxy_websocket_hop_speaks_tls_to_a_registered_tls_nest(tmp_path):
    """The third transport of the floor-cert posture: the SPA proxy's WS splice.

    A browser reaches a nest only through `conftest._serve_spa_proxy` (the raw
    nest sends no CORS headers), and that proxy's two hops used to disagree: the
    HTTP hop rode the installed `_FloorCertHTTPSHandler` opener and so already
    spoke TLS to a marked loopback nest, while `_proxy_websocket` opened a bare
    `socket.create_connection` and spoke PLAINTEXT into the TLS listener — the
    nest answered with a fatal `decode_error` alert and the upgrade never
    completed. Every docker-mode nest is `https://` by construction, so every
    web app-driven journey under `--nest docker` died at login with an opaque
    `ConnectionBarrierTimeout`. That is lesson (5)'s
    shape exactly — a wiring gap, not a decision gap — and the same twin
    `ws_sslopt` gave raw dialers: the hop asks `common.auth.upstream_tls_context`
    and wraps its upstream socket under the SAME conjunction the opener applies
    (a loopback host AND a port marked by `mark_tls_nest`).

    Pinned end-to-end: a real TLS listener that completes the upgrade, a real
    `websocket.create_connection` through the proxy, and the `101` arriving back
    is the only observable that proves the splice spoke TLS.
    """
    import websocket

    from common.auth import mark_tls_nest

    with _floor_cert_websocket_listener() as (port, handshakes):
        mark_tls_nest(port)
        with _spa_proxy_to(f"https://127.0.0.1:{port}", tmp_path) as ws_url:
            ws = websocket.create_connection(ws_url, timeout=10)
            ws.close()
    assert handshakes == [True], (
        f"the proxy's WS hop did not complete a TLS handshake upstream: {handshakes!r}"
    )


def test_the_spa_proxy_websocket_hop_still_verifies_an_unregistered_tls_listener(tmp_path):
    """The converse, so the splice's relaxation cannot creep onto every loopback
    upstream any more than the urllib or raw-WS ones may: a TLS listener this
    harness never declared as a nest is verified, the leaf is refused, and no
    upgrade completes — the browser side sees the dial fail, never a `101`."""
    import websocket

    with _floor_cert_websocket_listener() as (port, handshakes):
        with _spa_proxy_to(f"https://127.0.0.1:{port}", tmp_path) as ws_url:
            with pytest.raises(Exception):  # noqa: PT011 - the class IS the assertion
                websocket.create_connection(ws_url, timeout=10)
    assert True not in handshakes, (
        f"an undeclared TLS listener was reached without verification: {handshakes!r}"
    )


def test_upstream_tls_context_keeps_verifying_a_registered_non_loopback_nest():
    """The live-box guard, for the proxy's hop: the same conjunction as
    `ws_sslopt`'s, so a real box on a real ACME cert (`_LiveProvider` marks its
    port too) is verified, a marked loopback floor cert is not, and a plain
    `http://` upstream gets no context at all — the hop stays a bare splice."""
    from common import auth

    port = 59_235
    auth.mark_tls_nest(port)
    assert auth.upstream_tls_context(f"https://127.0.0.1:{port}").verify_mode == ssl.CERT_NONE
    assert auth.upstream_tls_context(f"https://example.com:{port}").verify_mode == ssl.CERT_REQUIRED
    assert auth.upstream_tls_context(f"http://127.0.0.1:{port}") is None


def _rust_loopback_gated_kinds() -> set[str]:
    """The kinds `pre_identity_allowlist.rs::requires_loopback_peer` matches.

    A static parse of one `matches!` arm — the same shape, and the same
    soundness argument, as `_registered_admin_bridge_kinds` above.
    """
    import re
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    source = (root / "bins/fauna-nest/src/pre_identity_allowlist.rs").read_text()
    body = source.split("pub fn requires_loopback_peer", 1)[1].split("}", 1)[0]
    return set(re.findall(r'"(fauna\.[a-z0-9_.]+)"', body))


def test_the_loopback_gated_kind_set_matches_nests_own_gate():
    """Class (5)'s body half is pinned to nest by EQUALITY, not restated.

    The harness excludes a test from docker/live *because* nest will refuse the
    kind from a non-loopback peer — the two sets are one fact wearing two hats.
    If a second kind joins the Rust gate, this goes red rather than letting the
    harness keep a stale list and stamp a ❌ against a product behaving exactly
    as designed, which is how the whole mail cluster read before class (5).
    """
    from helpers import nest_surface as ns

    assert set(ns.LOOPBACK_GATED_KINDS) == _rust_loopback_gated_kinds(), (
        "helpers/nest_surface.LOOPBACK_GATED_KINDS and "
        "pre_identity_allowlist.rs::requires_loopback_peer have diverged"
    )


def test_a_test_whose_own_body_reaches_the_enrollment_kind_is_excluded():
    """The half `BRIDGE_SPAWN_FIXTURES` cannot see, and why it needed closing.

    `test_request_enrollment_auto_approves_when_mail_enabled` drives
    `fauna.bridges.request_enrollment` over an anonymous WS client with no bridge
    fixture anywhere in its closure — so class (5)'s fixture half passed it
    through. It was one of three survivors of the docker-mode `tests/api/` slice
    on 2026-08-29, failing on exactly the `remote_enrollment_unsupported` that
    class (5) exists to describe: a false ❌ against a deliberate product gate.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    name = "test_request_enrollment_auto_approves_when_mail_enabled"
    item = _FakeItem(fixturenames=["nest_instance"], name=name)

    verdict = ns.classify(item, nest_mode_mod.parse_nest_mode("docker"))
    assert verdict is not None, "docker must exclude it"
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "fauna.bridges.request_enrollment" in reason, reason
    assert "loopback" in reason, "the reason must name the gate, not a policy"


def test_the_loopback_body_half_leaves_standalone_untouched():
    """The converse, so the new check cannot creep onto the inner loop:
    standalone hosts the enrollment path perfectly well (harness and nest share
    a loopback), and `classify` returns before the scan on that path."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["nest_instance"],
        name="test_request_enrollment_auto_approves_when_mail_enabled",
    )
    assert ns.classify(item, nest_mode_mod.parse_nest_mode("standalone")) is None


def _rust_test_hook_routes() -> set[str]:
    """Every route literal registered by a module gated on `test-hooks`."""
    import re
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    src = root / "bins/fauna-nest/src"
    routes: set[str] = set()
    for path in sorted(src.glob("*.rs")):
        text = path.read_text()
        if '#![cfg(feature = "test-hooks")]' not in text:
            continue
        head = text.split("#[cfg(test)]", 1)[0]
        routes.update(re.findall(r'"(/api/[a-z0-9/_{}.-]+)"', head))
    return routes


def test_the_test_hook_prefix_covers_every_test_hook_route_nest_registers():
    """Class (6) is a PREFIX, and this is what makes that sound.

    The harness cannot match whole routes (it builds them by f-string, and some
    carry path parameters), so it excludes on a prefix — which is only correct if
    every route a `test-hooks`-gated module registers actually sits under it. If
    someone adds a test hook at, say, `/api/v1/debug/…`, this goes red rather
    than letting a docker run collect a test that can only 404, and stamp the ❌
    on the feature instead of on the artifact.
    """
    from helpers import nest_surface as ns

    registered = _rust_test_hook_routes()
    assert registered, "no test-hook routes found — the parse has rotted"
    outside = sorted(r for r in registered if not r.startswith(ns.TEST_HOOK_ROUTE_PREFIX))
    assert not outside, (
        "these test-hook routes sit outside TEST_HOOK_ROUTE_PREFIX, so class (6) "
        f"cannot see the tests that drive them: {outside}"
    )


def test_a_test_reaching_a_test_hook_route_is_excluded_from_docker():
    """The measured case: `test_web_paywall_folder_expired_token_serves_teaser`
    mints an expired capability-URL token through
    `/api/v1/test/web-paywall/expired-token`, whose module says in its own header
    that production never compiles it. It failed the 2026-08-29 docker-mode
    `tests/api/` slice with a bare 404."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["nest_instance"],
        name="test_web_paywall_folder_expired_token_serves_teaser",
    )
    verdict = ns.classify(item, nest_mode_mod.parse_nest_mode("docker"))
    assert verdict is not None, "docker must exclude it"
    klass, reason = verdict.klass, verdict.reason
    assert klass == ns.DECLARED_ABSENCE
    assert "/api/v1/test/" in reason, reason
    assert "test-hooks" in reason, "the reason must name the compile-time gate"


def test_the_test_hook_class_leaves_standalone_and_its_siblings_untouched():
    """Two converses in one, both load-bearing.

    Standalone builds `--features test-hooks`, so the routes are there and the
    class must never fire; and an ordinary test that names no hook route stays
    eligible for docker — a prefix scan that over-reached would quietly shrink
    the mode's coverage while reporting nothing.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    hooked = _FakeItem(
        fixturenames=["nest_instance"],
        name="test_web_paywall_folder_expired_token_serves_teaser",
    )
    assert ns.classify(hooked, nest_mode_mod.parse_nest_mode("standalone")) is None

    ordinary = _FakeItem(fixturenames=["nest_instance"], name="test_download_roundtrip_bytes_identical")
    assert ns.classify(ordinary, nest_mode_mod.parse_nest_mode("docker")) is None


# ── Restart in place: the flip is the provider's act, not the helper's ───────
#
# `stop_nest` was always mode-agnostic — it rides `nest["proc"]`, and docker's
# `_ContainerProc` answers terminate/kill/wait. Its partner could not be: bringing
# a nest BACK meant re-spawning `node_binary` against `config_path`, both declared
# absent in docker, so two whole modules (`test_nest_flip_resilience.py`,
# `test_offline_gate.py`) opted out with `standalone_only` and the two features they
# witness — `connect-and-sign-in`, `offline-aware-controls` — could never be
# stamped against the real artifact.


def test_a_docker_nest_is_restarted_by_its_provider_not_by_respawning_a_binary():
    """The helper asks; it does not reach for a binary that is not there.

    The assertion is carried by the guard rather than by a spy: the handle
    declares docker's real absences, so a helper that fell through to the binary
    path would raise `NestCapabilityError` on `node_binary` — exactly what this
    test saw before the `start_in_place` key existed.
    """
    from common.nest import start_nest_in_place

    mode = nm.parse_nest_mode("docker")
    started = []
    handle = nm.NestHandle(
        {"port": 4321, "start_in_place": lambda: started.append("up")},
        mode,
        absent=nm.absent_capabilities(mode),
    )

    assert start_nest_in_place(handle) is handle
    assert started == ["up"], "the provider's own restart was not the thing called"


def test_standalone_BECOMES_the_supervisor_for_a_nest_that_exits_itself():
    """`resume_after_self_exit`, standalone half — the same delegation shape as
    `start_in_place`, for the other lifecycle the factory reset produces.

    A nest that takes the production reset path `exit(0)`s and expects a
    supervisor to restart it into the boot-time wipe. A `start_nest` binary has
    none, so the harness plays the part: wait for the self-exit, re-spawn.
    """
    import common.nest as cn

    acted = []

    class _Proc:
        def wait(self, timeout=None):
            acted.append(("waited", timeout))
            return 0

    nest = {"proc": _Proc(), "start_in_place": lambda: acted.append(("started",))}
    assert cn.resume_after_self_exit(nest, timeout=30.0) is nest
    assert acted == [("waited", 30.0), ("started",)], acted


def test_a_supervised_nest_does_NOT_get_a_second_supervisor():
    """Docker half, and the point is what it must NOT do.

    s6 restarts the nest inside the container, so a harness that also waited on
    `proc.wait()` would be waiting for the CONTAINER to exit — which it never
    does — and time out at 30 s against a nest that came back in two. The
    provider answers the key with the truth about its own mode, and the *proof*
    that the box came back wiped stays where it already was and where it is the
    same assertion in both modes: the caller's own fresh/unclaimed poll.
    """
    import common.nest as cn

    mode = nm.parse_nest_mode("docker")
    waited = []
    resumed = []

    class _NeverExits:
        def wait(self, timeout=None):
            waited.append(timeout)
            raise AssertionError("the container does not exit when the nest does")

    handle = nm.NestHandle(
        {"port": 4321, "proc": _NeverExits(),
         "resume_after_self_exit": lambda timeout: resumed.append(timeout)},
        mode,
        absent=nm.absent_capabilities(mode),
    )

    assert cn.resume_after_self_exit(handle, timeout=30.0) is handle
    assert resumed == [30.0]
    assert waited == [], "the provider's answer was not the thing called"


def test_a_dedicated_fixture_nest_still_takes_the_binary_path(monkeypatch):
    """The converse, and the reason the fallback is not a guess.

    A nest carrying no `start_in_place` came from one of the dedicated-nest
    fixtures, which spawn a local binary directly and are standalone-only by
    construction. Standalone must pay nothing for this key existing.
    """
    import common.nest as cn

    spawned = []

    def _fake_spawn(cli_args, port, log_path, **kwargs):
        spawned.append((tuple(cli_args), port))
        return "respawned-proc"

    monkeypatch.setattr(cn, "_build_nest_cli_args", lambda *a, **kw: ["nest", "--port", "4321"])
    monkeypatch.setattr(cn, "_spawn_and_wait", _fake_spawn)

    nest = {
        "node_binary": "/tmp/fauna-nest", "port": 4321, "config_path": "/tmp/c.toml",
        "blob_dir": "/tmp/blobs", "handle_domain_seed": "example.test",
        "log_path": "/tmp/n.log",
    }
    out = cn.start_nest_in_place(nest)

    assert out is nest
    assert out["proc"] == "respawned-proc"
    assert spawned == [(("nest", "--port", "4321"), 4321)]


def test_the_container_restart_starts_the_SAME_container(monkeypatch):
    """`docker start <name>`, never `docker run` of a fresh one.

    The distinction is the whole fidelity of the benign flip: the same container
    keeps its published port and its `/data` bind-mount, so the client's
    `node_url` and the nest's identity, DB and claim all survive while the process
    tree is replaced. A `docker run` would be a different container on a different
    port — a factory reset wearing a restart's name.
    """
    import conftest

    calls = []

    class _Result:
        returncode = 0
        stdout = ""
        stderr = ""

    def _fake_run(argv, **kwargs):
        calls.append(list(argv))
        return _Result()

    monkeypatch.setattr(conftest.subprocess, "run", _fake_run)
    conftest._ContainerProc("fauna-nest-axis-4321").start()

    assert calls == [["docker", "start", "fauna-nest-axis-4321"]]


def test_a_failed_container_restart_says_what_docker_said(monkeypatch):
    """A restart that silently did not happen is the worst outcome available.

    The test that follows would fail on a stale connection with no hint that the
    nest never came back, so the raise carries docker's own stderr.
    """
    import conftest

    class _Result:
        returncode = 125
        stdout = ""
        stderr = "Error response from daemon: No such container: fauna-nest-axis-4321"

    monkeypatch.setattr(conftest.subprocess, "run", lambda argv, **kw: _Result())

    with pytest.raises(RuntimeError, match="No such container"):
        conftest._ContainerProc("fauna-nest-axis-4321").start()


# ── `log_path` in docker: the container log, streamed to a host file ────────


def test_the_container_log_stream_writes_to_a_FILE_never_a_pipe(tmp_path, monkeypatch):
    """The one shape that must not be got wrong, and it is convention 13's:
    `docker logs --follow` is a child that writes for as long as the nest lives,
    so a `subprocess.PIPE` nobody drains blocks it the moment ~64 KB fill — and
    a blocked writer looks exactly like a quiet nest, which is the failure this
    stream exists to diagnose.

    It is also why the file is the CAPABILITY rather than an implementation
    detail: `NestLogWatch` seeks by byte offset in a growing file, so what the
    provider must publish is a path, not a stream object.
    """
    import conftest

    spawned = []
    path = str(tmp_path / "nest.log")

    class _FakeProc:
        def terminate(self): pass
        def wait(self, timeout=None): return 0
        def kill(self): pass

    def _fake_popen(argv, **kwargs):
        spawned.append((list(argv), kwargs))
        return _FakeProc()

    monkeypatch.setattr(conftest.subprocess, "Popen", _fake_popen)
    stream = conftest._ContainerLogStream("fauna-nest-axis-4321", path)
    try:
        # `popen_group_kwargs()` arms the run-lifetime reaper, which spawns a
        # child of its own the first time it is asked — so pick the docker call
        # by name rather than by position.
        docker_calls = [c for c in spawned if c[0][:1] == ["docker"]]
        assert len(docker_calls) == 1, spawned
        argv, kwargs = docker_calls[0]
        assert argv == ["docker", "logs", "--follow", "fauna-nest-axis-4321"], (
            "the first attach must replay the container's WHOLE log — a boot "
            "line is exactly what a test watching for the nest coming back reads"
        )
        assert kwargs["stderr"] is conftest.subprocess.STDOUT, (
            "tracing writes to stderr; a stream that dropped it would publish an "
            "empty file for a nest that was logging all along")
        sink = kwargs["stdout"]
        assert sink is not conftest.subprocess.PIPE
        assert hasattr(sink, "fileno"), (
            "an undrained PIPE blocks the writer at ~64 KB — e2e-conventions.md "
            "point 13; the sink must be a real file")
        assert stream.path == path
    finally:
        stream.close()


def test_a_full_replay_TRUNCATES_the_log_while_a_reattach_appends(tmp_path, monkeypatch):
    """Which file mode each attach takes, because the two attaches want
    opposite things and the wrong pairing is silent both ways.

    A FULL replay (no `--since`) owns the file: it happens once per container,
    and a `docker run` retried after a bind collision would otherwise leave the
    dead container's boot lines sitting above the live one's — which a reader
    that scans the whole file rather than from an offset would answer from.
    `test_web_claim_pin_wasm_witness.py` is exactly that reader: it takes the
    box's real `nest_actor_id` off the claim banner, and the wrong box's banner
    is a wrong answer rather than a missing one.

    A RE-ATTACH appends: it continues the same container's log, and a reader's
    recorded byte offset has to keep meaning what it meant.
    """
    import conftest

    modes = []
    path = str(tmp_path / "nest.log")

    class _FakeProc:
        def terminate(self): pass
        def wait(self, timeout=None): return 0
        def kill(self): pass

    def _fake_popen(argv, **kwargs):
        if list(argv)[:1] == ["docker"]:
            modes.append(kwargs["stdout"].mode)
        return _FakeProc()

    monkeypatch.setattr(conftest.subprocess, "Popen", _fake_popen)
    stream = conftest._ContainerLogStream("fauna-nest-axis-4321", path)
    try:
        stream.reattach_since("2026-09-02T10:00:00.000000000Z")
        assert modes == ["wb", "ab"], modes
    finally:
        stream.close()


def test_a_container_restart_reattaches_the_log_stream_AFTER_the_restart_point(monkeypatch):
    """The half a re-attach gets wrong by default, and it would be silent.

    `docker logs --follow` exits when the container stops, so a restarted nest
    logs into nothing unless someone re-attaches — and a naive re-attach replays
    the container's whole history into the same file, which is worse than
    silence: `NestLogWatch` records a byte offset *before* the action under
    test, so a replayed beacon from an EARLIER operation satisfies a wait that
    was supposed to prove this one reached the nest. That is the vacuous-kill
    outcome the watch exists to refuse.

    So the restart carries its own boundary: the moment before `docker start`,
    when the container is down and can therefore log nothing.
    """
    import conftest

    calls = []
    reattached = []

    class _Result:
        returncode = 0
        stdout = ""
        stderr = ""

    class _FakeStream:
        def reattach_since(self, since):
            reattached.append(since)

    monkeypatch.setattr(conftest.subprocess, "run",
                        lambda argv, **kw: (calls.append(list(argv)), _Result())[1])
    conftest._ContainerProc("fauna-nest-axis-4321",
                            log_stream=_FakeStream()).start()

    assert calls == [["docker", "start", "fauna-nest-axis-4321"]]
    assert len(reattached) == 1, "the restart must re-attach exactly once"
    # An RFC3339 UTC instant, which is what `docker logs --since` reads.
    assert reattached[0].endswith("Z") and "T" in reattached[0], reattached[0]


def test_a_failed_container_restart_does_NOT_reattach_the_log_stream(monkeypatch):
    """The boundary is only sound if the container really came back. A
    re-attach after a failed `docker start` would arm a `--since` bound against
    a container that is still down, and the next successful restart's lines
    would fall before it — a stream that is silently blind from then on.
    """
    import conftest

    reattached = []

    class _Result:
        returncode = 125
        stdout = ""
        stderr = "Error response from daemon: No such container"

    class _FakeStream:
        def reattach_since(self, since):
            reattached.append(since)

    monkeypatch.setattr(conftest.subprocess, "run", lambda argv, **kw: _Result())

    with pytest.raises(RuntimeError):
        conftest._ContainerProc("fauna-nest-axis-4321",
                                log_stream=_FakeStream()).start()
    assert reattached == []


# ── Class (7): the test brings its OWN nest IMAGE ──────────────────────────
# The image twin of NEST_BINARY_FIXTURES, and found the same way its own
# docstring predicts: a docker-mode `--feature` sweep on 2026-08-29 SELECTED 81
# of `tests/platform/docker/`'s 89 tests, every one of which boots
# `fauna-nest-test:local` rather than the run's `--nest docker:<ref>` image.


def test_an_own_image_fixture_excludes_the_test_from_docker_and_live():
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["relay_nest", "docker_image"])
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    for other in ("docker", "live"):
        verdict = ns.classify(item, nm.parse_nest_mode(other))
        assert verdict is not None, f"{other} must not adopt a foreign image's outcome"
        klass, reason = verdict.klass, verdict.reason
        assert klass == ns.DECLARED_ABSENCE
        assert "docker_image" in reason
        assert other in reason


def test_the_own_image_reason_names_the_tag_it_actually_boots():
    """A reader who sees the deselect must not have to go find which image."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["docker_image"])
    reason = ns.classify(item, nm.parse_nest_mode("docker")).reason
    assert "fauna-nest-test:local" in reason


def _package_own_image_fixtures():
    """Re-derive the class from `tests/platform/docker/`'s AST.

    Transitive across CALLS and across FIXTURE REQUESTS, and across module
    boundaries: a fixture reaching `docker_build` through another fixture is as
    much an own-image fixture as one calling it directly, and
    `test_bridge_rekey_rotation.py` imports `test_bridge_enrollment_pop`'s
    `serving_nest` rather than defining its own.

    Returns `(fixtures, requests_of)` — the second so the redundancy pin below
    can ask what a dropped name would have carried.
    """
    import ast
    from pathlib import Path

    pkg = Path(__file__).resolve().parents[1] / "tests" / "platform" / "docker"
    funcs = {}
    for source in sorted(pkg.glob("*.py")):
        tree = ast.parse(source.read_text())
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                funcs.setdefault(node.name, node)

    def called_names(node):
        out = set()
        for sub in ast.walk(node):
            if isinstance(sub, ast.Call):
                target = sub.func
                if isinstance(target, ast.Name):
                    out.add(target.id)
                elif isinstance(target, ast.Attribute):
                    out.add(target.attr)
        return out

    def requested_names(node):
        return {a.arg for a in node.args.args} | {a.arg for a in node.args.kwonlyargs}

    def is_fixture(node):
        return any("fixture" in ast.unparse(d) for d in node.decorator_list)

    # Seed on the tag itself, not on `docker_build`: a fixture can boot the
    # image without ever building it — `start_container_with_ports` defaults
    # `image=IMAGE_TAG`, and two ACME modules take exactly that route, which a
    # `docker_build`-only seed missed (found by this pin, 2026-08-29, when the
    # collection it predicted still selected them).
    def names_in(node):
        return (
            {n.id for n in ast.walk(node) if isinstance(n, ast.Name)}
            | {n.attr for n in ast.walk(node) if isinstance(n, ast.Attribute)}
        )

    reaches = {n for n, f in funcs.items() if "IMAGE_TAG" in names_in(f)}
    grew = True
    while grew:
        grew = False
        for name, node in funcs.items():
            if name in reaches:
                continue
            if (called_names(node) | requested_names(node)) & reaches:
                reaches.add(name)
                grew = True

    fixtures = {n for n in reaches if is_fixture(funcs[n])}
    return fixtures, {n: requested_names(funcs[n]) for n in fixtures}


def _fixture_names_defined_outside_the_docker_package():
    """Every fixture name defined anywhere in the e2e tree but that package."""
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    pkg = root / "tests" / "platform" / "docker"
    names = set()
    for source in sorted(root.rglob("*.py")):
        if source.parent == pkg:
            continue
        try:
            tree = ast.parse(source.read_text())
        except SyntaxError:
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if any("fixture" in ast.unparse(d) for d in node.decorator_list):
                names.add(node.name)
    return names


def _start_options_per_definition() -> dict[str, dict[str, frozenset]]:
    """Fixture name → {file: the start options that definition literally passes}.

    Deliberately rooted at all THREE nest-start entry points, including bare
    `start_nest` — which the table's own derivation (`_derive_start_options_tree`)
    does not follow, because an unrouted fixture has no business being in the
    table. That gap is exactly what this scan exists to see: a fixture invisible
    to the derivation can still SHARE A NAME with one the derivation covers, and
    then the table's entry is applied to it by `classify` all the same.
    """
    import ast
    from collections import defaultdict
    from pathlib import Path

    starters = {"start_nest", "_make_nest", "_start_dedicated_nest"}
    # Positional plumbing, not a request for anything (`_derive_start_options_tree`
    # drops the same shapes, and a falsy literal is the documented "wants the
    # default" spelling).
    plumbing = {None, "port", "tmp_dir", "config_name", "label"}

    root = Path(__file__).resolve().parents[1]
    per: dict[str, dict[str, frozenset]] = defaultdict(dict)
    for source in sorted(root.rglob("*.py")):
        try:
            tree = ast.parse(source.read_text())
        except SyntaxError:
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if not any("fixture" in ast.unparse(d) for d in node.decorator_list):
                continue
            asked = set()
            for call in ast.walk(node):
                if not isinstance(call, ast.Call):
                    continue
                fn = getattr(call.func, "id", None) or getattr(call.func, "attr", None)
                if fn not in starters:
                    continue
                for kw in call.keywords:
                    if kw.arg in plumbing:
                        continue
                    try:
                        if ast.literal_eval(kw.value) in (False, None, "", 0):
                            continue
                    except Exception:
                        pass
                    asked.add(kw.arg)
            rel = source.relative_to(root.parent).as_posix()
            per[node.name][rel] = frozenset(asked)
    return per


def test_a_shared_fixture_name_never_carries_two_different_option_sets():
    """`FIXTURE_START_OPTIONS` keys on the BARE fixture name, so two fixtures
    sharing a name get ONE entry — and it must be true of both.

    The sibling pin one class down made this mechanical for `OWN_IMAGE_FIXTURES`
    and its docstring states the gap plainly: *"the other two sets assert 'none
    of these names collides today' in prose; this one checks it."* This is that
    check for the options table.

    **A collision is not itself the defect, and an over-strict pin here would be
    wrong.** Several names are shared today with every definition asking for
    nothing (`head_nest`, `nest`, `scenario`) — one entry, true of each, costing
    exactly zero. What must never happen is a DISAGREEMENT, and there were TWO
    when this was written:

    * `handled_nest` — the routed conftest fixture (no options) and
      `test_onboarding_launch_routing_smoke.py`'s module-local one, which passes
      `handle_domain_seed` + `serve_tls`. The table said `frozenset()` about
      both. Found by hand, and the reason this pin was written.
    * `unclaimed_nest` — `test_onboarding_localhost.py` (nothing) against
      `test_onboarding_logged_in_terminal_empty_store.py`
      (`handle_domain_seed`, `serve_tls`, `unclaimed`). **Found by this pin on
      its first run**, after the hand search had already stopped at one. That is
      the argument for the pin in a sentence: the prose claim it replaces had
      been wrong in two places at once, and hand-checking found half.

    Why a masked wrong entry still has to fail. Both fixtures also requested
    `nest_binary`, so the closure rule excluded their tests anyway and nothing
    went visibly wrong — but the arm that ROUTES such a fixture is precisely the
    arm that removes that mask, and the entry would then have claimed "asks for
    nothing" about a fixture asking for an option docker cannot honour. That is
    a setup-time `NestModeError` where a collection-time class (4) was
    available: the ❌-against-working-product-code outcome the arm's ordering
    exists to prevent. Both fixes were the table's own precedent — a name only
    its module uses (`smoke_handled_nest`, `empty_store_unclaimed_nest`), after
    the two `unclaimed_nest` renames its comment already records.
    """
    from helpers import nest_surface as ns

    per = _start_options_per_definition()
    disagreeing = {
        name: {f: sorted(o) for f, o in sites.items()}
        for name, sites in sorted(per.items())
        if len(set(sites.values())) > 1
    }
    assert not disagreeing, (
        "these fixture names are defined more than once with DIFFERENT start "
        "options, so one definition's options are applied to the other's "
        "tests:\n"
        + "\n".join(f"  {n}: {files}" for n, files in disagreeing.items())
        + "\nGive the module-local one a name only its module uses, then check "
        "its entry. A shared name whose definitions agree is fine and is not "
        "reported here."
    )
    # ...and the entry, where there is one, must be that agreed set: a name the
    # table describes must be described correctly for every fixture wearing it.
    for name, declared in sorted(ns.FIXTURE_START_OPTIONS.items()):
        sites = per.get(name)
        if not sites or len(sites) < 2:
            continue  # single definition: the derivation pin already owns it
        agreed = next(iter(set(sites.values())))
        assert agreed == frozenset(declared), (
            f"{name!r} is defined in {sorted(sites)} — all asking for "
            f"{sorted(agreed)} — but the table declares {sorted(declared)}."
        )


def test_the_own_image_fixture_list_matches_the_platform_docker_package():
    """`OWN_IMAGE_FIXTURES` is derived from the package's AST, never hand-read.

    Same discipline as `test_the_local_nest_fixture_list_matches_conftest`, minus
    the names that collide with a fixture defined elsewhere (see the next two
    pins) — matching is by name, and this module's docstring records what a
    shared name costs.
    """
    from helpers import nest_surface as ns

    derived, _ = _package_own_image_fixtures()
    expected = derived - _fixture_names_defined_outside_the_docker_package()
    assert expected == set(ns.OWN_IMAGE_FIXTURES), (
        "tests/platform/docker/'s own-image fixtures and "
        "nest_surface.OWN_IMAGE_FIXTURES have diverged.\n"
        f"  only in the package: {sorted(expected - set(ns.OWN_IMAGE_FIXTURES))}\n"
        f"  only in the list: {sorted(set(ns.OWN_IMAGE_FIXTURES) - expected)}\n"
        "A fixture that boots fauna-nest-test:local is not a witness of the "
        "run's nest in any mode — the record would name a commit, a digest and "
        "a mode the test never touched (feature-catalog.md § The ledger). Add "
        "it to OWN_IMAGE_FIXTURES."
    )


def test_no_own_image_name_collides_with_a_fixture_outside_the_package():
    """The hazard this module's docstring names, made mechanical.

    `classify` matches on fixture NAMES, and a name shared with a fixture
    somewhere else deselects strangers with a reason that reads perfectly
    plausible — the exact shape that cost the harness a whole suite when `app`
    collided with a parametrized argname. The other two sets assert "none of
    these names collides today" in prose; this one checks it.
    """
    from helpers import nest_surface as ns

    collisions = set(ns.OWN_IMAGE_FIXTURES) & _fixture_names_defined_outside_the_docker_package()
    assert not collisions, (
        f"{sorted(collisions)} name a fixture both inside and outside "
        "tests/platform/docker/, so matching on the name would deselect tests "
        "that bring no image of their own. Drop the name from "
        "OWN_IMAGE_FIXTURES — but only after checking the test below, which "
        "asks whether anything is actually lost by dropping it."
    )


def test_every_name_dropped_for_colliding_is_redundant_in_its_own_closure():
    """Dropping a colliding name must cost no coverage, and that is checkable.

    A dropped fixture is safe only because it *requests* a surviving own-image
    fixture, which `item.fixturenames` (the transitive closure) therefore still
    carries. Today the one dropped name is `nest`, defined in both
    `test_docker_e2e.py` and `test_spa_serving.py`, and both request
    `docker_image`. If a future collision has no such cover, this fails rather
    than letting the class quietly shrink.
    """
    from helpers import nest_surface as ns

    derived, requests_of = _package_own_image_fixtures()
    dropped = derived - set(ns.OWN_IMAGE_FIXTURES)
    for name in sorted(dropped):
        cover = requests_of[name] & set(ns.OWN_IMAGE_FIXTURES)
        assert cover, (
            f"{name!r} was dropped from OWN_IMAGE_FIXTURES for colliding with a "
            f"fixture outside tests/platform/docker/, but it requests no "
            f"surviving own-image fixture — so its tests would no longer be "
            f"classified out, and would write ledger records naming an artifact "
            f"they never touched. Give it a non-colliding name instead."
        )


def test_the_own_image_class_and_the_binary_class_do_not_overlap():
    """Two different artifacts, two different reasons; a reader gets the right one."""
    from helpers import nest_surface as ns

    assert not set(ns.OWN_IMAGE_FIXTURES) & set(ns.NEST_BINARY_FIXTURES)
    assert not set(ns.OWN_IMAGE_FIXTURES) & set(ns.LOCAL_NEST_FIXTURES)


# ── The tag the own-artifact venue actually boots ──────────────────────────


def test_a_current_image_says_nothing():
    """A quiet run is the common case, so it must cost no noise."""
    import datetime

    from tests.platform.docker import helpers as dh

    dh._age_reported = False
    fresh = (datetime.datetime.now() - datetime.timedelta(days=1)).isoformat()
    assert dh.report_image_age(fresh) is None


def test_a_stale_image_names_the_drift_the_remedy_and_the_measured_cost():
    """The one thing a reader needs is that a red here may not be the product."""
    import datetime

    from tests.platform.docker import helpers as dh

    dh._age_reported = False
    old = (datetime.datetime.now() - datetime.timedelta(days=15)).isoformat()
    message = dh.report_image_age(old)
    assert message is not None, "a 15-day-old tag must not pass in silence"
    assert "15 days ago" in message
    assert dh.IMAGE_TAG in message
    assert "docker pull ghcr.io/faunasocial/nest:dev" in message
    assert "signature_failed" in message, (
        "the message must name the shape the reader will actually see, or it "
        "will be read as boilerplate and skipped"
    )
    dh._age_reported = False


def test_the_warning_is_once_per_run_not_once_per_fixture():
    """~40 module-scoped fixtures call the door; 40 copies is noise, not signal."""
    import datetime

    from tests.platform.docker import helpers as dh

    dh._age_reported = False
    old = (datetime.datetime.now() - datetime.timedelta(days=15)).isoformat()
    assert dh.report_image_age(old) is not None
    assert dh.report_image_age(old) is None
    dh._age_reported = False


def test_an_unparseable_created_stamp_is_not_fatal():
    """A docker output shape change must never take the suite down with it."""
    from tests.platform.docker import helpers as dh

    dh._age_reported = False
    assert dh.report_image_age("not-a-timestamp") is None
    assert dh.report_image_age("") is None


# ── The gate tally's machine-readable twin ─────────────────────────────────
# `record_gate` has always tallied (mode, test_id, class, reason) and the
# terminal summary prints it grouped by class. That answers "what did this
# sweep not cover, and why", but not the question the catalog actually asks:
# **which feature PAGES does each class blank?** A cell may only be stamped by
# a run that collected the feature's whole tagged set (feature-catalog.md
# § Cell semantics), so a page with one gated witness can never carry a stamp
# for this mode — and the class that gated that witness is the whole diagnosis:
# a fact about the artifact (the cell is honestly blank forever) or harness
# debt (`mode_unbuilt`, closable).
#
# The join needs two things the tally dropped on the floor: the item's
# **repo-relative** node id — the string the contract cites — and the slugs its
# `@pytest.mark.feature` markers name. Both are known at the moment of the
# gate; neither is recoverable afterwards, because the nest axis deselects
# before `_apply_feature_axis` ever tags anything.


def test_a_gate_hit_carries_the_repo_id_and_the_slugs_it_blanks():
    """Without these two fields the class tally cannot name a single page."""
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate(
        "docker", "tests/test_nostr.py::test_x[tui-docker]", ns.DECLARED_ABSENCE,
        "spawns a bridge",
        repo_id="tests/e2e-unified/tests/test_nostr.py::test_x",
        features=("nostr", "bridges"),
    )
    (hit,) = ns.gate_hits()
    assert hit.mode == "docker"
    assert hit.repo_id == "tests/e2e-unified/tests/test_nostr.py::test_x"
    assert hit.features == ("nostr", "bridges")
    assert hit.klass == ns.DECLARED_ABSENCE
    ns.reset_gate_hits()


def test_a_runtime_declaration_still_records_without_them():
    """`mode_unbuilt` & co. fire from action code with no item in scope.

    They must stay callable — the two new fields are collection-time knowledge,
    not a new requirement on every caller.
    """
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "tests/test_x.py::test_y", ns.MODE_UNBUILT, "no seam")
    (hit,) = ns.gate_hits()
    assert hit.repo_id == ""
    assert hit.features == ()
    ns.reset_gate_hits()


def test_the_gates_payload_carries_both_halves_of_the_join():
    """Gated tests alone cannot tell a blanked page from an unwitnessed one.

    A contract citation that is neither collected nor gated is a THIRD thing —
    an app-axis deselection or a stale citation — and reporting it as a mode
    exclusion would blame the artifact for a harness or catalog fact.
    """
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate(
        "docker", "tests/test_a.py::test_gated[tui-docker]", ns.DECLARED_ABSENCE,
        "boots its own image",
        repo_id="tests/e2e-unified/tests/test_a.py::test_gated",
        features=("nostr",),
    )
    payload = ns.gates_payload(
        mode="docker",
        collected=["tests/e2e-unified/tests/test_a.py::test_ran"],
    )
    assert payload["mode"] == "docker"
    assert payload["collected"] == ["tests/e2e-unified/tests/test_a.py::test_ran"]
    (gated,) = payload["gated"]
    assert gated["repo_id"] == "tests/e2e-unified/tests/test_a.py::test_gated"
    assert gated["class"] == ns.DECLARED_ABSENCE
    assert gated["features"] == ["nostr"]
    assert gated["reason"] == "boots its own image"
    ns.reset_gate_hits()


def test_the_payload_is_json_round_trippable():
    """It is written to a file and read by a script that never imports pytest."""
    import json

    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.DECLARED_ABSENCE, "why",
                   repo_id="tests/e2e-unified/t.py::x", features=("a",))
    payload = ns.gates_payload(mode="docker", collected=["tests/e2e-unified/t.py::y"])
    assert json.loads(json.dumps(payload)) == payload
    ns.reset_gate_hits()


def test_collected_ids_are_deduplicated_and_sorted():
    """One test parametrized per app collapses to the one line a contract cites."""
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    payload = ns.gates_payload(mode="docker", collected=["b::t", "a::t", "b::t"])
    assert payload["collected"] == ["a::t", "b::t"]
    ns.reset_gate_hits()


# ── Which of the seven ratified classes fired ──────────────────────────────
# `klass` is the three-value SKIP vocabulary shared with `app_surface`
# (`mode_unbuilt` / `declared_absence` / `skip_environment`) — it says what the
# run should *do* about the exclusion. It is not the ratified exclusion class,
# and six of the seven collapse into `declared_absence`: a measured docker
# collection gates 194 tests and every single one reports that same word.
#
# The audit's whole question is which of the seven blanks a given page — a fact
# about the artifact (keep; the cell is honestly blank) or harness debt
# (closable) — so the verdict has to name the rule it applied. Recovering it by
# matching the reason PROSE would be a second source of truth for a decision
# `classify` already made unambiguously at each of its returns.


def test_the_verdict_names_the_ratified_rule_not_only_the_skip_class():
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_binary"])
    verdict = ns.classify(item, nm.parse_nest_mode("docker"))
    assert verdict.klass == ns.DECLARED_ABSENCE
    assert verdict.rule == ns.RULE_NEST_BINARY


def test_each_rule_is_reachable_and_distinct():
    """Two rules answering the same id would silently merge in the audit."""
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    cases = {
        ns.RULE_MODE_MARKER: _FakeItem(markers=[_FakeMarker("standalone_only")]),
        ns.RULE_NEST_BINARY: _FakeItem(fixturenames=["nest_binary"]),
        ns.RULE_OWN_IMAGE: _FakeItem(
            fixturenames=[sorted(ns.OWN_IMAGE_FIXTURES)[0]]),
        ns.RULE_BRIDGE_SPAWN: _FakeItem(
            fixturenames=[sorted(ns.BRIDGE_SPAWN_FIXTURES)[0]]),
    }
    for expected, item in cases.items():
        verdict = ns.classify(item, docker)
        assert verdict is not None, f"{expected} became unreachable"
        assert verdict.rule == expected, f"{expected} answered {verdict.rule}"


def test_every_rule_id_is_declared_in_the_registry():
    """The audit groups by rule; an unregistered one would print as a bare id.

    Derived from the module's own `RULE_*` constants rather than a hand-listed
    two, so a rule added without a label fails here instead of reaching a report
    as a bare identifier — which is what the registry's docstring already
    promises this test does.
    """
    from helpers import nest_surface as ns

    declared = {
        getattr(ns, n) for n in dir(ns)
        if n.startswith("RULE_") and n != "RULE_LABELS"
        and isinstance(getattr(ns, n), str)
    }
    assert declared <= set(ns.RULE_LABELS), (
        f"rule ids with no label: {sorted(declared - set(ns.RULE_LABELS))}"
    )
    for rule, label in ns.RULE_LABELS.items():
        assert isinstance(label, str) and label, rule


# ── Class (4): a start option the mode's provider cannot honour ─────────────
# Ruling (3)'s seam, classifier half. It is currently SHADOWED for every real
# test — each of these fixtures also requests `nest_binary`, and that closure is
# checked first — which is the point: the class must exist before arm 1 removes
# the request, or the routing turns a clean collection-time exclusion into a
# setup-time `NestModeError` from the provider's backstop. `all_rules` is the
# audit's view and sees it today.

#: The fixtures whose `handle_domain_seed` is their OWN dial authority — routed
#: 2026-09-02 on the strength of `common.nest.OWN_DIAL_AUTHORITY`, and the only
#: sites that may pass that sentinel.
_OWN_AUTHORITY_FIXTURES = (
    "caldav_cross_nest_peer",
    "cross_nest_foreign",
    "cross_nest_foreign_ephemeral",
    "smoke_handled_nest",
)

#: Every fixture whose start options include `handle_domain_seed` — class (4)'s
#: permanent residents, the two populations ruling (3) names (an IP-literal
#: authority above; an `unclaimed=True` nest whose claim is the CLIENT's).
#:
#: Derived from the table rather than hand-listed, so a fixture that gains or
#: loses the seed arrives here without an edit — the point of the pins below is
#: what routing bought, not which names are in the set.
def _handle_domain_seed_fixtures():
    from helpers import nest_surface as ns

    return sorted(
        name for name, opts in ns.FIXTURE_START_OPTIONS.items()
        if "handle_domain_seed" in opts
    )


def test_the_own_authority_sentinel_resolves_to_the_nests_own_dial_authority():
    """`OWN_DIAL_AUTHORITY` → `"<dial_host>:<port>"`, and nothing else moves.

    The sentinel is what let the four IP-literal fixtures route at all, and the
    fact it rests on is small enough to be worth pinning directly rather than
    inferring from an integration run: the authority a nest ADVERTISES as its
    handle domain and the authority its `url` tells a client to DIAL are composed
    from the same two values, in the same function, three lines apart. A resolver
    that read anything else — a hard-coded `127.0.0.1`, the bind host, the port
    before its default was applied — would produce a nest whose handle resolves
    somewhere its own URL does not point, and every consumer's symptom would be a
    discovery miss that looks like a product bug.

    The pass-through half is the widening guarantee: ~40 sites pass a plain
    domain string and one passes `None`, and none of them can notice the
    sentinel exists.
    """
    from common.nest import OWN_DIAL_AUTHORITY, resolve_handle_domain_seed

    assert resolve_handle_domain_seed(
        OWN_DIAL_AUTHORITY, "127.0.0.1", 13042) == "127.0.0.1:13042"
    # Not hard-coded to loopback: a `dial_host` nest advertises the authority a
    # client actually dials, which is the whole meaning of the sentinel.
    assert resolve_handle_domain_seed(
        OWN_DIAL_AUTHORITY, "192.0.2.7", 8443) == "192.0.2.7:8443"

    assert resolve_handle_domain_seed("fauna.test", "127.0.0.1", 1) == "fauna.test"
    assert resolve_handle_domain_seed(None, "127.0.0.1", 1) is None


def test_the_own_authority_sentinel_is_only_passed_by_fixtures_that_declare_it():
    """Passing `OWN_DIAL_AUTHORITY` IS asking for `handle_domain_seed`, so a site
    that passes it and is not in `FIXTURE_START_OPTIONS` under that option is a
    fixture the classifier cannot see asking for a standalone-only knob — the
    setup-time `NestModeError` this axis exists to turn into a collection-time
    declared absence.

    The derivation pin above already grades the kwarg; this one grades the
    sentinel's own spread, which is the thing a future author is likelier to
    copy. Deliberately an EQUALITY over the fixtures that use it rather than a
    subset check: a fifth site is fine, and must say so here.
    """
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    passers = set()
    for path, source in _walkable_sources(root).items():
        if path.name == Path(__file__).name:
            continue
        try:
            tree = ast.parse(source)
        except SyntaxError:
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for sub in ast.walk(node):
                if not isinstance(sub, ast.Call):
                    continue
                for kw in sub.keywords:
                    if (kw.arg == "handle_domain_seed"
                            and isinstance(kw.value, ast.Name)
                            and kw.value.id == "OWN_DIAL_AUTHORITY"):
                        passers.add(node.name)

    assert passers == set(_OWN_AUTHORITY_FIXTURES), (
        "the sites passing OWN_DIAL_AUTHORITY and the recorded set have "
        f"diverged.\n  only in the tree: {sorted(passers - set(_OWN_AUTHORITY_FIXTURES))}"
        f"\n  only in the record: {sorted(set(_OWN_AUTHORITY_FIXTURES) - passers)}"
    )

    from helpers import nest_surface as ns
    for name in _OWN_AUTHORITY_FIXTURES:
        assert "handle_domain_seed" in ns.FIXTURE_START_OPTIONS.get(name, frozenset()), (
            f"{name} passes the sentinel but does not declare handle_domain_seed"
        )


def test_a_handle_domain_seed_fixture_left_the_binary_closure():
    """What routing the class (4) residents actually bought, and the only way to
    check it: their DEFINITIONS no longer name a nest binary fixture.

    None of these can ever collect in docker — `handle_domain_seed` is the
    `--handle-domain` boot flag and no provider but standalone honours it — so
    "does it run in a container" is the wrong question to ask of them and would
    make this pin vacuous. The question that is not vacuous is which REASON the
    audit gives: while a fixture requests `nest_binary`, `_verdicts` reports the
    MIXED `nest_binary` class first (its ratified order puts the closable class
    ahead of the facts), so seven permanent residents were counted as closable
    work in every mode audit. Dropping the request is what moves them behind the
    named option.

    Checked on the definition rather than through `classify`, because a
    `_FakeItem` carries whatever closure the test hands it — asserting
    `nest_binary` is absent from a closure this file composed would assert only
    that this file did not put it there.

    ⚠ **`LOCAL_NEST_FIXTURES` residents are exempt, and the exemption is an
    equality so it cannot grow quietly.** A fixture in that set spawns a host
    process beside its nest — `unclaimed_mail_nest_ui`'s unapproved MTA — so it
    is standalone-only by a rule that does not care what options it passes, and
    its binary request is ENTAILED rather than incidental: the audit already
    reports it as a fact. Routing its nest half is the venue seam's work
    (`_start_mail_venue`, a provider METHOD — ruling (3)), not this arm's, and
    demanding it here would push a mail venue through the wrong seam to satisfy
    a pin. What the exemption must not become is a place to park an ordinary
    fixture, hence the equality.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]
    seeds = set(_handle_domain_seed_fixtures())
    exempt = seeds & set(ns.LOCAL_NEST_FIXTURES)
    assert exempt == {"unclaimed_mail_nest_ui"}, (
        "the host-spawn exemption from this pin has moved. A fixture may sit "
        "here only because a STRONGER structural rule (LOCAL_NEST_FIXTURES — it "
        "spawns a host process beside its nest) already grades it, which makes "
        "its binary request entailed. Anything else belongs behind "
        f"`_start_dedicated_nest`: {sorted(exempt)}"
    )
    wanted = seeds - exempt
    seen = {}
    for source in _walkable_sources(root).values():
        try:
            tree = ast.parse(source)
        except SyntaxError:
            continue
        for node in ast.walk(tree):
            if (isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                    and node.name in wanted
                    and any("fixture" in ast.unparse(d)
                            for d in node.decorator_list)):
                params = {a.arg for a in node.args.args}
                seen.setdefault(node.name, set()).update(
                    params & set(ns.NEST_BINARY_FIXTURES))

    assert set(seen) == wanted, (
        "a fixture declaring handle_domain_seed has no definition the tree can "
        f"find: {sorted(wanted - set(seen))}"
    )
    offenders = {n: sorted(b) for n, b in seen.items() if b}
    assert not offenders, (
        "a handle_domain_seed fixture still requests a nest BINARY, so the "
        "audit reports it under the MIXED `nest_binary` class rather than the "
        f"option it actually cannot have: {offenders}. Route it through "
        "`_start_dedicated_nest`, which resolves the binary lazily."
    )


def test_a_fixture_wanting_an_unhonoured_option_is_class_4_in_docker():
    """The exemplar has to be a fixture wanting a PRODUCT CHOICE, not artifact
    wiring. It used to be `self_signed_nest` (`serve_tls`) and arm 5 correctly
    turned that red by honouring the option — the same shape arm 1 hit, and the
    first thing to check whenever a pin here goes red is whether the rule broke
    or the exemplar simply stopped being an example.

    `web_hosting_nest` was the exemplar for one arm, wanting `handle_domain` —
    and arm 4's second half turned it green the same way arm 5 turned
    `self_signed_nest` green: by making the option honourable rather than by
    weakening the rule. Its domain now rides the claim (`claim_domain`), which
    every mode makes.

    `unclaimed_mail_nest_ui` replaces it and is the stable exemplar this pin
    wanted all along, because its exclusion is structural rather than pending
    work: it wants `handle_domain_seed`, the `--handle-domain` BOOT flag, and it
    wants it because the nest is deliberately left unclaimed for the client to
    claim through the UI. There is no wire act to carry a domain on a nest nobody
    has claimed, so no provider can ever honour this one.

    **Which is why the rule it asserts changed on 2026-09-02, and the change is
    this docstring finally being mechanised rather than a weakening.** The
    paragraph above had said "structural rather than pending work" and "no
    provider can ever honour this one" for a month while the pin asserted the
    class that MEANS pending work; the split into `permanent_option` gives the
    prose a rule id. If a future arm turns this red, the first question is still
    the one at the top: did the rule break, or did the exemplar stop being one —
    and for THIS exemplar the second is not available, since a nest nobody has
    claimed can carry no domain over the wire in any mode.
    """
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    item = _FakeItem(fixturenames=["unclaimed_mail_nest_ui"])
    assert ns.RULE_PERMANENT_OPTION in ns.all_rules(item, docker)

    verdict = next(v for v in ns._verdicts(item, docker)
                   if v.rule == ns.RULE_PERMANENT_OPTION)
    assert verdict.klass == ns.DECLARED_ABSENCE
    assert "handle_domain_seed" in verdict.reason, (
        "the reason must name the option"
    )
    assert "docker" in verdict.reason, "the reason must name the mode"


def test_a_zero_option_fixture_is_not_gated_on_options():
    """The seven-odd zero-option fixtures are the first able to run in a
    container, so gating them on options would defeat the arm entirely.

    `dedicated_mail_nest` is the sharp case: it reaches `_make_nest` through an
    impl helper that forwards two option parameters, and it passes neither.
    """
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    for fixture in ("second_nest", "third_nest", "stoppable_nest",
                    "dedicated_mail_nest"):
        rules = ns.all_rules(_FakeItem(fixturenames=[fixture]), docker)
        assert ns.RULE_UNSUPPORTED_OPTION not in rules, (
            f"{fixture} asks for no start options, so nothing about options "
            "may exclude it"
        )


def test_the_option_rule_reads_the_PROVIDER_declaration_not_a_second_list():
    """Ruling (3): a provider that grows an option un-excludes every fixture
    needing only what it now supports, **with no table edit**. That is only true
    if the classifier subtracts the provider's own declaration."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    # `rotatable_tls_nest`, not `self_signed_nest`: the latter also seeds
    # `cors_origins` (a browser dials it raw), so it no longer wants ONLY `serve_tls`.
    item = _FakeItem(fixturenames=["rotatable_tls_nest"])

    class _GrownProvider:
        name = "docker"
        builds_local_nest = False
        supported_options = frozenset({"serve_tls"})

    saved = nest_mode_mod._PROVIDERS.get("docker")
    try:
        nest_mode_mod.register_provider("docker", _GrownProvider())
        assert ns.RULE_UNSUPPORTED_OPTION not in ns.all_rules(item, docker), (
            "a provider that declares `serve_tls` must un-exclude the fixture "
            "that only wanted `serve_tls`, with no edit to FIXTURE_START_OPTIONS"
        )
    finally:
        if saved is not None:
            nest_mode_mod.register_provider("docker", saved)

    # ...and the un-exclusion is specific, not a blanket off-switch: a fixture
    # wanting a SECOND option it still does not declare stays gated on that one.
    class _PartlyGrownProvider(_GrownProvider):
        supported_options = frozenset({"serve_tls"})

    try:
        nest_mode_mod.register_provider("docker", _PartlyGrownProvider())
        verdict = next(v for v in ns._verdicts(
            _FakeItem(fixturenames=["spki_pinned_nest"]), docker)
            if v.rule == ns.RULE_UNSUPPORTED_OPTION)
        assert "dial_host" in verdict.reason
        assert "serve_tls" not in verdict.reason.split("which nest mode")[0], (
            "an option the provider now honours must not be reported unsupported"
        )
    finally:
        if saved is not None:
            nest_mode_mod.register_provider("docker", saved)


def test_standalone_is_never_gated_on_start_options():
    """Standalone's provider IS `_make_nest`, so it honours the whole
    vocabulary by construction — and the classifier returns before any fixture
    rule in that mode anyway. Pinned so a future edit cannot make the default
    inner loop pay for the axis."""
    from helpers import nest_surface as ns

    standalone = nm.parse_nest_mode("standalone")
    for fixture in sorted(ns.FIXTURE_START_OPTIONS):
        assert ns.classify(_FakeItem(fixturenames=[fixture]), standalone) is None


def test_a_mode_with_no_registered_provider_gates_nothing_on_options():
    """The classifier must not be the thing that raises about an unregistered
    provider: `pytest_configure` already refuses an unbuilt mode, and guessing a
    set here is wrong in both directions (empty over-excludes, full silently
    disables the class)."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    saved = nest_mode_mod._PROVIDERS.pop("docker", None)
    try:
        rules = ns.all_rules(_FakeItem(fixturenames=["self_signed_nest"]), docker)
        assert ns.RULE_UNSUPPORTED_OPTION not in rules
    finally:
        if saved is not None:
            nest_mode_mod._PROVIDERS["docker"] = saved


def test_a_recorded_gate_carries_the_rule_through_to_the_payload():
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.DECLARED_ABSENCE, "why",
                   repo_id="tests/e2e-unified/t.py::x", features=("a",),
                   rule=ns.RULE_TEST_HOOKS)
    (gated,) = ns.gates_payload(mode="docker", collected=[])["gated"]
    assert gated["rule"] == ns.RULE_TEST_HOOKS
    ns.reset_gate_hits()


# ── the report says which SELECTION it audited ──────────────────────────────
# Measured the hard way, 2026-08-29: the same catalog and the same mode answered
# "15 of 105, and 47 pages are not a mode question" under `--app tui` and
# "18 of 105, and 14 are not" under `--app tui --include-independent`. The
# difference is 126 `tests/api/` citations an explicit `--app` deselects unless
# the independent suites are added back — nothing to do with docker at all.
#
# A report that does not state its own selection invites exactly that misreading,
# and the misreading is expensive: the first numbers said the mode-attributable
# set was the SMALLER half of the partial catalog, when it is the larger.


def test_the_payload_records_the_selection_it_was_collected_under():
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    payload = ns.gates_payload(
        mode="docker", collected=[],
        selection=["tests/", "--nest", "docker", "--app", "tui"])
    assert payload["selection"] == ["tests/", "--nest", "docker", "--app", "tui"]
    ns.reset_gate_hits()


def test_the_selection_defaults_to_empty_not_missing():
    """A payload from before this field must still load and render."""
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    assert ns.gates_payload(mode="docker", collected=[])["selection"] == []
    ns.reset_gate_hits()


# ── every rule that applies, not only the deciding one ─────────────────────
# `classify` answers the run's question — "exclude this test, and tell the
# reader why" — and one reason is the right answer there. The AUDIT asks a
# different one: is this page blanked by closable debt or by a fact about the
# artifact? For that, the first match is not enough and is systematically
# biased, because the ratified order puts nest_binary (mixed) ahead of
# bridge_spawn (a fact): `dedicated_mail_nest` requests `nest_binary` AND spawns
# MTA+MDA on the host, so its tests report the closable class while being
# blocked by the immovable one. Measured 2026-08-29: that shape reached 3 of the
# 5 pages the narrow run called nest_binary-sole-blocked.
#
# So the rules are one lazily-evaluated sequence: `classify` takes the first,
# `all_rules` takes them all. One definition of each rule, no second classifier
# to drift.


def test_all_rules_reports_every_match_in_ratified_order():
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_binary",
                                   sorted(ns.BRIDGE_SPAWN_FIXTURES)[0]])
    rules = ns.all_rules(item, nm.parse_nest_mode("docker"))
    assert ns.RULE_NEST_BINARY in rules
    assert ns.RULE_BRIDGE_SPAWN in rules
    assert rules.index(ns.RULE_NEST_BINARY) < rules.index(ns.RULE_BRIDGE_SPAWN), (
        "the ratified order is the order classify applies them in"
    )


def test_classify_still_answers_the_FIRST_match_only():
    """The run's message must not change: one exclusion, one reason."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_binary",
                                   sorted(ns.BRIDGE_SPAWN_FIXTURES)[0]])
    verdict = ns.classify(item, nm.parse_nest_mode("docker"))
    assert verdict.rule == ns.RULE_NEST_BINARY
    assert "bridge" not in verdict.reason.lower()


def test_all_rules_is_empty_for_an_eligible_test():
    from helpers import nest_surface as ns

    assert ns.all_rules(_FakeItem(), nm.parse_nest_mode("docker")) == ()


def test_standalone_still_pays_nothing():
    """The inner loop's standing requirement — no rule may fire in standalone."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_binary", "reclaimable_nest"])
    assert ns.all_rules(item, nm.parse_nest_mode("standalone")) == ()
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None


def test_a_mode_marker_still_wins_outright_in_standalone():
    from helpers import nest_surface as ns

    item = _FakeItem(markers=[_FakeMarker("docker_only")])
    assert ns.all_rules(item, nm.parse_nest_mode("standalone")) == (
        ns.RULE_MODE_MARKER,)


def test_the_gate_records_every_applicable_rule():
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.DECLARED_ABSENCE, "why",
                   repo_id="tests/e2e-unified/t.py::x",
                   rule=ns.RULE_NEST_BINARY,
                   all_rules=(ns.RULE_NEST_BINARY, ns.RULE_BRIDGE_SPAWN))
    (gated,) = ns.gates_payload(mode="docker", collected=[])["gated"]
    assert gated["rule"] == ns.RULE_NEST_BINARY
    assert gated["all_rules"] == [ns.RULE_NEST_BINARY, ns.RULE_BRIDGE_SPAWN]
    ns.reset_gate_hits()


def test_all_rules_defaults_to_the_deciding_rule_alone():
    """A runtime declaration knows one rule at most; the payload stays uniform."""
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.MODE_UNBUILT, "why")
    (gated,) = ns.gates_payload(mode="docker", collected=[])["gated"]
    assert gated["all_rules"] == []
    ns.reset_gate_hits()


# ── the fixture closure, so the clustering is derived and not hand-read ─────
# The audit says which CLASS blanks a page. The next question is always which
# FIXTURE the gated witnesses reach, because that is the unit a fix acts on: the
# 22 nest_binary-sole-blocked pages turned out to reach just a handful
# (`two_nodes` × 14, the bespoke per-test nests, and a set of `tests/api/` tests
# that spawn their own nest for no reason at all). Grouping 69 witnesses by
# fixture is what turned "triage 22 pages" into "answer three questions".
#
# `classify` already computes the closure to make its decision; recording it
# costs a list copy and saves the next session an AST pass over the test tree.


def test_a_gate_hit_can_carry_the_fixture_closure_it_matched_on():
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.DECLARED_ABSENCE, "why",
                   repo_id="tests/e2e-unified/t.py::x",
                   rule=ns.RULE_NEST_BINARY,
                   closure=("two_nodes", "node_binary", "tmp_path_factory"))
    (gated,) = ns.gates_payload(mode="docker", collected=[])["gated"]
    assert gated["closure"] == ["node_binary", "tmp_path_factory", "two_nodes"], (
        "sorted, so two payloads of the same run diff cleanly"
    )
    ns.reset_gate_hits()


def test_the_closure_defaults_empty_for_a_runtime_declaration():
    from helpers import nest_surface as ns

    ns.reset_gate_hits()
    ns.record_gate("docker", "t.py::x", ns.MODE_UNBUILT, "why")
    (gated,) = ns.gates_payload(mode="docker", collected=[])["gated"]
    assert gated["closure"] == []
    ns.reset_gate_hits()


# ---------------------------------------------------------------------------
# N nests per run, arm 2: `peer_url`, the per-run network, labels and the sweep
# (`testing.md` § Default app and nest mode — the ratified paragraph's rulings
# (2) and (4)).
# ---------------------------------------------------------------------------


def test_peer_url_is_a_contract_key_every_mode_answers():
    """Ruling (2): `peer_url` is "a contract key every mode answers — the
    authority ANOTHER NEST in the run dials".

    Contract, not capability: a mode that could not answer it would have to
    declare it absent, and then a two-nest test would read the absence as "this
    mode has no second nest" rather than as "this nest cannot be dialled by a
    peer". Both facts are real and they are not the same fact, which is why the
    private-range refusal is its own exclusion class (8) instead of a missing
    key. So `peer_url` stays out of `CAPABILITY_KEYS` and every handle carries
    one.
    """
    assert "peer_url" not in nm.CAPABILITY_KEYS, (
        "peer_url is part of every mode's contract, not a capability a mode may "
        "decline — see testing.md ruling (2)"
    )
    for mode_name in (nm.STANDALONE, nm.DOCKER, nm.LIVE):
        assert "peer_url" not in nm.absent_capabilities(
            nm.parse_nest_mode(mode_name)
        ), f"{mode_name} may not declare peer_url absent"


def test_a_loopback_nest_peer_url_is_its_own_url():
    """Standalone and live both answer `url`: the harness, the client and a peer
    nest all reach them at the same authority, because there is no container
    network in between."""
    conftest = pytest.importorskip("conftest")

    handle = conftest._as_nest_handle(
        {"url": "http://127.0.0.1:13001", "port": 13001},
    )
    assert handle["peer_url"] == "http://127.0.0.1:13001"


def test_an_explicit_peer_url_is_not_overwritten():
    """Docker's differs from its `url` — the harness dials the published host
    port, a peer nest dials the container IP on the run's network — so the
    provider sets it and the handle wrapper must leave it alone."""
    conftest = pytest.importorskip("conftest")

    handle = conftest._as_nest_handle({
        "url": "https://127.0.0.1:41234",
        "port": 41234,
        "peer_url": "https://172.18.0.3:3000",
        "serve_tls": True,
    })
    assert handle["peer_url"] == "https://172.18.0.3:3000"


def test_the_run_labels_name_the_harness_and_this_run():
    """Ruling (4): "every container and network the provider starts carries the
    labels `fauna-e2e=1` and `fauna-e2e-run=<run id>`" — the live suite's
    provider-label sweep applied to docker, because a container is the one child
    convention 9's process-group reap cannot reach."""
    conftest = pytest.importorskip("conftest")

    labels = conftest._docker_run_labels()
    assert labels["fauna-e2e"] == "1"
    assert labels["fauna-e2e-run"] == conftest._docker_run_id()


def test_the_run_id_carries_the_pid_and_its_start_time():
    """The sweep decides "is the owning run dead?" the way the fleet's own
    session-liveness probe does — pid plus start time, so a recycled pid cannot
    make a dead run look alive."""
    import os
    import sys

    conftest = pytest.importorskip("conftest")

    run_id = conftest._docker_run_id()
    pid, start = conftest._parse_docker_run_id(run_id)
    assert pid == os.getpid()
    if sys.platform.startswith("linux"):
        assert start is not None, "a run id with no start time cannot be pid-checked"
    else:
        # `_proc_start_ticks` reads /proc and answers None everywhere else BY
        # CONTRACT, so off-Linux the id degrades to the bare pid and liveness
        # is pid-alive alone (`_docker_run_is_alive`'s own comment). No docker
        # box is off-Linux today (mac has none by design — the machine's setup
        # guide), so the degradation is pinned rather than the mechanism
        # skipped: a start half appearing here would mean a new platform arm
        # that this pin's Linux branch should then cover too.
        assert start is None and run_id == str(os.getpid()), (
            f"off-Linux the run id must be the bare pid, got {run_id!r}"
        )
    assert conftest._docker_run_is_alive(run_id), "this run is alive"


def _needs_an_os_that_answers_for_a_dead_pid():
    """Declared skip where `conftest._pid_alive` refuses to ask by contract.

    On win it answers None for every pid — the POSIX `os.kill(pid, 0)` idiom
    would TerminateProcess the pid it asks about, and there is no docker-mode
    run on Windows to justify another probe — so no owner can ever read as dead
    there and the sweep, correctly, removes nothing. A proof that a DEAD owner
    is swept therefore has no dead owner to find on Windows (measured on the Windows host's
    first whole-directory tier_1 run, 2026-09-22); win's own answer is pinned by
    `test_a_pid_the_os_will_not_answer_for_reads_as_alive`.
    """
    import sys

    if sys.platform == "win32":
        from helpers.app_surface import skip_environment

        skip_environment(
            "conftest._pid_alive answers None for every pid on Windows by contract "
            "(os.kill would terminate it; no docker-mode run on Windows), so no owner "
            "can read as dead here"
        )


def test_a_dead_owner_is_swept_and_a_live_one_is_left_alone():
    """The sweep's whole decision, without a docker daemon: a leftover labelled
    with a run whose pid is gone is reclaimable; one whose pid is alive belongs
    to a sibling session and is untouchable."""
    _needs_an_os_that_answers_for_a_dead_pid()
    conftest = pytest.importorskip("conftest")

    mine = conftest._docker_run_id()
    dead = conftest._format_docker_run_id(2_147_483_646, 1.0)

    assert conftest._docker_run_is_alive(mine)
    assert not conftest._docker_run_is_alive(dead)


def test_a_malformed_run_label_is_not_swept():
    """An unparseable label is somebody else's convention, not a dead run of
    ours: the sweep leaves it, because deleting a container it cannot account
    for is how a sweep eats a sibling's work."""
    conftest = pytest.importorskip("conftest")

    assert conftest._docker_run_is_alive("not-a-run-id"), (
        "an unparseable owner must read as ALIVE so the sweep declines it"
    )


def test_the_legacy_name_prefix_is_still_recognised():
    """`fauna-nest-axis-<port>` predates the labels, so the sweep unions the
    prefix exactly as the Hetzner sweep unions its `e2e-` one — otherwise the
    containers a pre-label session leaked are unreclaimable forever."""
    conftest = pytest.importorskip("conftest")

    assert conftest._DOCKER_LEGACY_PREFIX == "fauna-nest-axis-"


def test_the_per_run_network_name_is_derived_from_the_run_id():
    """One user-defined network per run (ruling (1)'s topology), so two sibling
    sessions on the shared dev box cannot collide on it, and the session's own
    teardown can find it without carrying state across a crash."""
    conftest = pytest.importorskip("conftest")

    name = conftest._docker_run_network()
    assert conftest._docker_run_id() in name
    assert name.startswith("fauna-e2e-net-")


# ── Arm 2 test scaffolding ────────────────────────────────────────────────
# The provider's own logic — retry, network, labels, `peer_url`, the sweep — is
# pinned WITHOUT a docker daemon: these are decisions about what to run, and a
# decision testable headlessly must not be left to a tier_4 sweep to witness.
# "This needs a real container" is the shape of excuse to attack hardest, since
# believing it is what licenses shipping a mechanism untested. What genuinely
# needs the daemon here is only whether docker honours the flags it is handed,
# which the tier_4 suite already exercises.


class _FakeResult:
    def __init__(self, stdout="", returncode=0):
        self.stdout = stdout
        self.stderr = ""
        self.returncode = returncode


class _FakeLogFollower:
    """`docker logs --follow` stood in: the log stream's child, with nothing to
    say and nothing to kill."""

    returncode = 0

    def terminate(self):
        pass

    def kill(self):
        pass

    def wait(self, timeout=None):
        return 0


def _install_fake_docker(monkeypatch, ports, fake_start, created=None):
    """Stand the docker provider up against a fake daemon.

    Patches exactly the seams `_DockerProvider.start` reaches for, so the code
    under test is the real method: port allocation, the image probe, container
    start, health, claim, container IP, the network calls — and the one spawn
    `start` makes outside those helpers, the log stream's `docker logs --follow`
    child. Against a fake daemon that child has nothing to follow, and on a box
    with no docker CLI at all (mac, by design — the machine's setup guide) the
    exec itself raised `FileNotFoundError`: twelve of these pins were red on the
    first whole-directory tier_1 run there (2026-09-22), while everywhere else a
    real, immediately-dying child was spawned per pin. Only the log follower is
    stood in; any other spawn under the fake is a hole in this list and fails
    loudly rather than reaching a daemon.
    """
    import conftest
    import drivers.port_util as port_util
    from helpers import nest_mode as nm_mod
    from tests.platform.docker import helpers as dh

    def _fake_popen(argv, *args, **kwargs):
        argv = list(argv)
        assert argv[:2] == ["docker", "logs"], (
            f"the fake daemon does not cover this spawn: {argv}"
        )
        return _FakeLogFollower()

    monkeypatch.setattr(conftest._DockerProvider, "_run_network_ready", False)
    monkeypatch.setattr(conftest.atexit, "register", lambda *a, **k: None)
    monkeypatch.setattr(nm_mod, "run_mode", lambda: nm_mod.parse_nest_mode("docker"))
    monkeypatch.setattr(conftest.subprocess, "run",
                        lambda *a, **k: _FakeResult())  # image probe + sweep listings
    monkeypatch.setattr(conftest.subprocess, "Popen", _fake_popen)  # the log follower
    monkeypatch.setattr(port_util, "find_free_port", lambda: next(ports))
    monkeypatch.setattr(dh, "start_container_with_ports", fake_start)
    monkeypatch.setattr(dh, "wait_for_health", lambda *a, **k: None)
    monkeypatch.setattr(dh, "claim_admin_api", lambda *a, **k: {"token": "t"})
    monkeypatch.setattr(dh, "generate_claim_code", lambda: "CLAIM-CODE")
    monkeypatch.setattr(dh, "container_ip", lambda name: "172.30.0.9")
    monkeypatch.setattr(dh, "remove_container", lambda name: None)
    monkeypatch.setattr(dh, "remove_network", lambda name: None)
    monkeypatch.setattr(
        dh, "create_network",
        lambda name, labels=None: (created.append(name) if created is not None else None),
    )


def _run_sweep_over(monkeypatch, rows):
    """Run the real sweep against a fake `docker ps` listing; return what it
    removed. `rows` is a list of `(container_name, owner_run_id)`."""
    import conftest
    from tests.platform.docker import helpers as dh

    removed = []
    listing = "\n".join(f"{name}\t{owner}" for name, owner in rows)

    def _fake_run(argv, **kwargs):
        argv = list(argv)
        if argv[:2] == ["docker", "ps"] and "{{.Names}}\t{{.Label" in " ".join(argv):
            return _FakeResult(listing)
        return _FakeResult()

    monkeypatch.setattr(conftest.subprocess, "run", _fake_run)
    monkeypatch.setattr(dh, "remove_container", lambda name: removed.append(name))
    monkeypatch.setattr(dh, "remove_network", lambda name: removed.append(name))
    conftest._DockerProvider._sweep_dead_runs()
    return removed


def test_a_bind_collision_is_retried_once_on_a_fresh_port(monkeypatch, tmp_path_factory):
    """`find_free_port` binds, reads the port and closes it, so between that
    close and `docker run` a sibling session can take it — the race
    `find_free_ports`' own docstring documents. Holding the socket open would
    not help (docker cannot bind a port the harness still holds), so the
    provider retries once on a *fresh* port instead of pre-reserving.
    """
    import conftest

    ports, attempts = iter((41001, 41002)), []

    def _fake_start(name, port_map, **kwargs):
        attempts.append(name)
        if len(attempts) == 1:
            raise RuntimeError(
                "docker run failed:\nError response from daemon: driver failed "
                "programming external connectivity: Bind for 127.0.0.1:41001 "
                "failed: port is already allocated"
            )
        return "cid"

    _install_fake_docker(monkeypatch, ports, _fake_start)
    handle, _cleanup = conftest._DockerProvider().start(None, tmp_path_factory, "nest")

    assert attempts == ["fauna-nest-axis-41001", "fauna-nest-axis-41002"], (
        "a collision must be retried on a NEW port, never the same one"
    )
    assert handle["port"] == 41002


def test_every_mode_answers_nest_id_from_the_one_shared_reader(
    monkeypatch, tmp_path_factory
):
    """`nest_id` is not a capability — every mode must answer it.

    The federation CHANNEL is harness-driven: the pytest process dials the
    listener's `/api/v1/federation/ws` while signing the
    `fauna.federation.hello` **as the initiator nest**, so it needs both nests'
    ids and the initiator's key off disk. Docker published neither, and the
    consequence was not a declared absence but a bare `KeyError('nest_id')` —
    a failure no classifier could name and no reason line could explain.

    It is answerable in docker for the same reason `claim-code` and `db_path`
    are: `/data` is a host bind-mount, so the file the image writes at boot sits
    under the handle's own `tmp_dir`. So the fix is one shared reader, not a
    per-mode branch — and this pins that BOTH modes go through it, which is the
    half a value assertion alone would miss.
    """
    import conftest
    from nacl.signing import SigningKey

    from common.nest import NEST_DEPLOYMENT_KEY, nest_id_from_data_dir

    seed = bytes(range(32))
    expected = bytes(SigningKey(seed).verify_key).hex()

    mounted = []
    _install_fake_docker(
        monkeypatch, iter((41070,)),
        lambda *a, **k: mounted.append(k.get("data_volume")) or "cid",
    )
    # The image writes the seed into /data at boot; the fake daemon does not, so
    # stand in for it at the moment `wait_for_health` returns — the same instant
    # the real provider reads it.
    from tests.platform.docker import helpers as dh
    monkeypatch.setattr(
        dh, "wait_for_health",
        lambda port, name, **k: pathlib.Path(
            mounted[-1], NEST_DEPLOYMENT_KEY
        ).write_bytes(seed),
    )

    handle, _cleanup = conftest._DockerProvider().start(
        None, tmp_path_factory, "nest",
    )
    assert handle["nest_id"] == expected, (
        "docker must publish the identity the image wrote into its data dir — "
        "without it every federation-channel test raises KeyError in this mode"
    )

    # And the reader is genuinely shared: pointed at the same directory it gives
    # the same answer, which is what makes it one implementation rather than two
    # that agree today.
    assert nest_id_from_data_dir(handle["tmp_dir"]) == expected
    assert nest_id_from_data_dir(tmp_path_factory.mktemp("no-seed")) == "", (
        "a nest whose seed is not on disk yet answers empty, not a raise — "
        "the contract `start_nest` has always had"
    )


def test_the_deployment_seed_filename_has_exactly_one_home():
    """It was spelled in three places, and a fourth was about to be added.

    `common.nest` reads it for the handle, `common.helpers.sign_as_nest` opens
    it to sign the federation hello, and the docker provider now needs it too.
    A filename copied per caller is the shape that silently half-migrates when
    the nest renames it (the `nest_identity.key` → `nest_deployment.key`
    unification is the precedent, `box-recovery.md` § Single-identity
    unification)."""
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    me = Path(__file__).resolve()
    spellers = []
    for rel in ("tests/common", "tests/e2e-unified"):
        for source in sorted((root / rel).rglob("*.py")):
            if source.resolve() == me:
                continue  # this pin has to name the string to look for it
            tree = ast.parse(source.read_text(encoding="utf-8", errors="replace"))
            for node in ast.walk(tree):
                if isinstance(node, ast.Constant) and node.value == "nest_deployment.key":
                    spellers.append(
                        f"{source.relative_to(root).as_posix()}:{node.lineno}"
                    )
    assert len(spellers) == 1, (
        "the deployment-seed filename must be spelled once, in "
        f"`common.nest.NEST_DEPLOYMENT_KEY` — found {spellers}"
    )
    assert spellers[0].startswith("tests/common/nest.py"), spellers


def test_a_second_bind_collision_is_not_retried_again(monkeypatch, tmp_path_factory):
    """Two collisions in a row is not a race any more — it is a machine with no
    free ports, and that must be loud rather than an unbounded retry loop that
    reads as a hang."""
    import conftest

    def _fake_start(name, port_map, **kwargs):
        raise RuntimeError("Bind for 127.0.0.1:41001 failed: port is already allocated")

    _install_fake_docker(monkeypatch, iter((41001, 41002, 41003)), _fake_start)
    with pytest.raises(RuntimeError, match="already allocated"):
        conftest._DockerProvider().start(None, tmp_path_factory, "nest")


def test_a_non_collision_failure_is_not_retried(monkeypatch, tmp_path_factory):
    """Retrying a bad image or a missing network would spend a second container
    start to fail the same way with a worse diagnosis — docker answers 125 for
    every daemon-side refusal alike, which is why the match is on the message."""
    import conftest

    attempts = []

    def _fake_start(name, port_map, **kwargs):
        attempts.append(name)
        raise RuntimeError("Error response from daemon: No such image: nope:latest")

    _install_fake_docker(monkeypatch, iter((41001, 41002)), _fake_start)
    with pytest.raises(RuntimeError, match="No such image"):
        conftest._DockerProvider().start(None, tmp_path_factory, "nest")
    assert len(attempts) == 1


def test_every_container_joins_the_run_network_and_carries_both_labels(monkeypatch, tmp_path_factory):
    """Ruling (4)'s labels and ruling (1)'s topology, on the same call: without
    the network there is no address a peer could dial, and without the owner
    label a leaked container is reclaimable only by a human."""
    import conftest

    seen = {}

    def _fake_start(name, port_map, **kwargs):
        seen.update(kwargs)
        return "cid"

    _install_fake_docker(monkeypatch, iter((41010,)), _fake_start)
    conftest._DockerProvider().start(None, tmp_path_factory, "nest")

    assert seen["network"] == conftest._docker_run_network()
    assert seen["labels"] == conftest._docker_run_labels()


def test_a_docker_peer_url_is_the_container_ip_not_the_published_port(monkeypatch, tmp_path_factory):
    """`127.0.0.1` inside a container is that container, so the published host
    port is unreachable as a peer authority. This is the only mode where `url`
    and `peer_url` differ — the reason `peer_url` had to become a contract key
    rather than staying an alias."""
    import conftest

    _install_fake_docker(monkeypatch, iter((41020,)), lambda *a, **k: "cid")
    handle, _cleanup = conftest._DockerProvider().start(None, tmp_path_factory, "nest")

    assert handle["url"] == "https://127.0.0.1:41020"
    assert handle["peer_url"] == "https://172.30.0.9:3000", (
        "a peer dials the container's own address on the run network, at the "
        "IN-CONTAINER port (3000), not the host's published one"
    )


def test_the_run_network_is_created_once_per_session_not_once_per_nest(monkeypatch, tmp_path_factory):
    """One per RUN (ruling (1)). A second nest is the zero-option call and must
    land on the network the first one already made — otherwise the two cannot
    address each other at all, which is the whole point of having one."""
    import conftest

    created = []
    _install_fake_docker(monkeypatch, iter((41030, 41031)),
                         lambda *a, **k: "cid", created=created)
    provider = conftest._DockerProvider()
    provider.start(None, tmp_path_factory, "nest")
    provider.start(None, tmp_path_factory, "nest2")

    assert created == [conftest._docker_run_network()]


def test_the_sweep_declines_this_runs_own_containers(monkeypatch):
    """The sweep runs BEFORE this run's first container, but a second one starts
    while the first is serving — so "not mine" has to be checked by run id, not
    inferred from the sweep's timing."""
    _needs_an_os_that_answers_for_a_dead_pid()
    import conftest

    mine = conftest._docker_run_id()
    dead = conftest._format_docker_run_id(2_147_483_646, "1")
    removed = _run_sweep_over(monkeypatch, [
        ("fauna-nest-axis-1", mine),
        ("fauna-nest-axis-2", dead),
    ])
    assert removed == ["fauna-nest-axis-2"]


def test_the_sweep_declines_a_container_whose_owner_is_alive(monkeypatch):
    """A sibling session's four-hour docker run is not garbage. The sweep has no
    age heuristic at all, deliberately: an age rule is how a sweep eats live
    work."""
    import conftest
    import os

    alive = conftest._format_docker_run_id(
        os.getpid(), conftest._proc_start_ticks(os.getpid()),
    )
    removed = _run_sweep_over(monkeypatch, [("fauna-nest-axis-3", alive)])
    assert removed == []


def test_the_sweep_declines_an_unparseable_owner(monkeypatch):
    """Fails CLOSED. An unknown owner is somebody else's convention, and the cost
    asymmetry is the inverse of the liveness probe's: a false "dead" here
    destroys a running test, where a false "active" there only starves a queue.
    """
    removed = _run_sweep_over(monkeypatch, [("fauna-nest-axis-4", "who-knows")])
    assert removed == []


def test_a_pid_the_os_will_not_answer_for_reads_as_alive(monkeypatch):
    """The third liveness answer, and the one a red-verify caught as unpinned.

    `_pid_alive` answers None — not False — where the OS declines to say: an
    unexpected `OSError` on POSIX, and every pid on Windows (where the harmless
    POSIX `os.kill(pid, 0)` idiom would actually terminate the process it asked
    about, so the probe refuses to ask at all). "Dead" and "I could not ask" are
    different facts, and only the first licenses deleting somebody's container,
    so the unanswered case must join the unparseable one on the decline side
    rather than collapsing into `bool(None) == False`.
    """
    import conftest

    monkeypatch.setattr(conftest, "_pid_alive", lambda pid: None)

    assert conftest._docker_run_is_alive(
        conftest._format_docker_run_id(2_147_483_646, "1")
    ), "an unanswerable pid must read as ALIVE so the sweep declines it"


def test_a_start_time_that_no_longer_matches_is_a_recycled_pid(monkeypatch):
    """The half that pid-alive alone cannot see: the pid IS alive, but it is not
    the process that started this container — an unrelated program the OS handed
    the number to after the owning run died. Without this, a leaked container's
    owner reads as alive forever and nothing ever reclaims it."""
    import conftest
    import os

    monkeypatch.setattr(conftest, "_pid_alive", lambda pid: True)
    monkeypatch.setattr(conftest, "_proc_start_ticks", lambda pid: "999999")

    assert not conftest._docker_run_is_alive(
        conftest._format_docker_run_id(os.getpid(), "12345")
    ), "a live pid with a DIFFERENT start time is a recycled pid, not the owner"


# ---------------------------------------------------------------------------
# N nests per run, arm 3a: the provider half of the declared-options seam
# (`testing.md` § Default app and nest mode, ruling (3)).
# ---------------------------------------------------------------------------


def test_every_provider_declares_which_start_options_it_honours():
    """Ruling (3)'s seam: "a provider declares `supported_options`".

    Declared rather than discovered, because the point of the seam is to answer
    at COLLECTION time — before a container exists — whether a fixture's options
    can be met. A provider that only found out by trying could never be asked.
    """
    import conftest

    for provider in (conftest._StandaloneProvider(), conftest._DockerProvider(),
                     conftest._LiveProvider()):
        assert isinstance(provider.supported_options, frozenset), (
            f"{provider.name} must declare supported_options as a frozenset"
        )


def test_standalone_supports_exactly_what_make_nest_takes():
    """Pinned to `_make_nest`'s own signature, not restated beside it.

    Standalone's provider IS `_make_nest`, so its supported set is a fact about
    that function. Restating it by hand is how the two drift, and the drift is
    silent in the direction that matters: an option `_make_nest` grows but the
    declaration does not would be classified as unsupportable in the ONE mode
    that supports everything.
    """
    import inspect

    import conftest

    params = set(inspect.signature(conftest._make_nest).parameters)
    params -= {"nest_binary", "tmp_path_factory", "label"}

    assert conftest._StandaloneProvider().supported_options == frozenset(params), (
        "standalone supports exactly the per-nest options _make_nest takes"
    )


def test_live_declares_no_options_because_it_starts_no_nest():
    """Live honours nothing, and that is not an omission awaiting an arm: it
    starts no nest at all, so there is no start for an option to be a parameter
    of. Declaring the empty set is what makes the classifier's
    `needs - supported` non-empty for every option-passing fixture there."""
    import conftest

    assert conftest._LiveProvider().supported_options == frozenset()


def test_a_refusal_names_what_the_provider_does_support():
    """The runtime refusal stays as the BACKSTOP (ruling (3) is explicit that it
    is never the classifier), but it now reads off the same declaration the
    classifier will — so a message and a collection-time verdict cannot disagree
    about what a mode can do."""
    import conftest

    with pytest.raises(nm.NestModeError) as exc:
        conftest._DockerProvider().start(
            None, None, "nest", handle_domain_seed="mail.test",
        )
    message = str(exc.value)
    assert "handle_domain_seed" in message
    assert (
        "supports only ['claim_domain', 'cors_origins', 'dial_host', "
        "'extra_env', 'serve_tls', 'static_dir', 'unclaimed']"
        in message
    ), (
        "the refusal must state the provider's declared set, not a prose "
        "restatement of it that can drift"
    )


def test_a_refusal_still_fires_before_anything_is_started(monkeypatch):
    """Order matters: the refusal is checked before the image probe, the network
    and the container, so an unsupportable option costs nothing and cannot leave
    a half-built nest behind."""
    import conftest

    started = []
    _install_fake_docker(monkeypatch, iter((41040,)),
                         lambda *a, **k: started.append(1) or "cid")
    with pytest.raises(nm.NestModeError):
        conftest._DockerProvider().start(
            None, None, "nest", handle_domain_seed="a.test")
    assert started == []


def test_a_falsy_option_is_not_a_request(monkeypatch, tmp_path_factory):
    """`_make_nest`'s defaults are `False`/`None`, and a fixture that passes one
    explicitly is asking for the default, not for a feature. Refusing those
    would exclude the zero-option fixtures — the very ones arm 1 routes first."""
    import conftest

    _install_fake_docker(monkeypatch, iter((41050,)), lambda *a, **k: "cid")
    handle, _cleanup = conftest._DockerProvider().start(
        None, tmp_path_factory, "nest",
        unclaimed=False, claim_domain=None, serve_tls=False,
    )
    assert handle["port"] == 41050


# ---------------------------------------------------------------------------
# N nests per run, arm 5: the ARTIFACT-WIRING start options, honoured in docker
# (`testing.md` § Default app and nest mode, ruling (3)).
# ---------------------------------------------------------------------------


def test_docker_honours_the_artifact_wiring_options():
    """Ruling (3) sorts every start option by WHERE THE KNOB LIVES, and these
    sort as *artifact wiring*: each is one container start parameter, and
    each is a deployment input the image already catalogues rather than a
    preference a human expresses. So the provider honours them — a mounted
    `nest.toml` or a new entrypoint env would have been configuration-file
    theatre, and a declared absence would have been a lie about what the
    artifact can do.

    `cors_origins` is the clearest case of that catalogue clause: the image reads
    `FAUNA_CORS_ORIGINS` and seeds `[nest].cors_origins` on first run all by
    itself (`installers/docker.md` § Environment Variables lists it as a Seed),
    so honouring it is passing one variable the entrypoint already expects — not
    a new env, and not the choice surface, which stays the app-set
    `fauna.admin.set_cors_origins` state whose DB row wins over any seed
    (`provisioning/registry.md` § Health-poll CORS).

    `claim_domain` is in the set for a DIFFERENT reason and the distinction is
    worth keeping straight: it is not artifact wiring, it is the product choice
    itself, honoured because the harness's own claim can carry it
    (`claim_admin_api(mail_domain=...)`). Ruling (3) sorts options by where the
    knob lives, and "on the wire" is a place a container provider can reach.

    `handle_domain_seed` is deliberately NOT here, and permanently: it is the
    `--handle-domain` boot flag, wanted only where no wire act exists to replace
    it (an IP-literal authority the claim gate refuses; an `unclaimed=True` nest
    with no harness claim at all). Honouring it would take an entrypoint env or a
    mounted `nest.toml` — the configuration-file theatre ruling (3) bans by name.

    `static_dir` is met the way `serve_tls` is, before it is asked for: the
    image serves its own bundled SPA build at `/app/` and the share viewer page,
    so a fixture asking for a nest that serves the build gets the image's.
    """
    import conftest

    assert conftest._DockerProvider().supported_options == frozenset(
        {"unclaimed", "claim_domain", "serve_tls", "dial_host", "extra_env",
         "cors_origins", "static_dir"}
    )


def test_an_unclaimed_docker_nest_is_left_for_the_client_to_claim(
    monkeypatch, tmp_path_factory
):
    """Honouring `unclaimed` is *not spending* the claim code — not withholding
    it. The code is injected as `FAUNA_CLAIM_CODE` either way, which is what
    leaves the claim drivable from the client UI, and the provider simply does
    not make the call itself.
    """
    import conftest
    from tests.platform.docker import helpers as dh

    envs, claimed = [], []
    _install_fake_docker(
        monkeypatch, iter((41060,)),
        lambda *a, **k: envs.append(k.get("env")) or "cid",
    )
    monkeypatch.setattr(dh, "claim_admin_api",
                        lambda *a, **k: claimed.append(a) or {"token": "t"})

    handle, _cleanup = conftest._DockerProvider().start(
        None, tmp_path_factory, "nest", unclaimed=True,
    )
    assert claimed == [], "an unclaimed nest is one the PROVIDER never claims"
    assert handle["admin"] is None
    assert envs[0]["FAUNA_CLAIM_CODE"] == "CLAIM-CODE", (
        "the code is still injected — `unclaimed` withholds the claim, not the "
        "means to make it"
    )
    assert handle["claim_code"] == "CLAIM-CODE"


def test_an_unclaimed_docker_nest_answers_the_claim_code_file_contract(
    monkeypatch, tmp_path_factory
):
    """Standalone's `unclaimed` contract is "the one-time code is on disk at
    `<tmp_dir>/claim-code`", and docker answers it *by construction* rather than
    by a second mechanism: the entrypoint writes `$FAUNA_CLAIM_CODE` to
    `/data/claim-code` before the nest boots, and `/data` IS the handle's
    `tmp_dir` (the host bind-mount). So the two modes meet the same contract
    with no per-mode branch in any test.

    What is pinned here is the half that is a harness fact — that `tmp_dir` and
    the mounted `/data` are the same directory, so the path a test composes
    resolves. The other half is the image's, and is exercised by the docker-mode
    run of the onboarding journeys.
    """
    import conftest

    mounted = []
    _install_fake_docker(
        monkeypatch, iter((41061,)),
        lambda *a, **k: mounted.append(k.get("data_volume")) or "cid",
    )
    handle, _cleanup = conftest._DockerProvider().start(
        None, tmp_path_factory, "nest", unclaimed=True,
    )
    assert mounted[0] == handle["tmp_dir"], (
        "`<tmp_dir>/claim-code` only resolves if tmp_dir is the host side of "
        "the /data the entrypoint writes it into"
    )


def test_serve_tls_is_honoured_by_the_image_having_no_other_posture(
    monkeypatch, tmp_path_factory
):
    """`serve_tls=True` costs this provider nothing, and that is the honest
    reason it is supported rather than a shortcut: the image serves its
    self-signed floor cert on the listener with no plain-HTTP posture to
    opt out of, so the option is already met before it is asked for.

    Pinned as *no change to the container start* plus the https url, so that a
    future flavour which did have a plain-HTTP posture could not quietly inherit
    a claim of support it no longer satisfies.
    """
    import conftest

    calls = []
    _install_fake_docker(
        monkeypatch, iter((41062, 41063)),
        lambda *a, **k: calls.append(k) or "cid",
    )
    plain, _c1 = conftest._DockerProvider().start(None, tmp_path_factory, "a")
    tls, _c2 = conftest._DockerProvider().start(
        None, tmp_path_factory, "b", serve_tls=True,
    )

    assert tls["serve_tls"] is True and plain["serve_tls"] is True
    assert tls["url"].startswith("https://")
    assert calls[0]["env"] == calls[1]["env"], (
        "asking for TLS must not change the container's wiring — the image has "
        "no other posture to switch out of"
    )


def test_a_lan_dialled_docker_nest_widens_its_publication(
    monkeypatch, tmp_path_factory
):
    """`dial_host` is the authority a CLIENT dials, and it is the only way to
    drive that client's SPKI-**pin** trust branch instead of its loopback
    short-circuit. Standalone answers it by widening the nest's own bind from
    `127.0.0.1` to `0.0.0.0`; docker's twin is the PUBLICATION, because the
    container already binds every interface inside its own namespace and what
    has to widen is the host-side `-p`.

    Widening rather than *moving* it mirrors standalone exactly, and that is
    load-bearing: the harness's own health probe and claim call keep dialling
    loopback because they are not the thing under test, and a publication
    pinned to the LAN address alone would have dragged both onto the LAN path
    for no gain.
    """
    import conftest

    published = []
    _install_fake_docker(
        monkeypatch, iter((41064,)),
        lambda *a, **k: published.append(k.get("publish_host")) or "cid",
    )
    handle, _cleanup = conftest._DockerProvider().start(
        None, tmp_path_factory, "nest", dial_host="192.0.2.7",
    )
    assert published[0] == "0.0.0.0"
    assert handle["url"] == "https://192.0.2.7:41064", (
        "the url carries the authority the CLIENT dials, which is the whole "
        "point of the option"
    )


def test_a_loopback_dial_host_keeps_the_tight_publication(
    monkeypatch, tmp_path_factory
):
    """The default and the explicitly-loopback case are the same case, and
    neither may widen: publishing an ordinary harness nest on every interface
    would expose a sibling session's nest to the LAN for nothing."""
    import conftest

    published = []
    _install_fake_docker(
        monkeypatch, iter((41065, 41066)),
        lambda *a, **k: published.append(k.get("publish_host")) or "cid",
    )
    default, _c1 = conftest._DockerProvider().start(None, tmp_path_factory, "a")
    explicit, _c2 = conftest._DockerProvider().start(
        None, tmp_path_factory, "b", dial_host="127.0.0.1",
    )
    assert published == ["127.0.0.1", "127.0.0.1"]
    assert default["url"] == "https://127.0.0.1:41065"
    assert explicit["url"] == "https://127.0.0.1:41066"


def test_docker_and_standalone_agree_on_what_loopback_means():
    """Pinned to the same predicate rather than to a second copy of the rule.

    `_is_loopback_host` is the Python twin of the client's own
    `is_loopback_authority` — `localhost` and the IP literals only, DNS
    deliberately not resolved — and it is what standalone's bind widening keys
    on. A docker-side re-implementation could drift by exactly the cases that
    decide which trust branch a test drives, which is the one thing this option
    exists to control.
    """
    import conftest
    from common.nest import _is_loopback_host

    for host in ("localhost", "127.0.0.1", "127.0.0.53", "::1",
                 "192.0.2.7", "example.test", "0.0.0.0"):
        assert conftest._publish_host_for(host) == (
            "127.0.0.1" if _is_loopback_host(host) else "0.0.0.0"
        ), f"docker's publication widening disagreed about {host!r}"


def test_growing_the_supported_set_un_excludes_fixtures_with_no_table_edit():
    """Ruling (3)'s promise, measured: "a provider that grows an option
    un-excludes every fixture needing only what it now supports, with no table
    edit". The three TLS/dial fixtures ask for exactly the options arm 5 added,
    so they stop being class (4) — and `FIXTURE_START_OPTIONS` was not touched
    to make that happen.

    `unclaimed_mail_nest_ui` is the control: it also wants `unclaimed`, but it
    wants `handle_domain_seed` too, so it stays excluded — naming the option that
    is still missing rather than going quiet. It is the durable control now that
    arm 4 landed, because that option is one no provider can grow into.
    """
    import helpers.nest_surface as ns

    supported = ns._declared_options(nm.parse_nest_mode("docker"))
    for fixture in ("self_signed_nest", "rotatable_tls_nest", "spki_pinned_nest"):
        assert not (ns.FIXTURE_START_OPTIONS[fixture] - supported), (
            f"{fixture} asks only for artifact wiring docker now honours"
        )
    assert ns.FIXTURE_START_OPTIONS["unclaimed_mail_nest_ui"] - supported == (
        frozenset({"handle_domain_seed"})
    )


def test_catalogued_ipc_reaches_the_container(monkeypatch, tmp_path_factory):
    """`extra_env` is honoured as container env, which works because every s6
    run script is `with-contenv` and the nest's own `exec` line ADDS to the
    inherited environment rather than replacing it — so a value handed to
    `docker run` reaches `fauna-nest` itself.

    It is added to the provider's own wiring, never substituted for it: the
    fixture is asking for one more IPC value, not for a different nest.
    """
    import conftest

    envs = []
    _install_fake_docker(
        monkeypatch, iter((41070,)),
        lambda *a, **k: envs.append(k.get("env")) or "cid",
    )
    seed = "ab" * 32
    conftest._DockerProvider().start(
        None, tmp_path_factory, "nest",
        extra_env={"FAUNA_DEPLOYMENT_SEED": seed},
    )
    assert envs[0]["FAUNA_DEPLOYMENT_SEED"] == seed
    assert envs[0]["FAUNA_PORT"] == "3000", (
        "extra_env adds to the provider's wiring; it does not replace it"
    )


def test_an_uncatalogued_env_name_is_refused_loudly(monkeypatch, tmp_path_factory):
    """The restriction is the invariant guard, not fussiness: an unrestricted
    `extra_env` would be a general-purpose entrypoint-env hatch, i.e. exactly
    the configuration-file theatre ruling (3) bans — a knob nobody catalogued,
    reachable only in one mode, standing in for a constant or an app choice.

    This refusal is necessarily the provider's own and necessarily at setup:
    the classifier grades option NAMES (`FIXTURE_START_OPTIONS` records that a
    fixture passes `extra_env`, never which keys), so a value-level rule cannot
    be answered at collection. Loud is therefore the whole of the contract.
    """
    import conftest

    _install_fake_docker(monkeypatch, iter((41071,)), lambda *a, **k: "cid")
    with pytest.raises(nm.NestModeError) as exc:
        conftest._DockerProvider().start(
            None, tmp_path_factory, "nest",
            extra_env={"FAUNA_DEPLOYMENT_SEED": "ab", "FAUNA_UNCATALOGUED_KNOB": "x"},
        )
    message = str(exc.value)
    assert "FAUNA_UNCATALOGUED_KNOB" in message, "the refusal must name the offender"
    assert "FAUNA_DEPLOYMENT_SEED" in message, (
        "and the set it does honour, so the reader can tell which half was wrong"
    )


def test_extra_env_can_never_overwrite_the_providers_own_wiring():
    """A value the provider sets ITSELF is not a fixture's to change: the port,
    the NAT-mode seed and the claim code are how this provider addresses,
    reaches and claims the nest it is starting. Letting `extra_env` reach them
    would let a fixture silently break the handle it is about to be handed.

    Pinned as a disjointness property of the allowlist rather than as a check
    inside `start`, so it holds for every future name added to either side.
    """
    import conftest

    provider_owned = {"FAUNA_PORT", "FAUNA_MODE", "FAUNA_CLAIM_CODE"}
    assert not (conftest._DOCKER_EXTRA_ENV & provider_owned), (
        "the allowlist must not name env the provider sets itself: "
        f"{sorted(conftest._DOCKER_EXTRA_ENV & provider_owned)}"
    )


def test_every_deployment_input_on_the_allowlist_is_catalogued():
    """"Restricted to catalogued IPC" made mechanical for the half where
    "catalogued" is a fact: `installers/docker.md` § Environment Variables is
    the bucket catalogue, so a `FAUNA_*` name the provider passes through must
    appear there — otherwise the harness would be inventing a deployment input
    and calling it wiring.

    Deliberately scoped to the `FAUNA_*` names. `RUST_LOG` is the nest's own
    diagnostic level, not a deployment input, so the catalogue is right not to
    list it and this pin is right not to demand it — the paragraph's "the log
    level for the crash beacon" is what admits it, and it is on the allowlist
    for that stated reason rather than by an appeal to a table it does not
    belong in.
    """
    from pathlib import Path

    import conftest

    doc = Path(__file__).resolve().parents[3] / (
        "docs/goal/architecture/installers/docker.md"
    )
    body = doc.read_text()
    section = body.split("## Environment Variables", 1)[1]
    for name in sorted(conftest._DOCKER_EXTRA_ENV):
        if not name.startswith("FAUNA_"):
            continue
        assert f"`{name}`" in section, (
            f"{name} is passed through to a container but is not catalogued in "
            "installers/docker.md § Environment Variables"
        )


# ---------------------------------------------------------------------------
# N nests per run, arm 3c: class (8) — a test that hands ONE nest the address of
# ANOTHER (`testing.md` § Default app and nest mode, ruling (2)).
# ---------------------------------------------------------------------------


def test_class_8_excludes_a_test_that_reads_a_peer_authority_in_docker():
    """The class, on a real witness.

    `test_cross_nest_shared_folder_renders_as_a_foreign_row` relays a welcome
    from nest A to nest B (`conv_api.welcome_deliver(..., nest_url=...)`). In
    docker each nest is a container on the run's user-defined network, so the
    address it hands over is RFC1918 and `validate_peer_url` refuses it — a
    product refusal, and a ❌ that would accuse working federation code.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["nest_instance"],
        name="test_cross_nest_shared_folder_renders_as_a_foreign_row",
    )
    verdict = ns.classify(item, nest_mode_mod.parse_nest_mode("docker"))
    assert verdict is not None, "docker must exclude a nest-dials-nest test"
    assert verdict.klass == ns.DECLARED_ABSENCE
    assert verdict.rule == ns.RULE_PRIVATE_PEER, verdict.rule
    assert "validate_peer_url" in verdict.reason, (
        "the reason must name the guard doing the refusing, so a reader can "
        "check the classification against the product rather than trust it"
    )


def test_class_8_leaves_a_same_nest_round_trip_alone():
    """The half every rejected instrument got wrong, and the reason for the key.

    `test_dm_roundtrip` names `fauna.conversations.channel.fetch` and
    `keypackage.fetch` — both kinds whose handler DOES relay — but it passes no
    peer authority, so on one nest they are local operations that a container
    hosts perfectly well. A kind-set instrument excluded it (and ~20 more like
    it); reading the `peer_url` key does not, because the test never takes one.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    for name in (
        "test_dm_roundtrip",
        "test_key_package_lifecycle",
        "test_channel_message_roundtrip",
    ):
        item = _FakeItem(fixturenames=["nest_instance"], name=name)
        rules = set(ns.all_rules(item, nest_mode_mod.parse_nest_mode("docker")))
        assert ns.RULE_PRIVATE_PEER not in rules, (
            f"{name} drives a relaying kind WITHOUT a peer authority — "
            "class (8) must not claim it"
        )


def test_class_8_leaves_a_host_driven_two_nest_test_alone():
    """The other half: two nests is not the class — handing one to the other is.

    `test_users_on_different_nests` holds two nests and drives each ITSELF
    (`ApiActor(nest_url=second_nest["url"])`). Two containers can both be
    dialled by the harness, so it collects in docker; only a nest asked to dial
    the other cannot. `ApiActor`'s parameter being *named* `nest_url` is exactly
    why the field name could not be the instrument.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["nest_instance", "second_nest"],
        name="test_users_on_different_nests",
    )
    rules = set(ns.all_rules(item, nest_mode_mod.parse_nest_mode("docker")))
    assert ns.RULE_PRIVATE_PEER not in rules, (
        "a host-driven two-nest test is not class (8)"
    )


def test_class_8_is_docker_only():
    """Standalone shares a loopback and live peers ARE globally routable, so the
    refusal exists in neither. A class that fired in live would exclude the one
    mode whose peers the guard was written for."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["nest_instance"],
        name="test_cross_nest_shared_folder_renders_as_a_foreign_row",
    )
    for mode_name in ("standalone", "live"):
        rules = set(
            ns.all_rules(item, nest_mode_mod.parse_nest_mode(mode_name))
        )
        assert ns.RULE_PRIVATE_PEER not in rules, (
            f"class (8) must not fire in {mode_name}"
        )


def test_class_8_never_claims_a_test_that_starts_no_nest():
    """The pins in THIS file read `peer_url` to assert on it, headless, with no
    nest anywhere. Reporting "hands one nest the address of another" for a test
    that starts none is precisely the false account the mode audit exists to
    catch, so the class is gated on a nest being in the closure."""
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    item = _FakeItem(
        fixturenames=["monkeypatch", "tmp_path_factory"],
        name="test_a_docker_peer_url_is_the_container_ip_not_the_published_port",
    )
    rules = set(ns.all_rules(item, nest_mode_mod.parse_nest_mode("docker")))
    assert ns.RULE_PRIVATE_PEER not in rules, (
        "a nest-less test cannot be class (8) whatever keys it reads"
    )


def test_class_8_still_sees_a_test_whose_nest_fixture_has_been_ROUTED():
    """The gate must survive arm 1, and until 2026-09-02 it did not.

    Class (8) is gated on the closure holding a nest, and that gate was spelled
    as `LOCAL_NEST_FIXTURES | NEST_BINARY_FIXTURES | {"nest_instance"}` — two
    sets whose whole meaning is *this fixture has NOT been routed yet*. Routing
    a fixture is precisely the act of removing it from both. So the moment arm 1
    routed a nest-DIALS-nest fixture, class (8) went blind to it and its tests
    were admitted to docker, where the product refuses the dial — and for the
    enrolling half (`fauna.feed.contributors.grant`) it refuses *silently*, so
    the ❌ would have been a 120-second timeout with no diagnosis. The gate and
    the classification would have failed in opposite directions at the same
    instant, which is why this is pinned rather than left to review.

    The fix is the same structural member `_LOCAL_NEST_FIXTURE_USERS` adopted
    for the prebuild trigger after the identical failure on 2026-08-30:
    `nest_mode` is the one name every nest-starting fixture MUST request,
    because it has to hand it to `_start_dedicated_nest` to pick a provider. A
    fixture routed in future is covered by being routed.
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    docker = nest_mode_mod.parse_nest_mode("docker")
    for fixture, name in (
        ("two_report_nests",
         "test_exchange_originator_moves_k3_aggregate_to_peer_unprompted"),
        ("two_trend_nests",
         "test_trending_cycle_fetches_peer_only_post_and_surfaces_it"),
    ):
        # Exactly the closure a routed fixture produces: no `nest_binary`, no
        # `node_binary`, no `nest_instance` — that is what routing removed.
        item = _FakeItem(
            fixturenames=[fixture, "nest_mode", "tmp_path_factory", "request"],
            name=name,
        )
        rules = set(ns.all_rules(item, docker))
        assert ns.RULE_PRIVATE_PEER in rules, (
            f"{name} reads `peer_url` off a nest its ROUTED fixture started, so "
            f"it hands one container the RFC1918 address of another and the "
            f"product refuses it — but class (8) did not claim it. The gate at "
            f"`nest_surface._NEST_BEARING_FIXTURES` must recognise a routed "
            f"fixture's closure, not only an un-routed one's ({sorted(rules)})"
        )


def test_the_class_8_nest_gate_is_the_structural_member_not_a_list():
    """Stated at the source, because the outcome pin above can be satisfied by
    hand-listing the two fixtures — which is the failure one slice later.

    `nest_mode` subsumes every nest-bearing name for the same reason it does in
    `_LOCAL_NEST_FIXTURE_USERS`: a fixture that starts a harness-owned nest
    cannot avoid requesting it. The un-routed sets stay in the union because
    they are still the truth for everything arm 1 has not reached.
    """
    from helpers import nest_surface as ns

    assert "nest_mode" in ns._NEST_BEARING_FIXTURES, (
        "the gate must key on the structural member; a hand-maintained list of "
        "routed fixture names is the shape that already failed twice"
    )
    assert "nest_instance" in ns._NEST_BEARING_FIXTURES, (
        "the session nest is still nest-bearing"
    )
    assert ns.NEST_BINARY_FIXTURES <= ns._NEST_BEARING_FIXTURES, (
        "an un-routed fixture is still recognised by its binary request"
    )


def test_the_key_read_selector_counts_loads_and_not_writes():
    """Why `_key_reads` is a subscript LOAD and not a string constant.

    The reach graph is keyed on the bare function name, so a `peer_url` mention
    inside a method as common as `start` — which is where the docker provider
    WRITES the key — merges into every caller of every same-named method.
    Measured before the narrowing: a constant scan selected 3428 of ~4200 test
    functions, the whole suite. Taking the value out of a handle is the act
    being classified; furnishing the handle is what every test does.
    """
    import ast

    from helpers import kind_reach

    keys = frozenset({"peer_url"})
    write = ast.parse('def start(self):\n    return {"peer_url": ip}\n')
    read = ast.parse('def relay(h):\n    return call(nest_url=h["peer_url"])\n')
    assert kind_reach._key_reads(write, keys) == set(), (
        "a provider furnishing the key must not read as handing it over"
    )
    assert kind_reach._key_reads(read, keys) == {"peer_url"}


def test_the_peer_url_guard_still_refuses_a_private_range_target():
    """Class (8) pinned to nest by POSTURE, since it has no kind set to pin to.

    The class says the product refuses an RFC1918 peer. If `validate_peer_url`
    ever widened to accept one — a home-LAN nest is a first-class deployment,
    and `fauna_core::counterparty_url` already takes exactly that view for the
    custody dial — class (8) would be excluding tests the product had started
    allowing, silently and forever. This goes red instead.
    """
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    source = (root / "bins/fauna-nest/src/federation_channel.rs").read_text()
    body = source.split("pub(crate) async fn validate_peer_url", 1)[1]
    body = body.split("\n}\n", 1)[0]

    assert "is_loopback_literal" in body, (
        "the carve-out must still be by loopback LITERAL — a prefix or hostname "
        "carve-out would admit a container address and dissolve class (8)"
    )
    assert "resolve_global_addrs" in body, (
        "a non-loopback peer must still be required to resolve to a globally "
        "routable address; without this check class (8) does not exist and "
        "helpers/nest_surface.RULE_PRIVATE_PEER is excluding tests for nothing"
    )


#: Functions that feed a peer-authority request field from something OTHER than
#: a handle's ``peer_url`` — every one of them REVIEWED as a client the harness
#: (or the app, or a bridge) dials itself, not a nest asked to dial a peer.
#:
#: This list exists because the two acts are syntactically identical:
#: ``ApiActor(nest_url=second_nest["url"])`` builds a client, and
#: ``conv_api.channel_fetch(..., nest_url=nest_b["url"])`` asks nest A to dial
#: nest B, and nothing in the source separates them. So class (8) cannot be
#: derived from the non-``peer_url`` side; it is derived from the ``peer_url``
#: side, and THIS is the guard that keeps that derivation complete — a relay
#: written any other way is invisible to the classifier and comes back as a
#: false ❌ in a docker run, so it has to land here first.
_PEER_FIELDS_REVIEWED_AS_CLIENT_DIALS = {
    "alice_linux",             # scenarios: the driver's own nest
    "api_actor_b",             # ApiActor client on nest B (host-driven)
    "api_actor_c",             # ApiActor client on nest C (host-driven)
    "api_actor_peer",          # ApiActor client on the session's own nest_instance
    "bob_linux",               # scenarios: the driver's own nest
    "launch_on_home",          # skew_client: the app dials the nest it is launched against
    "make",                    # conftest local factory: the nest it just made
    "unclaimed_caldav_nest",   # the fixture's own nest
    # ── Reached only once the selector followed local ALIASES and hand-composed
    # authorities (2026-09-02). Every one re-read at its call site; all are a
    # client dialling the nest it belongs to, none is a relay.
    "mail_bridge_mta",         # the MTA bridge dials ITS OWN nest
    "provision_caldav_on_running_nest",   # the nest this helper just started
    "test_activitypub_federation_live_three_party",  # `_ui_claim`: the app's own nest
    "test_private_relay_behind_hetzner_public_node",  # ditto, private axis
    "test_provision_drives_nest_url_without_device_toml",  # sync-agent → its nest
    "test_the_hint_opens_a_connected_app_and_the_domains_first_answer_drops_it",
                               # the app's own registry row, not a peer
    "test_two_pipe_isolation",  # two agents, one nest, both their own
    "test_wasm_claim_pin_differing_root_overwrites",   # web claim of its own nest
    "test_wasm_claim_pin_same_root_preserves_rotation_seq",  # ditto
}


def _peer_fields_not_fed_from_peer_url() -> set[str]:
    """Every function handing a peer-authority field a NON-``peer_url`` authority.

    Three widenings past the original ``handle["url"]``-only form, and each one
    is a hole a real site fell through on 2026-09-02:

    * **Hand-composed authorities.** ``contributor_seeds=[f"http://127.0.0.1:
      {port_b}"]`` is *more* invisible than a ``url`` read, not less — there is
      no handle in it at all. Both federation relays this slice migrated were
      spelled exactly that way, so the guard whose whole job is completeness
      could not see either of them.
    * **Local aliases.** Both were also spelled over two statements
      (``b_url = …`` then ``contributor_seeds=[b_url]``), which the direct-value
      match missed for free. One assignment of indirection is not obfuscation,
      it is ordinary style.
    * **List elements.** ``contributor_seeds`` takes a *list* of peer
      authorities, so the value at the keyword is an ``ast.List`` and never the
      authority itself.

    Deliberately one assignment deep and within one function: a guard that
    chased the whole dataflow would be the seed-less taint analysis ruling (2)
    already rejected by measurement. This catches the shapes that occur.
    """
    import ast
    from pathlib import Path

    peer_fields = {
        "nest_url",
        "recipient_nest_url",
        "owner_nest_url",
        "destination_nest_url",
        # A LIST of peer authorities, and the enrolling half of class (8):
        # `feed_routes.rs::create_feed_core` scheme-checks each seed, upserts it
        # as a contributor and fires `notify_exchange_transition`, so the
        # originator worker dials it minutes later. Nothing validates the
        # address, which is why this half fails silently in docker.
        "contributor_seeds",
    }

    def is_url_read(node):
        return (
            isinstance(node, ast.Subscript)
            and isinstance(node.slice, ast.Constant)
            and node.slice.value == "url"
        )

    def is_hand_composed_authority(node):
        """`f"http://127.0.0.1:{port}"` — a literal authority, interpolated."""
        if not isinstance(node, ast.JoinedStr):
            return False
        parts = node.values
        for i, part in enumerate(parts[:-1]):
            if not isinstance(part, ast.Constant):
                continue
            if not isinstance(parts[i + 1], ast.FormattedValue):
                continue
            if str(part.value).rstrip().endswith(("://127.0.0.1:", "://localhost:")):
                return True
        return False

    def is_suspect(node, aliases):
        if is_url_read(node) or is_hand_composed_authority(node):
            return True
        if isinstance(node, ast.Name) and node.id in aliases:
            return True
        if isinstance(node, (ast.List, ast.Tuple)):
            return any(is_suspect(e, aliases) for e in node.elts)
        return False

    root = Path(__file__).resolve().parents[1]
    out: set[str] = set()
    for path in sorted(root.rglob("*.py")):
        try:
            tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        except SyntaxError:
            continue
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            aliases = {
                node.targets[0].id
                for node in ast.walk(fn)
                if isinstance(node, ast.Assign)
                and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and (is_url_read(node.value)
                     or is_hand_composed_authority(node.value))
            }
            for node in ast.walk(fn):
                if isinstance(node, ast.Call):
                    for kw in node.keywords:
                        if kw.arg in peer_fields and is_suspect(kw.value, aliases):
                            out.add(fn.name)
                elif isinstance(node, ast.Assign):
                    for tgt in node.targets:
                        if (
                            isinstance(tgt, ast.Subscript)
                            and isinstance(tgt.slice, ast.Constant)
                            and tgt.slice.value in peer_fields
                            and is_suspect(node.value, aliases)
                        ):
                            out.add(fn.name)
    return out


def test_no_peer_authority_is_fed_from_anything_but_peer_url():
    """Class (8)'s completeness guard — the half the classifier cannot see.

    The classifier reads `peer_url`, so a relay written any other way is not
    merely unclassified: it is INVISIBLE, and shows up in a docker run as a ❌
    against working federation code — the exact failure class (8) exists to
    prevent. Since a client construction and a relay are the same syntax, no
    rule can separate them; a reviewed list can.

    New name here? Decide which act it is. A nest being asked to dial another
    nest takes `peer_url` (and becomes class (8) automatically). A client this
    process dials itself joins the list above, with a comment saying whose nest
    it is.
    """
    actual = _peer_fields_not_fed_from_peer_url()
    unexpected = actual - _PEER_FIELDS_REVIEWED_AS_CLIENT_DIALS
    assert not unexpected, (
        "these feed a peer-authority request field from something the class "
        f"(8) classifier cannot see: {sorted(unexpected)}. If one asks a NEST "
        "to dial another nest, use the handle's `peer_url` key instead — that "
        "is the contract key for the authority another nest dials, and reading "
        "it is what marks the test class (8). If it is a client this process "
        "dials itself, add it to _PEER_FIELDS_REVIEWED_AS_CLIENT_DIALS with a "
        "comment."
    )
    stale = _PEER_FIELDS_REVIEWED_AS_CLIENT_DIALS - actual
    assert not stale, (
        f"reviewed entries that no longer exist: {sorted(stale)} — drop them, "
        "so the list keeps meaning what it says"
    )


# ---------------------------------------------------------------------------
# N nests per run, arm 1: the zero-option dedicated-nest fixtures are the mode
# provider's to start (`testing.md` § Default app and nest mode, ruling (1)).
# ---------------------------------------------------------------------------

#: The zero-option dedicated-nest fixtures arm 1 routed through the provider.
#: Each asks for a nest and nothing else, and every consumer reads only
#: `url` / `port` / `admin` — which is what makes them the first able to be a
#: second CONTAINER rather than a second local process.
_PROVIDER_ROUTED_NESTS = (
    "second_nest",
    "third_nest",
    "stoppable_nest",
    "rotatable_nest",
    "reclaimable_nest",
    "delete_account_nest",
    "dedicated_no_mail_nest",
)


def test_the_zero_option_nest_fixtures_are_admitted_to_docker():
    """Arm 1's whole point, stated as the outcome rather than the mechanism.

    Before this, each of the seven took `nest_binary` as a fixture parameter and
    called `_make_nest` itself — two independent exclusion signals, both of them
    honest at the time and both wrong once the mode owns the start. A docker run
    now gets a second container here instead of a silent deselect.
    """
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    for fixture in _PROVIDER_ROUTED_NESTS:
        verdict = ns.classify(_FakeItem(fixturenames=[fixture, "app"]), docker)
        assert verdict is None, (
            f"{fixture} is routed through the mode provider, so nothing about "
            f"a locally-compiled binary may exclude it: {verdict}"
        )


def test_the_routed_fixtures_no_longer_name_the_binary_or_make_nest():
    """The two exclusion signals are gone at the SOURCE, not masked downstream.

    `LOCAL_NEST_FIXTURES` is derived from conftest's AST and `nest_binary` from
    the fixture closure, so a fixture that still spelled either would come back
    into both tables on the next derivation. Asserting on the source is what
    makes the tables above a consequence rather than a parallel claim.
    """
    import ast
    from pathlib import Path

    conftest = Path(__file__).resolve().parents[1] / "conftest.py"
    tree = ast.parse(conftest.read_text())
    funcs = {
        n.name: n
        for n in tree.body
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }

    for fixture in _PROVIDER_ROUTED_NESTS:
        node = funcs[fixture]
        params = {a.arg for a in node.args.args}
        assert "nest_binary" not in params, (
            f"{fixture} still takes `nest_binary` as a fixture parameter, which "
            "puts it back in every non-standalone mode's exclusion closure — "
            "resolve it lazily through `_start_dedicated_nest` instead"
        )
        called = {
            sub.func.id
            for sub in ast.walk(node)
            if isinstance(sub, ast.Call) and isinstance(sub.func, ast.Name)
        }
        assert "_make_nest" not in called, (
            f"{fixture} still calls `_make_nest` directly, so it spawns a local "
            "binary whatever the run's mode says — go through the provider"
        )
        assert "_start_dedicated_nest" in called, (
            f"{fixture} must start its nest through the mode provider seam"
        )


#: The MODULE-LOCAL dedicated-nest fixtures routed through the provider seam, as
#: `<path relative to tests/e2e-unified>::<fixture>`.
#:
#: These are the twin of `_PROVIDER_ROUTED_NESTS` one layer out: same zero-option
#: shape, same two exclusion signals, but defined in a test module rather than in
#: conftest — which is the half `LOCAL_NEST_FIXTURES` was never able to see (its
#: docstring says so). Each was a verbatim inlining of `_make_nest` plus its
#: cleanup: `mktemp(label)`, `find_free_port()`, `start_nest(...)`, then
#: terminate/wait/kill. Nothing about them was bespoke except the label.
_PROVIDER_ROUTED_MODULE_LOCAL_NESTS = (
    "tests/test_folder_paywall.py::paywall_nest",
    "tests/test_gated_post_compose.py::gated_nest",
    "tests/test_identity_succession_aftermath.py::grant_mint_nest",
    "tests/test_nest_trust.py::trust_mint_nest",
    "tests/test_sell_post.py::sell_nest",
    "tests/test_subscription_payments.py::payments_nest",
    "tests/test_subscriptions.py::subs_nest",
    "tests/test_subscriptions.py::subs_consumer_nest",
    # The `tests/api/` bespoke single-nest family. Same zero-option shape as the
    # eight above, and routed for the same reason — but the family is worth
    # naming separately because a premise this row carried about it turned out
    # to be false. Three of them (`dr_nest`, `report_nest`, `signal_nest`) read
    # `nest["db_path"]` and poke the nest's SQLite file directly, which reads
    # like exclusion class (4) db/poking and therefore like a family that must
    # NOT be admitted to docker. Measured: docker answers `db_path` (and
    # `blob_dir`/`tmp_dir`/`proc`), because `_DockerProvider` bind-mounts a host
    # directory at `/data` and the image's `fauna` user is uid 1000 — the same
    # uid the dev VMs run as — so the host side is genuinely readable.
    # `DOCKER_ABSENT` is only `{config_path, node_binary, log_path}`. The class
    # bites on **live**, which answers none of them, and live already refuses a
    # dedicated start outright.
    "tests/api/test_dr_restore.py::dr_nest",
    "tests/api/test_invite_requests.py::nest",
    "tests/api/test_payments_webhook.py::paid_nest",
    "tests/api/test_report_sharing.py::report_nest",
    "tests/api/test_signal_sharing.py::signal_nest",
    "tests/api/test_snapshot_create_idempotency.py::snap_nest",
    "tests/api/test_wire_version_skew.py::skew_nest",
    # The nest-DIALS-nest federation pair (2026-09-02). Routed like every other
    # zero-option fixture, but they are the first whose tests must then be
    # EXCLUDED again — by class (8) rather than by a binary closure, which is
    # the whole point of the arm: the reason a docker run gives becomes the true
    # one ("the product refuses this dial") instead of the incidental one ("the
    # fixture compiles a binary"). Their peer authorities moved to `peer_url` in
    # the same commit, because routing them without that migration would have
    # admitted them to docker with class (8) unable to see them.
    "tests/api/test_federation_exchange_originator.py::two_report_nests",
    "tests/api/test_trending_federation.py::two_trend_nests",
    # The two ordinary single-nest stragglers of the zero-option census. Neither
    # is a mode question about its own nest: `head_nest` version-pins the
    # CLIENTS and not the nest, and `local_nest` only ever wanted "a nest with
    # open registration". Routing `local_nest` also retired the last fixed nest
    # port (13025) and the module-local `nest_binary` that shadowed conftest's.
    "tests/test_client_at_rest_upgrade.py::head_nest",
    "tests/platform/linux/test_desktop_join.py::local_nest",
    # The HOST-driven federation family (2026-09-02) — the mirror image of the
    # nest-dials-nest pair above, and the reason the distinction is worth the
    # words. These three drive `clients/ws_rpc_federation_client.py`, which
    # opens `/api/v1/federation/ws` on the listener *from the pytest process*
    # and signs the hello as the initiator. No nest dials another, so
    # `validate_peer_url` never runs and class (8) never fires: routing them
    # admits them to docker for real rather than swapping one exclusion reason
    # for another. Their blocker was never the fixture — it was the harness's
    # client hard-coding the loopback (empty) served-cert SPKI against an image
    # that always serves TLS, closed in the commit before this one.
    "tests/api/test_cross_nest_api.py::two_nests",
    "tests/api/test_post_forwarding.py::public_nest",
    "tests/api/test_post_forwarding.py::initiator_nest",
    # The `tests/api/` PLAIN-FUNCTION family (2026-09-02) — tests that inlined
    # the start into their own body rather than into a fixture, so there was no
    # fixture for either AST pin to grade and `node_binary` reached them as a
    # test parameter. Same zero-option shape as every routed family above; the
    # only new thing is that routing them had to MINT the fixture rather than
    # rewrite one.
    "tests/api/test_services.py::services_nest",
    "tests/api/test_admin_auth.py::admin_auth_nest",
    "tests/api/test_deploy_verify_serving.py::deploy_gate_nest",
    # …and the OPTION-PASSING half of the same family — the first module-local
    # fixtures to route while asking a nest for something. They are why the
    # declared-options table had to learn to read the whole tree first: until it
    # did, `classify` could not see a module-local fixture's options at all, so
    # an option a mode could not honour would have reached the provider's
    # runtime backstop instead of a collection-time class (4). All three ask for
    # options docker declares (`unclaimed`, `claim_domain`), so they route
    # rather than re-classify.
    "tests/api/test_dav_enable_independence.py::dav_unclaimed_nest",
    "tests/api/test_claim_primary_domain.py::claim_target_nest",
    "tests/api/test_web_subdomain_hosting.py::subdomain_nest",
    # Box recovery: three sequential boxes in one journey, and the first FACTORY
    # fixture the seam has. Its options are decided at runtime (the seed comes
    # from the original box's claim reply), so its two call shapes are spelled
    # out rather than parameterised — a factory forwarding an options dict would
    # ask the provider for `extra_env` with `FIXTURE_START_OPTIONS`, which reads
    # literal kwargs, unable to see it.
    "tests/api/test_box_recovery.py::box_recovery_nest",
    "tests/api/test_box_recovery.py::rebuilt_box",
    # The schema-skew family: stop the nest, mutate `schema_meta` on disk with
    # host-side sqlite3, restart on the SAME data dir. It reads standalone-only
    # from the calling side and is not, and both mechanisms that make it route
    # are in the code being CALLED: `start_nest_in_place` delegates to the
    # handle's own `start_in_place` (docker restarts its container), and docker
    # answers `db_path` because `/data` is a host bind-mount owned by uid 1000.
    "tests/api/test_schema_version_compat.py::schema_skew_nest",
    # The self-service onboarding journey, and the last routable `node_binary`
    # consumer. Its blocker was never a fixture either: the nest carried a
    # `--handle-domain` BOOT SEED, standalone-only by ruling (3), and the test
    # needed the domain because it asserts `nest.info.domain`, asserts
    # `registration.handle_domain` echoes it, and registers alice AT it. The
    # replacement is not a provider mechanism but a different door in the
    # product — the test's own step-0 claim now carries `mail_domain=DOMAIN`,
    # a wire act every mode honours — so routing it cost one changed argument
    # rather than a new option. The claim stays the TEST's (the ceremony is
    # what step 0 witnesses), which is why the fixture is `unclaimed=True`
    # rather than a `claim_domain` hand-off to the harness.
    "tests/api/test_onboarding.py::onboarding_nest",
    # The APP-JOURNEY option-passing family (2026-09-02) — the first routed
    # fixtures outside `tests/api/`, and the ones whose tests drive a real app
    # UI rather than the wire. Every option they pass is one docker declares
    # (`unclaimed`, `serve_tls`, `claim_domain`), so all six route rather than
    # re-classify, and the census that named them is now exhausted of
    # zero-option module-locals: what is left passes `handle_domain_seed` (the
    # two shapes no claim can express) or a non-default `config_name` (the
    # `paired` fixture's private NAT seed), both permanent class-(4) residents.
    #
    # `autoenable_nest` is also the seam's first MODULE-scoped caller, which is
    # safe by construction rather than by luck: `_start_dedicated_nest` takes
    # only `request` plus the session-scoped `nest_mode` / `tmp_path_factory`,
    # and resolves the session-scoped `nest_binary` lazily — nothing narrower
    # than module scope is in its closure.
    "tests/test_trust_prompt.py::unclaimed_trust_nest",
    "tests/test_tui_credential_store_rekey.py::rekey_unclaimed_nest",
    "tests/test_tui_headless_credential_store.py::headless_unclaimed_nest",
    "tests/test_onboarding_dns_glue_windows.py::windows_onboard_nest",
    "tests/test_onboarding_self_signed_probe.py::self_signed_tls_nest",
    "tests/test_mail_auto_enable_first_setup.py::autoenable_nest",
    # The NAMED-BUILD family (2026-10-05) — the first routed fixtures whose
    # standalone build is not `nest_binary`. Each needs a provider the default
    # build does not compile (`bluesky`, `activitypub`), so each names its build
    # to the seam (`binary=`, ruling (1)) and a container run serves the
    # shipped image, which compiles all three. `DEDICATED_NEST_BINARIES` is the
    # table, pinned below to the literal each passes. The Bridges page's nest
    # stopped needing its narrow build to be load-bearing on the same day:
    # every app filters that page through `is_unified_bridges_page_bridge`,
    # so the image renders the one ActivityPub card too, and
    # `BridgesActions._card_index` refuses to guess the day it does not.
    "tests/api/test_bluesky_oauth.py::domained_bluesky_nest",
    "tests/api/test_bluesky_oauth.py::domainless_bluesky_nest",
    "tests/test_bridges.py::bridges_nest",
)


#: `_LOG_PATH_READERS` and `test_every_log_path_reader_is_accounted_for` were
#: RETIRED 2026-09-02, and by having their subject removed rather than by being
#: dispositioned away. The map graded every raw `nest["log_path"]` read because
#: the key was publishable by one provider only — the standalone one redirects
#: the nest process's output to a file it names, while "a container's log lives
#: in the daemon" — so a routed fixture whose test read it by subscript raised
#: in docker, once inside a FAILURE-PATH diagnostic where the raise replaced the
#: assertion that had actually failed.
#:
#: Docker now publishes the key too (`conftest._ContainerLogStream` follows the
#: container's log into `<tmp_dir>/nest.log`, the same path standalone uses), so
#: the hazard has no remaining routed mode to fire in. The only mode still
#: declaring the key absent is LIVE, which declares *every* capability absent
#: for one reason — the nest is on another machine — and excludes every test
#: that starts a nest long before a handle read could be reached. A map narrowed
#: to that would be a second mechanism for a fact `LIVE_ABSENT` already carries,
#: and the guard it duplicates is self-diagnosing by construction: a live read
#: raises `NestCapabilityError` naming the mode and the reason, not a bare
#: `KeyError`.
#:
#: Kept as a note rather than deleted silently because the map's own entry was a
#: standing hazard someone would otherwise go looking for:
#: `test_web_claim_pin_wasm_witness.py` reads the box's real `nest_actor_id` off
#: its console claim banner and its docstring says the log IS the assertion, so
#: "mode-tolerance here is a redesign rather than a `.get`". That redesign is
#: what did not have to happen: the banner is in the container's log as readily
#: as in a file beside a binary (measured on `ghcr.io/faunasocial/nest:latest`),
#: so the test's own witness now works in either mode unchanged.


#: Every read of a mail venue's BRIDGE-PROCESS affordances — the MTA subprocess
#: and the two bridge log files — from outside the fixture that builds them,
#: with the disposition that keeps it. Empty is the target state and the current
#: one: the two things these reads were doing are now handle METHODS every venue
#: answers (`assert_mta_running`, `bridge_log_hint`).
#:
#: Why this needs a pin, and why it belongs on the nest-mode axis at all. Ruling
#: (3) says the docker shape of a mail nest is the image's own s6-supervised
#: bridges, and names `mda.respawn()`, the MTA log file and the metrics port as
#: that mode's declared absences. `mta_proc` is a `subprocess.Popen` and
#: `mta_log_file` is a path on the HOST filesystem — neither exists when the
#: bridges are s6 services inside a container, where the same two questions are
#: answered by `s6-svstat` and `docker logs`. So every raw read of them is a
#: standalone-only fact wired into a test body, and the count was not small:
#: **29 byte-identical liveness preconditions and 29 log-path pointers, across 25
#: files**, all of them written by copying a neighbour.
#:
#: That is the same shape as the retired `_LOG_PATH_READERS` one pin up — a key
#: only one provider can publish, read straight off a harness handle — and it is
#: the fourth time this arm has met it. The remedy differed each time, and the
#: three answers rank: `log_path` had two readers and took a `.get`, until 2026-
#: 09-02 showed the key itself could be published in both modes and the map went
#: away entirely; these had fifty-eight, where a per-site fallback would have
#: been fifty-eight chances to get it wrong. The contract moved to the handle
#: instead, which is the same answer
#: `start_in_place` gave for restarting a nest: the thing that KNOWS how the
#: bridges run is the thing that should answer questions about them.
_BRIDGE_PROCESS_READERS: dict[str, str] = {}


class _FakeProc:
    """The only two things `assert_mta_running` asks of a bridge process."""

    def __init__(self, code=None):
        self._code = code

    def poll(self):
        return self._code


def _mail_venue(**fields):
    """A bare handle carrying only the mixin — the contract under test is the
    mixin's, and a real `DedicatedMailNestHandle` would drag a nest, two spawned
    bridges and a stub MX in with it."""
    conftest = pytest.importorskip("conftest")

    venue = type("_Venue", (conftest.MailVenueBridges,), {})()
    for k, v in fields.items():
        setattr(venue, k, v)
    return venue


def test_assert_mta_running_passes_while_the_bridge_lives():
    """The precondition twenty-nine call sites used to spell by hand."""
    _mail_venue(mta_proc=_FakeProc(None), mta_log_file="/tmp/mta.log").assert_mta_running()


def test_assert_mta_running_reports_the_exit_status_and_the_log():
    """A dead MTA is never diagnosable from the assertion alone, so the failure
    carries the venue's own log pointer — which is why the hand-written idiom
    interpolated `mta_log_file` and why the method must not lose it."""
    venue = _mail_venue(mta_proc=_FakeProc(2), mta_log_file="/tmp/mta.log")
    with pytest.raises(AssertionError) as exc:
        venue.assert_mta_running()
    assert "status 2" in str(exc.value)
    assert "/tmp/mta.log" in str(exc.value)


def test_assert_mta_running_names_a_venue_that_runs_no_mta():
    """The CalDAV-only variant spawns none. The raw idiom answered this case with
    `AttributeError: 'NoneType' object has no attribute 'poll'`; a test asserting
    the precondition has stated it needs an MTA, so it should be told that."""
    venue = _mail_venue(mta_proc=None, mta_log_file=None)
    with pytest.raises(AssertionError) as exc:
        venue.assert_mta_running()
    assert "no MTA" in str(exc.value)


def test_the_log_hint_names_the_role_and_never_raises():
    """`bridge_log_hint` is interpolated into failure messages, several of them
    inside `except` blocks. A raise there replaces the assertion that actually
    failed — the exact trap `log_path` sprang on the apple diagnostic — so an
    unanswerable role must degrade to a sentence."""
    venue = _mail_venue(mta_log_file="/tmp/mta.log")
    assert venue.bridge_log_hint() == "mta log: /tmp/mta.log"
    assert "not exposed" in venue.bridge_log_hint("mda")


def test_the_log_lines_reader_is_empty_rather_than_raising(tmp_path):
    """Same rule one layer down, for the diagnostic that greps the log instead of
    pointing at it: a venue that cannot answer, and a path that will not open,
    both give back nothing."""
    log = tmp_path / "mda.log"
    log.write_text("first\nsecond\n")
    assert _mail_venue(mda_log_file=str(log)).bridge_log_lines("mda") == ["first", "second"]
    assert _mail_venue(mda_log_file=None).bridge_log_lines("mda") == []
    assert _mail_venue(mda_log_file=str(tmp_path / "nope.log")).bridge_log_lines("mda") == []


def test_every_handle_that_carries_a_bridge_log_answers_the_venue_questions():
    """A conftest handle that stores a bridge log file MUST mix in
    `MailVenueBridges`.

    The three tests above pin what the mixin DOES; none of them pinned who HAS
    it, and that is the gap that bit. `UnclaimedMailNestUiHandle` stored
    `mta_log_file` and inherited nothing, so five linux tests in the 2026-09-11
    `--app linux` sweep died with `AttributeError: 'UnclaimedMailNestUiHandle'
    object has no attribute 'bridge_log_hint'` INSTEAD of reporting the mail
    failure they had actually hit — the helper is
    interpolated inside `except` blocks, so a missing one does not just fail to
    help, it destroys the evidence and makes five different failures look like
    one harness crash.

    Detecting by "stores a bridge log" rather than by a hand-kept class list is
    the point: a new venue handle inherits the requirement the moment it holds
    the field, with nobody remembering to add it here.

    Red-verified by dropping the base class back off `UnclaimedMailNestUiHandle`:
    this fails naming that class.
    """
    import inspect

    conftest = pytest.importorskip("conftest")
    needles = ("self.mta_log" + "_file =", "self.mda_log" + "_file =")

    offenders = []
    for name, obj in vars(conftest).items():
        if not inspect.isclass(obj) or obj is conftest.MailVenueBridges:
            continue
        if getattr(obj, "__module__", None) != conftest.__name__:
            continue
        try:
            src = inspect.getsource(obj)
        except OSError:  # pragma: no cover - source always available here
            continue
        if not any(n in src for n in needles):
            continue
        if not issubclass(obj, conftest.MailVenueBridges):
            offenders.append(name)

    assert not offenders, (
        f"these conftest handles store a bridge log file but do not mix in "
        f"`MailVenueBridges`: {sorted(offenders)}. A venue that holds the log "
        f"must answer `bridge_log_hint()` / `bridge_log_lines()`, because both "
        f"are interpolated into failure messages inside `except` blocks — an "
        f"AttributeError there replaces the assertion that actually failed."
    )


def test_no_test_reads_a_bridge_process_or_log_path_directly():
    """The MTA process and the bridge logs are venue-shaped, so tests ask the
    handle rather than the host.

    Red-verified by reverting one call site to the old three-line idiom: the pin
    names that file. Red-verified in the staleness direction too, by the lift
    itself — the map started with twenty-five entries and every one of them had
    to go before this went green.
    """
    from pathlib import Path

    # Split so this pin's own source does not contain a needle, the same
    # self-matching trap `_LOG_PATH_READERS` solves the same way. The skip by
    # filename below is the belt to this suspenders: the prose above names the
    # attributes, and prose is scanned no differently from code.
    #
    # No leading dot, deliberately. `test_caldav_client_seal_to_mua.py` reached
    # the same field through `getattr(handle, "mda_log_file", None)` — a STRING,
    # invisible to an attribute-shaped needle — and it was the one site doing
    # something a `.get`-style fallback could plausibly have excused, so a dotted
    # needle would have left exactly the hardest case unwatched. The names are
    # distinctive enough that the wider match costs nothing.
    needles = ("mta_" + "proc", "mta_log" + "_file", "mda_log" + "_file")

    root = Path(__file__).resolve().parents[1]
    found = set()
    for folder in ("tests", "helpers", "actions", "common"):
        for source in sorted((root / folder).rglob("*.py")):
            if source.name == "test_nest_mode_axis.py":
                continue
            rel = source.relative_to(root).as_posix()
            if any(n in source.read_text() for n in needles):
                found.add(rel)

    unmapped = sorted(found - set(_BRIDGE_PROCESS_READERS))
    assert not unmapped, (
        f"these files read a mail venue's bridge process or log path directly: "
        f"{unmapped}\nUse the handle's own `assert_mta_running()` for the "
        f"liveness precondition and `bridge_log_hint()` for the failure-message "
        f"pointer — both are answerable by a venue whose bridges are s6 services "
        f"in a container, which a `Popen` and a host path are not "
        f"(testing.md § Default app and nest mode, ruling (3)). If the raw "
        f"handle really is the subject, add it here saying why."
    )
    stale = sorted(set(_BRIDGE_PROCESS_READERS) - found)
    assert not stale, (
        f"`_BRIDGE_PROCESS_READERS` names files that no longer read one: {stale}\n"
        f"Drop the entry — a map that outlives its subject stops being read."
    )



# ── arm 6: the mail-venue seam ──────────────────────────────────────────────
#
# A mail venue is a provider METHOD, not a start option, so it needs its own
# small family of pins: the fixture→venue-options table (the twin of
# `test_the_fixture_start_options_table_matches_the_tree`), the exact map of
# tests that read a HOST-spawn affordance off the handle, and the private
# container seam the docker venue reaches `start` through.

def _venue_fixtures(source: str) -> dict[str, frozenset]:
    """`{fixture: literal venue kwargs}` for the fixtures in `source` that route
    through `_start_mail_venue`.

    Deliberately simpler than `_derive_start_options`, and the difference is a
    fact rather than a shortcut: the venue seam has ONE call site per fixture,
    always in the fixture body, never behind an impl helper — so there is no
    cross-file closure to chase. If that ever stops being true this pin goes red
    (the fixture disappears from the derivation), which is the correct failure.

    Same falsy rule as its twin: `registration_open=False` is asking for the
    default, not requesting an option.
    """
    import ast as _ast

    out = {}
    tree = _ast.parse(source)
    for node in _ast.walk(tree):
        if not isinstance(node, (_ast.FunctionDef, _ast.AsyncFunctionDef)):
            continue
        asked = set()
        found = False
        for call in _ast.walk(node):
            if not (isinstance(call, _ast.Call)
                    and isinstance(call.func, _ast.Name)
                    and call.func.id == "_start_mail_venue"):
                continue
            found = True
            for kw in call.keywords:
                if kw.arg is None:
                    continue
                if isinstance(kw.value, _ast.Constant) and not kw.value.value:
                    continue
                asked.add(kw.arg)
        if found:
            out[node.name] = frozenset(asked)
    return out


def test_the_mail_venue_options_table_matches_the_tree():
    """`MAIL_VENUE_FIXTURE_OPTIONS` is pinned to the kwargs each venue fixture
    LITERALLY passes `_start_mail_venue` — the venue twin of the start-options
    pin above, and needed for exactly the same reason.

    A fixture the table cannot see cannot be a collection-time class (4). It
    would route into a container, ask for a venue shape this mode cannot stand
    up, and get the provider's setup-time refusal instead — a ❌ against working
    product code, which is the outcome the arm ordering exists to prevent.

    The table is also what says "this fixture IS a mail venue": a routed fixture
    asking for nothing still carries an empty set, because absence is what the
    derivation reads as "never routed". Same lesson `FIXTURE_START_OPTIONS`
    records one table up.
    """
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]
    derived = _venue_fixtures((root / "conftest.py").read_text())

    def _shown(mapping, name):
        return sorted(mapping[name]) if name in mapping else "ABSENT"

    assert derived == dict(ns.MAIL_VENUE_FIXTURE_OPTIONS), (
        "the tree's per-fixture MAIL VENUE options and "
        "nest_surface.MAIL_VENUE_FIXTURE_OPTIONS have diverged.\n"
        + "\n".join(
            f"  {name}: tree={_shown(derived, name)} "
            f"table={_shown(ns.MAIL_VENUE_FIXTURE_OPTIONS, name)}"
            for name in sorted(set(derived) | set(ns.MAIL_VENUE_FIXTURE_OPTIONS))
            if derived.get(name) != ns.MAIL_VENUE_FIXTURE_OPTIONS.get(name)
        )
        + "\nA venue fixture asking for a shape its mode's provider does not "
        "declare is a collection-time declared absence; this table is what the "
        "classifier subtracts `supported_venue_options` from."
    )


def test_no_venue_fixture_is_in_both_option_tables():
    """A fixture asks a NEST for start options or asks a PROVIDER for a venue —
    never both, because the two are answered by different declarations.

    Without this, a venue fixture left behind in `FIXTURE_START_OPTIONS` would be
    graded against `supported_options` as well, and a docker provider growing a
    *nest* option would silently un-exclude a *venue* it still cannot stand up.
    """
    from helpers import nest_surface as ns

    both = sorted(set(ns.FIXTURE_START_OPTIONS) & set(ns.MAIL_VENUE_FIXTURE_OPTIONS))
    assert not both, (
        f"these fixtures are in BOTH option tables: {both}\n"
        f"A mail-venue fixture passes no per-nest start options at all — it "
        f"passes venue options to `_start_mail_venue`. Remove it from "
        f"`FIXTURE_START_OPTIONS`."
    )


#: The attribute names a mail venue exposes only because its bridges run on the
#: HOST. Split so this file's own source does not contain a needle for the
#: sibling raw-read scan, the same self-matching trap `_LOG_PATH_READERS` solves
#: the same way.
_VENUE_HOST_ONLY_ATTRS = (
    "stub" + "_mx", "dkim_public" + "_dns_value", "dkim" + "_selector",
    "mda", "mta", "mta" + "_proc", "mta_log" + "_file", "mda_log" + "_file",
)

#: The fixtures that route through the venue seam — the derivation's seed. Kept
#: here rather than imported from the table under test, so the pin cannot agree
#: with a wrong table by construction.
_VENUE_FIXTURE_NAMES = (
    "dedicated_mail_nest",
    "dedicated_mail_nest_handle_domain",
    "dedicated_caldav_mailbox_less_nest",
    "dedicated_caldav_only_nest",
)


def test_mail_venue_host_affordance_readers_are_exactly_pinned():
    """`MAIL_VENUE_HOST_AFFORDANCE_READERS` is the exact set of test functions
    that read a host-spawn affordance off a ROUTED mail venue.

    Both directions matter and each has already earned itself elsewhere in this
    file: an UNMAPPED reader is a test that would route into a container and die
    on an `AttributeError` in its own body, and a STALE entry is a test excluded
    from docker for a reason that stopped being true — the quieter of the two
    failures, and the one a shrinking count would never show.

    Only the routed fixtures are seeded. A fixture that has not routed is
    excluded for a more basic reason already (its binary closure), so a reader
    there is not yet a problem — and listing it would make this map look like a
    backlog instead of a boundary.
    """
    import ast as _ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]
    seeds = set(_VENUE_FIXTURE_NAMES)
    found = set()
    for source_path in sorted((root / "tests").rglob("*.py")):
        if source_path.name == "test_nest_mode_axis.py":
            continue
        try:
            tree = _ast.parse(source_path.read_text())
        except (OSError, SyntaxError):
            continue
        rel = source_path.relative_to(root).as_posix()
        for node in _ast.walk(tree):
            if not isinstance(node, (_ast.FunctionDef, _ast.AsyncFunctionDef)):
                continue
            params = [a.arg for a in node.args.args]
            used = seeds & set(params)
            if not used:
                continue
            # Follow one level of aliasing (`handle = dedicated_mail_nest`),
            # which is how almost every consumer names it.
            alias = set(used)
            for sub in _ast.walk(node):
                if (isinstance(sub, _ast.Assign)
                        and isinstance(sub.value, _ast.Name)
                        and sub.value.id in alias):
                    for t in sub.targets:
                        if isinstance(t, _ast.Name):
                            alias.add(t.id)
            for sub in _ast.walk(node):
                if (isinstance(sub, _ast.Attribute)
                        and isinstance(sub.value, _ast.Name)
                        and sub.value.id in alias
                        and sub.attr in _VENUE_HOST_ONLY_ATTRS):
                    found.add(f"{rel}::{node.name}")

    mapped = set(ns.MAIL_VENUE_HOST_AFFORDANCE_READERS)
    unmapped = sorted(found - mapped)
    assert not unmapped, (
        f"these tests read a HOST-spawn affordance off a routed mail venue and "
        f"are not pinned: {unmapped}\n"
        f"In docker the venue's bridges are s6 services — there is no "
        f"harness-held process to poll, no host log path, and no operator-hatch "
        f"MX override routing to an in-process SMTP sink. Either ask the handle "
        f"a venue-shaped question instead, or add the test to "
        f"`MAIL_VENUE_HOST_AFFORDANCE_READERS` with the disposition that keeps "
        f"it standalone-only."
    )
    stale = sorted(mapped - found)
    assert not stale, (
        f"`MAIL_VENUE_HOST_AFFORDANCE_READERS` names tests that no longer read "
        f"one: {stale}\nDrop the entry — each one is a docker exclusion, so a "
        f"stale line keeps a test out of a mode it could now run in."
    )


def test_the_private_container_venue_seam_has_one_caller():
    """`_DockerProvider.start`'s `_venue_ports` / `_venue_env` are the provider's
    own seam, reachable only from `start_mail_venue`.

    They are keyword-only and underscore-prefixed so a fixture cannot pass them
    through `**kwargs` and have them skip `_refuse_unsupported` — which would be
    a per-nest knob nothing declares and nothing grades. This pin is the belt to
    that suspenders: if a second caller appears, the honest move is a declared
    venue option or a named start option, not a wider private door.
    """
    import ast as _ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    source = (root / "conftest.py").read_text()
    tree = _ast.parse(source)

    callers = set()
    for node in _ast.walk(tree):
        if not isinstance(node, (_ast.FunctionDef, _ast.AsyncFunctionDef)):
            continue
        for call in _ast.walk(node):
            if not isinstance(call, _ast.Call):
                continue
            names = {kw.arg for kw in call.keywords if kw.arg}
            if names & {"_venue_ports", "_venue_env"}:
                callers.add(node.name)

    assert callers == {"start_mail_venue"}, (
        f"the private container-venue seam is reached from {sorted(callers)}, "
        f"not just `start_mail_venue`. A second caller means the need is no "
        f"longer venue-private: declare it as a venue option (graded against "
        f"`supported_venue_options`) or as a named start option, so the "
        f"classifier can see it at collection instead of the provider "
        f"discovering it at setup."
    )


def test_every_provider_declares_its_venue_options_and_answers_the_seam():
    """Every registered provider answers `start_mail_venue` and declares a
    `supported_venue_options`, so no mode reaches a venue fixture by accident.

    The default in `nest_mode.supported_venue_options` is the empty set — a
    provider that forgets to declare refuses loudly rather than accepting a venue
    shape it does not build. This pin makes "forgot to declare" visible at
    tier_1, where the alternative is a mode that silently stands up no mail venue
    and reports a pass.
    """
    import conftest as _conftest
    from helpers import nest_mode as nest_mode_mod

    for mode_name in (nest_mode_mod.STANDALONE, nest_mode_mod.DOCKER,
                      nest_mode_mod.LIVE):
        provider = nest_mode_mod.provider_for(
            nest_mode_mod.NestMode(mode_name))
        assert callable(getattr(provider, "start_mail_venue", None)), (
            f"provider for {mode_name!r} does not answer `start_mail_venue`")
        assert isinstance(getattr(provider, "supported_venue_options", None),
                          frozenset), (
            f"provider for {mode_name!r} does not declare "
            f"`supported_venue_options` as a frozenset")

    # Docker's declaration is the one the classifier subtracts, so pin what it
    # deliberately does NOT carry: `caldav_admin_port` is standalone-only by
    # ratification (s6 owns the rebind in the image). `caldav_only` DOES carry
    # now — the second bring-up path is built, and this line moving from the
    # absent set to the present one is the whole of that arm's declaration.
    docker = _conftest._DockerProvider()
    assert "caldav_admin_port" not in docker.supported_venue_options
    assert "caldav_only" in docker.supported_venue_options
    assert "registration_open" in docker.supported_venue_options


def test_the_caldav_only_docker_venue_publishes_no_mail_port():
    """A CalDAV-only container venue exposes `caldav_port` and NO mail port.

    **The venue's shape is read off its own publication**, which is why this can
    be pinned without a container: a CalDAV-only deployment writes no
    `/data/imap-enabled`, so the image's MTA re-downs itself by its own
    run-script gate (`docker/s6/fauna-mail-bridge-mta/run`) and its three SMTP
    listeners never bind. Publishing 25/465/587 would therefore hand a consumer
    three host ports that accept nothing — the quietest possible failure, and
    exactly the "declared absent, never silently None" line this class already
    holds for the host-spawn affordances.

    `imaps_port` is absent for a subtler reason worth keeping: standalone's
    CalDAV-only venue DOES expose one, because `_spawn_mda_bridge(pin_caldav_hatch=
    True)` pins IMAP listeners into the operator hatch whatever the deployment
    toggle says — the same host-spawn affordance hiding in plain sight that the
    CalDAV listener turned out to be. The image has no hatch and binds each DAV
    protocol per its own `fetch_config` flag, so with mail off there is genuinely
    no IMAPS listener here. Attribute-absent rather than `None`: a test reading
    it gets an `AttributeError` naming this class, not a falsy value it might
    skip on.
    """
    import conftest as _conftest

    caldav_only = _conftest.DockerMailVenueHandle(
        nest={}, domain="fauna.test", mail_ports={8443: 41111},
        container_name="fauna-nest-axis-1",
    )
    assert caldav_only.caldav_only is True
    assert caldav_only.caldav_port == 41111
    for absent in ("mx_port", "submission_port_465", "submission_port_587",
                   "imaps_port"):
        assert not hasattr(caldav_only, absent), (
            f"a CalDAV-only container venue publishes no SMTP/IMAP port, so "
            f"{absent!r} must be absent rather than a host port that accepts "
            f"nothing"
        )

    full = _conftest.DockerMailVenueHandle(
        nest={}, domain="fauna.test",
        mail_ports={25: 1, 465: 2, 587: 3, 993: 4, 8443: 5},
        container_name="fauna-nest-axis-2",
    )
    assert full.caldav_only is False
    assert (full.mx_port, full.submission_port_465, full.submission_port_587,
            full.imaps_port, full.caldav_port) == (1, 2, 3, 4, 5)


def test_the_caldav_only_docker_venue_answers_the_two_mta_questions_by_refusing():
    """Both MTA-shaped questions name the CalDAV-only venue's missing MTA.

    The same two answers the standalone venue gives — `assert_mta_running`
    raises "runs no MTA" (`MailVenueBridges`) and `rebind_after_enable(mta=True)`
    refuses (`DedicatedMailNestHandle`) — because the venues answer ONE contract
    and a consumer must not have to know which one it holds.

    Neither is reachable as a live-container question: `assert_mta_running`'s
    supervisor read would report the image's MTA `down ... normally up` (its
    correct idle state) and pass, and `rebind_after_enable`'s mail branch would
    block on an SMTP banner from a listener that will never bind, failing after
    the whole budget with a timeout that names a port instead of the venue shape.
    Refusing up front is the difference between a diagnosis and a stall.
    """
    import conftest as _conftest

    handle = _conftest.DockerMailVenueHandle(
        nest={}, domain="fauna.test", mail_ports={8443: 41111},
        container_name="fauna-nest-axis-1",
    )
    with pytest.raises(AssertionError, match="no MTA"):
        handle.assert_mta_running()
    with pytest.raises(RuntimeError, match="mta=False"):
        handle.rebind_after_enable()



#: Every function still taking `tests/api/conftest.py::node_binary`, with the
#: disposition that keeps it there. Recorded as an exact map rather than a
#: shrinking count, so a NEW consumer is an edit someone has to justify.
#:
#: Two dispositions only, and the distinction is the one this arm keeps making:
#: **PREMISE** — a local binary is what the test is a witness OF, so the binary
#: closure is its true exclusion reason and routing it would assert nothing; and
#: **OPEN** — it is routable and simply has not been routed, which is the state
#: the sibling pin on `LOCAL_NEST_FIXTURES` refuses to let go unlabelled. An
#: OPEN entry is a backlog item with a name, never a silent survivor.
_NODE_BINARY_CONSUMERS = {
    "tests/api/test_deploy_verify_serving.py::"
    "test_real_health_serves_the_artifact_set_build_id_and_the_gate_binds_to_it":
        "PREMISE: `extra_env={'FAUNA_BUILD_ID': …}` plays the artifact's part — a "
        "local binary standing in for what the image does natively. Against a "
        "container the harness would override the artifact's own stamp in order "
        "to assert the artifact reports its own stamp.",
}


def test_every_surviving_node_binary_consumer_is_accounted_for():
    """`tests/api/conftest.py::node_binary` is the last un-routed binary fixture
    in the API tree, and this pin is what stops it becoming permanent furniture.

    The family it served was routed in six files; five functions still take it.
    Asserted as an exact map so that a **new** consumer — the shape someone
    writes out of habit, copying a neighbour — reds here instead of quietly
    re-growing the class this arm spent its passes shrinking. Removals are edits
    to a recorded fact, which is the same property `FIXTURE_START_OPTIONS` and
    `LOCAL_NEST_FIXTURES` are kept exhaustive for.

    It deliberately does NOT assert that every entry has a *structural* reason,
    the way the `LOCAL_NEST_FIXTURES` pin does — because four of the five do not:
    they are routable and simply not yet routed. Claiming otherwise would be the
    convenient lie this row has now caught twice (a recon that undercounted this
    very family, and a "class (4) db-poking" reading that measurement refuted).
    What it asserts instead is that each survivor carries a disposition someone
    wrote down.
    """
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    found = {}
    for source in sorted((root / "tests" / "api").glob("*.py")):
        rel = source.relative_to(root).as_posix()
        tree = ast.parse(source.read_text())
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if any(a.arg == "node_binary" for a in node.args.args):
                found[f"{rel}::{node.name}"] = node.lineno

    assert set(found) == set(_NODE_BINARY_CONSUMERS), (
        "the `node_binary` consumer set moved:\n"
        + "".join(
            f"  NEW (record a disposition, or route it): {q}\n"
            for q in sorted(set(found) - set(_NODE_BINARY_CONSUMERS))
        )
        + "".join(
            f"  GONE (drop it from the map): {q}\n"
            for q in sorted(set(_NODE_BINARY_CONSUMERS) - set(found))
        )
        + "\nA new consumer puts its test back in every non-standalone mode's "
        "exclusion closure. Route it through `_start_dedicated_nest` unless a "
        "locally-compiled binary is what the test is a witness OF."
    )

    unexplained = [
        q for q, why in _NODE_BINARY_CONSUMERS.items()
        if not why.startswith(("PREMISE:", "OPEN:", "OPEN,"))
    ]
    assert not unexplained, (
        f"a survivor with no disposition: {sorted(unexplained)} — say PREMISE "
        "(the binary is the subject) or OPEN (routable, not yet routed)"
    )


#: Every function that reaches a `NEST_BINARY_FIXTURES` name, with the
#: disposition that keeps it there. The general form of `_NODE_BINARY_CONSUMERS`
#: above, and the standing `nest_binary` verdict's own "needs per-test triage"
#: finally being done rather than re-counted.
#:
#: `nest_binary` is the last exclusion class the mode audit grades **MIXED** —
#: "closable ONLY where the binary is incidental; a FACT where the binary IS the
#: subject" — and MIXED is an honest verdict only while somebody is actually
#: splitting it. Same two dispositions as the sibling pin, same meanings:
#: **PREMISE** — the locally-compiled binary is what the test is a witness OF
#: (it kills the process mid-test, reads its `--db`/`--blob-dir` off the host,
#: inspects its files), so routing the fixture would assert nothing; and
#: **OPEN** — routable, simply not yet routed, which makes it a backlog item
#: with a name instead of a silent survivor.
#:
#: Grown **one cluster per commit**: a half-populated map that reads as complete
#: is worse than none, so `_NEST_BINARY_FILES_AWAITING_TRIAGE` names the files
#: still untriaged and the pin holds *both* halves exact. A file leaves that set
#: only by having every one of its consumers land here.
_NEST_BINARY_DISPOSITIONS = {
    # ---- tests/platform/sync/ — triaged 2026-09-02 -----------------------
    # The cluster the standing verdict expected to be "mostly PREMISE". It is
    # not: 12 of the 14 never look at the nest again after starting it. What
    # made it *look* like premise work is that every test spawns the binary by
    # hand — `start_node`, or an inline `subprocess.Popen` copy of it — which is
    # a statement about the HARNESS, not about what the test witnesses.
    # Its two PREMISE residents — the push-always chunk test and the
    # metadata-only relay test, both reading the nest's own blob dir — were
    # RETIRED 2026-09-30 with the `SIGNATURE_REQUIRED` flip: each was about
    # what the headless daemon RECORDS, and the nest refuses every record the
    # daemon's unsigned legacy data plane sends.

    # The rest of the package is ROUTED (2026-09-02) and so is gone from this
    # map: all eight remaining OPEN tests across five files, plus the four in
    # `test_orchestrator.py` routed earlier the same day, now take `sync_nest` —
    # ONE package-conftest fixture over `_start_dedicated_nest` for the whole
    # family, and since the two PREMISE residents retired it has no
    # `node_binary` consumer left.
    #
    # Four findings that the routing paid for, kept because the next family
    # routed will meet at least three of them:
    #
    # * **It is not port + spawn + claim code — the SCHEME changes too.** These
    #   files wrote `node_url = "http://127.0.0.1:{port}"`, and a docker nest is
    #   `https://` with a self-signed cert (there is no plain-HTTP docker
    #   posture — `_DockerProvider` says so where it builds the url). Pass the
    #   nest's `url`, never re-derive one from its port; the daemon dials it
    #   through the same trusted-dial path the apps use, so nothing else is owed.
    # * **Do not name the new fixture something the classifier already knows.**
    #   Several rules match by fixture NAME, so a collision silently hands your
    #   test a stranger's rule. The first attempt was called `relay_nest`, which
    #   is in `OWN_IMAGE_FIXTURES`; the four orchestrator tests stayed excluded
    #   from docker with a confident wrong reason (that they boot their own
    #   image) until it was renamed. Check new nest-shaped fixture names against
    #   `OWN_IMAGE_FIXTURES` and `NEST_BINARY_FIXTURES` first.
    # * **A nest-side log LEVEL is a start option, and a shared fixture cannot
    #   ask for one.** Two of these tests set a `fauna_nest` data-plane `RUST_LOG=…=debug`
    #   on the nest they spawned. Carrying that into the routed form means
    #   `extra_env` on the fixture — which live refuses outright — so eight
    #   tests would have been gated to buy a debug line for two. It was dropped
    #   instead; the daemons' own debug logs are untouched, and every mode
    #   publishes the nest's log at `nest["log_path"]`, so the tests' failure
    #   dumps stay mode-blind.
    # * **The hardcoded ports were not only a routing blocker — they were a live
    #   cross-checkout collision.** `test_config_pull.py` and
    #   `test_text_merge.py` both took 13013, and a dev machine grants two e2e
    #   slots at once, so two checkouts running this package concurrently bound
    #   the same ports (observed on the day this landed, with a sibling checkout
    #   running the whole package while these were being routed). A
    #   provider-started nest takes a free port per test, so the class is gone
    #   for the twelve; it survives in the package's self-spawning residents.
    # ---- tests/api/ — triaged 2026-09-02, which completes that tree ------
    # Its third consumer is in `_NODE_BINARY_CONSUMERS` above; with these two
    # no `tests/api/` test reaches a binary fixture without a disposition (the
    # bluesky-OAuth four that made it six ROUTED 2026-10-05, onto a named build
    # — `_PROVIDER_ROUTED_MODULE_LOCAL_NESTS`).
    # The `paired` pair. Routing is possible and would be a mistake: it moves
    # them from a MIXED class to a FACT one, which is a worse place to be.
    "tests/api/test_namespace_sync.py::test_sync_pull_empty":
        "PREMISE: the subject is two nests paired over a federation channel, "
        "and the pairing is expressed as the private nest dialling the public "
        "one at `http://127.0.0.1:<port>`. Ruling (2) says a container network "
        "cannot express that — `validate_peer_url` refuses RFC1918 as "
        "`NonGlobal` — so the routed form would be excluded by `private_peer`, "
        "a FACT, and would assert nothing it asserts today. The loopback "
        "topology, not the compilation, is what makes the binary load-bearing "
        "here; recorded as PREMISE because the operative half of that word is "
        "'routing would assert nothing'.",
    "tests/api/test_namespace_sync.py::test_sync_push_and_pull_roundtrip":
        "PREMISE: same two-nest loopback pairing as its file-mate above, and "
        "the same reason — routing it would trade a MIXED exclusion for a FACT "
        "one rather than for a container.",
    "tests/api/test_namespace_sync.py::test_sync_refuses_a_namespace_that_is_not_the_paired_actors":
        "PREMISE: same two-nest loopback pairing as its file-mates above (the "
        "`paired` fixture, not a routed dedicated nest), and the same reason — "
        "routing it would trade a MIXED exclusion for a FACT one rather than "
        "for a container.",
    "tests/api/test_namespace_sync.py::test_the_relay_holds_only_sealed_bytes_it_cannot_read":
        "PREMISE: same two-nest loopback pairing as its file-mates above (the "
        "`paired` fixture), and a second reason of its own — the witness is the "
        "relay's OWN DISK: it opens the relay's `namespace_entries` row over "
        "`db_path` and walks every file under the relay's data dir for the "
        "plaintext marker. A container's data dir is not the host's, so the "
        "routed form would read no disk at all and pass vacuously.",

    # The forward-queue pair — the same topology as the `paired` pair above,
    # driven through an app instead of the wire.
    "tests/test_forward_queue.py::"
    "test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it":
        "PREMISE: the subject is a private home nest forwarding to a paired "
        "public relay, and the pairing is the home nest dialling the relay at "
        "`http://127.0.0.1:<port>` (the user's home-side pairing row, on top of "
        "two boot seeds no per-nest start option carries). A container network cannot "
        "express that loopback pairing — `validate_peer_url` refuses RFC1918 as "
        "`NonGlobal` — so the routed form would be excluded by `private_peer`, "
        "a FACT, and assert nothing it asserts today; the binary is load-"
        "bearing for the same reason the `paired` pair's is.",
    "tests/test_forward_queue.py::"
    "test_stop_forwarding_drops_the_queue_and_keeps_the_post":
        "PREMISE: same two-nest loopback pairing as its file-mate above (the "
        "`home_behind_refusing_relay` fixture), and the same reason — routing "
        "it would trade a MIXED exclusion for a FACT one rather than for a "
        "container.",

    # ---- tests/platform/bridge/ and tests/platform/linux/ — 2026-09-02 ---
    # Four tests, one shape each, and both shapes are the verdict's own
    # examples of a FACT: poking the nest's db file, and installing its binary.
    "tests/platform/bridge/test_smtp_sort.py::test_smtp_two_emails_sort":
        "PREMISE: `set_inbox_open` opens the nest's `nest.db` with `sqlite3` "
        "and UPDATEs a row mid-test, which is the verdict's own 'reading its "
        "db_path' half and then some — a container's db is inside the "
        "container, and a routed form would need a product surface for what "
        "this test does with SQL.",
    "tests/platform/bridge/test_smtp_crossnest_sort.py::test_crossnest_sort":
        "PREMISE: the same `sqlite3` UPDATE against `nest.db`, twice over — it "
        "runs two nests and pokes both dbs. Its cross-origin subject would also "
        "put it in `private_peer` territory if routed, so the db access is the "
        "first of two reasons rather than the only one.",
    "tests/platform/linux/test_installer.py::TestNestInstaller::"
    "test_install_and_uninstall":
        "PREMISE: the test hands the nest INSTALLER a `--local-binary` and "
        "asserts what the install and uninstall did to the host. A locally "
        "built binary is the input to the thing under test, not scaffolding "
        "around it, and there is no container form of installing a nest onto a "
        "machine.",
    "tests/platform/linux/test_installer.py::TestNestInstaller::"
    "test_uninstall_preserves_data_by_default":
        "PREMISE: same installer, same `--local-binary`, and the assertion is "
        "about what survives on the host filesystem after an uninstall.",

    # ---- the last non-machine-specific files — triaged 2026-09-02 --------
    # After these, everything still awaiting triage is `tests/platform/windows/`
    # or `tests/platform/macos/`: gradeable from any machine because the scan is
    # machine-independent, runnable only on theirs.

    # `tests/test_tray_close_to_tray.py` used to hold five OPEN entries here and
    # holds none now: it is ROUTED (2026-09-02), the first of the OPEN set to go.
    # Its `tray_app` fixture took `nest_binary` and never referenced it — dead
    # boilerplate (2026-06-23), when `nest_instance` still
    # declared the name itself, that the 2026-08-02 lazy-resolution change
    # silently turned into a live exclusion signal. Deleting the parameter was
    # the whole routing for all five: measured over a `--nest docker --app linux`
    # collect, 1 of 6 before and 6 of 6 after, the five having been deselected as
    # "depends on nest_binary". They leave this map because they leave the
    # POPULATION — the set-equality assert above would name them GONE otherwise,
    # which is the bookkeeping working, not a nuisance.
    #
    # Why the parameter must not come back is recorded in the fixture's own
    # docstring, next to the ⚠ that keeps the two `tests/platform/windows/`
    # `harness` fixtures safe: those declare `nest_binary` without referencing it
    # ON PURPOSE, because `SyncTestHarness.start` calls `build_node()` where no
    # closure can see it. Same syntax, opposite meaning; only reading the fixture
    # tells them apart, so an "unused fixture argument" sweep over this tree
    # would be a regression.
    #
    # Asked and closed, so nobody re-runs the hunt: the tray shape does not
    # repeat. Exactly THREE fixtures in the whole suite declare a
    # `NEST_BINARY_FIXTURES` name and never mention it in their own body — the
    # two deliberate `tests/platform/windows/` `harness` fixtures above, and
    # `tests/platform/linux/test_installer.py::TestNestInstaller.
    # _cleanup_stale_nest`, which is `autouse=True` and therefore the one place
    # a dead parameter COULD pull a non-consumer into the class. It does not:
    # both tests in that class declare `nest_binary` themselves and both are
    # PREMISE above (the installer takes the local binary as its input), so
    # deleting it would move no classification at all. There is no third
    # tray_app waiting to be found.
    #
    # `tests/test_bridges.py`'s four OPEN entries left this map 2026-10-05:
    # `bridges_nest` routed onto a named build, its card addressed by what the
    # page renders rather than a fixed index (`BridgesActions._card_index`).

    # Two self-spawning onboarding nests. Both already use `find_free_port()`,
    # so they are missing less than the sync twelve were.
    # The onboarding pair. Their `--static-dir` premise was checked 2026-09-02,
    # as the row asked, and it does NOT hold: the path both fixtures pass is the
    # `static_dir` fixture — `just web-test`, the TEST-FLAVOURED SPA whose
    # `fauna-wasm-onboarding` carries the `test-helpers` feature. conftest's own
    # docstring draws the line ("Production builds use `just web` and don't ship
    # those setters"), and convention 15 compiles that surface out of release
    # artifacts by design. Measured against the artifact rather than argued:
    # inside `ghcr.io/faunasocial/nest:latest`, `grep -rl` over
    # `/usr/share/fauna-web` finds neither `setHandleCheckSnapshotForTest` nor
    # `setStepForTest`. Both tests read the machine over that bridge
    # (`call_machine_method` / `handle_check_snapshot()`), so routed they would
    # not run at all — and `test_onboarding_localhost.py`'s stated subject is
    # that the bundle "reflects current Rust", which an image built from another
    # commit cannot witness even in principle. Unlike the sync package's
    # cargo-culted `--static-dir`, this one IS the subject.
    "tests/test_onboarding_localhost.py::"
    "test_test_at_localhost_reaches_admin_claim":
        "PREMISE: the nest is the only thing that can serve THIS TREE's "
        "test-flavoured SPA at its own origin. The subject is the localhost "
        "handle-check driven through the wasm bundle in a browser — the module "
        "docstring's own claim is that the bundle reflects current Rust — and "
        "the image ships a production SPA, built from another commit, with the "
        "e2e setters compiled out (convention 15). A routed form would drive a "
        "different bundle through a bridge that is not there.",
    "tests/test_onboarding_handle_check_reset.py::"
    "test_handle_check_resets_on_identity_reimport":
        "PREMISE: same test-flavoured-SPA premise as its onboarding sibling, "
        "and it binds harder here — the reset is read through "
        "`call_machine_method`/`handle_check_snapshot()`, the cross-app bridge "
        "the production bundle does not expose, which the doc calls the "
        "reliable way to read it rather than a brittle message scrape.",


    # PREMISE, and ratified in the file's own docstring rather than inferred.
    "tests/test_version_skew_real_binary.py::"
    "test_previous_client_journey_against_head_nest":
        "PREMISE: the subject IS two locally-compiled nests at different "
        "commits — the pinned previous release rebuilt from source against "
        "HEAD. The file's own docstring settles the mode question against "
        "`version-compatibility.md` § Dim 6 § Tier choice: wire skew has no "
        "image dimension, so running the image would add only Docker packaging "
        "coverage the tier_4 upgrade gate already owns, at the cost of a "
        "self-hosted-runner dependency. Both sides are debug builds on purpose, "
        "so a release image would confound PROFILE skew with the VERSION skew "
        "under test.",
    "tests/test_version_skew_real_binary.py::"
    "test_the_two_nests_are_genuinely_different_builds":
        "PREMISE: it exists to assert that the two local builds really are "
        "different builds — the control that keeps its file-mate from passing "
        "vacuously. There is no form of that assertion that does not hold two "
        "binaries.",

    # ---- the machine-specific half — triaged 2026-09-02 -------------------
    # `tests/platform/windows/` (17) and `tests/platform/macos/` (2): the last
    # files in the awaiting set, and the ones this arm could always GRADE from
    # anywhere (the scan is machine-independent by design) but never RUN off
    # their own platform. So every one of the nineteen is a disposition and
    # none is a routing — the routing edit belongs to a session on the machine
    # that can watch it come up.
    #
    # All nineteen come out OPEN, which is the kind of result this arm has
    # learned to distrust: a triage that answers the same way every time has
    # stopped discriminating. It survives the check for the same reason the
    # MOOT half did — the two words are decided by something the tests
    # themselves say, not by how they look. PREMISE is "the locally-compiled
    # binary is what the test is a witness OF"; every host-side nest artefact
    # in these five files (`blob_dir`, `nest.db`, the `--db` path) is touched
    # only inside a harness function, and the one PREMISE shape that ends this
    # sweep the other way — `test_version_skew_real_binary.py` above — is in
    # the map already.

    # The windows sync trio (16, ROUTED
    # 2026-09-07) is ROUTED and leaves this map — the same bookkeeping move
    # the macos pair below made, for the same reason: they leave the
    # `nest_binary` POPULATION, so a hand-written entry here would drift from
    # a derived fact.
    #
    # `tests/platform/windows/conftest.py::win_sync_nest` (the windows twin of
    # `tests/platform/sync/conftest.py::sync_nest`) replaces every
    # self-spawned nest across `test_sync_edge_cases.py` (10, including
    # `TestFaunaignore`, itself classifier-invisible before this — no
    # `harness(nest_binary)` in its own signature), `test_sync_scenarios.py`
    # (5), and `test_sync_files.py` (1, likewise classifier-invisible
    # before). Their `--handle-domain test.fauna.social` was INERT —
    # established, not assumed: `tests/platform/sync/test_file_sync.py
    # ::test_file_sync_bidirectional` runs the identical `register_user` →
    # `create_folder` → `add_folder_member` admin sequence against a nest
    # asking for no start option at all (`sync_nest`), so `win_sync_nest`
    # asks for none either.
    #
    # **Verified on Windows (2026-09-07):** `--nest docker --collect-only`
    # selects all sixteen (was excluded pre-routing); the routed form runs
    # green under standalone, the only mode buildable on this machine —
    # `docker` itself is not installed here, and provisioning a new
    # toolchain onto a dev machine needs an explicit human go-ahead, so an
    # actual `--nest docker` RUN against a pulled
    # `ghcr.io/faunasocial/nest:latest` is unverified and owed to a
    # docker-capable windows machine (the macOS-pair precedent below).
    #
    # The two windows IPC/UI files that used to sit here are DELETED
    # (2026-09-08,). They were routed onto
    # `_start_dedicated_nest` on 2026-09-07, but the routing was
    # never reachable: both files also built a `fauna-service` binary that had
    # been removed from the workspace on 2026-03-27 (one day
    # after the tests were written), so every run died building that OTHER
    # binary before a nest of either mode was started. No successor binary
    # exists and none may: `docs/goal/architecture/apps/windows.md` puts MLS
    # in-process by P/Invoke and retires the MLS-delegate service model, and
    # `docs/goal/architecture/serialization.md` retires BARE from every IPC
    # path, so the background-service-owns-identity-over-a-BARE-pipe surface
    # those tests pinned is retired product design, not a broken dependency.
    # The cross-nest DM behavior they claimed is covered by
    # `tests/api/test_cross_nest_api.py` (federation keypackage/welcome) and
    # `tests/test_fauna_mls_cross_nest_roundtrip.py` (GUI proof on web and
    # linux); a windows-GUI cross-nest leg is a named Track-E gap, not
    # coverage this deletion removed.

    # The macos pair (2) is ROUTED (2026-09-02) and leaves this map — the same
    # bookkeeping move `tests/test_tray_close_to_tray.py` made above, for the
    # same reason: they leave the `nest_binary` POPULATION, so a hand-written
    # entry here would drift from a derived fact.
    #
    # This pair was the interesting one, because its `--handle-domain` WAS
    # load-bearing where the windows sync files' was not: `register_handled_actor`
    # mints the handle `macsync`/`macdel` in the domain the fixture seeds, and
    # the nest recomputes that domain from its own resolved handle domain to
    # check the registration signature, so a mismatch is a `signature_failed`
    # reject. That did not make it a permanent blank — `common.auth`'s own
    # docstring names three doors onto that domain and says any of them will
    # do, and one of them, `claim_domain`, is a WIRE act docker already
    # supports — so the new `tests/platform/macos/conftest.py::mac_sync_nest`
    # fixture asks for `claim_domain=`, not `handle_domain_seed=`, mirroring
    # `tests/platform/sync/test_sync_register.py::sync_register_nest`'s
    # already-green-in-docker shape exactly (both: claim onto a domain, open
    # registration over the wire after the claim, nothing else). The old
    # hand-written `--config` file that used to seed `registration_mode =
    # "open"` pre-claim, and the hand-written `<db_dir>/claim-code`, and the
    # module-level `PORT = 13300` / `PORT + 1`, are all gone with it.
    #
    # **The baseline run measured a real, pre-existing bug the routing fixes
    # for free**: the pair's old hand-written `--config` file only ever wrote
    # `[nest] mode / registration_mode`, and by 2026-09-02 the node config
    # schema requires a `listen` field the file never supplied — both tests'
    # nest failed to boot and timed out waiting on `/health`, unconditionally,
    # before this routing landed. `mac_sync_nest` uses the maintained
    # `config/default.toml` template (`common.nest.start_nest`'s default)
    # instead of a bespoke file, so this class of drift cannot recur here.
    #
    # **Verified on macOS (2026-09-02):** `--nest docker --collect-only`
    # selects both tests (was excluded pre-routing); the routed form runs
    # green under the default `standalone` mode, the only mode buildable on
    # this machine — `docker` itself is not installed here, and provisioning
    # a new toolchain onto a dev machine needs an explicit human go-ahead, so
    # an actual `--nest docker` RUN against a pulled
    # `ghcr.io/faunasocial/nest:latest` is unverified and owed to a
    # docker-capable machine.
}

#: The files whose `NEST_BINARY_FIXTURES` consumers have NOT been triaged yet.
#:
#: Recorded as FILES rather than a count, deliberately: every count this arm has
#: ever written down went stale within two passes, while a filename does not
#: drift and a new file taking a binary fixture reds here on its first commit.
#: The set can only shrink — a cluster's triage commit moves its files out by
#: giving every consumer a disposition above.
#:
#: **EMPTY as of 2026-09-02, which is the terminal state and not an idle one.**
#: Every `nest_binary` consumer in the tree now carries a verdict: derived MOOT
#: (a ratified FACT already holds it out of docker), or a hand-written PREMISE
#: or OPEN above. Empty makes the sibling pin STRICTER rather than vacuous —
#: "triaged" is defined as "needs a disposition and is not named here", so with
#: nothing exempted the next file to take a binary fixture is triaged from its
#: first commit and reds until somebody writes the sentence. Re-adding a name
#: is therefore a deliberate act with a cost, which is the property that keeps
#: a remainder from quietly regrowing.
_NEST_BINARY_FILES_AWAITING_TRIAGE = frozenset()


def _nest_binary_consumer_closures() -> dict:
    """`{qualified test -> its transitive fixture-name closure}`, for every test
    whose closure reaches a `NEST_BINARY_FIXTURES` name.

    Resolved through the CONFTEST CHAIN as well as the test module, because that
    is the only way the scan sees the same population pytest does: most of the
    class reaches its binary through a conftest fixture (`dedicated_mail_nest`,
    `unclaimed_caldav_nest`, the CalDAV variant family), and a module-local-only
    scan silently misses every one of them — measured, it saw 101 of the 120 and
    missed all 30 of the run's caldav/mail/dav gates.

    Returning the whole closure rather than just the binary's name is what lets
    the caller re-run the REAL classifier over it (`nest_surface.all_rules`)
    instead of re-deriving a second, divergent opinion about what excludes a
    test. Cross-checked once against the truth it stands in for: over the 64
    `nest_binary` gates of a `--nest docker` collect, feeding these closures to
    `all_rules` reproduced the run's own rule tuple for 59, and the five misses
    were all the same one — `permanent_option`, which is a property of the
    kwargs a fixture PASSES and so is invisible to a closure. All five already
    carried `bridge_spawn`, so on the only question this pin asks of the rules
    — is any of them a ratified FACT? — the agreement was 64 of 64.

    Deliberately a superset of any one run: it is machine- and mode-independent,
    so it grades every platform family's tests from whichever dev machine runs
    it, and keeps this pin's answer the same on all of them, which a collect
    never could.
    """
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]

    def fixtures_in(path: Path) -> dict:
        out = {}
        try:
            tree = ast.parse(path.read_text())
        except (SyntaxError, OSError):
            return out
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for dec in node.decorator_list:
                d = dec.func if isinstance(dec, ast.Call) else dec
                if getattr(d, "attr", None) == "fixture" or getattr(d, "id", None) == "fixture":
                    out[node.name] = [a.arg for a in node.args.args]
        return out

    conftests, found = {}, {}
    for source in sorted(root.glob("tests/**/*.py")):
        if source.name == "conftest.py":
            continue
        try:
            tree = ast.parse(source.read_text())
        except SyntaxError:
            continue

        # Nearest conftest last, so a closer definition shadows a farther one —
        # `tests/platform/conftest.py::node_binary` and its `tests/api/` twin are
        # different fixtures that happen to share a name.
        chain, d = [], source.parent
        while True:
            c = d / "conftest.py"
            if c.exists():
                chain.append(c)
            if d == root:
                break
            d = d.parent
        table = {}
        for c in reversed(chain):
            if c not in conftests:
                conftests[c] = fixtures_in(c)
            table.update(conftests[c])
        table.update(fixtures_in(source))

        rel = source.relative_to(root).as_posix()

        def visit(node, prefix):
            """Descend classes, keying by the FULL path.

            `ast.walk` plus a `file::name` key collapses same-named methods in
            different classes onto one entry, and the tree has one such case
            squarely inside this population: `test_installer.py` defines
            `test_install_and_uninstall` five times — once per packaging
            flavour (shell, deb, snap, flatpak, nest installer). Under the flat
            key four of the five were invisible, so four consumers could have
            been added or lost with nothing to red. The key is now pytest's own
            node id minus parametrisation, which is also what a reader can paste
            straight into `-k`.
            """
            for child in getattr(node, "body", []):
                if isinstance(child, ast.ClassDef):
                    visit(child, prefix + [child.name])
                    continue
                if not isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    continue
                if not child.name.startswith("test_"):
                    continue
                seen, stack = set(), [a.arg for a in child.args.args if a.arg != "self"]
                while stack:
                    cur = stack.pop()
                    if cur in seen:
                        continue
                    seen.add(cur)
                    stack.extend(table.get(cur, ()))
                if seen & ns.NEST_BINARY_FIXTURES:
                    found["::".join([rel, *prefix, child.name])] = frozenset(seen)

        visit(tree, [])
    return found


def _fact_rules() -> frozenset:
    """The rule ids whose ratified verdict is FACT.

    Read from `nest_surface.FACT_RULES`, which is deliberately NOT the same file
    as the grading table it mirrors. The first shape of this helper imported that
    table directly — the right instinct (one owner, no third copy) and the wrong
    reach: the tool holding it is kept out of the curated tree, because its
    input is a `--nest-gates-json` payload from our own harness and no checkout
    outside it has one. This file, by
    contrast, ships — so the import made the curated tree fail at import time,
    and the publish-hygiene gate caught it on the merge.

    `FACT_RULES` is therefore the same fact placed where a consumer in this tree
    can reach it, and the two are pinned equal by
    `tests/scripts/test_features_mode_audit.py`, which can import both. The copy
    is real; the drift is not possible.
    """
    from helpers import nest_surface as ns

    return ns.FACT_RULES


def _nest_binary_triage_split():
    """`(needs a disposition, MOOT, stale awaiting-names)`.

    Three things, and the middle one is the point. A test carrying `nest_binary`
    **and** a rule whose ratified verdict is FACT is not routing work at all:
    the FACT holds it out of docker no matter what happens to its binary, so
    routing it frees no cell and would only turn a clean collection-time
    exclusion into a runtime ❌ against product code behaving as ratified. That
    is not a hypothesis — it is the overstatement `BRIDGE_BINARY_FIXTURES` was
    added to stop, worded there as "routing them would have turned seven clean
    collection-time exclusions into seven runtime ❌", and the audit already
    reports its page-level twin ("appears to sole-block but does NOT").

    So MOOT is **derived, never recorded**. Writing it by hand would be a
    judgement where a computation exists, and it would rot the moment a class
    moved. Measured on this tree: of 120 consumers, 65 are MOOT — including
    every one of the CalDAV, atproto/bluesky, dav-at-rest and mail-roundtrip
    clusters that the commissioning row's own work-breakdown listed as routing
    work. Only 55 need a disposition at all.

    ONE literal draws the remaining boundary — `_NEST_BINARY_FILES_AWAITING_TRIAGE`
    — and everything else derives from it, which is what forces the halves to
    partition rather than merely be asserted to. Deriving the triaged half from
    the disposition map's own keys instead (the first shape of this pin) let a
    file leave that half by having its last disposition DELETED, so the mutation
    that removes an entry reported "a new untriaged file" rather than "this
    consumer lost its disposition" — a red either way, but one naming the wrong
    edit, and a green if someone made both halves of that edit at once.
    """
    from helpers import nest_surface as ns

    facts = _fact_rules()
    docker = nm.parse_nest_mode("docker")
    need, moot = {}, {}
    for qualified, closure in _nest_binary_consumer_closures().items():
        item = _FakeItem(fixturenames=closure, name=qualified.split("::")[-1])
        rules = frozenset(ns.all_rules(item, docker))
        (moot if rules & facts else need)[qualified] = rules

    files = {q.split("::")[0] for q in need}
    return need, moot, _NEST_BINARY_FILES_AWAITING_TRIAGE - files


def test_the_moot_half_is_derived_from_the_ratified_verdicts():
    """Nothing carrying a ratified FACT is counted as routing work.

    Two assertions, and the first is the load-bearing one because it guards the
    DERIVATION rather than its result. Every consumer's reconstructed closure
    must still make the classifier name a binary rule: if the conftest walk broke
    — a renamed file, a changed decorator shape — `all_rules` would return an
    empty tuple for everybody, no rule would intersect the FACT set, and the
    whole population would silently reappear as apparent work. That failure has
    exactly the shape this arm keeps meeting: a measurement that looks like a
    finding. So it is asserted directly instead of inferred from the numbers
    looking plausible.

    The second says a hand-written disposition never lands on a MOOT test. Both
    directions matter: a cluster that BECOMES moot (a new FACT class lands over
    it) leaves its dispositions behind as dead bookkeeping that still reads as a
    backlog, and a cluster that stops being moot (the venue seam lands and
    `bridge_spawn` no longer applies) must re-enter triage rather than coast on
    a stale entry. Neither is visible in a count.
    """
    need, moot, _ = _nest_binary_triage_split()
    from helpers import nest_surface as ns

    binary_rules = {ns.RULE_NEST_BINARY, ns.RULE_FEATURE_SET_BINARY,
                    ns.RULE_LOCAL_NEST_FIXTURE}
    blind = sorted(q for q, rules in {**need, **moot}.items()
                   if not (rules & binary_rules))
    assert not blind, (
        f"{blind[:5]} reach a NEST_BINARY_FIXTURES name by AST but the "
        "classifier names no binary rule for the reconstructed closure — the "
        "conftest walk is broken, and every MOOT test is about to reappear as "
        "routing work"
    )

    # `_NODE_BINARY_CONSUMERS` is deliberately excluded here: it is the older,
    # narrower `node_binary`-only registry, kept exhaustive by its own sibling
    # test (`test_every_surviving_node_binary_consumer_is_accounted_for`)
    # regardless of MOOT status — an entry there documents WHY the fixture is
    # used, not "still needs routing", so it staying moot is not dead
    # bookkeeping the way a `_NEST_BINARY_DISPOSITIONS` entry would be.
    dead = sorted(set(_NEST_BINARY_DISPOSITIONS) & set(moot))
    assert not dead, (
        f"{dead} carries a hand-written disposition but is already held out of "
        "docker by a ratified FACT, so routing it would free nothing. Drop the "
        "entry — MOOT is derived, and a recorded one is bookkeeping that reads "
        "as a backlog item."
    )


def test_every_triaged_nest_binary_consumer_carries_a_disposition():
    """The `nest_binary` MIXED class, split test by test instead of re-counted.

    The class's verdict has read "needs per-test triage" for a month while every
    pass reported a fresh total instead of doing it, and a total is precisely
    what cannot be acted on: it says how much is left, never which half is work.
    So this asserts the two things a total cannot — that every consumer in a
    triaged file carries PREMISE or OPEN, and that a triaged file holds no
    consumer nobody has looked at.

    **PREMISE** — the locally-compiled binary is what the test is a witness OF
    (it kills the process mid-test, reads its `--db`/`--blob-dir` off the host,
    inspects its files), so routing would assert nothing. **OPEN** — routable,
    simply not yet routed: a backlog item with a name instead of a silent
    survivor. Tests a ratified FACT already excludes never reach this map at
    all; that half is derived by the pin above.

    Exhaustive over triaged files rather than over the tree, because the row
    that commissioned it is explicit that a half-populated map reading as
    complete is worse than none. Nothing escapes through the seam: "triaged" is
    *defined* as "needs a disposition and is not in the awaiting set", so a
    brand-new consumer file is triaged-by-default and reds here on the commit
    that adds it — being unknown to both lists is not a way to be unmentioned.

    It deliberately does NOT check that a disposition is the RIGHT one — nothing
    here would notice PREMISE relabelled OPEN. That judgement is a reader's, and
    claiming otherwise would be the convenient lie this arm has now caught three
    times. What it enforces is that somebody wrote one down.
    """
    need, moot, _ = _nest_binary_triage_split()
    dispositioned = dict(_NEST_BINARY_DISPOSITIONS)
    # `_NODE_BINARY_CONSUMERS` entries that have gone MOOT are excluded the
    # same way `test_the_moot_half_is_derived_from_the_ratified_verdicts`
    # excludes them from its own dead-bookkeeping check: that dict's own
    # exhaustiveness is guarded by a sibling test regardless of MOOT status,
    # so a still-`need`ed consumer of the bare `node_binary` fixture keeps
    # counting here, but a moot one no longer needs a `need`-side match.
    dispositioned.update(
        (q, v) for q, v in _NODE_BINARY_CONSUMERS.items() if q not in moot
    )
    triaged_files = {q.split("::")[0] for q in need} - _NEST_BINARY_FILES_AWAITING_TRIAGE

    in_triaged = {q for q in need if q.split("::")[0] in triaged_files}
    assert in_triaged == set(dispositioned), (
        "the triaged consumer set moved:\n"
        + "".join(
            f"  NEW (record a disposition, or route it): {q}\n"
            for q in sorted(in_triaged - set(dispositioned))
        )
        + "".join(
            f"  GONE (drop it from the map): {q}\n"
            for q in sorted(set(dispositioned) - in_triaged)
        )
        + "\nA new consumer in an already-triaged file re-grows the class this "
        "pin exists to shrink. Route it through `_start_dedicated_nest` unless a "
        "locally-compiled binary is what the test is a witness OF."
    )

    unexplained = sorted(
        q for q, why in dispositioned.items()
        if not why.startswith(("PREMISE:", "OPEN:", "OPEN,"))
    )
    assert not unexplained, (
        f"a survivor with no disposition: {unexplained} — say PREMISE (the "
        "binary is the subject) or OPEN (routable, not yet routed)"
    )


def test_the_awaiting_triage_set_names_only_files_that_still_have_consumers():
    """The other half of the bookkeeping: the remainder can only SHRINK.

    A file is the unit because a filename does not drift and a count does — every
    count this arm recorded went stale within two passes, including the scoping
    note that commissioned class (9) and was wrong about its own membership in
    both directions. The sibling pin already forces every *new* or *untriaged*
    consumer to be named somewhere; the one thing it cannot see is a name in the
    awaiting set with no untriaged consumer left — a file renamed, its tests
    routed, or every one of them turned MOOT by a class landing over it. Left
    unchecked that is a remainder inflating itself, which is the same failure as
    a stale count wearing a filename.
    """
    _, _, stale = _nest_binary_triage_split()
    assert not stale, (
        f"{sorted(stale)} is listed as awaiting `nest_binary` triage but has no "
        "consumer needing one — the file was renamed, its tests were routed, or "
        "a ratified FACT now covers them all. Drop it: 'what is left' has to be "
        "readable as a count of real work."
    )


def test_the_routed_module_local_fixtures_are_admitted_to_docker():
    """The same outcome assertion as the conftest seven, for the module-local
    family — a docker run gets a container here instead of a silent deselect."""
    from helpers import nest_surface as ns

    docker = nm.parse_nest_mode("docker")
    for qualified in _PROVIDER_ROUTED_MODULE_LOCAL_NESTS:
        fixture = qualified.split("::", 1)[1]
        verdict = ns.classify(_FakeItem(fixturenames=[fixture, "app"]), docker)
        assert verdict is None, (
            f"{qualified} is routed through the mode provider, so nothing about "
            f"a locally-compiled binary may exclude it: {verdict}"
        )


def test_the_routed_module_local_fixtures_no_longer_spawn_their_own_nest():
    """Asserted at the SOURCE, like its conftest twin, and for the same reason:
    both exclusion signals are derived, so a fixture that still spelled either
    would simply come back on the next derivation.

    The extra assertion here is `start_nest`: a module-local fixture reaches the
    binary by calling it directly rather than through `_make_nest`, which is
    precisely why neither conftest-rooted AST pin could ever grade this family.
    """
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    for qualified in _PROVIDER_ROUTED_MODULE_LOCAL_NESTS:
        rel, name = qualified.split("::", 1)
        tree = ast.parse((root / rel).read_text())
        node = next(
            (n for n in ast.walk(tree)
             if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
             and n.name == name),
            None,
        )
        assert node is not None, f"{qualified} no longer exists — update the set"

        params = {a.arg for a in node.args.args}
        assert not params & {"nest_binary", "node_binary"}, (
            f"{qualified} still takes a binary fixture parameter, which puts it "
            "back in every non-standalone mode's exclusion closure — resolve it "
            "lazily through `_start_dedicated_nest` instead"
        )
        called = {
            (sub.func.id if isinstance(sub.func, ast.Name) else sub.func.attr)
            for sub in ast.walk(node) if isinstance(sub, ast.Call)
            and isinstance(sub.func, (ast.Name, ast.Attribute))
        }
        assert "start_nest" not in called, (
            f"{qualified} still calls `common.nest.start_nest` directly, so it "
            "spawns a local binary whatever the run's mode says — go through the "
            "provider seam"
        )
        assert "_start_dedicated_nest" in called, (
            f"{qualified} must start its nest through the mode provider seam"
        )


def test_the_prebuild_trigger_covers_every_provider_routed_nest_fixture():
    """Routing a fixture through the provider seam moves the nest binary from
    its DECLARED closure to a lazy `getfixturevalue`, so `_prebuild_binaries`
    must have some other way to know the run needs it — or the build falls back
    into the first requesting test's setup, inside `timeout = 900`. That bound
    inversion is the exact failure `_prebuild_binaries` exists to prevent, and
    its own docstring says so about `nest_instance`.

    **Measured 2026-08-30, and this pin is the residue.** `_LOCAL_NEST_FIXTURE_USERS`
    was the hand-maintained list `("nest_instance", "app", "persistent_app",
    "fresh_app")`. The eight module-local fixtures routed the day before were
    covered by accident — every one is an app journey, so `app` was in the
    closure — and the `tests/api/` family routed here is not: those are pure
    API tests with no driver, so nothing in the list matched, the prebuild never
    fired, and a real-path run of all seven files died `Failed: Timeout
    (>900.0s)` in the FIRST fixture's `build_node()`, with the machine's build
    queue six deep. Every later test then errored on the same memoized failure —
    40 errors that look like a product bug and are a harness bound inversion.

    The fix is to stop maintaining the list by hand: **`nest_mode` as a fixture
    PARAMETER is requested by exactly the nest-starting fixtures** —
    `nest_instance`, `_driver_cache` (which is how plain-`app` tests reach it),
    and every dedicated-nest fixture the seam routes, in conftest and in test
    modules alike. It cannot be otherwise: a fixture must take `nest_mode` to
    hand it to `_start_dedicated_nest`. So the trigger is structural, and a
    fixture routed in the future is covered by being routed rather than by
    someone remembering this list.

    Both halves are asserted, because either alone would pass vacuously.
    """
    import ast
    from pathlib import Path

    conftest_mod = pytest.importorskip("conftest")

    assert "nest_mode" in conftest_mod._LOCAL_NEST_FIXTURE_USERS, (
        "`nest_mode` must be a prebuild trigger: it is the one name every "
        "provider-routed dedicated-nest fixture necessarily requests, so "
        "without it a run selecting only such fixtures builds the nest inside "
        "the first test's 900 s budget instead of at collection"
    )

    root = Path(__file__).resolve().parents[1]
    sources = [root / "conftest.py"] + sorted((root / "tests").rglob("*.py"))
    offenders = []
    for source in sources:
        try:
            tree = ast.parse(source.read_text())
        except SyntaxError:  # pragma: no cover — another test's job
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if node.name == "_start_dedicated_nest":
                continue
            # FIXTURES only. A plain test may call the seam with a fake request
            # to assert something about it — the pins in this very file do —
            # and such a call starts no nest, so no prebuild owes it anything.
            if not any("fixture" in ast.unparse(d) for d in node.decorator_list):
                continue
            calls = {
                (sub.func.id if isinstance(sub.func, ast.Name) else sub.func.attr)
                for sub in ast.walk(node)
                if isinstance(sub, ast.Call)
                and isinstance(sub.func, (ast.Name, ast.Attribute))
            }
            if "_start_dedicated_nest" not in calls:
                continue
            params = {a.arg for a in node.args.args}
            if "nest_mode" not in params:
                offenders.append(
                    f"{source.relative_to(root).as_posix()}::{node.name}"
                )

    assert offenders == [], (
        "these fixtures reach the provider seam without requesting `nest_mode`, "
        "so they are invisible to the prebuild trigger and their nest build "
        "lands inside a per-test timeout:\n  " + "\n  ".join(offenders)
    )


def test_the_provider_seam_resolves_the_binary_only_where_it_is_real():
    """`_start_dedicated_nest` asks for `nest_binary` in standalone and NEVER in
    docker or live, where the fixture refuses outright — the property that lets
    the seven stop declaring it. Checked against a fake request that records
    what was asked for, so this needs no nest and no daemon."""
    conftest = pytest.importorskip("conftest")

    class _Recorder:
        def __init__(self):
            self.asked = []

        def getfixturevalue(self, name):
            self.asked.append(name)
            return "/nowhere/fauna-nest"

    for mode_name, expect_binary in (
        ("standalone", True),
        ("docker", False),
        ("live", False),
    ):
        mode = nm.parse_nest_mode(mode_name)
        rec = _Recorder()
        started = {}

        import helpers.nest_mode as nest_mode_mod

        real = nest_mode_mod.provider_for

        class _Provider:
            #: Carried from the REAL provider, never asserted here: whether a
            #: mode needs a locally-built binary is that provider's declaration
            #: (`builds_local_nest` defaults True, so a fake that omitted it
            #: would "prove" docker builds one).
            builds_local_nest = getattr(real(mode), "builds_local_nest", True)

            def start(self, nest_binary, tmp_path_factory, label, **options):
                started["binary"] = nest_binary
                return ({"url": "x"}, lambda: None)

        nest_mode_mod.provider_for = lambda _m: _Provider()
        try:
            conftest._start_dedicated_nest(rec, mode, None, "probe")
        finally:
            nest_mode_mod.provider_for = real

        assert ("nest_binary" in rec.asked) is expect_binary, (
            f"{mode_name}: the binary must be resolved "
            f"{'here' if expect_binary else 'nowhere'}, asked={rec.asked}"
        )
        assert (started["binary"] is not None) is expect_binary


def test_the_provider_seam_resolves_a_named_build_only_where_it_is_real():
    """`binary=` names the build standalone resolves, and only standalone: a
    container run serves the image instead, so the name must never reach a
    `getfixturevalue` there (`bluesky_nest_binary` would compile a nest the run
    discards, inside a test's clock). And the image stands in for a named build
    only when it ships that build's subject: a class (9) name is refused at the
    seam, never silently replaced by an artifact the test cannot depend on
    (`testing.md` § Default app and nest mode, ruling (1))."""
    conftest = pytest.importorskip("conftest")
    import helpers.nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    class _Recorder:
        def __init__(self):
            self.asked = []

        def getfixturevalue(self, name):
            self.asked.append(name)
            return f"/nowhere/{name}"

    real = nest_mode_mod.provider_for

    def _start(mode_name, binary):
        mode = nm.parse_nest_mode(mode_name)
        rec = _Recorder()
        started = {}

        class _Provider:
            builds_local_nest = getattr(real(mode), "builds_local_nest", True)

            def start(self, nest_binary, tmp_path_factory, label, **options):
                started["binary"] = nest_binary
                started["options"] = options
                return ({"url": "x"}, lambda: None)

        nest_mode_mod.provider_for = lambda _m: _Provider()
        try:
            conftest._start_dedicated_nest(
                rec, mode, None, "probe", binary=binary, claim_domain="x.test")
        finally:
            nest_mode_mod.provider_for = real
        return rec.asked, started

    asked, started = _start("standalone", "bluesky_nest_binary")
    assert asked == ["bluesky_nest_binary"], asked
    assert started["binary"] == "/nowhere/bluesky_nest_binary"
    assert started["options"] == {"claim_domain": "x.test"}, (
        "`binary` chooses the artifact; it is not a start option and must "
        f"never reach the provider: {started['options']}"
    )

    for mode_name in ("docker", "live"):
        asked, started = _start(mode_name, "bluesky_nest_binary")
        assert asked == [] and started["binary"] is None, (mode_name, asked)

        refused = sorted(ns.FEATURE_SET_BINARY_FIXTURES)[0]
        with pytest.raises(nm.NestModeError, match="IMAGE_SERVABLE"):
            _start(mode_name, refused)


def test_every_named_build_is_declared_and_image_servable():
    """`DEDICATED_NEST_BINARIES` is pinned to the literal `binary=` each
    fixture hands `_start_dedicated_nest`, tree-wide — the same
    hand-table-plus-AST-equality shape as `FIXTURE_START_OPTIONS`.

    The table is not bookkeeping. A named build is invisible to
    `item.fixturenames` by design (that is what keeps it out of docker's
    exclusion closure), so the table is the only place the collection-time
    prebuild can learn it: a fixture missing here compiles its nest inside
    its first test's timeout. And every name must be one the image can stand
    in for, or a container run would refuse it at setup — a ❌ against working
    product code where a collection-time verdict belonged."""
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    root = Path(__file__).resolve().parents[1]
    found = {}
    for path, source in _walkable_sources(root).items():
        try:
            tree = ast.parse(source)
        except SyntaxError:
            continue
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for call in ast.walk(fn):
                if not (isinstance(call, ast.Call)
                        and isinstance(call.func, ast.Name)
                        and call.func.id == "_start_dedicated_nest"):
                    continue
                for kw in call.keywords:
                    if kw.arg != "binary":
                        continue
                    where = f"{path.relative_to(root)}::{fn.name}"
                    assert isinstance(kw.value, ast.Constant) \
                            and isinstance(kw.value.value, str), (
                        f"{where} passes a computed `binary=`; the table and "
                        "the prebuild can only see a literal"
                    )
                    found[fn.name] = kw.value.value

    assert found == ns.DEDICATED_NEST_BINARIES, (
        f"named builds in the tree {found} != DEDICATED_NEST_BINARIES "
        f"{ns.DEDICATED_NEST_BINARIES}"
    )
    for fixture, build in ns.DEDICATED_NEST_BINARIES.items():
        assert build in ns.IMAGE_SERVABLE_BINARY_FIXTURES, (
            f"{fixture} names {build!r}, which the image cannot stand in for"
        )
        assert build not in ns.FEATURE_SET_BINARY_FIXTURES, (fixture, build)
        assert build in ns.NEST_BINARY_FIXTURES, (fixture, build)
        assert fixture in ns.FIXTURE_START_OPTIONS, (
            f"{fixture} starts a nest but has no FIXTURE_START_OPTIONS row"
        )


def test_config_overrides_is_not_a_per_nest_start_option_in_any_mode():
    """Arm 4's smallest piece: the config-file rewrite retires from the per-nest
    option vocabulary altogether (`testing.md` § Default app and nest mode,
    ruling (3): "`config_overrides` has no caller and retires rather than
    ports").

    It is pinned rather than merely deleted because it is the one option whose
    mechanism the invariant BANS outright: a dict of string replacements applied
    to a rewritten `nest.toml` is configuration-file theatre by construction
    (`principles.md` § One configuration surface), so a future session reaching
    for a per-nest knob must not find this as the shortest path. Every other
    retired option in this arm is a product choice moving to its own wire kind;
    this one has nowhere to move to, and that is the point.

    It pins `_make_nest`, the per-nest option seam, the three providers'
    declarations, and `common.nest.start_nest` beneath them. That last
    parameter outlived the others while its one caller seeded a private nest's
    pull target into the rewritten file; the pull target is now the user's own
    pairing row (`private-mode.md` § Implementation status today, ruled
    2026-10-01), so the rewrite had nothing left to carry and retired too.
    """
    import inspect

    import conftest
    from common import nest as common_nest
    from helpers import nest_mode as nest_mode_mod

    for seam in (conftest._make_nest, common_nest.start_nest):
        assert "config_overrides" not in inspect.signature(seam).parameters, (
            f"`config_overrides` must not be a `{seam.__name__}` parameter: a "
            "per-nest config-file rewrite is the theatre ruling (3) retires, and "
            "a live parameter is an invitation to reach for it"
        )

    for mode_name in ("standalone", "docker", "live"):
        mode = nest_mode_mod.parse_nest_mode(mode_name)
        assert "config_overrides" not in nest_mode_mod.supported_options(mode), (
            f"nest mode {mode_name!r} must not declare `config_overrides` as an "
            "option it honours"
        )


def test_registration_posture_is_never_a_start_option_nor_a_config_seed():
    """Arm 4, product-choice half: the registration posture is an ADMIN CHOICE,
    so the harness expresses it the way an admin's app does — one
    `fauna.admin.set_registration_mode` call after the claim — and nowhere else
    (`testing.md` § Default app and nest mode, ruling (3): "*a product choice a
    user or admin makes in the app is not a start option at all*: it is applied
    **after** boot over the wire, mode-agnostically").

    Three doors are pinned shut, because the posture had reached for all three
    over its life and each is a different flavour of the same mistake:

    * a **CLI flag** — already gone, and the nest pins its own side
      (`main.rs::registration_posture_has_no_cli_flags`);
    * a **config-file seed** — `[nest] registration_mode`, written into the
      rewritten `nest.toml` by `load_and_rewrite_config`. This is the one this
      arm removes, and it is textbook configuration-file theatre
      (`principles.md` § One configuration surface): a file knob standing in for
      a choice the product already exposes on the wire;
    * a **per-nest start option** — the parameter this arm retires from
      `_make_nest`, `common.nest.start_nest` and every provider declaration.

    The last one is what shrinks class (4): while the posture was a start
    option, every fixture wanting an open nest was a collection-time declared
    absence in docker, for a knob the image was always able to honour — it just
    had to be asked over the wire rather than at boot.
    """
    import inspect

    import conftest
    from common import nest as nest_mod
    from helpers import nest_mode as nest_mode_mod

    assert "registration_open" not in inspect.signature(conftest._make_nest).parameters, (
        "the registration posture must not be a `_make_nest` start option — it "
        "is `fauna.admin.set_registration_mode` after the claim"
    )
    assert "registration_open" not in inspect.signature(nest_mod.start_nest).parameters, (
        "the registration posture must not be a `start_nest` start option either: "
        "the module-local self-spawners call it directly, so a parameter here is "
        "the same door standing open one level down"
    )
    assert "registration_mode" not in inspect.signature(
        nest_mod.load_and_rewrite_config
    ).parameters, (
        "no `[nest] registration_mode` config SEED: a file knob standing in for "
        "an admin choice the product exposes on the wire is configuration-file "
        "theatre (principles.md § One configuration surface)"
    )

    for mode_name in ("standalone", "docker", "live"):
        mode = nest_mode_mod.parse_nest_mode(mode_name)
        assert "registration_open" not in nest_mode_mod.supported_options(mode), (
            f"nest mode {mode_name!r} must not declare `registration_open`"
        )

    from helpers import nest_surface
    offenders = sorted(
        name for name, opts in nest_surface.FIXTURE_START_OPTIONS.items()
        if "registration_open" in opts
    )
    assert offenders == [], (
        f"{offenders} still ask a nest for `registration_open` at start; the "
        "posture moved post-boot, so the table must record that it moved"
    )


def test_the_registration_posture_helper_speaks_the_admin_kind():
    """The post-boot half, pinned to the WIRE KIND rather than to the helper's
    existence — an empty helper would satisfy the negative pin above while
    leaving the posture unset, and every test that wants an open nest would then
    fail far away, in `fauna.account.register`, with a refusal that looks like a
    product bug.

    Pinned by source inspection rather than by a live call because this is a
    tier_1 file: the round trip itself is exercised by every fixture that opens
    registration, which is the coverage that matters.
    """
    import inspect

    from common import auth as auth_mod

    src = inspect.getsource(auth_mod.set_registration_mode)
    assert "fauna.admin.set_registration_mode" in src, (
        "the harness must open registration the way an admin's app does — over "
        "`fauna.admin.set_registration_mode`, not by seeding a config file"
    )


# ---------------------------------------------------------------------------
# N nests per run, arm 4 (second half): `handle_domain` splits into two names,
# and the split is by WHICH MECHANISM SETS THE DOMAIN
# (`testing.md` § Default app and nest mode, ruling (3)).
# ---------------------------------------------------------------------------


class _StubProc:
    """Stands in for the spawned nest process in the headless `start_nest` pins.

    These pins are about which ARGUMENT reaches which mechanism — argv or the
    claim — so the process itself is scenery. Nothing here starts a nest.
    """

    pid = 4242

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def poll(self):
        return None


def test_claim_domain_rides_the_claim_in_standalone(monkeypatch, tmp_path):
    """`claim_domain` is a WIRE ACT, and this is the pin that says so: the value
    reaches `claim_admin` as its `mail_domain`, and it reaches the nest's argv as
    nothing at all.

    That is the whole reason the option is honourable in a container. The old
    `handle_domain` was a `--handle-domain` boot flag, and a boot flag is exactly
    what a provider holding a released image cannot add.
    """
    import common.nest as cn

    argv_seen, claims = [], []

    def _fake_build(node_binary, port, config_path, blob_dir, **kw):
        argv_seen.append(kw)
        return ["nest"]

    monkeypatch.setattr(cn, "_build_nest_cli_args", _fake_build)
    monkeypatch.setattr(cn, "_spawn_and_wait", lambda *a, **kw: _StubProc())
    monkeypatch.setattr(
        cn, "load_and_rewrite_config", lambda *a, **kw: "/tmp/cfg.toml"
    )
    import common.auth as ca
    monkeypatch.setattr(
        ca, "claim_admin",
        lambda port, code, **kw: claims.append(kw) or {"signing_key": None},
    )

    # `tmp_path`, never a machine-global path: `/work/tmp` is the Linux dev
    # box's large-temp dataset, and on Windows Python resolves that string
    # against the CURRENT DRIVE — so this line used to CREATE `D:\work\tmp`
    # there, whose mere existence then flipped justfile's `tmp_root` probe and
    # `build-slot.py`'s `_slot_dir()` onto a path only that box has (see the
    # commit that added this comment). Nothing here needs a shared directory:
    # every spawn in this test is monkeypatched.
    tmp = tmp_path / "claim-domain-pin"
    tmp.mkdir(parents=True, exist_ok=True)
    nest = cn.start_nest("/tmp/fauna-nest", str(tmp), port=45999,
                         claim_domain="fauna.test")

    assert claims and claims[0].get("mail_domain") == "fauna.test", (
        "the domain must ride the claim — that is what makes it mode-agnostic"
    )
    assert argv_seen[0].get("handle_domain") is None, (
        "a claimed domain must NOT also become a --handle-domain boot flag: two "
        "doors onto one domain is the thing ruling (3) forbids"
    )
    assert nest["claim_domain"] == "fauna.test", (
        "carried as a re-spawn input — a factory reset wipes the DB, and the "
        "primary mail_domains row that IS the identity lives there"
    )


def test_the_seed_reaches_argv_and_never_the_claim(monkeypatch, tmp_path):
    """The mirror image, and the reason the two names are not one name with a
    flag: `handle_domain_seed` is the `--handle-domain` boot argument and touches
    the claim not at all."""
    import common.nest as cn

    argv_seen, claims = [], []
    monkeypatch.setattr(
        cn, "_build_nest_cli_args",
        lambda nb, port, cp, bd, **kw: argv_seen.append(kw) or ["nest"],
    )
    monkeypatch.setattr(cn, "_spawn_and_wait", lambda *a, **kw: _StubProc())
    monkeypatch.setattr(
        cn, "load_and_rewrite_config", lambda *a, **kw: "/tmp/cfg.toml"
    )
    import common.auth as ca
    monkeypatch.setattr(
        ca, "claim_admin",
        lambda port, code, **kw: claims.append(kw) or {"signing_key": None},
    )

    tmp = tmp_path / "seed-pin"  # never a machine-global path — see the sibling above
    tmp.mkdir(parents=True, exist_ok=True)
    cn.start_nest("/tmp/fauna-nest", str(tmp), port=45998,
                  handle_domain_seed="127.0.0.1:45998")

    assert argv_seen[0].get("handle_domain") == "127.0.0.1:45998"
    assert claims[0].get("mail_domain") is None, (
        "the seed is a boot fact; carrying it into the claim would register a "
        "local target the claim gate refuses anyway"
    )


def test_a_claim_domain_on_an_unclaimed_nest_is_refused(tmp_path):
    """The contradiction the split exists to make un-representable.

    `unclaimed=True` is the harness declining to claim, because the CLIENT claims
    through the app — that is the whole point of those fixtures. So there is no
    claim for a domain to ride, and asking for one is a bug in the caller, not a
    thing to silently drop. This is why four perfectly registerable `fauna.test`
    fixtures are seed sites: not because of the value, because of the claim.
    """
    import common.nest as cn

    with pytest.raises(ValueError) as exc:
        cn.start_nest("/tmp/fauna-nest", str(tmp_path), port=45997,
                      unclaimed=True, claim_domain="fauna.test")
    assert "handle_domain_seed" in str(exc.value), (
        "the refusal must name the option that DOES work here"
    )


def test_docker_carries_the_claim_domain_into_its_own_claim(
    monkeypatch, tmp_path_factory
):
    """Ruling (3)'s promise for this option, measured in the mode that could not
    honour its predecessor: the container provider makes a claim of its own, and
    the domain rides that one exactly as it rides standalone's."""
    import conftest
    import tests.platform.docker.helpers as dh

    seen = {}
    _install_fake_docker(monkeypatch, iter((41060,)), lambda *a, **k: "cid")
    # Patched on the DEFINING module, not on conftest: the provider imports the
    # helper inside `start`, so a name bound on conftest is never consulted.
    monkeypatch.setattr(
        dh, "claim_admin_api",
        lambda port, code, **kw: seen.update(kw) or {"signing_key": None},
    )
    conftest._DockerProvider().start(
        None, tmp_path_factory, "nest", claim_domain="fauna.test",
    )
    assert seen.get("mail_domain") == "fauna.test", (
        "the container provider must carry the domain into the claim it already "
        "makes — that is the whole reason this option is honourable here"
    )


def test_no_fixture_asks_for_both_doors_onto_one_domain():
    """The two-door rule, pinned where it can be checked rather than remembered.

    A nest whose fixture registers its own domain must not ALSO have it
    registered at claim: `add_local_domain` is idempotent by domain NAME and
    answers the loser with `skipped: true`, silently discarding its
    `mta_sts_cert_mode` / catch-all / DKIM arguments. The
    suite's mail callers ask for `per_host` where the claim hard-codes
    `expand_primary`, so the discard would be a real behaviour change wearing a
    redundancy's clothes.

    Asking for both NAMES is the shallower half of the same mistake and is what
    this can see statically; the deeper half — a `claim_domain` beside the
    fixture's own `add_local_domain` — is why seven fixtures pass no domain
    option at all.
    """
    from helpers import nest_surface as ns

    both = {
        fixture: opts
        for fixture, opts in ns.FIXTURE_START_OPTIONS.items()
        if {"claim_domain", "handle_domain_seed"} <= opts
    }
    assert both == {}, (
        f"{sorted(both)} ask for a claimed domain AND a boot seed; a nest has "
        "one identity and one door onto it"
    )


def test_the_reclaim_after_a_factory_reset_carries_the_claimed_domain():
    """Two re-spawn inputs, two different reasons, and the second is the
    correctness bug this pin exists to keep fixed.

    The seed is an argv fact, so a re-spawn just passes it again. The claimed
    domain is DB state — the primary `mail_domains` row — and a factory reset
    wipes exactly that. A re-claim that forgot the domain would bring the nest
    back on the `localhost` identity fallback while every handle the test had
    already signed said otherwise: a silent wrong answer rather than a failure.

    Pinned by AST rather than by a run. `factory_reset_and_restart` drives a real
    admin WS-RPC round trip and cannot be exercised headlessly, and the fact
    worth pinning is the wiring, not the round trip.
    """
    import ast

    import common.nest as cn

    tree = ast.parse(pathlib.Path(cn.__file__).read_text())
    fn = next(
        n for n in ast.walk(tree)
        if isinstance(n, ast.FunctionDef)
        and n.name == "factory_reset_and_restart"
    )
    carried = {
        kw.arg: ast.unparse(kw.value)
        for c in ast.walk(fn)
        if isinstance(c, ast.Call) and getattr(c.func, "id", None) == "claim_admin"
        for kw in c.keywords
    }
    assert carried, "the re-claim call moved — re-anchor this pin"
    assert "mail_domain" in carried, (
        "the re-claim must carry a mail_domain, else the reset drops the "
        "deployment identity the claim had registered"
    )
    assert "claim_domain" in carried["mail_domain"], (
        "and it must come from the nest's recorded claim_domain re-spawn input, "
        f"not from somewhere else: {carried['mail_domain']!r}"
    )


# ---------------------------------------------------------------------------
# The TLS channel binding is a MODE fact, not a constant (`testing.md`
# § Default app and nest mode, ruling (1) — every nest the harness starts is
# the provider's, and the provider decides whether it serves a certificate).
# ---------------------------------------------------------------------------

#: The one home for `sha256(SubjectPublicKeyInfo)` in the harness, mirroring
#: `libs/fauna-protocol/src/tls_spki.rs` on the product side.
_SPKI_FINGERPRINT_HOME = "helpers/tls_spki.py"


def _files_that_fingerprint_an_spki() -> set:
    """Every harness file that reads a served leaf or fingerprints an SPKI.

    Keyed on the *call*, never on the name alone: `SubjectPublicKeyInfo` also
    spells the DKIM `p=` value and an ActivityPub actor's public key, and
    neither is a fingerprint — only hashing one is. File-level co-occurrence
    would sweep both of those in.

    ⚠ It resolves one hop of local dataflow, and must. Every real instance of
    this duplication has been written in two statements — `spki = key.
    public_bytes(..., SubjectPublicKeyInfo)` then `sha256(spki)` — so a rule
    reading only the call's own arguments matches the *inline* form and nothing
    else. Written that way first, this pin reported an empty set against a tree
    that had three copies plus the one home, which is the failure mode a pin
    over a rare shape is most prone to: a green that means "found nothing"
    rather than "found nothing wrong". Hence the anchor assertion below.
    """
    import ast
    from pathlib import Path

    def _names_spki(node) -> bool:
        return any(
            isinstance(sub, ast.Attribute) and sub.attr == "SubjectPublicKeyInfo"
            for sub in ast.walk(node)
        )

    root = Path(__file__).resolve().parents[1]
    found = set()
    for path in root.rglob("*.py"):
        try:
            tree = ast.parse(path.read_text())
        except SyntaxError:  # pragma: no cover — a syntax error is its own red
            continue
        # One hop: every name bound anywhere in the file to an expression that
        # names the SPKI format. Deliberately file-wide rather than per-scope —
        # over-reaching here can only ever ask a caller to import the one home,
        # while under-reaching lets a copy through unseen.
        spki_names = {
            target.id
            for node in ast.walk(tree)
            if isinstance(node, ast.Assign) and _names_spki(node.value)
            for target in node.targets
            if isinstance(target, ast.Name)
        }
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            # The read half: pulling the peer's leaf off a handshake. Counted
            # with the hash half because it is the same duplication wearing a
            # different face — and it is the half that hides better. The
            # fingerprint copies were at least *called* something recognisable;
            # a fifth `_served_leaf` sat in `test_nest.py` under a docstring
            # saying "same read as" the copy it was copied from, and two more
            # modules reached across and imported another test module's private
            # `_served_leaf`, which no census of the hash alone can see.
            if ast.unparse(node.func).endswith(".getpeercert"):
                found.add(path.relative_to(root).as_posix())
                continue
            if ast.unparse(node.func) not in ("hashlib.sha256", "sha256"):
                continue
            for arg in node.args:
                if _names_spki(arg) or (
                    isinstance(arg, ast.Name) and arg.id in spki_names
                ):
                    found.add(path.relative_to(root).as_posix())
    return found


def test_the_served_cert_fingerprint_has_exactly_one_home():
    """Priority #4: the channel-binding fingerprint is computed in one place.

    The number matters because two implementations of it — the nest's, from
    `tbs_certificate.subject_pki.raw`, and the harness's, from a re-encode of
    the parsed key — have to agree for any Fauna TLS binding to work. That
    agreement is a real cross-implementation pin wherever it is asserted, and it
    stops being one the moment the harness half is re-spelled per test file:
    each copy is then its own chance to drift, and a copy that drifts fails as
    a mysterious product red rather than as a diff.

    It had been re-spelled five times before `helpers/tls_spki.py` existed —
    two docker cert tests fingerprinting, a third re-reading the leaf under a
    docstring naming the copy it came from, two more modules importing another
    *test module's* private `_served_leaf`, and the federation client's
    hard-coded empty string — and a sixth was about to be written. The count
    kept growing while it was being counted, which is the argument for a pin
    rather than a sweep.
    """
    actual = _files_that_fingerprint_an_spki()
    assert _SPKI_FINGERPRINT_HOME in actual, (
        f"{_SPKI_FINGERPRINT_HOME} no longer computes the fingerprint — this "
        "pin has lost its anchor; re-point it at wherever the one home moved"
    )
    extra = actual - {_SPKI_FINGERPRINT_HOME}
    assert not extra, (
        f"these re-spell the SPKI fingerprint instead of importing it from "
        f"{_SPKI_FINGERPRINT_HOME}: {sorted(extra)}. Use `spki_sha256_hex` / "
        "`served_leaf_spki_sha256_hex` / `observed_spki_sha256_hex` there — a "
        "second spelling of this number is a second chance for the harness to "
        "disagree with the nest about it"
    )


def test_the_federation_client_observes_its_channel_binding_never_assumes_it():
    """The hello's `spki_sha256` comes off the socket, in both places it rides.

    A constant here is a standalone-only assumption wearing the clothes of a
    protocol value: the empty string is what a plain-HTTP loopback nest serves,
    and it is the only posture tier_3's nests have ever had. Every *deployment
    artifact* serves TLS, so the first federation test admitted to docker mode
    under a constant would have been refused at `verify_hello_and_build_reply`'s
    first check — a ❌ against product code that was working correctly, which is
    the failure class this whole axis exists to prevent.

    Pinned at both sites deliberately. The value rides the wire once and the
    signed tuple once, and the listener rebuilds the tuple from the wire copy —
    so two *different* expressions would not fail here, they would fail as a bad
    signature, one layer away from the cause.
    """
    import ast
    from pathlib import Path

    src = Path(__file__).resolve().parents[1] / "clients" / "ws_rpc_federation_client.py"
    tree = ast.parse(src.read_text())

    spellings = [
        ast.unparse(value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Dict)
        for key, value in zip(node.keys, node.values)
        if isinstance(key, ast.Constant) and key.value == "spki_sha256"
    ]
    assert len(spellings) == 2, (
        "expected the channel binding at exactly two sites — the signed "
        f"`FederationHelloSig` tuple and the hello payload — found {spellings}"
    )
    assert set(spellings) == {"self.observed_spki_sha256"}, (
        "the federation client must sign and send the binding it observed off "
        "the open connection, not a constant or a second read: "
        f"{spellings}. See `observed_spki_sha256_hex`'s docstring for why a "
        "value read from any other connection binds nothing."
    )


# ── Class (3)'s app door: the admin shell's element IDs ───────────────────
#
# The two kind scans see a mutation Python SENDS; a journey test mutates
# through the admin shell (convention 8), so the kind leaves the tui/app
# process and no Python string names it. Measured 2026-10-04 on a tui sweep
# against the staging box: the OAuth issuer key and session secret rotated,
# the web apex actor designated, a spam penalty set to 1000, NAT mode and the
# DAV toggles flipped — every one by a test both kind scans read as eligible.
# The door is the same shape the kinds gave: a closed vocabulary the spec
# registers (testing.md § Default app and nest mode → *Live mode* (d)).


def test_the_admin_shell_vocabulary_is_read_from_the_spec():
    """`admin_shell_element_ids` is derived from the UI spec's `pages.admin-*`
    entries and never hand-listed, for the reason `ui_walk.canonical_pages`
    gives: a hand-written list drifts the day someone adds a page, and drifts
    downward. The pin checks the derivation, not a copy: known members in; an
    ID the spec lists on a non-admin page too out (`error-message` is on every
    page, and says nothing about where the app is); the main shell's
    `admin-tab` — the way INTO the shell, on no admin page — out."""
    from helpers import nest_surface as ns

    ids = ns.admin_shell_element_ids()
    assert {
        "admin-dashboard-heading",
        "admin-nest-oauth-section",
        "admin-mail-spam-save-button",
        "admin-stat-card-value",  # a component member, through `components:`
        "admin-factory-reset-button",
    } <= ids, sorted(ids)[:20]
    assert not {"error-message", "page-heading", "admin-tab", "feed-tab"} & ids


def test_a_ui_driven_admin_journey_is_excluded_from_live():
    """The third door, on the journey that opened it: this test rotates the
    OAuth issuer key and the session secret through the admin Nest page. The
    tui process sends the kind, so both kind scans read the test as eligible,
    and on 2026-10-04 it rotated the staging box's keys. Its body names
    `admin-nest-oauth-section`, and that is where the scan catches it — on
    live only; a throwaway nest pays nothing for the rotation."""
    from helpers import nest_surface as ns

    item = _FakeItem(
        name="test_the_admin_walks_the_three_sign_in_key_controls_through_the_app",
        fixturenames=["app", "nest_instance"],
    )
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    assert ns.classify(item, nm.parse_nest_mode("docker")) is None
    verdict = ns.classify(item, nm.parse_nest_mode("live"))
    assert verdict is not None, "a UI-driven admin mutation must exclude on live"
    assert verdict.rule == ns.RULE_GLOBAL_ADMIN
    assert "admin shell" in verdict.reason
    assert "admin-nest-oauth" in verdict.reason
    assert "shared-box" in verdict.reason


def test_an_admin_shell_fixture_is_caught_through_its_own_reach():
    """There is no `ADMIN_SHELL_FIXTURES` list to drift: `admin_app` (through
    `_login_admin_as`) waits for `admin-dashboard-heading`, so every fixture
    that lands the app in the shell names an admin element itself. On
    2026-10-04 `crowded_admin_app` designated the staging box's web apex actor
    through exactly this door."""
    from helpers import nest_surface as ns

    for door in ("admin_app", "crowded_admin_app"):
        item = _FakeItem(fixturenames=[door, "app", "nest_instance"])
        verdict = ns.classify(item, nm.parse_nest_mode("live"))
        assert verdict is not None, door
        assert verdict.rule == ns.RULE_GLOBAL_ADMIN
        assert door in verdict.reason
        assert "admin-dashboard-heading" in verdict.reason
        assert ns.classify(item, nm.parse_nest_mode("docker")) is None, door


def test_the_admin_journeys_the_staging_sweep_ran_are_all_excluded_from_live():
    """The modules the 2026-10-04 staging sweep ran as eligible, each read off
    its own source rather than remembered: every test function in them is
    class (3) on live now. This is the row's success definition, held
    in-process — a `--nest live --collect-only` run lists none of them."""
    import ast
    from pathlib import Path

    from helpers import nest_surface as ns

    modules = (
        "test_admin_oauth_issuer_keys.py",
        "test_admin_picker_all_accounts.py",
        "test_admin_mail_unlisted_penalty.py",
        "test_admin_calendar.py",
        "test_admin_contacts.py",
        "test_admin_files.py",
        "test_admin_nat_mode.py",
        "test_admin_nest.py",
        "test_admin_settings.py",
        "test_admin_dns.py",
        "test_admin.py",
    )
    eligible = []
    for module in modules:
        tree = ast.parse((Path(__file__).parent / module).read_text(encoding="utf-8"))
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if not fn.name.startswith("test_"):
                continue
            item = _FakeItem(name=fn.name, fixturenames=[a.arg for a in fn.args.args])
            if ns.classify(item, nm.parse_nest_mode("live")) is None:
                eligible.append(f"{module}::{fn.name}")
    assert not eligible, (
        f"admin-UI journeys still eligible for the live box: {eligible}"
    )


def test_a_journey_that_names_no_admin_element_still_runs_on_live():
    """The coverage direction: the scan admits everything outside the shell,
    and the bare-name merge must not drag an admin element onto an ordinary
    journey through a shared helper name."""
    from helpers import kind_reach
    from helpers import nest_surface as ns

    reached = kind_reach.element_ids_reached_by(
        "test_enable_activitypub", ns.admin_shell_element_ids()
    )
    assert not reached, sorted(reached)
    item = _FakeItem(name="test_enable_activitypub")
    assert ns.classify(item, nm.parse_nest_mode("live")) is None


def test_live_ok_re_admits_a_ui_driven_admin_journey():
    """The two live-only residents — the harness-tier definer and the box
    bootstrap — drive the admin shell by design, under § The shared-box rule's
    non-destructive carve-out with the blast-radius argument in their
    docstrings; the marker is how they stay on live. The real modules carry it:
    without it the recipes that run them (`e2e-live-bootstrap`, the opt-in
    tier definer) would deselect their own test."""
    from pathlib import Path

    from helpers import nest_surface as ns

    item = _FakeItem(
        name="test_the_admin_defines_the_e2e_harness_tier",
        fixturenames=["admin_app"],
        markers=[_FakeMarker(ns.LIVE_READMIT_MARKER)],
    )
    assert ns.classify(item, nm.parse_nest_mode("live")) is None
    bare = _FakeItem(
        name="test_the_admin_defines_the_e2e_harness_tier", fixturenames=["admin_app"]
    )
    assert ns.classify(bare, nm.parse_nest_mode("live")) is not None
    for module in ("test_live_e2e_harness_tier.py", "test_live_box_bootstrap.py"):
        source = (Path(__file__).parent / module).read_text(encoding="utf-8")
        assert f"pytest.mark.{ns.LIVE_READMIT_MARKER}" in source, module


def test_a_private_helper_resolves_in_its_own_module(tmp_path):
    """A bare call to a `_name` resolves in the module that makes it — that is
    what the leading underscore means — so the graph keys private functions
    per module instead of merging them by bare name. Measured 2026-10-04: the
    merge was the admin-shell scan's dominant false positive (four test modules
    each defining their own `_state`, `_step`, `_snapshot` or `_off` inherited
    a live-provisioning test's `_state`, which reads the mail toggle), and
    lifting it also returned 45 tests the kind scan had been excluding the same
    way. An attribute call (`obj._state()`, a private method on an object from
    anywhere) and a bare call to a private name the module does not define (an
    import) keep the merge — still the safe direction."""
    from helpers import kind_reach

    tree = tmp_path / "e2e"
    (tree / "tests").mkdir(parents=True)
    (tree / "tests" / "test_live_relay.py").write_text(
        "def _state(app):\n"
        "    return app.read('admin-mail-enabled-toggle')\n"
        "def test_relay(app):\n"
        "    _state(app)\n",
        encoding="utf-8",
    )
    (tree / "tests" / "test_gap_rules.py").write_text(
        "def _state(gap):\n"
        "    return gap.value\n"
        "def test_gap(gap):\n"
        "    _state(gap)\n"
        "def test_method(obj):\n"
        "    obj._state()\n"
        "def test_imported(x):\n"
        "    _other(x)\n",
        encoding="utf-8",
    )
    (tree / "tests" / "test_other.py").write_text(
        "def _other(x):\n"
        "    x.read('admin-mail-enabled-toggle')\n",
        encoding="utf-8",
    )
    reach = kind_reach.build_reach(frozenset({"admin-mail-enabled-toggle"}), root=tree)
    hit = frozenset({"admin-mail-enabled-toggle"})
    assert reach["test_relay"] == hit, "a module's own private helper still reaches"
    assert reach["test_gap"] == frozenset(), (
        "a bare call to the module's own `_state` inherited another module's"
    )
    assert reach["test_method"] == hit, "an attribute call to a private name keeps the merge"
    assert reach["test_imported"] == hit, (
        "a bare call to a private name the module does not define keeps the merge"
    )
    assert reach["_state"] == hit, "the bare-name lookup view is the union over modules"


def test_a_module_constant_a_function_names_is_part_of_its_reach(tmp_path):
    """A string a test holds in a module-level constant — a `parametrize` table,
    an element-ID name — is reached as surely as one written in its body.

    Red-verified 2026-10-05 against the tree itself: the tui live sweep's
    collection against dev.example.com still selected
    `test_mail_admin_policy::test_admin_puts_policy_substruct_over_wire`, whose
    kinds (`fauna.bridges.put_*_policy`) live in the module's `_PUT_CASES` table
    and reach the test only through `@pytest.mark.parametrize(..., _PUT_CASES)`;
    its `policy_nest` is a "dedicated" nest, which in live resolves to the box
    itself, so the run would have overwritten the box's global mail policies.
    `test_nest_rotation_admin_journey` named its admin-shell IDs the same way
    (`ARM_BUTTON = "admin-nest-seed-rotate-button"`). A name that resolves to
    the function's own module's top-level assignment contributes that
    assignment's literals; any other name contributes nothing."""
    from helpers import kind_reach

    tree = tmp_path / "e2e"
    (tree / "tests").mkdir(parents=True)
    (tree / "tests" / "test_policy.py").write_text(
        "import pytest\n"
        "_CASES = [('fauna.bridges.put_spam_policy', {}), ('fauna.x.read', {})]\n"
        "ARM = 'admin-nest-seed-rotate-button'\n"
        "UNUSED = 'fauna.bridges.put_auth_policy'\n"
        "@pytest.mark.parametrize('kind,payload', _CASES)\n"
        "def test_put(kind, payload):\n"
        "    call(kind, payload)\n"
        "def test_arm(app):\n"
        "    app.click(ARM)\n"
        "def test_plain(app):\n"
        "    app.read('nothing')\n",
        encoding="utf-8",
    )
    (tree / "tests" / "test_elsewhere.py").write_text(
        "def test_shadow(app):\n"
        "    ARM = 'harmless'\n"
        "    app.click(ARM)\n",
        encoding="utf-8",
    )
    vocabulary = frozenset({
        "fauna.bridges.put_spam_policy", "fauna.bridges.put_auth_policy",
        "admin-nest-seed-rotate-button",
    })
    reach = kind_reach.build_reach(vocabulary, root=tree)
    assert reach["test_put"] == frozenset({"fauna.bridges.put_spam_policy"}), (
        "a kind named only in the parametrize table the decorator passes was missed"
    )
    assert reach["test_arm"] == frozenset({"admin-nest-seed-rotate-button"}), (
        "an element ID held in a module constant the body names was missed"
    )
    assert reach["test_plain"] == frozenset(), "an unnamed constant reaches nothing"
    assert reach["test_shadow"] == frozenset(), (
        "a name resolves in its OWN module — another module's constant is not reach"
    )


# ── The disposable-box declaration: `--live-box shared|disposable` (2026-10-04) ──
#
# Class (3) is a policy about the BOX — a human may be signed into it — and
# the CD gate runs against a box the pipeline wipes by design. `testing.md`
# § The shared-box rule → *The disposable-box declaration*: the run declares
# it, beside `--nest live:URL`, and only for a box the tree names as a staging
# box. Pinned here: the grammar, the three refusals, and that the declaration
# switches off class (3) and nothing else.


def _live(url="https://dev.example.com"):
    return nm.parse_nest_mode(f"live:{url}")


def test_a_run_declares_a_shared_box_unless_it_says_otherwise():
    """Absent flag and the explicit default read the same: shared. The
    destructive direction is never the one you get by leaving something out."""
    assert nm.declare_box(_live(), None).disposable_box is False
    assert nm.declare_box(_live(), nm.BOX_SHARED).disposable_box is False
    # `shared` states the default, so it is harmless on a mode with no box.
    assert nm.declare_box(nm.parse_nest_mode("standalone"), nm.BOX_SHARED).disposable_box is False


@pytest.mark.parametrize("url", [
    "https://dev.example.com",
    "https://dev.example.com/",
    "https://test.example.com",
    "https://test.example.com:8443",
    "  https://dev.example.com  ",
])
def test_the_declaration_is_accepted_for_a_named_staging_box(url):
    mode = nm.declare_box(_live(url), nm.BOX_DISPOSABLE)
    assert mode.disposable_box is True
    assert mode.is_live
    assert mode.argument == url.strip()


def test_the_declaration_is_refused_for_the_production_box():
    """The whole reason the declaration is gated on a list: a `cd_suite` run
    pointed at example.com must still deselect its factory-resetting residents."""
    with pytest.raises(nm.NestModeError) as info:
        nm.declare_box(_live("https://example.com"), nm.BOX_DISPOSABLE)
    message = str(info.value)
    assert "example.com" in message
    assert "dev.example.com" in message and "test.example.com" in message
    assert "shared-box rule" in message


def test_the_declaration_is_refused_for_a_box_the_tree_does_not_name():
    with pytest.raises(nm.NestModeError, match="staging.example"):
        nm.declare_box(_live("https://staging.example"), nm.BOX_DISPOSABLE)


def test_the_declaration_resolves_the_url_exactly_as_the_provider_does(monkeypatch):
    """`--nest live` with no URL falls back to `FAUNA_LIVE_NEST_URL`, then the
    project's live box — the provider's precedence — so the refusal reads the
    box the run would actually hit, never a flag-only view of it."""
    monkeypatch.setenv("FAUNA_LIVE_NEST_URL", "https://test.example.com")
    assert nm.live_url(nm.parse_nest_mode("live")) == "https://test.example.com"
    assert nm.declare_box(nm.parse_nest_mode("live"), nm.BOX_DISPOSABLE).disposable_box is True

    monkeypatch.delenv("FAUNA_LIVE_NEST_URL", raising=False)
    assert nm.live_url(nm.parse_nest_mode("live")) == "https://example.com"
    with pytest.raises(nm.NestModeError, match="example.com"):
        nm.declare_box(nm.parse_nest_mode("live"), nm.BOX_DISPOSABLE)

    # The flag's URL beats the env, as for every other harness input.
    monkeypatch.setenv("FAUNA_LIVE_NEST_URL", "https://example.com")
    assert nm.live_url(_live("https://dev.example.com/")) == "https://dev.example.com"
    assert nm.declare_box(_live(), nm.BOX_DISPOSABLE).disposable_box is True


@pytest.mark.parametrize("raw", ["standalone", "docker", "docker:ghcr.io/x/nest:dev"])
def test_the_declaration_needs_a_live_mode(raw):
    """A disposable box is a statement about a live box. On a mode the harness
    starts itself there is no box to declare, and accepting the flag there
    would let a recipe carry it into a later `--nest live` edit unnoticed."""
    with pytest.raises(nm.NestModeError, match="live"):
        nm.declare_box(nm.parse_nest_mode(raw), nm.BOX_DISPOSABLE)


@pytest.mark.parametrize("raw", ["", "   ", "throwaway", "DISPOSABLE "])
def test_an_unknown_or_empty_declaration_is_refused(raw):
    """Same contract as `--nest`: a present-but-empty value is a broken recipe
    (`--live-box "$UNSET"`), and an unknown word is never read as the default."""
    with pytest.raises(nm.NestModeError, match="shared|disposable"):
        nm.declare_box(_live(), raw)


def test_the_declaration_does_not_touch_the_test_id_stamp():
    """`test_x[tui-live]` stays the baseline key whatever the box is."""
    mode = nm.declare_box(_live(), nm.BOX_DISPOSABLE)
    assert mode.id == "live"
    assert str(mode) == "live"


def test_a_disposable_box_switches_class_three_off_on_every_door():
    """All four doors class (3) has: the marker, the fixture list, the body's
    kind reach and the admin shell. Each is a verdict on a shared box and
    nothing on a disposable one — there is no human on the box to protect."""
    from helpers import nest_surface as ns

    shared = _live()
    disposable = nm.declare_box(shared, nm.BOX_DISPOSABLE)

    by_marker = _FakeItem(markers=[_FakeMarker(ns.GLOBAL_ADMIN_MARKER)])
    # Two real tests: one reaches a global kind through an admin-client wrapper,
    # one drives the admin shell through the app UI.
    by_kind = _FakeItem(name="test_force_rotate_dkim_flips_active_selector_live")
    by_shell = _FakeItem(name="test_live_mail_receive_ui_only", fixturenames=["app"])

    for item in (by_marker, by_kind, by_shell):
        verdict = ns.classify(item, shared)
        assert verdict is not None and verdict.rule == ns.RULE_GLOBAL_ADMIN, (
            f"{item.name}: expected class (3) on a shared box, got {verdict}"
        )
        assert ns.classify(item, disposable) is None, (
            f"{item.name}: class (3) must be off on a declared-disposable box"
        )
        assert ns.RULE_GLOBAL_ADMIN not in ns.all_rules(item, disposable)

    # The fixture door. Every class-(3) fixture is also excluded by a
    # structural class (the subsumption pin above), so the first verdict is
    # never class (3) here; the audit's full account is where the door shows,
    # and where the declaration must close it — and only it.
    by_fixture = _FakeItem(fixturenames=["registration_posture_nest"])
    assert ns.RULE_GLOBAL_ADMIN in ns.all_rules(by_fixture, shared)
    rules_disposable = ns.all_rules(by_fixture, disposable)
    assert ns.RULE_GLOBAL_ADMIN not in rules_disposable
    assert rules_disposable, "the structural exclusion must survive the declaration"


def test_a_disposable_box_lifts_nothing_structural():
    """The declaration is a policy switch, not `live_ok` writ large: a test
    that needs a local binary, spawns a host bridge or reaches a test hook is
    excluded from live by a FACT about where the nest runs, and the box being
    disposable changes none of those facts."""
    from helpers import nest_surface as ns

    disposable = nm.declare_box(_live(), nm.BOX_DISPOSABLE)
    binary = _FakeItem(fixturenames=["sell_nest", "nest_binary", "logged_in_app"])
    verdict = ns.classify(binary, disposable)
    assert verdict is not None and verdict.rule == ns.RULE_NEST_BINARY
    bridge = _FakeItem(
        fixturenames=["mail_bridge_mta"],
        markers=[_FakeMarker(ns.LIVE_READMIT_MARKER)],
    )
    verdict = ns.classify(bridge, disposable)
    assert verdict is not None and verdict.rule != ns.RULE_GLOBAL_ADMIN


def test_the_staging_boxes_are_the_ones_the_pipeline_verifies_against():
    """The list the declaration is gated on must name every box the image
    workflows verify a candidate on (`VERIFY_HOST`), or the CD gate could not
    declare its own target disposable — and it must never name the production
    box."""
    workflows = pathlib.Path(__file__).resolve().parents[3] / ".github" / "workflows"
    verify_hosts = set()
    for path in sorted(workflows.glob("*.yml")):
        for match in re.finditer(r"^\s*VERIFY_HOST:\s*(\S+)", path.read_text(), re.M):
            verify_hosts.add(match.group(1).strip("'\""))
    assert verify_hosts, "no workflow declares a VERIFY_HOST — the pin lost its source"
    assert verify_hosts <= nm.DISPOSABLE_BOX_HOSTS, (
        f"a workflow verifies against {sorted(verify_hosts - nm.DISPOSABLE_BOX_HOSTS)}, "
        "which the CD gate could not declare disposable"
    )
    assert "example.com" not in nm.DISPOSABLE_BOX_HOSTS


# ── The live-mode harness gaps (2026-10-05) ──────────────────────────────────
# A full tui sweep against dev.example.com recorded tests as failed live cells
# that live mode could never have passed. Each class below is decided at
# collection; these pins hold the shape of each rule and its mode scope.


class _OwnCodeItem(_FakeItem):
    """A `_FakeItem` with a source file, which the own-code scans read."""

    def __init__(self, path, name, fixturenames=(), markers=()):
        super().__init__(fixturenames=fixturenames, markers=markers, name=name)
        self.path = path


def _own_code_item(tmp_path, source, name, fixturenames=()):
    path = tmp_path / "test_own_code_probe.py"
    path.write_text(source, encoding="utf-8")
    return _OwnCodeItem(path, name, fixturenames=fixturenames)


def test_a_tier_1_test_is_deselected_from_docker_and_live_only():
    """A tier_1 test has no nest for a mode to choose, so a docker or live run
    deselects it; standalone, the mode it was written for, keeps it."""
    from helpers import nest_surface as ns

    item = _FakeItem(markers=[_FakeMarker(ns.IN_PROCESS_TIER_MARKER)])
    assert ns.classify(item, nm.parse_nest_mode("standalone")) is None
    for mode in ("docker", "live"):
        verdict = ns.classify(item, nm.parse_nest_mode(mode))
        assert verdict is not None and verdict.rule == ns.RULE_IN_PROCESS_TIER, mode


def test_a_capability_read_in_the_tests_own_code_is_deselected_from_live(tmp_path):
    """`nest_instance["db_path"]` in the body, or a shared helper the table
    derives as reading one (`stop_nest` reads `proc`), excludes the test from
    live — the mode that answers no capability key. Docker answers some and its
    helpers branch on which, so it never fires there."""
    from helpers import nest_surface as ns

    source = (
        "from common.nest import stop_nest\n"
        "def _private_read(nest):\n"
        "    return nest['db_path']\n"
        "def test_reads(nest_instance):\n"
        "    _private_read(nest_instance)\n"
        "def test_stops(nest_instance):\n"
        "    stop_nest(nest_instance)\n"
        "def test_neither(nest_instance):\n"
        "    return nest_instance['url']\n"
    )
    live, docker = nm.parse_nest_mode("live"), nm.parse_nest_mode("docker")
    for name, key in (("test_reads", "db_path"), ("test_stops", "proc")):
        item = _own_code_item(tmp_path, source, name, ["nest_instance"])
        verdict = next(
            (v for v in ns._verdicts(item, live) if v.rule == ns.RULE_ABSENT_CAPABILITY),
            None,
        )
        assert verdict is not None and key in verdict.reason, name
        assert ns.RULE_ABSENT_CAPABILITY not in ns.all_rules(item, docker), name
    item = _own_code_item(tmp_path, source, "test_neither", ["nest_instance"])
    assert ns.RULE_ABSENT_CAPABILITY not in ns.all_rules(item, live)


def test_the_capability_helper_table_is_shared_functions_only():
    """The derived table names the shared helpers that read a capability key,
    and never a METHOD: a method name is merged by every class that defines it
    (`stop`, `close`), the flood that took a graph-wide scan to 5,709 tests."""
    from helpers import kind_reach

    table = kind_reach.capability_helpers(frozenset(nm.CAPABILITY_KEYS))
    assert "proc" in table["stop_nest"]
    assert "db_path" in table["segment_dir"]
    assert not {"stop", "close", "launch", "read"} & table.keys()


def test_a_start_option_the_tests_own_code_passes_is_class_four(tmp_path):
    """Two doors the closure cannot see: an option passed straight to a
    provider's `start`, and a nest fixture requested lazily. Both used to reach
    the provider's backstop at setup as a `NestModeError`."""
    from helpers import nest_surface as ns

    source = (
        "def _boxes(provider):\n"
        "    return provider.start(None, None, 'a', unclaimed=True, serve_tls=False)\n"
        "def test_inline(provider):\n"
        "    _boxes(provider)\n"
        "def test_lazy(request):\n"
        "    request.getfixturevalue('self_signed_nest')\n"
    )
    live = nm.parse_nest_mode("live")
    inline = ns.classify(_own_code_item(tmp_path, source, "test_inline"), live)
    assert inline.rule == ns.RULE_PERMANENT_OPTION and "unclaimed" in inline.reason
    assert "serve_tls" not in inline.reason, "a falsy literal asks for the default"
    lazy = ns.classify(_own_code_item(tmp_path, source, "test_lazy"), live)
    assert lazy.rule == ns.RULE_PERMANENT_OPTION and "self_signed_nest" in lazy.reason


def test_two_distinct_nests_are_deselected_from_live_only():
    """Every nest a live run starts is the same box, so a test that needs a
    second one — a destination, a linked nest, a peer — cannot run there."""
    from helpers import nest_surface as ns

    item = _FakeItem(fixturenames=["nest_instance", "second_nest"])
    assert ns.RULE_ONE_LIVE_BOX in set(ns.all_rules(item, nm.parse_nest_mode("live")))
    assert ns.RULE_ONE_LIVE_BOX not in set(ns.all_rules(item, nm.parse_nest_mode("docker")))
    single = _FakeItem(fixturenames=["second_nest"])
    assert ns.RULE_ONE_LIVE_BOX not in set(ns.all_rules(single, nm.parse_nest_mode("live")))


def test_a_harness_box_premise_is_deselected_from_live_only():
    """Marker or listed fixture, the premise holds on a nest this harness
    started — docker's included — and on no deployed box."""
    from helpers import nest_surface as ns

    for item in (
        _FakeItem(markers=[_FakeMarker(ns.HARNESS_BOX_MARKER)]),
        _FakeItem(fixturenames=["archive_nest"]),
    ):
        assert ns.RULE_HARNESS_BOX in set(ns.all_rules(item, nm.parse_nest_mode("live")))
        assert ns.RULE_HARNESS_BOX not in set(ns.all_rules(item, nm.parse_nest_mode("docker")))
        assert ns.classify(item, nm.parse_nest_mode("standalone")) is None


def test_the_harness_box_marker_is_registered():
    from helpers import nest_surface as ns

    ini = (pathlib.Path(__file__).resolve().parents[1] / "pytest.ini").read_text()
    assert f"\n    {ns.HARNESS_BOX_MARKER}:" in ini


def test_the_test_hook_kinds_are_what_the_gated_modules_register():
    """`TEST_HOOK_KINDS` by equality with the kinds nest registers (`.add(
    "fauna.…"`) in the modules `lib.rs` gates on `#[cfg(feature =
    "test-hooks")]` — a newly gated kind fails here until it is listed."""
    from helpers import nest_surface as ns

    src = pathlib.Path(__file__).resolve().parents[3] / "bins" / "fauna-nest" / "src"
    lib = (src / "lib.rs").read_text(encoding="utf-8")
    modules = re.findall(r'#\[cfg\(feature = "test-hooks"\)\]\s*pub mod (\w+);', lib)
    assert "protocol_test" in modules
    registered = set()
    for module in modules:
        for path in (src / f"{module}.rs", src / module / "mod.rs"):
            if path.exists():
                registered |= set(re.findall(
                    r'\.add\(\s*"(fauna\.[a-z0-9_.]+)"', path.read_text(encoding="utf-8")
                ))
    assert registered == ns.TEST_HOOK_KINDS


def test_a_test_hook_kind_is_class_six_in_both_non_standalone_modes():
    from helpers import nest_surface as ns

    item = _FakeItem(name="test_admin_can_open_ws_rpc_session_and_echo")
    for mode in ("docker", "live"):
        assert ns.RULE_TEST_HOOKS in set(ns.all_rules(item, nm.parse_nest_mode(mode))), mode
