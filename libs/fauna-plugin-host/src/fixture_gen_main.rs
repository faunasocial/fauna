//! E2e test helper: write an in-repo fixture plugin, componentized against
//! this crate's WIT, to the path given as the first argument — what a tier_3
//! app journey serves as the module a plugin document names
//! (`tests/e2e-unified/tests/test_plugin_install.py`). The hello plugin by
//! default; `--refusing` as a second argument writes the variant whose
//! `start` refuses (`fauna_plugin_host::fixture::refusing_plugin`). Built
//! only with `test-helpers`; no production path builds a component.

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(out) = args.next() else {
        eprintln!("usage: plugin-fixture-gen <out.wasm> [--refusing]");
        std::process::exit(2);
    };
    let built = match args.next() {
        None => fauna_plugin_host::fixture::hello_plugin(),
        Some(flag) if flag == "--refusing" => fauna_plugin_host::fixture::refusing_plugin(),
        Some(other) => {
            eprintln!(
                "plugin-fixture-gen: unknown argument {}",
                other.to_string_lossy()
            );
            std::process::exit(2);
        }
    };
    let component = built.unwrap_or_else(|e| {
        eprintln!("plugin-fixture-gen: componentize the fixture plugin: {e:#}");
        std::process::exit(1);
    });
    if let Err(e) = std::fs::write(&out, component) {
        eprintln!("plugin-fixture-gen: write {}: {e}", out.to_string_lossy());
        std::process::exit(1);
    }
}
