pub mod bridged;
pub mod fauna_mls;
pub mod smtp;

// `debug_assertions` arm: see the gate rationale on `manager.rs`'s injection-seam
// impl block (testing.md convention 15 — visibility is profile-aware so a plain
// debug build reaches the seams; release strips them unless the feature opts in).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub mod mock;
