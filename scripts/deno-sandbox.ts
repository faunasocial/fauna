// deno-sandbox.ts — run a Deno build tool under the narrowed permission set
// (build-system.md § The Deno build sandbox).
//
// Usage, from a tree's deno.json task (cwd = the tree root):
//   deno run --allow-read --allow-env --allow-run=deno,git,bwrap --allow-ffi ../../scripts/deno-sandbox.ts \
//     [--net=<hosts>] [--serve] [--read=<paths>] [--sha-env=<NAME>] -- <deno run arguments, e.g. npm:vite build>
//
// It spawns `deno run --no-prompt` — inside bubblewrap (below), under a
// system-call filter (seccompFilterFd) — in an
// environment cleared to ENV_NAMES and ENV_PREFIXES, with:
//   --allow-read=<list>         the tree, the ANCESTOR_MARKERS of every ancestor, the
//                               Linux platform probes, and --read
//   --allow-env, --allow-sys    unscoped — why each cannot be narrowed is recorded in
//                               the goal doc (the environment itself is scrubbed)
//   --allow-write=.             the tree, minus every root entry present at launch
//                               except OUTPUT_DIRS
//   --allow-net=<--net>         omitted when --net is absent: no network
//   --allow-run=<esbuild>       the exact esbuild binaries, found below
//   --allow-ffi=<rollup addons> the exact files, found below
//   --preload=<deno-sandbox-preload.mjs>  directory listings come back sorted, so
//                               the output does not follow the checkout's order
//
// Native code — the esbuild binary, a `.node` addon loaded in-process — is outside
// Deno's reach once granted, so the whole child runs under bubblewrap
// (bwrapArgs): the filesystem it sees is what the read and write rows grant, and
// nothing else. `--serve` (dev, preview) shares the host's network namespace so the
// server is reachable on loopback; every other task gets none.
//
// The run/ffi grants are why this launcher exists: the binaries' paths carry the
// platform and the version, so no fixed string in deno.json can name them, and a
// directory-wide grant would let any package run a native file it shipped itself.
// They are found by PACKAGE NAME in the tree's own node_modules (the graph `deno
// install` resolved under the tree's own deno.json); this file lists directories
// and never reads package files.
//
// This file is first-party and imports nothing, so it is the one process in a
// build that holds the launcher grant (read, env, running `deno`, `git` and
// `bwrap`, and ffi — it loads the system libseccomp and libc to build the
// filter; the child's own `--allow-ffi` is the exact-file list below).

const NATIVE: { dirPrefix: string; kind: "run" | "ffi"; match: (name: string) => boolean }[] = [
  // esbuild's JS API drives its Go binary as a subprocess.
  { dirPrefix: "@esbuild+", kind: "run", match: (n) => n === "esbuild" || n === "esbuild.exe" },
  // rollup 4 parses through a Node-API addon, which Deno loads as FFI.
  { dirPrefix: "@rollup+rollup-", kind: "ffi", match: (n) => n.endsWith(".node") },
  // astro 7's toolchain: rolldown bundles (vite 8), the astro compiler parses
  // `.astro` files, satteri renders markdown, lightningcss transforms CSS — each a
  // Node-API addon in its per-platform package.
  { dirPrefix: "@rolldown+binding-", kind: "ffi", match: (n) => n.endsWith(".node") },
  { dirPrefix: "@astrojs+compiler-binding-", kind: "ffi", match: (n) => n.endsWith(".node") },
  { dirPrefix: "@bruits+satteri-", kind: "ffi", match: (n) => n.endsWith(".node") },
  { dirPrefix: "lightningcss-", kind: "ffi", match: (n) => n.endsWith(".node") },
];

// The names the build tools probe in every ancestor directory of the tree (measured
// with DENO_AUDIT_PERMISSIONS over the site and web builds). Deno answers an
// ungranted probe with NotCapable, not `false`, so this list must be complete: a new
// tool's new marker shows up as a `NotCapable … read` on an ancestor path, and joins.
const ANCESTOR_MARKERS = [
  "package.json",
  "lerna.json",
  "pnpm-workspace.yaml",
  "bun.lock",
  "bun.lockb",
  ".pnp.cjs",
  ".pnp.js",
  "jsconfig.json",
  "tsconfig.json",
  "node_modules",
];

// Platform probes the tools make on Linux (WSL detection).
const LINUX_PROBES = ["/proc/version", "/proc/sys/fs/binfmt_misc/WSLInterop", "/run/WSL"];

// The root entries a build writes. Every other entry present at the tree root when
// the build starts is denied — the tracked sources and configs, which a package could
// otherwise edit into the next commit. New root-level files (vite's
// `vite.config.ts.timestamp-*.mjs`) stay writable because they are absent at launch.
const OUTPUT_DIRS = ["build", "dist", ".svelte-kit", ".cache", ".astro"];

// Denied even when absent at launch: each would let one build widen the NEXT build's
// grant — the task flags (deno.json), the resolved graph (deno.lock, package.json),
// the installed packages and the very native files the run/ffi grants name
// (node_modules). A deny outranks every allow and is matched on the path as written,
// symlinks and `..` included. Tools' caches therefore live under the tree's `.cache/`,
// never node_modules/.vite or .astro. This launcher sits outside every tree.
const NEVER_WRITE = ["deno.json", "deno.lock", "package.json", "node_modules"];

// The environment the child keeps. Everything else — tokens, the SSH agent socket,
// cloud credentials — is cleared.
const ENV_NAMES = [
  "PATH",
  "LANG",
  "LC_ALL",
  "TERM",
  "COLORTERM",
  "NO_COLOR",
  "FORCE_COLOR",
  "CI",
  "TMPDIR",
  "TEMP",
  "TMP",
  "NODE_ENV",
  // The measurement method (the goal doc's) keeps working under the launcher.
  "DENO_AUDIT_PERMISSIONS",
];
const ENV_PREFIXES = ["VITE_", "FAUNA_WEB_", "ASTRO_", "PUBLIC_"];

// What a Deno process needs from the system inside bubblewrap, read-only (measured on
// the web and site builds and the web check, 2026-10-04): /usr holds the dynamic
// loader and libc, and the merged-/usr links are recreated as links; /etc/hosts
// resolves `localhost` (without it the site build fails `getaddrinfo ENOTFOUND
// localhost`). Nothing else under /etc is needed, and never the home directory.
const SYSTEM_RO = ["/usr", "/etc/hosts"];
const USR_LINKS = ["/bin", "/lib", "/lib64", "/sbin"];

// The artifact-set opt-out: only the Dockerfile's web-builder stage sets it, because
// user namespaces are off inside Docker. Never a dev-machine setting.
const NO_BWRAP_OPT_OUT = "FAUNA_DENO_SANDBOX_NO_BWRAP";

function fail(msg: string): never {
  console.error(`deno-sandbox: ${msg}`);
  Deno.exit(2);
}

// Web and the site are built only on Linux — the Linux dev machine, the image's
// web-builder stage and the deploy runners (owner-ruled 2026-10-05): nowhere else
// does an OS sandbox contain the native code a build graph runs.
if (Deno.build.os !== "linux") {
  fail(`web and the site are built only on Linux, never on ${Deno.build.os} (build-system.md § The Deno build sandbox)`);
}

function parseArgs(argv: string[]): { net: string[]; serve: boolean; read: string[]; shaEnv: string | null; rest: string[] } {
  const sep = argv.indexOf("--");
  if (sep < 0) fail("expected `--` before the deno run arguments");
  const opt = (flag: string) => argv.slice(0, sep).find((a) => a.startsWith(`--${flag}=`))?.slice(flag.length + 3);
  const list = (flag: string) => (opt(flag) ?? "").split(",").filter(Boolean);
  const rest = argv.slice(sep + 1);
  if (rest.length === 0) fail("nothing to run after `--`");
  return { net: list("net"), serve: argv.slice(0, sep).includes("--serve"), read: list("read"), shaEnv: opt("sha-env") ?? null, rest };
}

// `a/b/../c` → `a/c`; a relative path is taken from `base`.
function resolvePath(base: string, rel: string): string {
  const abs = rel.startsWith("/") ? rel : `${base}/${rel}`;
  const parts: string[] = [];
  for (const p of abs.split(/\/+/)) {
    if (p === "..") parts.pop();
    else if (p !== "." && p !== "") parts.push(p);
  }
  return `/${parts.join("/")}`;
}

// Every directory above `dir`, nearest first, the filesystem root last.
function ancestors(dir: string): string[] {
  const parts = dir.split("/").filter(Boolean);
  const out: string[] = [];
  for (let i = parts.length - 1; i >= 0; i--) out.push(`/${parts.slice(0, i).join("/")}`);
  return out;
}

function join(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

async function walkFiles(dir: string, depth: number, out: string[]): Promise<void> {
  if (depth < 0) return;
  for await (const e of Deno.readDir(dir)) {
    const p = `${dir}/${e.name}`;
    if (e.isDirectory) await walkFiles(p, depth - 1, out);
    else if (e.isFile) out.push(p);
  }
}

async function nativeGrants(tree: string): Promise<{ run: string[]; ffi: string[] }> {
  const run: string[] = [];
  const ffi: string[] = [];
  const store = `${tree}/node_modules/.deno`;
  let entries: Deno.DirEntry[] = [];
  try {
    entries = [...Deno.readDirSync(store)];
  } catch {
    return { run, ffi }; // no npm dependencies installed: nothing native to grant
  }
  for (const e of entries) {
    const rule = NATIVE.find((r) => e.isDirectory && e.name.startsWith(r.dirPrefix));
    if (!rule) continue;
    // <store>/<pkg@ver>/node_modules/<@scope>/<name>/[bin/]<file>
    const files: string[] = [];
    await walkFiles(`${store}/${e.name}/node_modules`, 3, files);
    for (const f of files) {
      if (rule.match(f.slice(f.lastIndexOf("/") + 1))) (rule.kind === "run" ? run : ffi).push(f);
    }
  }
  return { run, ffi };
}

// Output of a fixed-argument command, or null when it cannot run or fails.
async function commandOutput(cmd: string, args: string[]): Promise<string | null> {
  try {
    const out = await new Deno.Command(cmd, { args, stderr: "null" }).output();
    const text = new TextDecoder().decode(out.stdout).trim();
    return out.success && text ? text : null;
  } catch {
    return null;
  }
}

// Deno's real cache, so the child finds it with HOME pointed elsewhere.
async function denoDir(): Promise<string | null> {
  const info = await commandOutput("deno", ["info", "--json"]);
  if (!info) return null;
  try {
    const dir = JSON.parse(info).denoDir;
    return typeof dir === "string" && dir ? dir : null;
  } catch {
    return null;
  }
}

function isFile(p: string): boolean {
  try {
    return Deno.statSync(p).isFile;
  } catch {
    return false;
  }
}

// bubblewrap's path, from /usr/bin or the PATH entries; null when it is absent.
function findBwrap(path: string): string | null {
  for (const dir of ["/usr/bin", ...path.split(":").filter(Boolean)]) {
    if (isFile(`${dir}/bwrap`)) return `${dir}/bwrap`;
  }
  return null;
}

// The system-call filter bubblewrap applies to the whole child (build-system.md § The
// Deno build sandbox → Where bubblewrap's own containment stops, (6)). The sandbox
// shares the host's kernel, so this deny-list closes the families from which kernel
// escapes have repeatedly come; everything else is allowed, and a system call from
// another architecture takes libseccomp's default bad-architecture action (kill). The
// filter is built with the SYSTEM libseccomp through FFI — no package of our own, no
// second language — and handed to bwrap on an inherited memfd, so the child keeps its
// stdin (`--seccomp 0` would replace it with /dev/null). Measured on the web and site
// builds, the web check, dev and preview, 2026-10-04. A name absent on this
// architecture (`modify_ldt` on arm64) is skipped.
const SECCOMP_DENY = [
  "io_uring_setup", "io_uring_enter", "io_uring_register",
  "perf_event_open",
  "keyctl", "add_key", "request_key",
  "ptrace", "process_vm_readv", "process_vm_writev",
  "bpf", "userfaultfd",
  "kexec_load", "kexec_file_load", "init_module", "finit_module", "delete_module",
  "mount", "umount2", "pivot_root", "mount_setattr", "open_tree", "move_mount", "fsopen", "fsconfig", "fsmount", "fspick",
  "open_by_handle_at", "setns", "unshare", "chroot",
  "acct", "quotactl", "syslog", "uselib", "vhangup", "modify_ldt",
  "mbind", "move_pages", "set_mempolicy", "migrate_pages",
];
// clone3 carries its flags in a struct the filter cannot read: ENOSYS makes libc fall
// back to clone, where the CLONE_NEWUSER rule below reads them.
const SECCOMP_ENOSYS = ["clone3"];
const CLONE_NEWUSER = 0x10000000;
const SCMP_ACT_ALLOW = 0x7fff0000;
const SCMP_ACT_ERRNO = (errno: number) => 0x00050000 | (errno & 0xffff);
const EPERM = 1;
const ENOSYS = 38;
const SCMP_CMP_MASKED_EQ = 7;

// Build the filter and return the memfd holding its BPF program, inheritable by the
// child (no CLOEXEC) for `--seccomp <fd>`, with its closer. Fails closed when
// libseccomp is absent.
function seccompFilterFd(): { fd: number; close: () => void } {
  type Lib = Deno.DynamicLibrary<{
    seccomp_init: { parameters: ["u32"]; result: "pointer" };
    seccomp_syscall_resolve_name: { parameters: ["buffer"]; result: "i32" };
    seccomp_rule_add_array: { parameters: ["pointer", "u32", "i32", "u32", "buffer"]; result: "i32" };
    seccomp_export_bpf: { parameters: ["pointer", "i32"]; result: "i32" };
    seccomp_release: { parameters: ["pointer"]; result: "void" };
  }>;
  let lib: Lib;
  let libc: Deno.DynamicLibrary<{
    memfd_create: { parameters: ["buffer", "u32"]; result: "i32" };
    lseek: { parameters: ["i32", "i64", "i32"]; result: "i64" };
    close: { parameters: ["i32"]; result: "i32" };
  }>;
  try {
    lib = Deno.dlopen("libseccomp.so.2", {
      seccomp_init: { parameters: ["u32"], result: "pointer" },
      seccomp_syscall_resolve_name: { parameters: ["buffer"], result: "i32" },
      seccomp_rule_add_array: { parameters: ["pointer", "u32", "i32", "u32", "buffer"], result: "i32" },
      seccomp_export_bpf: { parameters: ["pointer", "i32"], result: "i32" },
      seccomp_release: { parameters: ["pointer"], result: "void" },
    });
    libc = Deno.dlopen("libc.so.6", {
      memfd_create: { parameters: ["buffer", "u32"], result: "i32" },
      lseek: { parameters: ["i32", "i64", "i32"], result: "i64" },
      close: { parameters: ["i32"], result: "i32" },
    });
  } catch (e) {
    fail(
      "the sandbox's system-call filter is built with the system libseccomp (`libseccomp.so.2`, the " +
        `\`libseccomp2\` package from the OS repository), which did not load: ${e instanceof Error ? e.message : e}`,
    );
  }
  const cstr = (s: string) => new TextEncoder().encode(s + "\0");
  const ctx = lib.symbols.seccomp_init(SCMP_ACT_ALLOW);
  if (!ctx) fail("seccomp_init failed");
  const rule = (name: string, action: number, args: Uint8Array<ArrayBuffer> = new Uint8Array(0)) => {
    const nr = lib.symbols.seccomp_syscall_resolve_name(cstr(name));
    if (nr < 0) return; // not a system call on this architecture
    const rc = lib.symbols.seccomp_rule_add_array(ctx, action, nr, args.length / 24, args);
    if (rc !== 0) fail(`seccomp rule for ${name} failed: ${rc}`);
  };
  for (const name of SECCOMP_DENY) rule(name, SCMP_ACT_ERRNO(EPERM));
  for (const name of SECCOMP_ENOSYS) rule(name, SCMP_ACT_ERRNO(ENOSYS));
  // struct scmp_arg_cmp { u32 arg; int op; u64 datum_a; u64 datum_b } — arg 0 (flags)
  // masked with CLONE_NEWUSER equals CLONE_NEWUSER.
  const cmp = new Uint8Array(24);
  const view = new DataView(cmp.buffer);
  view.setUint32(0, 0, true);
  view.setInt32(4, SCMP_CMP_MASKED_EQ, true);
  view.setBigUint64(8, BigInt(CLONE_NEWUSER), true);
  view.setBigUint64(16, BigInt(CLONE_NEWUSER), true);
  rule("clone", SCMP_ACT_ERRNO(EPERM), cmp);
  const fd = libc.symbols.memfd_create(cstr("deno-sandbox-seccomp"), 0);
  if (fd < 0) fail("memfd_create failed");
  const rc = lib.symbols.seccomp_export_bpf(ctx, fd);
  if (rc !== 0) fail(`seccomp_export_bpf failed: ${rc}`);
  lib.symbols.seccomp_release(ctx);
  libc.symbols.lseek(fd, 0n, 0);
  return { fd, close: () => void libc.symbols.close(fd) };
}

// The scrubbed child environment: the allowlist, over the launcher's own.
function scrubbedEnv(tree: string): Record<string, string> {
  const env: Record<string, string> = {};
  const names = new Set(ENV_NAMES);
  for (const [k, v] of Object.entries(Deno.env.toObject())) {
    if (names.has(k) || ENV_PREFIXES.some((p) => k.startsWith(p))) env[k] = v;
  }
  // Never created: no tool needed it, and the launcher has no write grant.
  const home = join(join(tree, ".cache"), "home");
  env.HOME = home;
  return env;
}

const { net, serve, read, shaEnv, rest } = parseArgs(Deno.args);
const tree = Deno.cwd();
// Outside a tree's own deno.json an `npm:` specifier resolves past the lockfile and
// the minimum release age, to whatever the registry serves today.
if (!isFile(join(tree, "deno.json"))) {
  fail(`no deno.json in ${tree} — run it through the tree's own \`deno task\``);
}

const env = scrubbedEnv(tree);
const cache = await denoDir();
if (cache) env.DENO_DIR = cache;
// git never runs inside the sandbox (`git -c alias.x='!sh'` is a shell): the launcher
// resolves HEAD itself, with fixed arguments, when the variable is unset.
if (shaEnv) {
  const sha = Deno.env.get(shaEnv) || (await commandOutput("git", ["rev-parse", "HEAD"]));
  if (sha) env[shaEnv] = sha;
}

const { run, ffi } = await nativeGrants(tree);
const extraReads = read.map((r) => resolvePath(tree, r));

const deny = new Set(NEVER_WRITE);
for (const e of Deno.readDirSync(tree)) {
  if (!OUTPUT_DIRS.includes(e.name)) deny.add(e.name);
}

// The child runs under bubblewrap; without it native code reaches the whole
// machine as the build user, so its absence fails the build.
let bwrap: string | null = null;
if (Deno.env.get(NO_BWRAP_OPT_OUT) === "1") {
  console.error(`deno-sandbox: no bubblewrap (${NO_BWRAP_OPT_OUT}=1, the image's web-builder stage)`);
} else {
  bwrap = findBwrap(env.PATH ?? "");
  if (!bwrap) {
    fail(
      "the build runs under bubblewrap, and `bwrap` was not found — install the " +
        "`bubblewrap` package from the OS repository (build-system.md § The Deno build sandbox)",
    );
  }
}

// Run before every tool: directory listings come back sorted (the file says why).
const preload = resolvePath(new URL(".", import.meta.url).pathname, "deno-sandbox-preload.mjs");
if (!isFile(preload)) fail(`missing ${preload}`);

const readList = [tree, tree.toUpperCase(), preload];
for (const dir of ancestors(tree)) {
  for (const m of ANCESTOR_MARKERS) readList.push(join(dir, m));
}
readList.push(...LINUX_PROBES);
readList.push(...extraReads);

const flags = [
  "run",
  "--no-prompt",
  `--allow-read=${[...new Set(readList)].join(",")}`,
  "--allow-env",
  "--allow-sys",
  "--allow-write=.",
  `--deny-write=${[...deny].join(",")}`,
  `--preload=${preload}`,
];
// A scoped flag is emitted only with a value: a bare `--allow-run` grants everything.
if (net.length) flags.push(`--allow-net=${net.join(",")}`);
if (run.length) flags.push(`--allow-run=${run.join(",")}`);
if (ffi.length) flags.push(`--allow-ffi=${ffi.join(",")}`);

// The bubblewrap sandbox: an empty root; the system paths Deno needs, Deno itself
// and its cache read-only; the tree read-write with every entry the write row denies
// bound read-only over it (bubblewrap applies binds in order); the read row's other
// paths read-only; a private /tmp; its own pid, ipc, uts, user and cgroup namespaces,
// and its own network namespace unless the task serves to the host; the system-call
// filter from the inherited memfd.
function bwrapArgs(deno: string, seccompFd: number): string[] {
  const a = ["--unshare-all", "--die-with-parent", "--tmpfs", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"];
  a.push("--seccomp", String(seccompFd));
  if (serve) a.push("--share-net");
  for (const p of SYSTEM_RO) a.push("--ro-bind-try", p, p);
  for (const l of USR_LINKS) {
    let target: string | null = null;
    try {
      target = Deno.readLinkSync(l);
    } catch { /* not a link */ }
    if (target) a.push("--symlink", target, l);
    else a.push("--ro-bind-try", l, l);
  }
  a.push("--ro-bind", deno, deno);
  if (env.DENO_DIR) a.push("--ro-bind-try", env.DENO_DIR, env.DENO_DIR);
  a.push("--bind", tree, tree);
  for (const n of deny) a.push("--ro-bind-try", join(tree, n), join(tree, n));
  for (const p of readList) {
    if (p !== tree && !p.startsWith(`${tree}/`) && !p.startsWith("/proc/")) a.push("--ro-bind-try", p, p);
  }
  a.push("--chdir", tree, "--", deno);
  return a;
}

if (bwrap) {
  // The host's temp directory is outside the sandbox; the private /tmp stands in.
  for (const k of ["TMPDIR", "TEMP", "TMP"]) if (env[k]) env[k] = "/tmp";
}
// Inside the sandbox PATH names nothing, so Deno is named by its own path there.
const seccomp = bwrap ? seccompFilterFd() : null;
const child = new Deno.Command(bwrap ?? "deno", {
  args: [...(bwrap && seccomp ? bwrapArgs(Deno.execPath(), seccomp.fd) : []), ...flags, ...rest],
  clearEnv: true,
  env,
  stdin: "inherit",
  stdout: "inherit",
  stderr: "inherit",
}).spawn();
// bwrap holds its own copy of the memfd; the launcher's is no longer needed.
seccomp?.close();
// Ctrl-C reaches the whole foreground process group, the tool included; outlive it
// so a dev/preview server's own shutdown finishes and its exit code is reported.
Deno.addSignalListener("SIGINT", () => {});
const status = await child.status;
Deno.exit(status.code);
