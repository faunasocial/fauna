//! The create-time audience — a folder born public (`ui/folders.md`
//! § Audience and website serving → *Born public*): the Name step's choice,
//! Create arming the declassify confirm, the confirm creating with
//! `audience: public`, cancel disarming, and a seam that cannot attest
//! offering nothing.

use std::sync::Arc;

use fauna_folders_machine::nest_api::{FakeCall, FakeFolderNestApi};
use fauna_folders_machine::observer::NullObserver;
use fauna_folders_machine::{
    DeviceOption, FolderWizardMachine, FolderWizardObserver, FolderWizardStep, SubmitPhase,
};

fn wizard(fake: &Arc<FakeFolderNestApi>) -> Arc<FolderWizardMachine> {
    let obs: Arc<dyn FolderWizardObserver> = Arc::new(NullObserver);
    FolderWizardMachine::new(
        obs,
        vec![DeviceOption {
            device_id: "aa".repeat(32),
            label: "laptop".into(),
        }],
        Arc::clone(fake) as _,
    )
}

fn attesting() -> Arc<FakeFolderNestApi> {
    let fake = Arc::new(FakeFolderNestApi::new());
    fake.set_can_attest(true);
    fake
}

/// Name → Devices → Review with the given audience chosen on the Name step.
fn to_review(m: &FolderWizardMachine, audience: &str) {
    m.set_name("site".into());
    m.set_audience(audience.into());
    m.next();
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Review);
}

fn creates(fake: &FakeFolderNestApi) -> Vec<Option<String>> {
    fake.calls()
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::CreateFolder { req } => Some(req.audience),
            FakeCall::SetPlace { .. } => None,
        })
        .collect()
}

#[test]
fn the_name_step_offers_the_choice_defaulting_private() {
    let fake = attesting();
    let m = wizard(&fake);
    let name = m.name_snapshot();
    assert!(name.audience_offered);
    assert_eq!(name.audience, "private");
    m.set_audience("public".into());
    assert_eq!(m.name_snapshot().audience, "public");
    // Anything else normalizes fail-closed — never public.
    m.set_audience("shared".into());
    assert_eq!(m.name_snapshot().audience, "private");
    m.set_audience("LOUD".into());
    assert_eq!(m.name_snapshot().audience, "private");
}

#[tokio::test]
async fn a_public_wizards_create_arms_and_sends_nothing() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "public");
    assert_eq!(m.review_snapshot().audience, "public");
    assert!(!m.review_snapshot().declassify_armed);

    assert_eq!(m.submit().await, FolderWizardStep::Review);
    let review = m.review_snapshot();
    assert!(
        review.declassify_armed,
        "Create arms the declassify confirm"
    );
    assert_eq!(review.phase, SubmitPhase::Idle);
    assert!(fake.calls().is_empty(), "arming sends nothing");

    // Create again while armed is not the confirm either.
    assert_eq!(m.submit().await, FolderWizardStep::Review);
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn the_confirm_creates_the_folder_public() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "public");
    m.submit().await;
    assert_eq!(m.confirm_public_create().await, FolderWizardStep::Done);
    assert_eq!(creates(&fake), vec![Some("public".to_string())]);
    assert!(!m.review_snapshot().declassify_armed);
}

#[tokio::test]
async fn the_confirm_is_inert_unless_armed() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "public");
    assert_eq!(m.confirm_public_create().await, FolderWizardStep::Review);
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn cancel_disarms_and_creates_nothing() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "public");
    m.submit().await;
    m.cancel_public_create();
    assert!(!m.review_snapshot().declassify_armed);
    assert_eq!(m.step(), FolderWizardStep::Review);
    assert!(fake.calls().is_empty());
    // A cancelled confirm can be armed again.
    m.submit().await;
    assert!(m.review_snapshot().declassify_armed);
}

/// Back while the confirm is armed is its cancel, never a step: the dialog
/// closes on Review, nothing is sent, and stepping back further never
/// carries a stale arm onto a later Review. An app whose dialog has no
/// cancel control of its own (tui paints it inline) cancels through Back.
#[tokio::test]
async fn back_while_armed_cancels_and_stays_on_review() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "public");
    m.submit().await;
    assert!(m.review_snapshot().declassify_armed);
    m.back();
    assert_eq!(m.step(), FolderWizardStep::Review);
    assert!(!m.review_snapshot().declassify_armed);
    assert!(fake.calls().is_empty());
    // Disarmed, Back is an ordinary step again.
    m.back();
    assert_eq!(m.step(), FolderWizardStep::Devices);
}

#[tokio::test]
async fn a_private_wizards_create_is_unchanged() {
    let fake = attesting();
    let m = wizard(&fake);
    to_review(&m, "private");
    assert_eq!(m.submit().await, FolderWizardStep::Done);
    assert_eq!(creates(&fake), vec![None]);
}

#[tokio::test]
async fn a_seam_that_cannot_attest_offers_nothing_and_creates_private() {
    let fake = Arc::new(FakeFolderNestApi::new());
    let m = wizard(&fake);
    assert!(!m.name_snapshot().audience_offered);
    to_review(&m, "public");
    assert_eq!(
        m.name_snapshot().audience,
        "private",
        "the choice is ignored"
    );
    assert_eq!(m.submit().await, FolderWizardStep::Done);
    assert_eq!(creates(&fake), vec![None]);
}

/// The attestation's failure is a partial failure: the folder exists, so the
/// wizard stays on Review marked created, with the error, and a retry never
/// re-sends the create.
#[tokio::test]
async fn a_failed_attestation_leaves_the_folder_created() {
    let fake = attesting();
    fake.set_unattested(Some("socket closed".into()));
    let m = wizard(&fake);
    to_review(&m, "public");
    m.submit().await;
    assert_eq!(m.confirm_public_create().await, FolderWizardStep::Review);
    let review = m.review_snapshot();
    assert!(review.created);
    assert_eq!(review.phase, SubmitPhase::Failed);
    assert_eq!(
        review.error.as_ref().map(|e| e.key.as_str()),
        Some("devices.wizard.create_attest_error")
    );
    fake.set_unattested(None);
    assert_eq!(m.submit().await, FolderWizardStep::Done);
    assert_eq!(creates(&fake).len(), 1, "the create is never re-sent");
}
