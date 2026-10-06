//! Shared test harness for fauna-client integration tests.
//!
//! Both halves now live in library code and are re-exported here so existing
//! `common::mpsc_pair()` / `common::MockClientChannel` call sites keep working:
//!
//! * the in-memory transport (`mpsc_pair` / `MpscAdapter` / `ServerSide`) is
//!   `fauna_ws_substrate::testing`, shared with the substrate's own supervisor
//!   tests and the federation channel tests (Spec Y2 slice 4);
//! * [`MockClientChannel`] — the client's [`SupervisedChannel`] with a *mocked*
//!   connect but the *real* push bridge and bearer refresh — is
//!   `fauna_client::testing`, behind the `test-util` feature. It moved out of
//!   this file so integration tests in *other* crates can drive a real
//!   `NestClient` through its connect lifecycle too; `fauna-client-mail-settings`'s
//!   `hydrate_waits_for_socket` is the first such consumer.

#![allow(dead_code)]
// `common` is compiled into every integration-test binary; the re-exported
// helpers below are used by some but not all of them.
#![allow(unused_imports)]

pub use fauna_client::testing::{
    ConnectQueue, MockClientChannel, MpscAdapter, ServerSide, connect_queue, mpsc_pair,
    mpsc_pair_with_capacity,
};
