//! Where the UI thread actually SITS when its heartbeat has gone stale.
//!
//! [`crate::UiThreadHeartbeat`] answers *whether* the host's main loop is
//! running, and the agent's own process watchdog answers *whose fault* a stall
//! is (our code, or a box that starved the process). Neither answers **what
//! holds the thread**, which is the only one of the three a fix can act on.
//!
//! `e2e-conventions.md` convention 11 names this measurement as the next one
//! owed: the 2026-09-10 whole-suite `--app linux`
//! sweep failed 205 tests, **110 of them on a stall reading "the process itself
//! was RUNNING"**, spread over 53 modules. A verdict that says only "synchronous
//! work, somewhere" cannot be clustered into causes, so every one of those 110
//! is a separate investigation. A stack turns them into a handful.
//!
//! Two captures exist, one per OS family that hosts a heartbeat on the dev
//! fleet: **Linux** (Linux) and **Windows** (Windows, aarch64 and x86_64). Every
//! other host reads `UNMEASURED`, never "no stall found" — the same rule the
//! heartbeat itself follows.
//!
//! # How it captures on Linux
//!
//! The stalled thread cannot capture its own stack — that it is not running is
//! the whole finding — so the agent's server thread asks it to, from outside:
//!
//! 1. **`SIGURG`, directed at that one thread** (`tgkill`). The signal is chosen
//!    for its *default* disposition: SIGURG is ignored unless someone handles it,
//!    so a signal that arrives with no handler installed can never kill the app.
//!    Every other plausible choice (`SIGRTMIN+n`, `SIGPROF`) terminates the
//!    process by default, which would turn a diagnostic into an outage.
//! 2. **The handler walks its own stack with `_Unwind_Backtrace`** — the C
//!    unwinder libgcc already links into every Rust binary, called through a
//!    hand-written `extern` block, so this instrument adds no dependency. It
//!    stores raw instruction pointers, each with the unwinder's signal-frame
//!    flag, into preallocated static arrays and nothing else: **no allocation,
//!    no locking, no formatting inside the handler.** That matters more than
//!    it looks. A handler that allocates can
//!    deadlock the very thread it is diagnosing — if the thread was interrupted
//!    inside `malloc` holding its arena lock, a `malloc` in the handler waits on
//!    it forever — and a wedged app fails every *subsequent* test too. So the
//!    convenient shape (capture a `std::backtrace::Backtrace`, or a
//!    `backtrace`-crate call, both of which allocate) is exactly the shape this
//!    must not use.
//! 3. **Symbolization happens on the agent's thread afterwards**, where
//!    allocating is safe: `dladdr` names the module and any exported symbol
//!    (which is most GTK/GLib frames), and `addr2line` — binutils, already on
//!    every dev box — turns the app's own frames into `fn (file:line)`. If it
//!    is missing or slow, the frames degrade to `module+0x…`, never to nothing.
//!
//! Two samples are taken, [`SAMPLE_GAP`] apart, and the thread's CPU time is
//! read from `/proc` across the same window. That is what separates the two
//! shapes of "synchronous work": a thread **parked** on a lock or a syscall
//! burns no CPU and shows the same frame twice, while one **computing** burns a
//! core and usually moves. They call for different fixes, and the stack alone
//! does not tell them apart.
//!
//! # How it captures on Windows
//!
//! Windows has no signal to hand the thread, and needs none: one thread may
//! **suspend** another in its own process and read its registers. That moves
//! the Linux hazard rather than removing it. A suspended thread keeps every
//! lock it held, so if the capturing thread then waits on one of them, both
//! threads wait for good: the heap lock (any allocation), the loader lock
//! (`dbghelp`'s module scan) or `dbghelp`'s own state. So the capture runs in
//! three steps, and the suspension covers only the first:
//!
//! 1. **Suspended: copy, and nothing else.** `SuspendThread`, then
//!    `GetThreadContext` (which also waits until the suspension has really
//!    happened), then `VirtualQuery` for where the stack's committed pages end,
//!    then `ReadProcessMemory` of the stack from the stack pointer upward into
//!    a buffer allocated **before** the suspension, then `ResumeThread`. Each is
//!    a plain kernel call. None of them allocates on the Rust heap, takes a
//!    user-mode lock or can panic, and a test with a counting allocator holds
//!    this window to zero allocations.
//! 2. **Resumed: walk the copy.** `dbghelp`'s `StackWalk64` unwinds from the
//!    copied registers, through the image's own unwind tables, and every stack
//!    read it asks for is served from the copy. A read of the live stack
//!    outside the copy is refused, since what is there now is not what the
//!    thread held. Image memory (code, unwind data) is read live, because it
//!    does not change.
//! 3. **Symbolize.** `SymFromAddr` and `SymGetLineFromAddr64` read the PDB the
//!    build leaves beside each binary, so our own frames come out as
//!    `fn (file:line)`. A module with no PDB, such as `ntdll`, still gets its
//!    exported names. `dbghelp` ships with the OS (`System32\dbghelp.dll`) and
//!    is declared in a hand-written `extern` block, so no crate is added. It
//!    is single-threaded, so every call is made under this module's capture
//!    lock, in a session keyed by a handle of its own that never touches the
//!    one Rust's own backtraces use.
//!
//! The same two samples, [`SAMPLE_GAP`] apart, and the thread's CPU time
//! (`GetThreadTimes`) separate parked from computing. The thread's scheduler
//! state (`NtQuerySystemInformation`) names its wait as well: `Waiting`
//! with the kernel's wait reason, or `Ready`, meaning it was runnable and the
//! box gave it no core.
//!
//! What it does NOT name: code compiled at run time. A frame in a JIT's code,
//! such as the .NET runtime's, has no module and no PDB `dbghelp` can read, so
//! it shows as `? +0x…`, and the walk stops there: such code has no unwind
//! table `dbghelp` can find either, so every frame past it would be a guess.
//! The report says the frames below are UNMEASURED. Only native code (Rust, C,
//! the OS) is named.
//!
//! # Another process: the windows app
//!
//! The windows app hosts no agent of ours: it is a C# process, so its stall is
//! judged from outside, by the FlaUI bridge, and its UI thread's stack is read
//! from outside too (`capture_foreign`, run by the `fauna-e2e-stall-capture`
//! helper binary on the bridge's app-side verdict). It is the same three steps
//! through a handle to that process: `VirtualQueryEx` and `ReadProcessMemory`
//! on it, the frames named by a `dbghelp` session opened on it (and closed
//! after), the thread's state read from the system's table by that pid.
//! Nothing else differs, the suspended window's rule included — and it is
//! safer there, since a capturing process shares no heap or loader lock with
//! the thread it suspends.
//!
//! # What it costs the app
//!
//! Nothing until a stall: the machinery is inert (no handler installed, no
//! thread, no timer) until an op has already timed out and the verdict has
//! already indicted our own code. On Linux the signal itself can make an
//! interruptible syscall on the target thread return `EINTR`; `SA_RESTART`
//! restarts every one that can be restarted, and the callers that cannot be
//! (`poll`/`epoll_wait`) are exactly the ones GLib and tokio already loop on. On
//! Windows the thread is suspended for two short copies and then runs on
//! untouched. It fires only on a thread that has been unresponsive for 25 s, in
//! a test build, on a test whose verdict is already failure.

use std::time::Duration;

/// How far apart the two samples are taken.
///
/// Long enough that a thread burning a core shows a measurable CPU delta at the
/// 10 ms granularity `/proc` reports, short enough to add no meaningful delay to
/// a 504 that has already waited 25 s. It is a *sampling interval*, not a wait
/// for something to happen — nothing is asserted about what occurs inside it
/// (convention 14 governs assertions, and this makes none).
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) const SAMPLE_GAP: Duration = Duration::from_millis(300);

/// The sentence comparing a capture's two samples, shared by both captures so
/// a reader (and a test) sees one wording whatever OS produced the stack.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn second_sample_clause(moved: bool) -> String {
    if moved {
        format!(
            "\n  (a second sample {:.2}s later sits in a DIFFERENT frame — the thread is \
working through code, not parked)",
            SAMPLE_GAP.as_secs_f64()
        )
    } else {
        format!(
            "\n  (a second sample {:.2}s later sits in the SAME frame — the thread is parked \
there)",
            SAMPLE_GAP.as_secs_f64()
        )
    }
}

/// Classify a thread's CPU usage across a sampling window.
///
/// Pure, and separate from the capture, because it is the half that decides
/// which *kind* of fix a stall needs — and a claim that load-bearing must be
/// assertable without a stalled thread to point at.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn cpu_verdict(cpu_delta: Duration, gap: Duration) -> &'static str {
    if gap.is_zero() {
        return "UNMEASURED (no sampling window)";
    }
    let share = cpu_delta.as_secs_f64() / gap.as_secs_f64();
    if share >= 0.5 {
        "COMPUTING — it is burning a core, so the work is the code in the stack, not a wait"
    } else if share > 0.02 {
        "PARTLY RUNNING — it alternates between running and waiting"
    } else {
        "WAITING — it burned no CPU, so the top frame is a block (lock, channel, syscall), not a computation"
    }
}

/// What finding the end of the capture's own frames needs to know about one
/// captured frame.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct FrameFacts<'a> {
    /// The object file `dladdr` placed the address in.
    pub(crate) module: &'a str,
    /// The nearest preceding EXPORTED symbol `dladdr` found, if any — which is
    /// not necessarily the function the address is in.
    pub(crate) symbol: Option<&'a str>,
    /// The unwinder reached this frame through a signal frame, so its address
    /// is the interrupted instruction itself rather than a return address
    /// (`_Unwind_GetIPInfo`'s `ip_before_insn`).
    pub(crate) interrupted: bool,
}

/// Index of the first frame of the INTERRUPTED code in a stack walked from
/// inside the capture's signal handler, or `None` when no boundary is found.
///
/// Two anchors, the first that finds anything winning:
///
/// 1. **The kernel's signal trampoline, by name** ([`trampoline_by_name`]) —
///    the aarch64 shape, `[vdso] __kernel_rt_sigreturn`. The boundary is the
///    frame after it.
/// 2. **The first frame the unwinder reached through a signal frame**
///    ([`first_interrupted_frame`]). The trampoline is where a walk crosses
///    from the handler back into the interrupted code, and the frame it lands
///    on is flagged, because its address comes from the saved signal context
///    rather than from a call. This is what x86-64 glibc needs: its trampoline
///    `__restore_rt` is a local symbol `dladdr` cannot name, so it reports the
///    nearest exported symbol before it instead (`__sigaction`, on the CI
///    runner that measured it) — an accident of that libc's link order, which
///    no name list should chase.
///
/// Neither anchor looks for this file's OWN frames by name: `dladdr` sees only
/// exported symbols, which they are not unless a binary is linked `-rdynamic`
/// (`addr2line` names them, later). ⚠ The frame at the returned index can be
/// libc — a parked thread's own `syscall` — and it is kept: it is where the
/// thread sits.
///
/// Pure, and separate from the capture, for the same reason as
/// [`cpu_verdict`]: which shape a stack takes depends on the architecture and
/// the libc, a developer's machine produces only its own, and so each shape
/// must be assertable from frames written down.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn interrupted_frame_index(frames: &[FrameFacts<'_>]) -> Option<usize> {
    trampoline_by_name(frames)
        .map(|i| i + 1)
        .or_else(|| first_interrupted_frame(frames))
}

/// The first frame the unwinder reached through a signal frame.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn first_interrupted_frame(frames: &[FrameFacts<'_>]) -> Option<usize> {
    frames.iter().position(|f| f.interrupted)
}

/// The kernel's signal trampoline, recognised by name: the vdso
/// (`__kernel_rt_sigreturn` on aarch64), or a symbol naming it outright.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn trampoline_by_name(frames: &[FrameFacts<'_>]) -> Option<usize> {
    frames.iter().position(|f| {
        f.module.contains("vdso")
            || f.symbol
                .is_some_and(|s| s.contains("sigreturn") || s.contains("restore_rt"))
    })
}

#[cfg(target_os = "linux")]
mod imp {
    //! The Linux capture. Every unsafe block here is annotated with the
    //! invariant that makes it sound; the handler's are the ones that matter.

    use super::{
        FrameFacts, SAMPLE_GAP, cpu_verdict, interrupted_frame_index, second_sample_clause,
    };
    use std::ffi::{CStr, c_void};
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    /// Deepest stack we record. Deep enough for a GTK dispatch chain plus the
    /// app's own frames; a fixed size is what keeps the handler allocation-free.
    const MAX_FRAMES: usize = 64;

    static FRAME_IPS: [AtomicUsize; MAX_FRAMES] = [const { AtomicUsize::new(0) }; MAX_FRAMES];
    /// Per frame, whether the unwinder reached it through a signal frame — the
    /// anchor stripping falls back on where the trampoline has no usable name
    /// ([`interrupted_frame_index`]).
    static FRAME_INTERRUPTED: [AtomicBool; MAX_FRAMES] =
        [const { AtomicBool::new(false) }; MAX_FRAMES];
    static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// The thread the current capture is aimed at. The handler checks it, so a
    /// SIGURG that arrives for any other reason (or on any other thread) is a
    /// no-op rather than a corrupted sample.
    static TARGET_TID: AtomicI32 = AtomicI32::new(0);
    static CAPTURE_DONE: AtomicBool = AtomicBool::new(false);
    /// Serializes captures: the statics above are one buffer, and the agent's
    /// server thread is not the only possible caller (the crate's own tests are
    /// another).
    static CAPTURING: Mutex<()> = Mutex::new(());

    /// Take the capture serialization. [`sample_stack`] demands the guard, so
    /// no caller can sample without it: two unserialized samples write into the
    /// same buffer, and the report then shows ANOTHER thread's stack under the
    /// right thread's CPU figures — found 2026-09-15, when a test that sampled
    /// directly handed the spinning-thread test its parked fixture's stack.
    pub(super) fn serialize_captures() -> MutexGuard<'static, ()> {
        CAPTURING.lock().unwrap_or_else(|e| e.into_inner())
    }

    // The C unwinder libgcc already links into every Rust binary. Declared here
    // rather than pulled in as a crate: it is four symbols, and a diagnostic
    // must not widen the dependency surface of a shipped workspace.
    #[allow(non_camel_case_types)]
    type _Unwind_Context = c_void;
    type UnwindTraceFn = extern "C" fn(*mut _Unwind_Context, *mut c_void) -> i32;
    unsafe extern "C" {
        fn _Unwind_Backtrace(trace: UnwindTraceFn, arg: *mut c_void) -> i32;
        fn _Unwind_GetIPInfo(ctx: *mut _Unwind_Context, ip_before_insn: *mut i32) -> usize;
    }
    /// `_URC_NO_REASON` — keep walking.
    const URC_NO_REASON: i32 = 0;
    /// `_URC_NORMAL_STOP` — stop walking, without an error.
    const URC_NORMAL_STOP: i32 = 4;

    /// This thread's kernel task id, cached per thread.
    ///
    /// `gettid` is the id `tgkill` and `/proc/self/task/<tid>` both take;
    /// `ThreadId` is a Rust-side number the kernel has never heard of.
    pub(crate) fn current_tid() -> i32 {
        thread_local! {
            static TID: i32 = unsafe { libc::gettid() };
        }
        TID.with(|t| *t)
    }

    /// The signal we ask the stalled thread to answer on.
    ///
    /// SIGURG's default disposition is **ignore**. That is the entire reason it
    /// is used: if the handler were ever missing when we send, the app carries
    /// on unharmed, where a real-time signal or SIGPROF would kill it. Its only
    /// real producer is TCP out-of-band data, which nothing in this workspace
    /// sends, and the handler ignores anything not aimed at the target thread.
    const CAPTURE_SIGNAL: i32 = libc::SIGURG;

    extern "C" fn trace_frame(ctx: *mut _Unwind_Context, arg: *mut c_void) -> i32 {
        // SAFETY: `arg` is the `&mut usize` handed to `_Unwind_Backtrace` below,
        // which outlives the walk; the unwinder calls us on the same thread,
        // serially.
        let n = unsafe { &mut *(arg as *mut usize) };
        if *n >= MAX_FRAMES {
            return URC_NORMAL_STOP;
        }
        let mut before_insn: i32 = 0;
        // SAFETY: `ctx` is the unwinder's own context, valid for this callback.
        let ip = unsafe { _Unwind_GetIPInfo(ctx, &mut before_insn) };
        if ip == 0 {
            return URC_NORMAL_STOP;
        }
        // A return address points at the instruction *after* the call, which can
        // belong to the next line — or the next function, when the call was the
        // last instruction. Step back one byte so symbolization lands inside the
        // call. A signal frame's IP is the interrupted instruction itself
        // (`before_insn`), and must not be adjusted.
        FRAME_IPS[*n].store(
            if before_insn != 0 { ip } else { ip - 1 },
            Ordering::Relaxed,
        );
        FRAME_INTERRUPTED[*n].store(before_insn != 0, Ordering::Relaxed);
        *n += 1;
        URC_NO_REASON
    }

    /// Runs **on the stalled thread**, inside a signal. Async-signal-safe by
    /// construction: no allocation, no locks, no formatting — only a walk into
    /// the preallocated array above.
    extern "C" fn on_capture_signal(_sig: i32, _info: *mut libc::siginfo_t, _uctx: *mut c_void) {
        // SAFETY: `__errno_location` is per-thread and always valid. A handler
        // that clobbers errno corrupts the interrupted call's own error
        // handling, which would make this instrument a source of bugs.
        let saved_errno = unsafe { *libc::__errno_location() };
        if current_tid() == TARGET_TID.load(Ordering::Relaxed)
            && !CAPTURE_DONE.load(Ordering::Relaxed)
        {
            let mut n: usize = 0;
            // SAFETY: `trace_frame` only touches the statics above and `n`,
            // which lives on this stack for the duration of the walk.
            unsafe { _Unwind_Backtrace(trace_frame, &mut n as *mut usize as *mut c_void) };
            FRAME_COUNT.store(n, Ordering::Relaxed);
            CAPTURE_DONE.store(true, Ordering::Release);
        }
        // SAFETY: as above.
        unsafe { *libc::__errno_location() = saved_errno };
    }

    /// Install the handler once, and only over a disposition nobody else owns.
    fn install_handler() -> Result<(), String> {
        static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
        INSTALLED
            .get_or_init(|| {
                // SAFETY: plain libc signal setup; the structs are zeroed before
                // use and every pointer is to a local that outlives the call.
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(CAPTURE_SIGNAL, std::ptr::null(), &mut old) != 0 {
                        return Err("could not read SIGURG's disposition".to_string());
                    }
                    // Refuse to displace a real handler: another component that
                    // handles SIGURG is one whose behaviour we would silently
                    // break, and no diagnostic is worth that.
                    if old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN {
                        return Err(
                            "SIGURG already has a handler in this process — capture skipped"
                                .to_string(),
                        );
                    }
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    sa.sa_sigaction = on_capture_signal as *const () as usize;
                    // SA_RESTART: restart every syscall the kernel can restart,
                    // so sampling a thread perturbs it as little as possible.
                    sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
                    libc::sigemptyset(&mut sa.sa_mask);
                    if libc::sigaction(CAPTURE_SIGNAL, &sa, std::ptr::null_mut()) != 0 {
                        return Err("could not install the SIGURG capture handler".to_string());
                    }
                }
                Ok(())
            })
            .clone()
    }

    /// One frame of a sample, as the handler recorded it.
    #[derive(Clone, Copy)]
    pub(super) struct Captured {
        pub(super) ip: usize,
        /// See [`FrameFacts::interrupted`].
        pub(super) interrupted: bool,
    }

    /// Ask `tid` for its stack and wait briefly for the handler to deliver it.
    /// The guard is [`serialize_captures`]'s: holding it is the only way in.
    pub(super) fn sample_stack(
        _serial: &MutexGuard<'static, ()>,
        tid: i32,
    ) -> Result<Vec<Captured>, String> {
        install_handler()?;
        CAPTURE_DONE.store(false, Ordering::Relaxed);
        FRAME_COUNT.store(0, Ordering::Relaxed);
        TARGET_TID.store(tid, Ordering::Relaxed);
        // SAFETY: a directed signal to a tid in our own process.
        let sent = unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, CAPTURE_SIGNAL) };
        if sent != 0 {
            return Err(format!("could not signal tid {tid}"));
        }
        // A thread in uninterruptible sleep (D state) will not run the handler
        // at all; bound the wait rather than adopting its stall.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !CAPTURE_DONE.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(
                    "the thread never ran the capture handler (uninterruptible sleep?)".to_string(),
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let n = FRAME_COUNT.load(Ordering::Relaxed);
        Ok((0..n)
            .map(|i| Captured {
                ip: FRAME_IPS[i].load(Ordering::Relaxed),
                interrupted: FRAME_INTERRUPTED[i].load(Ordering::Relaxed),
            })
            .collect())
    }

    /// One frame, as `dladdr` sees it before any symbolization.
    struct RawFrame {
        module: String,
        offset: usize,
        symbol: Option<String>,
    }

    fn dl_frames(ips: &[usize]) -> Vec<RawFrame> {
        ips.iter()
            .map(|&ip| {
                // SAFETY: `info` is zeroed and passed by pointer; `dladdr` only
                // writes borrowed pointers into it, read below while the
                // module stays loaded (it does — we are inside it).
                let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
                let found = unsafe { libc::dladdr(ip as *const c_void, &mut info) };
                if found == 0 || info.dli_fname.is_null() {
                    return RawFrame {
                        module: "?".to_string(),
                        offset: ip,
                        symbol: None,
                    };
                }
                let module = unsafe { CStr::from_ptr(info.dli_fname) }
                    .to_string_lossy()
                    .into_owned();
                let symbol = if info.dli_sname.is_null() {
                    None
                } else {
                    Some(
                        unsafe { CStr::from_ptr(info.dli_sname) }
                            .to_string_lossy()
                            .into_owned(),
                    )
                };
                RawFrame {
                    module,
                    offset: ip.saturating_sub(info.dli_fbase as usize),
                    symbol,
                }
            })
            .collect()
    }

    /// Symbolize one module's offsets with binutils `addr2line`.
    ///
    /// Returns `offset -> "fn (file:line)"` for whatever it could resolve. A
    /// missing, failing, or slow `addr2line` yields an empty map and the caller
    /// falls back to `module+0x…`: a degraded frame is still a frame, and this
    /// path must never be able to hang the agent.
    fn addr2line(module: &str, offsets: &[usize]) -> std::collections::HashMap<usize, String> {
        use std::collections::HashMap;
        use std::process::{Command, Stdio};

        let mut out = HashMap::new();
        if offsets.is_empty() {
            return out;
        }
        let mut cmd = Command::new("addr2line");
        cmd.arg("-a") // group the output by address, so it can be parsed
            .arg("-f") // function names
            .arg("-C") // demangle (binutils knows both Rust manglings)
            .arg("-i") // inlined frames
            .arg("-e")
            .arg(module);
        for off in offsets {
            cmd.arg(format!("0x{off:x}"));
        }
        let Ok(child) = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            return out;
        };
        // A debug binary's DWARF is large and this box is busy; kill rather than
        // wait forever. The agent's server thread is single-threaded, so a hung
        // symbolizer would make the app unreachable to the driver.
        let pid = child.id() as i32;
        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let watch = finished.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if watch.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // SAFETY: our own child, killed only if it outstayed the deadline.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        });
        let output = child.wait_with_output();
        finished.store(true, Ordering::Relaxed);
        let Ok(output) = output else { return out };
        let text = String::from_utf8_lossy(&output.stdout);

        let mut current: Option<usize> = None;
        let mut pending_fn: Option<String> = None;
        let mut inlined = 0usize;
        for line in text.lines() {
            if let Some(hex) = line.strip_prefix("0x") {
                current = usize::from_str_radix(hex.trim(), 16).ok();
                pending_fn = None;
                inlined = 0;
                continue;
            }
            let Some(off) = current else { continue };
            match pending_fn.take() {
                None => pending_fn = Some(line.to_string()),
                Some(name) => {
                    // `name` + this file:line line complete one frame.
                    let entry = out.entry(off).or_insert_with(|| {
                        let where_ = line.trim();
                        if name == "??" {
                            String::new()
                        } else if where_ == "??:0" || where_ == "??:?" {
                            name.clone()
                        } else {
                            format!("{name} ({where_})")
                        }
                    });
                    if entry.is_empty() {
                        out.remove(&off);
                    } else {
                        inlined += 1;
                        if inlined > 1 {
                            // Keep the innermost frame; say the rest exist.
                            let extra = inlined - 1;
                            let base = entry.split(" [+").next().unwrap_or(entry).to_string();
                            *entry = format!("{base} [+{extra} inlined]");
                        }
                    }
                }
            }
        }
        out
    }

    /// Turn raw instruction pointers into readable frames.
    fn symbolize(ips: &[usize]) -> Vec<String> {
        use std::collections::HashMap;

        let raw = dl_frames(ips);
        let mut by_module: HashMap<&str, Vec<usize>> = HashMap::new();
        for f in &raw {
            if f.module != "?" {
                by_module.entry(&f.module).or_default().push(f.offset);
            }
        }
        let resolved: HashMap<&str, HashMap<usize, String>> = by_module
            .iter()
            .map(|(module, offsets)| (*module, addr2line(module, offsets)))
            .collect();

        raw.iter()
            .map(|f| {
                let short = f.module.rsplit('/').next().unwrap_or(&f.module);
                let named = resolved
                    .get(f.module.as_str())
                    .and_then(|m| m.get(&f.offset))
                    .cloned()
                    .or_else(|| f.symbol.clone())
                    .unwrap_or_else(|| format!("+0x{:x}", f.offset));
                format!("{short}  {named}")
            })
            .collect()
    }

    /// Run `f` over a sample's frames as the boundary search sees them.
    pub(super) fn with_frame_facts<R>(
        frames: &[Captured],
        f: impl FnOnce(&[FrameFacts<'_>]) -> R,
    ) -> R {
        let ips: Vec<usize> = frames.iter().map(|c| c.ip).collect();
        let raw = dl_frames(&ips);
        let facts: Vec<FrameFacts<'_>> = raw
            .iter()
            .zip(frames)
            .map(|(r, c)| FrameFacts {
                module: &r.module,
                symbol: r.symbol.as_deref(),
                interrupted: c.interrupted,
            })
            .collect();
        f(&facts)
    }

    /// Drop the frames belonging to the capture itself, so the reported stack
    /// starts at the code that was actually interrupted.
    ///
    /// Where they end is [`interrupted_frame_index`]'s decision: everything
    /// before that index is this file's handler and the kernel's signal
    /// trampoline, and reporting those as the stall would point every reader
    /// here. No boundary found means nothing is stripped.
    ///
    /// It strips **addresses**, not rendered text, so the stripped list is also
    /// what the two samples are compared by — the handler frames are identical
    /// in both samples by construction, so comparing before stripping would
    /// report every thread as parked.
    fn strip_handler_ips(frames: &[Captured]) -> Vec<usize> {
        let from = with_frame_facts(frames, interrupted_frame_index).unwrap_or(0);
        frames[from..].iter().map(|c| c.ip).collect()
    }

    fn read_proc(tid: i32, what: &str) -> Option<String> {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/{what}")).ok()
    }

    /// `(state, cpu-time)` for a thread, from `/proc/self/task/<tid>/stat`.
    pub(super) fn thread_state(tid: i32) -> Option<(char, Duration)> {
        let stat = read_proc(tid, "stat")?;
        // The comm field is parenthesized and may contain spaces, so every
        // parse starts after the LAST ')'.
        let rest = &stat[stat.rfind(')')? + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let state = fields.first()?.chars().next()?;
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        // SAFETY: a plain sysconf read.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        Some((
            state,
            Duration::from_secs_f64((utime + stime) as f64 / hz as f64),
        ))
    }

    fn state_word(state: char) -> &'static str {
        match state {
            'R' => "R (running or runnable)",
            'S' => "S (interruptible sleep)",
            'D' => "D (uninterruptible sleep — kernel I/O)",
            'T' | 't' => "T (stopped)",
            'Z' => "Z (zombie)",
            other => Box::leak(format!("{other} (see proc(5))").into_boxed_str()),
        }
    }

    /// Another process's thread is not capturable here: the Linux capture
    /// asks the stalled thread itself, through a signal handler only its own
    /// process can install.
    pub(crate) fn capture_foreign(_pid: u32, _tid: u32) -> String {
        " — capturing another process's thread is built on Windows only (on Linux the \
stalled thread answers through a signal handler only its own process installs), so what holds it \
is UNMEASURED"
            .to_string()
    }

    pub(crate) fn capture(tid: i32) -> String {
        if tid <= 0 {
            return " — the host's heartbeat recorded no thread id, so its stack could not be \
captured (a host beating from a thread this agent can name gets it free)"
                .to_string();
        }
        let serial = serialize_captures();

        // The window is MEASURED, never assumed to be `SAMPLE_GAP`: a capture
        // that has to wait for the handler stretches it (up to two seconds when
        // the thread never answers), and reporting that longer window as the
        // short one turns an ordinary busy thread into a nonsense "+2.31s of
        // CPU over a 0.30s sample" — 770% of a core, which is the shape of a
        // broken instrument, not a finding. Found while mutation-proving this
        // file's own tests, 2026-09-10.
        let started = Instant::now();
        let before = thread_state(tid);
        let first = sample_stack(&serial, tid);
        std::thread::sleep(SAMPLE_GAP);
        let after = thread_state(tid);
        let window = started.elapsed();
        let second = sample_stack(&serial, tid);

        let mut out = String::new();
        out.push_str(&format!("\nUI-THREAD STALL STACK (tid {tid})"));
        if let (Some((_, cpu0)), Some((state, cpu1))) = (before, after) {
            let delta = cpu1.saturating_sub(cpu0);
            out.push_str(&format!(
                "\n  state {}, CPU +{:.2}s over the {:.2}s sample: {}",
                state_word(state),
                delta.as_secs_f64(),
                window.as_secs_f64(),
                cpu_verdict(delta, window),
            ));
        }
        if let Some(wchan) = read_proc(tid, "wchan").filter(|w| !w.trim().is_empty() && w != "0") {
            out.push_str(&format!("\n  kernel wait channel: {}", wchan.trim()));
        }
        if let Some(sys) = read_proc(tid, "syscall") {
            let sys = sys.trim();
            let word = sys.split_whitespace().next().unwrap_or(sys);
            out.push_str(&format!("\n  syscall: {word}"));
        }

        match first {
            Err(why) => out.push_str(&format!("\n  stack NOT captured: {why}")),
            Ok(ips) => {
                let ips = strip_handler_ips(&ips);
                let frames = symbolize(&ips);
                if frames.is_empty() {
                    out.push_str("\n  stack NOT captured: the walk produced no frames");
                } else {
                    for (i, f) in frames.iter().enumerate() {
                        out.push_str(&format!("\n  #{i:02} {f}"));
                    }
                    // Compared as RAW addresses, not as symbolized text:
                    // symbolizing costs an `addr2line` run (~3s on a 550MB debug
                    // binary), and two different call sites inside one function
                    // share a name while differing here — so the cheap
                    // comparison is also the precise one.
                    let later = second.as_ref().ok().map(|s| strip_handler_ips(s));
                    let moved = match (ips.first(), later.as_ref().and_then(|s| s.first())) {
                        (Some(a), Some(b)) => Some(a != b),
                        _ => None,
                    };
                    if let Some(moved) = moved {
                        out.push_str(&second_sample_clause(moved));
                    }
                }
            }
        }
        out
    }
}

#[cfg(all(windows, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod imp {
    //! The Windows capture: suspend, copy, resume, and only then walk the copy
    //! and symbolize it (the module doc says why in that order). Every unsafe
    //! block is annotated with the invariant that makes it sound; the ones in
    //! [`take_snapshot`] matter most, because that is the window in which a
    //! wrong call deadlocks the app.

    use super::{SAMPLE_GAP, cpu_verdict, second_sample_clause};
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    type Handle = *mut c_void;

    /// Deepest stack we report, as on Linux.
    const MAX_FRAMES: usize = 64;
    /// Most of the stack we copy, from the stack pointer upward. The frames that
    /// hold a thread sit nearest the pointer, so a deep stack is cut at its
    /// outermost (oldest) end and the walk simply stops there.
    const STACK_COPY_MAX: usize = 512 * 1024;

    /// The facts about `CONTEXT` the capture needs, per architecture. The
    /// struct is opaque here: we hand `dbghelp` the bytes the kernel wrote and
    /// read only the program counter, stack pointer and frame pointer back.
    /// Offsets and sizes are `winnt.h`'s `ARM64_NT_CONTEXT` and AMD64 `CONTEXT`.
    #[cfg(target_arch = "aarch64")]
    mod arch {
        /// `IMAGE_FILE_MACHINE_ARM64`.
        pub(super) const MACHINE: u32 = 0xAA64;
        pub(super) const CONTEXT_SIZE: usize = 0x390;
        pub(super) const FLAGS_OFFSET: usize = 0x0;
        /// `CONTEXT_ARM64 | CONTROL | INTEGER | FLOATING_POINT`.
        pub(super) const FLAGS_FULL: u32 = 0x0040_0007;
        pub(super) const PC_OFFSET: usize = 0x108;
        pub(super) const SP_OFFSET: usize = 0x100;
        /// `X29`.
        pub(super) const FP_OFFSET: usize = 0xF0;
    }
    #[cfg(target_arch = "x86_64")]
    mod arch {
        /// `IMAGE_FILE_MACHINE_AMD64`.
        pub(super) const MACHINE: u32 = 0x8664;
        pub(super) const CONTEXT_SIZE: usize = 0x4D0;
        pub(super) const FLAGS_OFFSET: usize = 0x30;
        /// `CONTEXT_AMD64 | CONTROL | INTEGER | FLOATING_POINT`.
        pub(super) const FLAGS_FULL: u32 = 0x0010_000B;
        /// `Rip`.
        pub(super) const PC_OFFSET: usize = 0xF8;
        /// `Rsp`.
        pub(super) const SP_OFFSET: usize = 0x98;
        /// `Rbp`.
        pub(super) const FP_OFFSET: usize = 0xA0;
    }

    /// A `CONTEXT`, 16-byte aligned as both architectures require.
    #[repr(C, align(16))]
    pub(super) struct ThreadContext([u8; arch::CONTEXT_SIZE]);

    impl ThreadContext {
        fn new() -> Box<Self> {
            let mut ctx = Box::new(Self([0; arch::CONTEXT_SIZE]));
            ctx.0[arch::FLAGS_OFFSET..arch::FLAGS_OFFSET + 4]
                .copy_from_slice(&arch::FLAGS_FULL.to_le_bytes());
            ctx
        }
        /// Constant, in-bounds offsets: this cannot panic, which matters
        /// because [`take_snapshot`] calls it while the thread is suspended.
        fn u64_at(&self, off: usize) -> u64 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.0[off..off + 8]);
            u64::from_le_bytes(b)
        }
        fn pc(&self) -> u64 {
            self.u64_at(arch::PC_OFFSET)
        }
        fn sp(&self) -> u64 {
            self.u64_at(arch::SP_OFFSET)
        }
        fn fp(&self) -> u64 {
            self.u64_at(arch::FP_OFFSET)
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        /// FILETIMEs count 100 ns ticks.
        fn as_duration(self) -> Duration {
            Duration::from_nanos(((u64::from(self.high) << 32) | u64::from(self.low)) * 100)
        }
    }

    #[allow(dead_code)] // the OS reads these fields; Rust never does
    #[repr(C)]
    struct MemoryBasicInformation {
        base_address: *mut c_void,
        allocation_base: *mut c_void,
        allocation_protect: u32,
        partition_id: u16,
        region_size: usize,
        state: u32,
        protect: u32,
        kind: u32,
    }

    /// `ADDRESS64`.
    #[allow(dead_code)] // the OS reads these fields; Rust never does
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Address64 {
        offset: u64,
        segment: u16,
        mode: u32,
    }
    /// `AddrModeFlat`.
    const ADDR_MODE_FLAT: u32 = 3;

    /// `STACKFRAME64`. `kd_help` is `KDHELP64`, 0x70 bytes, which nothing here
    /// reads but `dbghelp` writes.
    #[allow(dead_code)] // the OS reads these fields; Rust never does
    #[repr(C)]
    struct StackFrame64 {
        addr_pc: Address64,
        addr_return: Address64,
        addr_frame: Address64,
        addr_stack: Address64,
        addr_bstore: Address64,
        func_table_entry: *mut c_void,
        params: [u64; 4],
        far: i32,
        is_virtual: i32,
        reserved: [u64; 3],
        kd_help: [u64; 14],
    }

    /// Longest symbol name we take.
    const MAX_NAME: usize = 1024;
    /// `sizeof(SYMBOL_INFO)` with its one-byte `Name[1]`, which is what
    /// `SizeOfStruct` must say, however long the buffer behind it.
    const SYMBOL_INFO_SIZE: u32 = 88;

    /// `SYMBOL_INFO`, with room for a [`MAX_NAME`]-byte name.
    #[allow(dead_code)] // the OS reads these fields; Rust never does
    #[repr(C)]
    struct SymbolInfo {
        size_of_struct: u32,
        type_index: u32,
        reserved: [u64; 2],
        index: u32,
        size: u32,
        mod_base: u64,
        flags: u32,
        value: u64,
        address: u64,
        register: u32,
        scope: u32,
        tag: u32,
        name_len: u32,
        max_name_len: u32,
        name: [u8; MAX_NAME],
    }

    /// `IMAGEHLP_LINE64`.
    #[allow(dead_code)] // the OS reads these fields; Rust never does
    #[repr(C)]
    struct ImagehlpLine64 {
        size_of_struct: u32,
        key: *mut c_void,
        line_number: u32,
        file_name: *const u8,
        address: u64,
    }

    type ReadMemoryFn = unsafe extern "system" fn(Handle, u64, *mut c_void, u32, *mut u32) -> i32;
    type FunctionTableAccessFn = unsafe extern "system" fn(Handle, u64) -> *mut c_void;
    type GetModuleBaseFn = unsafe extern "system" fn(Handle, u64) -> u64;

    // The OS's own libraries, declared by hand as the Linux side declares its
    // unwinder: a diagnostic must not widen the workspace's dependency surface.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn GetCurrentProcess() -> Handle;
        fn GetLastError() -> u32;
        fn OpenThread(access: u32, inherit: i32, tid: u32) -> Handle;
        fn SuspendThread(thread: Handle) -> u32;
        fn ResumeThread(thread: Handle) -> u32;
        fn GetThreadContext(thread: Handle, ctx: *mut c_void) -> i32;
        fn GetThreadTimes(
            thread: Handle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn VirtualQueryEx(
            process: Handle,
            addr: *const c_void,
            info: *mut MemoryBasicInformation,
            len: usize,
        ) -> usize;
        fn ReadProcessMemory(
            process: Handle,
            base: *const c_void,
            buf: *mut c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
        fn DuplicateHandle(
            source_process: Handle,
            source: Handle,
            target_process: Handle,
            target: *mut Handle,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        /// A module's file name in ANY process, by its load address (an
        /// `HMODULE` is that address); a null module names the main executable.
        fn K32GetModuleFileNameExW(
            process: Handle,
            module: Handle,
            buf: *mut u16,
            size: u32,
        ) -> u32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(class: u32, buf: *mut u8, len: u32, returned: *mut u32) -> i32;
    }

    #[link(name = "dbghelp")]
    unsafe extern "system" {
        fn SymGetOptions() -> u32;
        fn SymSetOptions(options: u32) -> u32;
        fn SymInitializeW(process: Handle, search_path: *const u16, invade: i32) -> i32;
        fn SymCleanup(process: Handle) -> i32;
        fn SymRefreshModuleList(process: Handle) -> i32;
        fn StackWalk64(
            machine: u32,
            process: Handle,
            thread: Handle,
            frame: *mut StackFrame64,
            context: *mut c_void,
            read_memory: Option<ReadMemoryFn>,
            function_table_access: Option<FunctionTableAccessFn>,
            get_module_base: Option<GetModuleBaseFn>,
            translate_address: *const c_void,
        ) -> i32;
        fn SymFunctionTableAccess64(process: Handle, base: u64) -> *mut c_void;
        fn SymGetModuleBase64(process: Handle, addr: u64) -> u64;
        fn SymFromAddr(process: Handle, addr: u64, disp: *mut u64, sym: *mut SymbolInfo) -> i32;
        fn SymGetLineFromAddr64(
            process: Handle,
            addr: u64,
            disp: *mut u32,
            line: *mut ImagehlpLine64,
        ) -> i32;
    }

    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    const THREAD_GET_CONTEXT: u32 = 0x0008;
    const THREAD_QUERY_LIMITED_INFORMATION: u32 = 0x0800;
    const DUPLICATE_SAME_ACCESS: u32 = 0x2;
    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_VM_READ: u32 = 0x0010;
    const SYMOPT_UNDNAME: u32 = 0x2;
    const SYMOPT_DEFERRED_LOADS: u32 = 0x4;
    const SYMOPT_LOAD_LINES: u32 = 0x10;
    const SYMOPT_FAIL_CRITICAL_ERRORS: u32 = 0x200;
    const SYMOPT_NO_PROMPTS: u32 = 0x80000;
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

    /// Serializes captures, and with them every `dbghelp` call this module
    /// makes: the snapshot statics are one buffer, and `dbghelp` is documented
    /// single-threaded.
    static CAPTURING: Mutex<()> = Mutex::new(());

    pub(super) fn serialize_captures() -> MutexGuard<'static, ()> {
        CAPTURING.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The thread's id as `OpenThread` and the system's thread table take it.
    /// Windows thread ids are multiples of four well below `i32::MAX`, so the
    /// heartbeat's `i32` holds them unchanged.
    pub(crate) fn current_tid() -> i32 {
        // SAFETY: a read of this thread's own id; no arguments.
        unsafe { GetCurrentThreadId() as i32 }
    }

    /// The `dbghelp` session: a handle of its own to this process.
    ///
    /// `dbghelp` keys a session by the process handle it was given. Rust's own
    /// backtraces (a panic with `RUST_BACKTRACE`) open one on the
    /// `GetCurrentProcess()` pseudo-handle; a duplicated REAL handle is a
    /// different key, so this session never re-initializes or cleans up
    /// theirs. It is opened once and kept, because loading a debug binary's PDB
    /// costs seconds and a later stall would pay it again.
    fn session() -> Result<Handle, String> {
        static SESSION: OnceLock<Result<usize, String>> = OnceLock::new();
        SESSION
            .get_or_init(|| {
                let mut process: Handle = std::ptr::null_mut();
                // SAFETY: duplicates our own pseudo-handle into a real handle
                // written to the local above.
                let ok = unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        GetCurrentProcess(),
                        GetCurrentProcess(),
                        &mut process,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                };
                if ok == 0 {
                    // SAFETY: reads this thread's last-error value.
                    return Err(format!("DuplicateHandle failed (error {})", unsafe {
                        GetLastError()
                    }));
                }
                let dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_default();
                init_symbols(process, &[dir])?;
                Ok(process as usize)
            })
            .clone()
            .map(|h| h as Handle)
    }

    /// Open a `dbghelp` session on `process`, enumerating its modules now.
    ///
    /// An explicit search path, so `_NT_SYMBOL_PATH` (which may name a symbol
    /// server) is never consulted: a capture must not wait on the network.
    /// `dirs` is searched in order for each module's PDB by its file name.
    /// ⚠ A Rust binary embeds only that bare name (rustc links with
    /// `/PDBALTPATH:%_PDB%`, so no absolute path), which is why these
    /// directories, not the build's, decide whether a frame gets its line.
    fn init_symbols(process: Handle, dirs: &[std::path::PathBuf]) -> Result<(), String> {
        let wide: Vec<u16> = {
            use std::os::windows::ffi::OsStrExt;
            let joined = std::env::join_paths(dirs).unwrap_or_default();
            joined.encode_wide().chain(Some(0)).collect()
        };
        // SAFETY: plain dbghelp setup, under the capture lock. The options are
        // ADDED to whatever is set, never cleared, since they are process-wide.
        unsafe {
            SymSetOptions(
                SymGetOptions()
                    | SYMOPT_UNDNAME
                    | SYMOPT_DEFERRED_LOADS
                    | SYMOPT_LOAD_LINES
                    | SYMOPT_FAIL_CRITICAL_ERRORS
                    | SYMOPT_NO_PROMPTS,
            );
            if SymInitializeW(process, wide.as_ptr(), 1) == 0 {
                return Err(format!("SymInitialize failed (error {})", GetLastError()));
            }
        }
        Ok(())
    }

    /// A module's full path in `process`, by its load address; `None` (a null
    /// module) names the process's executable.
    fn module_path(process: Handle, module: Option<u64>) -> Option<String> {
        let mut buf = [0u16; 1024];
        let module = module.map_or(std::ptr::null_mut(), |base| base as Handle);
        // SAFETY: writes at most `buf.len()` UTF-16 units into `buf`.
        let n =
            unsafe { K32GetModuleFileNameExW(process, module, buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n.min(buf.len())]))
    }

    /// The process a capture reads: the handle its `dbghelp` session is keyed
    /// by, and its id.
    ///
    /// Two kinds. **This process** — tui's agent capturing its own stalled UI
    /// thread — uses the [`session`] kept for the life of the process. **Another
    /// process** — the FlaUI bridge's helper capturing the windows app, a C#
    /// process that hosts no agent of ours (`capture_foreign`) — gets a session
    /// of its own, closed when the capture ends.
    pub(super) struct Target {
        pub(super) process: Handle,
        pub(super) pid: u32,
        owned: bool,
    }

    impl Target {
        pub(super) fn own() -> Result<Self, String> {
            Ok(Self {
                process: session()?,
                pid: std::process::id(),
                owned: false,
            })
        }

        pub(super) fn foreign(pid: u32) -> Result<Self, String> {
            // SAFETY: opens a process by id for reading; null on failure.
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
            if process.is_null() {
                // SAFETY: reads this thread's last-error value.
                return Err(format!("could not open process {pid} (error {})", unsafe {
                    GetLastError()
                }));
            }
            // The target's own directory first, then the capturing binary's.
            // The second is what names a Rust library the target loaded: the
            // windows app runs a COPY of `fauna_ffi.dll`, and the 780 MB PDB
            // stays where cargo built it, which is also where the capture
            // helper is built (the same target directory and profile).
            let parent = |p: std::path::PathBuf| p.parent().map(|d| d.to_path_buf());
            let dirs: Vec<std::path::PathBuf> = [
                module_path(process, None)
                    .map(std::path::PathBuf::from)
                    .and_then(parent),
                std::env::current_exe().ok().and_then(parent),
            ]
            .into_iter()
            .flatten()
            .collect();
            if let Err(why) = init_symbols(process, &dirs) {
                // SAFETY: the handle opened above, closed once.
                unsafe { CloseHandle(process) };
                return Err(why);
            }
            Ok(Self {
                process,
                pid,
                owned: true,
            })
        }
    }

    impl Drop for Target {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: the session and handle `foreign` opened, each closed
                // once, under the capture lock the caller still holds.
                unsafe {
                    SymCleanup(self.process);
                    CloseHandle(self.process);
                }
            }
        }
    }

    /// An open handle to one thread, of this process or the [`Target`] one,
    /// closed on drop.
    pub(super) struct ThreadHandle(Handle);

    impl ThreadHandle {
        pub(super) fn open(tid: u32) -> Result<Self, String> {
            // SAFETY: opens a thread by id (thread ids are system-wide); null
            // on failure.
            let h = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    tid,
                )
            };
            if h.is_null() {
                // SAFETY: reads this thread's last-error value.
                return Err(format!("could not open thread {tid} (error {})", unsafe {
                    GetLastError()
                }));
            }
            Ok(Self(h))
        }

        /// User + kernel CPU time the thread has run so far.
        pub(super) fn cpu(&self) -> Option<Duration> {
            let zero = FileTime { low: 0, high: 0 };
            let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
            // SAFETY: four out-pointers to locals.
            let ok = unsafe { GetThreadTimes(self.0, &mut c, &mut e, &mut k, &mut u) };
            (ok != 0).then(|| k.as_duration() + u.as_duration())
        }
    }

    impl Drop for ThreadHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by `open` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// One sample: the registers, and the stack they point into, copied while
    /// the thread was suspended.
    pub(super) struct Snapshot {
        ctx: Box<ThreadContext>,
        /// The address `stack[0]` was copied from: the stack pointer.
        stack_lo: u64,
        stack: Vec<u8>,
        /// The whole stack's reservation, so a read of the part we did NOT
        /// copy can be refused rather than served from live memory.
        region_lo: u64,
        region_hi: u64,
    }

    impl Snapshot {
        /// Allocate everything a sample needs, BEFORE the thread is suspended.
        pub(super) fn prepare() -> Self {
            Self {
                ctx: ThreadContext::new(),
                stack_lo: 0,
                stack: vec![0u8; STACK_COPY_MAX],
                region_lo: 0,
                region_hi: 0,
            }
        }
    }

    /// Why a [`take_snapshot`] failed, as plain numbers: formatting the reason
    /// allocates, so it waits until the thread runs again.
    pub(super) enum SnapshotError {
        Suspend(u32),
        Context(u32),
        Query(u32),
        Read(u32),
    }

    impl SnapshotError {
        fn describe(&self) -> String {
            match self {
                Self::Suspend(e) => format!("SuspendThread failed (error {e})"),
                Self::Context(e) => format!("GetThreadContext failed (error {e})"),
                Self::Query(e) => {
                    format!("could not find the thread's stack (VirtualQuery error {e})")
                }
                Self::Read(e) => {
                    format!("could not copy the thread's stack (ReadProcessMemory error {e})")
                }
            }
        }
    }

    /// Suspend the thread, copy its registers and stack into `snap`, resume it.
    ///
    /// **The suspended window.** Between `SuspendThread` and `ResumeThread`
    /// this makes kernel calls only, into memory `snap` already owns: no Rust
    /// allocation (the thread may hold the heap lock), no lock of any kind, no
    /// formatting, nothing that can panic (a panic allocates its payload, and
    /// would unwind past the resume). An error is recorded as a number and
    /// described after the resume. `a_snapshot_allocates_nothing` holds this to
    /// zero allocations; the module doc says what an allocation here would do.
    ///
    /// `process` is the thread's own process (a [`Target`]'s handle), whose
    /// address space the stack is queried and copied out of.
    pub(super) fn take_snapshot(
        process: Handle,
        thread: &ThreadHandle,
        snap: &mut Snapshot,
    ) -> Result<(), SnapshotError> {
        // SAFETY: a thread of `process`, not this one (the caller refuses
        // that). On failure it returns u32::MAX and suspended nothing.
        if unsafe { SuspendThread(thread.0) } == u32::MAX {
            // SAFETY: reads this thread's last-error value.
            return Err(SnapshotError::Suspend(unsafe { GetLastError() }));
        }
        let mut result = Ok(());
        // SAFETY: `ctx` is a 16-aligned CONTEXT with its flags set; the call
        // writes within it. It also waits for the suspension to take effect.
        if unsafe { GetThreadContext(thread.0, snap.ctx.0.as_mut_ptr().cast()) } == 0 {
            // SAFETY: reads this thread's last-error value.
            result = Err(SnapshotError::Context(unsafe { GetLastError() }));
        } else {
            let sp = snap.ctx.sp();
            // SAFETY: zeroed plain-data struct, written by the call below.
            let mut mbi: MemoryBasicInformation = unsafe { std::mem::zeroed() };
            // SAFETY: a query about the thread's process's address space into
            // a local.
            let queried = unsafe {
                VirtualQueryEx(
                    process,
                    sp as *const c_void,
                    &mut mbi,
                    std::mem::size_of::<MemoryBasicInformation>(),
                )
            };
            if queried == 0 {
                // SAFETY: reads this thread's last-error value.
                result = Err(SnapshotError::Query(unsafe { GetLastError() }));
            } else {
                // The committed region holding the stack pointer runs up to the
                // stack's base: the frames live from `sp` to there.
                let top = (mbi.base_address as u64).saturating_add(mbi.region_size as u64);
                let want = (top.saturating_sub(sp) as usize).min(snap.stack.len());
                let mut copied = 0usize;
                // SAFETY: copies at most `want` ≤ `stack.len()` bytes from the
                // thread's process into the buffer allocated before the
                // suspension. A fault on the source fails the call rather than
                // the process.
                let ok = unsafe {
                    ReadProcessMemory(
                        process,
                        sp as *const c_void,
                        snap.stack.as_mut_ptr().cast(),
                        want,
                        &mut copied,
                    )
                };
                if ok == 0 && copied == 0 {
                    // SAFETY: reads this thread's last-error value.
                    result = Err(SnapshotError::Read(unsafe { GetLastError() }));
                }
                snap.stack_lo = sp;
                // `truncate` only lowers the length; it never allocates.
                snap.stack.truncate(copied);
                snap.region_lo = mbi.allocation_base as u64;
                snap.region_hi = top;
            }
        }
        // SAFETY: undoes the suspension above, exactly once.
        unsafe { ResumeThread(thread.0) };
        result
    }

    /// What the memory reader serves the walk from: the current snapshot.
    #[derive(Clone, Copy)]
    struct View {
        lo: u64,
        ptr: *const u8,
        len: usize,
        region_lo: u64,
        region_hi: u64,
    }

    thread_local! {
        /// `StackWalk64`'s memory callback takes no context argument, so the
        /// snapshot being walked is published here, on the walking thread, for
        /// the length of one walk.
        static VIEW: Cell<Option<View>> = const { Cell::new(None) };
    }

    /// `StackWalk64`'s memory reader: the stack from the copy, the images live.
    unsafe extern "system" fn read_memory(
        process: Handle,
        base: u64,
        buf: *mut c_void,
        size: u32,
        read: *mut u32,
    ) -> i32 {
        let Some(end) = base.checked_add(u64::from(size)) else {
            return 0;
        };
        if let Some(v) = VIEW.with(Cell::get) {
            if base >= v.lo && end <= v.lo + v.len as u64 {
                // SAFETY: the range lies inside the snapshot's buffer, which
                // outlives the walk; `buf` holds `size` bytes (the caller's
                // contract).
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        v.ptr.add((base - v.lo) as usize),
                        buf.cast::<u8>(),
                        size as usize,
                    );
                    if !read.is_null() {
                        *read = size;
                    }
                }
                return 1;
            }
            if base < v.region_hi && end > v.region_lo {
                // The stack, but not the part we copied: what is there now is
                // not what the thread held when it was sampled.
                return 0;
            }
        }
        let mut got = 0usize;
        // SAFETY: reads image memory of the walked process into the caller's
        // buffer; an unmapped address fails the call.
        let ok = unsafe {
            ReadProcessMemory(process, base as *const c_void, buf, size as usize, &mut got)
        };
        if !read.is_null() {
            // SAFETY: the caller's out-pointer.
            unsafe { *read = got as u32 };
        }
        ok
    }

    /// Unwind a snapshot. Returns each frame's address: the first is the
    /// instruction the thread was stopped at, every later one a return address.
    pub(super) fn walk(process: Handle, snap: &mut Snapshot) -> Vec<u64> {
        let flat = |offset| Address64 {
            offset,
            segment: 0,
            mode: ADDR_MODE_FLAT,
        };
        // SAFETY: zeroed plain-data struct; the three fields below seed it.
        let mut frame: StackFrame64 = unsafe { std::mem::zeroed() };
        frame.addr_pc = flat(snap.ctx.pc());
        frame.addr_frame = flat(snap.ctx.fp());
        frame.addr_stack = flat(snap.ctx.sp());
        VIEW.with(|v| {
            v.set(Some(View {
                lo: snap.stack_lo,
                ptr: snap.stack.as_ptr(),
                len: snap.stack.len(),
                region_lo: snap.region_lo,
                region_hi: snap.region_hi,
            }))
        });
        let mut ips: Vec<u64> = Vec::new();
        let mut last = (0u64, 0u64);
        while ips.len() < MAX_FRAMES {
            // SAFETY: `frame` and the copied context are ours and outlive the
            // call. The thread handle is null on purpose: with a memory reader
            // supplied, `dbghelp` needs none, and so can never read the LIVE
            // thread in place of the copy.
            let ok = unsafe {
                StackWalk64(
                    arch::MACHINE,
                    process,
                    std::ptr::null_mut(),
                    &mut frame,
                    snap.ctx.0.as_mut_ptr().cast(),
                    Some(read_memory),
                    Some(SymFunctionTableAccess64),
                    Some(SymGetModuleBase64),
                    std::ptr::null(),
                )
            };
            let here = (frame.addr_pc.offset, frame.addr_stack.offset);
            if ok == 0 || here.0 == 0 || here == last {
                break;
            }
            last = here;
            ips.push(here.0);
        }
        VIEW.with(|v| v.set(None));
        ips
    }

    /// The module an address lies in, in the walked process: its file name
    /// and load address. Asked of the `dbghelp` session, which enumerated the
    /// process's modules when it opened (and again at each capture's
    /// `SymRefreshModuleList`), so it answers for another process exactly as
    /// for this one.
    fn module_of(process: Handle, addr: u64) -> Option<(String, u64)> {
        // SAFETY: plain dbghelp query on the caller's session, under the lock.
        let base = unsafe { SymGetModuleBase64(process, addr) };
        if base == 0 {
            return None;
        }
        let path = module_path(process, Some(base)).unwrap_or_else(|| "?".to_string());
        let short = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        Some((short, base))
    }

    /// One named frame, and whether it lay in no module at all.
    pub(super) struct Named {
        pub(super) text: String,
        pub(super) moduleless: bool,
    }

    /// Name each frame: `module  fn (file:line)`, falling back to the nearest
    /// exported name and then to `module+0x…` — a degraded frame is still a
    /// frame.
    pub(super) fn symbolize(process: Handle, ips: &[u64]) -> Vec<Named> {
        ips.iter()
            .enumerate()
            .map(|(i, &ip)| {
                // A return address points after the call, which can belong to
                // the next line or function; step back into the call. The first
                // frame is the stopped instruction itself and is not adjusted.
                let at = if i == 0 { ip } else { ip.saturating_sub(1) };
                let Some((module, base)) = module_of(process, at) else {
                    return Named {
                        text: format!(
                            "?  +0x{ip:x} (no module: code compiled at run time, or freed)"
                        ),
                        moduleless: true,
                    };
                };
                let name = symbol_name(process, at);
                let line = source_line(process, at);
                let text = match (name, line) {
                    (Some(n), Some(l)) => format!("{module}  {n} ({l})"),
                    (Some(n), None) => format!("{module}  {n}"),
                    (None, _) => format!("{module}  +0x{:x}", at - base),
                };
                Named {
                    text,
                    moduleless: false,
                }
            })
            .collect()
    }

    /// What the report says where the walk reaches code no module holds.
    ///
    /// The walk unwinds through each image's own unwind tables, and code
    /// compiled at run time has none `dbghelp` can find — a .NET runtime
    /// registers its JIT code's tables with the OS inside its own process,
    /// and a managed app's PDB is a portable PDB `dbghelp` cannot read. So
    /// every frame past the first such one is a guess, and is not reported.
    pub(crate) const MANAGED_FRAMES_UNMEASURED: &str = "frames below are UNMEASURED: the \
frame above lies in code compiled at run time (a .NET app's managed code is JIT-compiled), which \
has no module, unwind table or PDB dbghelp can read, so the walk stops there; the frames above it \
are the native ones";

    /// `SymTagFunction`: a function's own record in a PDB, which spans it.
    const SYM_TAG_FUNCTION: u32 = 5;

    /// The symbol covering `addr`. A PDB function record names the function
    /// itself. Anything else (a public or exported name: every `ntdll` frame,
    /// and code the PDB has no record for) is only the NEAREST name before the
    /// address, so it carries its distance in the debugger's own notation,
    /// `name+0x1c`: a small distance is that function, a large one may be a
    /// neighbour.
    fn symbol_name(process: Handle, addr: u64) -> Option<String> {
        // SAFETY: zeroed plain-data struct; the two size fields are set below.
        let mut sym: Box<SymbolInfo> = Box::new(unsafe { std::mem::zeroed() });
        sym.size_of_struct = SYMBOL_INFO_SIZE;
        sym.max_name_len = MAX_NAME as u32;
        let mut disp = 0u64;
        // SAFETY: `sym` has room for `max_name_len` name bytes.
        if unsafe { SymFromAddr(process, addr, &mut disp, &mut *sym) } == 0 {
            return None;
        }
        let len = (sym.name_len as usize).min(MAX_NAME);
        let name = String::from_utf8_lossy(&sym.name[..len]).into_owned();
        Some(if sym.tag == SYM_TAG_FUNCTION {
            name
        } else {
            format!("{name}+0x{disp:x}")
        })
    }

    fn source_line(process: Handle, addr: u64) -> Option<String> {
        let mut line = ImagehlpLine64 {
            size_of_struct: std::mem::size_of::<ImagehlpLine64>() as u32,
            key: std::ptr::null_mut(),
            line_number: 0,
            file_name: std::ptr::null(),
            address: 0,
        };
        let mut disp = 0u32;
        // SAFETY: `line` is a sized IMAGEHLP_LINE64; dbghelp writes a pointer
        // to a NUL-terminated name it owns, read at once below.
        if unsafe { SymGetLineFromAddr64(process, addr, &mut disp, &mut line) } == 0
            || line.file_name.is_null()
        {
            return None;
        }
        // SAFETY: as above.
        let file = unsafe { std::ffi::CStr::from_ptr(line.file_name.cast()) }.to_string_lossy();
        Some(format!("{file}:{}", line.line_number))
    }

    /// `(state, wait reason)` of one thread of process `pid`, from the
    /// system's thread table (`SYSTEM_PROCESS_INFORMATION` and the
    /// `SYSTEM_THREAD_INFORMATION` rows after it, 64-bit layout).
    pub(super) fn thread_state(pid: u32, tid: u32) -> Option<(u32, u32)> {
        let mut len = 1usize << 20;
        let buf = loop {
            // u64 elements, so the table is 8-byte aligned.
            let mut buf = vec![0u64; len / 8];
            let mut returned = 0u32;
            // SAFETY: the call writes at most `len` bytes into `buf`.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    len as u32,
                    &mut returned,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH && len < (64 << 20) {
                len = (returned as usize + (64 << 10)).max(len * 2);
                continue;
            }
            if status != 0 {
                return None;
            }
            break buf;
        };
        // SAFETY: a byte view of the u64 buffer above, same length in bytes.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
        let u32_at = |off: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                bytes.get(off..off + 4)?.try_into().ok()?,
            ))
        };
        let u64_at = |off: usize| -> Option<u64> {
            Some(u64::from_le_bytes(
                bytes.get(off..off + 8)?.try_into().ok()?,
            ))
        };
        let pid = u64::from(pid);
        let mut entry = 0usize;
        loop {
            let next = u32_at(entry)? as usize;
            if u64_at(entry + 0x50)? == pid {
                let threads = u32_at(entry + 4)? as usize;
                for i in 0..threads {
                    let row = entry + 0x100 + i * 0x50;
                    if u64_at(row + 0x30)? == u64::from(tid) {
                        return Some((u32_at(row + 0x44)?, u32_at(row + 0x48)?));
                    }
                }
                return None;
            }
            if next == 0 {
                return None;
            }
            entry += next;
        }
    }

    /// `KTHREAD_STATE` 5.
    pub(super) const STATE_WAITING: u32 = 5;

    fn state_words(state: u32, reason: u32) -> String {
        let state_word = match state {
            0 => "Initialized",
            1 => "Ready (runnable, given no core: the box, not the thread)",
            2 => "Running",
            3 => "Standby (chosen to run next)",
            4 => "Terminated",
            5 => "Waiting",
            6 => "Transition",
            7 => "DeferredReady (runnable, given no core: the box, not the thread)",
            _ => "?",
        };
        if state != STATE_WAITING {
            return format!("{state_word} ({state})");
        }
        // The `KWAIT_REASON` values a stalled UI thread is usually in; any other
        // is printed as its number, never guessed at.
        let reason_word = match reason {
            4 => " DelayExecution (a sleep)",
            5 => " Suspended",
            6 => " UserRequest (a wait on a handle)",
            13 => " WrUserRequest (a wait on a handle)",
            15 => " WrQueue (an I/O completion or worker queue)",
            37 => " WrAlertByThreadId (a lock, condvar or channel)",
            _ => "",
        };
        format!("Waiting (wait reason {reason}{reason_word})")
    }

    pub(crate) fn capture(tid: i32) -> String {
        if tid <= 0 {
            return " — the host's heartbeat recorded no thread id, so its stack could not be \
captured (a host beating from a thread this agent can name gets it free)"
                .to_string();
        }
        if tid == current_tid() {
            return " — the stalled thread is the one asked to capture it, which cannot suspend \
itself, so what holds it is UNMEASURED"
                .to_string();
        }
        let _serial = serialize_captures();
        let header = format!("\nUI-THREAD STALL STACK (tid {tid})");
        capture_in(header, Target::own(), tid as u32)
    }

    /// Capture thread `tid` of ANOTHER process, `pid` — the windows app's UI
    /// thread, asked for by the FlaUI bridge on its app-side verdict through
    /// the `fauna-e2e-stall-capture` helper. The same capture as [`capture`]:
    /// only the process the stack is read out of differs.
    pub(crate) fn capture_foreign(pid: u32, tid: u32) -> String {
        if tid == 0 {
            return " — no thread id was given, so the stack could not be captured".to_string();
        }
        if pid == std::process::id() && tid as i32 == current_tid() {
            return " — the stalled thread is the one asked to capture it, which cannot suspend \
itself, so what holds it is UNMEASURED"
                .to_string();
        }
        let _serial = serialize_captures();
        let header = format!("\nUI-THREAD STALL STACK (pid {pid}, tid {tid})");
        capture_in(header, Target::foreign(pid), tid)
    }

    /// The capture both entry points share, under the caller's capture lock.
    fn capture_in(mut out: String, target: Result<Target, String>, tid_u: u32) -> String {
        let target = match target {
            Ok(t) => t,
            Err(why) => {
                out.push_str(&format!("\n  stack NOT captured: {why}"));
                return out;
            }
        };
        let process = target.process;
        // Modules loaded since the session opened (a plugin, a late DLL) must
        // be known before their frames are named.
        // SAFETY: plain dbghelp call on the target's session, under the lock.
        unsafe { SymRefreshModuleList(process) };
        let thread = match ThreadHandle::open(tid_u) {
            Ok(t) => t,
            Err(why) => {
                out.push_str(&format!("\n  stack NOT captured: {why}"));
                return out;
            }
        };

        // Both samples are taken before anything is walked or named, so the
        // second is as close to the first as `SAMPLE_GAP` says. The window is
        // MEASURED, as on Linux, never assumed.
        let (mut first, mut second) = (Snapshot::prepare(), Snapshot::prepare());
        let started = Instant::now();
        let before = thread.cpu();
        let first_taken = take_snapshot(process, &thread, &mut first);
        std::thread::sleep(SAMPLE_GAP);
        let after = thread.cpu();
        let state = thread_state(target.pid, tid_u);
        let window = started.elapsed();
        let second_taken = take_snapshot(process, &thread, &mut second);

        let state_text = state.map_or_else(|| "?".to_string(), |(s, r)| state_words(s, r));
        match (before, after) {
            (Some(cpu0), Some(cpu1)) => {
                let delta = cpu1.saturating_sub(cpu0);
                out.push_str(&format!(
                    "\n  state {state_text}, CPU +{:.2}s over the {:.2}s sample: {}",
                    delta.as_secs_f64(),
                    window.as_secs_f64(),
                    cpu_verdict(delta, window),
                ));
            }
            _ => out.push_str(&format!("\n  state {state_text}, CPU time UNMEASURED")),
        }

        if let Err(why) = first_taken {
            out.push_str(&format!("\n  stack NOT captured: {}", why.describe()));
            return out;
        }
        let ips = walk(process, &mut first);
        if ips.is_empty() {
            out.push_str("\n  stack NOT captured: the walk produced no frames");
            return out;
        }
        for (i, f) in symbolize(process, &ips).iter().enumerate() {
            out.push_str(&format!("\n  #{i:02} {}", f.text));
            if f.moduleless {
                out.push_str(&format!("\n  … {MANAGED_FRAMES_UNMEASURED}"));
                break;
            }
        }
        // Compared by the stopped instruction's raw address, as on Linux: two
        // call sites in one function share a name and differ here.
        if second_taken.is_ok() {
            let later = walk(process, &mut second);
            if let (Some(a), Some(b)) = (ips.first(), later.first()) {
                out.push_str(&second_sample_clause(a != b));
            }
        }
        out
    }
}

#[cfg(not(any(
    target_os = "linux",
    all(windows, any(target_arch = "aarch64", target_arch = "x86_64"))
)))]
mod imp {
    /// No capture on this host: Linux and Windows (aarch64, x86_64) each have
    /// one, and a host without one must read as *unmeasured* rather than as "no
    /// stall found" — the same rule the heartbeat itself follows.
    pub(crate) fn capture(_tid: i32) -> String {
        " — no UI-thread stack capture on this OS (Linux and Windows only), so what holds the \
thread is UNMEASURED"
            .to_string()
    }

    /// No capture on this host, of any process.
    pub(crate) fn capture_foreign(_pid: u32, _tid: u32) -> String {
        capture(0)
    }

    /// Every host without a capture reports 0, which [`capture`] reads as
    /// "unknown".
    pub(crate) fn current_tid() -> i32 {
        0
    }
}

pub(crate) use imp::{capture, capture_foreign, current_tid};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_burned_a_core_reads_as_computing() {
        let v = cpu_verdict(Duration::from_millis(290), SAMPLE_GAP);
        assert!(v.contains("COMPUTING"), "{v}");
    }

    #[test]
    fn a_thread_that_burned_nothing_reads_as_waiting() {
        let v = cpu_verdict(Duration::ZERO, SAMPLE_GAP);
        assert!(v.contains("WAITING"), "{v}");
        // The distinction is the whole point: a parked thread and a spinning
        // one need opposite fixes, and calling one the other sends a session
        // hunting the wrong suspect.
        assert!(!v.contains("COMPUTING"), "{v}");
    }

    #[test]
    fn an_empty_window_is_unmeasured_rather_than_idle() {
        let v = cpu_verdict(Duration::ZERO, Duration::ZERO);
        assert!(v.contains("UNMEASURED"), "{v}");
    }

    const THIS_BINARY: &str = "/target/debug/deps/fauna_e2e_agent-0123456789abcdef";

    fn frame(
        module: &'static str,
        symbol: Option<&'static str>,
        interrupted: bool,
    ) -> FrameFacts<'static> {
        FrameFacts {
            module,
            symbol,
            interrupted,
        }
    }

    #[test]
    fn an_aarch64_stack_starts_after_the_vdso_trampoline() {
        let frames = [
            frame(THIS_BINARY, None, false),
            frame("linux-vdso.so.1", Some("__kernel_rt_sigreturn"), false),
            frame("/lib/aarch64-linux-gnu/libc.so.6", Some("syscall"), true),
            frame(THIS_BINARY, None, false),
        ];
        assert_eq!(interrupted_frame_index(&frames), Some(2));
    }

    /// The stack a parked thread produced on an x86-64 `ubuntu-24.04` CI
    /// runner, 2026-09-15. glibc's trampoline `__restore_rt` is a LOCAL
    /// symbol, so `dladdr` names the nearest exported one before it
    /// (`__sigaction`) — a name that is an accident of that libc's link order,
    /// which is why it must not simply join the name list.
    #[test]
    fn an_x86_64_glibc_stack_starts_at_the_interrupted_frame_though_its_trampoline_is_unnamed() {
        let frames = [
            frame(THIS_BINARY, None, false),
            frame(
                "/lib/x86_64-linux-gnu/libc.so.6",
                Some("__sigaction"),
                false,
            ),
            frame("/lib/x86_64-linux-gnu/libc.so.6", Some("syscall"), true),
            frame(THIS_BINARY, None, false),
        ];
        assert_eq!(
            interrupted_frame_index(&frames),
            Some(2),
            "the trampoline must be stripped, and the parked thread's own libc \
             `syscall` after it kept"
        );
    }

    #[test]
    fn a_named_trampoline_is_found_even_where_the_unwinder_flags_nothing() {
        let frames = [
            frame(THIS_BINARY, None, false),
            frame(
                "/lib/x86_64-linux-gnu/libc.so.6",
                Some("__restore_rt"),
                false,
            ),
            frame(THIS_BINARY, None, false),
        ];
        assert_eq!(interrupted_frame_index(&frames), Some(2));
    }

    /// Nested signals: the walk starts in OUR handler, so the first signal
    /// frame from the top is the one our signal interrupted, and an older one
    /// further down belongs to the interrupted code.
    #[test]
    fn the_nearest_signal_frame_is_the_boundary() {
        let frames = [
            frame(THIS_BINARY, None, false),
            frame(
                "/lib/x86_64-linux-gnu/libc.so.6",
                Some("__sigaction"),
                false,
            ),
            frame(THIS_BINARY, None, true),
            frame(
                "/lib/x86_64-linux-gnu/libc.so.6",
                Some("__sigaction"),
                false,
            ),
            frame("/lib/x86_64-linux-gnu/libc.so.6", Some("syscall"), true),
        ];
        assert_eq!(interrupted_frame_index(&frames), Some(2));
    }

    /// No boundary means nothing is stripped: a stack that still opens with
    /// the handler is a visible defect, where one cut at a guess could
    /// silently drop the frame that holds the thread.
    #[test]
    fn a_stack_with_neither_anchor_has_no_boundary() {
        let frames = [
            frame(THIS_BINARY, None, false),
            frame("/lib/x86_64-linux-gnu/libc.so.6", Some("syscall"), false),
        ];
        assert_eq!(interrupted_frame_index(&frames), None);
    }

    /// The fixtures both captures are proved on: frames with names no other
    /// code in this workspace has.
    #[cfg(any(
        target_os = "linux",
        all(windows, any(target_arch = "aarch64", target_arch = "x86_64"))
    ))]
    mod fixtures {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        /// A frame with a name no other code in this workspace has, so finding
        /// it in a captured stack proves the capture walked the *target*
        /// thread's stack and not the handler's or the caller's.
        #[inline(never)]
        pub(super) fn e2e_stall_fixture_parks_here(
            rx: &mpsc::Receiver<()>,
            entered: &mpsc::Sender<i32>,
        ) {
            let _ = entered.send(current_tid());
            let _ = rx.recv();
        }

        #[inline(never)]
        pub(super) fn e2e_stall_fixture_spins_here(stop: &AtomicBool, entered: &mpsc::Sender<i32>) {
            let _ = entered.send(current_tid());
            while !stop.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        }

        /// Pull `CPU +X.XXs over the Y.YYs sample` back out of a report.
        pub(super) fn reported_cpu_and_window(report: &str) -> (f64, f64) {
            let line = report
                .lines()
                .find(|l| l.contains("CPU +"))
                .unwrap_or_else(|| panic!("no CPU line in:\n{report}"));
            let after_cpu = line.split("CPU +").nth(1).expect("CPU +");
            let cpu: f64 = after_cpu
                .split('s')
                .next()
                .and_then(|s| s.parse().ok())
                .expect("a CPU number");
            let window: f64 = line
                .split("over the ")
                .nth(1)
                .and_then(|s| s.split('s').next())
                .and_then(|s| s.parse().ok())
                .expect("a window number");
            (cpu, window)
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::fixtures::*;
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, mpsc};

        /// The tid arrives from inside the fixture's `send`, before it reaches
        /// `recv`, so a capture taken at once can sample the thread still
        /// finishing the send (seen on a loaded box). Wait until the kernel has
        /// it asleep.
        fn wait_until_asleep(tid: i32) {
            use std::time::{Duration, Instant};
            let deadline = Instant::now() + Duration::from_secs(10);
            while super::super::imp::thread_state(tid).is_none_or(|(state, _)| state != 'S') {
                assert!(
                    Instant::now() < deadline,
                    "thread {tid} never went to sleep"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        #[test]
        fn a_parked_thread_names_the_frame_it_is_parked_in() {
            let (release_tx, release_rx) = mpsc::channel();
            let (entered_tx, entered_rx) = mpsc::channel();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_parks_here(&release_rx, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");
            wait_until_asleep(tid);

            let report = capture(tid);
            assert!(
                report.contains("e2e_stall_fixture_parks_here"),
                "the capture must name the frame the thread sits in:\n{report}"
            );
            assert!(
                report.contains("WAITING"),
                "a blocked thread burns no CPU:\n{report}"
            );
            // The reported stack must start at the interrupted code, not at the
            // capture: leaving the handler's own frames on would point every
            // reader of every stall at this file.
            assert!(
                !report.contains("on_capture_signal") && !report.contains("_Unwind_Backtrace"),
                "the capture's own frames must be stripped:\n{report}"
            );
            // A parked thread is parked: both samples sit at the same address.
            // ⚠ Stated rather than implied: this assertion alone does NOT prove
            // the samples are compared after stripping — the handler frames are
            // identical in every sample, so a comparison made before stripping
            // would call every thread parked and pass here for the wrong
            // reason. What rules that out is the stripping assertion above,
            // since both now read the same stripped list.
            assert!(
                report.contains("parked there"),
                "a thread blocked on a channel must read as parked:\n{report}"
            );

            // ...and sampling it must not have wedged it: the whole instrument
            // is worthless if diagnosing a stall creates one.
            let _ = release_tx.send(());
            handle.join().expect("the sampled thread still finishes");
        }

        /// The fallback anchor, witnessed on a real stack. Where the trampoline
        /// HAS a name (aarch64's vdso), the unwinder's signal-frame flag must
        /// land on exactly the frame after it: the same boundary by the other
        /// route. That agreement is what trusting the flag alone rests on where
        /// the name is missing (x86-64 glibc), and it is the half of that claim
        /// a machine with a named trampoline can run.
        #[test]
        fn the_unwinder_flags_the_frame_after_the_trampoline_as_interrupted() {
            use super::super::imp::{sample_stack, serialize_captures, with_frame_facts};

            let (release_tx, release_rx) = mpsc::channel();
            let (entered_tx, entered_rx) = mpsc::channel();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_parks_here(&release_rx, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");
            wait_until_asleep(tid);

            let frames = sample_stack(&serialize_captures(), tid)
                .expect("the parked thread answers the capture");
            let (by_name, by_flag, listing) = with_frame_facts(&frames, |facts| {
                let listing = facts
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        format!(
                            "  #{i:02} {} {:?} interrupted={}",
                            f.module, f.symbol, f.interrupted
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    trampoline_by_name(facts),
                    first_interrupted_frame(facts),
                    listing,
                )
            });
            let _ = release_tx.send(());
            handle.join().expect("the sampled thread still finishes");

            assert!(
                by_flag.is_some_and(|i| i > 0),
                "the unwinder flagged no frame below the handler as interrupted, so \
                 the fallback anchor has nothing to find:\n{listing}"
            );
            if let Some(trampoline) = by_name {
                assert_eq!(
                    by_flag,
                    Some(trampoline + 1),
                    "the flag must mark the frame right after the named trampoline:\n{listing}"
                );
            }
        }

        #[test]
        fn a_spinning_thread_reads_as_computing_and_names_its_frame() {
            let stop = Arc::new(AtomicBool::new(false));
            let (entered_tx, entered_rx) = mpsc::channel();
            let spin_stop = stop.clone();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_spins_here(&spin_stop, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");

            let report = capture(tid);
            assert!(
                report.contains("e2e_stall_fixture_spins_here"),
                "a CPU-bound thread's frame must be named too:\n{report}"
            );
            // Not `COMPUTING`: on an oversubscribed box the scheduler can give
            // the spinner under half a core across the window (0.16s of 0.33s
            // measured on a saturated Linux host), which `cpu_verdict` rightly calls
            // PARTLY RUNNING. Its threshold is pinned by the pure tests above;
            // what this real capture must hold whatever the load is that a
            // runnable spinner never reads as a wait.
            assert!(
                report.contains("state R") && !report.contains("WAITING —"),
                "a thread burning a core must not read as waiting:\n{report}"
            );
            // One thread cannot burn more CPU than the window it was measured
            // over. The reported window must therefore be the MEASURED one —
            // asserting `SAMPLE_GAP` while the capture waited on a slow handler
            // reported 770% of a core (found by mutation, 2026-09-10).
            let (cpu, window) = reported_cpu_and_window(&report);
            assert!(
                cpu <= window * 1.5,
                "a single thread cannot burn {cpu:.2}s of CPU in a {window:.2}s window:\n{report}"
            );

            stop.store(true, Ordering::Relaxed);
            handle.join().expect("the sampled thread still finishes");
        }

        #[test]
        fn an_unknown_thread_id_reports_no_capture_rather_than_guessing() {
            let report = capture(0);
            assert!(report.contains("no thread id"), "{report}");
            assert!(!report.contains("#00"), "{report}");
        }

        #[test]
        fn a_beating_thread_can_be_named_by_its_tid() {
            // `current_tid` is what wires the heartbeat to the capture: the
            // thread that beats is the thread we sample.
            let mine = current_tid();
            assert!(mine > 0, "gettid must answer on Linux");
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(current_tid()).unwrap())
                .join()
                .unwrap();
            assert_ne!(mine, rx.recv().unwrap(), "each thread has its own tid");
        }
    }

    #[cfg(all(windows, any(target_arch = "aarch64", target_arch = "x86_64")))]
    mod windows {
        use super::super::imp::{
            STATE_WAITING, Snapshot, Target, ThreadHandle, serialize_captures, take_snapshot,
            thread_state,
        };
        use super::fixtures::*;
        use super::*;
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, mpsc};
        use std::time::Instant;

        /// Counts this thread's Rust heap allocations, so a test can hold the
        /// capture's suspended window to zero of them. It only counts, and it
        /// stands for the whole test binary, since a global allocator is one
        /// per binary; every other test is unaffected.
        struct Counting;

        thread_local! {
            static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
        }

        fn count() {
            // `try_with`: an allocation during this thread's teardown, after
            // its locals are gone, is not counted rather than a panic.
            let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        }

        fn allocations_on_this_thread() -> u64 {
            ALLOCATIONS.with(Cell::get)
        }

        // SAFETY: every call forwards to `System` unchanged; counting is a
        // thread-local integer bump that never allocates.
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                count();
                // SAFETY: forwarded with the caller's own contract.
                unsafe { System.alloc(layout) }
            }
            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                count();
                // SAFETY: as above.
                unsafe { System.alloc_zeroed(layout) }
            }
            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
                count();
                // SAFETY: as above.
                unsafe { System.realloc(ptr, layout, new_size) }
            }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                // SAFETY: as above.
                unsafe { System.dealloc(ptr, layout) }
            }
        }

        #[global_allocator]
        static COUNTING: Counting = Counting;

        /// The tid arrives from inside the fixture's `send`, before it reaches
        /// `recv`; wait until the scheduler has it in a wait, as the Linux twin
        /// waits for `S`.
        fn wait_until_waiting(tid: i32) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while thread_state(std::process::id(), tid as u32)
                .is_none_or(|(state, _)| state != STATE_WAITING)
            {
                assert!(
                    Instant::now() < deadline,
                    "thread {tid} never entered a wait"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        fn park_a_fixture() -> (mpsc::Sender<()>, std::thread::JoinHandle<()>, i32) {
            let (release_tx, release_rx) = mpsc::channel();
            let (entered_tx, entered_rx) = mpsc::channel();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_parks_here(&release_rx, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");
            wait_until_waiting(tid);
            (release_tx, handle, tid)
        }

        #[test]
        fn a_parked_thread_names_the_frame_it_is_parked_in() {
            let (release_tx, handle, tid) = park_a_fixture();

            let report = capture(tid);
            // Named from the PDB, with its source line: the frame a fix acts
            // on, not just an address in the test binary.
            assert!(
                report
                    .lines()
                    .any(|l| l.contains("e2e_stall_fixture_parks_here") && l.contains("stall.rs:")),
                "the capture must name the frame the thread sits in, with its line:\n{report}"
            );
            assert!(
                report.contains("state Waiting") && report.contains("WAITING"),
                "a blocked thread is in a wait and burns no CPU:\n{report}"
            );
            // The stack starts where the thread sits. Windows runs no code on
            // the target to take it, so no frame of the capture's own can be
            // on it; one that is would mean the walk ran on the wrong thread.
            assert!(
                !report.contains("take_snapshot") && !report.contains("stall::imp::capture"),
                "the capture's own frames must not appear:\n{report}"
            );
            assert!(
                report.contains("parked there"),
                "a thread blocked on a channel must read as parked:\n{report}"
            );

            // ...and sampling it must not have wedged it.
            let _ = release_tx.send(());
            handle.join().expect("the sampled thread still finishes");
        }

        /// The FlaUI bridge's path: the thread named by a process id and a
        /// thread id, read through a process handle and a `dbghelp` session of
        /// its own rather than this process's kept one. Proved here on a thread
        /// of this test binary (the handle is a second, independent one to
        /// the same process); the windows app e2e proves it across processes.
        #[test]
        fn a_thread_named_by_its_process_id_is_captured_through_that_process() {
            let (release_tx, handle, tid) = park_a_fixture();

            let report = capture_foreign(std::process::id(), tid as u32);
            let _ = release_tx.send(());
            handle.join().expect("the sampled thread still finishes");

            assert!(
                report.contains(&format!("(pid {}, tid {tid})", std::process::id())),
                "the report names the process it read:\n{report}"
            );
            assert!(
                report
                    .lines()
                    .any(|l| l.contains("e2e_stall_fixture_parks_here") && l.contains("stall.rs:")),
                "the foreign path must name the frame the thread sits in, with its line:\n{report}"
            );
            assert!(
                report.contains("state Waiting") && report.contains("parked there"),
                "{report}"
            );
        }

        #[test]
        fn a_process_that_cannot_be_opened_reports_no_capture() {
            // Pid 0 is the System Idle Process, which no user process opens.
            let report = capture_foreign(0, 4);
            assert!(report.contains("stack NOT captured"), "{report}");
            assert!(!report.contains("#00"), "{report}");
        }

        /// The hazard the module doc names, held to a number: while the target
        /// is suspended the capture must not allocate, since the target may
        /// hold the heap lock and an allocation would then wait on it for good.
        #[test]
        fn a_snapshot_allocates_nothing_while_the_thread_is_suspended() {
            let (release_tx, handle, tid) = park_a_fixture();
            let thread = ThreadHandle::open(tid as u32).expect("open the fixture thread");
            let _serial = serialize_captures();
            let target = Target::own().expect("this process's dbghelp session");
            let mut snap = Snapshot::prepare();

            let before = allocations_on_this_thread();
            let taken = take_snapshot(target.process, &thread, &mut snap).is_ok();
            let during = allocations_on_this_thread() - before;

            let _ = release_tx.send(());
            handle.join().expect("the sampled thread still finishes");
            assert!(taken, "the parked thread must be sampled");
            assert_eq!(
                during, 0,
                "take_snapshot allocated {during} time(s) while the thread was suspended"
            );
        }

        #[test]
        fn a_spinning_thread_reads_as_computing_and_names_its_frame() {
            let stop = Arc::new(AtomicBool::new(false));
            let (entered_tx, entered_rx) = mpsc::channel();
            let spin_stop = stop.clone();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_spins_here(&spin_stop, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");

            let report = capture(tid);
            stop.store(true, Ordering::Relaxed);
            handle.join().expect("the sampled thread still finishes");

            assert!(
                report.contains("e2e_stall_fixture_spins_here"),
                "a CPU-bound thread's frame must be named too:\n{report}"
            );
            // Not `COMPUTING`, for the reason the Linux twin gives: a loaded
            // box can give the spinner under half a core. A runnable spinner
            // must never read as a wait.
            assert!(
                !report.contains("WAITING —"),
                "a thread burning a core must not read as waiting:\n{report}"
            );
            let (cpu, window) = reported_cpu_and_window(&report);
            assert!(
                cpu <= window * 1.5,
                "a single thread cannot burn {cpu:.2}s of CPU in a {window:.2}s window:\n{report}"
            );
        }

        #[test]
        fn an_unknown_thread_id_reports_no_capture_rather_than_guessing() {
            let report = capture(0);
            assert!(report.contains("no thread id"), "{report}");
            assert!(!report.contains("#00"), "{report}");
        }

        /// A thread cannot suspend itself and then resume itself, so asking the
        /// capture to sample its own thread is refused rather than attempted.
        #[test]
        fn the_capturing_thread_is_never_asked_to_suspend_itself() {
            let report = capture(current_tid());
            assert!(report.contains("cannot suspend itself"), "{report}");
            assert!(!report.contains("#00"), "{report}");
        }

        #[test]
        fn a_beating_thread_can_be_named_by_its_tid() {
            let mine = current_tid();
            assert!(mine > 0, "GetCurrentThreadId must answer on Windows");
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(current_tid()).unwrap())
                .join()
                .unwrap();
            assert_ne!(mine, rx.recv().unwrap(), "each thread has its own tid");
        }
    }
}
