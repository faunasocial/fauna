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
//! # How it captures
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
//! # What it costs the app
//!
//! Nothing until a stall: the machinery is inert (no handler installed, no
//! thread, no timer) until an op has already timed out and the verdict has
//! already indicted our own code. The signal itself can make an interruptible
//! syscall on the target thread return `EINTR`; `SA_RESTART` restarts every one
//! that can be restarted, and the callers that cannot be (`poll`/`epoll_wait`)
//! are exactly the ones GLib and tokio already loop on. It fires only on a
//! thread that has been unresponsive for 25 s, in a test build, on a test whose
//! verdict is already failure.

use std::time::Duration;

/// How far apart the two samples are taken.
///
/// Long enough that a thread burning a core shows a measurable CPU delta at the
/// 10 ms granularity `/proc` reports, short enough to add no meaningful delay to
/// a 504 that has already waited 25 s. It is a *sampling interval*, not a wait
/// for something to happen — nothing is asserted about what occurs inside it
/// (convention 14 governs assertions, and this makes none).
pub(crate) const SAMPLE_GAP: Duration = Duration::from_millis(300);

/// Classify a thread's CPU usage across a sampling window.
///
/// Pure, and separate from the capture, because it is the half that decides
/// which *kind* of fix a stall needs — and a claim that load-bearing must be
/// assertable without a stalled thread to point at.
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

    use super::{FrameFacts, SAMPLE_GAP, cpu_verdict, interrupted_frame_index};
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
    fn thread_state(tid: i32) -> Option<(char, Duration)> {
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
                    match moved {
                        Some(true) => out.push_str(&format!(
                            "\n  (a second sample {:.2}s later sits in a DIFFERENT frame — the \
thread is working through code, not parked)",
                            SAMPLE_GAP.as_secs_f64()
                        )),
                        Some(false) => out.push_str(&format!(
                            "\n  (a second sample {:.2}s later sits in the SAME frame — the thread \
is parked there)",
                            SAMPLE_GAP.as_secs_f64()
                        )),
                        None => {}
                    }
                }
            }
        }
        out
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    /// No capture off Linux: the mechanism is `tgkill` + `/proc` + `dladdr`, and
    /// a host without them must read as *unmeasured* rather than as "no stall
    /// found" — the same rule the heartbeat itself follows.
    pub(crate) fn capture(_tid: i32) -> String {
        " — no UI-thread stack capture on this OS (Linux only), so what holds the thread is \
UNMEASURED"
            .to_string()
    }

    /// Every non-Linux host reports 0, which [`capture`] reads as "unknown".
    pub(crate) fn current_tid() -> i32 {
        0
    }
}

pub(crate) use imp::{capture, current_tid};

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

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, mpsc};

        /// A frame with a name no other code in this workspace has, so finding
        /// it in a captured stack proves the capture walked the *target*
        /// thread's stack and not the handler's or the caller's.
        #[inline(never)]
        fn e2e_stall_fixture_parks_here(rx: &mpsc::Receiver<()>, entered: &mpsc::Sender<i32>) {
            let _ = entered.send(current_tid());
            let _ = rx.recv();
        }

        /// Pull `CPU +X.XXs over the Y.YYs sample` back out of a report.
        fn reported_cpu_and_window(report: &str) -> (f64, f64) {
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

        #[inline(never)]
        fn e2e_stall_fixture_spins_here(stop: &AtomicBool, entered: &mpsc::Sender<i32>) {
            let _ = entered.send(current_tid());
            while !stop.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        }

        #[test]
        fn a_parked_thread_names_the_frame_it_is_parked_in() {
            let (release_tx, release_rx) = mpsc::channel();
            let (entered_tx, entered_rx) = mpsc::channel();
            let handle =
                std::thread::spawn(move || e2e_stall_fixture_parks_here(&release_rx, &entered_tx));
            let tid = entered_rx.recv().expect("fixture thread reports its tid");

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
            assert!(
                report.contains("COMPUTING"),
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
}
