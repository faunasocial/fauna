//! `fauna-text-model`: the shared trainable **binary text-model** primitive —
//! a pure Bernoulli n-gram Naive-Bayes model over the deterministic positional
//! tokenizer's ordered token stream.
//!
//! The per-user spam model is the first instance of this primitive; the
//! trainable **topic factor** (`docs/goal/behavior/topic-factors.md` § The
//! model) is the second. The crate is **mail-independent and WASM-safe by
//! construction**: serde + serde_json + the two `unicode-*` crates only — no
//! uniffi, no tokio/dns/parser — so it compiles to nest (native), the MDA (Go
//! via UniFFI/cgo through `fauna-mail`'s re-export), and every app
//! (WASM/UniFFI).
//!
//! **At-rest compat surface:** [`classifier::SpamModel`]'s serde_json field
//! names (`version` / `ngrams` / `spam` / `ham` / `spam_messages` /
//! `ham_messages`) are what sealed at-rest model blobs decode by and MUST NOT
//! change. The type keeps its historical
//! `SpamModel` name here and is re-exported by `fauna-mail` at its original
//! paths, so no call site or UniFFI-exported symbol moved. [`topic::TopicModel`]
//! is a sibling type sharing the classifier's `pub(crate)` internals (feature
//! extraction, NB posterior, eviction pass) with its **own, new** at-rest
//! serde_json surface (`topic.rs` module doc), pinned by its own golden test.

pub mod classifier;
pub mod publish;
pub mod tokenizer;
pub mod topic;
