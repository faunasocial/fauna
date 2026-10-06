"""Pin `door-deploy-receive.sh` — the forced command behind the front door's deploy key.

The wrapper is the only thing standing between a leaked deploy key and the box
that serves the site, the SPA and the software-update feed. Its contract
(`docs/goal/architecture/front-door.md` § The box) is that the key can drop a
release into `/srv/fauna/{site,app}/releases/<stamp>/`, flip `current` onto
one, and do nothing else — no write outside those directories and no read of
anything at all.

`SSH_ORIGINAL_COMMAND` is attacker-controlled text, so every case below drives
the wrapper with a crafted command and asserts three things about a refusal:
it exits non-zero, rsync was never started, and nothing was created outside the
releases dirs. The escape target is the deploy user's own `.ssh/` directory on
purpose: that is a place an unprivileged deploy user really can write, and
replacing `authorized_keys` there removes the forced command altogether.

The wrapper hard-codes its root and its rsync path (neither is a knob anybody
chooses), so the harness runs a copy with exactly those two constants pointed
into a scratch directory. One round trip uses a real GNU rsync client, which
is what pins the allowlisted option string to what `rsync -az --delete`
actually sends rather than to what this file believes it sends.
"""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
from pathlib import Path

import pytest


def _gnu_userland() -> bool:
    """The wrapper targets the box — Debian, POSIX paths, GNU coreutils (`mv -T`
    is what makes the flip atomic, and BSD `mv` has no such flag)."""
    if os.name == "nt" or shutil.which("sh") is None:
        return False
    probe = subprocess.run(["mv", "--version"], capture_output=True, text=True)
    return probe.returncode == 0 and "GNU coreutils" in probe.stdout


pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(
        not _gnu_userland(),
        reason="the deploy wrapper runs on a Debian box: needs sh + GNU coreutils",
    ),
]

WRAPPER = (
    Path(__file__).resolve().parents[2]
    / "services/fauna-front-door/deploy/door-deploy-receive.sh"
)
ROOT_LINE = "ROOT=/srv/fauna\n"
RSYNC_LINE = "RSYNC=/usr/bin/rsync\n"
# What `rsync -az --delete` sends (rsync 3.4.1); the letters after `e.` are the
# client's capability string and vary by version.
OPTS = "-logDtprze.iLsfxCIvu"
STAMP = "20260919T120000Z-abcdef123456"


class Box:
    """A scratch stand-in for the box: the content root, a deploy-user home
    beside it, a secret the key must never read, and a recording fake rsync."""

    def __init__(self, tmp: Path, rsync: Path | None = None):
        tmp = tmp.resolve()
        self.root = tmp / "srv" / "fauna"
        for kind in ("site", "app"):
            (self.root / kind / "releases").mkdir(parents=True)
        self.home_ssh = tmp / "home" / "deploy" / ".ssh"
        self.secret = tmp / "secret"
        self.secret.write_text("not for the deploy key\n")
        self.rsync_log = tmp / "rsync-argv.txt"
        bindir = tmp / "bin"
        bindir.mkdir()
        fake = bindir / "rsync"
        fake.write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{self.rsync_log}"\n')
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        self.bindir = bindir
        text = WRAPPER.read_text()
        text = text.replace(ROOT_LINE, f"ROOT={self.root}\n")
        text = text.replace(RSYNC_LINE, f"RSYNC={rsync or fake}\n")
        self.wrapper = tmp / "door-deploy-receive"
        self.wrapper.write_text(text)
        self.wrapper.chmod(0o755)

    def run(self, command: str | None) -> subprocess.CompletedProcess:
        env = {"PATH": f"{self.bindir}{os.pathsep}{os.environ['PATH']}"}
        if command is not None:
            env["SSH_ORIGINAL_COMMAND"] = command
        return subprocess.run(
            ["sh", str(self.wrapper)], env=env, capture_output=True, text=True,
            stdin=subprocess.DEVNULL, timeout=30,
        )

    def release(self, kind: str = "site", stamp: str = STAMP) -> Path:
        return self.root / kind / "releases" / stamp

    def assert_refused(self, command: str | None) -> None:
        result = self.run(command)
        assert result.returncode != 0, f"ACCEPTED: {command!r}\n{result.stderr}"
        assert not self.rsync_log.exists(), (
            f"rsync was started for {command!r}: {self.rsync_log.read_text()!r}"
        )
        assert not self.home_ssh.exists(), f"{command!r} created {self.home_ssh}"


@pytest.fixture
def box(tmp_path: Path) -> Box:
    return Box(tmp_path)


def test_the_two_hard_coded_constants_are_where_the_harness_expects_them():
    text = WRAPPER.read_text()
    assert text.count(ROOT_LINE) == 1
    assert text.count(RSYNC_LINE) == 1


def test_a_release_upload_is_accepted_and_its_argv_is_rebuilt(box: Box):
    target = f"{box.release()}/"
    result = box.run(f"rsync --server {OPTS} --delete . {target}")
    assert result.returncode == 0, result.stderr
    assert box.release().is_dir()
    assert box.rsync_log.read_text().splitlines() == [
        "--server", OPTS, "--delete",
        "--munge-links", "--no-specials", "--no-devices", ".", target,
    ]


def test_an_app_release_upload_is_accepted(box: Box):
    result = box.run(f"rsync --server {OPTS} --delete . {box.release('app')}/")
    assert result.returncode == 0, result.stderr
    assert box.release("app").is_dir()


def test_traversal_out_of_the_releases_dir_is_refused(box: Box):
    escape = f"{box.root}/site/releases/../../../../home/deploy/.ssh/"
    assert Path(os.path.normpath(escape)) == box.home_ssh
    box.assert_refused(f"rsync --server {OPTS} --delete . {escape}")


@pytest.mark.parametrize("stamp", ["..", ".", "a.b", "a/b", "", "UPPER", "a b", "*"])
def test_a_stamp_outside_the_stamp_alphabet_is_refused(box: Box, stamp: str):
    before = sorted(p.name for p in (box.root / "site" / "releases").iterdir())
    box.assert_refused(
        f"rsync --server {OPTS} --delete . {box.root}/site/releases/{stamp}/"
    )
    after = sorted(p.name for p in (box.root / "site" / "releases").iterdir())
    assert after == before


@pytest.mark.parametrize(
    "target",
    [
        "{root}/site/releases/{stamp}",  # no trailing slash
        "{root}/site/current/",
        "{root}/site/",
        "{root}/",
        "{root}/other/releases/{stamp}/",
        "/{stamp}/",
        "{stamp}/",
    ],
)
def test_a_target_that_is_not_exactly_a_release_dir_is_refused(box: Box, target: str):
    box.assert_refused(
        f"rsync --server {OPTS} --delete . "
        + target.format(root=box.root, stamp=STAMP)
    )


def test_the_read_direction_is_refused(box: Box):
    valid = f"{box.release()}/"
    # One source: the source IS the last token, so a last-token check sees it.
    box.assert_refused(f"rsync --server --sender {OPTS} . {box.secret}")
    # Traversal dressed as a release path.
    box.assert_refused(
        f"rsync --server --sender {OPTS} . {box.root}/site/releases/../../../secret"
    )
    # Several sources: only the last is a release path; the others are the read.
    box.assert_refused(f"rsync --server --sender {OPTS} . {box.secret} {valid}")
    # The same read shaped to keep the token count of a real upload.
    box.assert_refused(f"rsync --server --sender {OPTS} . {valid}")
    box.assert_refused(f"rsync --server {OPTS} --delete {box.secret} {valid}")
    box.assert_refused(f"rsync --server {OPTS} --sender . {valid}")


def test_daemon_mode_is_refused(box: Box):
    valid = f"{box.release()}/"
    box.assert_refused(f"rsync --server --daemon . {valid}")
    box.assert_refused(f"rsync --server --daemon {OPTS} --delete . {valid}")
    box.assert_refused(f"rsync --daemon {OPTS} --delete . {valid}")


@pytest.mark.parametrize(
    "extra",
    [
        "--log-file={outside}/log",
        "--backup-dir={outside}",
        "--partial-dir={outside}",
        "--temp-dir={outside}",
        "--link-dest={outside}",
        "--copy-dest={outside}",
        "--compare-dest={outside}",
        "--files-from={outside}/secret",
        "--keep-dirlinks",
        "--remove-source-files",
    ],
)
def test_a_server_option_pointing_outside_the_root_is_refused(box: Box, extra: str):
    valid = f"{box.release()}/"
    extra = extra.format(outside=box.secret.parent)
    box.assert_refused(f"rsync --server {OPTS} --delete {extra} . {valid}")
    # …and in the slot `--delete` normally holds, keeping the token count.
    box.assert_refused(f"rsync --server {OPTS} {extra} . {valid}")


@pytest.mark.parametrize(
    "opts",
    [
        "-KlogDtprze.iLsfxCIvu",  # keep-dirlinks: follow a planted directory link
        "-logDtprzKe.iLsfxCIvu",
        "-logDtprze.iLs-fx",
        "-logDtprze.iLs=x",
        "-logDtprze",
        "-e.iLsfxCIvu",
        "logDtprze",  # no leading dash: all letters, and not an option word at all
        "K",
        "--log-file=/x",
    ],
)
def test_any_option_string_but_the_one_a_deploy_sends_is_refused(box: Box, opts: str):
    box.assert_refused(f"rsync --server {opts} --delete . {box.release()}/")


def test_a_release_path_that_is_a_planted_symlink_is_refused(box: Box):
    outside = box.secret.parent / "outside"
    outside.mkdir()
    box.release(stamp="planted").symlink_to(outside)
    box.assert_refused(
        f"rsync --server {OPTS} --delete . {box.release(stamp='planted')}/"
    )
    # A link inside an earlier release is unreachable: a target is one stamp deep.
    box.release().mkdir()
    (box.release() / "link").symlink_to(outside)
    box.assert_refused(f"rsync --server {OPTS} --delete . {box.release()}/link/")
    # A dangling link must not be materialised into the place it points at.
    box.release(stamp="dangling").symlink_to(box.home_ssh)
    box.assert_refused(
        f"rsync --server {OPTS} --delete . {box.release(stamp='dangling')}/"
    )


def test_a_releases_parent_that_is_a_symlink_is_refused(box: Box):
    """The `releases` dir itself planted as a link out of the root.

    Nothing the deploy key can run reaches this state, so it is defense in
    depth rather than a hole — and that is exactly why it needs a pin: it is
    the one thing `require_real`'s `readlink -f` clause refuses on its own, so
    without this case that clause can be deleted with every other test still
    green.
    """
    outside = box.secret.parent / "outside"
    outside.mkdir()
    for kind in ("site", "app"):
        shutil.rmtree(box.root / kind / "releases")
        (box.root / kind / "releases").symlink_to(outside)
    box.assert_refused(f"rsync --server {OPTS} --delete . {box.release()}/")
    box.assert_refused("flip site " + STAMP)
    # …and nothing was created through the planted parent on the way to the
    # refusal: the parent is checked before `mkdir -p` runs.
    assert list(outside.iterdir()) == []


def test_a_kind_dir_that_is_a_symlink_is_refused(box: Box):
    """A link one component HIGHER: `site` itself, with a real `releases`
    inside it.

    This is the case only `require_real`'s `readlink -f` clause answers — the
    `-L` test looks at the last component alone, and here that component is a
    genuine directory. Without this the clause is deletable with every other
    test still green.
    """
    outside = box.secret.parent / "elsewhere"
    (outside / "releases").mkdir(parents=True)
    shutil.rmtree(box.root / "site")
    (box.root / "site").symlink_to(outside)
    assert (box.root / "site" / "releases").is_dir()
    assert not (box.root / "site" / "releases").is_symlink()
    box.assert_refused(f"rsync --server {OPTS} --delete . {box.release()}/")
    (outside / "releases" / STAMP).mkdir()
    box.assert_refused("flip site " + STAMP)
    assert not (box.root / "site" / "current").exists()


def test_shell_syntax_in_the_command_is_never_interpreted(box: Box):
    valid = f"{box.release()}/"
    pwned = box.secret.parent / "pwned"
    for tail in (f"; touch {pwned}", f"&& touch {pwned}", f"$(touch {pwned})",
                 f"`touch {pwned}`", f"| touch {pwned}"):
        box.assert_refused(f"rsync --server {OPTS} --delete . {valid} {tail}")
        assert not pwned.exists()
    # A glob is matched as text, never expanded against the box's files.
    box.release().mkdir()
    box.assert_refused(f"rsync --server {OPTS} --delete . {box.root}/site/releases/*/")


@pytest.mark.parametrize(
    "command",
    [None, "", "sh", "bash -i", "scp -t /tmp", "cat /etc/passwd", "rsync", "rsync --server",
     "/usr/bin/rsync --server -logDtprze.iLsfxCIvu --delete . /srv/fauna/site/releases/x/"],
)
def test_anything_else_is_refused(box: Box, command: str | None):
    box.assert_refused(command)


def test_flip_moves_current_atomically_and_keeps_three(box: Box):
    stamps = [f"2026090{n}t000000z-r{n}" for n in range(1, 6)]
    for n, stamp in enumerate(stamps):
        box.release(stamp=stamp).mkdir()
        os.utime(box.release(stamp=stamp), (n, n))
    result = box.run(f"flip site {stamps[-1]}")
    assert result.returncode == 0, result.stderr
    assert os.readlink(box.root / "site" / "current") == f"releases/{stamps[-1]}"
    assert sorted(p.name for p in (box.root / "site" / "releases").iterdir()) == stamps[2:]


def test_flip_never_prunes_the_release_it_just_made_live(box: Box):
    stamps = [f"2026090{n}t000000z-r{n}" for n in range(1, 6)]
    for n, stamp in enumerate(stamps):
        box.release(stamp=stamp).mkdir()
        os.utime(box.release(stamp=stamp), (n, n))
    # A rollback: the OLDEST release goes live, outside the newest three.
    assert box.run(f"flip site {stamps[0]}").returncode == 0
    assert box.release(stamp=stamps[0]).is_dir()
    assert os.readlink(box.root / "site" / "current") == f"releases/{stamps[0]}"


def test_flip_refuses_a_kind_that_is_not_site_or_app(box: Box):
    # The directories exist, so only the kind check stands in the way.
    (box.root / "other" / "releases" / STAMP).mkdir(parents=True)
    box.release("app").mkdir()
    box.assert_refused(f"flip other {STAMP}")
    box.assert_refused(f"flip site/../app {STAMP}")
    assert not (box.root / "other" / "current").exists()
    assert not (box.root / "app" / "current").exists()


@pytest.mark.parametrize(
    "command",
    [
        "flip site ..",
        "flip site ../../app",
        "flip site",
        "flip other {stamp}",
        "flip site {stamp} extra",
        "flip site missing",
        "flip site/../app {stamp}",
    ],
)
def test_a_malformed_flip_is_refused(box: Box, command: str):
    box.release().mkdir()
    box.assert_refused(command.format(stamp=STAMP))
    assert not (box.root / "site" / "current").exists()


def test_flip_refuses_a_release_that_is_a_symlink(box: Box):
    outside = box.secret.parent / "outside"
    outside.mkdir()
    box.release(stamp="planted").symlink_to(outside)
    box.assert_refused("flip site planted")
    assert not (box.root / "site" / "current").exists()


def _gnu_rsync() -> str | None:
    path = shutil.which("rsync")
    if path is None:
        return None
    banner = subprocess.run([path, "--version"], capture_output=True, text=True).stdout
    return path if banner.startswith("rsync  version 3.") else None


@pytest.fixture
def real_box(tmp_path: Path):
    rsync = _gnu_rsync()
    if rsync is None:
        pytest.skip("no GNU rsync 3.x on this machine — the box and the deploy runner both have one")
    box = Box(tmp_path, rsync=Path(rsync))
    # A remote shell that is the forced command: whatever the client asks for
    # becomes SSH_ORIGINAL_COMMAND, exactly as sshd hands it over.
    ssh = tmp_path / "forced-ssh"
    ssh.write_text(
        '#!/bin/sh\nshift\nSSH_ORIGINAL_COMMAND="$*" exec sh "%s"\n' % box.wrapper
    )
    ssh.chmod(0o755)
    return box, rsync, ssh


def test_a_real_rsync_deploy_lands_and_flips(real_box, tmp_path: Path):
    box, rsync, ssh = real_box
    dist = tmp_path / "dist"
    (dist / "sub").mkdir(parents=True)
    (dist / "index.html").write_text("<h1>door</h1>\n")
    (dist / "sub" / "a.txt").write_text("a\n")
    # The exact client invocation the deploy workflows use.
    upload = subprocess.run(
        [rsync, "-az", "--delete", "-e", str(ssh), f"{dist}/", f"box:{box.release()}/"],
        capture_output=True, text=True, timeout=60,
    )
    assert upload.returncode == 0, upload.stderr
    assert (box.release() / "sub" / "a.txt").read_text() == "a\n"
    assert box.run(f"flip site {STAMP}").returncode == 0
    assert (box.root / "site" / "current" / "index.html").read_text() == "<h1>door</h1>\n"


def test_a_real_rsync_client_cannot_read_or_escape(real_box, tmp_path: Path):
    box, rsync, ssh = real_box
    dist = tmp_path / "dist"
    dist.mkdir()
    (dist / "authorized_keys").write_text("ssh-ed25519 attacker\n")
    stolen = tmp_path / "stolen"
    pull = subprocess.run(
        [rsync, "-az", "-e", str(ssh), f"box:{box.secret}", str(stolen)],
        capture_output=True, text=True, timeout=60,
    )
    assert pull.returncode != 0
    assert not stolen.exists()
    push = subprocess.run(
        [rsync, "-az", "--delete", "-e", str(ssh), f"{dist}/",
         f"box:{box.root}/site/releases/../../../../home/deploy/.ssh/"],
        capture_output=True, text=True, timeout=60,
    )
    assert push.returncode != 0
    assert not box.home_ssh.exists()


def test_a_real_rsync_push_can_land_neither_a_symlink_nor_a_fifo(real_box, tmp_path: Path):
    """The accepted option word is the CLIENT's, and `rsync -az` keeps symlinks
    (`-l`) and specials (`-D`) — so refusing them has to be the receiver's job.

    The escape target is the same secret the rest of this file uses: a link
    that survives the push is a file the door would later serve from outside
    its docroot, which is the whole composition (front-door.md § The box —
    the deploy key "can replace content ... and **read nothing**").
    """
    box, rsync, ssh = real_box
    dist = tmp_path / "dist"
    dist.mkdir()
    (dist / "index.html").write_text("<h1>door</h1>\n")
    (dist / "leak.txt").symlink_to(box.secret)        # absolute, out of the root
    (dist / "up.txt").symlink_to("../../../../secret")  # relative traversal
    os.mkfifo(dist / "park")

    upload = subprocess.run(
        [rsync, "-az", "--delete", "-e", str(ssh), f"{dist}/", f"box:{box.release()}/"],
        capture_output=True, text=True, timeout=60,
    )
    landed = box.release()
    # An ordinary deploy still round-trips: these are receiver-side refusals of
    # two shapes, not a stricter deploy contract.
    assert (landed / "index.html").read_text() == "<h1>door</h1>\n", upload.stderr
    for name in ("leak.txt", "up.txt"):
        link = landed / name
        if link.is_symlink():
            target = os.readlink(link)
            assert target.startswith("/rsyncd-munged/"), f"{name} -> {target}"
        assert not link.exists(), f"{name} resolves to something on the box"
    assert not (landed / "park").exists(), "a FIFO landed in the release"
    assert box.secret.read_text() == "not for the deploy key\n"
