//! Test-only counting global allocators for the memory-bound tests.
//!
//! Each test binary installs at most one as its `#[global_allocator]`; the
//! counters are process-wide, so a binary's tests must run one at a time.
//! Dev-dependency only — never part of a release graph.
//!
//! - [`PeakLive`]: high-water mark of *live* bytes (alloc minus dealloc), for
//!   "this stream never holds more than X" bounds.
//! - [`LargestAlloc`]: the largest *single* allocation made while tracking,
//!   for "no whole-segment buffer" bounds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static LIVE_PEAK: AtomicUsize = AtomicUsize::new(0);

/// Install with `#[global_allocator] static A: PeakLive = PeakLive;`.
pub struct PeakLive;

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    LIVE_PEAK.fetch_max(now, Relaxed);
}

impl PeakLive {
    /// Run `f`; the most bytes live at once while it ran, above what was live
    /// before it.
    pub fn peak_during(f: impl FnOnce()) -> usize {
        let base = Self::begin();
        f();
        Self::end(base)
    }

    /// Start a window for an async body: pair with [`PeakLive::end`].
    pub fn begin() -> usize {
        let base = LIVE.load(Relaxed);
        LIVE_PEAK.store(base, Relaxed);
        base
    }

    /// Close a window opened by [`PeakLive::begin`] (pass its return value).
    pub fn end(base: usize) -> usize {
        LIVE_PEAK.load(Relaxed) - base
    }
}

// SAFETY: every call forwards to `System` unchanged; the counters only observe
// sizes.
unsafe impl GlobalAlloc for PeakLive {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        moved
    }
}

static TRACKING: AtomicBool = AtomicBool::new(false);
static LARGEST: AtomicUsize = AtomicUsize::new(0);

/// Install with `#[global_allocator] static A: LargestAlloc = LargestAlloc;`.
pub struct LargestAlloc;

fn note(size: usize) {
    if TRACKING.load(Relaxed) {
        LARGEST.fetch_max(size, Relaxed);
    }
}

impl LargestAlloc {
    /// Reset the high-water mark and start recording.
    pub fn start() {
        LARGEST.store(0, Relaxed);
        TRACKING.store(true, Relaxed);
    }

    /// Stop recording; the largest single allocation seen since
    /// [`start`](Self::start).
    pub fn stop() -> usize {
        TRACKING.store(false, Relaxed);
        LARGEST.load(Relaxed)
    }
}

// SAFETY: every call forwards to `System` unchanged; the counter only reads
// the requested size.
unsafe impl GlobalAlloc for LargestAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
