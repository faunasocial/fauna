;; tier_3 community-labeler fixture: a "cat detector" that speaks the real
;; `label()` ABI and emits BARE (labeler-registry design §6).
;;
;; Published verbatim as the `wasm_bytes` of a `content_kind="mail"` labeler in
;; `tests/e2e-unified/tests/test_capability_labeler_drain.py`. wasmi's
;; `Module::new` (via `LabelerRuntime::load_module`, used at BOTH the nest publish
;; gate and the MDA holder's `run_wasm_labeler_score`) parses WAT text, so these
;; bytes ARE the module — no separate wat2wasm step, and `wasm_hash`/`wasm_size`
;; bind to exactly this file's bytes.
;;
;; Behaviour (the frame bar's two arms):
;;   input contains "cat"  → BARE Vec<Label> = [{category:"cat", confidence:0.9,
;;                           source:TextAnalysis}] → primary score 900 per-mille.
;;   input lacks "cat"     → length-0 output → empty Vec<Label> → score 0.
;;
;; The pre-baked hit blob is `[4-byte LE len = 14][14 BARE bytes]`. The 14 BARE
;; bytes are `serde_bare::to_vec(&vec![Label{"cat", 0.9, TextAnalysis}])`, pinned
;; against drift by the Rust test `cat_labeler_wat_fixture_scores_cat_and_not`
;; in `libs/fauna-ffi/src/labeler.rs` (which loads THIS file):
;;   01                        Vec length = 1
;;   03 63 61 74               String "cat" (len 3 + "cat")
;;   cd cc cc cc cc cc ec 3f   f64 0.9, little-endian IEEE-754
;;   01                        LabelSource::TextAnalysis (serde_bare enum tag 1)
;;
;; Adapted from `libs/fauna-labeler/tests/wasm_labeler_e2e.rs`'s CAT_LABELER_WAT,
;; whose `0xCAFECAFE` marker output predates the BARE ABI.
(module
  ;; Linear memory: 2 pages (128 KiB). The host writes the BARE-encoded
  ;; LabelerPostInput via `alloc` (page 1+); page 0 holds our pre-baked output.
  (memory (export "memory") 2)

  ;; Bump-allocator top, starting at byte 65536 (page 1) — well clear of the
  ;; page-0 data segment below.
  (global $heap_top (mut i32) (i32.const 65536))

  ;; The cat-hit output blob at offset 1024: [len=14 LE][BARE Vec<Label>].
  (data (i32.const 1024) "\0e\00\00\00\01\03\63\61\74\cd\cc\cc\cc\cc\cc\ec\3f\01")

  ;; alloc(size) -> ptr: bump from heap_top upward.
  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (local.get $size)))
    (local.get $ptr)
  )

  ;; scan_for_cat(ptr, len) -> found: 1 if bytes [0x63,0x61,0x74] ("cat") appear
  ;; in memory[ptr..ptr+len], else 0.
  (func $scan_for_cat (param $ptr i32) (param $len i32) (result i32)
    (local $i i32)
    (local $end i32)
    (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 2)))
    (local.set $i (local.get $ptr))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_s (local.get $i) (local.get $end)))
        (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 0x63))
          (then
            (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 1))) (i32.const 0x61))
              (then
                (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 2))) (i32.const 0x74))
                  (then (return (i32.const 1)))
                )
              )
            )
          )
        )
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
    (i32.const 0)
  )

  ;; label(ptr, len) -> out_ptr: the pre-baked hit blob on a cat hit, else a
  ;; freshly bump-allocated 4-byte length-0 prefix (empty Vec<Label> → score 0).
  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out i32)
    (if (i32.eqz (call $scan_for_cat (local.get $ptr) (local.get $len)))
      (then
        (local.set $out (global.get $heap_top))
        (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 4)))
        (i32.store (local.get $out) (i32.const 0))
        (return (local.get $out))
      )
    )
    (i32.const 1024)
  )
)
