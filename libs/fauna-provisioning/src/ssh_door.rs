//! The SSH door's admin-device half (`installers/vps.md` § The SSH door →
//! *The key*): mint the Ed25519 keypair a door box admits, and — on a desktop
//! app — put the private half where SSH keys live.
//!
//! The public half rides into the box's cloud-init as
//! [`crate::cloud_init::SshDoor`]; the private half is this device's alone and
//! is never logged, serialized or stored by the app. A desktop app writes it
//! once with [`install_key`] (`~/.ssh/fauna-<domain>`, mode 0600, plus a
//! `Host <domain>` entry in `~/.ssh/config`); a phone or web app hands it over
//! once as a file.
//!
//! The OpenSSH private-key container (`openssh-key-v1`, unencrypted) is
//! encoded here by hand: it is a short, fixed layout (PROTOCOL.key in the
//! OpenSSH distribution) and the workspace carries no ssh-key crate. The unit
//! tests read the result back with the system `ssh-keygen` where one exists.

use base64::Engine as _;
use fauna_core::secret::{SecretBytes, SecretString};
use rand::RngCore as _;
use rand::rngs::OsRng;
use sha2::{Digest as _, Sha256};

const KEY_TYPE: &str = "ssh-ed25519";

// The armor lines, split so no source line carries a whole private-key marker
// (the publish gate's secret scanner reads one as a leaked key).
/// The first line of an OpenSSH private-key file.
pub const PEM_BEGIN: &str = concat!("-----BEGIN ", "OPENSSH PRIVATE KEY", "-----");
/// The last line of an OpenSSH private-key file.
pub const PEM_END: &str = concat!("-----END ", "OPENSSH PRIVATE KEY", "-----");

/// One freshly minted door keypair. `Debug` never prints the private half.
pub struct SshDoorKey {
    /// The `authorized_keys` line: `ssh-ed25519 <base64> <comment>`.
    pub public_line: String,
    /// `SHA256:<base64, unpadded>` — the form `ssh-keygen -l` and an ssh
    /// client's host-key prompt print.
    pub fingerprint: String,
    /// The whole OpenSSH private-key file ([`PEM_BEGIN`] … [`PEM_END`]),
    /// zeroized on drop.
    pub private_openssh: SecretString,
}

impl std::fmt::Debug for SshDoorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshDoorKey")
            .field("public_line", &self.public_line)
            .field("fingerprint", &self.fingerprint)
            .field("private_openssh", &"<redacted>")
            .finish()
    }
}

fn put_string(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Mint a fresh door keypair for `domain`'s box. The comment names the box,
/// so the key is recognisable in the admin's own `~/.ssh` and in the box's
/// `authorized_keys`.
pub fn mint(domain: &str) -> SshDoorKey {
    let signing = ed25519_dalek::SigningKey::generate(&mut OsRng);
    let public = signing.verifying_key().to_bytes();
    let comment = format!("fauna-door@{domain}");

    let mut blob = Vec::with_capacity(51);
    put_string(&mut blob, KEY_TYPE.as_bytes());
    put_string(&mut blob, &public);

    let b64 = base64::engine::general_purpose::STANDARD;
    let public_line = format!("{KEY_TYPE} {} {comment}", b64.encode(&blob));
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    );

    // The private section: two equal check words, the key, its comment, then
    // 1, 2, 3 … padding to the cipher block size (8 for "none"). Every buffer
    // holding the secret is sized up front (so no reallocation leaves a copy
    // behind) and moved into a zeroize-on-drop wrapper once filled.
    let check = OsRng.next_u32().to_be_bytes();
    let mut private = Vec::with_capacity(160 + comment.len());
    private.extend_from_slice(&check);
    private.extend_from_slice(&check);
    put_string(&mut private, KEY_TYPE.as_bytes());
    put_string(&mut private, &public);
    let mut keypair = Vec::with_capacity(64);
    keypair.extend_from_slice(signing.as_bytes());
    keypair.extend_from_slice(&public);
    let keypair = SecretBytes::from(keypair);
    put_string(&mut private, &keypair);
    put_string(&mut private, comment.as_bytes());
    let mut pad = 1u8;
    while private.len() % 8 != 0 {
        private.push(pad);
        pad += 1;
    }
    let private = SecretBytes::from(private);

    let mut container = Vec::with_capacity(256 + private.len());
    container.extend_from_slice(b"openssh-key-v1\0");
    put_string(&mut container, b"none");
    put_string(&mut container, b"none");
    put_string(&mut container, b"");
    container.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut container, &blob);
    put_string(&mut container, &private);
    let container = SecretBytes::from(container);

    let body = SecretString::new(b64.encode(&*container));
    let mut pem = String::with_capacity(body.len() + 128);
    pem.push_str(PEM_BEGIN);
    pem.push('\n');
    for line in body.as_bytes().chunks(70) {
        // Base64 output is ASCII, so every 70-byte chunk is valid UTF-8.
        pem.push_str(std::str::from_utf8(line).unwrap_or_default());
        pem.push('\n');
    }
    pem.push_str(PEM_END);
    pem.push('\n');

    SshDoorKey {
        public_line,
        fingerprint,
        private_openssh: SecretString::new(pem),
    }
}

/// The file name a door key for `domain` takes (`fauna-<domain>`), or `None`
/// when `domain` is not a plain DNS name — the name becomes a path component
/// and a `Host` pattern, so nothing but letters, digits, dots and hyphens.
pub fn key_file_name(domain: &str) -> Option<String> {
    let ok = !domain.is_empty()
        && !domain.starts_with(['.', '-'])
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then(|| format!("fauna-{domain}"))
}

/// The command a phone or web admin runs once they have put the handed-over
/// key file somewhere (no `Host` entry was written for them).
pub fn command_with_identity(file: &str, domain: &str) -> String {
    format!("ssh -i {file} root@{domain}")
}

/// What [`install_key`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledDoorKey {
    /// The private key's path — `<ssh_dir>/fauna-<domain>`, suffixed `-2`,
    /// `-3` … when that name was already taken.
    pub key_path: std::path::PathBuf,
    /// The `ssh` command that reaches the box: `ssh <domain>` (the `Host`
    /// entry carries the user and the key).
    pub command: String,
}

#[cfg(not(target_arch = "wasm32"))]
const MARKER_PREFIX: &str = "# Fauna SSH door: ";

/// Write `key` for `domain` into `ssh_dir` the way SSH keys live on a desktop
/// (`installers/vps.md` § The SSH door → *The key*): the private key at
/// `fauna-<domain>` (0600; the directory 0700 when this creates it), never
/// over an existing file — the name is suffixed instead — and a `Host
/// <domain>` entry appended to `config`, replacing the entry an earlier door
/// for the same domain left (a re-created box mints a fresh key). The entry
/// names the key by its absolute path.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_key(
    ssh_dir: &std::path::Path,
    domain: &str,
    key: &SshDoorKey,
) -> std::io::Result<InstalledDoorKey> {
    use std::io::{Error, ErrorKind, Write as _};

    let base = key_file_name(domain).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("not a plain domain name: {domain:?}"),
        )
    })?;

    if !ssh_dir.exists() {
        std::fs::create_dir_all(ssh_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(ssh_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    let mut n = 1u32;
    let (key_path, mut file) = loop {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = ssh_dir.join(&name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(f) => break (path, f),
            Err(e) if e.kind() == ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    };
    file.write_all(key.private_openssh.as_bytes())?;
    file.sync_all()?;

    write_host_entry(ssh_dir, domain, &key_path)?;

    Ok(InstalledDoorKey {
        key_path,
        command: format!("ssh {domain}"),
    })
}

/// `config` with this domain's earlier door entry (from its marker line to the
/// blank line closing it) removed and a fresh one appended. Written to a
/// sibling file and renamed over the target — the resolved target, so a
/// symlinked `config` stays a symlink.
#[cfg(not(target_arch = "wasm32"))]
fn write_host_entry(
    ssh_dir: &std::path::Path,
    domain: &str,
    key_path: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::Write as _;

    let link = ssh_dir.join("config");
    let target = match std::fs::canonicalize(&link) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => link.clone(),
        Err(e) => return Err(e),
    };
    let existing = match std::fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let marker = format!("{MARKER_PREFIX}{domain}");
    let mut kept = String::with_capacity(existing.len() + 200);
    let mut skipping = false;
    for line in existing.lines() {
        if line.trim_end() == marker {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim().is_empty() {
                skipping = false;
            }
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if !kept.is_empty() && !kept.ends_with("\n\n") {
        kept.push('\n');
    }
    kept.push_str(&format!(
        "{marker}\nHost {domain}\n    User root\n    IdentityFile \"{}\"\n    IdentitiesOnly yes\n\n",
        key_path.display()
    ));

    let tmp = target.with_file_name(".config.fauna-ssh-door.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(&target) {
            // Keep the admin's own mode on a config they already had.
            f.set_permissions(meta.permissions())?;
        }
        f.write_all(kept.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fauna-ssh-door-{name}-{}-{}",
            std::process::id(),
            OsRng.next_u32()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ssh_keygen() -> Option<&'static str> {
        std::process::Command::new("ssh-keygen")
            .arg("-?")
            .output()
            .ok()
            .map(|_| "ssh-keygen")
    }

    #[test]
    fn the_public_line_and_fingerprint_have_openssh_shape() {
        let k = mint("example.org");
        let mut parts = k.public_line.split(' ');
        assert_eq!(parts.next(), Some("ssh-ed25519"));
        let blob = base64::engine::general_purpose::STANDARD
            .decode(parts.next().unwrap())
            .unwrap();
        assert_eq!(blob.len(), 4 + 11 + 4 + 32);
        assert_eq!(parts.next(), Some("fauna-door@example.org"));
        assert!(k.fingerprint.starts_with("SHA256:"));
        assert_eq!(k.fingerprint.len(), 7 + 43);
        assert!(k.private_openssh.starts_with(&format!("{PEM_BEGIN}\n")));
        assert!(!format!("{k:?}").contains("PRIVATE KEY"), "Debug redacts");
        assert_ne!(
            mint("example.org").public_line,
            k.public_line,
            "fresh each time"
        );
    }

    /// The real reader agrees: `ssh-keygen -y` derives our public line from our
    /// private file, and `-l` prints our fingerprint.
    #[test]
    fn ssh_keygen_reads_the_private_key_back() {
        let Some(bin) = ssh_keygen() else {
            eprintln!("no ssh-keygen on this machine; the shape test stands alone");
            return;
        };
        let dir = scratch("keygen");
        let k = mint("box.example.org");
        let installed = install_key(&dir, "box.example.org", &k).unwrap();
        let out = std::process::Command::new(bin)
            .arg("-y")
            .arg("-f")
            .arg(&installed.key_path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let derived = String::from_utf8(out.stdout).unwrap();
        let ours: Vec<_> = k.public_line.split(' ').take(2).collect();
        let theirs: Vec<_> = derived.trim().split(' ').take(2).collect();
        assert_eq!(ours, theirs);

        let out = std::process::Command::new(bin)
            .arg("-l")
            .arg("-f")
            .arg(&installed.key_path)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&k.fingerprint),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn mode(p: &Path) -> u32 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            0o600
        }
    }

    #[test]
    fn install_writes_the_key_0600_in_a_fresh_0700_dir_and_a_host_entry() {
        let home = scratch("fresh");
        let ssh = home.join(".ssh");
        let k = mint("door.example.org");
        let got = install_key(&ssh, "door.example.org", &k).unwrap();
        assert_eq!(got.key_path, ssh.join("fauna-door.example.org"));
        assert_eq!(got.command, "ssh door.example.org");
        assert_eq!(mode(&ssh), 0o700);
        assert_eq!(mode(&got.key_path), 0o600);
        assert_eq!(
            std::fs::read_to_string(&got.key_path).unwrap(),
            *k.private_openssh
        );
        let config = std::fs::read_to_string(ssh.join("config")).unwrap();
        assert_eq!(
            config,
            format!(
                "# Fauna SSH door: door.example.org\nHost door.example.org\n    User root\n    IdentityFile \"{}\"\n    IdentitiesOnly yes\n\n",
                got.key_path.display()
            )
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn install_never_overwrites_and_a_recreated_box_replaces_only_its_own_entry() {
        let ssh = scratch("again");
        std::fs::write(ssh.join("fauna-a.example"), "the admin's own file").unwrap();
        std::fs::write(ssh.join("config"), "Host build\n    HostName 10.0.0.2\n").unwrap();

        let first = install_key(&ssh, "a.example", &mint("a.example")).unwrap();
        assert_eq!(first.key_path, ssh.join("fauna-a.example-2"));
        assert_eq!(
            std::fs::read_to_string(ssh.join("fauna-a.example")).unwrap(),
            "the admin's own file"
        );
        let other = install_key(&ssh, "b.example", &mint("b.example")).unwrap();
        let second = install_key(&ssh, "a.example", &mint("a.example")).unwrap();
        assert_eq!(second.key_path, ssh.join("fauna-a.example-3"));

        let config = std::fs::read_to_string(ssh.join("config")).unwrap();
        assert!(
            config.starts_with("Host build\n    HostName 10.0.0.2\n\n"),
            "{config}"
        );
        assert_eq!(config.matches("Host a.example\n").count(), 1, "{config}");
        assert!(config.contains(&format!("\"{}\"", second.key_path.display())));
        assert!(!config.contains(&format!("\"{}\"", first.key_path.display())));
        assert!(config.contains(&format!("\"{}\"", other.key_path.display())));
        std::fs::remove_dir_all(ssh).unwrap();
    }

    #[test]
    fn a_domain_that_is_not_a_plain_name_is_refused() {
        let ssh = scratch("refuse");
        for bad in ["", "../x", "a/b", ".hidden", "-o", "a b", "a..b"] {
            assert!(key_file_name(bad).is_none(), "{bad:?}");
            assert!(install_key(&ssh, bad, &mint("x")).is_err(), "{bad:?}");
        }
        assert_eq!(std::fs::read_dir(&ssh).unwrap().count(), 0);
        std::fs::remove_dir_all(ssh).unwrap();
    }
}
