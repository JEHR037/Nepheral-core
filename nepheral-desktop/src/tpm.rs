// SPDX-License-Identifier: BUSL-1.1
//! The TPM, through Windows' Platform Crypto Provider: what kind this
//! computer has (a chip, firmware such as Intel PTT or AMD fTPM, or
//! Microsoft Pluton), and an RSA key that lives in it and never leaves, used
//! to seal the node store (`node_store.rs`) under DPAPI.
//!
//! A TPM can be cleared (a BIOS update resets an AMD fTPM), and the key
//! with it. The node key is therefore also kept in the wallet's vault,
//! under its password, and restored from there when the TPM cannot open it.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use windows_sys::core::{HRESULT, PCWSTR};
use windows_sys::Win32::Security::Cryptography::{
    NCryptCreatePersistedKey, NCryptDecrypt, NCryptEncrypt, NCryptFinalizeKey, NCryptFreeObject,
    NCryptGetProperty, NCryptOpenKey, NCryptOpenStorageProvider, NCryptSetProperty,
    BCRYPT_OAEP_PADDING_INFO, BCRYPT_SHA1_ALGORITHM, BCRYPT_SHA256_ALGORITHM,
    MS_PLATFORM_CRYPTO_PROVIDER, NCRYPT_KEY_HANDLE, NCRYPT_LENGTH_PROPERTY, NCRYPT_PAD_OAEP_FLAG,
    NCRYPT_PCP_PLATFORM_TYPE_PROPERTY, NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM,
    NCRYPT_SILENT_FLAG,
};
use zeroize::Zeroizing;

/// What seals the node store, as announced to other nodes (HELLO bits 4–6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A TPM 2.0 chip on the board.
    Discrete,
    /// A TPM 2.0 in the processor's firmware (Intel PTT, AMD fTPM, Qualcomm).
    Firmware,
    /// Microsoft Pluton.
    Pluton,
}

const MAGIC: &[u8; 5] = b"NTPM1";
const KEY_NAME: PCWSTR = windows_sys::core::w!("Nepheral node store");

/// An NCrypt handle, freed on drop.
struct Handle(usize);

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: a handle NCrypt gave out and nothing else frees.
            unsafe { NCryptFreeObject(self.0) };
        }
    }
}

fn check(hr: HRESULT, what: &str) -> Result<(), String> {
    if hr == 0 {
        Ok(())
    } else {
        Err(format!("TPM: {what} failed (0x{:08x})", hr as u32))
    }
}

fn provider() -> Result<Handle, String> {
    let mut prov: NCRYPT_PROV_HANDLE = 0;
    // SAFETY: an out-pointer to a local.
    check(
        unsafe { NCryptOpenStorageProvider(&mut prov, MS_PLATFORM_CRYPTO_PROVIDER, 0) },
        "opening the platform provider",
    )?;
    Ok(Handle(prov))
}

/// The TPM 2.0 there is, if any.
pub fn detect() -> Option<Kind> {
    let prov = provider().ok()?;
    let mut buf = [0u8; 512];
    let mut len = 0u32;
    // SAFETY: the buffer and its length are passed together.
    let hr = unsafe {
        NCryptGetProperty(
            prov.0,
            NCRYPT_PCP_PLATFORM_TYPE_PROPERTY,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut len,
            0,
        )
    };
    if hr != 0 {
        return None;
    }
    let wide: Vec<u16> = buf[..len as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    kind_of(&String::from_utf16_lossy(&wide))
}

/// From the provider's description, e.g.
/// "TPM-Version:2.0 -Level:0-Revision:1.38-VendorID:'INTC'-Firmware:…".
fn kind_of(platform: &str) -> Option<Kind> {
    if !platform.contains("TPM-Version:2.0") {
        return None;
    }
    let vendor = platform
        .split("VendorID:'")
        .nth(1)
        .and_then(|v| v.split('\'').next())
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    Some(match vendor.as_str() {
        "MSFT" => Kind::Pluton,
        "INTC" | "AMD" | "QCOM" | "HISI" => Kind::Firmware,
        _ => Kind::Discrete,
    })
}

/// The sealing key, made in the TPM the first time.
fn key(create: bool) -> Result<(Handle, Handle), String> {
    let prov = provider()?;
    let mut key: NCRYPT_KEY_HANDLE = 0;
    // SAFETY: out-pointer to a local; the name is a static wide string.
    let hr = unsafe { NCryptOpenKey(prov.0, &mut key, KEY_NAME, 0, NCRYPT_SILENT_FLAG) };
    if hr == 0 {
        return Ok((prov, Handle(key)));
    }
    if !create {
        return Err(format!(
            "TPM: the sealing key is gone (0x{:08x})",
            hr as u32
        ));
    }
    // SAFETY: as above.
    check(
        unsafe { NCryptCreatePersistedKey(prov.0, &mut key, NCRYPT_RSA_ALGORITHM, KEY_NAME, 0, 0) },
        "creating the sealing key",
    )?;
    let key = Handle(key);
    let bits: u32 = 2048;
    // SAFETY: four bytes, as the length property expects.
    check(
        unsafe {
            NCryptSetProperty(
                key.0,
                NCRYPT_LENGTH_PROPERTY,
                (&bits as *const u32).cast(),
                4,
                0,
            )
        },
        "sizing the sealing key",
    )?;
    // SAFETY: a key made just above.
    check(
        unsafe { NCryptFinalizeKey(key.0, NCRYPT_SILENT_FLAG) },
        "finishing the sealing key",
    )?;
    Ok((prov, key))
}

fn oaep(hash: u8) -> BCRYPT_OAEP_PADDING_INFO {
    BCRYPT_OAEP_PADDING_INFO {
        pszAlgId: if hash == 2 {
            BCRYPT_SHA256_ALGORITHM
        } else {
            BCRYPT_SHA1_ALGORITHM
        },
        pbLabel: std::ptr::null_mut(),
        cbLabel: 0,
    }
}

fn rsa(key: &Handle, input: &[u8], hash: u8, decrypt: bool) -> Result<Vec<u8>, String> {
    let pad = oaep(hash);
    let pad_ptr: *const core::ffi::c_void = (&pad as *const BCRYPT_OAEP_PADDING_INFO).cast();
    let call = |out: *mut u8, cap: u32, len: &mut u32| -> HRESULT {
        // SAFETY: input and output buffers are passed with their lengths;
        // the padding info outlives the call.
        unsafe {
            if decrypt {
                NCryptDecrypt(
                    key.0,
                    input.as_ptr(),
                    input.len() as u32,
                    pad_ptr,
                    out,
                    cap,
                    len,
                    NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
                )
            } else {
                NCryptEncrypt(
                    key.0,
                    input.as_ptr(),
                    input.len() as u32,
                    pad_ptr,
                    out,
                    cap,
                    len,
                    NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
                )
            }
        }
    };
    let mut len = 0u32;
    check(call(std::ptr::null_mut(), 0, &mut len), "sizing")?;
    let mut out = vec![0u8; len as usize];
    check(
        call(out.as_mut_ptr(), len, &mut len),
        if decrypt { "unsealing" } else { "sealing" },
    )?;
    out.truncate(len as usize);
    Ok(out)
}

/// `NTPM1`, the OAEP hash (1 SHA-1, 2 SHA-256), the wrapped key's length
/// (u16, big-endian), the key wrapped by the TPM, a nonce, and the data
/// under XChaCha20-Poly1305 with that key.
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    use rand::RngCore;
    let (_prov, key) = key(true)?;
    let mut dek = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut dek[..]);
    // SHA-256 where the TPM takes it, SHA-1 otherwise; both are sound for
    // wrapping a key.
    let (hash, wrapped) = match rsa(&key, &dek[..], 2, false) {
        Ok(w) => (2u8, w),
        Err(_) => (1u8, rsa(&key, &dek[..], 1, false)?),
    };
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new((&*dek).into())
        .encrypt(XNonce::from_slice(&nonce), plain)
        .map_err(|_| "TPM: encrypting failed".to_string())?;
    let mut out = Vec::with_capacity(MAGIC.len() + 3 + wrapped.len() + 24 + ct.len());
    out.extend_from_slice(MAGIC);
    out.push(hash);
    out.extend_from_slice(&(wrapped.len() as u16).to_be_bytes());
    out.extend_from_slice(&wrapped);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn is_sealed(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
    let body = sealed
        .strip_prefix(MAGIC.as_slice())
        .ok_or("TPM: not a sealed store")?;
    let (&hash, rest) = body.split_first().ok_or("TPM: truncated")?;
    if rest.len() < 2 {
        return Err("TPM: truncated".into());
    }
    let n = u16::from_be_bytes([rest[0], rest[1]]) as usize;
    let rest = &rest[2..];
    if rest.len() < n + 24 {
        return Err("TPM: truncated".into());
    }
    let (wrapped, rest) = rest.split_at(n);
    let (nonce, ct) = rest.split_at(24);
    let (_prov, key) = key(false)?;
    let dek = Zeroizing::new(rsa(&key, wrapped, hash, true)?);
    if dek.len() != 32 {
        return Err("TPM: wrong key size".into());
    }
    XChaCha20Poly1305::new(dek.as_slice().into())
        .decrypt(XNonce::from_slice(nonce), ct)
        .map_err(|_| "TPM: the store does not open with this TPM's key".to_string())
}

/// Never touches a real TPM: only the parsing.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_strings_say_what_kind() {
        let s =
            |v: &str| format!("TPM-Version:2.0 -Level:0-Revision:1.38-VendorID:'{v}'-Firmware:1");
        assert_eq!(kind_of(&s("INTC")), Some(Kind::Firmware));
        assert_eq!(kind_of(&s("AMD ")), Some(Kind::Firmware));
        assert_eq!(kind_of(&s("MSFT")), Some(Kind::Pluton));
        assert_eq!(kind_of(&s("IFX ")), Some(Kind::Discrete));
        assert_eq!(kind_of(&s("NTC ")), Some(Kind::Discrete));
        assert_eq!(kind_of("TPM-Version:1.2 -VendorID:'IFX '"), None);
    }

    /// Uses this computer's TPM: `cargo test -p nepheral-desktop tpm -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_tpm_seals_and_opens() {
        let Some(kind) = detect() else {
            eprintln!("no TPM 2.0 here");
            return;
        };
        eprintln!("TPM: {kind:?}");
        let plain = b"{\"seed\":\"00\"}".repeat(40);
        let sealed = seal(&plain).unwrap();
        assert!(is_sealed(&sealed));
        assert_eq!(unseal(&sealed).unwrap(), plain);
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(unseal(&bad).is_err());
    }

    #[test]
    fn a_truncated_store_is_refused_before_the_tpm_is_asked() {
        assert!(unseal(b"NTPM1").is_err());
        assert!(unseal(b"NTPM1\x02\x01\x00abc").is_err());
        assert!(!is_sealed(b"{\"seed\":\"00\"}"));
    }
}
