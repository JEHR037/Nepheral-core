// SPDX-License-Identifier: BUSL-1.1
//! What a payer wrote into an off-chain payment about where the credits go.
//!
//! This lives in `core-crypto` because both ends need it and neither may
//! guess: a client builds the payload to show a buyer, and the payment watcher
//! parses it back. A disagreement between the two would send credits to the
//! wrong account, so the encoding is defined once, here.
//!
//! Litecoin carries no sender identity the network can trust, so the payment
//! itself has to say who it is for. An `OP_RETURN` output is how: it costs a
//! few bytes, every explorer can see it, and — the part that matters — it is
//! signed by whoever made the payment, so no seller, explorer or process can
//! rewrite it afterwards.
//!
//! Two shapes are in use, told apart by length alone. A discriminant byte
//! would have to be trusted to be honest about the bytes that follow it; a
//! length cannot lie about itself.
//!
//! | Bytes | Meaning |
//! | --- | --- |
//! | 32 | Direct sale: credit this wallet. |
//! | 40 | Market fill: credit this buyer for this listing. |

const MAX_SCRIPT_LEN: usize = 80;

/// The direct-sale payload: the wallet to credit.
pub const WALLET_LEN: usize = 32;

/// The market payload: the buyer, then the listing as a big-endian u64.
pub const FILL_LEN: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Memo {
    /// Buying credits from the network itself.
    Wallet([u8; 32]),
    /// Paying a seller for a specific listing on the board.
    Fill { buyer: [u8; 32], ask_id: u64 },
}

impl Memo {
    /// Who the credits belong to, whichever kind of payment this is.
    pub fn receiver(&self) -> [u8; 32] {
        match self {
            Self::Wallet(w) => *w,
            Self::Fill { buyer, .. } => *buyer,
        }
    }
}

/// Builds the payload a buyer must attach to pay for a listing. Kept beside
/// the parser so the two cannot drift apart.
pub fn encode_fill(buyer: &[u8; 32], ask_id: u64) -> [u8; FILL_LEN] {
    let mut out = [0u8; FILL_LEN];
    out[..32].copy_from_slice(buyer);
    out[32..].copy_from_slice(&ask_id.to_be_bytes());
    out
}

pub fn extract(script_hex: &str) -> Option<Memo> {
    let bytes = hex::decode(script_hex.trim()).ok()?;
    extract_from_script_bytes(&bytes)
}

/// Kept for the direct-sale path, which has no use for a listing id.
pub fn extract_wallet_root(script_hex: &str) -> Option<[u8; 32]> {
    match extract(script_hex)? {
        Memo::Wallet(w) => Some(w),
        Memo::Fill { .. } => None,
    }
}

pub fn extract_from_script_bytes(script: &[u8]) -> Option<Memo> {
    let payload = op_return_payload(script)?;
    match payload.len() {
        WALLET_LEN => {
            let mut w = [0u8; 32];
            w.copy_from_slice(payload);
            Some(Memo::Wallet(w))
        }
        FILL_LEN => {
            let mut buyer = [0u8; 32];
            buyer.copy_from_slice(&payload[..32]);
            let ask_id = u64::from_be_bytes(payload[32..].try_into().ok()?);
            Some(Memo::Fill { buyer, ask_id })
        }
        // Any other length is not a payload this network wrote. Guessing at
        // what it meant would risk crediting the wrong account.
        _ => None,
    }
}

/// Returns the pushed data of a bare `OP_RETURN` output.
///
/// Only the two minimal push encodings wallets actually emit are accepted, and
/// nothing may follow the push: a script with trailing bytes is not the script
/// we think we are reading.
fn op_return_payload(script: &[u8]) -> Option<&[u8]> {
    if script.is_empty() || script.len() > MAX_SCRIPT_LEN {
        return None;
    }
    if script[0] != 0x6a {
        return None;
    }
    // Direct push: [OP_RETURN][len][data], for lengths under 0x4c.
    if script.len() >= 2 {
        let len = script[1] as usize;
        if len < 0x4c && script.len() == 2 + len {
            return Some(&script[2..]);
        }
    }
    // PUSHDATA1: [OP_RETURN][0x4c][len][data].
    if script.len() >= 3 && script[1] == 0x4c {
        let len = script[2] as usize;
        if script.len() == 3 + len {
            return Some(&script[3..]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet() -> [u8; 32] {
        let mut r = [0u8; 32];
        for (i, b) in r.iter_mut().enumerate() {
            *b = i as u8;
        }
        r
    }

    fn direct_push(payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0x6a, payload.len() as u8];
        s.extend_from_slice(payload);
        s
    }

    fn pushdata1(payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0x6a, 0x4c, payload.len() as u8];
        s.extend_from_slice(payload);
        s
    }

    #[test]
    fn a_wallet_payload_is_a_direct_sale() {
        let script = direct_push(&wallet());
        assert_eq!(
            extract_from_script_bytes(&script),
            Some(Memo::Wallet(wallet()))
        );
    }

    #[test]
    fn a_forty_byte_payload_names_a_listing() {
        let script = direct_push(&encode_fill(&wallet(), 7));
        assert_eq!(
            extract_from_script_bytes(&script),
            Some(Memo::Fill {
                buyer: wallet(),
                ask_id: 7
            })
        );
    }

    #[test]
    fn the_two_shapes_are_told_apart_by_length_alone() {
        // The same buyer bytes appear in both; only the length distinguishes
        // "credit this wallet" from "credit this wallet for listing N".
        let w = extract_from_script_bytes(&direct_push(&wallet())).unwrap();
        let f = extract_from_script_bytes(&direct_push(&encode_fill(&wallet(), 0))).unwrap();
        assert_ne!(w, f);
        assert_eq!(w.receiver(), f.receiver());
    }

    #[test]
    fn a_listing_id_survives_the_round_trip() {
        for id in [0u64, 1, 255, 256, u32::MAX as u64, u64::MAX] {
            let script = direct_push(&encode_fill(&wallet(), id));
            match extract_from_script_bytes(&script) {
                Some(Memo::Fill { ask_id, .. }) => assert_eq!(ask_id, id),
                other => panic!("expected a fill for {id}, got {other:?}"),
            }
        }
    }

    #[test]
    fn both_push_encodings_are_accepted() {
        assert!(extract_from_script_bytes(&pushdata1(&wallet())).is_some());
        assert!(extract_from_script_bytes(&pushdata1(&encode_fill(&wallet(), 3))).is_some());
    }

    #[test]
    fn the_hex_form_is_decoded_and_trimmed() {
        let hex_form = hex::encode(direct_push(&wallet()));
        assert_eq!(extract(&hex_form), Some(Memo::Wallet(wallet())));
        assert_eq!(
            extract(&format!("  {hex_form}\n")),
            Some(Memo::Wallet(wallet()))
        );
    }

    #[test]
    fn extract_wallet_root_ignores_market_payments() {
        // The direct-sale path must not read a market payment as a purchase
        // from the network, or it would mint credits nobody sold.
        let fill = hex::encode(direct_push(&encode_fill(&wallet(), 1)));
        assert_eq!(extract_wallet_root(&fill), None);
        let sale = hex::encode(direct_push(&wallet()));
        assert_eq!(extract_wallet_root(&sale), Some(wallet()));
    }

    #[test]
    fn a_non_op_return_script_is_rejected() {
        assert_eq!(extract_from_script_bytes(&[0x76, 0xa9, 0x14]), None);
    }

    #[test]
    fn an_unrecognised_length_is_rejected() {
        for len in [1usize, 16, 31, 33, 39, 41, 64] {
            let script = direct_push(&vec![0u8; len]);
            assert_eq!(
                extract_from_script_bytes(&script),
                None,
                "{len} bytes should not parse"
            );
        }
    }

    #[test]
    fn trailing_bytes_invalidate_the_script() {
        let mut script = direct_push(&wallet());
        script.push(0xFF);
        assert_eq!(extract_from_script_bytes(&script), None);
    }

    #[test]
    fn empty_and_oversized_scripts_are_rejected() {
        assert_eq!(extract_from_script_bytes(&[]), None);
        assert_eq!(extract_from_script_bytes(&[0x6a; 200]), None);
    }

    #[test]
    fn malformed_hex_returns_none() {
        assert_eq!(extract("zz not hex"), None);
        assert_eq!(extract("6a2"), None);
    }
}
