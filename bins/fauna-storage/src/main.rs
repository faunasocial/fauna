//! Planned S3-compatible storage daemon for nest backups. Not yet
//! implemented: this binary is a reserved workspace member so the backup
//! design can land incrementally; running it prints a notice and exits.

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "fauna-storage")]
struct Cli {
    /// Bind address
    #[arg(long, default_value = "127.0.0.1:3900")]
    bind: String,

    /// Directory to expose as S3-compatible storage
    #[arg(long)]
    data_dir: std::path::PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _cli = Cli::parse();
    eprintln!("fauna-storage is a placeholder — not yet implemented");
    Ok(())
}
