// SPDX-License-Identifier: BUSL-1.1
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signer, SigningKey};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use core_crypto::identity::PeerId;
use core_crypto::network::{domains, signing_digest};

const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_GROUPS: usize = 4;
const CODE_GROUP_LEN: usize = 4;

/// How the assistant signs in a room. Only shown: `is_ai` is what marks an
/// answer, so members on older versions still tell it apart.
pub const AI_CODENAME: &str = "NAI";
pub const MAX_ROOM_TEXT_BYTES: usize = 32 * 1024;
pub const ROOM_FLOOD_TTL: u8 = 6;
pub const ROOM_MAX_CLOCK_SKEW_MS: u64 = 10 * 60 * 1_000;

pub const ANIMAL_CODENAMES: &[&str] = &[
    "Fox",
    "Wolf",
    "Bear",
    "Owl",
    "Tiger",
    "Lynx",
    "Hawk",
    "Otter",
    "Raven",
    "Stoat",
    "Falcon",
    "Badger",
    "Heron",
    "Marten",
    "Eagle",
    "Mole",
    "Beaver",
    "Jackal",
    "Mantis",
    "Crane",
    "Cobra",
    "Lemur",
    "Panda",
    "Boar",
    "Stork",
    "Hyena",
    "Coyote",
    "Bison",
    "Civet",
    "Ibis",
    "Macaw",
    "Caracal",
    "Tapir",
    "Quokka",
    "Kestrel",
    "Vulture",
    "Gerbil",
    "Numbat",
    "Wombat",
    "Genet",
    "Okapi",
    "Ibex",
    "Dingo",
    "Vicuna",
    "Capybara",
    "Pangolin",
    "Manatee",
    "Marmot",
    "Nyala",
    "Saiga",
    "Serval",
    "Tegu",
    "Uakari",
    "Viper",
    "Wallaby",
    "Xerus",
    "Yapok",
    "Zebra",
    "Aardvark",
    "Bushbaby",
    "Cassowary",
    "Dugong",
    "Echidna",
    "Fennec",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomCode(pub String);

impl RoomCode {
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let mut buf = String::with_capacity(CODE_GROUPS * (CODE_GROUP_LEN + 1));
        for g in 0..CODE_GROUPS {
            if g > 0 {
                buf.push('-');
            }
            for _ in 0..CODE_GROUP_LEN {
                let idx = rng.gen_range(0..CODE_ALPHABET.len());
                buf.push(CODE_ALPHABET[idx] as char);
            }
        }
        Self(buf)
    }

    pub fn parse(input: &str) -> Result<Self, String> {
        let stripped: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let want = CODE_GROUPS * CODE_GROUP_LEN;
        if stripped.len() != want {
            return Err(format!(
                "Room code must be {want} characters (got {})",
                stripped.len()
            ));
        }
        if let Some(c) = stripped
            .chars()
            .find(|c| !CODE_ALPHABET.contains(&(*c as u8)))
        {
            return Err(format!("Invalid character in room code: {c}"));
        }
        let groups: Vec<&str> = (0..CODE_GROUPS)
            .map(|g| &stripped[g * CODE_GROUP_LEN..(g + 1) * CODE_GROUP_LEN])
            .collect();
        Ok(Self(groups.join("-")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn keys(&self) -> RoomKeys {
        RoomKeys::derive(self)
    }
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RoomKeys {
    pub room_id: [u8; 32],
    key: [u8; 32],
}

impl std::fmt::Debug for RoomKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomKeys")
            .field("room_id", &hex::encode(&self.room_id[..8]))
            .finish_non_exhaustive()
    }
}

impl RoomKeys {
    fn derive(code: &RoomCode) -> Self {
        let chain_id = core_crypto::network::active().chain_id();
        let mut id = Sha256::new();
        id.update(b"nepheral/room-id/v1");
        id.update(chain_id);
        id.update(code.as_str().as_bytes());
        let mut k = Sha256::new();
        k.update(b"nepheral/room-key/v1");
        k.update(chain_id);
        k.update(code.as_str().as_bytes());
        Self {
            room_id: id.finalize().into(),
            key: k.finalize().into(),
        }
    }
}

pub fn animal_for(peer_id: &PeerId, room: &RoomCode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nepheral/room-codename/v1");
    hasher.update(peer_id);
    hasher.update(room.as_str().as_bytes());
    let digest = hasher.finalize();
    let mut idx_bytes = [0u8; 8];
    idx_bytes.copy_from_slice(&digest[..8]);
    let n = u64::from_le_bytes(idx_bytes);
    let animal = ANIMAL_CODENAMES[(n % ANIMAL_CODENAMES.len() as u64) as usize];
    let suffix = (n / ANIMAL_CODENAMES.len() as u64) % 100;
    format!("{animal}-{suffix:02}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomPayload {
    Join { contributes: bool },
    Presence { contributes: bool },
    Leave,
    Message { text: String, is_ai: bool },
}

impl RoomPayload {
    fn is_bounded(&self) -> bool {
        match self {
            RoomPayload::Message { text, .. } => text.len() <= MAX_ROOM_TEXT_BYTES,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomEnvelope {
    pub version: u8,
    pub room_id: [u8; 32],
    pub sender_pid: PeerId,
    pub msg_id: [u8; 16],
    pub sent_at_ms: u64,
    pub ttl: u8,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl RoomEnvelope {
    pub const VERSION: u8 = 1;

    fn aad(room_id: &[u8; 32], sender: &PeerId, msg_id: &[u8; 16], sent_at_ms: u64) -> Vec<u8> {
        let mut aad = Vec::with_capacity(88);
        aad.extend_from_slice(room_id);
        aad.extend_from_slice(sender);
        aad.extend_from_slice(msg_id);
        aad.extend_from_slice(&sent_at_ms.to_be_bytes());
        aad
    }

    fn signing_digest(&self) -> [u8; 32] {
        signing_digest(
            domains::ROOM_ENVELOPE,
            &[
                &[self.version],
                &self.room_id,
                &self.sender_pid,
                &self.msg_id,
                &self.sent_at_ms.to_be_bytes(),
                &self.nonce,
                &(self.ciphertext.len() as u32).to_be_bytes(),
                &self.ciphertext,
            ],
        )
    }

    pub fn seal(
        keys: &RoomKeys,
        signer: &SigningKey,
        payload: &RoomPayload,
        sent_at_ms: u64,
    ) -> Result<Self, String> {
        if !payload.is_bounded() {
            return Err("room message too long".to_string());
        }
        let sender_pid = signer.verifying_key().to_bytes();
        let mut msg_id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut msg_id);
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut plaintext = core_crypto::wire::encode(payload);
        let cipher = XChaCha20Poly1305::new((&keys.key).into());
        let aad = Self::aad(&keys.room_id, &sender_pid, &msg_id, sent_at_ms);
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| "room encryption failed".to_string());
        plaintext.zeroize();
        let mut env = RoomEnvelope {
            version: Self::VERSION,
            room_id: keys.room_id,
            sender_pid,
            msg_id,
            sent_at_ms,
            ttl: ROOM_FLOOD_TTL,
            nonce,
            ciphertext: ciphertext?,
            signature: [0u8; 64],
        };
        env.signature = signer.sign(&env.signing_digest()).to_bytes();
        Ok(env)
    }

    pub fn verify(&self, now_ms: u64) -> Result<(), String> {
        if self.version != Self::VERSION {
            return Err(format!(
                "unsupported room envelope version {}",
                self.version
            ));
        }
        if self.ttl > ROOM_FLOOD_TTL {
            return Err("room envelope TTL out of range".to_string());
        }
        if self.sent_at_ms.abs_diff(now_ms) > ROOM_MAX_CLOCK_SKEW_MS {
            return Err("room envelope is stale or from the future".to_string());
        }
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&self.sender_pid)
            .map_err(|_| "room sender is not an ed25519 key".to_string())?;
        vk.verify_strict(
            &self.signing_digest(),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "room envelope signature invalid".to_string())
    }

    pub fn open(&self, keys: &RoomKeys) -> Result<RoomPayload, String> {
        if self.room_id != keys.room_id {
            return Err("envelope belongs to another room".to_string());
        }
        let cipher = XChaCha20Poly1305::new((&keys.key).into());
        let aad = Self::aad(
            &self.room_id,
            &self.sender_pid,
            &self.msg_id,
            self.sent_at_ms,
        );
        let mut plaintext = cipher
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| "room envelope did not decrypt".to_string())?;
        let payload: Option<RoomPayload> = core_crypto::wire::decode(&plaintext);
        plaintext.zeroize();
        let payload = payload.ok_or_else(|| "room payload malformed".to_string())?;
        if !payload.is_bounded() {
            return Err("room payload too long".to_string());
        }
        Ok(payload)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

/// Whether a message calls the assistant: `@NAI`, or the handles older
/// versions taught (`@agente`, `@nepheral`), in any case.
pub fn mentions_agent(content: &str) -> bool {
    const HANDLES: [&str; 3] = ["@nai", "@agente", "@nepheral"];
    let lower = content.to_lowercase();
    let bytes = lower.as_bytes();
    for handle in HANDLES {
        let mut from = 0usize;
        while let Some(pos) = lower[from..].find(handle) {
            let start = from + pos;
            let end = start + handle.len();
            let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
            let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
            if before_ok && after_ok {
                return true;
            }
            from = end;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'@' || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }

    #[test]
    fn codes_are_four_groups_of_four() {
        let code = RoomCode::generate();
        assert_eq!(code.as_str().len(), 19);
        assert_eq!(code.as_str().matches('-').count(), 3);
        assert_eq!(RoomCode::parse(code.as_str()).unwrap(), code);
    }

    #[test]
    fn parse_normalises_case_spacing_and_dashes() {
        let code = RoomCode::generate();
        let messy = code.as_str().to_lowercase().replace('-', " ");
        assert_eq!(RoomCode::parse(&messy).unwrap(), code);
        assert!(RoomCode::parse("ABCD-EFGH").is_err());
        assert!(RoomCode::parse("ABCD-EFGH-JKMN-PQR0").is_err());
    }

    #[test]
    fn members_read_and_outsiders_do_not() {
        let code = RoomCode::generate();
        let keys = code.keys();
        let env = RoomEnvelope::seal(
            &keys,
            &signer(1),
            &RoomPayload::Message {
                text: "hola".into(),
                is_ai: false,
            },
            1_000,
        )
        .unwrap();
        let wire = RoomEnvelope::from_bytes(&env.to_bytes()).unwrap();
        assert!(wire.verify(1_000).is_ok());
        assert_eq!(
            wire.open(&keys).unwrap(),
            RoomPayload::Message {
                text: "hola".into(),
                is_ai: false
            }
        );
        let other = RoomCode::generate().keys();
        assert!(wire.open(&other).is_err());
    }

    #[test]
    fn a_relabelled_sender_breaks_the_signature() {
        let keys = RoomCode::generate().keys();
        let mut env = RoomEnvelope::seal(&keys, &signer(1), &RoomPayload::Leave, 5_000).unwrap();
        env.sender_pid = signer(2).verifying_key().to_bytes();
        assert!(env.verify(5_000).is_err());
    }

    #[test]
    fn stale_envelopes_are_refused() {
        let keys = RoomCode::generate().keys();
        let env = RoomEnvelope::seal(&keys, &signer(1), &RoomPayload::Leave, 0).unwrap();
        assert!(env.verify(ROOM_MAX_CLOCK_SKEW_MS * 2).is_err());
    }

    #[test]
    fn codenames_are_stable_and_suffixed() {
        let code = RoomCode::generate();
        let a = animal_for(&[1u8; 32], &code);
        assert_eq!(a, animal_for(&[1u8; 32], &code));
        assert!(a.contains('-'));
    }

    #[test]
    fn mentions_need_a_word_boundary() {
        assert!(mentions_agent("@NAI, resume la conversación"));
        assert!(mentions_agent("qué opinas @nai?"));
        assert!(!mentions_agent("@naive question"));
        assert!(!mentions_agent("mail@nai.com"));
        assert!(mentions_agent("hey @agente what do you think"));
        assert!(mentions_agent("@Nepheral, resume esto"));
        assert!(mentions_agent("dime algo @AGENTE"));
        assert!(!mentions_agent("mail@agentemail.com"));
        assert!(!mentions_agent("write to foo@agente.io"));
        assert!(!mentions_agent("@agentes plural"));
        assert!(!mentions_agent("no mention here"));
    }
}
