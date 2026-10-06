//! fauna.social front door binary. All policy lives in the library
//! (`fauna_front_door`); this is only process bring-up.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    fauna_front_door::run(fauna_front_door::DoorConfig::production()).await
}
