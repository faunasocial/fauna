//! End-to-end WASM labeler test using a real WAT module.
//!
//! The WAT module implements a simple "cat detector" labeler:
//! - Scans input bytes for the substring "cat" (bytes 0x63, 0x61, 0x74).
//! - If found: returns 4-byte LE length (4) + 4 marker bytes (0xCA, 0xFE, 0xCA, 0xFE).
//! - If not found: returns 4-byte LE length = 0.

use fauna_labeler::LabelerRuntime;

/// WAT source for the cat-detector labeler module.
///
/// Memory layout:
///   Page 0 (offset 0..65535):     used by the host for input (written via alloc).
///   Page 1+ (offset 65536+):      bump allocator region for alloc.
///
/// Exports:
///   memory                         — linear memory (2 pages minimum)
///   alloc(size: i32) -> ptr: i32   — bump allocator from page 1 upward
///   label(ptr: i32, len: i32) -> out_ptr: i32
const CAT_LABELER_WAT: &str = r#"
(module
  ;; Linear memory: 2 pages (128 KiB).
  (memory (export "memory") 2)

  ;; Bump allocator state: current top of the heap, starting at byte 65536 (page 1).
  (global $heap_top (mut i32) (i32.const 65536))

  ;; alloc(size: i32) -> ptr: i32
  ;; Returns the current heap top, then advances it by `size`.
  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    ;; ptr = heap_top
    (local.set $ptr (global.get $heap_top))
    ;; heap_top += size
    (global.set $heap_top
      (i32.add (global.get $heap_top) (local.get $size))
    )
    (local.get $ptr)
  )

  ;; scan_for_cat(ptr: i32, len: i32) -> found: i32
  ;; Returns 1 if bytes [0x63, 0x61, 0x74] appear in memory[ptr..ptr+len], else 0.
  (func $scan_for_cat (param $ptr i32) (param $len i32) (result i32)
    (local $i i32)
    (local $end i32)
    ;; end = ptr + len - 2  (need at least 3 bytes)
    (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 2)))
    (local.set $i (local.get $ptr))
    (block $break
      (loop $loop
        ;; if i >= end, break
        (br_if $break (i32.ge_s (local.get $i) (local.get $end)))
        ;; check memory[i] == 0x63 ('c')
        (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 0x63))
          (then
            ;; check memory[i+1] == 0x61 ('a')
            (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 1))) (i32.const 0x61))
              (then
                ;; check memory[i+2] == 0x74 ('t')
                (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 2))) (i32.const 0x74))
                  (then
                    (return (i32.const 1))
                  )
                )
              )
            )
          )
        )
        ;; i++
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
    (i32.const 0)
  )

  ;; label(ptr: i32, len: i32) -> out_ptr: i32
  ;; Calls scan_for_cat; writes output at a freshly allocated buffer.
  ;; Output format: 4-byte LE length prefix + payload bytes.
  ;;   found:     length=4, payload=[0xCA, 0xFE, 0xCA, 0xFE]
  ;;   not found: length=0, no payload
  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out_ptr i32)
    (local $found i32)
    (local.set $found (call $scan_for_cat (local.get $ptr) (local.get $len)))
    (if (i32.eqz (local.get $found))
      (then
        ;; Allocate 4 bytes for the zero-length prefix only.
        (local.set $out_ptr (global.get $heap_top))
        (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 4)))
        ;; Write length = 0 (4 bytes LE)
        (i32.store (local.get $out_ptr) (i32.const 0))
        (return (local.get $out_ptr))
      )
    )
    ;; Allocate 8 bytes: 4-byte length prefix + 4 marker bytes.
    (local.set $out_ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 8)))
    ;; Write length = 4 (LE)
    (i32.store (local.get $out_ptr) (i32.const 4))
    ;; Write marker bytes: 0xCA, 0xFE, 0xCA, 0xFE
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 4)) (i32.const 0xCA))
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 5)) (i32.const 0xFE))
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 6)) (i32.const 0xCA))
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 7)) (i32.const 0xFE))
    (local.get $out_ptr)
  )
)
"#;

fn make_runtime() -> LabelerRuntime {
    LabelerRuntime::new(16 * 1024 * 1024, 100_000).expect("LabelerRuntime::new should succeed")
}

#[test]
fn wasm_labeler_detects_cat_content() {
    let rt = make_runtime();
    let module = rt
        .load_module(CAT_LABELER_WAT.as_bytes())
        .expect("WAT module should compile");

    let input = b"I love my cat";
    let output = rt
        .execute(&module, input)
        .expect("execute should succeed for cat input");

    assert!(
        !output.is_empty(),
        "output should be non-empty when 'cat' is present"
    );
    assert_eq!(
        output,
        &[0xCA, 0xFE, 0xCA, 0xFE],
        "marker bytes should be [0xCA, 0xFE, 0xCA, 0xFE]"
    );
}

#[test]
fn wasm_labeler_ignores_non_cat_content() {
    let rt = make_runtime();
    let module = rt
        .load_module(CAT_LABELER_WAT.as_bytes())
        .expect("WAT module should compile");

    let input = b"The weather is nice";
    let output = rt
        .execute(&module, input)
        .expect("execute should succeed for non-cat input");

    assert!(
        output.is_empty(),
        "output should be empty when 'cat' is absent"
    );
}

#[test]
fn wasm_labeler_handles_empty_input() {
    let rt = make_runtime();
    let module = rt
        .load_module(CAT_LABELER_WAT.as_bytes())
        .expect("WAT module should compile");

    let output = rt
        .execute(&module, b"")
        .expect("execute should succeed for empty input");

    assert!(output.is_empty(), "output should be empty for empty input");
}

#[test]
fn wasm_labeler_respects_fuel_limits() {
    // cpu_limit_us = 0 → fuel = 0 (saturating_mul), which is insufficient for any WASM execution.
    let rt = LabelerRuntime::new(16 * 1024 * 1024, 0).expect("LabelerRuntime::new should succeed");
    let module = rt
        .load_module(CAT_LABELER_WAT.as_bytes())
        .expect("WAT module should compile");

    let result = rt.execute(&module, b"I love my cat");
    assert!(
        result.is_err(),
        "execution should fail when fuel limit is exceeded"
    );
}

/// A module that tries to grow linear memory far past the runtime's cap, then
/// touches the (would-be) grown region.
const MEMORY_BOMB_WAT: &str = r#"
(module
  (memory (export "memory") 1)                       ;; 64 KiB initial
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "label") (param i32 i32) (result i32)
    ;; Attempt to grow by 1000 pages (~64 MiB) — far past the 1 MiB cap.
    (drop (memory.grow (i32.const 1000)))
    ;; Store at ~2 MiB: only in bounds if the grow succeeded. If the limiter
    ;; denied it, memory is still 64 KiB and this store traps (OOB).
    (i32.store (i32.const 2000000) (i32.const 42))
    (i32.const 0))
)
"#;

/// `memory.grow` past the configured cap must be denied (the
/// limiter bounds growth, not just the initial size). Without the `StoreLimits`
/// limiter, wasmi would let the module balloon to ~64 MiB and the store would
/// succeed; with it, the grow is denied, memory stays 64 KiB, and the
/// out-of-bounds store traps — so `execute` errors instead of OOMing the holder.
#[test]
fn wasm_labeler_memory_growth_is_bounded() {
    // 1 MiB memory cap; generous CPU so the failure is memory-bound, not fuel.
    let rt = LabelerRuntime::new(1024 * 1024, 100_000).expect("LabelerRuntime::new should succeed");
    let module = rt
        .load_module(MEMORY_BOMB_WAT.as_bytes())
        .expect("WAT module should compile");

    let result = rt.execute(&module, b"x");
    assert!(
        result.is_err(),
        "a module growing memory past the cap must fail, not OOM the holder"
    );
}

/// A module that returns a bogus 4 GiB output-length prefix from tiny memory.
const HUGE_OUTPUT_LEN_WAT: &str = r#"
(module
  (memory (export "memory") 1)                       ;; 64 KiB
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "label") (param i32 i32) (result i32)
    ;; Write length prefix = 0xFFFFFFFF (~4 GiB) at offset 100, return that ptr.
    (i32.store (i32.const 100) (i32.const 0xFFFFFFFF))
    (i32.const 100))
)
"#;

/// an untrusted output-length prefix must be bounds-checked
/// against the module's actual memory BEFORE the host allocates. Without the
/// clamp, `vec![0u8; 0xFFFFFFFF]` would attempt a ~4 GiB allocation (OOM) even
/// though the read would fail; with it, `execute` returns a clean error and this
/// test completes instantly.
#[test]
fn wasm_labeler_rejects_oversized_output_length() {
    let rt = LabelerRuntime::new(1024 * 1024, 100_000).expect("LabelerRuntime::new should succeed");
    let module = rt
        .load_module(HUGE_OUTPUT_LEN_WAT.as_bytes())
        .expect("WAT module should compile");

    let result = rt.execute(&module, b"");
    let err = result.expect_err("an out-of-memory output length must be rejected");
    assert!(
        err.to_string().contains("exceeds WASM memory"),
        "error should identify the oversized output length, got: {err}"
    );
}
