# Conformance test vectors

Each `.bin` file is a canonical DAG-CBOR encoding of one logical
fixture. Used by `tests/conformance.rs` to pin the wire format
byte-for-byte.

| File | Decodes to |
|---|---|
| `envelope_request.bin` | `Frame::Request` (echo example) |
| `envelope_reply_ok.bin` | `Frame::Reply` with `ok=true` |
| `envelope_reply_err.bin` | `Frame::Reply` with `ok=false` |
| `envelope_push.bin` | `Frame::Push` (knock example) |
| `envelope_cancel.bin` | `Frame::Cancel` |
| `push_knock.bin` | `KnockPayload` |
| `push_resync_required.bin` | `ResyncRequiredPayload` |
| `echo_request.bin` | `EchoRequest` |
| `echo_reply.bin` | `EchoReply` |

Re-generate with:

    cargo run -p fauna-protocol --example gen_test_vectors

A change in the bytes of any of these vectors **without a corresponding
schema change** is a forward-compat violation. Enforcement (since 2026-08-19 —
the old claim here that `check-cddl-evolution.py` "runs against PRs to catch
these" was wrong twice over: nothing runs on PRs, and that script never reads
vectors): `tests/conformance.rs` pins Rust types to these bytes (decode →
re-encode → byte equality) and runs in the `protocol-integration-test-check`
heavy gate; schema-file drift is gated separately by `cddl-evolution-check` on
the merge path. What no machine can judge is a *regenerated* vector — reviewing
new bytes against the schema-change intent is a session duty at the commit that
regenerates them.
