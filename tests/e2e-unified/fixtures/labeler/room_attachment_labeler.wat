;; tier_3 room-labeler fixture that READS ATTACHMENT BYTES: published with
;; `input_schema.needs_attachment_bytes = true`, so a community room's home
;; nest hands it `LabelerInputWithAttachments` — the v1 `LabelerPostInput`
;; bytes followed by the attachment facet (`content-moderation-and-ranking.md`
;; § Tier-3 → *The attachment facet*). On any input containing the four bytes
;; "meow" it emits ONE label in the canonical five — `commercial` at 0.9, what
;; a bubble's and a card's `content-label-badge` paints — and nothing otherwise.
;;
;; The journeys put "meow" ONLY inside an attachment's bytes (a message's
;; attachment plaintext, a room post's PNG in a private ancillary chunk the
;; ingress strip keeps) and never in the words, so a badge is a verdict on the
;; bytes. The scan is `meow_bytes_labeler.wat`'s; the output is
;; `room_commercial_labeler.wat`'s (`commercial` rather than `nsfw`, so the
;; journey reads a badge and never a collapse). wasmi parses WAT text, so these
;; bytes ARE the module and the registry's `wasm_hash` binds to this file.
;;
;; The hit blob at offset 1024 is `[len = 21 LE][21 BARE bytes]`:
;;   01                                  Vec length = 1
;;   0a 63 6f 6d 6d 65 72 63 69 61 6c    String "commercial" (len 10)
;;   cd cc cc cc cc cc ec 3f             f64 0.9, little-endian IEEE-754
;;   01                                  LabelSource::TextAnalysis
;;
;; Memory: 4 pages (256 KiB) — sized for the small test attachments, since the
;; host writes the whole input, facet included, in one `alloc`.
(module
  (memory (export "memory") 4)
  (global $heap_top (mut i32) (i32.const 65536))
  (data (i32.const 1024) "\15\00\00\00\01\0a\63\6f\6d\6d\65\72\63\69\61\6c\cd\cc\cc\cc\cc\cc\ec\3f\01")
  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (local.get $size)))
    (local.get $ptr))
  (func $scan_for_meow (param $ptr i32) (param $len i32) (result i32)
    (local $i i32)
    (local $end i32)
    (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 3)))
    (local.set $i (local.get $ptr))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_s (local.get $i) (local.get $end)))
        (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 0x6d))
          (then
            (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 1))) (i32.const 0x65))
              (then
                (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 2))) (i32.const 0x6f))
                  (then
                    (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 3))) (i32.const 0x77))
                      (then (return (i32.const 1))))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    (i32.const 0))
  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out i32)
    (if (i32.eqz (call $scan_for_meow (local.get $ptr) (local.get $len)))
      (then
        (local.set $out (global.get $heap_top))
        (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 4)))
        (i32.store (local.get $out) (i32.const 0))
        (return (local.get $out))))
    (i32.const 1024))
)
