// SPDX-License-Identifier: BUSL-1.1
pub mod agent_crypto;
pub mod block;
pub mod chain;
pub mod chain_file;
pub mod extrinsic;
pub mod finality;
pub mod identity;
pub mod light;
pub mod lottery;
pub mod mnemonic;
pub mod network;
pub mod pallets;
pub mod payment_memo;
pub mod receipts;
pub mod validators;
pub mod wire;
pub use block::{
    Block, BlockBody, BlockHash, BlockHeader, LotteryResult, EARLY_BLOCK_EXT_THRESHOLD,
    EARLY_BLOCK_LOTTERY_THRESHOLD, SLOT_DRIFT_TOLERANCE, SLOT_DURATION_MS,
};
pub use chain::{team_pubkey, ChainError, ChainState, RedeemedCounts};
pub use extrinsic::{
    DecodedCall, Extrinsic, ExtrinsicError, PALLET_BALANCES, PALLET_CONTRIBUTIONS, PALLET_NAI,
    PALLET_SYSTEM,
};
pub use identity::{peer_id_from_cert, NodeIdentity, PeerId};
pub use lottery::{current_hour_epoch, lottery_select, PI_BITS};
pub use mnemonic::NodeMnemonic;
pub use network::Network;
pub use validators::{authority_peer_id, ValidatorError, ValidatorSet};

use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct ShamirShare {
    pub index: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShamirConfig {
    pub total_shares: u8,
    pub threshold: u8,
}

impl ShamirConfig {
    pub fn try_new(total_shares: u8, threshold: u8) -> Result<Self, String> {
        if total_shares == 0 {
            return Err("total_shares must be >= 1".to_string());
        }
        if threshold == 0 {
            return Err("threshold must be >= 1".to_string());
        }
        if threshold > total_shares {
            return Err(format!(
                "threshold {threshold} exceeds total_shares {total_shares}"
            ));
        }
        Ok(Self {
            total_shares,
            threshold,
        })
    }

    pub fn dynamic_for(n_peers: usize) -> Self {
        let n = n_peers.clamp(1, 255) as u8;
        let threshold = if n >= 2 {
            ((n as u16).div_ceil(2) as u8).max(2)
        } else {
            1
        };
        Self {
            total_shares: n,
            threshold,
        }
    }
}

struct Gf256Tables {
    exp: [u8; 512],
    log: [u8; 256],
}

const fn build_gf256_tables() -> Gf256Tables {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0usize;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        let mut next = x << 1;
        if next & 0x100 != 0 {
            next ^= 0x11B;
        }
        next ^= x;
        x = next & 0xFF;
        i += 1;
    }
    let mut j = 255usize;
    while j < 512 {
        exp[j] = exp[j - 255];
        j += 1;
    }
    Gf256Tables { exp, log }
}

static GF256: Gf256Tables = build_gf256_tables();

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    GF256.exp[GF256.log[a as usize] as usize + GF256.log[b as usize] as usize]
}

fn gf_inv(a: u8) -> Option<u8> {
    if a == 0 {
        return None;
    }
    Some(GF256.exp[255 - GF256.log[a as usize] as usize])
}

pub struct ShamirSSS {
    config: ShamirConfig,
}

impl ShamirSSS {
    pub fn new(config: ShamirConfig) -> Self {
        Self { config }
    }

    pub fn split_secret(&self, secret: &[u8]) -> Result<Vec<ShamirShare>, String> {
        if secret.is_empty() {
            return Err("Secret cannot be empty".to_string());
        }
        let n = self.config.total_shares as usize;
        let k = self.config.threshold as usize;
        let mut rng = rand::thread_rng();
        let mut shares: Vec<ShamirShare> = (1..=self.config.total_shares)
            .map(|index| ShamirShare {
                index,
                value: Vec::with_capacity(secret.len()),
            })
            .collect();
        let mut coefficients = vec![0u8; k];
        for &secret_byte in secret {
            coefficients[0] = secret_byte;
            rng.fill_bytes(&mut coefficients[1..]);
            for share in shares.iter_mut().take(n) {
                let x = share.index;
                let mut y = 0u8;
                for &c in coefficients.iter().rev() {
                    y = gf_mul(y, x) ^ c;
                }
                share.value.push(y);
            }
        }
        coefficients.zeroize();
        Ok(shares)
    }

    pub fn reconstruct_secret(&self, shares: &[ShamirShare]) -> Result<Vec<u8>, String> {
        let k = self.config.threshold as usize;
        if shares.len() < k {
            return Err(format!(
                "Insufficient shares: need {}, got {}",
                k,
                shares.len()
            ));
        }
        let selected = &shares[..k];
        let secret_len = selected[0].value.len();
        let mut seen = [false; 256];
        for share in selected {
            if share.index == 0 {
                return Err("Share index 0 is invalid".to_string());
            }
            if seen[share.index as usize] {
                return Err(format!("Duplicate share index {}", share.index));
            }
            seen[share.index as usize] = true;
            if share.value.len() != secret_len {
                return Err("Inconsistent share lengths".to_string());
            }
        }

        let mut basis = Vec::with_capacity(k);
        for (i, si) in selected.iter().enumerate() {
            let mut num = 1u8;
            let mut den = 1u8;
            for (j, sj) in selected.iter().enumerate() {
                if i != j {
                    num = gf_mul(num, sj.index);
                    den = gf_mul(den, si.index ^ sj.index);
                }
            }
            let inv = gf_inv(den).ok_or_else(|| "Degenerate share set".to_string())?;
            basis.push(gf_mul(num, inv));
        }

        let mut secret = vec![0u8; secret_len];
        for (share, &b) in selected.iter().zip(basis.iter()) {
            for (out, &y) in secret.iter_mut().zip(share.value.iter()) {
                *out ^= gf_mul(y, b);
            }
        }
        Ok(secret)
    }

    pub fn fingerprint_share(share: &ShamirShare) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update([share.index]);
        hasher.update(&share.value);
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_and_reconstruct() {
        let config = ShamirConfig::try_new(5, 3).unwrap();
        let shamir = ShamirSSS::new(config);
        let secret = b"SensitivePayload";

        let shares = shamir.split_secret(secret).expect("Failed to split secret");
        assert_eq!(shares.len(), 5);

        let selected_shares = &shares[..3];
        let reconstructed = shamir
            .reconstruct_secret(selected_shares)
            .expect("Failed to reconstruct");

        assert_eq!(reconstructed, secret.to_vec());
    }

    #[test]
    fn test_insufficient_shares_fails() {
        let config = ShamirConfig::try_new(5, 3).unwrap();
        let shamir = ShamirSSS::new(config);
        let secret = b"TestData";

        let shares = shamir.split_secret(secret).expect("Failed to split");

        let result = shamir.reconstruct_secret(&shares[..2]);
        assert!(result.is_err());
    }

    #[test]
    fn every_threshold_subset_reconstructs() {
        let shamir = ShamirSSS::new(ShamirConfig::try_new(5, 3).unwrap());
        let secret: Vec<u8> = (0..=255u8).collect();
        let shares = shamir.split_secret(&secret).unwrap();
        for a in 0..5 {
            for b in (a + 1)..5 {
                for c in (b + 1)..5 {
                    let subset = vec![shares[c].clone(), shares[a].clone(), shares[b].clone()];
                    assert_eq!(shamir.reconstruct_secret(&subset).unwrap(), secret);
                }
            }
        }
    }

    #[test]
    fn duplicate_indices_are_rejected() {
        let shamir = ShamirSSS::new(ShamirConfig::try_new(3, 2).unwrap());
        let shares = shamir.split_secret(b"abc").unwrap();
        let dup = vec![shares[0].clone(), shares[0].clone()];
        assert!(shamir.reconstruct_secret(&dup).is_err());
    }

    #[test]
    fn invalid_configs_are_errors_not_panics() {
        assert!(ShamirConfig::try_new(0, 0).is_err());
        assert!(ShamirConfig::try_new(3, 0).is_err());
        assert!(ShamirConfig::try_new(2, 3).is_err());
    }

    #[test]
    fn dynamic_threshold_never_drops_to_one_with_two_or_more_peers() {
        assert_eq!(ShamirConfig::dynamic_for(1).threshold, 1);
        assert_eq!(ShamirConfig::dynamic_for(2).threshold, 2);
        assert_eq!(ShamirConfig::dynamic_for(3).threshold, 2);
        assert_eq!(ShamirConfig::dynamic_for(5).threshold, 3);
    }
}
