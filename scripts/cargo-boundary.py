"""The cargo boundary's one home: every COMPILING cargo runs with no network.

A module and a program, stdlib only. On Linux a compiling cargo runs inside a
`bwrap` network namespace that holds nothing but `lo`; on macOS under
`sandbox-exec` with an egress-denying profile (`scripts/kache-mac.sb` by
default); Windows has no unprivileged primitive and gets none. Around the
compile, a `cargo fetch` prelude runs OUTSIDE (it fetches the registry bytes
the compile inside reads), and `cargo run` / `cargo test` / `cargo bench` run
their program outside after compiling it inside, so a test that talks to a
live server keeps its network while no build script or proc-macro ever has
one. Every other cargo subcommand (`fetch`, `install`, `metadata`, …) runs
outside as it is. The namespace is not the whole of "no network": a socket
family the namespace does not contain (`AF_VSOCK` reaches the hypervisor host
from inside it) is refused by a system-call filter the line hands `bwrap` —
`socket` and `socketpair` answer EAFNOSUPPORT for every family but
`SOCKET_FAMILIES`, and `io_uring_setup` (whose socket operation would bypass
the filter) EPERM. The compile inside also may write nothing but its writable
set — the target directory cargo resolves for the command, a private scratch
(on Linux an empty `/tmp`, on macOS a directory under the target exported as
`TMPDIR`), and what the caller adds (the dev front: the compile cache's store
and kache's state directory) — on Linux by a read-only root with those paths
bound, on macOS by the profile's `(deny file-write*)` with one parameter per
writable role (`MAC_ROLES`) — and the runner's file-command variables are unset
before anything runs, so nothing a build script writes is run later outside. The
ruling: docs/goal/architecture/build-compile-cache.md § The boundary's scope
and what OFF means, decisions (1), (5), (6) and (7).

Two placements read this file, one spelling of the boundary in both:

* the dev fleet's build front imports it and calls `argv` for every cargo it
  runs, keeping its own policy (which darwin profile, the compile cache's
  wrap, the slot);
* a CI runner puts `scripts/cargo-shim/cargo` first on the job's PATH, so
  every cargo of the job — a recipe's, a bare step's, one a tool spawns —
  calls back into this program.

Usage:
    cargo-boundary.py [--mac-profile <path>] [--label <text>] [--writable <path>]… [--held] -- cargo …

runs ONE cargo behind the boundary (`--writable`, repeatable, names the whole
writable set in place of the one resolved from cargo — on macOS the target
first, then `fix`'s workspace root, the order `writable_set` resolves them in):
it prints the boundary line
(`[cargo-boundary] \\`<cmd…>\\` compiles with no network (<label>)`) and execs
the bounded command; a cargo that compiles nothing runs as it is. A refusal (a
GitHub-hosted Linux runner without bubblewrap) is printed and exits 1, nothing
run. `--held` is the shim's call and means the same thing (the shim has already
taken its own directory off PATH). When FAUNA_CARGO_BOUNDARY_RECORD names a
file, each bounded compile's command line is appended to it — the job's own
witness reads it.

    cargo-boundary.py runner-setup
    cargo-boundary.py runner-witness

are a CI job's two steps (decision (6), parts (iv)–(v)): `runner-setup` probes
the runner's primitive and writes the shim's directory to `$GITHUB_PATH` and
this program's path, its interpreter and the record's path to `$GITHUB_ENV`;
`runner-witness`, the job's last step, reds the job when the record is empty,
or when the variables, the shim's place first on PATH or the digests
`runner-setup` recorded have changed (decision (7), part (b)).
"""
from __future__ import annotations

import hashlib
import secrets
import json
import os
import platform
import shlex
import shutil
import struct
import subprocess
import sys
import sysconfig

#: The Linux boundary's constant part (decisions (1), (7) and (8)): every
#: namespace unshared but the pid namespace (no network but `lo`; a user
#: namespace of its own, so `/proc/<pid>/root` of a host process answers
#: `Permission denied`) — kache's daemon, which a compile's client may start,
#: must outlive its starter and pass its control socket's peer-pid check, and
#: neither holds across a pid namespace (measured on the primary dev VM
#: 2026-11-05: a daemon started under `--unshare-pid` died with its starter,
#: one kept alive answered `daemon stop` with `the connection's peer PID is
#: not available`) — the line ending with its parent, and a root that is an
#: EMPTY tmpfs, read-only once the line's
#: binds are made (`--remount-ro /`, the last option) — so no pathname unix
#: socket outside the compile's own trees exists inside to be reached (decision
#: (8): a connect passes a read-only bind). On it: `/dev` as before, a fresh
#: `/proc`, `/run`, `/tmp` and `/dev/shm` empty tmpfs (`TMPDIR` the private
#: `/tmp`), `/usr`, `/etc` and `/sys` read-only, and the system-call filter
#: read from file descriptor `SECCOMP_FD` (`seccomp_program`; the caller opens
#: the descriptor, `seccomp_fd_wrapper`). The line's dynamic part goes between
#: this body and its last `BWRAP_TAIL` words (`bwrap_line`): the host's
#: `USR_LINKS`, `$HOME` as an empty directory, the read list read-only
#: (`readable_set`) and the writable set (`writable_set`, the caller's). The
#: compile cache's launcher carries a second copy of this line in Rust
#: (tools/kache-launcher/src/tail.rs, LINUX_BWRAP), pinned equal to this one by
#: test_kache_launcher_front_pins.py.
BWRAP = ("/usr/bin/bwrap", "--unshare-user", "--unshare-ipc", "--unshare-net", "--unshare-uts", "--unshare-cgroup-try",
         "--die-with-parent", "--tmpfs", "/",
         "--dev-bind", "/dev", "/dev", "--proc", "/proc", "--tmpfs", "/run", "--tmpfs", "/tmp",
         "--tmpfs", "/dev/shm", "--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc",
         "--ro-bind", "/sys", "/sys", "--setenv", "TMPDIR", "/tmp", "--seccomp", "9",
         "--remount-ro", "/", "--")
#: How many of `BWRAP`'s words come after its dynamic part: `--remount-ro /
#: --` (the root goes read-only only once every bind's mount point is made).
BWRAP_TAIL = 3
#: The root's entries a merged-`/usr` host keeps as symlinks into `/usr`: each
#: is made inside as the same symlink, or bound read-only where the host has a
#: directory (the Deno build sandbox's `USR_LINKS`, build-system.md § The Deno
#: build sandbox). The launcher's copy is pinned equal.
USR_LINKS = ("/bin", "/lib", "/lib64", "/sbin")
#: The cross linkers `.cargo/config.toml` names by bare name, one per
#: toolchain: a compile for their targets runs them from PATH, so the
#: toolchain's root above the program's `bin` directory is on the read list
#: (decision (8), a path added BY NAME): the Android NDK's prebuilt LLVM, whose
#: clang wrappers read the sysroot beside their `bin`, and llvm-mingw's.
CROSS_LINKERS = ("aarch64-linux-android26-clang", "aarch64-w64-mingw32-clang")
#: The target C tools the justfile exports by absolute path, read by `cc-rs` in
#: a build script inside the boundary: the pinned wasm32 C compiler
#: (`supply-chain/wasm32-cc.toml`; `zstd-sys` compiles C for every web wasm
#: chunk). Each one's install root (above `bin`) joins the read list by name.
TARGET_TOOL_VARS = ("CC_wasm32_unknown_unknown", "AR_wasm32_unknown_unknown")
#: The descriptor `bwrap` reads the filter from — the `9` in `BWRAP`.
SECCOMP_FD = 9
#: The socket families a bounded process may open (decision (1)): the ones the
#: network namespace CONTAINS — unix (the compile cache's wrapper reaches its
#: daemon through one), inet and inet6 (a connect inside is refused, loudly:
#: the namespace has only `lo`) and netlink (glibc's name resolution lists the
#: namespace's addresses through one). Every other family answers
#: EAFNOSUPPORT, as a kernel built without it would: `AF_VSOCK` is not
#: namespaced (measured on the primary dev VM 2026-11-05: a connect to the hypervisor from
#: inside `--unshare-net` reached it), and the rule is the family set, not the
#: one hole. A family a legitimate compile turns out to need is added here BY
#: NAME with its reason in the ruling's status paragraph, never by widening.
SOCKET_FAMILIES = (1, 2, 10, 16)  # AF_UNIX, AF_INET, AF_INET6, AF_NETLINK
#: Per machine (`platform.machine()`): the kernel's audit arch word and the
#: numbers of `socket`, `socketpair` and `io_uring_setup`. A machine not here
#: has no filter, and so no boundary: refused, never a compile outside.
SECCOMP_ARCH = {
    "x86_64": (0xC000003E, 41, 53, 425),
    "aarch64": (0xC00000B7, 198, 199, 425),
}
#: The filter's words — classic BPF over `struct seccomp_data` (the number at
#: 0, the arch at 4, the first argument's low word at 16 on these little-endian
#: machines) — and the kernel's answers.
_BPF_LD_W_ABS, _BPF_JEQ_K, _BPF_JSET_K, _BPF_RET_K = 0x20, 0x15, 0x45, 0x06
_SECCOMP_RET_KILL_PROCESS, _SECCOMP_RET_ERRNO, _SECCOMP_RET_ALLOW = 0x80000000, 0x00050000, 0x7FFF0000
_EPERM, _EAFNOSUPPORT = 1, 97
#: x86-64's x32 ABI marks its numbers with this bit: a call made that way is
#: not one of this filter's numbers and is killed, as a foreign arch is.
_X32_SYSCALL_BIT = 0x40000000
_DATA_NR, _DATA_ARCH, _DATA_ARG0 = 0, 4, 16


def seccomp_program(machine: str) -> bytes:
    """The system-call filter for `machine`, as `bwrap --seccomp` reads it
    (decision (1)): a call from a foreign arch, or by x32's numbers, kills the
    process; `io_uring_setup` answers EPERM (its socket operation would open a
    family past this filter); `socket` and `socketpair` answer EAFNOSUPPORT
    for every family but `SOCKET_FAMILIES`; everything else is allowed. The
    launcher's Rust copy (tail.rs, `seccomp_program`) builds the same bytes,
    pinned equal by test_kache_launcher_front_pins.py."""
    if machine not in SECCOMP_ARCH:
        raise BoundaryRefused(
            f"{LINE} REFUSED — no system-call filter for the machine {machine!r} "
            f"(one of {', '.join(sorted(SECCOMP_ARCH))}); a compile here would keep every socket "
            "family the network namespace does not contain (build-compile-cache.md § The boundary's "
            "scope and what OFF means, decision (1))")
    arch, nr_socket, nr_socketpair, nr_io_uring_setup = SECCOMP_ARCH[machine]
    n = len(SOCKET_FAMILIES)
    program = [
        (_BPF_LD_W_ABS, 0, 0, _DATA_ARCH),
        (_BPF_JEQ_K, 1, 0, arch),  # this machine's arch: past the kill
        (_BPF_RET_K, 0, 0, _SECCOMP_RET_KILL_PROCESS),
        (_BPF_LD_W_ABS, 0, 0, _DATA_NR),
        (_BPF_JSET_K, 0, 1, _X32_SYSCALL_BIT),  # an x32 number: killed
        (_BPF_RET_K, 0, 0, _SECCOMP_RET_KILL_PROCESS),
        (_BPF_JEQ_K, 0, 1, nr_io_uring_setup),
        (_BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EPERM),
        (_BPF_JEQ_K, 1, 0, nr_socket),  # socket: to the family check
        (_BPF_JEQ_K, 0, n + 2, nr_socketpair),  # socketpair: the same; anything else: allowed
        (_BPF_LD_W_ABS, 0, 0, _DATA_ARG0),
    ]
    program += [(_BPF_JEQ_K, n - i, 0, family) for i, family in enumerate(SOCKET_FAMILIES)]
    program += [(_BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EAFNOSUPPORT),
                (_BPF_RET_K, 0, 0, _SECCOMP_RET_ALLOW)]
    return b"".join(struct.pack("<HBBI", *word) for word in program)


def seccomp_fd_wrapper(machine: str | None = None) -> list[str]:
    """The argv prefix that opens the filter on `SECCOMP_FD` and runs the rest
    of the line — one `bash -c` outside `bwrap`, the program's bytes a `printf`
    literal inside it, so the prefix is still one argv a caller runs as it is."""
    program = seccomp_program(machine or platform.machine())
    literal = "".join(f"\\{b:03o}" for b in program)
    return ["/bin/bash", "-c", f"exec {SECCOMP_FD}< <(printf '{literal}'); exec \"$@\"",
            "kache-seccomp"]
#: The runner's file-command variables, unset in the bounded command's one
#: `bash -c` line before anything runs (decision (7), part (a)): nothing cargo
#: does writes them, and a compile that could would steer every later step.
FILE_COMMAND_VARS = ("GITHUB_ENV", "GITHUB_PATH", "GITHUB_OUTPUT", "GITHUB_STATE",
                     "GITHUB_STEP_SUMMARY")
#: Set INSIDE the boundary, so a process can tell it already runs there (a
#: build script's `cargo metadata` reaching this program again takes no second
#: namespace — the kernel refuses a nested `bwrap`). The launcher's tail reads
#: the same name.
TOKEN = "FAUNA_KACHE_INSIDE"
SET_TOKEN = ("/bin/bash", "-c", f'export {TOKEN}="$(readlink /proc/self/ns/net)"; exec "$@"',
             "kache-netns")
#: The macOS profile's file name, and the tree's copy beside this file — the
#: default when the caller names none (a CI runner; a dev machine's front passes
#: its installed copy instead).
MAC_PROFILE_NAME = "kache-mac.sb"
TREE_MAC_PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), MAC_PROFILE_NAME)
#: The darwin profile's immutable snapshots, a directory beside the installed copy:
#: `just kache-launcher-install` writes `<dir>/profiles/kache-mac-<sha256>.sb`
#: before it swaps `<dir>/kache-mac.sb`, and a bounded compile applies the snapshot
#: (`mac_profile_snapshot`), naming it inside as `PROFILE_VAR` — so the kache
#: launcher's tail re-applies the very file the compile runs under even when the
#: profile is reinstalled mid-build. Without it a reinstall made every unit started
#: after the swap die on `sandbox_apply: Operation not permitted`: macOS refuses a
#: nested `sandbox-exec` whose profile differs (build-compile-cache.md § The
#: launcher, Stage 2's design, decision 3). The launcher's Rust copies
#: (tools/kache-launcher/src/tail.rs, PROFILE_VAR and MAC_PROFILE_SNAPSHOTS) are
#: pinned equal by test_kache_launcher_front_pins.py.
MAC_PROFILE_SNAPSHOTS = "profiles"
PROFILE_VAR = "FAUNA_KACHE_PROFILE"


def mac_profile_snapshot(profile: str) -> str:
    """The immutable snapshot of the darwin profile at `profile` —
    `<dirname>/profiles/kache-mac-<sha256 of its bytes>.sb` when that file
    exists — else `profile` itself (an install from before the snapshots, a
    hosted runner's tree copy, a missing file)."""
    try:
        with open(profile, "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
    except OSError:
        return profile
    snap = os.path.join(os.path.dirname(profile), MAC_PROFILE_SNAPSHOTS, f"kache-mac-{digest}.sb")
    return snap if os.path.isfile(snap) else profile
#: The darwin profile's writable roles (decision (7), part (c)), each a
#: `(param …)` the profile allows writes under and `sandbox-exec -D` sets: the
#: target cargo resolves, this compile's scratch (`TMPDIR`), the compile cache's
#: store, kache's own state directory and its log directory (the front's, when
#: kache wraps), and the workspace root (`fix` and `clippy --fix` alone). Every role is passed on
#: every line — a parameter the profile reads and nobody sets fails the profile
#: closed — and an unused one names `MAC_UNUSED_ROLE`. The same names are set
#: in the bounded command's environment, so the kache launcher's mac tail
#: re-applies exactly this profile inside the compile: macOS refuses a nested
#: `sandbox-exec` unless the profile and its parameters are identical
#: (`sandbox_apply: Operation not permitted`, measured on macOS 2026-11-05). The
#: launcher's Rust copy (tools/kache-launcher/src/tail.rs, MAC_ROLES) is pinned
#: equal by test_kache_launcher_front_pins.py.
MAC_ROLES = ("FAUNA_TARGET", "FAUNA_SCRATCH", "FAUNA_STORE", "FAUNA_KACHE_STATE",
             "FAUNA_KACHE_LOG", "FAUNA_WORKSPACE")
#: The path an unused role names: under a character device, so it can never
#: exist and nothing can make it.
MAC_UNUSED_ROLE = "/dev/null/fauna-unused-role"
#: The darwin scratch's parent under the target; each compile makes its own
#: directory below it and removes it when the bounded command ends.
MAC_SCRATCH_DIR = ".fauna-scratch"
#: The workflow step a GitHub-hosted Linux runner needs before it compiles.
RUNNER_STEP = "Install bubblewrap"
#: The prefix of every line this program prints.
LINE = "[cargo-boundary]"
#: The record a bounded compile is appended to, when set.
RECORD_ENV = "FAUNA_CARGO_BOUNDARY_RECORD"

#: The cargo subcommands that compile, aliases included — decision (1)'s list,
#: pinned. The compile cache's stub restore reads the same set.
COMPILING_SUBCOMMANDS = frozenset({
    "build", "b", "check", "c", "clippy", "fix", "test", "t", "bench", "run", "r",
    "doc", "d", "rustc", "rustdoc",
})
#: The platforms with a boundary primitive (Windows has none).
BOUNDARY_ON_COMPILE = frozenset({"linux", "darwin"})
#: cargo's global options that take a value as the next word (before the subcommand)
GLOBAL_VALUE_OPTS = ("-Z", "--config", "-C", "--color")
#: the flags the `cargo fetch` prelude carries: what decides WHICH registry bytes
#: the build inside will read — nothing else (`-Z` and the toolchain besides)
FETCH_FLAGS = ("--locked", "--offline", "--frozen")
FETCH_VALUE_FLAGS = ("--manifest-path", "--target")
#: `cargo run`'s options that take the next word as their value: the program's own
#: arguments start at the first word that is neither an option nor such a value,
#: `--` or not (`cargo run --bin uniffi-bindgen generate …`)
RUN_VALUE_OPTS = ("-p", "--package", "--bin", "--example", "-F", "--features", "-j", "--jobs",
                  "--profile", "--target", "--target-dir", "--manifest-path", "--message-format",
                  "--color", "--config", "-Z", "-C", "--lockfile-path", "--artifact-dir")


class BoundaryRefused(Exception):
    """A compile that must not run: a GitHub-hosted Linux runner with no `bwrap`."""


def cmd_name(cmd0: str, plat: str) -> str:
    """A command's program name: the basename, on win32 case-folded and
    without `.exe`."""
    fold = str.lower if plat == "win32" else str
    name = fold(cmd0.replace("\\", "/").rsplit("/", 1)[-1])
    if plat == "win32" and name.endswith(".exe"):
        name = name[: -len(".exe")]
    return name


def bwrap_present() -> bool:
    return os.path.exists(BWRAP[0])


def runner_reading(env, plat: str) -> str:
    """A CI runner, by class (decision (5)): `"boundary"` — bound as on a dev
    machine (a Linux runner with /usr/bin/bwrap; a macOS runner); `"refuse"` —
    a GitHub-hosted Linux runner without bwrap (loud, never a silent compile
    outside; a runner that does not say `RUNNER_ENVIRONMENT=self-hosted` is
    read as hosted); `"remnant"` — a self-hosted Linux runner without bwrap,
    which compiles outside with one warning line until its install is on
    record. Off a runner, always `"boundary"`; nothing
    keys on GITHUB_ACTIONS alone."""
    if env.get("GITHUB_ACTIONS") != "true" or plat != "linux" or bwrap_present():
        return "boundary"
    return "remnant" if env.get("RUNNER_ENVIRONMENT") == "self-hosted" else "refuse"


def usr_links(readlink=os.readlink, isdir=os.path.isdir) -> list[str]:
    """The `USR_LINKS` part of the line, read off this host: a symlink is made
    inside as the same symlink (`--symlink <target> <path>`), a directory bound
    read-only, an absent entry left out."""
    out: list[str] = []
    for path in USR_LINKS:
        try:
            out += ["--symlink", readlink(path), path]
        except OSError:
            if isdir(path):
                out += ["--ro-bind", path, path]
    return out


def topmost(paths) -> list[str]:
    """`paths` in order without repeats and without any path under another
    one of them — a bind of the ancestor already shows it."""
    seen = list(dict.fromkeys(p.rstrip("/") or "/" for p in paths if p))
    return [p for p in seen
            if not any(q != p and (q == "/" or p.startswith(q + "/")) for q in seen)]


def bwrap_line(writable=(), readable=(), home: str | None = None,
               links: list[str] | None = None) -> list[str]:
    """The `bwrap` line: the constant part's body, then the dynamic part —
    `links` (default this host's `usr_links`), `home` as an empty directory,
    each of `readable` bound read-only (`--ro-bind-try`: a tree this host lacks
    is absent inside, and a compile that reads it fails there loudly), each of
    `writable` bound read-write, after the read list so a writable path under a
    read-only tree is writable — then the last `BWRAP_TAIL` words (decisions
    (7), part (c), and (8))."""
    dyn = list(usr_links() if links is None else links)
    if home:
        dyn += ["--dir", home]
    dyn += [w for p in readable for w in ("--ro-bind-try", p, p)]
    dyn += [w for p in writable for w in ("--bind", p, p)]
    return [*BWRAP[:-BWRAP_TAIL], *dyn, *BWRAP[-BWRAP_TAIL:]]


def mac_roles(roles=None) -> list[tuple[str, str]]:
    """Every darwin role in `MAC_ROLES`' order with its path — `roles`' value,
    else `MAC_UNUSED_ROLE`."""
    roles = roles or {}
    return [(r, roles.get(r) or MAC_UNUSED_ROLE) for r in MAC_ROLES]


def boundary_prefix(plat: str, mac_profile: str | None = None, writable=(),
                    roles=None, readable=(), home: str | None = None) -> list[str] | None:
    """The argv prefix that runs a cargo behind `plat`'s boundary with the token
    set inside — on linux the `bwrap` line over its empty root with `readable`
    bound read-only, `home` made and `writable` bound (`bwrap_line`), its
    system-call filter opened first (`seccomp_fd_wrapper`; a machine with
    no filter is `BoundaryRefused`); on darwin `sandbox-exec -f <mac_profile>`
    (the tree's profile when none is named) with every role of
    `mac_roles(roles)` as `-D <role>=<path>`, the same pairs, `TMPDIR` (the
    scratch role) and `PROFILE_VAR` (the profile applied, which the launcher's
    tail re-applies) set in the environment inside — else None (win32)."""
    if plat == "linux":
        return [*seccomp_fd_wrapper(), *bwrap_line(writable, readable, home), *SET_TOKEN]
    if plat == "darwin":
        pairs = mac_roles(roles)
        params = [w for r, p in pairs for w in ("-D", f"{r}={p}")]
        scratch = dict(pairs)["FAUNA_SCRATCH"]
        tmpdir = [f"TMPDIR={scratch}"] if scratch != MAC_UNUSED_ROLE else []
        profile = mac_profile or TREE_MAC_PROFILE
        return ["/usr/bin/sandbox-exec", "-f", profile, *params,
                "/usr/bin/env", f"{TOKEN}=1", f"{PROFILE_VAR}={profile}", *tmpdir,
                *(f"{r}={p}" for r, p in pairs)]
    return None


def mac_scratch(target: str) -> str:
    """A fresh scratch directory's path for one darwin compile, under `target`
    (decision (7), part (ii)): random, so no later process reads it by name."""
    return os.path.join(target, MAC_SCRATCH_DIR, secrets.token_hex(8))


#: cargo's option naming the target directory on the command line.
TARGET_DIR_OPT = "--target-dir"


def _subcommand_args(cmd: list[str]) -> tuple[str, list[str]]:
    """(the subcommand, its own arguments before any `--`)."""
    i = subcommand_at(cmd)
    sub = cmd[i] if i < len(cmd) else ""
    rest = cmd[i + 1:]
    return sub, (rest[:rest.index("--")] if "--" in rest else rest)


def target_dir_arg(args: list[str]) -> str | None:
    """A `--target-dir` the command line names, or None (the last one wins, as
    in cargo)."""
    found = None
    for j, a in enumerate(args):
        if a == TARGET_DIR_OPT and j + 1 < len(args):
            found = args[j + 1]
        elif a.startswith(TARGET_DIR_OPT + "="):
            found = a.split("=", 1)[1]
    return found


def rewrites_workspace(cmd: list[str]) -> bool:
    """`cargo fix` and `cargo clippy --fix`: the named residual of decision (7),
    whose compile binds the workspace root writable too."""
    sub, args = _subcommand_args(cmd)
    return sub == "fix" or (sub == "clippy" and "--fix" in args)


def metadata_argv(cmd: list[str]) -> list[str]:
    """The non-compiling `cargo metadata --no-deps` that reads the target
    directory cargo resolves for `cmd` (the same program, toolchain, global
    options and `--manifest-path`)."""
    i = subcommand_at(cmd)
    _, args = _subcommand_args(cmd)
    out = [*cmd[:i], "metadata", "--no-deps", "--format-version", "1"]
    for j, a in enumerate(args):
        if a == "--manifest-path" and j + 1 < len(args):
            out += [a, args[j + 1]]
        elif a.startswith("--manifest-path="):
            out.append(a)
    return out


#: The cache-directory tag cargo writes into a target it creates. A target that
#: exists without it (an empty dataset, a directory made by `mkdir -p`) is
#: re-created by cargo atomically through a temporary SIBLING in the target's
#: parent — a write the read-only root refuses — so the helper tags the
#: resolved target first, outside, and cargo finds it complete.
CACHEDIR_TAG = ("Signature: 8a477f597d28d172789f06886806bc55\n"
                "# This file is a cache directory tag created by cargo.\n"
                "# For information about cache directory tags see https://bford.info/cachedir/\n")


def tag_line(target: str) -> str:
    """The bash line that tags `target` when it is not tagged yet."""
    tag = os.path.join(target, "CACHEDIR.TAG")
    fmt = CACHEDIR_TAG.replace("%", "%%").replace("\n", "\\n")  # one line: printf's own escapes
    return f"[ -e {shlex.quote(tag)} ] || printf {shlex.quote(fmt)} > {shlex.quote(tag)}"


#: One `cargo metadata` per command line, cwd and environment in this process:
#: the writable and the read list both read it.
_METADATA: dict = {}


def _default_run(env):
    def run(a):
        p = subprocess.run(a, capture_output=True, text=True, env=dict(env), timeout=300)
        return p.returncode, p.stdout, p.stderr
    return run


def cargo_metadata(cmd: list[str], environ=None, run=None) -> tuple[dict, list[str], str]:
    """(`cargo metadata --no-deps`'s answer for `cmd` or {}, the argv asked, its
    last stderr line) — run here, outside."""
    env = os.environ if environ is None else environ
    argv_ = metadata_argv(cmd)
    key = (tuple(argv_), os.getcwd(), tuple(sorted(env.items())))
    if run is None and key in _METADATA:
        return _METADATA[key]
    try:
        rc, out, err = (run or _default_run(env))(argv_)
    except (OSError, subprocess.SubprocessError) as e:
        rc, out, err = 127, "", str(e)
    try:
        meta = json.loads(out) if rc == 0 else {}
    except ValueError:
        meta = {}
    tail = (err or "").strip().splitlines()[-1:] or ["no output"]
    answer = (meta if isinstance(meta, dict) else {}), argv_, tail[0]
    if run is None and answer[0]:
        _METADATA[key] = answer
    return answer


def writable_set(cmd: list[str], environ=None, run=None, meta=None) -> list[str]:
    """What a bounded compile of `cmd` may write beside the scratch (decision
    (7)): the target directory cargo resolves for THIS command — its
    `--target-dir` argument, else `cargo metadata --no-deps`'s
    `target_directory` (which honours `CARGO_TARGET_DIR`, `build.target-dir`
    and `--manifest-path`), run here, outside (`meta`, when the caller already
    holds the answer) — plus the workspace root for `fix` and `clippy --fix`.
    Raises BoundaryRefused when cargo cannot say: never a compile with a
    guessed set."""
    _, args = _subcommand_args(cmd)
    explicit = target_dir_arg(args)
    fix = rewrites_workspace(cmd)
    if explicit is None or fix:
        argv_, tail = metadata_argv(cmd), "no output"
        if meta is None:
            meta, argv_, tail = cargo_metadata(cmd, environ, run)
        if not meta.get("target_directory") or (fix and not meta.get("workspace_root")):
            raise BoundaryRefused(
                f"{LINE} REFUSED — `{shlex.join(argv_)}` could not name the target directory "
                f"this compile may write (decision (7)): {tail}")
    paths = [os.path.abspath(explicit) if explicit is not None else meta["target_directory"]]
    if fix:
        paths.append(meta["workspace_root"])
    return paths


#: The variable a build grant's jobserver rides in, and the form naming a FIFO.
JOBSERVER_ENV = "CARGO_MAKEFLAGS"
_JOBSERVER_FIFO = "--jobserver-auth=fifo:"


def jobserver_fifo(environ) -> str | None:
    """The FIFO the grant's jobserver names (`--jobserver-auth=fifo:<path>` in
    `CARGO_MAKEFLAGS`), or None: cargo opens it BY PATH, so it is on the read
    list — a FIFO, no socket — or the grant's live width is silently lost."""
    for word in (environ.get(JOBSERVER_ENV) or "").split():
        if word.startswith(_JOBSERVER_FIFO) and len(word) > len(_JOBSERVER_FIFO):
            return word[len(_JOBSERVER_FIFO):]
    return None


def git_common_dir(root: str, run=None, environ=None) -> str | None:
    """The git common directory of the checkout at `root`, absolute, or None
    (no git, not a checkout)."""
    env = os.environ if environ is None else environ
    try:
        rc, out, _ = (run or _default_run(env))(
            ["git", "-C", root, "rev-parse", "--path-format=absolute", "--git-common-dir"])
    except (OSError, subprocess.SubprocessError):
        return None
    out = (out or "").strip()
    return out if rc == 0 and os.path.isabs(out) else None


def cargo_config_dirs(cwd: str, isdir=os.path.isdir) -> list[str]:
    """Every `.cargo` directory of `cwd` and its ancestors, nearest first:
    cargo merges the configuration of each (its arrays, `rustflags` among them,
    concatenated), so a compile inside that lacked one resolves a different
    configuration than the outside half after it — a different `-C metadata`
    for every unit, recompiled outside (measured on the primary dev VM
    2026-11-05: the main checkout's `.cargo/config.toml` is an ancestor of
    every checkout nested under it)."""
    out, d = [], os.path.abspath(cwd)
    while True:
        if isdir(os.path.join(d, ".cargo")):
            out.append(os.path.join(d, ".cargo"))
        parent = os.path.dirname(d)
        if parent == d:
            return out
        d = parent


def toolchain_root(program: str, path: str | None) -> str | None:
    """The root above the `bin` directory `program` resolves to on `path`
    (symlinks followed), or None when it is not found."""
    found = shutil.which(program, path=path)
    if not found:
        return None
    return os.path.dirname(os.path.dirname(os.path.realpath(found)))


def readable_set(cmd: list[str], environ=None, run=None, meta=None) -> list[str]:
    """What a bounded compile of `cmd` may read beside the system trees
    (decision (8)): `CARGO_HOME` (default `~/.cargo`), `RUSTUP_HOME` (default
    `~/.rustup`) and the workspace root cargo resolves (`cargo metadata
    --no-deps`, `meta` when the caller holds it) — and, each added BY NAME
    with its reason: the directory of the cargo program `cmd` names (a rustup
    proxy outside a `CARGO_HOME` the caller set elsewhere; cargo starts rustc
    by the same PATH), the `.cargo` directory of the cwd and each ancestor
    (`cargo_config_dirs`: the same configuration on both sides of the
    boundary), the checkout's git common directory (`commit!()` runs `git
    rev-parse HEAD` at compile time, else the artifact is silently unstamped),
    the toolchain root of each of `CROSS_LINKERS` on PATH, the install root of
    each `TARGET_TOOL_VARS` compiler the caller exports by absolute path, the grant's
    jobserver FIFO (`jobserver_fifo`), and the front's directory and its
    interpreter's install root (`FAUNA_CARGO_FRONT`, `FAUNA_CARGO_FRONT_PYTHON`:
    a build script's own cargo reaches the cargo shim on PATH, which calls the
    front back through them — the front a checkout's copy or the machine's
    policy copy, whichever granted). Raises
    BoundaryRefused when cargo cannot name the workspace."""
    env = os.environ if environ is None else environ
    home = env.get("HOME")
    out: list[str] = []
    for var, dflt in (("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")):
        if env.get(var):
            out.append(os.path.abspath(env[var]))
        elif home:
            out.append(os.path.join(home, dflt))
    argv_, tail = metadata_argv(cmd), "no output"
    if meta is None:
        meta, argv_, tail = cargo_metadata(cmd, env, run)
    root = meta.get("workspace_root")
    if not root:
        raise BoundaryRefused(
            f"{LINE} REFUSED — `{shlex.join(argv_)}` could not name the workspace this "
            f"compile reads (decision (8)): {tail}")
    out.append(root)
    program = shutil.which(cmd[0], path=env.get("PATH")) if cmd else None
    if program:
        out.append(os.path.dirname(os.path.abspath(program)))
    out += cargo_config_dirs(os.getcwd())
    common = git_common_dir(root, run, env)
    if common:
        out.append(common)
    for program in CROSS_LINKERS:
        found = toolchain_root(program, env.get("PATH"))
        if found:
            out.append(found)
    for var in TARGET_TOOL_VARS:
        tool = env.get(var)
        if tool and os.path.isabs(tool):
            root = os.path.dirname(os.path.dirname(os.path.realpath(tool)))
            if root not in out:
                out.append(root)
    fifo = jobserver_fifo(env)
    if fifo:
        out.append(fifo)
    front = env.get("FAUNA_CARGO_FRONT")
    if front and os.path.isabs(front):
        out.append(os.path.dirname(front))
    python = env.get("FAUNA_CARGO_FRONT_PYTHON")
    if python and os.path.isabs(python):
        # the interpreter as named (a venv's `bin/python` beside its `pyvenv.cfg`)
        # and the installation it resolves to
        out.append(os.path.dirname(os.path.dirname(python)))
        out.append(os.path.dirname(os.path.dirname(os.path.realpath(python))))
    return out


def run_own_args(args: list[str]) -> list[str]:
    """`cargo run`'s own arguments in `args` (everything before `--` already):
    up to the first positional word, which begins the program's arguments."""
    i = 0
    while i < len(args) and args[i].startswith("-"):
        if args[i] in RUN_VALUE_OPTS:
            i += 1
        i += 1
    return args[:i]


def subcommand_at(cmd: list[str]) -> int:
    """The index of a cargo command line's subcommand — past `+<toolchain>` and
    the global options (a value-taking one's value included) — or len(cmd)."""
    i = 1
    if i < len(cmd) and cmd[i].startswith("+"):
        i += 1
    while i < len(cmd) and cmd[i].startswith("-"):
        if cmd[i] in GLOBAL_VALUE_OPTS and i + 1 < len(cmd):
            i += 1
        i += 1
    return i


def plan(cmd: list[str]) -> tuple[list[str], list[str], list[str] | None]:
    """(the `cargo fetch` prelude, the half run inside the boundary, the half run
    outside after it or None). `run` builds inside (`build` with the same
    selection) and runs outside; `test`/`bench` compile inside (`--no-run`) and
    run outside, every unit then Fresh. Everything else runs inside as it is."""
    i = subcommand_at(cmd)
    head = cmd[:i]
    toolchain = head[1:2] if len(head) > 1 and head[1].startswith("+") else []
    sub = cmd[i] if i < len(cmd) else ""
    rest = cmd[i + 1:]
    args = rest[:rest.index("--")] if "--" in rest else rest
    if sub in ("run", "r"):
        args = run_own_args(args)
    fetch = [cmd[0], *toolchain, "fetch"]
    words = [*head[1 + len(toolchain):], *args]
    j = 0
    while j < len(words):
        w = words[j]
        if w in FETCH_FLAGS or (w.startswith("-Z") and w != "-Z") \
                or any(w.startswith(f + "=") for f in FETCH_VALUE_FLAGS):
            fetch.append(w)
        elif (w == "-Z" or w in FETCH_VALUE_FLAGS) and j + 1 < len(words):
            fetch += [w, words[j + 1]]
            j += 1
        elif w in GLOBAL_VALUE_OPTS and j + 1 < len(words):
            j += 1  # a global option's value is never a flag of its own
        j += 1
    if sub in ("run", "r"):
        return fetch, [*head, "build", *args], list(cmd)
    if sub in ("test", "t", "bench") and "--no-run" not in args and "--doc" not in args:
        return fetch, [*head, sub, *args, "--no-run"], list(cmd)
    return fetch, list(cmd), None


def sandboxed_self() -> bool:
    """The kernel's answer on macOS: `sandbox_check(getpid())` is 1 under a profile."""
    try:
        import ctypes
        lib = ctypes.CDLL("/usr/lib/system/libsystem_sandbox.dylib")
        lib.sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
        return lib.sandbox_check(os.getpid(), None, 0) == 1
    except (OSError, AttributeError):
        return False


def inside(environ) -> bool:
    """Whether this process already runs inside the boundary. On linux: the token
    equals this process's network namespace AND that namespace has no interface
    but `lo` — a stale token, or one forged to the host's namespace, is never
    inside. On macOS: the token is `1` AND the kernel's `sandbox_check` of this
    process reads 1 — never the token alone."""
    token = environ.get(TOKEN)
    if not token:
        return False
    if sys.platform == "darwin":
        return token == "1" and sandboxed_self()
    try:
        if token != os.readlink("/proc/self/ns/net"):
            return False
        with open("/proc/self/net/dev", encoding="utf-8") as f:
            ifaces = [ln.split(":", 1)[0].strip() for ln in f.read().splitlines()[2:] if ":" in ln]
    except OSError:
        return False
    return ifaces == ["lo"]


def boundary_line(cmd: list[str], label: str) -> str:
    """The one line a bounded compile prints, in both placements."""
    return f"{LINE} `{shlex.join(cmd[:4])}…` compiles with no network ({label})"


def record(cmd: list[str], environ=None) -> None:
    """Append one bounded compile to the record FAUNA_CARGO_BOUNDARY_RECORD
    names, if it names one."""
    env = os.environ if environ is None else environ
    path = env.get(RECORD_ENV)
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(shlex.join(cmd) + "\n")


def argv(
    cmd: list[str],
    label: str | None = None,
    environ: dict[str, str] | None = None,
    platform: str | None = None,
    mac_profile: str | None = None,
    writable=None,
    also_writable=(),
    also_readable=(),
) -> list[str] | None:
    """The argv that runs a cargo behind the boundary — one `bash -c` line, so a
    caller's process scope, pid and lock fds carry the whole sequence: the
    `cargo fetch` prelude outside (its failure the command's), the inside half
    under `boundary_prefix` with the token set INSIDE, then the outside half if
    the plan has one. Every COMPILING cargo on a platform with a primitive, and
    no other; None for a cargo that compiles nothing, a command that is not
    cargo, a platform with no primitive (win32), a process already inside, or a
    self-hosted runner's remnant (a warning line). Raises BoundaryRefused on a
    GitHub-hosted Linux runner without bwrap. `label` is the boundary line's
    suffix (the caller's word on the compile — `kache-wrapped`, `unwrapped`,
    `runner`); this function never decides a wrap. With a label, a bounded
    compile prints its boundary line and is recorded (`record`); without one,
    nothing is printed.

    The compile inside may write only its writable set (decision (7)):
    `writable` when the caller names it whole, else `writable_set`'s (the
    target directory, then the workspace root for `fix`), with
    `also_writable` added either way — a mapping of darwin role to path (the
    front's: the compile-cache store, `FAUNA_STORE`, and kache's state
    directory, `FAUNA_KACHE_STATE`), whose paths linux binds. On linux the
    compile reads nothing but the system trees, `readable_set`'s and
    `also_readable` (the front's: kache's own directories), each bound
    read-only over an empty root (decision (8)). On darwin the
    set's first path is the target role and a second the workspace role (a
    third has no role: refused), and the helper adds this compile's scratch
    under the target (`mac_scratch`), removed when the command ends. Every path
    is made (`mkdir -p`) before the inside half runs — a cold target is a
    directory to bind. The file-command variables (`FILE_COMMAND_VARS`) are
    unset first."""
    env = os.environ if environ is None else environ
    plat = platform or sys.platform
    if plat not in BOUNDARY_ON_COMPILE or not cmd or inside(env):
        return None
    if cmd_name(cmd[0], plat) != "cargo":
        return None
    i = subcommand_at(cmd)
    if i >= len(cmd) or cmd[i] not in COMPILING_SUBCOMMANDS:
        return None
    # A CI runner, by class — read only for a cargo the boundary would take, so a
    # non-compiling one is never refused.
    reading = runner_reading(env, plat)
    if reading == "remnant":
        print(f"{LINE} WARNING — a self-hosted Linux runner without /usr/bin/bwrap: "
              f"`{shlex.join(cmd[:4])}…` compiles WITH a network, the transitional remnant of "
              "build-compile-cache.md § The boundary's scope and what OFF means, decision (5); "
              "install bubblewrap on this runner to close it", file=sys.stderr, flush=True)
        return None
    if reading == "refuse":
        raise BoundaryRefused(
            f"{LINE} REFUSED — `{shlex.join(cmd[:4])}…` on a GitHub-hosted Linux runner "
            "without /usr/bin/bwrap would compile with a network (build-compile-cache.md § The boundary's "
            f"scope and what OFF means, decision (5)); add the `{RUNNER_STEP}` step (as ci.yml's "
            "`web` job runs it) to this job before the step that compiles")
    target = None
    if writable is None:
        writable = writable_set(cmd, env)
        target = writable[0] if writable else None
    readable: list[str] = []
    if plat == "linux":
        readable = topmost([*readable_set(cmd, env), *also_readable])
    also = dict(also_writable or {})
    scratch = None
    roles = None
    if plat == "darwin":
        if len(writable) > 2:
            raise BoundaryRefused(
                f"{LINE} REFUSED — a darwin writable set names the target and `fix`'s workspace "
                f"root, no more (decision (7)); {len(writable)} paths have no role: "
                f"{shlex.join(writable)}")
        roles = dict(also)
        if writable:
            scratch = mac_scratch(writable[0])
            roles.update(FAUNA_TARGET=writable[0], FAUNA_SCRATCH=scratch)
        if len(writable) > 1:
            roles["FAUNA_WORKSPACE"] = writable[1]
    paths = list(dict.fromkeys([*writable, *also.values(), *([scratch] if scratch else [])]))
    prefix = boundary_prefix(plat, mac_profile, paths, roles, readable, env.get("HOME"))
    fetch, inner, outer = plan(cmd)
    inside_line = shlex.join([*prefix, *inner])
    lines = ["set -e", "unset " + " ".join(FILE_COMMAND_VARS), shlex.join(fetch)]
    if paths:
        lines.append(shlex.join(["mkdir", "-p", "--", *paths]))
    if target:
        lines.append(tag_line(target))
    if scratch:
        # the scratch goes when the command ends, the compile's exit kept — so the
        # inside half is not exec'd (an exec would drop the trap)
        lines.append("trap " + shlex.quote(shlex.join(["rm", "-rf", "--", scratch])) + " EXIT")
    if outer is None:
        lines.append(("" if scratch else "exec ") + inside_line)
    else:
        lines += [inside_line]
        if scratch:
            lines += [shlex.join(["rm", "-rf", "--", scratch]), "trap - EXIT"]
        lines.append("exec " + shlex.join(outer))
    if label is not None:
        print(boundary_line(cmd, label), file=sys.stderr, flush=True)
        record(cmd, env)
    return ["/bin/bash", "-c", "\n".join(lines), "kache-netns"]


#: The shim's directory, beside this file — what `runner-setup` puts first on
#: the job's PATH.
SHIM_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "cargo-shim")
#: The record's file name under the runner's temp directory.
RECORD_NAME = "cargo-boundary.log"
#: What `runner-setup` saw, under the runner's temp directory: the variables it
#: wrote, the shim's directory and the sha256 of the shim, this helper, the
#: darwin profile and the cargo PATH resolved to then (decision (7), part (b)).
MANIFEST_NAME = "cargo-boundary-setup.json"
#: The workflow steps that place and witness the boundary (decision (6), part (iv)).
SETUP_STEP = "Put every cargo behind the boundary"
WITNESS_STEP = "Every cargo compiled behind the boundary"
#: The Linux runner's namespace probe — the `Install bubblewrap` step's own line.
BWRAP_PROBE = ("/usr/bin/bwrap", "--unshare-all", "--ro-bind", "/", "/", "true")
SANDBOX_EXEC = "/usr/bin/sandbox-exec"


def sandbox_exec_present() -> bool:
    return os.path.exists(SANDBOX_EXEC)


def primitive_refusal(plat: str, probe=None) -> str | None:
    """Why this runner cannot bound a compile, or None when it can: on linux
    `/usr/bin/bwrap` present AND a namespace it makes (the probe); on darwin
    `/usr/bin/sandbox-exec` present; any other platform has no primitive."""
    if plat == "linux":
        if not bwrap_present():
            return (f"no /usr/bin/bwrap on this Linux runner — add the `{RUNNER_STEP}` step "
                    "(as ci.yml's `web` job runs it) before this one")
        run = probe or (lambda argv_: subprocess.run(argv_, capture_output=True).returncode)
        if run(list(BWRAP_PROBE)) != 0:
            return (f"`{shlex.join(BWRAP_PROBE)}` failed: this runner refuses an unprivileged "
                    f"namespace — the `{RUNNER_STEP}` step lifts the AppArmor restriction first")
        return None
    if plat == "darwin":
        if not sandbox_exec_present():
            return f"no {SANDBOX_EXEC} on this macOS runner"
        return None
    return f"platform {plat!r} has no boundary primitive (a Windows job carries none of these steps)"


def sha256_of(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def runner_setup(environ=None, plat: str | None = None, probe=None, which=None) -> int:
    """Decision (6), parts (i) and (iv): on a GitHub Actions runner with a
    primitive, put the shim first on the job's PATH (`$GITHUB_PATH`) and name
    this helper as its front and the record the witness reads (`$GITHUB_ENV`).
    Every cargo of every later step then reaches `main` through the shim. A
    runner without a primitive, or a call off GitHub Actions, is refused (exit 1,
    one line) — a job the shape gate says compiles never runs its cargo plain."""
    env = os.environ if environ is None else environ
    plat = plat or sys.platform
    gh_path, gh_env, temp = env.get("GITHUB_PATH"), env.get("GITHUB_ENV"), env.get("RUNNER_TEMP")
    if not (gh_path and gh_env and temp):
        print(f"{LINE} runner-setup REFUSED — not a GitHub Actions step (GITHUB_PATH, GITHUB_ENV "
              "and RUNNER_TEMP must all be set); a dev machine's front bounds its cargo itself",
              file=sys.stderr)
        return 1
    why = primitive_refusal(plat, probe)
    if why:
        print(f"{LINE} runner-setup REFUSED — {why}", file=sys.stderr)
        return 1
    record_path = os.path.join(temp, RECORD_NAME)
    lines = {
        "FAUNA_CARGO_FRONT": os.path.abspath(__file__),
        "FAUNA_CARGO_FRONT_PYTHON": sys.executable,
        RECORD_ENV: record_path,
    }
    # Decision (7), part (b): what the witness compares at the job's end. The
    # cargo is the one this step's PATH resolves (the shim is not on it yet);
    # a job with none has no proxy to guard.
    cargo = (which or shutil.which)("cargo", path=env.get("PATH"))
    files = [os.path.join(SHIM_DIR, "cargo"), os.path.abspath(__file__), TREE_MAC_PROFILE,
             *([os.path.realpath(cargo)] if cargo else [])]
    manifest = {"env": lines, "shim_dir": SHIM_DIR, "digests": {f: sha256_of(f) for f in files}}
    with open(os.path.join(temp, MANIFEST_NAME), "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=1, sort_keys=True)
    with open(gh_path, "a", encoding="utf-8") as f:
        f.write(SHIM_DIR + "\n")
    with open(gh_env, "a", encoding="utf-8") as f:
        for k, v in lines.items():
            f.write(f"{k}={v}\n")
    print(f"{LINE} runner-setup: PATH first {SHIM_DIR}")
    for k, v in lines.items():
        print(f"{LINE} runner-setup: {k}={v}")
    return 0


def witness_breaks(env) -> list[str]:
    """What a bypass could not keep green (decision (7), part (b)), compared with
    `runner-setup`'s manifest: each variable it wrote still as written, the
    shim's directory still first on PATH, every digest unchanged. Empty when
    all hold. It detects a boundary that leaked a write; it is not a second
    boundary — a helper rewritten to lie keeps itself green."""
    try:
        with open(os.path.join(env.get("RUNNER_TEMP") or "", MANIFEST_NAME), encoding="utf-8") as f:
            manifest = json.load(f)
    except (OSError, ValueError):
        return [f"no runner-setup manifest {MANIFEST_NAME} under RUNNER_TEMP"]
    broken = []
    for k, v in sorted(manifest.get("env", {}).items()):
        if env.get(k) != v:
            broken.append(f"{k} is {env.get(k)!r}, runner-setup wrote {v!r}")
    path = (env.get("PATH") or "").split(os.pathsep)
    # The witness runs as `uv run --no-project python …`, and `uv run` puts this
    # interpreter's scripts directory first on the PATH it hands us (`/usr/local/bin`
    # under a Debian python, `/opt/homebrew/bin` under Homebrew's). That ONE entry is
    # uv's, not the job's: pass over it, never a second copy a write could have planted.
    if env.get("UV") and len(path) > 1 and path[0] == sysconfig.get_path("scripts"):
        path = path[1:]
    first = path[0]
    if first != manifest.get("shim_dir"):
        broken.append(f"PATH starts with {first!r}, not the shim's directory {manifest.get('shim_dir')!r}")
    for path, digest in sorted(manifest.get("digests", {}).items()):
        try:
            now = sha256_of(path)
        except OSError:
            now = "absent"
        if now != digest:
            broken.append(f"{path} changed since runner-setup (sha256 {now[:12]}, was {digest[:12]})")
    return broken


def runner_witness(environ=None) -> int:
    """Decisions (6) part (v) and (7) part (b): the job's own witness. The shape
    gate says this job compiles, so an empty or absent record means no cargo
    reached the boundary — the shim was not on the cargo's PATH: exit 1. So
    does every break `witness_breaks` reads against runner-setup's manifest.
    Else print the count and every recorded command."""
    env = os.environ if environ is None else environ
    path = env.get(RECORD_ENV)
    try:
        with open(path, encoding="utf-8") as f:  # type: ignore[arg-type]
            cmds = [ln for ln in f.read().splitlines() if ln.strip()]
    except (OSError, TypeError):
        cmds = []
    if not cmds:
        print(f"{LINE} runner-witness: no cargo reached the boundary on a job the shape gate says "
              "compiles — the shim was not on the cargo's PATH", file=sys.stderr)
        return 1
    broken = witness_breaks(env)
    for why in broken:
        print(f"{LINE} runner-witness: {why} — a bounded compile may have written outside its "
              "writable set (build-compile-cache.md § The boundary's scope and what OFF means, "
              "decision (7))", file=sys.stderr)
    if broken:
        return 1
    print(f"{LINE} runner-witness: {len(cmds)} cargo compile(s) ran behind the boundary")
    for c in cmds:
        print(f"{LINE}   {c}")
    return 0


def _exec(cmd: list[str]) -> int:
    if os.name == "posix":
        os.execvp(cmd[0], cmd)  # never returns
    return subprocess.call(cmd)


_USAGE = ("usage: cargo-boundary.py [--mac-profile <path>] [--label <text>] [--writable <path>]… [--held] "
          "-- cargo …\n"
          "       cargo-boundary.py runner-setup | runner-witness")


def main(args: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if args is None else args)
    if args and args[0] in ("runner-setup", "runner-witness"):
        if len(args) > 1:
            print(_USAGE, file=sys.stderr)
            return 2
        return runner_setup() if args[0] == "runner-setup" else runner_witness()
    if "--" not in args:
        print(_USAGE, file=sys.stderr)
        return 2
    split = args.index("--")
    opts, cmd = args[:split], args[split + 1:]
    mac_profile, label, writable = None, "runner", None
    i = 0
    while i < len(opts):
        o = opts[i]
        if o == "--held":
            pass
        elif o in ("--mac-profile", "--label", "--writable") and i + 1 < len(opts):
            if o == "--mac-profile":
                mac_profile = opts[i + 1]
            elif o == "--writable":
                writable = [*(writable or []), opts[i + 1]]
            else:
                label = opts[i + 1]
            i += 1
        else:
            print(f"{LINE} unknown option {o!r}\n{_USAGE}", file=sys.stderr)
            return 2
        i += 1
    if not cmd:
        print(_USAGE, file=sys.stderr)
        return 2
    try:
        bounded = argv(cmd, label, mac_profile=mac_profile, writable=writable)
    except BoundaryRefused as refused:
        print(refused, file=sys.stderr, flush=True)
        return 1  # nothing ran: the refusal is the job's red, by name
    return _exec(bounded or cmd)


if __name__ == "__main__":
    sys.exit(main())
