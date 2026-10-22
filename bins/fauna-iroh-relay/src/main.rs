//! `fauna-iroh-relay` — the nest's self-hosted P2P relay sidecar. See the
//! crate-level docs in `lib.rs`. The relay server lives behind the non-default
//! `relay` feature; without it this binary is an empty shell (no iroh-relay tree
//! compiled), which is what keeps the default workspace build and the shipping
//! nest Docker image iroh-free.

#[cfg(feature = "relay")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let result = fauna_iroh_relay::run().await;
    // A refused credential is a routine end, not a crash: exit `EX_CONFIG` so
    // `docker logs` reads honestly and s6 restarts us against nest's fresh token
    // (`transport.md`, the sidecar credential's lifetime).
    if let Err(e) = &result
        && e.downcast_ref::<fauna_sidecar_client::CredentialRefused>()
            .is_some()
    {
        eprintln!("{e}");
        std::process::exit(fauna_sidecar_client::EXIT_CREDENTIAL_REFUSED);
    }
    result
}

#[cfg(not(feature = "relay"))]
fn main() {
    eprintln!(
        "fauna-iroh-relay was built without the `relay` feature — this is the \
         default empty shell and has nothing to serve. The relay sidecar is \
         built with `--features relay` only by an artifact that ships it; it is \
         never started by hand."
    );
    std::process::exit(2);
}
