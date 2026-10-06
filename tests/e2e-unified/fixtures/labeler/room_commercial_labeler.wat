;; tier_3 room-labeler fixture: speaks the real `label()` ABI and, on any input
;; containing "cat", emits ONE label in the canonical five — `commercial` at
;; 0.9 — which is what a bubble's `content-label-badge` paints; on anything
;; else it emits nothing.
;;
;; Published into the home nest's labeler registry as a `wasm` artifact by
;; `tests/e2e-unified/tests/test_conversation_room_labelers.py`, then named by
;; a community room's owner from the app. `commercial` rather than `spam` on
;; purpose: a spam verdict at 0.9 can collapse the bubble under a member's
;; default threshold, and the journey reads the badge, not the collapse.
;; wasmi parses WAT text, so these bytes ARE the module and the registry's
;; `wasm_hash` binds to exactly this file.
;;
;; The hit blob at offset 1024 is `[len = 21 LE][21 BARE bytes]`:
;;   01                                  Vec length = 1
;;   0a 63 6f 6d 6d 65 72 63 69 61 6c    String "commercial" (len 10)
;;   cd cc cc cc cc cc ec 3f             f64 0.9, little-endian IEEE-754
;;   01                                  LabelSource::TextAnalysis
;; Shape adapted from the nest conformance suite's room labeler fixture
;; (`bins/fauna-nest/tests/conformance_conversation_rooms.rs`, ROOM_LABELER_WAT).
(module
  (memory (export "memory") 2)
  (global $heap_top (mut i32) (i32.const 65536))
  (data (i32.const 1024) "\15\00\00\00\01\0a\63\6f\6d\6d\65\72\63\69\61\6c\cd\cc\cc\cc\cc\cc\ec\3f\01")
  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (local.get $size)))
    (local.get $ptr))
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
                  (then (return (i32.const 1))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    (i32.const 0))
  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out i32)
    (if (i32.eqz (call $scan_for_cat (local.get $ptr) (local.get $len)))
      (then
        (local.set $out (global.get $heap_top))
        (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 4)))
        (i32.store (local.get $out) (i32.const 0))
        (return (local.get $out))))
    (i32.const 1024))
)
