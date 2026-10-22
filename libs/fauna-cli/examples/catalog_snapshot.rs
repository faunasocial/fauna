//! Print the catalog snapshot — `just cli-catalog` writes it to
//! `tests/e2e-unified/catalog-actual-cli.yaml`.

fn main() {
    print!("{}", fauna_cli::catalog::snapshot_yaml());
}
