"""E2E tests: Linux installer — .deb, AppImage, Flatpak, Snap, systemd services."""

import glob
import os
import random
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.request

import pytest

from drivers.port_util import popen_group_kwargs, reap_descendants_of
from drivers.x_display import sweep_stale_x_locks
from helpers.app_surface import skip_environment

pytestmark = [pytest.mark.skipif(sys.platform != "linux", reason="Linux-only"), pytest.mark.tier_3]

needs_root = pytest.mark.skipif(os.getuid() != 0, reason="Requires root")
needs_snapd = pytest.mark.skipif(not shutil.which("snapcraft"), reason="snapcraft not found")
needs_flatpak = pytest.mark.skipif(not shutil.which("flatpak"), reason="flatpak not found")
needs_display = pytest.mark.skipif(
    not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"),
    reason="No display available",
)


# ── Helper functions ──


def _get_repo_root():
    """Return the repository root path via git."""
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return result.stdout.strip()


def max_mtime(patterns):
    """Return the maximum mtime across all files matching the glob patterns."""
    best = 0.0
    for pattern in patterns:
        for path in glob.glob(pattern, recursive=True):
            try:
                best = max(best, os.path.getmtime(path))
            except OSError:
                pass
    return best


def check_cache(stamp_path, current_mtime, artifact_path):
    """Return True if the cached artifact is up-to-date.

    Checks that both the stamp file and the artifact exist and the cached
    mtime recorded in the stamp is >= current_mtime.
    """
    if not os.path.exists(artifact_path) or not os.path.exists(stamp_path):
        return False
    try:
        with open(stamp_path) as f:
            cached_mtime = float(f.read().strip())
        return cached_mtime >= current_mtime
    except (ValueError, OSError):
        return False


def write_stamp(stamp_path, mtime):
    """Write mtime to the stamp file, creating parent directories as needed."""
    os.makedirs(os.path.dirname(stamp_path), exist_ok=True)
    with open(stamp_path, "w") as f:
        f.write(str(mtime))


def cargo_build(packages, extra_args=None, timeout=600):
    """Build the given cargo packages in release mode.

    Calls pytest.fail on non-zero exit. Returns the completed subprocess result.
    """
    cmd = ["cargo", "build", "--release"]
    for pkg in packages:
        cmd.extend(["-p", pkg])
    if extra_args:
        cmd.extend(extra_args)
    result = subprocess.run(
        cmd,
        capture_output=True, text=True, timeout=timeout,
    )
    if result.returncode != 0:
        pytest.fail(
            f"cargo build failed (rc={result.returncode}) for {packages}:\n"
            f"stdout: {result.stdout[-2000:]}\nstderr: {result.stderr[-2000:]}"
        )
    return result


def launch_and_check(binary_path, timeout=3, args=None):
    """Start a binary, wait, check it has not crashed, then kill it.

    Returns True if the process was still alive after the wait period.
    """
    cmd = [binary_path] + (args or [])
    proc = subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    time.sleep(timeout)
    rc = proc.poll()
    if rc is None:
        # Still alive — good, kill it
        proc.kill()
        proc.wait()
        return True
    # Exited — accept clean exit (0) as success (e.g. onboarding window closed)
    return rc == 0


def wait_for_http(url, timeout=10):
    """Poll url until it responds with a 2xx status or timeout expires.

    Returns True if reachable, False if timed out.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            resp = urllib.request.urlopen(url, timeout=2)
            if resp.status < 300:
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False


def _start_private_xvfb(max_attempts=10):
    """Start a throwaway Xvfb this test owns, on a display number it picks
    itself — and therefore knows.

    The shared driver's headless path (`drivers/linux.py::_headless_render_cmd_env`)
    goes through `xvfb-run -a`, which selects its display *inside* the child and
    exposes no accessor back to the launching process — fine for a driver whose
    tests never need the DISPLAY back, wrong for this one. A random high number
    (not a fixed one — this box runs many concurrent e2e sessions) with a
    collision retry mirrors what `xvfb-run -a` does internally, minus the
    opacity. Returns `(Popen, ":N")`.
    """
    last_err = ""
    # Same hygiene as the driver's launch path (`drivers/x_display.py`): the
    # collision branch below SIGKILLs a half-started Xvfb, which leaks its lock.
    sweep_stale_x_locks()
    for _ in range(max_attempts):
        num = random.randint(100, 9999)
        sock = f"/tmp/.X11-unix/X{num}"
        if os.path.exists(sock):
            continue
        display = f":{num}"
        log_path = tempfile.mktemp(prefix="fauna-e2e-xvfb-", suffix=".log")
        log = open(log_path, "w")
        proc = subprocess.Popen(
            ["Xvfb", display, "-screen", "0", "1024x768x24", "-nolisten", "tcp"],
            stdout=log, stderr=log,
            **popen_group_kwargs(),
        )
        reap_descendants_of(proc.pid)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                break
            if os.path.exists(sock):
                return proc, display
            time.sleep(0.1)
        # Collision (another run grabbed the same number between the check
        # above and Xvfb's own bind) or a genuine start failure — clean up and
        # try a different number.
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        last_err = open(log_path).read()
    raise RuntimeError(
        f"could not start a private Xvfb after {max_attempts} attempts: {last_err}"
    )


def _wm_class_pairs(display, timeout=15):
    """Poll `xwininfo -root -tree` for every top-level window's WM_CLASS
    (instance, class) pair, waiting for at least one non-empty pair to appear.

    No window manager runs under a bare Xvfb, so `_NET_CLIENT_LIST` is never
    populated (there is nothing to set it) — `xwininfo -tree` walks the raw X
    window tree instead, which needs no WM. A GTK4 process typically opens more
    than one top-level surface (helper/tooltip windows carry an empty
    `("" "")` pair) — collecting every non-empty pair rather than picking "the"
    window means this doesn't have to guess which one is the real application
    window on whatever screen happens to be showing at launch (onboarding,
    launch-chooser, or the authenticated main window all carry the SAME
    process-wide WM_CLASS).
    """
    pattern = re.compile(r'^\s*0x[0-9a-fA-F]+\s+"[^"]*":\s*\("([^"]*)"\s+"([^"]*)"\)')
    deadline = time.monotonic() + timeout
    last_output = ""
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["xwininfo", "-root", "-tree", "-display", display],
            capture_output=True, text=True, timeout=5,
        )
        last_output = result.stdout
        pairs = [
            (m.group(1), m.group(2))
            for line in result.stdout.splitlines()
            if (m := pattern.match(line)) and (m.group(1) or m.group(2))
        ]
        if pairs:
            return pairs
        time.sleep(0.3)
    raise TimeoutError(
        f"no window with a non-empty WM_CLASS appeared on {display} within "
        f"{timeout}s:\n{last_output}"
    )


# ── Source patterns for cache invalidation ──

DESKTOP_SOURCES = [
    "apps/fauna-linux/src/**/*.rs",
    "apps/fauna-linux/Cargo.toml",
    "libs/*/src/**/*.rs",
    "libs/*/Cargo.toml",
]
AGENT_SOURCES = [
    "bins/fauna-sync-agent/src/**/*.rs",
    "bins/fauna-sync-agent/Cargo.toml",
    "libs/*/src/**/*.rs",
    "libs/*/Cargo.toml",
]
NEST_SOURCES = [
    "bins/fauna-nest/src/**/*.rs",
    "bins/fauna-nest/Cargo.toml",
    "libs/*/src/**/*.rs",
    "libs/*/Cargo.toml",
]


# ── Build fixtures ──


@pytest.fixture(scope="session")
def repo_root():
    """Return the repository root path."""
    return _get_repo_root()


@pytest.fixture(scope="session")
def desktop_binary(repo_root):
    """Build fauna-linux in release mode, returning the binary path.

    Cached: skips the build if sources have not changed since the last run
    (stamp file at build/linux-installer/desktop.stamp).
    """
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.join(repo_root, "target"))
    binary_path = os.path.join(target_dir, "release", "fauna-desktop")
    stamp_path = os.path.join(repo_root, "build", "linux-installer", "desktop.stamp")

    full_patterns = [os.path.join(repo_root, p) for p in DESKTOP_SOURCES]
    current_mtime = max_mtime(full_patterns)

    if check_cache(stamp_path, current_mtime, binary_path):
        return binary_path

    cargo_build(["fauna-linux"])
    write_stamp(stamp_path, current_mtime)
    return binary_path


@pytest.fixture(scope="session")
def agent_binary(repo_root):
    """Build fauna-sync-agent in release mode, returning the binary path.

    Every install channel ships it beside fauna-desktop
    (installers/linux-desktop.md § Installation Files). Cached via
    build/linux-installer/agent.stamp.
    """
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.join(repo_root, "target"))
    binary_path = os.path.join(target_dir, "release", "fauna-sync-agent")
    stamp_path = os.path.join(repo_root, "build", "linux-installer", "agent.stamp")

    full_patterns = [os.path.join(repo_root, p) for p in AGENT_SOURCES]
    current_mtime = max_mtime(full_patterns)

    if check_cache(stamp_path, current_mtime, binary_path):
        return binary_path

    cargo_build(["fauna-sync-agent"])
    write_stamp(stamp_path, current_mtime)
    return binary_path


@pytest.fixture(scope="session")
def nest_binary(repo_root):
    """Build fauna-nest with the email feature in release mode, returning the binary path.

    Cached via build/linux-installer/nest.stamp.
    """
    target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.join(repo_root, "target"))
    binary_path = os.path.join(target_dir, "release", "fauna-nest")
    stamp_path = os.path.join(repo_root, "build", "linux-installer", "nest.stamp")

    full_patterns = [os.path.join(repo_root, p) for p in NEST_SOURCES]
    current_mtime = max_mtime(full_patterns)

    if check_cache(stamp_path, current_mtime, binary_path):
        return binary_path

    cargo_build(["fauna-nest"], extra_args=["--features", "email"])
    write_stamp(stamp_path, current_mtime)
    return binary_path


@pytest.fixture(scope="session")
def deb_path(desktop_binary, agent_binary, repo_root):
    """Assemble a .deb package from the built desktop binary.

    Stages:
      usr/bin/fauna-desktop
      usr/bin/fauna-sync-agent — ships beside the app on every channel
      usr/share/applications/social.fauna.fauna.desktop
      usr/share/icons/hicolor/scalable/apps/fauna.svg
      DEBIAN/control

    Returns the path to the built .deb file.
    """
    build_dir = os.path.join(repo_root, "build", "linux-installer")
    deb_file = os.path.join(build_dir, "fauna-desktop.deb")
    os.makedirs(build_dir, exist_ok=True)

    with tempfile.TemporaryDirectory(dir=build_dir, prefix="deb-stage-") as stage_dir:
        # Create directory tree
        bin_dir = os.path.join(stage_dir, "usr", "bin")
        apps_dir = os.path.join(stage_dir, "usr", "share", "applications")
        icons_dir = os.path.join(stage_dir, "usr", "share", "icons", "hicolor", "scalable", "apps")
        debian_dir = os.path.join(stage_dir, "DEBIAN")
        for d in (bin_dir, apps_dir, icons_dir, debian_dir):
            os.makedirs(d, exist_ok=True)

        # Stage binaries (the agent ships beside the app on every channel)
        shutil.copy2(desktop_binary, os.path.join(bin_dir, "fauna-desktop"))
        shutil.copy2(agent_binary, os.path.join(bin_dir, "fauna-sync-agent"))

        # Stage .desktop file
        # The entry's basename IS the app id (installers/linux-desktop.md
        # § Desktop entry) — the deb stages it under that name, which is also
        # what the metainfo's <launchable> points at.
        desktop_src = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "social.fauna.fauna.desktop"
        )
        shutil.copy2(desktop_src, os.path.join(apps_dir, "social.fauna.fauna.desktop"))

        # Stage icon
        icon_src = os.path.join(repo_root, "apps", "fauna-linux", "fauna.svg")
        shutil.copy2(icon_src, os.path.join(icons_dir, "fauna.svg"))

        # Stage debian control
        control_src = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "debian", "control"
        )
        shutil.copy2(control_src, os.path.join(debian_dir, "control"))

        # postinst / postrm
        app_dir = os.path.join(repo_root, "apps", "fauna-linux")
        pkg_root = stage_dir
        postinst_src = os.path.join(app_dir, "packaging", "debian", "postinst")
        postrm_src = os.path.join(app_dir, "packaging", "debian", "postrm")
        if os.path.exists(postinst_src):
            shutil.copy2(postinst_src, os.path.join(debian_dir, "postinst"))
            os.chmod(os.path.join(debian_dir, "postinst"), 0o755)
        if os.path.exists(postrm_src):
            shutil.copy2(postrm_src, os.path.join(debian_dir, "postrm"))
            os.chmod(os.path.join(debian_dir, "postrm"), 0o755)

        # AppStream metainfo
        metainfo_src = os.path.join(app_dir, "packaging", "social.fauna.fauna.metainfo.xml")
        metainfo_dir = os.path.join(pkg_root, "usr", "share", "metainfo")
        if os.path.exists(metainfo_src):
            os.makedirs(metainfo_dir, exist_ok=True)
            shutil.copy2(metainfo_src, os.path.join(metainfo_dir, "social.fauna.fauna.metainfo.xml"))

        # Build the .deb
        result = subprocess.run(
            ["dpkg-deb", "--build", stage_dir, deb_file],
            capture_output=True, text=True, timeout=60,
        )
        if result.returncode != 0:
            pytest.fail(
                f"dpkg-deb --build failed (rc={result.returncode}):\n"
                f"stdout: {result.stdout}\nstderr: {result.stderr}"
            )

    return deb_file


class TestShellScript:
    """Install/uninstall via apps/fauna-linux/install.sh with a temp PREFIX."""

    def test_install_and_uninstall(self, desktop_binary, agent_binary, repo_root):
        prefix = tempfile.mkdtemp(prefix="fauna-e2e-shell-")
        install_sh = os.path.join(repo_root, "apps", "fauna-linux", "install.sh")
        env = os.environ.copy()
        env["PREFIX"] = prefix

        rc = subprocess.run(
            ["bash", install_sh],
            capture_output=True, text=True, timeout=30,
            cwd=repo_root, env=env,
        )
        assert rc.returncode == 0, f"Install failed: {rc.stderr}"

        try:
            assert os.path.isfile(os.path.join(prefix, "bin", "fauna-desktop")), \
                "Binary not installed"
            assert os.path.isfile(os.path.join(prefix, "bin", "fauna-sync-agent")), \
                "Sync agent not installed beside the app " \
                "(installers/linux-desktop.md § Installation Files)"
            assert os.path.isfile(
                os.path.join(prefix, "share", "applications", "social.fauna.fauna.desktop")
            ), ".desktop file not installed under the app-id basename"
            assert os.path.isfile(
                os.path.join(prefix, "share", "icons", "hicolor", "scalable", "apps", "fauna.svg")
            ), "Icon not installed"
            assert os.access(os.path.join(prefix, "bin", "fauna-desktop"), os.X_OK), \
                "Binary not executable"
            assert os.access(os.path.join(prefix, "bin", "fauna-sync-agent"), os.X_OK), \
                "Sync agent not executable"
        finally:
            rc = subprocess.run(
                ["bash", install_sh, "--uninstall"],
                capture_output=True, text=True, timeout=30,
                cwd=repo_root, env=env,
            )
            assert rc.returncode == 0, f"Uninstall failed: {rc.stderr}"

        assert not os.path.exists(os.path.join(prefix, "bin", "fauna-desktop")), \
            "Binary still exists after uninstall"
        assert not os.path.exists(os.path.join(prefix, "bin", "fauna-sync-agent")), \
            "Sync agent still exists after uninstall"
        assert not os.path.exists(
            os.path.join(prefix, "share", "applications", "social.fauna.fauna.desktop")
        ), ".desktop still exists after uninstall"
        shutil.rmtree(prefix, ignore_errors=True)

    @pytest.mark.parametrize("has_fusermount3", [False, True])
    def test_install_names_the_fuse3_package_only_when_fusermount3_is_missing(
        self, has_fusermount3, repo_root, tmp_path
    ):
        """install.sh checks for `fusermount3` and names the package when it is
        missing (installers/linux-desktop.md § Installation Files; on-demand
        folders mount through it — on-demand-files.md § Linux FUSE binding). The
        install itself still succeeds: on-demand is a choice, not a prerequisite.

        Needs no built product: the script installs whatever sits at
        `$CARGO_TARGET_DIR/release/`, so two stand-in files do, and `PATH` is a
        directory holding only the tools the install path runs — with or
        without a `fusermount3` in it.
        """
        target = tmp_path / "target"
        (target / "release").mkdir(parents=True)
        for name in ("fauna-desktop", "fauna-sync-agent"):
            (target / "release" / name).write_text("#!/bin/sh\n")

        tools = tmp_path / "bin"
        tools.mkdir()
        for tool in ("install", "dirname", "id"):
            found = shutil.which(tool)
            assert found, f"{tool} is not on this box's PATH"
            os.symlink(found, tools / tool)
        if has_fusermount3:
            (tools / "fusermount3").write_text("#!/bin/sh\n")
            (tools / "fusermount3").chmod(0o755)

        bash = shutil.which("bash")
        install_sh = os.path.join(repo_root, "apps", "fauna-linux", "install.sh")
        rc = subprocess.run(
            [bash, install_sh],
            capture_output=True, text=True, timeout=30, cwd=repo_root,
            env={
                "PATH": str(tools),
                "HOME": str(tmp_path / "home"),
                "PREFIX": str(tmp_path / "prefix"),
                "CARGO_TARGET_DIR": str(target),
            },
        )
        assert rc.returncode == 0, f"Install failed: {rc.stdout}{rc.stderr}"
        assert (tmp_path / "prefix" / "bin" / "fauna-sync-agent").is_file()
        if has_fusermount3:
            # The package's name alone would match this test's own tmp path.
            assert "fusermount3 was not found" not in rc.stdout, (
                f"a box that has fusermount3 must not be told to install it: {rc.stdout!r}"
            )
        else:
            assert "fusermount3" in rc.stdout and "fuse3 package" in rc.stdout, (
                f"a box without fusermount3 must be told which package it needs: {rc.stdout!r}"
            )

    def test_uninstall_removes_the_invoking_users_agent_unit(
        self, desktop_binary, agent_binary, repo_root,
    ):
        """install.sh --uninstall runs AS the user, so it can (and must) remove
        that user's client-written systemd unit — the one channel that can.
        Root-scope channels rely on the unit's ConditionFileIsExecutable line
        instead (linux-desktop.md § Uninstall)."""
        prefix = tempfile.mkdtemp(prefix="fauna-e2e-shell-unit-")
        config_home = tempfile.mkdtemp(prefix="fauna-e2e-xdg-")
        install_sh = os.path.join(repo_root, "apps", "fauna-linux", "install.sh")
        # Drive removal through uninstall.sh — it must stay a working alias
        # for install.sh --uninstall (it once duplicated the logic and drifted).
        uninstall_sh = os.path.join(repo_root, "apps", "fauna-linux", "uninstall.sh")
        env = os.environ.copy()
        env["PREFIX"] = prefix
        env["XDG_CONFIG_HOME"] = config_home

        unit_dir = os.path.join(config_home, "systemd", "user")
        os.makedirs(unit_dir, exist_ok=True)
        unit_file = os.path.join(unit_dir, "fauna-sync-agent.service")
        with open(unit_file, "w") as f:
            f.write("[Unit]\nDescription=Fauna per-user sync agent\n")

        try:
            rc = subprocess.run(
                ["bash", install_sh],
                capture_output=True, text=True, timeout=30,
                cwd=repo_root, env=env,
            )
            assert rc.returncode == 0, f"Install failed: {rc.stderr}"

            rc = subprocess.run(
                ["bash", uninstall_sh],
                capture_output=True, text=True, timeout=30,
                cwd=repo_root, env=env,
            )
            assert rc.returncode == 0, f"Uninstall failed: {rc.stderr}"
            assert not os.path.exists(unit_file), \
                "User unit not removed by user-mode uninstall"
            assert not os.path.exists(os.path.join(prefix, "bin", "fauna-sync-agent")), \
                "Sync agent still installed after uninstall.sh"
        finally:
            shutil.rmtree(prefix, ignore_errors=True)
            shutil.rmtree(config_home, ignore_errors=True)

    @needs_display
    def test_binary_launches(self, desktop_binary):
        assert launch_and_check(desktop_binary, timeout=2), \
            "Desktop binary crashed on launch"


class TestUninstallPreservesData:
    """Uninstall must not touch user config in ~/.config/fauna/."""

    def test_config_survives_uninstall(self, desktop_binary, repo_root):
        prefix = tempfile.mkdtemp(prefix="fauna-e2e-data-")
        install_sh = os.path.join(repo_root, "apps", "fauna-linux", "install.sh")
        env = os.environ.copy()
        env["PREFIX"] = prefix

        rc = subprocess.run(
            ["bash", install_sh],
            capture_output=True, text=True, timeout=30,
            cwd=repo_root, env=env,
        )
        assert rc.returncode == 0, "Install failed"

        config_dir = os.path.join(
            os.environ.get("XDG_CONFIG_HOME",
                           os.path.join(os.path.expanduser("~"), ".config")),
            "fauna",
        )
        os.makedirs(config_dir, exist_ok=True)
        marker = os.path.join(config_dir, "e2e-test-marker.txt")
        with open(marker, "w") as f:
            f.write("preserve me")

        try:
            rc = subprocess.run(
                ["bash", install_sh, "--uninstall"],
                capture_output=True, text=True, timeout=30,
                cwd=repo_root, env=env,
            )
            assert rc.returncode == 0, "Uninstall failed"
            assert os.path.exists(marker), \
                "User config was deleted during uninstall"
        finally:
            if os.path.exists(marker):
                os.remove(marker)
            shutil.rmtree(prefix, ignore_errors=True)


@needs_root
class TestDebPackage:
    """Install/uninstall a .deb package built from the release binary."""

    def test_install_and_uninstall(self, deb_path):
        result = subprocess.run(
            ["dpkg", "-i", deb_path],
            capture_output=True, text=True, timeout=60,
        )
        assert result.returncode == 0, f"dpkg -i failed: {result.stderr}"

        try:
            query = subprocess.run(
                ["dpkg", "-l", "fauna-desktop"],
                capture_output=True, text=True,
            )
            assert query.returncode == 0, "fauna-linux not in dpkg database"
            assert os.path.isfile("/usr/bin/fauna-desktop"), "Binary not at /usr/bin/fauna"
            assert os.path.isfile("/usr/bin/fauna-sync-agent"), \
                "Sync agent not at /usr/bin/fauna-sync-agent"
            assert os.path.isfile(
                "/usr/share/applications/social.fauna.fauna.desktop"
            ), ".desktop not installed"
        finally:
            result = subprocess.run(
                ["dpkg", "-r", "fauna-desktop"],
                capture_output=True, text=True, timeout=60,
            )
            assert result.returncode == 0, f"dpkg -r failed: {result.stderr}"

        assert not os.path.isfile("/usr/bin/fauna-desktop"), \
            "Binary still exists after dpkg -r"


@pytest.mark.skip(reason="Snap packaging not production-ready")
@needs_root
@needs_snapd
class TestSnap:
    """Install/uninstall snap package (desktop only, no sync)."""

    def test_install_and_uninstall(self, repo_root):
        app_dir = os.path.join(repo_root, "apps", "fauna-linux")
        result = subprocess.run(
            ["snapcraft"],
            capture_output=True, text=True, timeout=1200,
            cwd=app_dir,
        )
        if result.returncode != 0:
            pytest.fail(f"snapcraft failed: {result.stderr[-2000:]}")

        snaps = glob.glob(os.path.join(app_dir, "*.snap"))
        assert snaps, "No .snap file produced"
        snap_file = snaps[0]

        try:
            result = subprocess.run(
                ["snap", "install", "--dangerous", snap_file],
                capture_output=True, text=True, timeout=60,
            )
            assert result.returncode == 0, f"snap install failed: {result.stderr}"
            result = subprocess.run(
                ["snap", "list", "fauna"],
                capture_output=True, text=True,
            )
            assert result.returncode == 0, "fauna not in snap list"
        finally:
            subprocess.run(
                ["snap", "remove", "fauna"],
                capture_output=True, text=True, timeout=60,
            )

        result = subprocess.run(
            ["snap", "list", "fauna"],
            capture_output=True, text=True,
        )
        assert result.returncode != 0, "fauna still in snap list after remove"


@pytest.mark.skip(reason="Flatpak packaging not production-ready")
@needs_flatpak
class TestFlatpak:
    """Install/uninstall flatpak (desktop only, no sync)."""

    def test_install_and_uninstall(self, repo_root):
        flatpak_dir = os.path.join(repo_root, "apps", "fauna-linux", "packaging", "flatpak")
        manifest = os.path.join(flatpak_dir, "social.fauna.fauna.yml")
        build_dir = os.path.join(repo_root, "build", "linux-installer", "flatpak-build")
        repo_dir = os.path.join(repo_root, "build", "linux-installer", "flatpak-repo")
        app_id = "social.fauna.fauna"

        result = subprocess.run(
            [
                "flatpak-builder", "--force-clean",
                "--user", "--install",
                "--repo", repo_dir,
                build_dir, manifest,
            ],
            capture_output=True, text=True, timeout=1200,
            cwd=repo_root,
        )
        if result.returncode != 0:
            pytest.fail(f"flatpak-builder failed: {result.stderr[-2000:]}")

        try:
            result = subprocess.run(
                ["flatpak", "list", "--user", "--app"],
                capture_output=True, text=True,
            )
            assert app_id in result.stdout, f"{app_id} not in flatpak list"
        finally:
            subprocess.run(
                ["flatpak", "uninstall", "--user", "-y", app_id],
                capture_output=True, text=True, timeout=60,
            )

        result = subprocess.run(
            ["flatpak", "list", "--user", "--app"],
            capture_output=True, text=True,
        )
        assert app_id not in result.stdout, f"{app_id} still installed"


@pytest.mark.skip(reason="AppImage not production-ready — no bundled libraries")
class TestAppImage:
    """Verify AppImage directory structure and binary launches."""

    def test_appdir_structure(self, desktop_binary, agent_binary, repo_root):
        appdir = tempfile.mkdtemp(prefix="fauna-e2e-appimage-")
        appimage_src = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "appimage",
        )

        try:
            usr_bin = os.path.join(appdir, "usr", "bin")
            os.makedirs(usr_bin, exist_ok=True)
            shutil.copy2(desktop_binary, os.path.join(usr_bin, "fauna-desktop"))
            os.chmod(os.path.join(usr_bin, "fauna-desktop"), 0o755)
            shutil.copy2(agent_binary, os.path.join(usr_bin, "fauna-sync-agent"))
            os.chmod(os.path.join(usr_bin, "fauna-sync-agent"), 0o755)

            apprun_src = os.path.join(appimage_src, "AppRun")
            apprun = os.path.join(appdir, "AppRun")
            shutil.copy2(apprun_src, apprun)
            os.chmod(apprun, 0o755)

            # The AppDir stages the ONE shared desktop entry under the app-id
            # basename — the AppDir used to carry a byte-identical copy of its
            # own, which is exactly the drift the 2026-08-22 convergence removed
            # (installers/linux-desktop.md § Desktop entry).
            desktop_src = os.path.join(
                repo_root, "apps", "fauna-linux", "packaging",
                "social.fauna.fauna.desktop",
            )
            shutil.copy2(
                desktop_src, os.path.join(appdir, "social.fauna.fauna.desktop")
            )

            assert os.path.isfile(apprun), "AppRun missing"
            assert os.access(apprun, os.X_OK), "AppRun not executable"
            assert os.path.isfile(os.path.join(usr_bin, "fauna-desktop")), \
                "Binary missing from AppDir"
            assert os.path.isfile(os.path.join(usr_bin, "fauna-sync-agent")), \
                "Sync agent missing from AppDir"
        finally:
            shutil.rmtree(appdir, ignore_errors=True)

    @needs_display
    def test_apprun_launches(self, desktop_binary, repo_root):
        appdir = tempfile.mkdtemp(prefix="fauna-e2e-appimage-run-")
        appimage_src = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "appimage",
        )

        try:
            usr_bin = os.path.join(appdir, "usr", "bin")
            os.makedirs(usr_bin, exist_ok=True)
            shutil.copy2(desktop_binary, os.path.join(usr_bin, "fauna-desktop"))
            os.chmod(os.path.join(usr_bin, "fauna-desktop"), 0o755)

            apprun = os.path.join(appdir, "AppRun")
            shutil.copy2(os.path.join(appimage_src, "AppRun"), apprun)
            os.chmod(apprun, 0o755)

            assert launch_and_check(apprun, timeout=2), \
                "AppImage binary crashed on launch"
        finally:
            shutil.rmtree(appdir, ignore_errors=True)


class TestAppRunDispatch:
    """AppRun's `--sync-agent` dispatch arm — the AppImage lifecycle mechanism.

    The systemd user unit written by the app under an AppImage install execs
    the stable `.AppImage` file itself with `--sync-agent` (the per-run FUSE
    mount path is throwaway), and AppRun dispatches to the bundled agent.
    Headless mechanism test with a stub agent — no display, no real binaries.
    """

    def _stage(self, repo_root):
        appdir = tempfile.mkdtemp(prefix="fauna-e2e-apprun-")
        appimage_src = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "appimage",
        )
        usr_bin = os.path.join(appdir, "usr", "bin")
        os.makedirs(usr_bin, exist_ok=True)
        for name in ("fauna-desktop", "fauna-sync-agent"):
            stub = os.path.join(usr_bin, name)
            with open(stub, "w") as f:
                f.write(f'#!/bin/sh\necho "STUB {name} $@"\n')
            os.chmod(stub, 0o755)
        apprun = os.path.join(appdir, "AppRun")
        shutil.copy2(os.path.join(appimage_src, "AppRun"), apprun)
        os.chmod(apprun, 0o755)
        return appdir, apprun

    def test_sync_agent_arg_dispatches_to_the_bundled_agent(self, repo_root):
        appdir, apprun = self._stage(repo_root)
        try:
            result = subprocess.run(
                [apprun, "--sync-agent"],
                capture_output=True, text=True, timeout=10,
            )
            assert result.returncode == 0, f"AppRun --sync-agent failed: {result.stderr}"
            assert "STUB fauna-sync-agent" in result.stdout, \
                f"--sync-agent did not dispatch to the agent: {result.stdout!r}"
        finally:
            shutil.rmtree(appdir, ignore_errors=True)

    def test_default_invocation_still_launches_the_app(self, repo_root):
        appdir, apprun = self._stage(repo_root)
        try:
            result = subprocess.run(
                [apprun, "--version"],
                capture_output=True, text=True, timeout=10,
            )
            assert result.returncode == 0, f"AppRun failed: {result.stderr}"
            assert "STUB fauna-desktop --version" in result.stdout, \
                f"default invocation did not reach the app: {result.stdout!r}"
        finally:
            shutil.rmtree(appdir, ignore_errors=True)


class TestDesktopFileValidation:
    """Validate the .desktop file with desktop-file-validate."""

    def test_desktop_file_valid(self, repo_root):
        desktop_file = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "social.fauna.fauna.desktop"
        )
        result = subprocess.run(
            ["desktop-file-validate", desktop_file],
            capture_output=True, text=True,
        )
        assert result.returncode == 0, \
            f"desktop-file-validate failed:\n{result.stdout}{result.stderr}"


class TestRunningWindowWmClass:
    """The shell associates a window with its desktop entry's icon two ways:
    GApplication id == entry basename (pinned headlessly by
    `packaging_identity_test.rs::the_desktop_entry_basename_and_the_launchable_are_the_app_id`),
    or the window's own WM_CLASS == the entry's `StartupWMClass`. The entry's
    declared side is pinned by
    `packaging_identity_test.rs::the_desktop_entry_declares_startup_wm_class`;
    this is the other half — what the RUNNING binary actually sets, which GTK
    derives from the program name (not guaranteed to equal the binary name),
    so nothing before this asserted the two sides actually agree
    (installers/linux-desktop.md § Installation Files).
    """

    def test_running_wm_class_matches_the_desktop_entrys_startup_wm_class(
        self, desktop_binary, repo_root,
    ):
        for tool in ("Xvfb", "xwininfo", "xprop"):
            if shutil.which(tool) is None:
                skip_environment(f"{tool} not installed (no x11-utils on this box)")

        desktop_file = os.path.join(
            repo_root, "apps", "fauna-linux", "packaging", "social.fauna.fauna.desktop"
        )
        expected = None
        with open(desktop_file) as f:
            for line in f:
                if line.startswith("StartupWMClass="):
                    expected = line.split("=", 1)[1].strip()
                    break
        assert expected, f"{desktop_file} declares no StartupWMClass"

        xvfb_proc, display = _start_private_xvfb()
        try:
            env = os.environ.copy()
            env["DISPLAY"] = display
            # Same headless-render env the shared e2e driver forces
            # (drivers/linux.py::_headless_render_cmd_env) — GDK must bind the
            # private Xvfb, never a Wayland compositor or the visible :0.
            env["GDK_BACKEND"] = "x11"
            env.pop("WAYLAND_DISPLAY", None)
            env["GSK_RENDERER"] = "cairo"
            env["XDG_CONFIG_HOME"] = tempfile.mkdtemp(prefix="fauna-e2e-wmclass-cfg-")
            env["XDG_DATA_HOME"] = tempfile.mkdtemp(prefix="fauna-e2e-wmclass-data-")

            app_log_path = tempfile.mktemp(prefix="fauna-e2e-wmclass-app-", suffix=".log")
            app_log = open(app_log_path, "w")
            app_proc = subprocess.Popen(
                [desktop_binary],
                env=env, stdout=app_log, stderr=app_log,
                **popen_group_kwargs(),
            )
            reap_descendants_of(app_proc.pid)
            try:
                if app_proc.poll() is not None:
                    pytest.fail(
                        f"fauna-desktop exited early (rc={app_proc.returncode}). "
                        f"log tail: {open(app_log_path).read()[-800:]}"
                    )
                pairs = _wm_class_pairs(display)
                observed = {value for pair in pairs for value in pair if value}
                assert expected in observed, (
                    f"running window's WM_CLASS pairs are {pairs!r}, none of "
                    f"which contain the desktop entry's StartupWMClass="
                    f"{expected!r} ({desktop_file}) — the shell cannot "
                    f"associate the window with its icon. app log tail: "
                    f"{open(app_log_path).read()[-800:]}"
                )
            finally:
                app_proc.send_signal(signal.SIGTERM)
                try:
                    app_proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app_proc.kill()
                    app_proc.wait()
                app_log.close()
        finally:
            xvfb_proc.terminate()
            try:
                xvfb_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                xvfb_proc.kill()
                xvfb_proc.wait()


class TestVersionFlag:
    """Verify --version flag works without starting GTK."""

    def test_version_output(self, desktop_binary):
        result = subprocess.run(
            [desktop_binary, "--version"],
            capture_output=True, text=True, timeout=5,
        )
        assert result.returncode == 0, f"--version failed: {result.stderr}"
        assert "fauna-desktop" in result.stdout, \
            f"Expected 'fauna-desktop' in output, got: {result.stdout}"


@needs_root
class TestNestInstaller:
    """Install/uninstall fauna-nest via the new installer script."""

    @pytest.fixture(autouse=True)
    def _cleanup_stale_nest(self, nest_binary, repo_root):
        """Clean up any leftover nest installation from a prior interrupted run."""
        install_sh = os.path.join(repo_root, "bins", "fauna-nest", "install.sh")
        if os.path.isfile("/usr/local/bin/fauna-nest"):
            subprocess.run(
                ["bash", install_sh, "--uninstall", "--remove-data"],
                capture_output=True, timeout=30,
            )
        yield
        if os.path.isfile("/usr/local/bin/fauna-nest"):
            subprocess.run(
                ["bash", install_sh, "--uninstall", "--remove-data"],
                capture_output=True, timeout=30,
            )

    def test_install_and_uninstall(self, nest_binary, repo_root):
        install_sh = os.path.join(repo_root, "bins", "fauna-nest", "install.sh")

        # Skip if fauna user already exists
        result = subprocess.run(["id", "fauna"], capture_output=True)
        if result.returncode == 0:
            pytest.skip("fauna user already exists — skipping to avoid clobbering")

        result = subprocess.run(
            [
                "bash", install_sh,
                "--non-interactive",
                "--local-binary", nest_binary,
                "--mode", "public",
                "--bind", "127.0.0.1:3199",
                "--domain", "e2e-test.local",
            ],
            capture_output=True, text=True, timeout=60,
        )
        assert result.returncode == 0, f"Install failed: {result.stderr}"

        try:
            assert os.path.isfile("/usr/local/bin/fauna-nest"), \
                "Binary not installed"

            assert os.path.isfile("/etc/fauna/nest.toml"), \
                "Config not generated"
            with open("/etc/fauna/nest.toml", "rb") as f:
                parsed_config = tomllib.load(f)
            assert parsed_config["nest"]["mode"] == "public"
            assert parsed_config["nest"]["listen"] == "127.0.0.1:3199"
            assert parsed_config["nest"]["domain"] == "e2e-test.local", (
                "domain must land under [nest], not get lost in [acme] — "
                f"parsed config: {parsed_config}"
            )

            result = subprocess.run(
                ["id", "fauna"], capture_output=True, text=True,
            )
            assert result.returncode == 0, "fauna user not created"

            assert os.path.isdir("/var/lib/fauna"), \
                "/var/lib/fauna not created"
            import pwd
            st = os.stat("/var/lib/fauna")
            owner = pwd.getpwuid(st.st_uid).pw_name
            assert owner == "fauna", \
                f"/var/lib/fauna owned by {owner}, expected fauna"

            assert os.path.isfile("/etc/systemd/system/fauna-nest.service"), \
                "Systemd unit not installed"

            subprocess.run(
                ["systemctl", "start", "fauna-nest"],
                capture_output=True, timeout=10,
            )
            assert wait_for_http("http://127.0.0.1:3199/", timeout=10), \
                "Nest did not respond within 10 seconds"
        finally:
            result = subprocess.run(
                ["bash", install_sh, "--uninstall", "--remove-data"],
                capture_output=True, text=True, timeout=60,
            )
            assert result.returncode == 0, f"Uninstall failed: {result.stderr}"

        assert not os.path.isfile("/usr/local/bin/fauna-nest"), \
            "Binary still exists"
        assert not os.path.isfile("/etc/systemd/system/fauna-nest.service"), \
            "Systemd unit still exists"
        assert not os.path.isdir("/etc/fauna"), \
            "/etc/fauna still exists"
        assert not os.path.isdir("/var/lib/fauna"), \
            "/var/lib/fauna still exists (--remove-data was set)"

        result = subprocess.run(["id", "fauna"], capture_output=True)
        assert result.returncode != 0, "fauna user still exists"

    def test_uninstall_preserves_data_by_default(self, nest_binary, repo_root):
        install_sh = os.path.join(repo_root, "bins", "fauna-nest", "install.sh")

        result = subprocess.run(["id", "fauna"], capture_output=True)
        if result.returncode == 0:
            pytest.skip("fauna user already exists")

        result = subprocess.run(
            [
                "bash", install_sh,
                "--non-interactive",
                "--local-binary", nest_binary,
                "--mode", "public",
                "--bind", "127.0.0.1:3199",
                "--domain", "e2e-test.local",
            ],
            capture_output=True, text=True, timeout=60,
        )
        assert result.returncode == 0, "Install failed"

        try:
            marker = "/var/lib/fauna/e2e-marker.txt"
            with open(marker, "w") as f:
                f.write("preserve me")

            result = subprocess.run(
                ["bash", install_sh, "--uninstall"],
                capture_output=True, text=True, timeout=60,
            )
            assert result.returncode == 0, "Uninstall failed"

            assert os.path.isdir("/var/lib/fauna"), \
                "/var/lib/fauna was removed (should be preserved)"
            assert os.path.isfile(marker), \
                "Data marker was removed"
        finally:
            if os.path.isdir("/var/lib/fauna"):
                shutil.rmtree("/var/lib/fauna")
            subprocess.run(
                ["bash", install_sh, "--uninstall", "--remove-data"],
                capture_output=True, timeout=30,
            )
