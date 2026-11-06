//! kache-launcher — cargo's `RUSTC_WRAPPER` on every machine kache wraps
//! (docs/goal/architecture/build-compile-cache.md § The launcher — every link
//! argfile-clean, one program on three machines; *Stage 2's design*).
//!
//! Cargo starts a wrapper as `<wrapper> <rustc> <args…>`, and hands it a command
//! line or an `@argfile` by whether ITS OWN start of the wrapper fits the OS limit.
//! What the wrapper adds is not counted: kache adds its remap flags, so a line that
//! fits for cargo can overflow for kache, whose start of rustc then fails — and
//! kache 1.2.2 compiles the unit unwrapped and stores nothing, exit 0, no warning.
//! This program closes that seam at the one link we own: it writes every argument
//! after the compiler path to an argfile, ALWAYS (one code path on every machine,
//! one form of key in the store), and runs kache behind the machine's boundary as
//! `<tail> <rustc> @<file>`, stdio inherited; it waits, removes the file and exits
//! with the tail's code.
//!
//! - The tail is the platform's (`tail.rs`): kache itself on Windows (its outbound
//!   block is the firewall rule on the program); `sandbox-exec` with the profile
//!   beside this program on macOS; a `bwrap --unshare-net` namespace on Linux, or
//!   kache itself when the front's namespace already holds this process. The binary
//!   is `FAUNA_KACHE_EXE`, never defaulted (the install layout differs per machine),
//!   and `KACHE_CONFIG` must be set — unset, kache would run on its default store,
//!   outside every bound. A missing variable, binary, tool or profile is one line,
//!   exit 2, nothing run: never an unsandboxed kache, never rustc unwrapped, which
//!   is the poison this program removes. (On Windows alone, a front from before
//!   stage 2 still names the binary `FAUNA_KACHE_LAUNCH`; it is read when
//!   `FAUNA_KACHE_EXE` is unset, until the stage-2 row closes — `STAGE1_WIN_TAIL_VAR`.)
//! - `--kache <args…>` runs the same tail with `<args…>` and no argfile: every
//!   non-compile start of kache (`report`, `gc --json`, `daemon stop`, `doctor`,
//!   `--version`) goes through here, so this program is the only way any kache runs.
//!   Cargo's first argument to a wrapper is the compiler's path, never an option.
//! - `--contract` prints the number the build fronts and this program agree on
//!   (decision 5): a front wraps only through a launcher answering at least its own.
//!   Bumped on every change to what the two agree on.
//! - One decision per compile, and only one: a WORKSPACE LINK PRODUCT — a unit of a
//!   package cargo was asked to build (`CARGO_PRIMARY_PACKAGE`) whose crate types
//!   include `cdylib` or `staticlib` — runs as `<rustc> @<file>` with no tail. Like
//!   an executable, which kache passes through under the owner's
//!   `cache_executables = false` (2026-11-02), it is the link product of the whole
//!   graph and carries the checkout's commit stamp, so its key is new at every
//!   commit and kache would only store it, never hit it (the release `fauna_ffi`
//!   staticlib+cdylib: 938 MiB a miss). Its dependencies still go through the tail.
//!   A registry crate that declares a cdylib is never primary, so it stays cached.
//!   The crate types are read through cargo's own `@argfile` too — on Windows the
//!   release `fauna_ffi` cdylib's line is past the OS limit and reaches the
//!   launcher only that way — and rustc then runs that file as cargo wrote it.
//!   The binary must still be named: a front that sets no kache wraps nothing.
//! - Passed through unchanged: an argument that already starts with `@` (cargo's
//!   own argfile, past the limit), or one the argfile grammar cannot carry — a line
//!   break (rustc reads an argfile as UTF-8 and splits it by `str::lines`) or a
//!   string that is not Unicode.
//! - The argfile is uniquely named in the OS temp directory (pid, counter, time),
//!   created new, and removed after the tail exits, whatever its code.

mod tail;

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tail::Platform;

/// The variable naming the kache binary the tail runs.
const EXE_VAR: &str = "FAUNA_KACHE_EXE";
/// The stage-1 win fronts' name for the same binary (their tail was kache itself).
/// The launcher is installed machine-wide while each checkout's fronts are its own,
/// so a win checkout not yet rebased past stage 2 still sets this and not
/// `EXE_VAR`; read on Windows alone and only when `EXE_VAR` is unset, so it keeps
/// compiling. Retired with the stage-2 row's close (build-compile-cache.md § The
/// launcher, *Stage 2's design*, decision 6).
const STAGE1_WIN_TAIL_VAR: &str = "FAUNA_KACHE_LAUNCH";
/// kache's own configuration variable; the launcher requires it set.
const CONFIG_VAR: &str = "KACHE_CONFIG";
/// What `--contract` answers: the fronts' `_KACHE_LAUNCHER_CONTRACT` must not
/// exceed it.
const CONTRACT: u32 = 2;
/// Set by cargo on every unit of a package the command line selected.
const PRIMARY_VAR: &str = "CARGO_PRIMARY_PACKAGE";
/// The crate types that make a primary unit a link product compiled without kache.
const LINK_PRODUCT_TYPES: [&str; 2] = ["cdylib", "staticlib"];
/// The launcher's own failures: nothing named, something missing, no argfile.
const LAUNCH_FAILED: i32 = 2;

fn main() {
    let mut argv = std::env::args_os().skip(1);
    let Some(first) = argv.next() else {
        fail(format_args!(
            "usage: kache-launcher <rustc> <args…> (cargo's RUSTC_WRAPPER) | --kache <args…> | --contract | --version"
        ));
    };
    let args: Vec<OsString> = argv.collect();
    if args.is_empty() && first == "--version" {
        println!("kache-launcher {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.is_empty() && first == "--contract" {
        println!("{CONTRACT}");
        return;
    }
    let stage1 = || cfg!(windows).then(|| named(STAGE1_WIN_TAIL_VAR)).flatten();
    let Some(kache) = named(EXE_VAR).or_else(stage1) else {
        fail(format_args!(
            "kache-launcher: {EXE_VAR} is not set — it names the kache binary to run; nothing was run"
        ));
    };
    if first == "--kache" {
        let tail = resolve_tail(&kache);
        let mut cmd = Command::new(&tail[0]);
        cmd.args(&tail[1..]).args(&args);
        let _filter = arm_seccomp(&mut cmd, &tail);
        process::exit(run(&mut cmd, &tail[0]));
    }
    let rustc = first;
    let primary = named(PRIMARY_VAR).is_some();
    if primary && is_link_product(&args) {
        process::exit(launch(None, &rustc, &args));
    }
    let tail = resolve_tail(&kache);
    process::exit(launch(Some(&tail), &rustc, &args));
}

/// A variable's value when it is set and not empty.
fn named(var: &str) -> Option<OsString> {
    std::env::var_os(var).filter(|v| !v.is_empty())
}

/// The argv that starts kache on this machine, or a failed launch naming what is
/// missing — checked before anything runs or any argfile is written.
fn resolve_tail(kache: &OsStr) -> Vec<OsString> {
    let Some(platform) = Platform::current() else {
        fail(format_args!(
            "kache-launcher: no kache tail for this platform; nothing was run"
        ));
    };
    if named(CONFIG_VAR).is_none() {
        fail(format_args!(
            "kache-launcher: {CONFIG_VAR} is not set — kache would run on its default store, \
             outside every bound; nothing was run"
        ));
    }
    let dir = match std::env::current_exe() {
        Ok(exe) => exe.parent().map(PathBuf::from).unwrap_or_default(),
        Err(err) => fail(format_args!(
            "kache-launcher: cannot read its own path ({err}); nothing was run"
        )),
    };
    let inside = platform == Platform::Linux && linux_inside();
    let mac_inside = named(tail::INSIDE_VAR).as_deref() == Some(OsStr::new("1"));
    let profile = tail::mac_profile(&dir, named(tail::PROFILE_VAR).as_deref(), mac_inside);
    if let Some(path) = tail::missing(platform, kache, &profile, inside, |p| p.is_file()) {
        fail(format_args!(
            "kache-launcher: {} is missing; nothing was run",
            path.display()
        ));
    }
    let (writable, reads) = match platform {
        Platform::Linux if !inside => (kache_roles(platform), linux_reads(kache, &dir)),
        Platform::Mac => (mac_roles(), tail::Reads::default()),
        _ => (Vec::new(), tail::Reads::default()),
    };
    tail::tail(platform, kache, &profile, inside, &writable, &reads)
}

/// linux's read list outside the front's boundary (build-compile-cache.md § The
/// boundary's scope and what OFF means, decision (8)): `CARGO_HOME` (default
/// `~/.cargo`) and `RUSTUP_HOME` (default `~/.rustup`) — the compiler kache probes
/// and runs — and the directories of the kache binary, this launcher and the
/// configuration `KACHE_CONFIG` names, the topmost of each nest; `$HOME` made as
/// an empty directory; the host's `USR_LINKS` part read off `/`.
fn linux_reads(kache: &OsStr, launcher_dir: &std::path::Path) -> tail::Reads {
    let home = named("HOME");
    let mut readable: Vec<OsString> = Vec::new();
    for (var, default) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        if let Some(path) = named(var) {
            readable.push(path);
        } else if let Some(home) = &home {
            readable.push(PathBuf::from(home).join(default).into_os_string());
        }
    }
    let config = named(CONFIG_VAR).map(PathBuf::from);
    for file in [Some(PathBuf::from(kache)), config].into_iter().flatten() {
        if let Some(parent) = file.parent() {
            readable.push(parent.as_os_str().to_owned());
        }
    }
    readable.push(launcher_dir.as_os_str().to_owned());
    let links = tail::usr_links(|path| match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::read_link(path).ok().map(|t| Some(t.into_os_string()))
        }
        Ok(meta) if meta.is_dir() => Some(None),
        _ => None,
    });
    tail::Reads {
        links,
        home,
        readable: tail::topmost(&readable),
    }
}

/// mac's roles (build-compile-cache.md § The boundary's scope and what OFF means,
/// decision (7)): the store, kache's state and log directories (`kache_roles`), a
/// scratch under the state directory (kache's `TMPDIR`), and —
/// inside the front's boundary on cargo (`FAUNA_KACHE_INSIDE` is `1`) — every role
/// the front set in this process's environment, which wins: macOS refuses a nested
/// `sandbox-exec` unless its profile and parameters equal the one this process
/// runs under, so inside a compile the tail repeats the front's roles exactly
/// (rustc then writes the compile's target and scratch through them).
fn mac_roles() -> Vec<tail::Role> {
    let mut roles = kache_roles(Platform::Mac);
    // outside a compile (`--kache`, and the `rustc -vV` probe of the front's `cargo
    // fetch` prelude) kache's scratch is `tmp` under its state directory — linux's
    // tail gives it a private `/tmp`; the profile refuses the user's temp directory
    // (kache's `kache-rustc-*.args`, measured on macOS 2026-11-05)
    if let Some(state) = kache_state_dir(Platform::Mac) {
        let scratch = state.join("tmp");
        let _ = fs::create_dir_all(&scratch);
        roles.push(("FAUNA_SCRATCH".to_owned(), scratch.into_os_string()));
    }
    if named(tail::INSIDE_VAR).as_deref() == Some(OsStr::new("1")) {
        for role in tail::MAC_ROLES {
            if let Some(path) = named(role) {
                roles.retain(|(r, _)| r != role);
                roles.push((role.to_owned(), path));
            }
        }
    }
    roles
}

/// kache's own state directory: on linux `$XDG_CACHE_HOME/kache`, else
/// `~/.cache/kache` (its `kache.log` and probe digests, measured on Linux
/// 2026-11-05); on macOS `~/Library/Caches/kache` (the probes and the rustc, sysroot
/// and linker digests, measured on macOS 2026-11-05). The slot front derives the same
/// directory.
fn kache_state_dir(platform: Platform) -> Option<PathBuf> {
    let home = named("HOME").map(PathBuf::from);
    if platform == Platform::Mac {
        return home.map(|h| h.join("Library").join("Caches").join("kache"));
    }
    named("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|c| c.join("kache"))
}

/// kache's writable set outside the front's boundary (build-compile-cache.md
/// § The boundary's scope and what OFF means, decision (7)): the store
/// `KACHE_CONFIG` names (`FAUNA_STORE`), kache's own state directory
/// (`FAUNA_KACHE_STATE`, `kache_state_dir`) and on macOS its log directory
/// `~/Library/Logs/kache` (`FAUNA_KACHE_LOG`), each made first so `bwrap` has a
/// directory to bind. A configuration naming no store is a failed launch: kache
/// would write its default store, which the boundary leaves read-only.
fn kache_roles(platform: Platform) -> Vec<tail::Role> {
    let config = named(CONFIG_VAR).unwrap_or_default();
    let store = fs::read_to_string(&config)
        .ok()
        .and_then(|text| tail::store_of(&text));
    let Some(store) = store else {
        fail(format_args!(
            "kache-launcher: {} names no local_store; nothing was run",
            PathBuf::from(config).display()
        ));
    };
    let mut set = vec![("FAUNA_STORE".to_owned(), OsString::from(store))];
    set.extend(
        kache_state_dir(platform).map(|c| ("FAUNA_KACHE_STATE".to_owned(), c.into_os_string())),
    );
    if platform == Platform::Mac {
        // kache.log: under the state directory on linux, its own on macOS (measured
        // 2026-11-05: `~/Library/Logs/kache/kache.log`, refused otherwise)
        set.extend(named("HOME").map(|h| {
            let logs = PathBuf::from(h).join("Library").join("Logs").join("kache");
            ("FAUNA_KACHE_LOG".to_owned(), logs.into_os_string())
        }));
    }
    for (_, path) in &set {
        let _ = fs::create_dir_all(path);
    }
    set
}

/// linux's two readings for `tail::inside`, taken only when the token is set.
fn linux_inside() -> bool {
    let Some(token) = std::env::var_os(tail::INSIDE_VAR) else {
        return false;
    };
    let netns = fs::read_link("/proc/self/ns/net")
        .ok()
        .map(PathBuf::into_os_string);
    let interfaces = fs::read_to_string("/proc/self/net/dev")
        .map(|text| tail::interfaces(&text))
        .unwrap_or_default();
    tail::inside(Some(&token), netns.as_deref(), &interfaces)
}

/// Whether rustc's arguments ask for a `cdylib` or `staticlib` (`--crate-type X`,
/// `--crate-type=X`, X a comma-separated list). An argument naming cargo's own
/// `@argfile` stands for that file's lines, read as rustc reads them (UTF-8, one
/// argument a line): on Windows the release `fauna_ffi` cdylib's line is past the
/// OS limit, so cargo hands the unit over that way. A file that cannot be read
/// adds nothing, and such a unit goes to the tail, as before. Called only for a
/// primary unit, so no other unit's argfile is read.
fn is_link_product(args: &[OsString]) -> bool {
    let mut words: Vec<String> = Vec::new();
    for arg in args.iter().filter_map(|a| a.to_str()) {
        match arg.strip_prefix('@') {
            Some(path) => {
                if let Ok(text) = fs::read_to_string(path) {
                    words.extend(text.lines().map(str::to_owned));
                }
            }
            None => words.push(arg.to_owned()),
        }
    }
    let mut next_is_type = false;
    words.iter().map(String::as_str).any(|a| {
        let value = if next_is_type {
            next_is_type = false;
            Some(a)
        } else if a == "--crate-type" {
            next_is_type = true;
            None
        } else {
            a.strip_prefix("--crate-type=")
        };
        value.is_some_and(|v| v.split(',').any(|t| LINK_PRODUCT_TYPES.contains(&t.trim())))
    })
}

fn fail(message: std::fmt::Arguments) -> ! {
    eprintln!("{message}");
    process::exit(LAUNCH_FAILED);
}

/// Puts the system-call filter on `tail::SECCOMP_FD` for a tail that starts with
/// `bwrap` — linux, outside the front's boundary (build-compile-cache.md § The
/// boundary's scope and what OFF means, decision (1)): the program's bytes in a
/// pipe whose read end the child dups onto the descriptor before it execs; the
/// reader returned lives until the tail has started. A machine with no program
/// is a failed launch, never a kache outside the filter. Any other tail: None.
fn arm_seccomp(cmd: &mut Command, tail: &[OsString]) -> Option<io::PipeReader> {
    if tail.first().map(OsString::as_os_str) != Some(OsStr::new(tail::LINUX_BWRAP[0])) {
        return None;
    }
    arm_seccomp_fd(cmd)
}

#[cfg(unix)]
fn arm_seccomp_fd(cmd: &mut Command) -> Option<io::PipeReader> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn dup2(oldfd: i32, newfd: i32) -> i32;
    }
    let Some(arch) = tail::Arch::current() else {
        fail(format_args!(
            "kache-launcher: no system-call filter for this machine; nothing was run"
        ));
    };
    let (reader, mut writer) = match io::pipe() {
        Ok(pipe) => pipe,
        Err(err) => fail(format_args!(
            "kache-launcher: cannot open the filter's pipe ({err}); nothing was run"
        )),
    };
    if let Err(err) = writer.write_all(&tail::seccomp_program(arch)) {
        fail(format_args!(
            "kache-launcher: cannot write the filter ({err}); nothing was run"
        ));
    }
    drop(writer);
    let fd = reader.as_raw_fd();
    // SAFETY: dup2 is async-signal-safe and touches nothing but the child's
    // descriptor table; `reader` outlives the spawn (the caller holds it).
    unsafe {
        cmd.pre_exec(move || {
            if dup2(fd, tail::SECCOMP_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Some(reader)
}

#[cfg(not(unix))]
fn arm_seccomp_fd(_cmd: &mut Command) -> Option<io::PipeReader> {
    None
}

/// Runs one compile — through the tail, or with no tail as rustc itself (a
/// workspace link product) — and returns the exit code to leave with.
fn launch(tail: Option<&[OsString]>, rustc: &OsStr, args: &[OsString]) -> i32 {
    // linux's tail shows kache nothing of the host's temp directory, where this
    // program's argfile and cargo's own sit: each is bound into it by itself,
    // read-only (`tail::bind_files`)
    let start = |files: &[OsString], rest: &mut dyn FnMut(&mut Command)| -> i32 {
        let (program, mut cmd, filter) = match tail {
            Some(tail) => {
                let tail = tail::bind_files(tail, files);
                let mut cmd = Command::new(&tail[0]);
                cmd.args(&tail[1..]).arg(rustc);
                let filter = arm_seccomp(&mut cmd, &tail);
                (tail[0].clone(), cmd, filter)
            }
            None => (rustc.to_owned(), Command::new(rustc), None),
        };
        rest(&mut cmd);
        let code = run(&mut cmd, &program);
        drop(filter);
        code
    };
    let cargo_files: Vec<OsString> = args
        .iter()
        .filter_map(|a| a.to_str()?.strip_prefix('@').map(OsString::from))
        .collect();
    let Some(lines) = argfile_lines(args) else {
        return start(&cargo_files, &mut |cmd| {
            cmd.args(args);
        });
    };
    let (path, file) = match create_argfile() {
        Ok(created) => created,
        Err(err) => fail(format_args!(
            "kache-launcher: cannot create an argfile in {}: {err}",
            std::env::temp_dir().display()
        )),
    };
    if let Err(err) = write_lines(file, &lines) {
        let _ = fs::remove_file(&path);
        fail(format_args!(
            "kache-launcher: cannot write {}: {err}",
            path.display()
        ));
    }
    let mut at = OsString::from("@");
    at.push(&path);
    let code = start(&[path.clone().into_os_string()], &mut |cmd| {
        cmd.arg(&at);
    });
    let _ = fs::remove_file(&path);
    code
}

/// The argfile's lines, one per argument — or None when the arguments go to the
/// tail as they are: none at all, cargo's own argfile, or an argument the argfile
/// grammar cannot carry.
fn argfile_lines(args: &[OsString]) -> Option<Vec<&str>> {
    if args.is_empty() {
        return None;
    }
    args.iter()
        .map(|a| {
            a.to_str()
                .filter(|s| !s.starts_with('@') && !s.contains(['\n', '\r']))
        })
        .collect()
}

fn create_argfile() -> io::Result<(PathBuf, File)> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir();
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("kache-launcher-{}-{n}-{nanos}.args", process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists && n < 64 => continue,
            Err(err) => return Err(err),
        }
    }
}

fn write_lines(file: File, lines: &[&str]) -> io::Result<()> {
    let mut out = io::BufWriter::new(file);
    for line in lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    // the tail reads it through the OS cache: flushed, never synced to disk
    out.flush()
}

/// Starts the tail (or rustc) with stdio inherited and waits: its exit code,
/// 128 + the signal for one a signal ended (unix, as a shell reports it), or
/// LAUNCH_FAILED when it could not start.
fn run(cmd: &mut Command, program: &OsStr) -> i32 {
    let status = match cmd.status() {
        Ok(status) => status,
        Err(err) => {
            eprintln!(
                "kache-launcher: cannot run {}: {err}",
                program.to_string_lossy()
            );
            return LAUNCH_FAILED;
        }
    };
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    LAUNCH_FAILED
}
