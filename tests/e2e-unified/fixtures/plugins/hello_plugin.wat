;; hello_plugin — the in-repo fixture plugin for the nest-hosted WASM runner
;; (`libs/fauna-plugin-host`, `docs/goal/architecture/third-party.md`
;; § Execution forms → *WASM components*). A CORE module: the test harness
;; componentizes it against `libs/fauna-plugin-host/wit/plugin.wit`
;; (`fauna_plugin_host::fixture::build_component`), so the import and export
;; signatures below are the canonical-ABI lowering of that WIT.
;;
;; Behaviour (what every embedder's test asserts against — the doc comment on
;; `fauna_plugin_host::fixture::HELLO_PLUGIN_WAT` is the summary):
;;   start   → state["pub"]     = holder.public-key()
;;             state["fetch"]   = [disc] of nest-api.call(account, "fauna.capabilities.fetch", {})
;;             state["refused"] = [disc] of nest-api.call(account, "fauna.feed.posts", {})
;;             state["http"]    = [disc] of http.fetch(GET https://undeclared.example/)
;;             state["now"]     = clock.now-millis() as 8 bytes LE
;;             log.log(info, "hello from the fixture plugin"); returns ok
;;             (account = the first of nest-api.bindings(), or none)
;;   handle  → "/s…" spins until the fuel budget ends it
;;             "/g…" asks for 128 MiB: 507 when the cap refuses it, 200 when admitted
;;             "/w…" 200 with the asserted actor as the body, 401 when anonymous
;;             else 404
;;
;; Memory map: data in page 0 (strings from 1024), scratch return areas at
;; 2048–2560, the bump heap from page 1.
(module
  (import "fauna:plugin/nest-api@0.1.0" "call"
    (func $call (param i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "fauna:plugin/nest-api@0.1.0" "bindings"
    (func $bindings (param i32)))
  (import "fauna:plugin/state@0.1.0" "get"
    (func $state_get (param i32 i32 i32)))
  (import "fauna:plugin/state@0.1.0" "put"
    (func $state_put (param i32 i32 i32 i32)))
  (import "fauna:plugin/state@0.1.0" "delete"
    (func $state_delete (param i32 i32)))
  (import "fauna:plugin/holder@0.1.0" "public-key"
    (func $public_key (param i32)))
  (import "fauna:plugin/holder@0.1.0" "open-grant"
    (func $open_grant (param i32 i32 i32 i32 i32)))
  (import "fauna:plugin/http@0.1.0" "fetch"
    (func $fetch (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "fauna:plugin/clock@0.1.0" "now-millis"
    (func $now_millis (result i64)))
  (import "fauna:plugin/log@0.1.0" "log"
    (func $log (param i32 i32 i32)))

  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 65536))

  (data (i32.const 1024) "fauna.capabilities.fetch")      ;; 24
  (data (i32.const 1056) "fauna.feed.posts")              ;; 16
  (data (i32.const 1088) "\a0")                            ;; 1: canonical CBOR {}
  (data (i32.const 1104) "https://undeclared.example/")   ;; 27
  (data (i32.const 1152) "GET")                            ;; 3
  (data (i32.const 1160) "pub")                            ;; 3
  (data (i32.const 1168) "fetch")                          ;; 5
  (data (i32.const 1176) "refused")                        ;; 7
  (data (i32.const 1184) "http")                           ;; 4
  (data (i32.const 1192) "now")                            ;; 3
  (data (i32.const 1200) "hello from the fixture plugin") ;; 29

  ;; A bump allocator — the host lowers lists and strings into our memory
  ;; through it. Never frees; grows the memory as needed.
  (func $cabi_realloc (export "cabi_realloc")
        (param $old i32) (param $old_size i32) (param $align i32) (param $new_size i32)
        (result i32)
    (local $ptr i32)
    (local $need i32)
    (local.set $ptr
      (i32.and
        (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get $align))))
    (global.set $heap (i32.add (local.get $ptr) (local.get $new_size)))
    (local.set $need
      (i32.sub (global.get $heap) (i32.mul (memory.size) (i32.const 65536))))
    (if (i32.gt_s (local.get $need) (i32.const 0))
      (then
        (drop (memory.grow
          (i32.div_u (i32.add (local.get $need) (i32.const 65535)) (i32.const 65536))))))
    (if (local.get $old_size)
      (then (memory.copy (local.get $ptr) (local.get $old) (local.get $old_size))))
    (local.get $ptr))

  (func (export "fauna:plugin/lifecycle@0.1.0#start") (result i32)
    (local $acc_disc i32) (local $acc_ptr i32) (local $acc_len i32) (local $list i32)
    ;; holder.public-key → (ptr,len) at 2048
    (call $public_key (i32.const 2048))
    (call $state_put (i32.const 1160) (i32.const 3)
                     (i32.load (i32.const 2048)) (i32.load (i32.const 2052)))
    ;; nest-api.bindings → (ptr,len) at 2064; take the first account if any
    (call $bindings (i32.const 2064))
    (if (i32.load (i32.const 2068))
      (then
        (local.set $list (i32.load (i32.const 2064)))
        (local.set $acc_disc (i32.const 1))
        (local.set $acc_ptr (i32.load (local.get $list)))
        (local.set $acc_len (i32.load offset=4 (local.get $list)))))
    ;; nest-api.call(account, "fauna.capabilities.fetch", {}) → result at 2080
    (call $call (local.get $acc_disc) (local.get $acc_ptr) (local.get $acc_len)
                (i32.const 1024) (i32.const 24) (i32.const 1088) (i32.const 1)
                (i32.const 2080))
    (call $state_put (i32.const 1168) (i32.const 5) (i32.const 2080) (i32.const 1))
    ;; nest-api.call(account, "fauna.feed.posts", {}) → result at 2112
    (call $call (local.get $acc_disc) (local.get $acc_ptr) (local.get $acc_len)
                (i32.const 1056) (i32.const 16) (i32.const 1088) (i32.const 1)
                (i32.const 2112))
    (call $state_put (i32.const 1176) (i32.const 7) (i32.const 2112) (i32.const 1))
    ;; http.fetch(GET https://undeclared.example/, no headers, no body) → 2144
    (call $fetch (i32.const 1152) (i32.const 3) (i32.const 1104) (i32.const 27)
                 (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                 (i32.const 2144))
    (call $state_put (i32.const 1184) (i32.const 4) (i32.const 2144) (i32.const 1))
    ;; clock.now-millis → 8 bytes LE at 2176
    (i64.store (i32.const 2176) (call $now_millis))
    (call $state_put (i32.const 1192) (i32.const 3) (i32.const 2176) (i32.const 8))
    ;; log.log(info, …)
    (call $log (i32.const 1) (i32.const 1200) (i32.const 29))
    ;; result<_, string>: discriminant 0 = ok, at 2208
    (i32.store8 (i32.const 2208) (i32.const 0))
    (i32.const 2208))

  (func (export "fauna:plugin/lifecycle@0.1.0#stop"))

  (func (export "fauna:plugin/ingress@0.1.0#handle")
        (param $m_ptr i32) (param $m_len i32)
        (param $p_ptr i32) (param $p_len i32)
        (param $q_disc i32) (param $q_ptr i32) (param $q_len i32)
        (param $h_ptr i32) (param $h_len i32)
        (param $b_ptr i32) (param $b_len i32)
        (param $a_disc i32) (param $a_ptr i32) (param $a_len i32)
        (result i32)
    (local $status i32)
    (local $second i32)
    (local.set $status (i32.const 404))
    ;; response record at 2304: status u16 @0, headers (ptr,len) @4, body (ptr,len) @12
    (i32.store (i32.const 2308) (i32.const 0))
    (i32.store (i32.const 2312) (i32.const 0))
    (i32.store (i32.const 2316) (i32.const 0))
    (i32.store (i32.const 2320) (i32.const 0))
    (if (i32.ge_u (local.get $p_len) (i32.const 2))
      (then
        (local.set $second (i32.load8_u offset=1 (local.get $p_ptr)))
        ;; "/s…": spin until the fuel budget ends it
        (if (i32.eq (local.get $second) (i32.const 115))
          (then (loop $spin (br $spin))))
        ;; "/g…": 2048 pages over the cap
        (if (i32.eq (local.get $second) (i32.const 103))
          (then
            (if (i32.eq (memory.grow (i32.const 2048)) (i32.const -1))
              (then (local.set $status (i32.const 507)))
              (else (local.set $status (i32.const 200))))))
        ;; "/w…": who am I
        (if (i32.eq (local.get $second) (i32.const 119))
          (then
            (if (local.get $a_disc)
              (then
                (local.set $status (i32.const 200))
                (i32.store (i32.const 2316) (local.get $a_ptr))
                (i32.store (i32.const 2320) (local.get $a_len)))
              (else (local.set $status (i32.const 401))))))))
    (i32.store16 (i32.const 2304) (local.get $status))
    (i32.const 2304)))
