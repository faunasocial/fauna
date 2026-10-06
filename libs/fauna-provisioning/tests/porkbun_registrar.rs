use fauna_provisioning::registrar::{Registrar, porkbun::PorkbunRegistrar};

fn creds() -> Option<(String, String)> {
    let key = std::env::var("PORKBUN_API_KEY").ok()?;
    let secret = std::env::var("PORKBUN_SECRET_KEY").ok()?;
    Some((key, secret))
}

#[tokio::test]
async fn verify_and_check_against_real_porkbun() {
    let (key, secret) = match creds() {
        Some(x) => x,
        None => {
            eprintln!("skipping: PORKBUN_API_KEY/PORKBUN_SECRET_KEY not set");
            return;
        }
    };
    let reg = PorkbunRegistrar::new(key, secret);
    let client = reqwest::Client::new();
    reg.verify(&client).await.expect("verify ok");
    // Use a timestamp-unique domain to guarantee availability on the check path.
    let domain = format!(
        "fauna-plan-test-{}.com",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let avail = reg.check(&client, &domain).await.expect("check ok");
    assert!(avail.available, "newly-minted domain should be available");
}
