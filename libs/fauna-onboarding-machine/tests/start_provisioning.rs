use fauna_onboarding_machine::{OnboardingMachine, observer::CountingObserver};
use fauna_provisioning::progress::OverallStatus;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_provisioning_returns_immediately() {
    let obs = CountingObserver::new();
    let m = OnboardingMachine::new(obs);
    let before = std::time::Instant::now();
    m.clone().start_provisioning();
    let elapsed = before.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "start_provisioning blocked for {:?}; should be a fire-and-forget spawn",
        elapsed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_provisioning_notifies_observer() {
    let obs = CountingObserver::new();
    let m = OnboardingMachine::new(obs.clone());
    m.clone().start_provisioning();
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if obs.count() > 0 {
            return;
        }
    }
    panic!(
        "observer was never notified after 200ms; \
         stash_provisioning_result may be missing observer.on_changed()"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_provisioning_marks_overall_failed_on_missing_provider() {
    let obs = CountingObserver::new();
    let m = OnboardingMachine::new(obs);
    m.clone().start_provisioning();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let snap = m.provisioning_snapshot();
        if snap.overall == OverallStatus::Failed {
            return;
        }
    }
    panic!("snapshot never transitioned to Failed");
}
