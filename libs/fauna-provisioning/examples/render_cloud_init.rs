//! TEST-ONLY helper: print the cloud-init user-data `build_cloud_init` renders,
//! so a tier_4 witness can run the host-side artifacts it ships (the
//! `fauna-reboot-coordinator` script, the unattended-upgrades stanzas) exactly
//! as a provisioned box receives them, rather than a copy lifted from source.
//!
//! The host-OS maintenance files are the same on every box (they do not vary
//! with any `CloudInitParams` field), so fixed fixture params are enough; the
//! witness parses the `write_files` entries out of stdout.
//!
//! An `examples/` binary on purpose — it adds no workspace member (so no Docker
//! crate-list drift) and is never built into a shipped artifact.
//!
//! Authority: `docs/goal/architecture/installers/vps.md` § Host OS Maintenance.

use fauna_provisioning::cloud_init::{CloudInitParams, build_cloud_init};

fn main() {
    let params = CloudInitParams {
        domain: "witness.example".into(),
        image_tag: "witness".into(),
        claim_code: "witness-claim-code".into(),
        enable_mail: false,
        deployment_seed: None,
    };
    print!("{}", build_cloud_init(&params));
}
