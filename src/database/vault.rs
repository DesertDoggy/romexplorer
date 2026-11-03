//! Secrets at rest: provider keys in `api.env` and each peer's token in `remote_sources`.
//!
//! One random key in a file (`data/master.key`, see [`AppPaths::master_key_path`]) seals
//! values as `enc:v1:<hex>` with XChaCha20-Poly1305. A key *file*, not a passphrase or an OS
//! keyring prompt, because the server restarts unattended -- after a power cut, an update, or
//! to run a schedule -- and must be able to read its own credentials with nobody there.
//!
//! What that protects: a copy of `api.env`, a database backup, or a database dump seen without
//! the key file. What it does not: someone who can read the data directory, who has the key
//! too. The key is never in a backup, because backups hold tables and it is a file.
//!
//! Each value is sealed with a *context* (`"api.env:igdb_key"`, `"remote_sources.token"`) as
//! associated data, so a sealed value cannot be copied into a different field and still open.
//! A value without the `enc:v1:` prefix is plaintext and passes through, so a file written
//! before this existed keeps working until `encrypt-credentials` is run.

use std::fmt;
use std::io::Write;
use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use super::error::DatabaseError;

pub const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 24;

#[derive(Clone)]
pub struct Vault {
    cipher: XChaCha20Poly1305,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault(..)")
    }
}

impl Vault {
    pub fn from_key(key: [u8; 32]) -> Self {
        Self { cipher: XChaCha20Poly1305::new(&key.into()) }
    }

    /// Reads the key file, creating it with a fresh random key if it does not exist.
    ///
    /// The key is written whole to a private temporary file and only then linked in under the
    /// real name, which fails if the name exists. So a second process starting at the same
    /// moment (the worker, the GUI) reads the file the first one finished rather than writing
    /// a different key, and a crash part way never leaves an empty `master.key` behind.
    pub fn load_or_create(path: &Path) -> Result<Self, DatabaseError> {
        for _ in 0..50 {
            match std::fs::read_to_string(path) {
                // An empty file is a key that was never written: an older build crashed while
                // creating it, or one is being written by `create_private` right now. Nothing
                // was sealed with it, so after a moment it is safe to replace.
                Ok(text) if text.trim().is_empty() => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                Ok(text) => return Self::from_hex(text.trim()).map_err(|e| DatabaseError::Config(format!("{}: {e}", path.display()))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::create(path),
                Err(e) => return Err(DatabaseError::Config(format!("cannot read {}: {e}", path.display()))),
            }
        }
        log::warn!("{} is empty; replacing it with a new key", path.display());
        let _ = std::fs::remove_file(path);
        Self::create(path)
    }

    fn create(path: &Path) -> Result<Self, DatabaseError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DatabaseError::Config(format!("cannot create {}: {e}", parent.display())))?;
        }
        let mut key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut key);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let temp = path.with_file_name(format!(".{name}.{}.{:x}.tmp", std::process::id(), rand::RngCore::next_u64(&mut rand::rng())));
        let written = (|| {
            let mut file = create_private(&temp)?;
            file.write_all(format!("{}\n", to_hex(&key)).as_bytes())?;
            file.sync_all()
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(DatabaseError::Config(format!("cannot write {}: {e}", temp.display())));
        }
        let linked = std::fs::hard_link(&temp, path);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(Self::from_key(key)),
            // Someone else won the race: use theirs.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load_or_create(path),
            // A filesystem without hard links: write the name directly, as before.
            Err(_) => match create_private(path) {
                Ok(mut file) => {
                    file.write_all(format!("{}\n", to_hex(&key)).as_bytes()).map_err(|e| DatabaseError::Config(format!("cannot write {}: {e}", path.display())))?;
                    file.sync_all().ok();
                    Ok(Self::from_key(key))
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load_or_create(path),
                Err(e) => Err(DatabaseError::Config(format!("cannot create {}: {e}", path.display()))),
            },
        }
    }

    fn from_hex(text: &str) -> Result<Self, String> {
        let bytes = from_hex(text).ok_or("not a hex key")?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| "the key must be 32 bytes (64 hex characters)")?;
        Ok(Self::from_key(key))
    }

    pub fn is_sealed(value: &str) -> bool {
        value.starts_with(PREFIX)
    }

    /// `enc:v1:` + hex(nonce ‖ ciphertext).
    pub fn seal(&self, context: &str, plaintext: &str) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut nonce);
        let sealed = self
            .cipher
            .encrypt(&XNonce::from(nonce), Payload { msg: plaintext.as_bytes(), aad: context.as_bytes() })
            .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory input");
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        format!("{PREFIX}{}", to_hex(&out))
    }

    /// The plaintext of `value`, which is returned unchanged if it was never sealed.
    ///
    /// The error never includes the value, sealed or not.
    pub fn open(&self, context: &str, value: &str) -> Result<String, VaultError> {
        let Some(hex) = value.strip_prefix(PREFIX) else { return Ok(value.to_string()) };
        let bytes = from_hex(hex).ok_or(VaultError::Malformed)?;
        if bytes.len() <= NONCE_LEN {
            return Err(VaultError::Malformed);
        }
        let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| VaultError::Malformed)?;
        let plain = self.cipher.decrypt(&XNonce::from(nonce), Payload { msg: ciphertext, aad: context.as_bytes() }).map_err(|_| VaultError::WrongKey)?;
        String::from_utf8(plain).map_err(|_| VaultError::Malformed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    /// Not something `seal` produced.
    Malformed,
    /// Sealed under a different key, for a different field, or altered.
    WrongKey,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "the sealed value is malformed",
            Self::WrongKey => "the sealed value doesn't open with this machine's master key (a different key, or the value was altered)",
        })
    }
}

impl std::error::Error for VaultError {}

/// Creates `path` for writing, owner-only before anything is written (0o600 on unix, an
/// owner-only DACL on Windows).
pub(crate) fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    romexplorer_types::private_file::create_new(path)
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(byte: u8) -> Vault {
        Vault::from_key([byte; 32])
    }

    #[test]
    fn a_sealed_value_opens_to_what_went_in() {
        let v = vault(1);
        let sealed = v.seal("api.env:igdb_key", "s3cret");
        assert!(sealed.starts_with(PREFIX));
        assert!(!sealed.contains("s3cret"));
        assert_eq!(v.open("api.env:igdb_key", &sealed).unwrap(), "s3cret");
    }

    #[test]
    fn sealing_twice_gives_different_text() {
        let v = vault(1);
        assert_ne!(v.seal("c", "same"), v.seal("c", "same"), "a fresh nonce every time");
    }

    #[test]
    fn plaintext_passes_through() {
        assert_eq!(vault(1).open("c", "plain-old-key").unwrap(), "plain-old-key");
    }

    /// A value sealed for one field can't be pasted into another.
    #[test]
    fn the_context_is_bound() {
        let v = vault(1);
        let sealed = v.seal("remote_sources.token", "t");
        assert_eq!(v.open("api.env:igdb_key", &sealed), Err(VaultError::WrongKey));
    }

    #[test]
    fn another_key_or_a_changed_byte_is_refused() {
        let sealed = vault(1).seal("c", "value");
        assert_eq!(vault(2).open("c", &sealed), Err(VaultError::WrongKey));
        let mut tampered = sealed.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_eq!(vault(1).open("c", &tampered), Err(VaultError::WrongKey));
        assert_eq!(vault(1).open("c", "enc:v1:zz"), Err(VaultError::Malformed));
        assert_eq!(vault(1).open("c", "enc:v1:00"), Err(VaultError::Malformed));
    }

    #[test]
    fn the_key_file_is_created_once_and_reused() {
        let dir = std::env::temp_dir().join(format!("rx_vault_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("master.key");
        let first = Vault::load_or_create(&path).unwrap();
        let sealed = first.seal("c", "v");
        let again = Vault::load_or_create(&path).unwrap();
        assert_eq!(again.open("c", &sealed).unwrap(), "v", "the second load reads the same key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "").unwrap();
        let replaced = Vault::load_or_create(&path).unwrap();
        assert!(replaced.open("c", &sealed).is_err(), "an empty key file is replaced with a new key");
        assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "no temporary file is left beside it");
        std::fs::write(&path, "not hex\n").unwrap();
        assert!(Vault::load_or_create(&path).is_err(), "a damaged key file is an error, never silently replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
