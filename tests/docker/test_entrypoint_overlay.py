"""Tests for the nest image's deployment-artifact shell (docker/).

The load-bearing half is ``docker/nest-toml-overlay.sh``: the artifact's own
writes into the nest's persisted ``/data/nest.toml``. It is a separate script
purely so this test can run it against the **real** ``config/default.toml``
without docker — which is the gate that was missing when the class below shipped.

**The failure class this file exists for.** The overlay used to be an inline
``sed`` in ``entrypoint.sh`` anchored on a ``require_registration`` line.
A commit (2026-07-13, *"registration posture is client-set — delete the
flags"*) removed that line from ``config/default.toml`` and did not touch the
entrypoint. **A sed address that matches nothing is a silent no-op with exit 0**,
which ``set -euo pipefail`` cannot catch, so the overlay wrote *nothing*: every
Docker nest first-booted on or after that date got a ``nest.toml`` with neither
``static_dir`` nor ``cors_origins``. With ``static_dir`` unset,
``bins/fauna-nest/src/lib.rs`` never calls ``mount_spa``, so there is no ``/app``
route at all and the request falls through to ``web_content_or_info`` → the nest
info page. ``https://<domain>/app`` served **byte-identical HTML to
``https://<domain>/``** for eleven days, found live on a real box 2026-07-24.
``FAUNA_CORS_ORIGINS`` was silently inert over the same window.

Nothing caught it because ``bins/fauna-nest/tests/spa_security_headers.rs``
mounts ``mount_spa`` directly against a temp dir: it tests *the mechanism* and
never that the deployment artifact configures it. Textbook mechanism-tested /
wiring-untested gap — so the assertions here deliberately couple the artifact to
the real ``config/default.toml`` rather than to a fixture of it. A future commit
that renames or drops a ``[nest]`` key goes red here, not in production.

No docker, no network, no sudo — pure text. Run with:
``pytest tests/docker/test_entrypoint_overlay.py -v``
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

_REPO = Path(__file__).resolve().parent.parent.parent
_OVERLAY = _REPO / "docker" / "nest-toml-overlay.sh"
_ENTRYPOINT = _REPO / "docker" / "entrypoint.sh"
_DEFAULT_TOML = _REPO / "config" / "default.toml"
_DOCKERFILE = _REPO / "Dockerfile"

# The bundled SPA's path inside the image. Hard-coded here as well as in the
# overlay on purpose: this is the artifact constant the Dockerfile's
# `COPY --from=web-builder ... /usr/share/fauna-web/` produces, so the test
# pins the contract rather than reading the value back out of the thing
# under test.
STATIC_DIR = "/usr/share/fauna-web"


def _run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(_OVERLAY), *args],
        capture_output=True, text=True, timeout=30,
    )


def _fresh_nest_toml(tmp_path: Path) -> Path:
    """A first-run /data/nest.toml exactly as entrypoint.sh creates it: a
    verbatim copy of the canonical config/default.toml."""
    dest = tmp_path / "nest.toml"
    shutil.copy(_DEFAULT_TOML, dest)
    return dest


def _keys(text: str) -> dict[str, str]:
    """Parse `key = value` lines (the flat shape the overlay writes)."""
    out = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("["):
            continue
        if " = " in line:
            k, _, v = line.partition(" = ")
            out[k.strip()] = v.strip()
    return out


# ── The regression that shipped ───────────────────────────────────────


def test_static_dir_lands_in_a_first_run_nest_toml(tmp_path):
    """The bug, stated positively: the SPA path must reach nest.toml.

    Without it `bins/fauna-nest/src/lib.rs` never calls `mount_spa`, so there is
    no `/app` route and the info page answers instead — violating
    `web-content-hosting.md` § Same-origin security model invariant 3 (`/app` is
    a reserved path, never served as user content).
    """
    nest_toml = _fresh_nest_toml(tmp_path)
    proc = _run("ensure-static-dir", str(nest_toml))
    assert proc.returncode == 0, f"overlay failed:\n{proc.stderr}"

    keys = _keys(nest_toml.read_text())
    assert keys.get("static_dir") == f'"{STATIC_DIR}"', (
        f"static_dir absent from nest.toml after the overlay ran.\n"
        f"The nest will not mount the SPA and /app will serve the info page.\n"
        f"--- nest.toml ---\n{nest_toml.read_text()}"
    )


def test_cors_origins_seed_lands_in_a_first_run_nest_toml(tmp_path):
    """FAUNA_CORS_ORIGINS is a documented boot seed (`installers/docker.md`
    § Environment Variables) — an unwritten seed is a silently inert env var."""
    nest_toml = _fresh_nest_toml(tmp_path)
    cors = '["https://app.example.test"]'
    proc = _run("seed-cors-origins", str(nest_toml), cors)
    assert proc.returncode == 0, f"overlay failed:\n{proc.stderr}"

    keys = _keys(nest_toml.read_text())
    assert keys.get("cors_origins") == cors, (
        f"cors_origins absent/wrong after the overlay ran — FAUNA_CORS_ORIGINS "
        f"is inert.\n--- nest.toml ---\n{nest_toml.read_text()}"
    )


def test_written_keys_land_inside_the_node_table(tmp_path):
    """Both keys are plain `[nest]` members. Landing them after a later table
    header would silently file them under *that* table instead — the nest would
    parse a valid file and still not mount the SPA."""
    nest_toml = _fresh_nest_toml(tmp_path)
    assert _run("ensure-static-dir", str(nest_toml)).returncode == 0
    assert _run("seed-cors-origins", str(nest_toml), "[]").returncode == 0

    table = None
    seen = {}
    for line in nest_toml.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            table = stripped
        elif " = " in stripped and not stripped.startswith("#"):
            seen[stripped.split(" = ")[0]] = table
    assert seen.get("static_dir") == "[nest]", f"static_dir under {seen.get('static_dir')}"
    assert seen.get("cors_origins") == "[nest]", f"cors_origins under {seen.get('cors_origins')}"


def test_a_same_named_key_in_another_table_does_not_defeat_the_write(tmp_path):
    """The search / rewrite /
    verify were **whole-file**, not `[nest]`-scoped.

    So a `static_dir` under *any other* table made the replace branch fire, the
    rewrite land on that foreign line, `[nest]` never gain the key — and the
    whole-file `grep -qxF` verification pass anyway. That is the same
    silent-success shape this script exists to eliminate, one level up: the
    original bug was an anchor that assumed something about `config/default.toml`
    which a later commit invalidated.

    Not reachable with today's shipped config (`[nest]` + `[acme]`, neither
    carrying these keys), which is exactly why it is worth pinning before some
    future table makes it reachable.
    """
    nest_toml = tmp_path / "nest.toml"
    nest_toml.write_text(
        '[nest]\ndata_dir = "/data"\n\n'
        # A decoy in a later table. Whole-file matching sees this and stops.
        '[some_future_table]\nstatic_dir = "/not/the/spa"\n'
    )
    assert _run("ensure-static-dir", str(nest_toml)).returncode == 0

    table = None
    seen = {}
    for line in nest_toml.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            table = stripped
        elif " = " in stripped and not stripped.startswith("#"):
            seen.setdefault(table, {})[stripped.split(" = ")[0]] = stripped

    assert "static_dir" in seen.get("[nest]", {}), (
        "[nest] never gained static_dir — a same-named key in another table "
        f"absorbed the write. Tables seen: { {t: list(k) for t, k in seen.items()} }"
    )
    # …and the foreign key is left exactly as it was: this script reconciles
    # `[nest]`, it does not get to rewrite somebody else's table.
    assert seen.get("[some_future_table]", {}).get("static_dir") == 'static_dir = "/not/the/spa"', (
        "the overlay rewrote a key belonging to another table"
    )


def test_node_table_header_is_found_when_indented(tmp_path):
    """TOML permits leading whitespace before a table header, and the insert
    anchor compared the line's first whitespace-delimited field — which is the
    *empty string* for an indented header, so `[nest]` went unrecognised and the
    boot was refused. Fails closed rather than open, but on a valid file.
    """
    nest_toml = tmp_path / "nest.toml"
    nest_toml.write_text('  [nest]\n  data_dir = "/data"\n')
    proc = _run("ensure-static-dir", str(nest_toml))
    assert proc.returncode == 0, (
        f"overlay refused a valid indented [nest] header; stderr:\n{proc.stderr}"
    )
    assert "static_dir" in _keys(nest_toml.read_text())


def test_overlay_fails_loudly_when_its_anchor_is_gone(tmp_path):
    """**The gate that actually kills the class.**

    The original defect was not the missing key — it was that the write could
    fail *silently*. A nest.toml the overlay cannot anchor into must abort the
    boot with a diagnostic, never proceed to serve a half-configured nest.
    """
    nest_toml = tmp_path / "nest.toml"
    nest_toml.write_text('[acme]\ndir = "/data/acme"\n')  # no [nest] table
    proc = _run("ensure-static-dir", str(nest_toml))
    assert proc.returncode != 0, (
        "overlay silently succeeded against a nest.toml it could not write to — "
        "this is exactly the 2026-07-13 failure mode"
    )
    assert "nest.toml" in proc.stderr, (
        f"failure was not diagnostic; stderr was:\n{proc.stderr}"
    )


# ── The every-boot reconcile (how already-deployed boxes self-heal) ────


def test_static_dir_self_heals_on_a_nest_toml_with_no_static_dir(tmp_path):
    """A box first-booted between 2026-07-13 and the fix has a nest.toml with no
    `static_dir`, and its first-run block never runs again. Pulling a fixed image
    must repair it, or every such box stays broken forever — the reason
    `ensure-static-dir` is an every-boot reconcile and not a first-run write.
    """
    nest_toml = _fresh_nest_toml(tmp_path)  # exactly the broken-window shape
    assert "static_dir" not in nest_toml.read_text()

    proc = _run("ensure-static-dir", str(nest_toml))
    assert proc.returncode == 0, f"reconcile failed:\n{proc.stderr}"
    assert _keys(nest_toml.read_text()).get("static_dir") == f'"{STATIC_DIR}"'


def test_static_dir_reconcile_is_idempotent_and_corrects_a_stale_value(tmp_path):
    """Repeated boots must not duplicate the key, and a value left over from an
    older image layout must be corrected rather than kept."""
    nest_toml = _fresh_nest_toml(tmp_path)
    nest_toml.write_text(
        nest_toml.read_text().replace("[nest]\n", '[nest]\nstatic_dir = "/old/path"\n')
    )
    for _ in range(3):
        assert _run("ensure-static-dir", str(nest_toml)).returncode == 0

    lines = [ln for ln in nest_toml.read_text().splitlines() if ln.startswith("static_dir")]
    assert lines == [f'static_dir = "{STATIC_DIR}"'], f"got {lines}"


def test_cors_seed_does_not_disturb_an_already_reconciled_static_dir(tmp_path):
    """The two subcommands write the same table; neither may clobber the other."""
    nest_toml = _fresh_nest_toml(tmp_path)
    assert _run("ensure-static-dir", str(nest_toml)).returncode == 0
    assert _run("seed-cors-origins", str(nest_toml), '["https://a.test"]').returncode == 0

    keys = _keys(nest_toml.read_text())
    assert keys.get("static_dir") == f'"{STATIC_DIR}"'
    assert keys.get("cors_origins") == '["https://a.test"]'


# ── The entrypoint actually calls it ──────────────────────────────────


def test_entrypoint_invokes_both_overlay_subcommands():
    """A perfect overlay wired to nothing is the same outage. Pin the calls, and
    pin that the reconcile is NOT inside the first-run block (the property the
    already-deployed boxes depend on)."""
    text = _ENTRYPOINT.read_text()
    assert "nest-toml-overlay.sh seed-cors-origins" in text
    assert "nest-toml-overlay.sh ensure-static-dir" in text

    first_run = text.index('if [ ! -f /data/nest.toml ]')
    # The `else`/`fi` pair that closes the first-run block.
    block_end = text.index('echo "=== Starting nest (existing config) ==="')
    reconcile = text.index("nest-toml-overlay.sh ensure-static-dir")
    assert reconcile > block_end > first_run, (
        "ensure-static-dir must run AFTER the first-run block, on every boot — "
        "inside it, no already-deployed box would ever self-heal"
    )


def test_dockerfile_ships_the_overlay_script():
    """The entrypoint calls it by absolute path; an uncopied script is a boot
    crash on every container from the image."""
    text = _DOCKERFILE.read_text()
    assert "COPY docker/nest-toml-overlay.sh /usr/local/bin/nest-toml-overlay.sh" in text
    assert "/usr/local/bin/nest-toml-overlay.sh" in text.split("RUN chmod +x")[1][:400], (
        "overlay script copied but never made executable"
    )


# ── Artifact shell smoke checks ───────────────────────────────────────
# (Absorbed from tests/docker/test_entrypoint.sh, which was wired to no gate and
# therefore never ran; these now execute on the merge path.)


@pytest.mark.parametrize("script", ["entrypoint.sh", "nest-toml-overlay.sh"])
def test_artifact_shell_scripts_parse(script):
    proc = subprocess.run(
        ["bash", "-n", str(_REPO / "docker" / script)],
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0, f"{script} syntax error:\n{proc.stderr}"


@pytest.mark.parametrize(
    "svc", ["fauna-nest", "fauna-mail-bridge-mta", "fauna-mail-bridge-mda"]
)
def test_s6_service_definitions_exist(svc):
    for f in ("type", "run"):
        assert (_REPO / "docker" / "s6" / svc / f).is_file(), f"docker/s6/{svc}/{f} missing"
