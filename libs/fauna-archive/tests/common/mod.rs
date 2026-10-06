//! The fixture zips, lifted into the library as `fauna_archive::testing` so
//! downstream crates (the import machine, `fauna-ffi`, the tui walk) build
//! their corpora from the one generator. This file stays only as the
//! `mod common;` shim the integration tests already name.

#![allow(dead_code)]

pub use fauna_archive::testing::*;
