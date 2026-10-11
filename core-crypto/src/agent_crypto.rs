// SPDX-License-Identifier: BUSL-1.1
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::edwards::CompressedEdwardsY;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::identity::PeerId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    InvalidRecipientPubkey,
    EncryptFailed,
    DecryptFailed,
    NoWrappedKeyForRecipient,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for CryptoError {}

pub fn pubkey_ed_to_x(peer_id: &PeerId) -> Option<[u8; 32]> {
    let compressed = CompressedEdwardsY(*peer_id);
    let edwards = compressed.decompress()?;
    Some(edwards.to_montgomery().to_bytes())
}

pub fn signing_key_ed_to_x_secret(sk: &SigningKey) -> StaticSecret {
    let seed = sk.to_bytes();
    let hash = Sha512::digest(seed);
    let mut x_priv = [0u8; 32];
    x_priv.copy_from_slice(&hash[..32]);
    StaticSecret::from(x_priv)
}

pub fn signing_key_ed_to_x_public(sk: &SigningKey) -> [u8; 32] {
    let vk: VerifyingKey = sk.verifying_key();
    let bytes = vk.to_bytes();
    pubkey_ed_to_x(&bytes).expect("VerifyingKey bytes always decompress")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedKey {
    pub recipient: PeerId,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

pub fn derive_shared_key(
    my_secret: &StaticSecret,
    their_x_pub: &[u8; 32],
    eph_pub: &[u8; 32],
    recipient_x_pub: &[u8; 32],
    context: &[u8],
) -> Result<[u8; 32], CryptoError> {
    let shared = my_secret.diffie_hellman(&XPublicKey::from(*their_x_pub));
    if !shared.was_contributory() {
        return Err(CryptoError::InvalidRecipientPubkey);
    }
    let mut h = Sha256::new();
    h.update(b"nepheral/x25519-kdf/v1");
    h.update((context.len() as u32).to_be_bytes());
    h.update(context);
    h.update(shared.as_bytes());
    h.update(eph_pub);
    h.update(recipient_x_pub);
    Ok(h.finalize().into())
}

const WRAP_CONTEXT: &[u8] = b"session-key-wrap";
const SEAL_CONTEXT: &[u8] = b"sealed-to-peer";

pub fn wrap_session_key(
    session_key: &[u8; 32],
    sender_eph_priv: &StaticSecret,
    recipients: &[PeerId],
) -> Vec<WrappedKey> {
    let eph_pub = XPublicKey::from(sender_eph_priv).to_bytes();
    let mut out = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        let Some(r_x_bytes) = pubkey_ed_to_x(recipient) else {
            continue;
        };
        let Ok(mut kek) = derive_shared_key(
            sender_eph_priv,
            &r_x_bytes,
            &eph_pub,
            &r_x_bytes,
            WRAP_CONTEXT,
        ) else {
            continue;
        };
        let cipher = XChaCha20Poly1305::new((&kek).into());
        kek.zeroize();
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = match cipher.encrypt(XNonce::from_slice(&nonce), session_key.as_slice()) {
            Ok(ct) => ct,
            Err(_) => continue,
        };
        out.push(WrappedKey {
            recipient: *recipient,
            nonce,
            ciphertext: ct,
        });
    }
    out
}

pub fn unwrap_session_key(
    wrapped_keys: &[WrappedKey],
    sender_eph_pub: &[u8; 32],
    my_peer_id: &PeerId,
    my_x_secret: &StaticSecret,
) -> Result<[u8; 32], CryptoError> {
    let wk = wrapped_keys
        .iter()
        .find(|w| &w.recipient == my_peer_id)
        .ok_or(CryptoError::NoWrappedKeyForRecipient)?;

    let my_x_pub = XPublicKey::from(my_x_secret).to_bytes();
    let mut kek = derive_shared_key(
        my_x_secret,
        sender_eph_pub,
        sender_eph_pub,
        &my_x_pub,
        WRAP_CONTEXT,
    )?;
    let cipher = XChaCha20Poly1305::new((&kek).into());
    kek.zeroize();
    let mut pt = cipher
        .decrypt(XNonce::from_slice(&wk.nonce), wk.ciphertext.as_slice())
        .map_err(|_| CryptoError::DecryptFailed)?;
    if pt.len() != 32 {
        pt.zeroize();
        return Err(CryptoError::DecryptFailed);
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&pt);
    pt.zeroize();
    Ok(k)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedMessage {
    pub eph_pub: [u8; 32],
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

pub fn seal_to_peer(recipient: &PeerId, plaintext: &[u8]) -> Result<SealedMessage, CryptoError> {
    let r_x = pubkey_ed_to_x(recipient).ok_or(CryptoError::InvalidRecipientPubkey)?;
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    let mut key = derive_shared_key(&eph_priv, &r_x, &eph_pub, &r_x, SEAL_CONTEXT)?;
    let sealed = aead_encrypt(&key, plaintext);
    key.zeroize();
    let (nonce, ciphertext) = sealed?;
    Ok(SealedMessage {
        eph_pub,
        nonce,
        ciphertext,
    })
}

pub fn open_from_peer(my_key: &SigningKey, sealed: &SealedMessage) -> Result<Vec<u8>, CryptoError> {
    let my_x = signing_key_ed_to_x_secret(my_key);
    let my_x_pub = XPublicKey::from(&my_x).to_bytes();
    let mut key = derive_shared_key(
        &my_x,
        &sealed.eph_pub,
        &sealed.eph_pub,
        &my_x_pub,
        SEAL_CONTEXT,
    )?;
    let opened = aead_decrypt(&key, &sealed.nonce, &sealed.ciphertext);
    key.zeroize();
    opened
}

pub fn aead_encrypt(
    session_key: &[u8; 32],
    payload: &[u8],
) -> Result<([u8; 24], Vec<u8>), CryptoError> {
    let cipher = XChaCha20Poly1305::new(session_key.into());
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), payload)
        .map_err(|_| CryptoError::EncryptFailed)?;
    Ok((nonce, ct))
}

pub fn aead_decrypt(
    session_key: &[u8; 32],
    nonce: &[u8; 24],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(session_key.into());
    cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError::DecryptFailed)
}

pub fn generate_ephemeral_keypair() -> (StaticSecret, [u8; 32]) {
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let secret = StaticSecret::from(seed);
    let public = XPublicKey::from(&secret);
    (secret, public.to_bytes())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBundle {
    pub version: u8,
    pub sender_eph_pub: [u8; 32],
    pub wrapped_keys: Vec<WrappedKey>,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

impl EncryptedBundle {
    pub fn encrypt(plaintext: &[u8], recipients: &[PeerId]) -> Result<Self, CryptoError> {
        if recipients.is_empty() {
            return Err(CryptoError::InvalidRecipientPubkey);
        }
        let mut session_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut session_key);

        let (eph_priv, eph_pub) = generate_ephemeral_keypair();
        let wrapped = wrap_session_key(&session_key, &eph_priv, recipients);
        drop(eph_priv);

        let sealed = aead_encrypt(&session_key, plaintext);
        session_key.zeroize();
        let (nonce, ciphertext) = sealed?;
        if wrapped.is_empty() {
            return Err(CryptoError::InvalidRecipientPubkey);
        }

        Ok(Self {
            version: 2,
            sender_eph_pub: eph_pub,
            wrapped_keys: wrapped,
            nonce,
            ciphertext,
        })
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/bundle-digest/v1");
        h.update(self.to_bytes());
        h.finalize().into()
    }

    pub fn decrypt(
        &self,
        my_peer_id: &PeerId,
        my_signing_key: &SigningKey,
    ) -> Result<Vec<u8>, CryptoError> {
        let my_x_secret = signing_key_ed_to_x_secret(my_signing_key);
        let k = unwrap_session_key(
            &self.wrapped_keys,
            &self.sender_eph_pub,
            my_peer_id,
            &my_x_secret,
        )?;
        let mut k = k;
        let pt = aead_decrypt(&k, &self.nonce, &self.ciphertext);
        k.zeroize();
        pt
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        crate::wire::encode(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let b: Self = crate::wire::decode(bytes)?;
        (b.version == 2).then_some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn fresh_wallet() -> (SigningKey, PeerId) {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let sk = SigningKey::from_bytes(&seed);
        let pid: PeerId = sk.verifying_key().to_bytes();
        (sk, pid)
    }

    #[test]
    fn ed_to_x_pubkey_matches_signing_key_derivation() {
        let (sk, pid) = fresh_wallet();
        let via_pid = pubkey_ed_to_x(&pid).expect("pubkey decodes");
        let via_sk = signing_key_ed_to_x_public(&sk);
        assert_eq!(via_pid, via_sk);
    }

    #[test]
    fn dh_from_ed_keys_matches_both_sides() {
        let (sk_a, pid_a) = fresh_wallet();
        let (sk_b, pid_b) = fresh_wallet();

        let a_x_priv = signing_key_ed_to_x_secret(&sk_a);
        let b_x_priv = signing_key_ed_to_x_secret(&sk_b);
        let a_x_pub = XPublicKey::from(pubkey_ed_to_x(&pid_a).unwrap());
        let b_x_pub = XPublicKey::from(pubkey_ed_to_x(&pid_b).unwrap());

        let shared_a = a_x_priv.diffie_hellman(&b_x_pub);
        let shared_b = b_x_priv.diffie_hellman(&a_x_pub);
        assert_eq!(shared_a.as_bytes(), shared_b.as_bytes());
    }

    #[test]
    fn wrap_then_unwrap_returns_same_session_key() {
        let (alice_sk, alice_pid) = fresh_wallet();
        let (sender_eph_priv, sender_eph_pub) = generate_ephemeral_keypair();
        let mut session_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut session_key);

        let wrapped = wrap_session_key(&session_key, &sender_eph_priv, &[alice_pid]);
        let alice_x_secret = signing_key_ed_to_x_secret(&alice_sk);
        let unwrapped = unwrap_session_key(&wrapped, &sender_eph_pub, &alice_pid, &alice_x_secret)
            .expect("unwrap succeeds");
        assert_eq!(session_key, unwrapped);
    }

    #[test]
    fn wrong_recipient_cannot_unwrap() {
        let (alice_sk, alice_pid) = fresh_wallet();
        let (eve_sk, eve_pid) = fresh_wallet();
        let (sender_eph_priv, sender_eph_pub) = generate_ephemeral_keypair();
        let mut session_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut session_key);
        let wrapped = wrap_session_key(&session_key, &sender_eph_priv, &[alice_pid]);

        let result = unwrap_session_key(
            &wrapped,
            &sender_eph_pub,
            &eve_pid,
            &signing_key_ed_to_x_secret(&eve_sk),
        );
        assert_eq!(result, Err(CryptoError::NoWrappedKeyForRecipient));

        let mut tampered = wrapped.clone();
        tampered[0].recipient = eve_pid;
        let tampered_result = unwrap_session_key(
            &tampered,
            &sender_eph_pub,
            &eve_pid,
            &signing_key_ed_to_x_secret(&eve_sk),
        );
        assert_eq!(tampered_result, Err(CryptoError::DecryptFailed));

        let _ = alice_sk;
    }

    #[test]
    fn aead_roundtrip_arbitrary_payload() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let payload = b"hola mundo \xf0\x9f\x91\x8b";
        let (nonce, ct) = aead_encrypt(&key, payload).unwrap();
        let pt = aead_decrypt(&key, &nonce, &ct).unwrap();
        assert_eq!(pt, payload);
    }

    #[test]
    fn aead_tampered_ciphertext_rejected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let payload = b"sensitive message";
        let (nonce, mut ct) = aead_encrypt(&key, payload).unwrap();
        ct[0] ^= 0xFF;
        let result = aead_decrypt(&key, &nonce, &ct);
        assert_eq!(result, Err(CryptoError::DecryptFailed));
    }

    #[test]
    fn multi_recipient_each_winner_decrypts_only_their_entry() {
        let (sk1, pid1) = fresh_wallet();
        let (sk2, pid2) = fresh_wallet();
        let (sk3, pid3) = fresh_wallet();
        let (eph_priv, eph_pub) = generate_ephemeral_keypair();
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        let wrapped = wrap_session_key(&k, &eph_priv, &[pid1, pid2, pid3]);
        assert_eq!(wrapped.len(), 3);

        for (pid, sk) in [(pid1, &sk1), (pid2, &sk2), (pid3, &sk3)] {
            let secret = signing_key_ed_to_x_secret(sk);
            let recovered = unwrap_session_key(&wrapped, &eph_pub, &pid, &secret).unwrap();
            assert_eq!(recovered, k, "each recipient recovers the same K");
        }
    }

    #[test]
    fn encrypted_bundle_roundtrip_through_any_recipient() {
        let (sk1, pid1) = fresh_wallet();
        let (sk2, pid2) = fresh_wallet();
        let (sk3, pid3) = fresh_wallet();
        let plaintext = b"hola agente \xc2\xa1qu\xc3\xa9 tal!";

        let bundle = EncryptedBundle::encrypt(plaintext, &[pid1, pid2, pid3]).unwrap();

        for (pid, sk) in [(pid1, &sk1), (pid2, &sk2), (pid3, &sk3)] {
            let pt = bundle.decrypt(&pid, sk).unwrap();
            assert_eq!(pt, plaintext);
        }
    }

    #[test]
    fn encrypted_bundle_outsider_cannot_decrypt() {
        let (_sk_winner, winner_pid) = fresh_wallet();
        let (eve_sk, eve_pid) = fresh_wallet();
        let bundle = EncryptedBundle::encrypt(b"secret", &[winner_pid]).unwrap();

        let result = bundle.decrypt(&eve_pid, &eve_sk);
        assert_eq!(result, Err(CryptoError::NoWrappedKeyForRecipient));
    }

    #[test]
    fn encrypted_bundle_serde_roundtrip() {
        let (sk, pid) = fresh_wallet();
        let plaintext = b"shamir-bound payload";
        let bundle = EncryptedBundle::encrypt(plaintext, &[pid]).unwrap();

        let bytes = bundle.to_bytes();
        let parsed = EncryptedBundle::from_bytes(&bytes).expect("decodes");
        let pt = parsed.decrypt(&pid, &sk).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn sealed_messages_open_only_for_their_recipient() {
        let (sk, pid) = fresh_wallet();
        let (eve, _) = fresh_wallet();
        let sealed = seal_to_peer(&pid, b"reply").unwrap();
        assert_eq!(open_from_peer(&sk, &sealed).unwrap(), b"reply");
        assert!(open_from_peer(&eve, &sealed).is_err());
    }

    #[test]
    fn a_low_order_ephemeral_key_is_refused() {
        let (sk, pid) = fresh_wallet();
        let bundle = EncryptedBundle::encrypt(b"x", &[pid]).unwrap();
        let mut forged = bundle.clone();
        forged.sender_eph_pub = [0u8; 32];
        assert!(forged.decrypt(&pid, &sk).is_err());
    }

    #[test]
    fn binary_encoding_is_compact() {
        let (_, pid) = fresh_wallet();
        let payload = vec![0xABu8; 10_000];
        let bundle = EncryptedBundle::encrypt(&payload, &[pid]).unwrap();
        assert!(bundle.to_bytes().len() < 10_300);
    }

    #[test]
    fn encrypted_bundle_tampered_ciphertext_rejected() {
        let (sk, pid) = fresh_wallet();
        let mut bundle = EncryptedBundle::encrypt(b"truth", &[pid]).unwrap();
        if let Some(b) = bundle.ciphertext.iter_mut().next() {
            *b ^= 0xFF;
        }
        let result = bundle.decrypt(&pid, &sk);
        assert_eq!(result, Err(CryptoError::DecryptFailed));
    }
}
