/// Step 4a foundation: pure `db` ↔ `fauna-client-caldav` adapter for the
/// events.md Decision B store migration (encrypted `bridge_caldav_*` over the
/// `fauna.bridges.*` calendar RPCs). Unit-tested in isolation; the `client.rs`
/// calendar/event methods are wired onto it in Step 4b
/// (tracked internally).
pub mod caldav_backend;
pub mod calendar_sidebar;
pub mod calendar_view;
pub mod day_cell_press;
pub mod day_grid;
/// The events rail of draft-persistence v2 — this app's trigger glue over the
/// shared `fauna_client_drafts::DraftsSync` (`reserved-folders.md` § Drafts Sync).
pub mod drafts;
pub mod event_detail;
pub mod event_form;
pub mod event_list;
pub mod event_popover;
pub mod mini_month;
pub mod month_grid;
pub mod time_utils;
pub mod week_grid;

use std::rc::Rc;

use crate::client::FaunaClient;

/// Handles returned from `build_events_view` for dynamic updates.
#[allow(dead_code)]
pub struct EventViewHandles {
    pub calendar: calendar_view::CalendarViewHandles,
}

/// Build the full events view: calendar layout with header controls,
/// left sidebar, and main stack for month/week/day/agenda views.
///
/// Returns `(outer_box, handles)`.
pub fn build_events_view(fauna_client: Rc<FaunaClient>) -> (gtk::Box, EventViewHandles) {
    let (view, calendar_handles) = calendar_view::build_calendar_view(&fauna_client);

    let handles = EventViewHandles {
        calendar: calendar_handles,
    };

    (view, handles)
}
