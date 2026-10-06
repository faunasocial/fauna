//! Per-step renderable snapshots + the aggregate. Clients read fresh copies
//! on every observer tick and render off them; they never see the internal
//! `State`. Mirrors `fauna_onboarding_machine::snapshots`.

pub mod aggregate;
pub mod device_places;
pub mod name;
pub mod review;

pub use aggregate::*;
pub use device_places::*;
pub use name::*;
pub use review::*;
