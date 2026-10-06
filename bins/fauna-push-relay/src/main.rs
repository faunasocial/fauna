pub mod api;
pub mod db;
pub mod push_sender;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:3478".to_string());
    let db_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "push-relay.db".to_string());

    let db = db::RelayDb::open(std::path::PathBuf::from(&db_path))?;

    let push_sender: std::sync::Arc<dyn push_sender::PushSender> =
        if std::env::var("APNS_KEY_PATH").is_ok() {
            let key_path = std::env::var("APNS_KEY_PATH").unwrap();
            let key_id = std::env::var("APNS_KEY_ID").expect("APNS_KEY_ID required");
            let team_id = std::env::var("APNS_TEAM_ID").expect("APNS_TEAM_ID required");
            let topic = std::env::var("APNS_TOPIC").expect("APNS_TOPIC required");
            let production = std::env::var("APNS_PRODUCTION").unwrap_or_default() == "true";
            std::sync::Arc::new(
                push_sender::ApnsPushSender::new(key_id, team_id, &key_path, topic, production)
                    .expect("APNs configuration failed"),
            )
        } else {
            tracing::warn!("no push credentials configured — using log-only sender");
            std::sync::Arc::new(push_sender::LogPushSender)
        };

    let state = api::RelayState::new(db, push_sender);

    // Spawn cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if let Ok(n) = cleanup_state.cleanup_expired()
                && n > 0
            {
                tracing::info!("cleaned up {n} expired wakes");
            }
        }
    });

    let app = api::relay_router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("Push relay listening on {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
