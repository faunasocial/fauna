//! Media's first folder — the plain headless create behind
//! `PlainFolderMaker` (`docs/goal/ui/media.md` § Errors & edge cases → *No
//! folder to upload into*, rulings 3 and 4).

use std::sync::Arc;

use fauna_core::folder_create::PlainFolderMaker;
use fauna_folders_machine::first_folder::{FIRST_FOLDER_NAME, HeadlessFolderMaker};
use fauna_folders_machine::nest_api::{FakeCall, FolderRow};
use fauna_folders_machine::{FakeFolderNestApi, FolderApiError};

fn maker(api: &Arc<FakeFolderNestApi>) -> HeadlessFolderMaker {
    HeadlessFolderMaker::new(Arc::clone(api) as _)
}

/// Ruling 3: the plain create's shape — the name, audience private (no
/// audience on the wire: the create default), no retention, and no device
/// enrolled (no `places.set` at all).
#[tokio::test]
async fn the_first_folder_is_a_plain_private_create_with_nothing_enrolled() {
    let api = Arc::new(FakeFolderNestApi::new());
    maker(&api)
        .make_plain_folder(FIRST_FOLDER_NAME)
        .await
        .expect("the create lands");
    let calls = api.calls();
    assert_eq!(calls.len(), 1, "one create, no place set: {calls:?}");
    let FakeCall::CreateFolder { req } = &calls[0] else {
        panic!("expected a create, got {calls:?}");
    };
    assert_eq!(req.name, "Personal");
    assert_eq!(req.retention_policy, None);
    assert_eq!(req.audience, None, "private is the create default");
}

/// Ruling 4: a create refused as a duplicate means another device made it —
/// the list carries it, so the maker answers that the folder exists.
#[tokio::test]
async fn a_duplicate_name_made_by_another_device_is_the_folder() {
    let api = Arc::new(FakeFolderNestApi::new());
    api.set_create_response(Err(FolderApiError::Conflict {
        detail: "name taken".into(),
    }));
    // The other device's row (the fake appends only on a successful create).
    api.set_folder_rows(vec![FolderRow {
        id: 9,
        name: FIRST_FOLDER_NAME.into(),
    }]);
    maker(&api)
        .make_plain_folder(FIRST_FOLDER_NAME)
        .await
        .expect("the race resolves to the one folder");
}

/// Ruling 6: a failed create the list does not carry answers its reason.
#[tokio::test]
async fn a_failed_create_answers_its_reason() {
    let api = Arc::new(FakeFolderNestApi::new());
    api.set_create_response(Err(FolderApiError::Transient {
        detail: "nest unreachable".into(),
    }));
    let err = maker(&api)
        .make_plain_folder(FIRST_FOLDER_NAME)
        .await
        .expect_err("nothing was made");
    assert!(
        err.contains("nest unreachable"),
        "the create's reason: {err}"
    );
}
