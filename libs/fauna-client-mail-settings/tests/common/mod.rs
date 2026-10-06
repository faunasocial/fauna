//! Shared fixture for the mail-settings integration tests. `rotation_resume.rs`,
//! `rotation_heal.rs`, and `succession_burn.rs` each hand-rolled this exact
//! `build()` (and its `ACTOR`/`SIGNER_SEED` constants) before this lift (round
//! 70 of the shared-Rust lift sweep) — a fresh `MailSettingsMachine` over
//! `FakeNestClient` + an empty `FakeSuccessionLedgerStore` + an empty `FakeMailStore` + a
//! `FakeSigner`, with no MUA-instructions customization. `spam_model_write.rs`
//! needs its own richer variant (extra capabilities + a pre-seeded MSEK) and
//! keeps it local.

use std::sync::Arc;

use fauna_client_mail_settings::testing::{
    FakeMailStore, FakeNestClient, FakeSigner, FakeSuccessionLedgerStore,
};
use fauna_client_mail_settings::{MailSettingsMachine, MuaInstructions};

pub const ACTOR: [u8; 32] = [0x33; 32];
pub const SIGNER_SEED: [u8; 32] = [0x44; 32];

/// The machine, its nest fake, its succession-ledger store (the grant log and the
/// succession's `prior_actor_ids`) and its mail custody.
#[allow(dead_code)] // not every test binary reads every part
pub struct Fixture {
    pub machine: MailSettingsMachine,
    pub nest: FakeNestClient,
    pub config: FakeSuccessionLedgerStore,
    pub mail: FakeMailStore,
}

pub fn fixture() -> Fixture {
    let nest = FakeNestClient::new();
    // The grant log keeps only events the chain signed, so the ledger is the
    // signer's identity.
    let config = FakeSuccessionLedgerStore::empty(
        fauna_core::identity::ActorKeypair::from_secret(SIGNER_SEED).actor_id(),
    );
    let mail = FakeMailStore::empty();
    let machine = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest.clone()),
        Arc::new(config.clone()),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );
    Fixture {
        machine,
        nest,
        config,
        mail,
    }
}

/// The machine, its nest fake and its mail custody — every test that does not
/// read the ledger half.
#[allow(dead_code)]
pub fn build() -> (MailSettingsMachine, FakeNestClient, FakeMailStore) {
    let f = fixture();
    (f.machine, f.nest, f.mail)
}
