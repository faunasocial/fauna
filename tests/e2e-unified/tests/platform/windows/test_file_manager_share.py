"""Explorer's Share leaf on windows, headless-first (`docs/goal/behavior/share-links.md`
§ Windows Explorer's Share leaf; the mechanism `docs/goal/architecture/apps/windows.md`
§ Shell Extension → *The Share hand-off*, four links).

The mile splits where the hand-off splits, the same cut the linux file
manager's witness makes (`tests/platform/linux/test_file_manager_share.py`):

(a) **the agent's answer** (link 1) — the windows app's own agent, with a public
    set and a private set bound to on-demand cloud-files roots through the
    Folders page, asked `ShareFile` for a path: the verb Explorer's leaf sends
    on the agent's pipe from `GetState` and `Invoke`. It is asked through the
    agent's `--share-target` mode, which sends the same verb and maps the
    answer through the same `fauna_core::app_route::AppRoute::for_share_target`
    the leaf's route builder calls: a public file prints the `share-link`
    route and the leaf's label, a set's own root the `folder-share` route,
    and a sub-folder, a private set's file or an unbound path nothing, with a
    non-zero exit, which is the error that hides the leaf (share-links
    outcomes 29, 30's second half);
(b) **the door** (link 4) — each route fed to the running app through
    `open_route`, the same `App.ApplyRoute` a forwarded `FaunaApp.exe "<uri>"`
    launch reaches, lands on `media-item-detail` with `share-link-create-modal`
    open, or on the Folders page with that set's `folder-share-button` flow
    open (outcomes 28, 30).

Links 2 and 3 are pinned below this tier: the leaf's route mapping
(`context_menu::share_route`, against the shared parser) and the app's
forward leg (`RouteHandoffEndpointTests`, real named events). What is left is
Explorer itself drawing the leaf and its click reaching `ShellExecuteW`, which
needs the shell extension installed on the box. Explorer's own
DLL cannot stand in here: it dials the per-user pipe alone, never this
launch's private one.

Mutations go through the UI (convention 8): the bindings are made on the
Folders page and the sub-folder is made in the root as Explorer's New folder
would make it; the sets, the public flip and the seeded files are the
`test_share_links` fixture carve-out. Every wait polls state, never a clock.
The windows app reaches a real agent only under both real-agent markers
(conftest `_apply_isolated_sync_agent_env`), which also keep it on this run's
own pipe.
"""

import os
import subprocess
from pathlib import Path

import pytest

from helpers.folder_content import (
    agent_diagnosis,
    bind_location_under_set,
    mountable_location,
    on_demand_root_stands,
)
from helpers.set_names import find_set
from helpers.waiting import wait_until
from tests.api.test_web_paywall_folder import _actor_client
from tests.test_share_links import FILENAME, _share_link_setup

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

#: The harness pipe override the agent's client half reads in a test-capable
#: build (`fauna_ipc::endpoint::E2E_PIPE_ENV`) — the pipe LEAF, no `\\.\pipe\`.
E2E_PIPE_ENV = "FAUNA_E2E_SYNC_PIPE"

PRIVATE_FILE = "diary.txt"  # `_share_link_setup`'s private set's one file
SUB = "album"
ROOT_DEADLINE_S = 90.0
AGENT_DEADLINE_S = 90.0


@pytest.fixture
def bound_share_sets(request, app, nest_instance, tmp_path):
    """The windows app logged in as an owner whose public and private sets are
    each bound to an on-demand cloud-files root, the public one also holding a
    sub-folder. Returns a dict of what the assertions name."""
    app, user, public_set, private_set = _share_link_setup(request, app, nest_instance)
    with _actor_client(nest_instance["url"], user) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    ids = {name: find_set(rows, name)["id"] for name in (public_set, private_set)}

    public_dir = mountable_location(request, tmp_path, "public")
    private_dir = mountable_location(request, tmp_path, "private")
    bind_location_under_set(app, public_set, public_dir, seat="owner")
    bind_location_under_set(app, private_set, private_dir, seat="owner")
    # A fresh windows binding is on-demand: its cloud-files root stands over the
    # directory once the agent has registered it — read from the filter, the
    # answer Explorer gets — and only then is the sub-folder made inside it.
    for location in (public_dir, private_dir):
        wait_until(
            lambda location=location: on_demand_root_stands(location),
            ROOT_DEADLINE_S,
            interval=0.3,
            diagnose=lambda location=location: (
                f"the on-demand root never stood over {location}; "
                f"error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
            ),
        )
    (public_dir / SUB).mkdir()
    return {
        "app": app,
        "public_set": public_set,
        "public_id": ids[public_set],
        "private_id": ids[private_set],
        "public_dir": public_dir,
        "private_dir": private_dir,
    }


def _share_target(app, agent_exe: str, path: Path) -> subprocess.CompletedProcess:
    """Ask this launch's agent for ``path``'s share target, as Explorer's leaf
    does, through the agent's own `--share-target` mode pointed at the
    launch's private pipe."""
    pipe = app.driver.sync_agent_pipe
    assert pipe, "the launch pins its agent's pipe (both real-agent markers)"
    env = dict(os.environ)
    env[E2E_PIPE_ENV] = pipe.rsplit("\\", 1)[-1]
    return subprocess.run(
        [agent_exe, "--share-target", str(path)],
        capture_output=True, text=True, timeout=30, env=env, check=False,
    )


def _answer(app, agent_exe: str, path: Path) -> list[str] | None:
    done = _share_target(app, agent_exe, path)
    return done.stdout.splitlines() if done.returncode == 0 else None


@pytest.mark.feature("share-links")
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_the_agent_answers_share_only_where_a_link_or_member_share_can_be_made(
    bound_share_sets, sync_agent_binary
):
    s = bound_share_sets
    app = s["app"]
    want_file = f"fauna://share-link?folder={s['public_id']}&path={FILENAME}"

    # The public file is a target once the set's engine has reported its
    # public-audience arm — fail-closed until then, so poll the answer. The
    # file is the one the nest already lists for the set, its placeholder
    # populated into the root.
    seen: list = []

    def _file_answered():
        seen.append(_answer(app, sync_agent_binary, s["public_dir"] / FILENAME))
        return seen[-1] is not None

    wait_until(_file_answered, AGENT_DEADLINE_S, diagnose=lambda: (
        f"ShareFile never answered for the public file; last={seen[-1:]!r}; "
        f"error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
    ))
    assert seen[-1] == [want_file, "Share"], "the route, then the leaf's label"

    assert _answer(app, sync_agent_binary, s["public_dir"]) == [
        f"fauna://folder-share?folder={s['public_id']}", "Share",
    ], "the set's own root opens its member sharing"
    assert _answer(app, sync_agent_binary, s["private_dir"]) == [
        f"fauna://folder-share?folder={s['private_id']}", "Share",
    ], "a root is a member-share target whatever its audience"

    for refused in (
        s["public_dir"] / SUB,
        s["private_dir"] / PRIVATE_FILE,
        s["public_dir"].parent,
    ):
        done = _share_target(app, sync_agent_binary, refused)
        assert done.returncode != 0 and done.stdout == "", (
            f"{refused} must offer no Share (a sub-folder, a private file, an "
            f"unbound path): rc={done.returncode} out={done.stdout!r}"
        )


@pytest.mark.feature("share-links")
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_the_share_routes_open_the_create_surface_and_the_member_share_flow(
    bound_share_sets,
):
    s = bound_share_sets
    app = s["app"]
    d = app.driver

    # The route the agent answers for the public file — over the file the nest
    # already lists for the set, so the door's locate does not wait on an upload.
    file_route = f"fauna://share-link?folder={s['public_id']}&path={FILENAME}"
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
        diagnose=lambda: (
            f"folder-share route did not open the member-share flow; "
            f"error={app.error_text()!r}"
        ),
    )
