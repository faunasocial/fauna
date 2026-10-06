# fauna-peer-channel fuzz — the PQ-2 hardening surface

Three cargo-fuzz targets over the peer channel's three attacker-facing decode
links (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer → Open
questions PQ-2; § Wormability posture rule 2 names the memory-safe stack
necessary but not sufficient — this is the parser-hardening sufficiency half):

| Target | Surface |
|---|---|
| `framing_decode` | L2 `[u32 BE len][CBOR]` framing — `PeerStreamAdapter`'s `LengthDelimitedCodec` decode (max-frame bound asserted) |
| `frame_decode` | the dispatcher's wire decode — `fauna_protocol::envelope::decode_frame` + the `Request`/`Reply`/`Push`/`Cancel` shapes |
| `kind_payload_decode` | every peer-leg kind's payload struct (the `fauna-peer-sync` allowlist + `fauna.peer.exchange`) |

Each target is a thin wrapper over `fauna_peer_channel::hardening` — the SAME
check bodies the merge-gate bounded smoke runs, so a crash found here is
replayable there and vice versa.

## Two halves, run differently

- **The bounded smoke (automated, always on):**
  `cargo test -p fauna-peer-channel --test fuzz_smoke` — corpus replay + a
  fixed deterministic mutation/random sweep, seconds. Runs in the async
  merge-gate check (`just peer-channel-hardening-check`). This does **not**
  build this fuzz crate.
- **Coverage-guided long runs (manual, scheduled):**
  `cargo fuzz run <target>` from this directory. ⚠ Requires `cargo-fuzz` and
  fetches `libfuzzer-sys` from crates.io — **outside the vetted workspace
  lockfile**, so the supply-chain policy applies: that install needs explicit
  maintainer approval of tool and channel first. As of 2026-08-12 neither is
  installed on any dev machine; the crate is scaffolding-complete (mirroring
  `libs/fauna-mls/fuzz`) awaiting that approval. The merge gate never depends
  on it.

## Corpus

`corpus/<target>/` is checked in, captured from a REAL two-`PeerChannel`
exchange over all the allowlisted kinds — regenerate with:

```
cargo test -p fauna-peer-channel --test fuzz_smoke -- --ignored regenerate_corpus
```

Any crash a long run finds lands as a fixed bug + a file under
`corpus/<target>/` — the smoke then replays it forever as a regression.
