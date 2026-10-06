/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

pub mod gen_cs;

use anyhow::Result;
use camino::Utf8PathBuf;
use clap::Parser;
use fs_err::File;
pub use gen_cs::generate_bindings;
use serde::{Deserialize, Serialize};
use std::io::Write;
use uniffi_bindgen::{Component, GenerationSettings};

#[derive(Parser)]
#[command(name = "uniffi-bindgen")]
#[command(version)]
#[command(propagate_version = true)]
struct Cli {
    /// Directory in which to write generated files. Default is same folder as .udl file.
    #[arg(long, short)]
    out_dir: Option<Utf8PathBuf>,

    /// Path to the optional uniffi config file. If not provided, uniffi-bindgen will try to guess it from the UDL's file location.
    #[arg(long, short)]
    config: Option<Utf8PathBuf>,

    /// Extract proc-macro metadata from cdylib for this crate.
    #[arg(long)]
    lib_file: Option<Utf8PathBuf>,

    /// Pass in a cdylib path rather than a UDL file
    #[arg(long = "library", conflicts_with = "lib_file", requires = "out_dir")]
    library_mode: bool,

    /// When `--library` is passed, only generate bindings for one crate
    #[arg(long = "crate", requires = "library_mode")]
    crate_name: Option<String>,

    /// Path to the UDL file, or cdylib if `library-mode` is specified
    source: Utf8PathBuf,

    /// Do not try to format the generated bindings.
    #[arg(long, short)]
    no_format: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigRoot {
    #[serde(default)]
    bindings: ConfigBindings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigBindings {
    #[serde(default)]
    csharp: gen_cs::Config,
}

struct BindingGenerator {
    try_format_code: bool,
}

impl uniffi_bindgen::BindingGenerator for BindingGenerator {
    type Config = gen_cs::Config;

    fn new_config(&self, root_toml: &toml::Value) -> Result<Self::Config> {
        Ok(
            match root_toml.get("bindings").and_then(|b| b.get("csharp")) {
                Some(v) => v.clone().try_into()?,
                None => Default::default(),
            },
        )
    }

    fn write_bindings(
        &self,
        settings: &GenerationSettings,
        components: &[Component<Self::Config>],
    ) -> anyhow::Result<()> {
        // Emit the shared FFI runtime once, in the parent `uniffi` namespace.
        // Holds BigEndianStream / RustBuffer struct / UniffiRustCallStatus /
        // ForeignBytes / the exception hierarchy — all crate-agnostic and
        // referenced by every per-crate file via `using uniffi;`. Without
        // this, cross-namespace `FfiConverter` calls fail to type-check
        // (each file would inline its own incompatible copy).
        let common_file = settings.out_dir.join("_UniffiCommon.cs");
        println!("Writing bindings file {common_file}");
        let mut f = File::create(&common_file)?;
        let common = gen_cs::formatting::add_header(gen_cs::COMMON_RUNTIME_CS.to_string());
        write!(f, "{common}")?;
        if self.try_format_code {
            let _ = gen_cs::formatting::format(&common_file)
                .map_err(|e| println!(
                    "Warning: Unable to auto-format {} using CSharpier (hint: 'dotnet tool install -g csharpier'): {e:?}",
                    common_file.file_name().unwrap(),
                ));
        }

        for Component { ci, config, .. } in components {
            let bindings_file = settings.out_dir.join(format!("{}.cs", ci.namespace()));
            println!("Writing bindings file {bindings_file}");
            let mut f = File::create(&bindings_file)?;

            let mut bindings = generate_bindings(config, ci)?;
            bindings = gen_cs::formatting::add_header(bindings);
            write!(f, "{bindings}")?;

            if self.try_format_code {
                let _ = gen_cs::formatting::format(&bindings_file)
                    .map_err(|e| println!(
                        "Warning: Unable to auto-format {} using CSharpier (hint: 'dotnet tool install -g csharpier'): {e:?}",
                        bindings_file.file_name().unwrap(),
                    ));
            }
        }
        Ok(())
    }

    fn update_component_configs(
        &self,
        settings: &GenerationSettings,
        components: &mut Vec<Component<Self::Config>>,
    ) -> Result<()> {
        for c in &mut *components {
            c.config
                .namespace
                .get_or_insert_with(|| format!("uniffi.{}", c.ci.namespace()));

            c.config.cdylib_name.get_or_insert_with(|| {
                settings
                    .cdylib
                    .clone()
                    .unwrap_or_else(|| format!("uniffi_{}", c.ci.namespace()))
            });
        }

        // Populate `external_packages` so each generated file emits
        // `using <ns>;` for sibling components. Without this, types
        // defined in another crate (e.g. fauna-provisioning's TldPriceQuote
        // referenced from fauna-onboarding-machine) render as bare names
        // and fail to compile under C#.
        let crate_to_namespace: std::collections::HashMap<String, String> = components
            .iter()
            .map(|c| {
                (
                    c.ci.crate_name().to_string(),
                    c.config
                        .namespace
                        .clone()
                        .expect("namespace was just set above"),
                )
            })
            .collect();
        for c in &mut *components {
            let local_crate = c.ci.crate_name().to_string();
            for (ext_crate, ext_namespace) in &crate_to_namespace {
                if ext_crate != &local_crate
                    && !c.config.external_packages.contains_key(ext_crate)
                {
                    c.config
                        .external_packages
                        .insert(ext_crate.clone(), ext_namespace.clone());
                }
            }
        }
        Ok(())
    }
}

pub fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.library_mode {
        let out_dir = cli
            .out_dir
            .expect("--out-dir is required when using --library");

        let config_supplier = {
            use uniffi_bindgen::cargo_metadata::CrateConfigSupplier;
            let cmd = ::cargo_metadata::MetadataCommand::new();
            let metadata = cmd.exec().unwrap();
            CrateConfigSupplier::from(metadata)
        };

        uniffi_bindgen::library_mode::generate_bindings(
            &cli.source,
            cli.crate_name,
            &BindingGenerator {
                try_format_code: !cli.no_format,
            },
            &config_supplier,
            cli.config.as_deref(),
            &out_dir,
            !cli.no_format,
        )
        .map(|_| ())
    } else {
        uniffi_bindgen::generate_external_bindings(
            &BindingGenerator {
                try_format_code: !cli.no_format,
            },
            &cli.source,
            cli.config.as_deref(),
            cli.out_dir.as_deref(),
            cli.lib_file.as_deref(),
            cli.crate_name.as_deref(),
            !cli.no_format,
        )
    }
}
