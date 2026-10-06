//! `MaybeSendSync` — a target-conditional auto-trait alias for async seams.
//!
//! `Send + Sync` on every target except `wasm32`, where it is empty.
//!
//! A shared state machine or backend stores its async transport seam behind an
//! `Arc<dyn …Seam>`. Natively the seam wraps a `Send + Sync` transport (e.g.
//! `Arc<NestClient>`), so the trait carries a `Send + Sync` supertrait and the
//! owner stays `Send` for `tokio::spawn`. On wasm the seam wraps a
//! single-threaded, `Rc`-based transport (`!Send`), so the supertrait must drop
//! to nothing or the `!Send` seam can't satisfy `impl …Seam`. Using this alias
//! as the seam's supertrait gives one trait body across both targets: the
//! transitive `MaybeSendSync: Send + Sync` keeps `dyn …Seam` (and thus the
//! owner) `Send` natively, while wasm relaxes it. Pairs with the
//! `#[cfg_attr(not(target_arch = "wasm32"), async_trait)]` /
//! `#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]` arm on the same
//! trait (the native arm boxes `Send` futures; the wasm arm `!Send` ones).
//!
//! Lives in `fauna-core` so both the protocol-coupled seams (re-exported as
//! `fauna_protocol::MaybeSendSync`) and the dependency-light ones
//! (`fauna-conversations`, which deliberately avoids a `fauna-protocol`
//! dependency) consume one canonical definition.

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}
