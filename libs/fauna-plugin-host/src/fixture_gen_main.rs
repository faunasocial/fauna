//! E2e test helper: write the in-repo hello plugin, componentized against
//! this crate's WIT, to the path given as the one argument — what a tier_3
//! app journey serves as the module a plugin document names
//! (`tests/e2e-unified/tests/test_plugin_install.py`). Built only with
//! `test-helpers`; no production path builds a component.

fn main() {
    let Some(out) = std::env::args_os().nth(1) else {
        eprintln!("usage: plugin-fixture-gen <out.wasm>");
        std::process::exit(2);
    };
    let component = fauna_plugin_host::fixture::hello_plugin().unwrap_or_else(|e| {
        eprintln!("plugin-fixture-gen: componentize the hello plugin: {e:#}");
        std::process::exit(1);
    });
    if let Err(e) = std::fs::write(&out, component) {
        eprintln!("plugin-fixture-gen: write {}: {e}", out.to_string_lossy());
        std::process::exit(1);
    }
}
