//! The ordinary [`FolderWizardMachine`] driven headlessly — the one create
//! path every preset takes (`ui/folders.md` § Where logic lives, "No wizard
//! logic per client"; `ui/media.md` § Errors & edge cases → *No folder to
//! upload into*, ruling 3: "never a second create path"). The photo-library
//! preset ([`crate::photo_library`]) and Media's first folder
//! ([`crate::first_folder`]) differ only in what they hand it.

use std::sync::Arc;

use fauna_protocol::folders::PlaceFlags;

use crate::machine::FolderWizardMachine;
use crate::nest_api::{FolderApiError, FolderNestApi, FolderRow};
use crate::observer::FolderWizardObserver;
use crate::state::{DeviceOption, FolderWizardStep, RetentionPolicy};

/// A preset's create: the name, the retention it stamps (`None` for a plain
/// create) and the devices it enrolls, each at `flags`.
pub(crate) struct HeadlessCreate {
    pub name: String,
    pub retention: Option<RetentionPolicy>,
    pub devices: Vec<DeviceOption>,
    pub flags: PlaceFlags,
}

/// Drive the wizard through Name → Devices → Review → submit, then re-list to
/// learn the created row (the create reply carries no id). A failed submit
/// surfaces the wizard's own error; a created set the list then does not
/// carry is a phantom and refuses the same way.
pub(crate) async fn create_headless(
    api: Arc<dyn FolderNestApi>,
    observer: Arc<dyn FolderWizardObserver>,
    create: HeadlessCreate,
) -> Result<FolderRow, FolderApiError> {
    let HeadlessCreate {
        name,
        retention,
        devices,
        flags,
    } = create;
    let device_count = devices.len();
    let wizard = FolderWizardMachine::new(observer, devices, Arc::clone(&api));
    wizard.set_name(name.clone());
    wizard.set_retention(retention);
    wizard.next(); // Name -> Devices
    for i in 0..device_count {
        let i = i as u32;
        wizard.toggle_device_member(i);
        wizard.set_device_flags(i, flags.originates, flags.accepts, flags.applies_deletes);
    }
    wizard.next(); // Devices -> Review

    // `submit()` is non-throwing: failure lands on the review snapshot.
    if wizard.submit().await != FolderWizardStep::Done {
        let review = wizard.review_snapshot();
        return Err(FolderApiError::Transient {
            detail: review
                .error
                .map(|e| e.log_line())
                .unwrap_or_else(|| format!("could not create the '{name}' folder")),
        });
    }

    find_named(&api, &name)
        .await?
        .ok_or_else(|| FolderApiError::Transient {
            detail: format!("the '{name}' folder was created but the nest does not list it"),
        })
}

/// The caller's own row named `name`, if the owner-scoped list carries one.
pub(crate) async fn find_named(
    api: &Arc<dyn FolderNestApi>,
    name: &str,
) -> Result<Option<FolderRow>, FolderApiError> {
    Ok(api
        .list_folder_rows()
        .await?
        .into_iter()
        .find(|row| row.name == name))
}

/// The observer a headless drive hands the wizard: nothing renders it.
pub(crate) struct HeadlessObserver;

impl FolderWizardObserver for HeadlessObserver {
    fn on_changed(&self) {}
}
