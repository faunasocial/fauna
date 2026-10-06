//! The in-repo fixture plugins (`test-helpers`): a core module written in
//! WebAssembly text, componentized against this crate's own WIT at test
//! time. No third-party plugin code exists anywhere in the tree, and no
//! production path ever builds a component.
#![cfg(feature = "test-helpers")]

use std::path::Path;

/// The hello plugin's core module — what `tests/e2e-unified/fixtures/plugins/
/// hello_plugin.wat` holds. Its behaviour, which every embedder's test asserts
/// against: on `start` it stores its public holder key under state key `pub`,
/// calls `fauna.capabilities.fetch` (in the ceiling) and `fauna.feed.posts`
/// (outside it) for its first bound account — the install row when none is
/// bound — and stores each result's discriminant (0 ok, 1 err) under `fetch`
/// and `refused`; asks for `https://undeclared.example/` and stores the
/// discriminant under `http`; stores the clock under `now` (8 bytes LE); logs
/// one `info` line. On ingress: `/spin…` loops forever, `/grow…` asks for
/// 128 MiB and answers 507 when refused (200 when admitted), `/whoami…`
/// answers 200 with the asserted actor as the body or 401 when anonymous;
/// anything else is 404.
pub const HELLO_PLUGIN_WAT: &str =
    include_str!("../../../tests/e2e-unified/fixtures/plugins/hello_plugin.wat");

/// The log line the hello plugin emits on `start`.
pub const HELLO_PLUGIN_LOG_LINE: &str = "hello from the fixture plugin";

/// Componentize `core_wat` (a core module in text) against the `plugin`
/// world: parse, embed the world's metadata, encode, validate.
pub fn build_component(core_wat: &str) -> anyhow::Result<Vec<u8>> {
    let mut core = wat::parse_str(core_wat)?;
    let mut resolve = wit_parser::Resolve::default();
    let wit_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    let (pkg, _) = resolve.push_dir(&wit_dir)?;
    let world = resolve.select_world(&[pkg], Some("plugin"))?;
    wit_component::embed_component_metadata(
        &mut core,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let component = wit_component::ComponentEncoder::default()
        .module(&core)?
        .validate(true)
        .encode()?;
    Ok(component)
}

/// The hello plugin as a component.
pub fn hello_plugin() -> anyhow::Result<Vec<u8>> {
    build_component(HELLO_PLUGIN_WAT)
}
