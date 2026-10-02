//! This node's identity (docs/P2P-SPEC.md §3): the secret key file, the fingerprint a
//! person reads aloud, and the pair code both screens show.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use iroh::{EndpointId, SecretKey};
use sha2::{Digest, Sha256};

pub const KEY_FILE: &str = "identity.key";
const KEY_LEN: usize = 32;

/// The message stored in `peers.last_error` when federation is refused for want of a key.
pub const KEY_LOST: &str = "identity key missing or unreadable";

pub fn fed_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("fed")
}

/// Load the key, or create it. A key that is gone or corrupt while peers exist is an error:
/// a fresh key would be a different node that every peer has pinned away.
pub fn load_or_create(data_dir: &Path, peers_exist: bool) -> anyhow::Result<SecretKey> {
    let dir = fed_dir(data_dir);
    let path = dir.join(KEY_FILE);
    ensure_private_dir(&dir)?;
    // A link could point the key anywhere; it is neither used nor replaced, peers or not.
    if is_symlink(&path) {
        bail!("{KEY_LOST}: {} is a symlink", path.display());
    }
    match read_key(&path) {
        Ok(Some(key)) => return Ok(key),
        Ok(None) if !peers_exist => {}
        Ok(None) => bail!(
            "{KEY_LOST}: {} is gone but peers are paired",
            path.display()
        ),
        Err(err) if peers_exist => return Err(err.context(KEY_LOST)),
        // No peer depends on a corrupt key, so replacing it loses nothing.
        Err(_) => fs::remove_file(&path).context("remove the corrupt identity key")?,
    }
    create_key(&path)
}

/// Read the key; `None` when there is no file. Refuses a file others can read.
pub fn read_key(path: &Path) -> anyhow::Result<Option<SecretKey>> {
    if is_symlink(path) {
        bail!("{} is a symlink", path.display());
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    require_private(path)?;
    let bytes: [u8; KEY_LEN] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} is not a {KEY_LEN}-byte key", path.display()))?;
    Ok(Some(SecretKey::from_bytes(&bytes)))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn create_key(path: &Path) -> anyhow::Result<SecretKey> {
    let mut seed = [0u8; KEY_LEN];
    getrandom::getrandom(&mut seed).map_err(|e| anyhow::anyhow!("system randomness: {e}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file: File = options
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    restrict_to_owner(path)?;
    file.write_all(&seed)?;
    file.sync_all()?;
    Ok(SecretKey::from_bytes(&seed))
}

pub(super) fn ensure_private_dir(dir: &Path) -> anyhow::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))?;
    restrict_to_owner(dir)
}

/// Refuse a key file other users can read: it was exposed, so repairing the mode would hide
/// that it may already be compromised. The owner decides (and may replace the key).
#[cfg(unix)]
fn require_private(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} is readable by other users (mode {mode:o}); run `chmod 600 {}`, or delete it to start with a new identity and pair again",
            path.display(),
            path.display()
        );
    }
    Ok(())
}

/// Windows has no mode bits to read back; the key's access is set to the user alone.
#[cfg(windows)]
fn require_private(path: &Path) -> anyhow::Result<()> {
    restrict_to_owner(path)
}

/// Make `path` owner-only, or fail: federation must not run on a key others can read.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let want = if path.is_dir() { 0o700 } else { 0o600 };
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode != want {
        fs::set_permissions(path, fs::Permissions::from_mode(want))
            .with_context(|| format!("cannot restrict {} to its owner", path.display()))?;
    }
    Ok(())
}

/// Windows has no mode bits; drop inherited access and grant the current user only.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> anyhow::Result<()> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(F)"))
        .output()
        .context("run icacls")?
        .status;
    if !status.success() {
        bail!(
            "cannot restrict {} to its owner (icacls failed)",
            path.display()
        );
    }
    Ok(())
}

fn digest_hex(parts: &[&[u8]]) -> Vec<u8> {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().to_vec()
}

/// First 16 hex characters of `sha256(node_id)`, as 4 groups of 4: `abcd ef01 2345 6789`.
pub fn fingerprint(node: &EndpointId) -> String {
    let hex: String = digest_hex(&[node.as_bytes()])
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    group(&hex)
}

/// 24 decimal digits from `sha256(min ‖ max)` as 6 groups of 4. Symmetric, so both sides
/// of a pairing show the same code.
pub fn pair_code(a: &EndpointId, b: &EndpointId) -> String {
    let (low, high) = if a.as_bytes() <= b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let digest = digest_hex(&[low.as_bytes(), high.as_bytes()]);
    let head = u128::from_be_bytes(digest[..16].try_into().expect("a digest has 32 bytes"));
    group(&format!("{:024}", head % 10u128.pow(24)))
}

fn group(text: &str) -> String {
    text.as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).expect("ASCII digits and hex"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[cfg(unix)]
    #[test]
    fn a_key_other_users_can_read_is_refused_with_the_fix_not_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(KEY_FILE);
        create_key(&path).unwrap();
        assert!(read_key(&path).unwrap().is_some());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_key(&path).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn fingerprint_is_four_groups_of_four_hex() {
        let print = fingerprint(&node(1));
        let groups: Vec<_> = print.split(' ').collect();
        assert_eq!(groups.len(), 4);
        assert!(groups
            .iter()
            .all(|g| g.len() == 4 && g.bytes().all(|b| b.is_ascii_hexdigit())));
        assert_eq!(print, fingerprint(&node(1)));
        assert_ne!(print, fingerprint(&node(2)));
    }

    #[test]
    fn pair_code_is_symmetric_six_groups_of_four_digits() {
        let (a, b) = (node(1), node(2));
        let code = pair_code(&a, &b);
        assert_eq!(code, pair_code(&b, &a));
        let groups: Vec<_> = code.split(' ').collect();
        assert_eq!(groups.len(), 6);
        assert!(groups
            .iter()
            .all(|g| g.len() == 4 && g.bytes().all(|b| b.is_ascii_digit())));
        assert_ne!(code, pair_code(&a, &node(3)));
    }

    #[test]
    fn creates_once_then_reloads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path(), false).unwrap();
        let again = load_or_create(dir.path(), true).unwrap();
        assert_eq!(first.public(), again.public());
    }

    #[test]
    fn a_lost_key_next_to_peers_is_never_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path(), false).unwrap();
        let path = fed_dir(dir.path()).join(KEY_FILE);
        fs::remove_file(&path).unwrap();
        assert!(load_or_create(dir.path(), true).is_err());
        assert!(!path.exists(), "no key may appear");
        fs::write(&path, b"short").unwrap();
        assert!(load_or_create(dir.path(), true).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"short", "the file is left alone");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_key_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let fed = fed_dir(dir.path());
        ensure_private_dir(&fed).unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, [7u8; KEY_LEN]).unwrap();
        let link = fed.join(KEY_FILE);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(load_or_create(dir.path(), false).is_err());
        assert!(read_key(&link).is_err());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&target).unwrap(), [7u8; KEY_LEN]);
    }

    #[test]
    fn a_corrupt_key_with_no_peers_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path(), false).unwrap();
        fs::write(fed_dir(dir.path()).join(KEY_FILE), b"short").unwrap();
        assert!(load_or_create(dir.path(), false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn dir_is_0700_and_key_0600_a_loose_dir_is_tightened_a_loose_key_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path(), false).unwrap();
        let fed = fed_dir(dir.path());
        let key = fed.join(KEY_FILE);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&fed), mode(&key)), (0o700, 0o600));
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&fed, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_or_create(dir.path(), true).is_err());
        assert_eq!(
            (mode(&fed), mode(&key)),
            (0o700, 0o644),
            "the key is not repaired"
        );
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        load_or_create(dir.path(), true).unwrap();
    }
}
