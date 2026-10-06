//! Linux glue for the shared `fauna_client_search::SearchManager` snapshot —
//! the Search page's process-wide manager (`host`) and its GTK observer
//! bridge (`observer`), the direct analogue of `crate::feed`
//! (`docs/goal/ui/search.md` § State & data shape, ratified 2026-08-02).
//!
//! The Search *view* lives in `crate::views::search`; this module owns the
//! manager instance + the notify→main-thread bridge it renders from.

pub mod host;
pub mod observer;
