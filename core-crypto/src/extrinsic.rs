// SPDX-License-Identifier: BUSL-1.1
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::identity::PeerId;
use crate::pallets::balances::BalancesCall;
use crate::pallets::contributions::ContributionsCall;
use crate::pallets::nai::NaiCall;

pub const PALLET_SYSTEM: u8 = 0;
pub const PALLET_BALANCES: u8 = 1;
pub const PALLET_CONTRIBUTIONS: u8 = 2;
pub const PALLET_NAI: u8 = 3;
pub const PALLET_IDENTITY: u8 = 4;
pub const PALLET_AUTHORITY: u8 = 5;
pub const PALLET_ECONOMY: u8 = 6;
pub const PALLET_MARKET: u8 = 7;
pub const PALLET_GOVERNANCE: u8 = 8;
pub const PALLET_REPUTATION: u8 = 9;
pub const PALLET_FINALITY: u8 = 10;
pub const PALLET_UPGRADE: u8 = 11;

pub const EXTRINSIC_VERSION: u8 = 2;

pub const MAX_EXTRINSIC_ARGS_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extrinsic {
    pub version: u8,
    pub pallet_idx: u8,
    pub args: Vec<u8>,
    pub signer: PeerId,
    pub nonce: u64,
    #[serde(with = "crate::wire::bytes64")]
    pub signature: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtrinsicError {
    UnknownPallet,
    UnsupportedVersion,
    DecodeFailure,
    BadSignature,
}

impl std::fmt::Display for ExtrinsicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for ExtrinsicError {}

impl Extrinsic {
    pub fn sign<C: Serialize>(
        signing_key: &SigningKey,
        pallet_idx: u8,
        call: &C,
        signer: PeerId,
        nonce: u64,
    ) -> Self {
        let args = serde_json::to_vec(call).unwrap_or_default();
        let mut e = Extrinsic {
            version: EXTRINSIC_VERSION,
            pallet_idx,
            args,
            signer,
            nonce,
            signature: [0u8; 64],
        };
        let sig = signing_key.sign(&e.hash_for_signing());
        e.signature = sig.to_bytes();
        e
    }

    pub fn hash_for_signing(&self) -> [u8; 32] {
        crate::network::signing_digest(
            crate::network::domains::EXTRINSIC,
            &[
                &[self.version, self.pallet_idx],
                &(self.args.len() as u32).to_be_bytes(),
                &self.args,
                &self.signer,
                &self.nonce.to_le_bytes(),
            ],
        )
    }

    pub fn verify_signature(&self) -> Result<(), ExtrinsicError> {
        if self.version != EXTRINSIC_VERSION {
            return Err(ExtrinsicError::UnsupportedVersion);
        }
        if self.args.len() > MAX_EXTRINSIC_ARGS_BYTES {
            return Err(ExtrinsicError::DecodeFailure);
        }
        let vk =
            VerifyingKey::from_bytes(&self.signer).map_err(|_| ExtrinsicError::BadSignature)?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify_strict(&self.hash_for_signing(), &sig)
            .map_err(|_| ExtrinsicError::BadSignature)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        crate::wire::encode(self)
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        crate::wire::decode(data)
    }
}

#[derive(Debug)]
pub enum DecodedCall {
    Balances(BalancesCall),
    Contributions(ContributionsCall),
    Nai(NaiCall),
    Identity(crate::pallets::identity::IdentityCall),
    Authority(crate::pallets::authority::AuthorityCall),
    Economy(crate::pallets::economy::EconomyCall),
    Market(crate::pallets::market::MarketCall),
    Governance(crate::pallets::governance::GovernanceCall),
    Reputation(crate::pallets::reputation::ReputationCall),
    Finality(crate::finality::FinalityCall),
    Upgrade(crate::pallets::upgrade::UpgradeCall),
}

impl DecodedCall {
    pub fn decode(pallet_idx: u8, args: &[u8]) -> Result<Self, ExtrinsicError> {
        match pallet_idx {
            PALLET_BALANCES => serde_json::from_slice::<BalancesCall>(args)
                .map(DecodedCall::Balances)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_CONTRIBUTIONS => serde_json::from_slice::<ContributionsCall>(args)
                .map(DecodedCall::Contributions)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_NAI => serde_json::from_slice::<NaiCall>(args)
                .map(DecodedCall::Nai)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_IDENTITY => {
                serde_json::from_slice::<crate::pallets::identity::IdentityCall>(args)
                    .map(DecodedCall::Identity)
                    .map_err(|_| ExtrinsicError::DecodeFailure)
            }
            PALLET_AUTHORITY => {
                serde_json::from_slice::<crate::pallets::authority::AuthorityCall>(args)
                    .map(DecodedCall::Authority)
                    .map_err(|_| ExtrinsicError::DecodeFailure)
            }
            PALLET_ECONOMY => serde_json::from_slice::<crate::pallets::economy::EconomyCall>(args)
                .map(DecodedCall::Economy)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_MARKET => serde_json::from_slice::<crate::pallets::market::MarketCall>(args)
                .map(DecodedCall::Market)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_GOVERNANCE => {
                serde_json::from_slice::<crate::pallets::governance::GovernanceCall>(args)
                    .map(DecodedCall::Governance)
                    .map_err(|_| ExtrinsicError::DecodeFailure)
            }
            PALLET_REPUTATION => {
                serde_json::from_slice::<crate::pallets::reputation::ReputationCall>(args)
                    .map(DecodedCall::Reputation)
                    .map_err(|_| ExtrinsicError::DecodeFailure)
            }
            PALLET_FINALITY => serde_json::from_slice::<crate::finality::FinalityCall>(args)
                .map(DecodedCall::Finality)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_UPGRADE => serde_json::from_slice::<crate::pallets::upgrade::UpgradeCall>(args)
                .map(DecodedCall::Upgrade)
                .map_err(|_| ExtrinsicError::DecodeFailure),
            PALLET_SYSTEM => Err(ExtrinsicError::UnknownPallet),
            _ => Err(ExtrinsicError::UnknownPallet),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn fresh_signer() -> (SigningKey, PeerId) {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let sk = SigningKey::from_bytes(&bytes);
        let pid: PeerId = sk.verifying_key().to_bytes();
        (sk, pid)
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let (sk, pid) = fresh_signer();
        let call = BalancesCall::Transfer {
            to: [9u8; 32],
            amount: 10,
        };
        let e = Extrinsic::sign(&sk, PALLET_BALANCES, &call, pid, 1);
        assert!(e.verify_signature().is_ok());
    }

    #[test]
    fn tampered_args_breaks_signature() {
        let (sk, pid) = fresh_signer();
        let call = BalancesCall::Transfer {
            to: [9u8; 32],
            amount: 10,
        };
        let mut e = Extrinsic::sign(&sk, PALLET_BALANCES, &call, pid, 1);
        if let Some(b) = e.args.iter_mut().next() {
            *b ^= 0xFF;
        }
        assert_eq!(e.verify_signature(), Err(ExtrinsicError::BadSignature));
    }

    #[test]
    fn rebroadcast_with_different_signer_breaks_signature() {
        let (sk_a, pid_a) = fresh_signer();
        let (_sk_b, pid_b) = fresh_signer();
        let call = BalancesCall::Transfer {
            to: [9u8; 32],
            amount: 10,
        };
        let mut e = Extrinsic::sign(&sk_a, PALLET_BALANCES, &call, pid_a, 1);
        e.signer = pid_b;
        assert_eq!(e.verify_signature(), Err(ExtrinsicError::BadSignature));
    }

    #[test]
    fn decoded_call_routes_by_pallet_idx() {
        let (sk, pid) = fresh_signer();
        let bal = BalancesCall::Transfer {
            to: [9u8; 32],
            amount: 10,
        };
        let e = Extrinsic::sign(&sk, PALLET_BALANCES, &bal, pid, 1);
        match DecodedCall::decode(e.pallet_idx, &e.args).unwrap() {
            DecodedCall::Balances(BalancesCall::Transfer { to, amount }) => {
                assert_eq!(to, [9u8; 32]);
                assert_eq!(amount, 10);
            }
            other => panic!("expected Balances::Transfer, got {:?}", other),
        }
    }

    #[test]
    fn decode_unknown_pallet_idx_errors() {
        assert_eq!(
            DecodedCall::decode(99, &[]).err(),
            Some(ExtrinsicError::UnknownPallet)
        );
        assert_eq!(
            DecodedCall::decode(PALLET_SYSTEM, &[]).err(),
            Some(ExtrinsicError::UnknownPallet)
        );
    }
}
