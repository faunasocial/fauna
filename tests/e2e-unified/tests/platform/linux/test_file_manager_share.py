"""The GNOME Files Share entry, headless-first (`docs/goal/architecture/apps/linux.md`
§ The file manager's Share entry (GNOME Files), witness parts (a) and (b); part
(c), the nautilus-python module itself, is `test_nautilus_share_module.py`).

The mile splits where the mechanism splits:

(a) **the helper** — the linux app's own child agent, with a public set and a
    private set bound to local folders through the Folders page: `fauna-sync-agent
    --share-target` on a public file prints the `share-link` route and the leaf's
    label, on a set's own root the `folder-share` route, and on a sub-folder or a
    private set's file nothing, with a non-zero exit — exactly what the module
    reads to decide whether to offer the leaf (share-links outcomes 29, 30's
    second half);
(b) **the door** — each printed route, fed to the app through `open_route` (the
    same `routes::offer` GIO's `fauna://` handler reaches), lands on
    `media-item-detail` with `share-link-create-modal` open, or on the Folders
    page with that set's `folder-share-button` flow open (outcomes 28, 30).

(e) **the opener** — KDE Dolphin's service menu lists Share on every single
    local item and runs the helper with `--open`: under a stub `gtk-launch` (the
    notification tap's launcher, found on `PATH`) the route is launched for the
    public file and the set's root, and nothing is launched, exit non-zero, for
    the sub-folder and the private set's file (witness part (e); part (d), the
    written service menu, is `file_manager_share.rs`'s unit tests).

What is left to a human is whether the leaf *appears* in a real GNOME Files —
the module's loading by nautilus-python, which needs `python3-nautilus` — or a
real Dolphin, which needs `dolphin`.

Mutations go through the UI (convention 8): the bindings are made on the
Folders page; the sets, the public flip and the seeded file are the
`test_share_links` fixture carve-out. Every wait polls state, never a clock.
"""

import os
import subprocess
from pathlib import Path

import pytest

from conftest import _resolve_linux_binary
from helpers.folder_content import bind_location_under_set
from helpers.set_names import find_set
from helpers.waiting import wait_until
from tests.api.test_web_paywall_folder import _actor_client
from tests.test_share_links import _share_link_setup

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

PHOTO = "photo.jpg"
SUB = "album"
HELPER_DEADLINE_S = 90.0


@pytest.fixture
def bound_share_sets(request, app, nest_instance, tmp_path):
    """The linux app logged in as an owner whose public and private sets are
    each bound to a local folder holding one file (the public one also a
    sub-folder). Returns a dict of what the assertions name."""
    app, user, public_set, private_set = _share_link_setup(request, app, nest_instance)
    with _actor_client(nest_instance["url"], user) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    ids = {name: find_set(rows, name)["id"] for name in (public_set, private_set)}

    public_dir = tmp_path / "public"
    private_dir = tmp_path / "private"
    for location in (public_dir, private_dir):
        location.mkdir()
        (location / PHOTO).write_bytes(b"not really a jpeg\n")
    (public_dir / SUB).mkdir()
    bind_location_under_set(app, public_set, public_dir, seat="owner")
    bind_location_under_set(app, private_set, private_dir, seat="owner")
    return {
        "app": app,
        "public_set": public_set,
        "public_id": ids[public_set],
        "private_id": ids[private_set],
        "public_dir": public_dir,
        "private_dir": private_dir,
    }


def _agent_binary() -> Path:
    desktop = _resolve_linux_binary()
    assert desktop is not None, "the linux app is built (`just linux-debug`)"
    agent = Path(desktop).parent / "fauna-sync-agent"
    assert agent.is_file(), f"the agent is built beside the app: {agent}"
    return agent


def _share_target(app, path: Path, *, open_with: Path | None = None) -> subprocess.CompletedProcess:
    """Run the helper exactly as the GNOME Files module does — or, with
    `open_with` (a directory holding a stub launcher, put first on `PATH`),
    as Dolphin's service menu does — against the app's own child agent (its
    private runtime dir names the socket)."""
    socket = app.driver.sync_agent_socket
    assert socket, "the linux launch has a private runtime dir"
    env = dict(os.environ)
    env["XDG_RUNTIME_DIR"] = os.path.dirname(os.path.dirname(socket))
    argv = [str(_agent_binary())]
    if open_with is not None:
        env["PATH"] = f"{open_with}{os.pathsep}{env.get('PATH', '')}"
        argv.append("--open")
    return subprocess.run(
        [*argv, "--share-target", str(path)],
        capture_output=True, text=True, timeout=30, env=env, check=False,
    )


def _answer(app, path: Path) -> list[str] | None:
    done = _share_target(app, path)
    return done.stdout.splitlines() if done.returncode == 0 else None


@pytest.mark.feature("share-links")
def test_the_helper_answers_only_where_a_link_or_member_share_can_be_made(bound_share_sets):
    s = bound_share_sets
    app = s["app"]
    want_file = f"fauna://share-link?folder={s['public_id']}&path={PHOTO}"

    # The public file is a target once the set's engine has reported its
    # public-audience arm — fail-closed until then, so poll the answer.
    seen: list = []

    def _file_answered():
        seen.append(_answer(app, s["public_dir"] / PHOTO))
        return seen[-1] is not None

    wait_until(_file_answered, HELPER_DEADLINE_S, diagnose=lambda: (
        f"--share-target never answered for the public file; last={seen[-1:]!r}; "
        f"app error={app.error_text()!r}"
    ))
    assert seen[-1] == [want_file, "Share"], "the route, then the leaf's label"

    assert _answer(app, s["public_dir"]) == [
        f"fauna://folder-share?folder={s['public_id']}", "Share",
    ], "the set's own root opens its member sharing"
    assert _answer(app, s["private_dir"]) == [
        f"fauna://folder-share?folder={s['private_id']}", "Share",
    ], "a root is a member-share target whatever its audience"

    for refused in (s["public_dir"] / SUB, s["private_dir"] / PHOTO, s["public_dir"].parent):
        done = _share_target(app, refused)
        assert done.returncode != 0 and done.stdout == "", (
            f"{refused} must offer no Share (a sub-folder, a private file, an "
            f"unbound path): rc={done.returncode} out={done.stdout!r}"
        )


@pytest.mark.feature("share-links")
def test_the_printed_routes_open_the_create_surface_and_the_member_share_flow(bound_share_sets):
    s = bound_share_sets
    app = s["app"]
    d = app.driver

    # The route the helper prints for a public file, over the file the nest
    # already lists for the set (the fixture's seeded `holiday.txt`), so the
    # door's locate does not wait on this run's upload.
    file_route = f"fauna://share-link?folder={s['public_id']}&path=holiday.txt"
    d.call_command("open_route", {"uri": file_route})
    wait_until(
        lambda: d.is_visible("media-item-detail") and d.is_visible("share-link-create-modal"),
        30.0,
        diagnose=lambda: (
            f"share-link route did not open the create surface: "
            f"detail={d.is_visible('media-item-detail')} "
            f"create={d.is_visible('share-link-create-modal')} "
            f"media items={app.media.item_names()!r} error={app.error_text()!r}"
        ),
    )

    d.call_command("open_route", {"uri": f"fauna://folder-share?folder={s['public_id']}"})
    wait_until(
        lambda: d.is_visible("folder-share-confirm"),
        30.0,
        diagnose=lambda: f"folder-share route did not open the member-share flow; error={app.error_text()!r}",
    )


def _stub_launcher(directory: Path) -> Path:
    """A `gtk-launch` that records its argv, one launch per line, and exits 0
    — the notification tap's launcher seam, which the opener finds on `PATH`."""
    directory.mkdir()
    record = directory / "launches"
    stub = directory / "gtk-launch"
    stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{record}"\n')
    stub.chmod(0o755)
    return record


@pytest.mark.feature("share-links")
def test_open_launches_the_route_only_where_share_is_offered(bound_share_sets, tmp_path):
    s = bound_share_sets
    app = s["app"]
    stub_dir = tmp_path / "launcher"
    record = _stub_launcher(stub_dir)
    desktop_id = "social.fauna.fauna.desktop"

    def _launches() -> list[str]:
        return record.read_text().splitlines() if record.exists() else []

    # The public file is a target only once the set's engine has reported its
    # public-audience arm, so poll — each refused try launches nothing.
    want_file = f"fauna://share-link?folder={s['public_id']}&path={PHOTO}"
    seen: list = []

    def _file_opened():
        seen.append(_share_target(app, s["public_dir"] / PHOTO, open_with=stub_dir))
        return seen[-1].returncode == 0

    wait_until(_file_opened, HELPER_DEADLINE_S, diagnose=lambda: (
        f"--open never answered for the public file; last rc={seen[-1].returncode} "
        f"out={seen[-1].stdout!r} err={seen[-1].stderr[-400:]!r}; app error={app.error_text()!r}"
    ))
    assert _launches() == [f"{desktop_id} {want_file}"], (
        "exactly one launch, after the refusals before it launched nothing"
    )

    root = _share_target(app, s["public_dir"], open_with=stub_dir)
    assert root.returncode == 0, root.stderr[-400:]
    assert _launches()[-1] == f"{desktop_id} fauna://folder-share?folder={s['public_id']}"

    before = _launches()
    for refused in (s["public_dir"] / SUB, s["private_dir"] / PHOTO):
        done = _share_target(app, refused, open_with=stub_dir)
        assert done.returncode != 0 and done.stdout == "", (
            f"{refused} must open nothing: rc={done.returncode} out={done.stdout!r}"
        )
    assert _launches() == before, f"an ineligible item launched: {_launches()[len(before):]!r}"
