;; Community-labeler fixture that declares `needs_attachment_bytes` and labels
;; an item BY ITS ATTACHMENT BYTES — the proof that the attachment facet of the
;; `label()` ABI reaches a module (`content-moderation-and-ranking.md` § Tier-3
;; → *The attachment facet*; `conversation-rooms.md` § The three classes →
;; *What the read covers*: "labels see the whole message, attachment bytes
;; included").
;;
;; Published with `input_schema.needs_attachment_bytes = true`, so the host
;; hands it `LabelerInputWithAttachments`: the v1 `LabelerPostInput` bytes
;; followed by the facet, `Vec<LabelerAttachmentInput { mime_type, size_bytes,
;; bytes }>`. The module scans the WHOLE input for the four bytes "meow"
;; (0x6d 0x65 0x6f 0x77) — a marker the tests put inside an attachment's
;; plaintext and never in the message text, so a hit is a hit on the bytes.
;; wasmi parses WAT text, so these bytes ARE the module and `wasm_hash` /
;; `wasm_size` bind to exactly this file.
;;
;; Behaviour:
;;   input contains "meow" → BARE Vec<Label> = [{category:"nsfw", confidence:0.7,
;;                           source:VisionModel}] → category-plane `nsfw` at 700
;;                           per-mille, primary score 700.
;;   input lacks "meow"    → length-0 output → empty Vec<Label> → nothing.
;;
;; The pre-baked hit blob at offset 1024 is `[4-byte LE len = 15][15 BARE bytes]`,
;; pinned against drift by `libs/fauna-labeler/tests/attachment_facet.rs`
;; (`the_meow_bytes_fixture_speaks_the_label_abi`):
;;   01                        Vec length = 1
;;   04 6e 73 66 77            String "nsfw" (len 4 + "nsfw")
;;   66 66 66 66 66 66 e6 3f   f64 0.7, little-endian IEEE-754
;;   02                        LabelSource::VisionModel (serde_bare enum tag 2)
;;
;; Memory: 4 pages (256 KiB). A module that asks for bytes must size its linear
;; memory for the facet it will be handed (the host writes the whole input in
;; one `alloc`, up to `LABELER_ATTACHMENT_FACET_MAX_BYTES`); this fixture is
;; sized for the test attachments, which are small on purpose.
(module
  (memory (export "memory") 4)

  (global $heap_top (mut i32) (i32.const 65536))

  (data (i32.const 1024) "\0f\00\00\00\01\04\6e\73\66\77\66\66\66\66\66\66\e6\3f\02")

  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (local.get $size)))
    (local.get $ptr)
  )

  ;; scan_for_meow(ptr, len) -> found: 1 if "meow" appears in
  ;; memory[ptr..ptr+len], else 0.
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
                      (then (return (i32.const 1)))
                    )
                  )
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

  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out i32)
    (if (i32.eqz (call $scan_for_meow (local.get $ptr) (local.get $len)))
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
