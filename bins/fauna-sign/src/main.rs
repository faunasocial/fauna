//! CLI tool: sign a file with Ed25519, output detached .sig file.
//!
//! Usage:
//!   echo "$KEY_HEX" | fauna-sign --key-stdin --input binary --output binary.sig
//!   fauna-sign --key-file key.hex --input binary --output binary.sig

use anyhow::{Context, Result};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use std::io::Read;

#[derive(Parser)]
#[command(name = "fauna-sign", about = "Sign release artifacts with Ed25519")]
struct Args {
    /// Read hex-encoded private key from stdin
    #[arg(long)]
    key_stdin: bool,
    /// Read hex-encoded private key from file
    #[arg(long)]
    key_file: Option<String>,
    /// Input file to sign
    #[arg(long)]
    input: String,
    /// Output signature file
    #[arg(long)]
    output: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let key_hex = if args.key_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim().to_string()
    } else if let Some(path) = &args.key_file {
        std::fs::read_to_string(path)
            .with_context(|| format!("reading key file {path}"))?
            .trim()
            .to_string()
    } else {
        anyhow::bail!("specify --key-stdin or --key-file");
    };

    // Deliberately not `fauna_core::hex32::decode`: this is a minimal standalone
    // release-signing tool (4 deps), and the value is an Ed25519 signing key — not a
    // Fauna identifier on any wire/at-rest surface — so pulling in the foundational
    // `fauna-core` crate for one decode is unjustified coupling.
    let key_bytes: [u8; 32] = hex::decode(&key_hex)
        .context("key must be valid hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("key must be 32 bytes (64 hex chars)"))?;
    let signing_key = SigningKey::from_bytes(&key_bytes);

    let input_bytes =
        std::fs::read(&args.input).with_context(|| format!("reading {}", args.input))?;

    let signature = signing_key.sign(&input_bytes);
    std::fs::write(&args.output, signature.to_bytes())
        .with_context(|| format!("writing {}", args.output))?;

    eprintln!("signed {} -> {}", args.input, args.output);
    Ok(())
}
