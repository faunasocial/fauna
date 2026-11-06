//! How kache is started on each platform — the launcher's tails (stage 2,
//! docs/goal/architecture/build-compile-cache.md § The launcher, *Stage 2's design*,
//! decision 1). Pure functions, so every (platform × inside) shape is a unit test
//! on any host; `main` reads the platform from `cfg(target_os)` and linux's two
//! readings from `/proc`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The platforms kache wraps, each with its egress boundary: win's is the
/// outbound firewall rule on the program (nothing to start), mac's a
/// `sandbox-exec` profile, linux's a network namespace of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    /// The platform this launcher was built for, or None where kache wraps nothing.
    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "windows") {
            Some(Platform::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Mac)
        } else if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else {
            None
        }
    }
}

/// mac's sandbox tool, by absolute path: never a PATH lookup.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// The profile beside the installed launcher (`just kache-launcher-install` copies
/// the reviewable source there on darwin): one
/// machine-wide copy, which the slot front reads by the same derivation (decision 3).
pub const MAC_PROFILE_NAME: &str = "kache-mac.sb";
/// The profile's immutable snapshots, a directory beside the launcher: the install
/// writes `profiles/kache-mac-<sha256>.sb` before it swaps `kache-mac.sb`, and the
/// slot front applies the snapshot to a compile and names it inside as
/// `PROFILE_VAR` — so a
/// reinstall mid-build never makes the tail's profile differ from the compile's.
pub const MAC_PROFILE_SNAPSHOTS: &str = "profiles";
/// The profile the front applied to the compile, set inside its boundary.
pub const PROFILE_VAR: &str = "FAUNA_KACHE_PROFILE";
/// linux's boundary's constant part (build-compile-cache.md § The boundary's scope
/// and what OFF means, decisions (1), (7) and (8)): every namespace unshared but
/// the pid namespace (no network but `lo`; kache's daemon must outlive the tail that
/// starts it and pass its control socket's peer-pid check, neither of which holds
/// across a pid namespace — measured 2026-11-05), the line ending with its parent,
/// and a root that is an EMPTY tmpfs, read-only once the binds are made
/// (`--remount-ro /`, the last option) — so no pathname unix socket outside the
/// trees kache needs exists inside to be reached; `/dev` as before, a fresh `/proc`,
/// `/run`, `/tmp` and `/dev/shm` empty tmpfs (`TMPDIR` the private `/tmp`), `/usr`,
/// `/etc` and `/sys` read-only, and the system-call filter read from descriptor
/// `SECCOMP_FD`. The dynamic part goes between its body and its last
/// `LINUX_BWRAP_TAIL` words (`bwrap_line`): the host's `USR_LINKS`, `$HOME` as an
/// empty directory, the read list read-only, the writable set. The SAME line as the
/// cargo boundary's (the one the slot front puts on cargo) — two copies, pinned
/// equal, not one home (decision 8): `scripts/cargo-boundary.py`'s `BWRAP`.
pub const LINUX_BWRAP: [&str; 37] = [
    "/usr/bin/bwrap",
    "--unshare-user",
    "--unshare-ipc",
    "--unshare-net",
    "--unshare-uts",
    "--unshare-cgroup-try",
    "--die-with-parent",
    "--tmpfs",
    "/",
    "--dev-bind",
    "/dev",
    "/dev",
    "--proc",
    "/proc",
    "--tmpfs",
    "/run",
    "--tmpfs",
    "/tmp",
    "--tmpfs",
    "/dev/shm",
    "--ro-bind",
    "/usr",
    "/usr",
    "--ro-bind",
    "/etc",
    "/etc",
    "--ro-bind",
    "/sys",
    "/sys",
    "--setenv",
    "TMPDIR",
    "/tmp",
    "--seccomp",
    "9",
    "--remount-ro",
    "/",
    "--",
];
/// How many of `LINUX_BWRAP`'s words follow its dynamic part: `--remount-ro / --`.
pub const LINUX_BWRAP_TAIL: usize = 3;
/// The root's entries a merged-`/usr` host keeps as symlinks into `/usr`, made
/// inside as the same symlink or bound read-only where the host has a directory —
/// the cargo boundary's `USR_LINKS`, pinned equal.
pub const USR_LINKS: [&str; 4] = ["/bin", "/lib", "/lib64", "/sbin"];
/// The descriptor `bwrap` reads the filter from — the `9` in `LINUX_BWRAP`; `main`
/// puts the program there before the tail starts.
pub const SECCOMP_FD: i32 = 9;
/// The socket families a bounded process may open (decision (1)): the ones the
/// network namespace CONTAINS — unix, inet, inet6, netlink. Every other family
/// answers EAFNOSUPPORT: `AF_VSOCK` is not namespaced (a connect to the hypervisor
/// from inside `--unshare-net` reached it, measured on the primary dev VM 2026-11-05), and the
/// rule is the family set, not the one hole. The SAME set as the cargo boundary's
/// (`SOCKET_FAMILIES` there), pinned equal through the program's bytes.
pub const SOCKET_FAMILIES: [u32; 4] = [1, 2, 10, 16];
/// The filter's bytes for x86-64 (`seccomp_program(Arch::X86_64)`), pinned: the
/// cargo boundary's Python copy builds these same bytes. Read by the pins alone.
#[cfg_attr(not(test), allow(dead_code))]
pub const SECCOMP_PROGRAM_X86_64: &str = "2000000004000000 150001003e0000c0 0600000000000080 2000000000000000 4500000100000040 0600000000000080 15000001a9010000 0600000001000500 1500010029000000 1500000635000000 2000000010000000 1500040001000000 1500030002000000 150002000a000000 1500010010000000 0600000061000500 060000000000ff7f";
/// The filter's bytes for aarch64 (`seccomp_program(Arch::Aarch64)`), pinned.
#[cfg_attr(not(test), allow(dead_code))]
pub const SECCOMP_PROGRAM_AARCH64: &str = "2000000004000000 15000100b70000c0 0600000000000080 2000000000000000 4500000100000040 0600000000000080 15000001a9010000 0600000001000500 15000100c6000000 15000006c7000000 2000000010000000 1500040001000000 1500030002000000 150002000a000000 1500010010000000 0600000061000500 060000000000ff7f";

/// The machines the filter is built for; another has no filter and so no
/// boundary (a failed launch, never a kache outside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// The machine this launcher was built for, or None where no filter exists.
    pub fn current() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    /// The kernel's audit arch word and the numbers of `socket`, `socketpair`
    /// and `io_uring_setup`.
    fn numbers(self) -> (u32, u32, u32, u32) {
        match self {
            Arch::X86_64 => (0xC000003E, 41, 53, 425),
            Arch::Aarch64 => (0xC00000B7, 198, 199, 425),
        }
    }
}

/// The system-call filter as `bwrap --seccomp` reads it — classic BPF over
/// `struct seccomp_data` (the number at 0, the arch at 4, the first argument's
/// low word at 16): a call from a foreign arch, or by x32's numbers, kills the
/// process; `io_uring_setup` answers EPERM (its socket operation would open a
/// family past this filter); `socket` and `socketpair` answer EAFNOSUPPORT for
/// every family but `SOCKET_FAMILIES`; everything else is allowed. The same
/// program as the cargo boundary's (`seccomp_program` in
/// `scripts/cargo-boundary.py`), byte for byte.
pub fn seccomp_program(arch: Arch) -> Vec<u8> {
    const LD_W_ABS: u16 = 0x20;
    const JEQ_K: u16 = 0x15;
    const JSET_K: u16 = 0x45;
    const RET_K: u16 = 0x06;
    const KILL_PROCESS: u32 = 0x8000_0000;
    const ERRNO: u32 = 0x0005_0000;
    const ALLOW: u32 = 0x7FFF_0000;
    const EPERM: u32 = 1;
    const EAFNOSUPPORT: u32 = 97;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    const DATA_NR: u32 = 0;
    const DATA_ARCH: u32 = 4;
    const DATA_ARG0: u32 = 16;
    let (audit_arch, nr_socket, nr_socketpair, nr_io_uring_setup) = arch.numbers();
    let n = SOCKET_FAMILIES.len() as u8;
    let mut program: Vec<(u16, u8, u8, u32)> = vec![
        (LD_W_ABS, 0, 0, DATA_ARCH),
        (JEQ_K, 1, 0, audit_arch),
        (RET_K, 0, 0, KILL_PROCESS),
        (LD_W_ABS, 0, 0, DATA_NR),
        (JSET_K, 0, 1, X32_SYSCALL_BIT),
        (RET_K, 0, 0, KILL_PROCESS),
        (JEQ_K, 0, 1, nr_io_uring_setup),
        (RET_K, 0, 0, ERRNO | EPERM),
        (JEQ_K, 1, 0, nr_socket),
        (JEQ_K, 0, n + 2, nr_socketpair),
        (LD_W_ABS, 0, 0, DATA_ARG0),
    ];
    for (i, family) in SOCKET_FAMILIES.iter().enumerate() {
        program.push((JEQ_K, n - i as u8, 0, *family));
    }
    program.push((RET_K, 0, 0, ERRNO | EAFNOSUPPORT));
    program.push((RET_K, 0, 0, ALLOW));
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for (code, jt, jf, k) in program {
        bytes.extend_from_slice(&code.to_le_bytes());
        bytes.push(jt);
        bytes.push(jf);
        bytes.extend_from_slice(&k.to_le_bytes());
    }
    bytes
}

/// What linux's line reads beside the system trees: the host's `USR_LINKS` part
/// (`usr_links`), `$HOME` made as an empty directory, and the read list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reads {
    pub links: Vec<OsString>,
    pub home: Option<OsString>,
    pub readable: Vec<OsString>,
}

/// The `USR_LINKS` part of the line for a host whose entries read `read(path)`:
/// `Some(Some(target))` a symlink, made the same symlink inside; `Some(None)` a
/// directory, bound read-only; `None` absent, left out.
pub fn usr_links(read: impl Fn(&str) -> Option<Option<OsString>>) -> Vec<OsString> {
    let mut out = Vec::new();
    for path in USR_LINKS {
        match read(path) {
            Some(Some(target)) => out.extend(["--symlink".into(), target, path.into()]),
            Some(None) => out.extend(["--ro-bind".into(), path.into(), path.into()]),
            None => {}
        }
    }
    out
}

/// `paths` in order without repeats and without any path under another one of
/// them — a bind of the ancestor already shows it.
pub fn topmost(paths: &[OsString]) -> Vec<OsString> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for p in paths {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() && !seen.contains(&p) {
            seen.push(p);
        }
    }
    seen.iter()
        .filter(|p| !seen.iter().any(|q| q != *p && p.starts_with(q)))
        .map(|p| p.clone().into_os_string())
        .collect()
}

/// The `bwrap` line: the constant part's body, then the dynamic part — `reads`'
/// links, its home made (`--dir`), each of its read list bound read-only
/// (`--ro-bind-try`: a tree the host lacks is absent inside, and what reads it
/// fails there loudly), each of `writable` bound read-write after them — then the
/// last `LINUX_BWRAP_TAIL` words.
pub fn bwrap_line(reads: &Reads, writable: &[OsString]) -> Vec<OsString> {
    let (body, last) = LINUX_BWRAP.split_at(LINUX_BWRAP.len() - LINUX_BWRAP_TAIL);
    let mut line: Vec<OsString> = body.iter().map(OsString::from).collect();
    line.extend(reads.links.iter().cloned());
    if let Some(home) = &reads.home {
        line.extend(["--dir".into(), home.clone()]);
    }
    for path in &reads.readable {
        line.extend(["--ro-bind-try".into(), path.clone(), path.clone()]);
    }
    for path in writable {
        line.extend(["--bind".into(), path.clone(), path.clone()]);
    }
    line.extend(last.iter().map(OsString::from));
    line
}

/// `tail` with each of `files` bound read-only into linux's line (before its last
/// `LINUX_BWRAP_TAIL` words and the program after them) — the argfiles kache
/// reads, which sit in the host's temp directory, a tree the line never shows
/// whole. Any other tail is returned as it is.
pub fn bind_files(tail: &[OsString], files: &[OsString]) -> Vec<OsString> {
    let mut out = tail.to_vec();
    if files.is_empty()
        || tail.first().map(OsString::as_os_str) != Some(OsStr::new(LINUX_BWRAP[0]))
        || tail.len() < LINUX_BWRAP_TAIL + 1
    {
        return out;
    }
    let at = tail.len() - LINUX_BWRAP_TAIL - 1;
    let binds = files
        .iter()
        .flat_map(|f| ["--ro-bind-try".into(), f.clone(), f.clone()]);
    out.splice(at..at, binds);
    out
}

/// The `local_store` a kache configuration's text names: the first
/// `local_store = '<path>'` (or `"<path>"`) line, a trailing comment allowed.
pub fn store_of(config: &str) -> Option<String> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "local_store" {
            return None;
        }
        let value = value.split_once('#').map_or(value, |(v, _)| v).trim();
        let quote = value.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
        (!inner.is_empty()).then(|| inner.to_owned())
    })
}
/// The token the slot front sets INSIDE its boundary on cargo: on linux that
/// namespace's `readlink /proc/self/ns/net` (inside it a second `bwrap` is refused
/// by the kernel), on macOS `1`.
pub const INSIDE_VAR: &str = "FAUNA_KACHE_INSIDE";

/// The mac profile's writable roles (build-compile-cache.md § The boundary's scope
/// and what OFF means, decision (7), part (c)), each a `(param …)` the profile
/// allows writes under, passed on every line as `-D <role>=<path>`. The SAME list
/// as the cargo boundary's; linux binds the same roles' paths.
pub const MAC_ROLES: [&str; 6] = [
    "FAUNA_TARGET",
    "FAUNA_SCRATCH",
    "FAUNA_STORE",
    "FAUNA_KACHE_STATE",
    "FAUNA_KACHE_LOG",
    "FAUNA_WORKSPACE",
];
/// The path an unused role names: under a character device, so it cannot exist.
pub const MAC_UNUSED_ROLE: &str = "/dev/null/fauna-unused-role";

/// A writable role and its path.
pub type Role = (String, OsString);

/// mac's `-D <role>=<path>` parameters: every role of `MAC_ROLES` in order, its
/// path from `roles` or `MAC_UNUSED_ROLE`.
pub fn mac_params(roles: &[Role]) -> Vec<OsString> {
    let mut params = Vec::new();
    for role in MAC_ROLES {
        let path = roles
            .iter()
            .find(|(r, _)| r == role)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| MAC_UNUSED_ROLE.into());
        let mut param = OsString::from(format!("{role}="));
        param.push(path);
        params.extend(["-D".into(), param]);
    }
    params
}

/// The argv that starts kache on `platform` — the compile's `<rustc> @<file>`, or
/// `--kache`'s arguments, follow it. `inside` matters on linux alone: already in
/// the front's namespace, kache runs as it is; mac's tail never skips, because
/// macOS re-applies a profile identical to the one it runs under (decision (7):
/// macOS refuses a nested `sandbox-exec` whose profile or parameters differ,
/// measured 2026-11-05 — so inside a compile `writable` must carry the front's
/// roles exactly). `writable` is the writable set by role (decision (7)): linux
/// binds its paths outside (the store and kache's own state directory, nothing
/// else) over the read list `reads` (decision (8)); mac passes every role
/// (`mac_params`); windows ignores both.
pub fn tail(
    platform: Platform,
    kache: &OsStr,
    profile: &Path,
    inside: bool,
    writable: &[Role],
    reads: &Reads,
) -> Vec<OsString> {
    match platform {
        Platform::Windows => vec![kache.to_owned()],
        Platform::Mac => {
            let mut line: Vec<OsString> = vec![
                SANDBOX_EXEC.into(),
                "-f".into(),
                profile.as_os_str().to_owned(),
            ];
            line.extend(mac_params(writable));
            // the scratch is kache's TMPDIR, inside a compile and out (linux's
            // private `/tmp`): the profile refuses the user's temp directory
            if let Some((_, scratch)) = writable.iter().find(|(r, _)| r == "FAUNA_SCRATCH") {
                let mut tmpdir = OsString::from("TMPDIR=");
                tmpdir.push(scratch);
                line.extend(["/usr/bin/env".into(), tmpdir]);
            }
            line.push(kache.to_owned());
            line
        }
        Platform::Linux if inside => vec![kache.to_owned()],
        Platform::Linux => {
            let paths: Vec<OsString> = writable.iter().map(|(_, p)| p.clone()).collect();
            let mut line = bwrap_line(reads, &paths);
            line.push(kache.to_owned());
            line
        }
    }
}

/// The mac profile the tail applies (`tail`'s `profile`): beside the launcher —
/// except inside the front's boundary on cargo (`inside`: `FAUNA_KACHE_INSIDE` is
/// `1`), where it is the snapshot the front applied and named in `PROFILE_VAR`
/// (`named`), so the nested `sandbox-exec` repeats the compile's own profile even
/// after a reinstall swapped the copy beside the launcher (macOS refuses a nested
/// profile that differs: `sandbox_apply: Operation not permitted`). Only a snapshot
/// of this launcher's own directory is taken — `<launcher_dir>/profiles/` and a
/// `kache-mac-<64 lowercase hex>.sb` name — anything else is the copy beside it.
pub fn mac_profile(launcher_dir: &Path, named: Option<&OsStr>, inside: bool) -> PathBuf {
    let beside = launcher_dir.join(MAC_PROFILE_NAME);
    let Some(named) = named.filter(|_| inside).map(Path::new) else {
        return beside;
    };
    let snapshots = launcher_dir.join(MAC_PROFILE_SNAPSHOTS);
    let snapshot_name = named
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|n| n.strip_prefix("kache-mac-"))
        .and_then(|n| n.strip_suffix(".sb"))
        .is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    if snapshot_name && named.parent() == Some(snapshots.as_path()) {
        named.to_path_buf()
    } else {
        beside
    }
}

/// The files the tail needs that are missing, in the order it would need them —
/// the platform's tool and profile, then kache. A missing one is a failed launch,
/// one line, nothing run: never an unsandboxed kache, never rustc in its place.
pub fn missing(
    platform: Platform,
    kache: &OsStr,
    profile: &Path,
    inside: bool,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut needed: Vec<PathBuf> = match platform {
        Platform::Windows => vec![],
        Platform::Mac => vec![SANDBOX_EXEC.into(), profile.to_path_buf()],
        Platform::Linux if inside => vec![],
        Platform::Linux => vec![LINUX_BWRAP[0].into()],
    };
    needed.push(PathBuf::from(kache));
    needed.into_iter().find(|p| !exists(p))
}

/// Whether this process already runs inside the front's boundary on linux: the
/// token equals this process's network namespace AND that namespace lists no
/// interface but `lo`. A stale token, or one forged to the host's namespace, is
/// never inside — the `bwrap` then stays, and fails the compile loudly inside.
pub fn inside(token: Option<&OsStr>, netns: Option<&OsStr>, interfaces: &[String]) -> bool {
    let (Some(token), Some(netns)) = (token, netns) else {
        return false;
    };
    !token.is_empty() && token == netns && interfaces.len() == 1 && interfaces[0] == "lo"
}

/// The interface names `/proc/self/net/dev` lists: past its two header lines, the
/// text before each line's `:`.
pub fn interfaces(net_dev: &str) -> Vec<String> {
    net_dev
        .lines()
        .skip(2)
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name.trim().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[OsString]) -> Vec<&str> {
        v.iter().map(|s| s.to_str().unwrap()).collect()
    }

    const KACHE: &str = "/home/u/.local/fauna-tools/kache/real/bin/kache";
    const DIR: &str = "/home/u/.local/fauna-tools/kache/bin";

    #[test]
    fn windows_runs_kache_itself_inside_or_out() {
        for inside in [false, true] {
            let argv = tail(
                Platform::Windows,
                OsStr::new(r"D:\tools\kache\bin\kache.exe"),
                Path::new(r"D:\tools\kache\bin"),
                inside,
                &[],
                &Reads::default(),
            );
            assert_eq!(strs(&argv), [r"D:\tools\kache\bin\kache.exe"]);
        }
    }

    fn role(r: &str, p: &str) -> Role {
        (r.to_owned(), p.into())
    }

    #[test]
    fn mac_runs_kache_under_the_profile_beside_the_launcher_with_every_role_always() {
        let roles = [
            role("FAUNA_KACHE_STATE", "/Users/u/Library/Caches/kache"),
            role("FAUNA_STORE", "/Users/u/.local/fauna-tools/kache/store"),
        ];
        for inside in [false, true] {
            let profile = mac_profile(Path::new(DIR), None, inside);
            let argv = tail(
                Platform::Mac,
                OsStr::new(KACHE),
                &profile,
                inside,
                &roles,
                &Reads::default(),
            );
            assert_eq!(profile, Path::new(DIR).join("kache-mac.sb"));
            assert_eq!(
                strs(&argv),
                [
                    "/usr/bin/sandbox-exec",
                    "-f",
                    profile.to_str().unwrap(),
                    "-D",
                    "FAUNA_TARGET=/dev/null/fauna-unused-role",
                    "-D",
                    "FAUNA_SCRATCH=/dev/null/fauna-unused-role",
                    "-D",
                    "FAUNA_STORE=/Users/u/.local/fauna-tools/kache/store",
                    "-D",
                    "FAUNA_KACHE_STATE=/Users/u/Library/Caches/kache",
                    "-D",
                    "FAUNA_KACHE_LOG=/dev/null/fauna-unused-role",
                    "-D",
                    "FAUNA_WORKSPACE=/dev/null/fauna-unused-role",
                    KACHE
                ],
                "inside {inside}"
            );
        }
        // a scratch role is kache's TMPDIR too
        let argv = tail(
            Platform::Mac,
            OsStr::new(KACHE),
            &Path::new(DIR).join("kache-mac.sb"),
            false,
            &[role("FAUNA_SCRATCH", "/Users/u/Library/Caches/kache/tmp")],
            &Reads::default(),
        );
        assert_eq!(
            strs(&argv)[argv.len() - 3..],
            [
                "/usr/bin/env",
                "TMPDIR=/Users/u/Library/Caches/kache/tmp",
                KACHE
            ]
        );
        assert!(strs(&argv).contains(&"FAUNA_SCRATCH=/Users/u/Library/Caches/kache/tmp"));
    }

    #[test]
    fn inside_a_compile_mac_re_applies_the_snapshot_the_front_named_and_only_its_own() {
        // a reinstall mid-build swaps `kache-mac.sb`; the compile runs under the
        // snapshot the front applied, so the nested apply must take that one, or
        // macOS refuses it (`sandbox_apply: Operation not permitted`)
        let dir = Path::new(DIR);
        let hex = "0123456789abcdef".repeat(4);
        let snap = dir.join("profiles").join(format!("kache-mac-{hex}.sb"));
        let beside = dir.join("kache-mac.sb");
        let named = |p: &Path| Some(p.as_os_str().to_owned());
        assert_eq!(mac_profile(dir, named(&snap).as_deref(), true), snap);
        // outside a compile nothing is nested: the copy beside the launcher
        assert_eq!(mac_profile(dir, named(&snap).as_deref(), false), beside);
        assert_eq!(mac_profile(dir, None, true), beside);
        // never a file outside the launcher's own snapshots, nor another name
        for other in [
            PathBuf::from(format!("/tmp/profiles/kache-mac-{hex}.sb")),
            dir.join("profiles").join("kache-mac-1234.sb"),
            dir.join("profiles")
                .join(format!("kache-mac-{}.sb", hex.to_uppercase())),
            dir.join("profiles").join(format!("permissive-{hex}.sb")),
            dir.join("profiles/../profiles")
                .join(format!("kache-mac-{hex}.sb")),
            beside.clone(),
        ] {
            assert_eq!(
                mac_profile(dir, named(&other).as_deref(), true),
                beside,
                "{other:?}"
            );
        }
        let argv = tail(
            Platform::Mac,
            OsStr::new(KACHE),
            &snap,
            true,
            &[],
            &Reads::default(),
        );
        assert_eq!(
            strs(&argv)[..3],
            ["/usr/bin/sandbox-exec", "-f", snap.to_str().unwrap()]
        );
    }

    #[test]
    fn linux_outside_runs_kache_behind_the_empty_root_with_its_reads_and_the_store_writable() {
        // the shape the cargo boundary's pure test pins for the same inputs
        let reads = Reads {
            links: usr_links(|p| (p != "/lib64").then(|| Some(format!("usr{p}").into()))),
            home: Some("/home/u".into()),
            readable: vec!["/home/u/.cargo".into(), "/home/u/.rustup".into()],
        };
        let argv = tail(
            Platform::Linux,
            OsStr::new(KACHE),
            Path::new(DIR),
            false,
            &[role("FAUNA_STORE", "$CARGO_TARGET_DIR/kache/store")],
            &reads,
        );
        assert_eq!(
            strs(&argv),
            [
                "/usr/bin/bwrap",
                "--unshare-user",
                "--unshare-ipc",
                "--unshare-net",
                "--unshare-uts",
                "--unshare-cgroup-try",
                "--die-with-parent",
                "--tmpfs",
                "/",
                "--dev-bind",
                "/dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/run",
                "--tmpfs",
                "/tmp",
                "--tmpfs",
                "/dev/shm",
                "--ro-bind",
                "/usr",
                "/usr",
                "--ro-bind",
                "/etc",
                "/etc",
                "--ro-bind",
                "/sys",
                "/sys",
                "--setenv",
                "TMPDIR",
                "/tmp",
                "--seccomp",
                "9",
                "--symlink",
                "usr/bin",
                "/bin",
                "--symlink",
                "usr/lib",
                "/lib",
                "--symlink",
                "usr/sbin",
                "/sbin",
                "--dir",
                "/home/u",
                "--ro-bind-try",
                "/home/u/.cargo",
                "/home/u/.cargo",
                "--ro-bind-try",
                "/home/u/.rustup",
                "/home/u/.rustup",
                "--bind",
                "$CARGO_TARGET_DIR/kache/store",
                "$CARGO_TARGET_DIR/kache/store",
                "--remount-ro",
                "/",
                "--",
                KACHE,
            ]
        );
        let bare = tail(
            Platform::Linux,
            OsStr::new(KACHE),
            Path::new(DIR),
            false,
            &[],
            &Reads::default(),
        );
        assert_eq!(&strs(&bare)[..LINUX_BWRAP.len()], LINUX_BWRAP);
        assert_eq!(
            LINUX_BWRAP[LINUX_BWRAP.len() - LINUX_BWRAP_TAIL - 1],
            SECCOMP_FD.to_string()
        );
    }

    #[test]
    fn an_argfile_is_bound_by_itself_before_the_tail_and_only_into_linuxs_line() {
        let line = tail(
            Platform::Linux,
            OsStr::new(KACHE),
            Path::new(DIR),
            false,
            &[],
            &Reads::default(),
        );
        let bound = bind_files(&line, &["/tmp/u/kache-launcher-1.args".into()]);
        let n = line.len();
        assert_eq!(&strs(&bound)[..n - 4], &strs(&line)[..n - 4]);
        assert_eq!(
            &strs(&bound)[n - 4..],
            [
                "--ro-bind-try",
                "/tmp/u/kache-launcher-1.args",
                "/tmp/u/kache-launcher-1.args",
                "--remount-ro",
                "/",
                "--",
                KACHE
            ]
        );
        let kache_alone = vec![OsString::from(KACHE)];
        assert_eq!(bind_files(&kache_alone, &["/x".into()]), kache_alone);
    }

    #[test]
    fn a_host_directory_in_place_of_a_usr_link_is_bound_read_only() {
        assert_eq!(
            strs(&usr_links(|p| (p == "/bin").then_some(None))),
            ["--ro-bind", "/bin", "/bin"]
        );
    }

    #[test]
    fn the_read_list_keeps_only_the_topmost_of_nested_paths() {
        let paths: Vec<OsString> = ["/k/bin", "/k", "/k/real/bin", "/kx", "/k"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(strs(&topmost(&paths)), ["/k", "/kx"]);
    }

    /// The program as the pins spell it: one hex word per instruction, space-separated
    /// (one long hex run would read as a key to the publish tree's secret scanner).
    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(8)
            .map(|w| w.iter().map(|b| format!("{b:02x}")).collect::<String>())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn the_seccomp_program_is_the_pinned_bytes_for_each_machine() {
        // the pins the cargo boundary's Python copy is held equal to
        assert_eq!(hex(&seccomp_program(Arch::X86_64)), SECCOMP_PROGRAM_X86_64);
        assert_eq!(
            hex(&seccomp_program(Arch::Aarch64)),
            SECCOMP_PROGRAM_AARCH64
        );
    }

    #[test]
    fn the_seccomp_program_differs_between_machines_only_in_the_arch_and_the_numbers() {
        let x = seccomp_program(Arch::X86_64);
        let a = seccomp_program(Arch::Aarch64);
        assert_eq!(x.len(), a.len());
        assert_eq!(x.len() % 8, 0);
        let words = |p: &[u8]| -> Vec<(u16, u8, u8, u32)> {
            p.chunks(8)
                .map(|w| {
                    (
                        u16::from_le_bytes([w[0], w[1]]),
                        w[2],
                        w[3],
                        u32::from_le_bytes([w[4], w[5], w[6], w[7]]),
                    )
                })
                .collect()
        };
        let (x, a) = (words(&x), words(&a));
        let differing: Vec<usize> = (0..x.len()).filter(|&i| x[i] != a[i]).collect();
        // the arch word, socket, socketpair — io_uring_setup's number is the same
        assert_eq!(differing, [1, 8, 9]);
        assert_eq!(x[1].3, 0xC000003E);
        assert_eq!(a[1].3, 0xC00000B7);
        // every jump lands inside the program, and the last word allows
        for (i, (code, jt, jf, _)) in x.iter().enumerate() {
            if *code != 0x06 {
                assert!((i + 1 + *jt as usize) < x.len());
                assert!((i + 1 + *jf as usize) < x.len());
            }
        }
        assert_eq!(x.last().unwrap().3, 0x7FFF_0000);
        // the family check lists exactly SOCKET_FAMILIES, in order
        let families: Vec<u32> = x[11..11 + SOCKET_FAMILIES.len()]
            .iter()
            .map(|w| w.3)
            .collect();
        assert_eq!(families, SOCKET_FAMILIES);
    }

    #[test]
    fn the_store_is_read_from_the_configuration_by_its_key() {
        let config = "[cache]\n# local_store = '/not/this'\nlocal_only = true\n\
                      local_store = '$CARGO_TARGET_DIR/kache/store'  # the dataset\n";
        assert_eq!(
            store_of(config).as_deref(),
            Some("$CARGO_TARGET_DIR/kache/store")
        );
        assert_eq!(store_of("local_store = \"/s\"").as_deref(), Some("/s"));
        assert_eq!(store_of("local_store_x = '/s'"), None);
        assert_eq!(store_of("local_store = /unquoted"), None);
        assert_eq!(store_of(""), None);
    }

    #[test]
    fn linux_inside_runs_kache_itself() {
        let argv = tail(
            Platform::Linux,
            OsStr::new(KACHE),
            Path::new(DIR),
            true,
            &[role("FAUNA_STORE", "/s")],
            &Reads::default(),
        );
        assert_eq!(strs(&argv), [KACHE]);
    }

    #[test]
    fn the_inside_decision_needs_the_token_equal_to_the_namespace_and_only_lo() {
        let ns = OsStr::new("net:[4026532281]");
        let lo = vec!["lo".to_owned()];
        // inside: the token is this namespace, and the namespace has only lo
        assert!(inside(Some(ns), Some(ns), &lo));
        // a stale token: another namespace's
        assert!(!inside(Some(OsStr::new("net:[4026531840]")), Some(ns), &lo));
        // forged to the host's namespace: it has more than lo
        assert!(!inside(
            Some(ns),
            Some(ns),
            &["lo".to_owned(), "enp0s5".to_owned()]
        ));
        // no token at all (or an empty one)
        assert!(!inside(None, Some(ns), &lo));
        assert!(!inside(Some(OsStr::new("")), Some(OsStr::new("")), &lo));
        // the namespace could not be read
        assert!(!inside(Some(ns), None, &lo));
        // no interface could be read
        assert!(!inside(Some(ns), Some(ns), &[]));
    }

    #[test]
    fn interfaces_are_read_past_the_two_header_lines() {
        let only_lo = "Inter-|   Receive                            |  Transmit\n \
                       face |bytes    packets errs drop fifo frame compressed multicast|bytes\n    \
                       lo:       0       0    0    0    0     0          0         0        0\n";
        assert_eq!(interfaces(only_lo), ["lo"]);
        let host = format!("{only_lo}enp0s5: 123 4 0 0 0 0 0 0 567\n");
        assert_eq!(interfaces(&host), ["lo", "enp0s5"]);
        assert!(interfaces("").is_empty());
    }

    #[test]
    fn a_missing_tool_profile_or_kache_is_named_in_the_order_the_tail_needs_them() {
        let dir = Path::new(DIR);
        let none_exist = |_: &Path| false;
        let all_exist = |_: &Path| true;
        let kache = OsStr::new(KACHE);
        assert_eq!(
            missing(Platform::Windows, kache, dir, false, all_exist),
            None
        );
        assert_eq!(
            missing(Platform::Windows, kache, dir, false, none_exist),
            Some(PathBuf::from(KACHE))
        );
        assert_eq!(
            missing(
                Platform::Mac,
                kache,
                &dir.join("kache-mac.sb"),
                false,
                none_exist
            ),
            Some(PathBuf::from("/usr/bin/sandbox-exec"))
        );
        let profile = dir.join("kache-mac.sb");
        let no_profile = |p: &Path| p != profile;
        assert_eq!(
            missing(Platform::Mac, kache, &profile, false, no_profile),
            Some(profile.clone())
        );
        assert_eq!(
            missing(Platform::Mac, kache, &profile, true, no_profile),
            Some(profile),
            "mac never skips its profile"
        );
        assert_eq!(
            missing(Platform::Linux, kache, dir, false, none_exist),
            Some(PathBuf::from("/usr/bin/bwrap"))
        );
        assert_eq!(
            missing(Platform::Linux, kache, dir, true, none_exist),
            Some(PathBuf::from(KACHE)),
            "inside needs no bwrap"
        );
        assert_eq!(missing(Platform::Linux, kache, dir, false, all_exist), None);
    }
}
