// SPDX-License-Identifier: BUSL-1.1

use serde::{de::DeserializeOwned, Serialize};

pub const MAX_DECODE_BYTES: usize = 8 * 1024 * 1024;

pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).unwrap_or_default()
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_DECODE_BYTES {
        return None;
    }
    postcard::from_bytes(bytes).ok()
}

pub mod bytes64 {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&hex::encode(v))
        } else {
            s.serialize_bytes(v)
        }
    }

    struct Bytes64Visitor;

    impl<'de> Visitor<'de> for Bytes64Visitor {
        type Value = [u8; 64];

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("64 bytes or 128 hex characters")
        }

        fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
            let b = hex::decode(v).map_err(E::custom)?;
            b.try_into().map_err(|_| E::custom("expected 64 bytes"))
        }

        fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            v.try_into().map_err(|_| E::custom("expected 64 bytes"))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = [0u8; 64];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(i, &self))?;
            }
            Ok(out)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        if d.is_human_readable() {
            d.deserialize_str(Bytes64Visitor)
        } else {
            d.deserialize_bytes(Bytes64Visitor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        data: Vec<u8>,
        #[serde(with = "bytes64")]
        sig: [u8; 64],
    }

    #[test]
    fn roundtrips_in_binary_and_json() {
        let s = Sample {
            data: vec![1, 2, 3],
            sig: [7u8; 64],
        };
        let bin = encode(&s);
        assert_eq!(decode::<Sample>(&bin).unwrap(), s);
        let json = serde_json::to_vec(&s).unwrap();
        assert_eq!(serde_json::from_slice::<Sample>(&json).unwrap(), s);
    }

    #[test]
    fn garbage_is_rejected_not_panicking() {
        assert!(decode::<Sample>(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).is_none());
    }
}
