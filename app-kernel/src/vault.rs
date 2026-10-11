// SPDX-License-Identifier: BUSL-1.1
//! The desktop's secrets at rest.
//!
//! The wallet's recovery phrase, the local API token and the local system
//! prompt used to sit in plain files under `~/.nepheral`, readable by every
//! program running as the user. They now live in one file, `vault.bin`,
//! sealed with a key derived from a password the user types when the app
//! opens. The password itself is never stored; the derived key is held in
//! memory only while the app runs, so settings can be saved without asking
//! for the password again, and is wiped when the app closes.
//!
//! Format: `NEPHVLT1` | version | Argon2id cost (memory KiB, passes, lanes)
//! | salt | XChaCha20-Poly1305 nonce | ciphertext. Everything before the
//! ciphertext is authenticated as associated data, so a file whose cost
//! parameters were tampered with does not open.

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const VAULT_FILE: &str = "vault.bin";
pub const LEGACY_IDENTITY_FILE: &str = "identity.mnemonic";
pub const DESKTOP_LOG_FILE: &str = "nepheral-desktop.log";
pub const MIN_PASSWORD_CHARS: usize = 8;

const MAGIC: &[u8; 8] = b"NEPHVLT1";
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = MAGIC.len() + 1 + 12 + SALT_LEN + NONCE_LEN;

/// What is kept secret. Wiped from memory when dropped.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Secrets {
    pub mnemonic: String,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// The node key's seed: a copy, so a node whose key store can no longer
    /// be opened (a cleared TPM) keeps its identity once the wallet opens.
    #[serde(default)]
    pub node_seed: Option<String>,
}

/// Argon2id cost. About half a second on a laptop: slow enough that guessing
/// a stolen file's password is expensive, quick enough to unlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfCost {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

impl KdfCost {
    pub const DEFAULT: KdfCost = KdfCost {
        memory_kib: 64 * 1024,
        passes: 3,
        lanes: 1,
    };
}

#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    WrongPassword,
    Corrupt(String),
    Io(String),
    WeakPassword,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "wrong password"),
            Self::Corrupt(why) => write!(f, "the vault file is damaged: {why}"),
            Self::Io(why) => write!(f, "{why}"),
            Self::WeakPassword => write!(
                f,
                "the password must have at least {MIN_PASSWORD_CHARS} characters"
            ),
        }
    }
}

impl std::error::Error for VaultError {}

/// What the app finds on disk when it opens.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultStatus {
    /// Nothing yet: a new wallet is created.
    Missing,
    /// A phrase in a plain file from before the vault: it is moved in.
    Legacy,
    Sealed,
}

pub fn status(dir: &Path) -> VaultStatus {
    if dir.join(VAULT_FILE).exists() {
        VaultStatus::Sealed
    } else if dir.join(LEGACY_IDENTITY_FILE).exists() {
        VaultStatus::Legacy
    } else {
        VaultStatus::Missing
    }
}

/// An unlocked vault: where it lives and the key that seals it.
pub struct Vault {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: [u8; SALT_LEN],
    cost: KdfCost,
}

impl Vault {
    /// Creates a vault holding `secrets`, replacing nothing: an existing file
    /// is an error, so a vault is never overwritten by accident.
    pub fn create(
        dir: &Path,
        password: &str,
        secrets: &Secrets,
        cost: KdfCost,
    ) -> Result<Vault, VaultError> {
        check_password(password)?;
        let path = dir.join(VAULT_FILE);
        if path.exists() {
            return Err(VaultError::Io(
                "a vault already exists; unlock it or restore over it".into(),
            ));
        }
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        let vault = Vault {
            key: derive(password, &salt, cost)?,
            path,
            salt,
            cost,
        };
        vault.save(secrets)?;
        Ok(vault)
    }

    /// Opens the vault in `dir`. A wrong password and a damaged file are
    /// told apart only as far as the file's own structure allows: a file that
    /// parses but does not decrypt reads as a wrong password.
    pub fn unlock(dir: &Path, password: &str) -> Result<(Vault, Secrets), VaultError> {
        let path = dir.join(VAULT_FILE);
        let bytes = std::fs::read(&path).map_err(|e| VaultError::Io(e.to_string()))?;
        let (cost, salt, nonce, ciphertext, header) = parse(&bytes)?;
        let key = derive(password, &salt, cost)?;
        let secrets = decrypt(&key, &nonce, ciphertext, header)?;
        Ok((
            Vault {
                path,
                key,
                salt,
                cost,
            },
            secrets,
        ))
    }

    /// Seals `secrets` again with the same key and a fresh nonce. Written to
    /// a temporary file first, so a crash mid-write never leaves half a vault.
    pub fn save(&self, secrets: &Secrets) -> Result<(), VaultError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
        let header = header(self.cost, &self.salt, &nonce);
        let mut plaintext = Zeroizing::new(
            serde_json::to_vec(secrets).map_err(|e| VaultError::Corrupt(e.to_string()))?,
        );
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| VaultError::Corrupt("encryption failed".into()))?;
        plaintext.zeroize();
        let mut out = header;
        out.extend_from_slice(&ciphertext);
        let tmp = self.path.with_extension("bin.tmp");
        crate::nepheral_config::write_private_file(&tmp, &out).map_err(VaultError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(|e| VaultError::Io(e.to_string()))
    }

    /// Reseals under a new password, with a new salt. The old password must
    /// open the file as it is on disk.
    pub fn change_password(
        &mut self,
        old: &str,
        new: &str,
        secrets: &Secrets,
    ) -> Result<(), VaultError> {
        check_password(new)?;
        let dir = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Vault::unlock(&dir, old)?;
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        self.key = derive(new, &salt, self.cost)?;
        self.salt = salt;
        self.save(secrets)
    }
}

/// Moves a phrase kept in a plain file, and the token and prompt kept in
/// `config.json`, into a new vault - then removes the plain copies.
///
/// Nothing is removed until the new vault has been read back with the same
/// password and found to hold the same phrase: a failure anywhere leaves the
/// old files exactly as they were.
pub fn migrate_legacy(
    dir: &Path,
    password: &str,
    cost: KdfCost,
) -> Result<(Vault, Secrets), VaultError> {
    let identity = dir.join(LEGACY_IDENTITY_FILE);
    let phrase = Zeroizing::new(
        std::fs::read_to_string(&identity).map_err(|e| VaultError::Io(e.to_string()))?,
    );
    core_crypto::NodeMnemonic::from_phrase(phrase.trim())
        .map_err(|e| VaultError::Corrupt(format!("the existing phrase is invalid: {e}")))?;
    let cfg = crate::nepheral_config::load_config().unwrap_or_default();
    let secrets = Secrets {
        mnemonic: phrase.trim().to_string(),
        api_token: cfg.local_api_server_token.clone(),
        system_prompt: cfg.local_system_prompt.clone(),
        node_seed: None,
    };
    let vault = Vault::create(dir, password, &secrets, cost)?;

    let (_, read_back) = Vault::unlock(dir, password)?;
    if read_back.mnemonic != secrets.mnemonic {
        let _ = std::fs::remove_file(dir.join(VAULT_FILE));
        return Err(VaultError::Corrupt(
            "the vault did not read back what was written; nothing was removed".into(),
        ));
    }

    wipe_plain_copies(dir)?;
    Ok((vault, secrets))
}

/// Removes every plain copy of a secret this app ever wrote: the phrase and
/// its restore backups, the token and prompt in `config.json`, and the
/// desktop log, which held addresses and peer identities.
pub fn wipe_plain_copies(dir: &Path) -> Result<(), VaultError> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == LEGACY_IDENTITY_FILE || name.starts_with("identity.mnemonic.bak-") {
                wipe_file(&entry.path())?;
            } else if name == DESKTOP_LOG_FILE {
                // Best effort: on Windows a log another process still has
                // open cannot be removed, and that must not undo the rest.
                if let Err(e) = wipe_file(&entry.path()) {
                    tracing::warn!(error = %e, "Old desktop log not removed yet");
                }
            }
        }
    }
    if crate::nepheral_config::load_config().is_some() {
        crate::nepheral_config::update(|c| {
            c.local_api_server_token = None;
            c.local_system_prompt = None;
        })
        .map_err(VaultError::Io)?;
    }
    Ok(())
}

/// Overwrites a file with zeros, flushes, then deletes it.
///
/// On an SSD the old blocks may survive in places the filesystem no longer
/// points to; this removes the copy any program could open, which is what a
/// plain file on disk was.
pub fn wipe_file(path: &Path) -> Result<(), VaultError> {
    use std::io::Write;
    let len = std::fs::metadata(path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
        let zeros = vec![0u8; len.min(64 * 1024 * 1024)];
        let _ = f.write_all(&zeros);
        let _ = f.sync_all();
    }
    std::fs::remove_file(path).map_err(|e| VaultError::Io(format!("{}: {e}", path.display())))
}

/// Replaces whatever vault is in `dir` with one holding `secrets`, keeping
/// the old file beside it: it opens with whatever password it had, so a
/// restore with the wrong phrase can still be undone.
pub fn restore(
    dir: &Path,
    password: &str,
    secrets: &Secrets,
    cost: KdfCost,
    now_ms: u64,
) -> Result<Vault, VaultError> {
    let vault = replace(dir, password, secrets, cost, now_ms)?;
    wipe_plain_copies(dir)?;
    Ok(vault)
}

/// Seals `secrets` in a new vault, keeping any existing one beside it as
/// `vault.bin.bak-<now_ms>`. Unlike `restore` it touches no other file: on
/// a headless node `identity.mnemonic` is the node's own key, not a stray
/// copy of the wallet.
pub fn replace(
    dir: &Path,
    password: &str,
    secrets: &Secrets,
    cost: KdfCost,
    now_ms: u64,
) -> Result<Vault, VaultError> {
    check_password(password)?;
    core_crypto::NodeMnemonic::from_phrase(secrets.mnemonic.trim())
        .map_err(|e| VaultError::Corrupt(format!("invalid recovery phrase: {e}")))?;
    let path = dir.join(VAULT_FILE);
    if path.exists() {
        std::fs::rename(&path, dir.join(format!("vault.bin.bak-{now_ms}")))
            .map_err(|e| VaultError::Io(e.to_string()))?;
    }
    Vault::create(dir, password, secrets, cost)
}

fn check_password(password: &str) -> Result<(), VaultError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(VaultError::WeakPassword);
    }
    Ok(())
}

fn derive(
    password: &str,
    salt: &[u8; SALT_LEN],
    cost: KdfCost,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let params = argon2::Params::new(cost.memory_kib, cost.passes, cost.lanes, Some(32))
        .map_err(|e| VaultError::Corrupt(format!("cost parameters: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(password.as_bytes(), salt, &mut *key)
        .map_err(|e| VaultError::Corrupt(format!("key derivation: {e}")))?;
    Ok(key)
}

fn header(cost: KdfCost, salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(MAGIC);
    h.push(VERSION);
    h.extend_from_slice(&cost.memory_kib.to_le_bytes());
    h.extend_from_slice(&cost.passes.to_le_bytes());
    h.extend_from_slice(&cost.lanes.to_le_bytes());
    h.extend_from_slice(salt);
    h.extend_from_slice(nonce);
    h
}

type Parsed<'a> = (KdfCost, [u8; SALT_LEN], [u8; NONCE_LEN], &'a [u8], &'a [u8]);

fn parse(bytes: &[u8]) -> Result<Parsed<'_>, VaultError> {
    if bytes.len() <= HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err(VaultError::Corrupt("not a Nepheral vault".into()));
    }
    if bytes[MAGIC.len()] != VERSION {
        return Err(VaultError::Corrupt(format!(
            "vault version {} is newer than this app",
            bytes[MAGIC.len()]
        )));
    }
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().expect("4 bytes"));
    let base = MAGIC.len() + 1;
    let cost = KdfCost {
        memory_kib: u32_at(base),
        passes: u32_at(base + 4),
        lanes: u32_at(base + 8),
    };
    // A file asking for absurd memory would stall the app before it could
    // even say the password is wrong.
    if cost.memory_kib > 1024 * 1024 || cost.passes > 64 || cost.lanes > 16 {
        return Err(VaultError::Corrupt("implausible cost parameters".into()));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&bytes[base + 12..base + 12 + SALT_LEN]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[base + 12 + SALT_LEN..HEADER_LEN]);
    Ok((
        cost,
        salt,
        nonce,
        &bytes[HEADER_LEN..],
        &bytes[..HEADER_LEN],
    ))
}

fn decrypt(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    header: &[u8],
) -> Result<Secrets, VaultError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: header,
                },
            )
            .map_err(|_| VaultError::WrongPassword)?,
    );
    serde_json::from_slice(&plaintext).map_err(|e| VaultError::Corrupt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nepheral_config::test_support::TempHome;

    const FAST: KdfCost = KdfCost {
        memory_kib: 256,
        passes: 1,
        lanes: 1,
    };

    fn phrase() -> String {
        core_crypto::NodeMnemonic::generate().phrase().to_string()
    }

    fn secrets() -> Secrets {
        Secrets {
            mnemonic: phrase(),
            api_token: Some("t".repeat(48)),
            system_prompt: Some("answer in Spanish".into()),
            node_seed: None,
        }
    }

    #[test]
    fn what_is_sealed_opens_with_the_password_and_nothing_else() {
        let home = TempHome::new("vault");
        let s = secrets();
        Vault::create(home.path(), "correct horse", &s, FAST).unwrap();
        let (_, back) = Vault::unlock(home.path(), "correct horse").unwrap();
        assert_eq!(back.mnemonic, s.mnemonic);
        assert_eq!(back.api_token, s.api_token);
        assert!(matches!(
            Vault::unlock(home.path(), "wrong horse"),
            Err(VaultError::WrongPassword)
        ));
    }

    #[test]
    fn the_file_holds_no_secret_in_the_clear() {
        let home = TempHome::new("vault");
        let s = secrets();
        Vault::create(home.path(), "correct horse", &s, FAST).unwrap();
        let raw = std::fs::read(home.path().join(VAULT_FILE)).unwrap();
        let text = String::from_utf8_lossy(&raw);
        let first_word = s.mnemonic.split_whitespace().next().unwrap();
        assert!(!text.contains(&s.mnemonic));
        assert!(!text.contains("answer in Spanish"));
        assert!(!text.contains(&"t".repeat(48)));
        assert!(!text.contains(&format!(" {first_word} ")));
    }

    #[test]
    fn a_tampered_header_or_body_does_not_open() {
        let home = TempHome::new("vault");
        Vault::create(home.path(), "correct horse", &secrets(), FAST).unwrap();
        let path = home.path().join(VAULT_FILE);
        let good = std::fs::read(&path).unwrap();

        let mut body = good.clone();
        let last = body.len() - 1;
        body[last] ^= 1;
        std::fs::write(&path, &body).unwrap();
        assert!(Vault::unlock(home.path(), "correct horse").is_err());

        let mut salt = good.clone();
        salt[MAGIC.len() + 1 + 12] ^= 1;
        std::fs::write(&path, &salt).unwrap();
        assert!(Vault::unlock(home.path(), "correct horse").is_err());

        std::fs::write(&path, b"NEPHVLT1").unwrap();
        assert!(matches!(
            Vault::unlock(home.path(), "correct horse"),
            Err(VaultError::Corrupt(_))
        ));
    }

    #[test]
    fn short_passwords_are_refused() {
        let home = TempHome::new("vault");
        assert_eq!(
            Vault::create(home.path(), "short", &secrets(), FAST).err(),
            Some(VaultError::WeakPassword)
        );
        assert!(!home.path().join(VAULT_FILE).exists());
    }

    #[test]
    fn an_existing_vault_is_never_overwritten_by_create() {
        let home = TempHome::new("vault");
        Vault::create(home.path(), "correct horse", &secrets(), FAST).unwrap();
        assert!(Vault::create(home.path(), "another one", &secrets(), FAST).is_err());
        assert!(Vault::unlock(home.path(), "correct horse").is_ok());
    }

    #[test]
    fn saving_reseals_without_the_password() {
        let home = TempHome::new("vault");
        let mut s = secrets();
        let vault = Vault::create(home.path(), "correct horse", &s, FAST).unwrap();
        s.system_prompt = Some("be brief".into());
        vault.save(&s).unwrap();
        let (_, back) = Vault::unlock(home.path(), "correct horse").unwrap();
        assert_eq!(back.system_prompt.as_deref(), Some("be brief"));
    }

    #[test]
    fn a_password_change_needs_the_old_one_and_retires_it() {
        let home = TempHome::new("vault");
        let s = secrets();
        let mut vault = Vault::create(home.path(), "correct horse", &s, FAST).unwrap();
        assert!(vault
            .change_password("wrong horse", "battery staple", &s)
            .is_err());
        vault
            .change_password("correct horse", "battery staple", &s)
            .unwrap();
        assert!(Vault::unlock(home.path(), "correct horse").is_err());
        assert_eq!(
            Vault::unlock(home.path(), "battery staple")
                .unwrap()
                .1
                .mnemonic
                .clone(),
            s.mnemonic
        );
    }

    #[test]
    fn migration_moves_every_plain_secret_in_and_removes_the_copies() {
        let home = TempHome::new("vault");
        let dir = home.path();
        let p = phrase();
        std::fs::write(dir.join(LEGACY_IDENTITY_FILE), &p).unwrap();
        std::fs::write(dir.join("identity.mnemonic.bak-1"), &p).unwrap();
        std::fs::write(dir.join(DESKTOP_LOG_FILE), "peer 93.189.89.201").unwrap();
        crate::nepheral_config::update(|c| {
            c.local_api_server_token = Some("a".repeat(48));
            c.local_system_prompt = Some("private prompt".into());
        })
        .unwrap();
        assert_eq!(status(dir), VaultStatus::Legacy);

        let (_, moved) = migrate_legacy(dir, "correct horse", FAST).unwrap();
        assert_eq!(moved.mnemonic, p);
        assert_eq!(moved.api_token.as_deref(), Some("a".repeat(48).as_str()));
        assert_eq!(moved.system_prompt.as_deref(), Some("private prompt"));
        assert_eq!(status(dir), VaultStatus::Sealed);
        assert!(!dir.join(LEGACY_IDENTITY_FILE).exists());
        assert!(!dir.join("identity.mnemonic.bak-1").exists());
        assert!(!dir.join(DESKTOP_LOG_FILE).exists());
        let cfg = crate::nepheral_config::load_config().unwrap();
        assert!(cfg.local_api_server_token.is_none());
        assert!(cfg.local_system_prompt.is_none());
    }

    #[test]
    fn a_failed_migration_leaves_the_plain_phrase_where_it_was() {
        let home = TempHome::new("vault");
        let dir = home.path();
        let p = phrase();
        std::fs::write(dir.join(LEGACY_IDENTITY_FILE), &p).unwrap();
        assert!(migrate_legacy(dir, "short", FAST).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(LEGACY_IDENTITY_FILE)).unwrap(),
            p
        );
        assert!(!dir.join(VAULT_FILE).exists());
    }

    #[test]
    fn a_restore_keeps_the_previous_vault_beside_the_new_one() {
        let home = TempHome::new("vault");
        let dir = home.path();
        let old = secrets();
        Vault::create(dir, "old password", &old, FAST).unwrap();
        let new = secrets();
        restore(dir, "new password", &new, FAST, 42).unwrap();
        assert_eq!(
            Vault::unlock(dir, "new password")
                .unwrap()
                .1
                .mnemonic
                .clone(),
            new.mnemonic
        );
        std::fs::rename(dir.join("vault.bin.bak-42"), dir.join("old")).unwrap();
        std::fs::create_dir(dir.join("o")).unwrap();
        std::fs::rename(dir.join("old"), dir.join("o").join(VAULT_FILE)).unwrap();
        assert_eq!(
            Vault::unlock(&dir.join("o"), "old password")
                .unwrap()
                .1
                .mnemonic
                .clone(),
            old.mnemonic
        );
    }

    #[test]
    fn a_restore_refuses_a_phrase_that_is_not_one() {
        let home = TempHome::new("vault");
        let bad = Secrets {
            mnemonic: "not a recovery phrase".into(),
            api_token: None,
            system_prompt: None,
            node_seed: None,
        };
        assert!(restore(home.path(), "correct horse", &bad, FAST, 1).is_err());
        assert!(!home.path().join(VAULT_FILE).exists());
    }
}
