//! Embeds the agent's property list in the macOS binary.
//!
//! The agent is a bare Mach-O its own LaunchAgent starts, so launchd makes it
//! its own responsible process and macOS reads the agent's Info.plist, not the
//! enclosing app's, when it raises the Local Network prompt for the peer leg
//! the agent binds while it hosts the account store. A bare binary's Info.plist
//! is its `__TEXT,__info_plist` section, written here at link time from the
//! tracked `macos/Info.plist` (docs/goal/architecture/account-sync-plane.md
//! § Implementation status today). The section is linked before either
//! signing step (installer/macos/build.sh, installer/macos/sign-app-bundle.sh),
//! so the signature binds it, and its `CFBundleIdentifier` becomes the
//! binary's default signing identifier.

use std::path::PathBuf;

fn main() {
    let plist = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"))
        .join("macos")
        .join("Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
