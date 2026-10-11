// SPDX-License-Identifier: BUSL-1.1
//! What the node needs to run before anyone unlocks anything: its own key
//! and the system prompt its model answers with.
//!
//! Neither holds funds - the wallet does, under its password - but both stay
//! off the disk in the clear. On Windows the file is sealed with DPAPI, which
//! ties it to this user's login on this computer: another account, or the
//! file copied elsewhere, cannot open it; where the computer has a TPM, the
//! file is also sealed inside DPAPI with a key that lives in the TPM
//! (`tpm.rs`). On Android it is sealed with a key
//! kept in the Android Keystore (the phone's secure hardware where it has
//! one), through `KeySealer.kt`. Elsewhere it is a file only its owner can
//! read (0600), like an SSH host key.

use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const NODE_STORE_FILE: &str = "node.bin";

#[cfg(windows)]
const ENTROPY: &[u8] = b"nepheral/node-store/v1";

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct NodeStore {
    /// The node key's seed, hex.
    seed: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

impl NodeStore {
    /// A store with a new random node key.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut seed[..]);
        Self {
            seed: hex::encode(&seed[..]),
            system_prompt: None,
        }
    }

    /// A store for a known key: the copy kept in the wallet's vault, when
    /// the store itself can no longer be opened.
    pub fn from_seed(seed: &str, system_prompt: Option<String>) -> Result<Self, String> {
        let seed = seed.trim().to_ascii_lowercase();
        if seed.len() != 64 || hex::decode(&seed).is_err() {
            return Err("the saved node key is not valid".into());
        }
        Ok(Self {
            seed,
            system_prompt,
        })
    }

    /// The key's seed, for the copy in the vault.
    pub fn seed_hex(&self) -> &str {
        &self.seed
    }

    pub fn signing_key(&self) -> Result<ed25519_dalek::SigningKey, String> {
        let mut seed = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(&self.seed, &mut seed[..])
            .map_err(|_| "the node store holds no valid key".to_string())?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// The store in `dir`, if there is one.
    pub fn load(dir: &Path) -> Result<Option<Self>, String> {
        let path = dir.join(NODE_STORE_FILE);
        let sealed = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read the node store: {e}")),
        };
        // A store written before sealing existed on this platform is plain
        // JSON: it is read, and sealed in place.
        let legacy = SEALS && sealed.first() == Some(&b'{');
        let plain = Zeroizing::new(if legacy {
            sealed.clone()
        } else {
            unseal(&sealed)?
        });
        let store: Self = serde_json::from_slice(&plain)
            .map_err(|e| format!("the node store is damaged: {e}"))?;
        if legacy {
            match store.save(dir) {
                Ok(()) => tracing::info!("Node store sealed in place"),
                Err(e) => tracing::warn!(error = %e, "Node store left as it was"),
            }
        }
        Ok(Some(store))
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let plain = Zeroizing::new(serde_json::to_vec(self).map_err(|e| e.to_string())?);
        let sealed = seal(&plain)?;
        app_kernel::nepheral_config::ensure_private_dir(dir)?;
        let path = dir.join(NODE_STORE_FILE);
        let tmp = dir.join(format!("{NODE_STORE_FILE}.tmp"));
        app_kernel::nepheral_config::write_private_file(&tmp, &sealed)?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("cannot save the node store: {e}"))
    }

    /// The store in `dir`, made with a new key if there is none yet.
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        if let Some(store) = Self::load(dir)? {
            return Ok(store);
        }
        let store = Self::generate();
        store.save(dir)?;
        tracing::info!("New node key created");
        Ok(store)
    }
}

/// Sealed with the TPM where there is one and the vault already keeps a
/// copy of the key (`allow_tpm`), then with DPAPI.
#[cfg(windows)]
fn seal_with(plain: &[u8], allow_tpm: bool) -> Result<Vec<u8>, String> {
    let kind = if allow_tpm {
        crate::tpm::detect()
    } else {
        None
    };
    let inner = match kind {
        Some(kind) => match crate::tpm::seal(plain) {
            Ok(sealed) => {
                note_seal(Some(tpm_seal_kind(kind)));
                Zeroizing::new(sealed)
            }
            Err(e) => {
                tracing::warn!(error = %e, "TPM did not seal the node store; DPAPI only");
                note_seal(None);
                Zeroizing::new(plain.to_vec())
            }
        },
        None => {
            note_seal(None);
            Zeroizing::new(plain.to_vec())
        }
    };
    dpapi::protect(&inner)
}

#[cfg(windows)]
fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    seal_with(
        plain,
        TPM_ALLOWED.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[cfg(windows)]
fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
    let inner = Zeroizing::new(dpapi::unprotect(sealed)?);
    if crate::tpm::is_sealed(&inner) {
        let plain = crate::tpm::unseal(&inner)?;
        note_seal(crate::tpm::detect().map(tpm_seal_kind));
        Ok(plain)
    } else {
        note_seal(None);
        Ok(inner.to_vec())
    }
}

/// Set once the wallet's vault holds a copy of the node key: only then is
/// the store sealed with the TPM, whose clearing would otherwise lose it.
#[cfg(windows)]
static TPM_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The vault now keeps a copy of the key: from here the store may be
/// sealed with the TPM.
pub fn allow_hardware_seal() {
    #[cfg(windows)]
    TPM_ALLOWED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn hw_seal_allowed() -> bool {
    #[cfg(windows)]
    {
        TPM_ALLOWED.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    {
        true
    }
}

#[cfg(target_os = "android")]
fn android_seal_kind() -> Option<mesh_network::protocol::HwSeal> {
    match keystore::level().unwrap_or(0) {
        2 => Some(mesh_network::protocol::HwSeal::Strongbox),
        1 => Some(mesh_network::protocol::HwSeal::Tee),
        _ => None,
    }
}

#[cfg(target_os = "android")]
fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    let sealed = keystore::call("seal", plain)?;
    note_seal(android_seal_kind());
    Ok(sealed)
}

#[cfg(target_os = "android")]
fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
    let plain = keystore::call("unseal", sealed)?;
    note_seal(android_seal_kind());
    Ok(plain)
}

#[cfg(not(any(windows, target_os = "android")))]
fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    Ok(plain.to_vec())
}

#[cfg(not(any(windows, target_os = "android")))]
fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(sealed.to_vec())
}

/// Whether this platform seals the file (a plain one is then migrated).
const SEALS: bool = cfg!(any(windows, target_os = "android"));

/// What sealed the store last read or written here, for HELLO.
static SEALED_WITH: std::sync::Mutex<Option<mesh_network::protocol::HwSeal>> =
    std::sync::Mutex::new(None);

#[cfg(any(windows, target_os = "android"))]
fn note_seal(seal: Option<mesh_network::protocol::HwSeal>) {
    *SEALED_WITH.lock().expect("seal note") = seal;
}

/// The hardware the node key is sealed in, as last seen by `load`/`save`.
pub fn hw_seal() -> Option<mesh_network::protocol::HwSeal> {
    *SEALED_WITH.lock().expect("seal note")
}

#[cfg(windows)]
fn tpm_seal_kind(kind: crate::tpm::Kind) -> mesh_network::protocol::HwSeal {
    use mesh_network::protocol::HwSeal;
    match kind {
        crate::tpm::Kind::Discrete => HwSeal::Tpm,
        crate::tpm::Kind::Firmware => HwSeal::Ftpm,
        crate::tpm::Kind::Pluton => HwSeal::Pluton,
    }
}

#[cfg(target_os = "android")]
mod keystore {
    use jni::objects::{JByteArray, JValue};

    const CLASS: &str = "net.nepheral.app.KeySealer";

    pub fn call(method: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        crate::android::with_class(CLASS, |env, class, _| {
            let arg = env.byte_array_from_slice(input)?;
            let out = env
                .call_static_method(class, method, "([B)[B", &[JValue::Object(&arg)])?
                .l()?;
            env.convert_byte_array(JByteArray::from(out))
        })
        .map_err(|e| format!("the Android Keystore refused: {e}"))
    }

    pub fn level() -> Result<u8, String> {
        crate::android::with_class(CLASS, |env, class, _| {
            let v = env
                .call_static_method(class, "hardwareLevel", "()I", &[])?
                .i()?;
            Ok(v.clamp(0, 2) as u8)
        })
    }
}

#[cfg(windows)]
mod dpapi {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// Copies the output out and frees the buffer Windows allocated.
    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: on success the API returns `cbData` bytes at `pbData`,
        // allocated with LocalAlloc; they are copied before being freed.
        unsafe {
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
            LocalFree(out.pbData as _);
            bytes
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(plain);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        // SAFETY: every pointer is to a live blob for the call's duration.
        let ok = unsafe {
            CryptProtectData(
                &input,
                null(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Windows could not seal the node store ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(take(out))
    }

    pub fn unprotect(sealed: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(sealed);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                null_mut(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Windows could not open the node store - was it made by another user or on \
                 another computer? ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_survives_a_round_trip_and_keeps_its_key() {
        let dir = std::env::temp_dir().join(format!("nepheral-node-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(NodeStore::load(&dir).unwrap().is_none());
        let mut store = NodeStore::load_or_create(&dir).unwrap();
        let key = store.signing_key().unwrap().verifying_key();
        store.system_prompt = Some("be brief".into());
        store.save(&dir).unwrap();

        let again = NodeStore::load(&dir).unwrap().unwrap();
        assert_eq!(again.signing_key().unwrap().verifying_key(), key);
        assert_eq!(again.system_prompt.as_deref(), Some("be brief"));

        // Sealed on Windows: neither the prompt nor the key reads in the file.
        #[cfg(windows)]
        {
            let raw = std::fs::read(dir.join(NODE_STORE_FILE)).unwrap();
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains("be brief"));
            assert!(!text.contains(&store.seed));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
