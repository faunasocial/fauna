//! The in-repo fixture plugins (`test-helpers`): a core module written in
//! WebAssembly text, componentized against this crate's own WIT at test
//! time. No third-party plugin code exists anywhere in the tree, and no
//! production path ever builds a component.
//!
//! The WIT is embedded with `include_str!`, never read from
//! `env!("CARGO_MANIFEST_DIR")` at run time: an `env!` makes the checkout's
//! absolute path part of every compile of this module, so a compile cache
//! keyed on it (`build-compile-cache.md`) could never share the unit between
//! two checkouts of the repository. `wit_is_the_one_embedded_file` fails when
//! `wit/` grows a file [`build_component`] does not read.
#![cfg(feature = "test-helpers")]

/// The crate's whole `wit/` directory: the one file of the package
/// `host.rs`'s `bindgen!` reads.
const PLUGIN_WIT: &str = include_str!("../wit/plugin.wit");

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
/// answers 200 with the asserted actor as the body or 401 when anonymous,
/// `/h…` answers 200 with `Clear-Site-Data: "*"`, `Alt-Svc: h3=":443"` and
/// `Content-Type: text/plain` (the nest's reply-header filter's probe);
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
    let pkg = resolve.push_str("plugin.wit", PLUGIN_WIT)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wit_is_the_one_embedded_file() {
        let names: Vec<String> = std::fs::read_dir("wit")
            .expect("read wit/")
            .map(|e| {
                e.expect("wit/ entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["plugin.wit"]);
        assert_eq!(
            std::fs::read_to_string("wit/plugin.wit").unwrap(),
            PLUGIN_WIT
        );
    }
}
