//! The peer-served change-row provenance ruling — re-exported from its one
//! home, [`fauna_protocol::peer_row`], where it moved so the same-account leg
//! (no `p2p-share` feature) serves and judges rows through the same code
//! (`docs/goal/behavior/p2p-shared-set-build.md` § Build design — the
//! same-account row leg, decision 3).

pub use fauna_protocol::peer_row::{
    LocalShareChange, PeerRowAdmission, RowRefusal, judge_peer_row, reader_over_cached_roster,
    screen_peer_row, serves_held_row,
};
