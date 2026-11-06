//! The launcher's acceptance items, docs/goal/architecture/build-compile-cache.md
//! § The launcher, *What is pinned* (1)–(4) and *Stage 2's acceptance* (c)–(e): the
//! launcher is run as cargo runs a wrapper, `<launcher> <rustc> <args…>`, with
//! `FAUNA_KACHE_EXE` naming `fake-tail` (this crate's stand-in for kache), which
//! reports the argv it was started with and each `@file`'s lines as JSON. Each run
//! gets a fresh, empty temp directory, so the argfile's place and its removal are
//! observable. The tail is the platform's own: kache itself on Windows, under
//! `sandbox-exec` on macOS, in a `bwrap` namespace on Linux — so on every host the
//! recorder sees `<rustc> @<file>`, behind that host's boundary. The pure shapes of
//! every platform are `src/tail.rs`'s unit tests.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_kache-launcher");
const FAKE_TAIL: &str = env!("CARGO_BIN_EXE_fake-tail");
const RUSTC: &str = r"C:\Users\u\.rustup\toolchains\nightly-x\bin\rustc.exe";
/// The sandbox profile for macOS runs: the tree's own,
/// which `just kache-launcher-install` copies beside the launcher — so every role
/// it reads must be one the tail passes (an unset parameter fails the profile).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PROFILE: &str = include_str!("../../../scripts/kache-mac.sb");

/// The launcher to run. On macOS its tail needs the profile beside it, as
/// `just kache-launcher-install` places it: a copy of the built launcher in a
/// directory of its own, with `PROFILE` beside it. Elsewhere the built binary
/// itself.
fn launcher() -> PathBuf {
    if !cfg!(target_os = "macos") {
        return PathBuf::from(LAUNCHER);
    }
    static INSTALLED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "kache-launcher-test-install-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let exe = dir.join("kache-launcher");
            std::fs::copy(LAUNCHER, &exe).unwrap();
            std::fs::write(dir.join("kache-mac.sb"), PROFILE).unwrap();
            exe
        })
        .clone()
}

/// A fresh, empty directory, handed to the launcher as its temp directory. On
/// linux always under `/tmp`, the tree the tail's line replaces with an empty
/// tmpfs, whatever this machine's `TMPDIR` says: an argfile there reaches kache
/// only through its own bind (`tail::bind_files`). A runner with no `TMPDIR` is
/// in `/tmp` already, so a test that followed `TMPDIR` passed on a dev machine
/// pointing it elsewhere and failed only on the runner.
fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let base = if cfg!(target_os = "linux") {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!(
        "kache-launcher-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What one run of the launcher is given beyond its arguments.
struct Env<'a> {
    /// `FAUNA_KACHE_EXE` — the binary the tail runs
    kache: Option<&'a str>,
    /// `KACHE_CONFIG` set — to a configuration naming a store, both outside the
    /// run's temp directory (the tail binds or passes the store; the fake tail
    /// ignores it), with `HOME` beside them for kache's state directory
    config: bool,
    /// cargo's `CARGO_PRIMARY_PACKAGE`
    primary: bool,
    /// the fake tail's exit code
    tail_exit: Option<i32>,
    /// `FAUNA_KACHE_LAUNCH` — the stage-1 win fronts' name for the binary
    stage1: Option<&'a str>,
}

impl Env<'_> {
    fn tail(kache: Option<&str>) -> Env<'_> {
        Env {
            kache,
            config: true,
            primary: false,
            tail_exit: None,
            stage1: None,
        }
    }
}

fn launch(tmp: &Path, tail: Option<&str>, args: &[&str], tail_exit: Option<i32>) -> Output {
    run(
        tmp,
        args,
        Env {
            tail_exit,
            ..Env::tail(tail)
        },
    )
}

/// As `launch`, with cargo's `CARGO_PRIMARY_PACKAGE` set when `primary`.
fn launch_as(
    tmp: &Path,
    tail: Option<&str>,
    args: &[&str],
    tail_exit: Option<i32>,
    primary: bool,
) -> Output {
    run(
        tmp,
        args,
        Env {
            tail_exit,
            primary,
            ..Env::tail(tail)
        },
    )
}

fn run(tmp: &Path, args: &[&str], env: Env) -> Output {
    let mut cmd = Command::new(launcher());
    cmd.args(args)
        .env("TMP", tmp)
        .env("TEMP", tmp)
        .env("TMPDIR", tmp)
        .env_remove("FAUNA_KACHE_EXE")
        .env_remove("FAUNA_KACHE_LAUNCH")
        .env_remove("FAUNA_KACHE_INSIDE")
        .env_remove("KACHE_CONFIG")
        .env_remove("CARGO_PRIMARY_PACKAGE")
        .env_remove("FAKE_TAIL_EXIT");
    if let Some(kache) = env.kache {
        cmd.env("FAUNA_KACHE_EXE", kache);
    }
    if let Some(stage1) = env.stage1 {
        cmd.env("FAUNA_KACHE_LAUNCH", stage1);
    }
    if env.config {
        let side = PathBuf::from(format!("{}-side", tmp.display()));
        let _ = std::fs::remove_dir_all(&side);
        std::fs::create_dir_all(&side).unwrap();
        let config = side.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "[cache]\nlocal_store = '{}'\n",
                side.join("store").display()
            ),
        )
        .unwrap();
        cmd.env("KACHE_CONFIG", config)
            .env("HOME", side.join("home"))
            .env_remove("XDG_CACHE_HOME");
    }
    if env.primary {
        cmd.env("CARGO_PRIMARY_PACKAGE", "1");
    }
    if let Some(code) = env.tail_exit {
        cmd.env("FAKE_TAIL_EXIT", code.to_string());
    }
    cmd.output().unwrap()
}

fn left_in(tmp: &Path) -> Vec<String> {
    std::fs::read_dir(tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// A failed launch: exit 2, one line on stderr containing `names`, nothing run
/// (the fake tail printed nothing) and no argfile left.
fn assert_failed_launch(out: &Output, tmp: &Path, names: &str) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "something ran: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(names), "{stderr}");
    assert!(left_in(tmp).is_empty(), "{:?}", left_in(tmp));
}

/// What the fake tail reported: its argv, and each `@file` with its lines (None when
/// the file could not be read).
struct Seen {
    argv: Vec<String>,
    files: Vec<(String, Option<Vec<String>>)>,
}

fn seen(out: &Output) -> Seen {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    let line = stdout.lines().last().unwrap_or_else(|| {
        panic!(
            "the tail printed nothing; launcher stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let mut p = Parser {
        s: line.as_bytes(),
        i: 0,
    };
    let doc = p.value();
    let argv = doc.get("argv").strings();
    let files = doc
        .get("files")
        .items()
        .iter()
        .map(|f| {
            let lines = match f.get("lines") {
                Json::Null => None,
                lines => Some(lines.strings()),
            };
            (f.get("path").string(), lines)
        })
        .collect();
    Seen { argv, files }
}

#[test]
fn every_compile_reaches_the_tail_as_rustc_and_one_argfile_of_its_arguments() {
    // (1): `<rustc> <args…>` → the tail runs `<rustc> @<file>`, the file's lines are
    // the args, and the file is gone after the launcher exits
    let tmp = scratch();
    let args = [
        RUSTC,
        "--crate-name",
        "fauna_log",
        "--edition=2024",
        r"libs\fauna-log\src\lib.rs",
        "--cfg",
        r#"feature="std""#,
        "a value with spaces",
        "",
        "-L",
        r"dependency=D:\src\checkout\target\debug\build\c2pa\0123456789abcdef\out",
    ];
    let out = launch(&tmp, Some(FAKE_TAIL), &args, None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = seen(&out);
    assert_eq!(seen.argv.len(), 2, "argv: {:?}", seen.argv);
    assert_eq!(seen.argv[0], RUSTC);
    let at = seen.argv[1]
        .strip_prefix('@')
        .expect("the second argument is an argfile");
    assert_eq!(
        Path::new(at).parent(),
        Some(tmp.as_path()),
        "the argfile sits in the temp dir"
    );
    assert_eq!(seen.files.len(), 1);
    let want: Vec<String> = args[1..].iter().map(|a| a.to_string()).collect();
    assert_eq!(
        seen.files[0].1.as_ref(),
        Some(&want),
        "the argfile's lines are the args, in order"
    );
    assert!(
        left_in(&tmp).is_empty(),
        "the argfile outlived the launcher: {:?}",
        left_in(&tmp)
    );
}

#[test]
fn an_argfile_or_a_line_break_already_present_is_passed_through_unchanged() {
    // (2): cargo's own argfile (past the limit), or an argument the argfile grammar
    // cannot carry, reaches the tail exactly as cargo gave it
    for args in [
        vec![RUSTC, r"@C:\Users\u\AppData\Local\Temp\cargo-argfile.txt"],
        vec![RUSTC, "--crate-name", "x", "@args.txt", "--edition=2024"],
        vec![RUSTC, "--cfg", "a\nb"],
        vec![RUSTC, "--cfg", "a\r"],
    ] {
        let tmp = scratch();
        let out = launch(&tmp, Some(FAKE_TAIL), &args, None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seen = seen(&out);
        assert_eq!(
            seen.argv,
            args.iter().map(|a| a.to_string()).collect::<Vec<_>>()
        );
        assert!(
            left_in(&tmp).is_empty(),
            "a pass-through wrote an argfile: {:?}",
            left_in(&tmp)
        );
    }
}

#[test]
fn the_tails_exit_code_and_stderr_are_the_launchers() {
    // (3): exit 3 from the tail → 3 from the launcher, stderr intact; the argfile
    // still goes
    let tmp = scratch();
    let out = launch(
        &tmp,
        Some(FAKE_TAIL),
        &[RUSTC, "--crate-name", "x"],
        Some(3),
    );
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fake-tail: this line is the tail's stderr"),
        "{stderr}"
    );
    assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
}

#[test]
fn no_kache_named_is_a_failed_compile_with_one_line_naming_the_variable() {
    // (4), (e): without FAUNA_KACHE_EXE nothing runs — never rustc unwrapped
    for kache in [None, Some("")] {
        let tmp = scratch();
        let out = launch(&tmp, kache, &[RUSTC, "--crate-name", "x"], None);
        assert_failed_launch(&out, &tmp, "FAUNA_KACHE_EXE");
    }
}

#[test]
fn a_stage1_win_front_still_compiles_until_its_checkout_rebases() {
    // the launcher is machine-wide, the fronts per checkout: a win checkout from
    // before stage 2 names kache in FAUNA_KACHE_LAUNCH, and on Windows that is read
    // when FAUNA_KACHE_EXE is unset — nowhere else, and never over FAUNA_KACHE_EXE
    let tmp = scratch();
    let env = Env {
        stage1: Some(FAKE_TAIL),
        ..Env::tail(None)
    };
    let out = run(&tmp, &[RUSTC, "--crate-name", "x"], env);
    if cfg!(windows) {
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seen = seen(&out);
        assert_eq!(seen.argv.len(), 2, "argv: {:?}", seen.argv);
        assert_eq!(seen.argv[0], RUSTC);
        assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
    } else {
        assert_failed_launch(&out, &tmp, "FAUNA_KACHE_EXE");
    }
    let tmp = scratch();
    let missing = tmp.join("no-such-kache.exe");
    let env = Env {
        stage1: Some(missing.to_str().unwrap()),
        ..Env::tail(Some(FAKE_TAIL))
    };
    let out = run(&tmp, &[RUSTC, "--crate-name", "x"], env);
    assert_eq!(
        out.status.code(),
        Some(0),
        "FAUNA_KACHE_EXE wins: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn no_kache_config_is_a_failed_compile_with_one_line_naming_the_variable() {
    // unset, kache would run on its default store, outside every bound
    let tmp = scratch();
    let env = Env {
        config: false,
        ..Env::tail(Some(FAKE_TAIL))
    };
    let out = run(&tmp, &[RUSTC, "--crate-name", "x"], env);
    assert_failed_launch(&out, &tmp, "KACHE_CONFIG");
}

#[test]
fn a_missing_kache_is_a_failed_compile_and_leaves_no_argfile() {
    // (e): checked before anything is written or started
    let tmp = scratch();
    let missing = tmp.join("no-such-kache.exe");
    let out = launch(
        &tmp,
        Some(missing.to_str().unwrap()),
        &[RUSTC, "--crate-name", "x"],
        None,
    );
    assert_failed_launch(&out, &tmp, "no-such-kache");
}

#[cfg(target_os = "macos")]
#[test]
fn a_missing_profile_beside_the_launcher_is_a_failed_compile() {
    // (e): the built launcher has no kache-mac.sb beside it — never an unsandboxed kache
    let tmp = scratch();
    let mut cmd = Command::new(LAUNCHER);
    cmd.args([RUSTC, "--crate-name", "x"])
        .env("TMPDIR", &tmp)
        .env("FAUNA_KACHE_EXE", FAKE_TAIL)
        .env("KACHE_CONFIG", tmp.join("no-such-config.toml"))
        .env_remove("CARGO_PRIMARY_PACKAGE");
    let out = cmd.output().unwrap();
    assert_failed_launch(&out, &tmp, "kache-mac.sb");
}

#[test]
fn kache_args_reach_the_tail_as_they_are_with_no_argfile() {
    // (d): `--kache <args…>` — every non-compile start of kache — runs the tail with
    // the arguments and nothing else, exit code included
    let tmp = scratch();
    let args = [
        "--kache", "report", "--format", "json", "--since", "5m", "--root", r"D:\a b",
    ];
    let out = launch(&tmp, Some(FAKE_TAIL), &args, Some(4));
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = seen(&out);
    assert_eq!(seen.argv, args[1..].to_vec());
    assert!(seen.files.is_empty());
    assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
}

#[test]
fn kache_args_without_kache_named_run_nothing() {
    let tmp = scratch();
    let out = launch(&tmp, None, &["--kache", "--version"], None);
    assert_failed_launch(&out, &tmp, "FAUNA_KACHE_EXE");
}

#[test]
fn contract_prints_the_number_with_nothing_named() {
    // (decision 5): the fronts wrap only through a launcher answering at least theirs
    let tmp = scratch();
    let out = run(
        &tmp,
        &["--contract"],
        Env {
            config: false,
            ..Env::tail(None)
        },
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "2");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_workspace_link_product_runs_as_rustc_and_one_argfile_with_no_tail() {
    // a primary unit asking for a cdylib or staticlib — the release `fauna_ffi`
    // staticlib+cdylib — never reaches kache: `fake-tail` stands in for rustc
    // here, and the kache named is one that cannot start, so reaching it fails
    let forms: [&[&str]; 4] = [
        &["--crate-type", "staticlib", "--crate-type", "cdylib"],
        &["--crate-type=cdylib"],
        &["--crate-type", "lib,staticlib"],
        &["--crate-type", "rlib", "--crate-type", "cdylib"],
    ];
    for form in forms {
        let tmp = scratch();
        let missing = tmp.join("no-such-kache");
        let mut args = vec![FAKE_TAIL, "--crate-name", "fauna_ffi"];
        args.extend_from_slice(form);
        args.push("libs/fauna-ffi-cdylib/src/lib.rs");
        let out = launch_as(&tmp, Some(missing.to_str().unwrap()), &args, None, true);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{form:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seen = seen(&out);
        assert_eq!(seen.argv.len(), 1, "{form:?}: argv {:?}", seen.argv);
        let at = seen.argv[0].strip_prefix('@').expect("one argfile");
        let lines = seen.files[0].1.clone().expect("the argfile was readable");
        assert_eq!(seen.files[0].0, at);
        assert_eq!(lines, args[1..].to_vec(), "{form:?}");
        assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
    }
}

/// Cargo's own argfile for one unit, outside the run's temp directory (which must
/// be empty after the run): its lines are the arguments, as cargo writes them.
fn cargo_argfile(tmp: &Path, args: &[&str]) -> String {
    let side = PathBuf::from(format!("{}-cargo", tmp.display()));
    let _ = std::fs::remove_dir_all(&side);
    std::fs::create_dir_all(&side).unwrap();
    let file = side.join("cargo-argfile.txt");
    let text: String = args.iter().map(|a| format!("{a}\n")).collect();
    std::fs::write(&file, text).unwrap();
    format!("@{}", file.display())
}

#[test]
fn a_workspace_link_product_behind_cargos_own_argfile_runs_as_rustc_with_no_tail() {
    // on Windows the release `fauna_ffi` cdylib's line is past the OS limit (90,355
    // characters, 2026-11-05), so cargo hands the launcher `<rustc> @<its file>`;
    // the crate types are read from that file, and rustc runs it as cargo wrote it
    let forms: [&[&str]; 2] = [
        &["--crate-type", "cdylib"],
        &["--crate-type=staticlib,cdylib"],
    ];
    for form in forms {
        let tmp = scratch();
        let missing = tmp.join("no-such-kache");
        let mut lines = vec!["--crate-name", "fauna_ffi", "--edition=2024"];
        lines.extend_from_slice(form);
        lines.push("libs/fauna-ffi-cdylib/src/lib.rs");
        let at = cargo_argfile(&tmp, &lines);
        let args = [FAKE_TAIL, at.as_str()];
        let out = launch_as(&tmp, Some(missing.to_str().unwrap()), &args, None, true);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{form:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seen = seen(&out);
        assert_eq!(
            seen.argv,
            vec![at.clone()],
            "{form:?}: cargo's argfile, unchanged"
        );
        assert_eq!(
            seen.files[0]
                .1
                .clone()
                .expect("cargo's argfile was readable"),
            lines,
            "{form:?}"
        );
        assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
    }
}

#[test]
fn every_other_unit_behind_cargos_own_argfile_still_reaches_the_tail() {
    // a non-primary cdylib, a primary library, and an argfile that cannot be read
    // all go to the tail with cargo's argfile unchanged, as before
    let cases: [(&[&str], bool, bool); 3] = [
        (&["--crate-type", "cdylib"], false, true),
        (&["--crate-type", "lib"], true, true),
        (&["--crate-type", "cdylib"], true, false),
    ];
    for (form, primary, readable) in cases {
        let tmp = scratch();
        let mut lines = vec!["--crate-name", "x"];
        lines.extend_from_slice(form);
        let at = if readable {
            cargo_argfile(&tmp, &lines)
        } else {
            format!("@{}", tmp.join("no-such-cargo-argfile.txt").display())
        };
        let args = [RUSTC, at.as_str()];
        let out = launch_as(&tmp, Some(FAKE_TAIL), &args, None, primary);
        assert_eq!(out.status.code(), Some(0), "{form:?} {primary} {readable}");
        let seen = seen(&out);
        assert_eq!(
            seen.argv,
            vec![RUSTC.to_string(), at.clone()],
            "{form:?} {primary} {readable}"
        );
        assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
    }
}

#[test]
fn rustcs_exit_code_is_the_launchers_for_a_link_product() {
    let tmp = scratch();
    let out = launch_as(
        &tmp,
        Some("no-such-kache"),
        &[FAKE_TAIL, "--crate-type", "cdylib"],
        Some(5),
        true,
    );
    assert_eq!(out.status.code(), Some(5));
}

#[test]
fn every_other_unit_still_reaches_the_tail() {
    // a link product of a package cargo was not asked for (a registry crate that
    // declares a cdylib), and a primary unit that is not a link product (a
    // library, a binary), both stay cached
    let cases: [(&[&str], bool); 4] = [
        (&["--crate-type", "cdylib"], false),
        (&["--crate-type", "lib"], true),
        (&["--crate-type", "bin"], true),
        (&["--crate-name", "staticlib"], true),
    ];
    for (form, primary) in cases {
        let tmp = scratch();
        let mut args = vec![RUSTC];
        args.extend_from_slice(form);
        let out = launch_as(&tmp, Some(FAKE_TAIL), &args, None, primary);
        assert_eq!(out.status.code(), Some(0), "{form:?}");
        let seen = seen(&out);
        assert_eq!(seen.argv.len(), 2, "{form:?}: argv {:?}", seen.argv);
        assert_eq!(seen.argv[0], RUSTC, "{form:?}");
        assert!(left_in(&tmp).is_empty(), "{:?}", left_in(&tmp));
    }
}

#[test]
fn a_link_product_without_kache_named_is_still_a_failed_compile() {
    let tmp = scratch();
    let out = launch_as(
        &tmp,
        None,
        &[FAKE_TAIL, "--crate-type", "cdylib"],
        None,
        true,
    );
    assert_failed_launch(&out, &tmp, "FAUNA_KACHE_EXE");
}

#[test]
fn version_prints_the_workspace_version() {
    let tmp = scratch();
    let out = launch(&tmp, None, &["--version"], None);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("kache-launcher {}", env!("CARGO_PKG_VERSION"))
    );
}

// --- a minimal JSON reader for the fake tail's report (std only) -----------------------

enum Json {
    Null,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(fields) => &fields.iter().find(|(k, _)| k == key).unwrap().1,
            _ => panic!("not an object"),
        }
    }
    fn items(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => panic!("not an array"),
        }
    }
    fn string(&self) -> String {
        match self {
            Json::Str(s) => s.clone(),
            _ => panic!("not a string"),
        }
    }
    fn strings(&self) -> Vec<String> {
        self.items().iter().map(Json::string).collect()
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> u8 {
        self.s[self.i]
    }
    fn eat(&mut self, b: u8) {
        assert_eq!(self.peek(), b, "at byte {}", self.i);
        self.i += 1;
    }
    fn value(&mut self) -> Json {
        match self.peek() {
            b'n' => {
                assert_eq!(&self.s[self.i..self.i + 4], b"null");
                self.i += 4;
                Json::Null
            }
            b'"' => Json::Str(self.string()),
            b'[' => {
                self.eat(b'[');
                let mut items = Vec::new();
                while self.peek() != b']' {
                    if !items.is_empty() {
                        self.eat(b',');
                    }
                    items.push(self.value());
                }
                self.eat(b']');
                Json::Arr(items)
            }
            b'{' => {
                self.eat(b'{');
                let mut fields = Vec::new();
                while self.peek() != b'}' {
                    if !fields.is_empty() {
                        self.eat(b',');
                    }
                    let key = self.string();
                    self.eat(b':');
                    fields.push((key, self.value()));
                }
                self.eat(b'}');
                Json::Obj(fields)
            }
            other => panic!("unexpected {:?} at byte {}", other as char, self.i),
        }
    }
    fn string(&mut self) -> String {
        self.eat(b'"');
        let mut out = Vec::new();
        loop {
            let b = self.peek();
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = self.peek();
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap();
                            let c = char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap();
                            self.i += 4;
                            out.extend_from_slice(c.to_string().as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        String::from_utf8(out).unwrap()
    }
}
