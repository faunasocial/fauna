# Fauna's Share entry in GNOME Files (nautilus-python, the Nautilus 4.0 API).
#
# Written by the Fauna app at sign-in with its agent command filled in — do
# not edit; the next sign-in rewrites it. Mechanism:
# docs/goal/architecture/apps/linux.md § The file manager's Share entry
# (GNOME Files).
#
# This module is the file manager's shell and nothing else: it holds no
# protocol, no decision and no user-facing text beyond the product name. On one
# selected local item it asks the sync agent's `--share-target` helper, which
# answers with the in-app route and the leaf's localized label, or with nothing
# and a non-zero exit when the item cannot be shared; only an answer puts the
# Fauna > Share item on the menu, and opening the leaf hands the route to the
# desktop's fauna:// handler, the Fauna app.

import shlex
import subprocess

import gi

gi.require_version("Nautilus", "4.0")
from gi.repository import Gio, GObject, Nautilus  # noqa: E402

# The agent command for this install channel, the one the agent's systemd
# user unit runs (the app fills it in).
AGENT_COMMAND = "@AGENT_COMMAND@"

# Above the agent's own 6 s request bound, so a wedged agent costs one bounded
# wait and never hangs the right-click.
HELPER_TIMEOUT_S = 8

ROUTE_SCHEME = "fauna://"


def share_target(path):
    """The (route, label) the helper answers for `path`, or None."""
    try:
        argv = shlex.split(AGENT_COMMAND) + ["--share-target", path]
        done = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=HELPER_TIMEOUT_S,
            check=False,
        )
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    lines = done.stdout.splitlines()
    if len(lines) < 2 or not lines[0].startswith(ROUTE_SCHEME) or not lines[1]:
        return None
    return lines[0], lines[1]


def _open_route(uri):
    Gio.AppInfo.launch_default_for_uri(uri, None)


class FaunaShareMenuProvider(GObject.GObject, Nautilus.MenuProvider):
    def get_file_items(self, *args):
        # Nautilus 4.0 passes (files); older hosts passed (window, files).
        files = args[-1]
        if len(files) != 1:
            return []
        item = files[0]
        if item.get_uri_scheme() != "file":
            return []
        location = item.get_location()
        path = location.get_path() if location is not None else None
        if not path:
            return []
        target = share_target(path)
        if target is None:
            return []
        route, label = target
        top = Nautilus.MenuItem(name="FaunaShare::Fauna", label="Fauna")
        submenu = Nautilus.Menu()
        top.set_submenu(submenu)
        leaf = Nautilus.MenuItem(name="FaunaShare::Share", label=label)
        leaf.connect("activate", lambda *_: _open_route(route))
        submenu.append_item(leaf)
        return [top]
