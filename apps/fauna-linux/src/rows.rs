//! Row models the views render: the plain shapes the client layer maps nest
//! replies onto (messages, contacts, knocks, calendars, events, sync files).
//! In-memory only — nothing here is persisted.

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub conversation_id: String,
    pub from_actor: String,
    pub body: String,
    pub timestamp: String,
    pub is_read: bool,
    /// `true` when the message was authenticated via MLS.
    pub verified: bool,
}

#[derive(Debug, Clone)]
pub struct ContactRow {
    pub peer_actor_id: String,
    pub peer_handle: Option<String>,
    /// The peer's handle domain, when known (`Some` for a local peer, `None`
    /// for a federated peer).
    /// Pairs with `peer_handle` for the shared roster-filter predicate.
    pub peer_domain: Option<String>,
    pub status: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KnockRow {
    pub peer_actor_id: String,
    pub summary: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalendarRow {
    pub id: String,
    pub name: String,
    pub visibility: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: String,
    pub calendar_id: String,
    pub summary: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub attendance_mode: Option<String>,
    pub capacity: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SyncFileRow {
    pub id: i64,
    pub folder: String,
    pub path: String,
    pub local_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub state: String,
}
