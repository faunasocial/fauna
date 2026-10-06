# codec-fixtures

The Rust fauna-cbor codec is gated against the [`ipld/codec-fixtures`](https://github.com/ipld/codec-fixtures) corpus, pinned to the commit in `bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT` (today: `a312b720a4f8302c60f075aa3d33149967a4aa45`).

The corpus is **not** vendored or submoduled into this tree. The `scripts/fetch-dagcbor-fixtures.sh` script clones it to `/work/tmp/codec-fixtures` (shared with the Go bridge's gate at `bins/fauna-bridges/internal/dagcbor/fixtures_test.go`). Run:

    just dagcbor-fixtures-rust

This sets `DAGCBOR_FIXTURES_DIR=/work/tmp/codec-fixtures` and invokes `cargo test -p fauna-cbor --test codec_fixtures`. Without the env var, the test self-skips (passes with a "skipped" message).

The justfile target also wires into CI via `just check-generated` (alongside the Go gate).
