// SPDX-License-Identifier: BUSL-1.1

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::identity::PeerId;
use crate::network::{domains, signing_digest};

pub const MAX_RECEIPT_MODEL_B: u16 = 72;
pub const MAX_RECEIPT_TOKENS: u32 = 8_192;
pub const MAX_RECEIPT_GEN_MS: u32 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceClaim {
    pub query_digest: [u8; 32],
    pub sender_pid: PeerId,
    pub responder_pid: PeerId,
    pub hour_epoch: u64,
    pub model_hash: [u8; 32],
    pub model_params_b: u16,
    pub output_tokens: u32,
    pub gen_ms: u32,
    #[serde(with = "crate::wire::bytes64")]
    pub responder_sig: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReceipt {
    pub claim: ServiceClaim,
    #[serde(with = "crate::wire::bytes64")]
    pub sender_sig: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptError {
    BadResponderKey,
    BadResponderSignature,
    BadSenderKey,
    BadSenderSignature,
    SelfService,
    OutOfRange,
}

impl ServiceClaim {
    pub fn claim_digest(&self) -> [u8; 32] {
        signing_digest(
            domains::WIN_RECEIPT,
            &[
                &self.query_digest,
                &self.sender_pid,
                &self.responder_pid,
                &self.hour_epoch.to_le_bytes(),
                &self.model_hash,
                &self.model_params_b.to_le_bytes(),
                &self.output_tokens.to_le_bytes(),
                &self.gen_ms.to_le_bytes(),
            ],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        key: &SigningKey,
        query_digest: [u8; 32],
        sender_pid: PeerId,
        hour_epoch: u64,
        model_hash: [u8; 32],
        model_params_b: u16,
        output_tokens: u32,
        gen_ms: u32,
    ) -> Self {
        let mut claim = ServiceClaim {
            query_digest,
            sender_pid,
            responder_pid: key.verifying_key().to_bytes(),
            hour_epoch,
            model_hash,
            model_params_b: model_params_b.min(MAX_RECEIPT_MODEL_B),
            output_tokens: output_tokens.min(MAX_RECEIPT_TOKENS),
            gen_ms: gen_ms.min(MAX_RECEIPT_GEN_MS),
            responder_sig: [0u8; 64],
        };
        claim.responder_sig = key.sign(&claim.claim_digest()).to_bytes();
        claim
    }

    pub fn verify(&self) -> Result<(), ReceiptError> {
        if self.sender_pid == self.responder_pid {
            return Err(ReceiptError::SelfService);
        }
        if self.model_params_b > MAX_RECEIPT_MODEL_B
            || self.output_tokens > MAX_RECEIPT_TOKENS
            || self.gen_ms > MAX_RECEIPT_GEN_MS
        {
            return Err(ReceiptError::OutOfRange);
        }
        let vk = VerifyingKey::from_bytes(&self.responder_pid)
            .map_err(|_| ReceiptError::BadResponderKey)?;
        vk.verify_strict(
            &self.claim_digest(),
            &Signature::from_bytes(&self.responder_sig),
        )
        .map_err(|_| ReceiptError::BadResponderSignature)
    }

    pub fn dimension(&self) -> u64 {
        crate::lottery::compute_dimension(
            self.model_params_b as u64,
            self.output_tokens as u64,
            self.gen_ms as u64,
        )
    }
}

impl ServiceReceipt {
    fn sender_digest(claim: &ServiceClaim) -> [u8; 32] {
        signing_digest(
            domains::SERVICE_RECEIPT,
            &[&claim.claim_digest(), &claim.responder_sig],
        )
    }

    pub fn countersign(claim: ServiceClaim, sender_key: &SigningKey) -> Self {
        let sender_sig = sender_key.sign(&Self::sender_digest(&claim)).to_bytes();
        ServiceReceipt { claim, sender_sig }
    }

    pub fn verify(&self) -> Result<(), ReceiptError> {
        self.claim.verify()?;
        let vk = VerifyingKey::from_bytes(&self.claim.sender_pid)
            .map_err(|_| ReceiptError::BadSenderKey)?;
        vk.verify_strict(
            &Self::sender_digest(&self.claim),
            &Signature::from_bytes(&self.sender_sig),
        )
        .map_err(|_| ReceiptError::BadSenderSignature)
    }
}

/// Most custodians one answer can credit: as many as a query has
/// (`mesh_network::protocol::MAX_CUSTODIANS`).
pub const MAX_CUSTODY_PER_RECEIPT: usize = 64;

/// The custodians whose fragments an answer was rebuilt from, signed by the
/// node that answered.
///
/// That node knows who released a share to it: each share's index maps to
/// the custodian list the asker signed into the query. The asker checks the
/// list against the custodians it chose before submitting it with its
/// receipt, so crediting custody takes both ends of a real, answered
/// question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyAttestation {
    pub custodians: Vec<PeerId>,
    #[serde(with = "crate::wire::bytes64")]
    pub responder_sig: [u8; 64],
}

impl CustodyAttestation {
    fn digest(claim: &ServiceClaim, custodians: &[PeerId]) -> [u8; 32] {
        signing_digest(
            domains::CUSTODY_ATTEST,
            &[
                &claim.claim_digest(),
                &(custodians.len() as u32).to_le_bytes(),
                &custodians.concat(),
            ],
        )
    }

    pub fn sign(claim: &ServiceClaim, custodians: Vec<PeerId>, responder_key: &SigningKey) -> Self {
        let responder_sig = responder_key
            .sign(&Self::digest(claim, &custodians))
            .to_bytes();
        Self {
            custodians,
            responder_sig,
        }
    }

    /// Signed by the responder of `claim`, over this exact list.
    pub fn verify(&self, claim: &ServiceClaim) -> Result<(), ReceiptError> {
        if self.custodians.len() > MAX_CUSTODY_PER_RECEIPT {
            return Err(ReceiptError::OutOfRange);
        }
        let vk = VerifyingKey::from_bytes(&claim.responder_pid)
            .map_err(|_| ReceiptError::BadResponderKey)?;
        vk.verify_strict(
            &Self::digest(claim, &self.custodians),
            &Signature::from_bytes(&self.responder_sig),
        )
        .map_err(|_| ReceiptError::BadResponderSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }

    #[test]
    fn a_countersigned_receipt_verifies() {
        let responder = key(1);
        let sender = key(2);
        let claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        let receipt = ServiceReceipt::countersign(claim, &sender);
        assert_eq!(receipt.verify(), Ok(()));
    }

    #[test]
    fn a_custody_attestation_binds_its_list_and_its_claim() {
        let responder = key(1);
        let sender = key(2);
        let claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        let att = CustodyAttestation::sign(&claim, vec![[5u8; 32], [6u8; 32]], &responder);
        assert_eq!(att.verify(&claim), Ok(()));

        let mut padded = att.clone();
        padded.custodians.push([7u8; 32]);
        assert_eq!(
            padded.verify(&claim),
            Err(ReceiptError::BadResponderSignature)
        );

        let other = ServiceClaim::sign(
            &responder,
            [8u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        assert_eq!(att.verify(&other), Err(ReceiptError::BadResponderSignature));

        let by_sender = CustodyAttestation::sign(&claim, vec![[5u8; 32]], &sender);
        assert_eq!(
            by_sender.verify(&claim),
            Err(ReceiptError::BadResponderSignature)
        );
    }

    #[test]
    fn a_tampered_claim_fails() {
        let responder = key(1);
        let sender = key(2);
        let claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        let mut receipt = ServiceReceipt::countersign(claim, &sender);
        receipt.claim.output_tokens = 8_000;
        assert!(receipt.verify().is_err());
    }

    #[test]
    fn serving_yourself_is_refused() {
        let k = key(3);
        let claim = ServiceClaim::sign(
            &k,
            [1u8; 32],
            k.verifying_key().to_bytes(),
            1,
            [0u8; 32],
            1,
            1,
            1,
        );
        assert_eq!(claim.verify(), Err(ReceiptError::SelfService));
    }

    #[test]
    fn a_claim_signature_cannot_be_reused_as_a_sender_signature() {
        let responder = key(1);
        let sender = key(2);
        let claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        let forged = ServiceReceipt {
            sender_sig: claim.responder_sig,
            claim,
        };
        assert!(forged.verify().is_err());
    }

    #[test]
    fn a_model_size_above_the_cap_is_clamped_when_signed() {
        // The size a responder declares sets what its answer earns, and
        // nothing checks it against the model it actually ran. The cap is
        // what bounds a lie.
        let responder = key(1);
        let sender = key(2);
        let claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            400,
            400,
            3_000,
        );
        assert_eq!(claim.model_params_b, MAX_RECEIPT_MODEL_B);
        assert_eq!(claim.verify(), Ok(()));
    }

    #[test]
    fn a_claim_declaring_more_than_the_cap_is_refused() {
        let responder = key(1);
        let sender = key(2);
        let mut claim = ServiceClaim::sign(
            &responder,
            [9u8; 32],
            sender.verifying_key().to_bytes(),
            100,
            [0u8; 32],
            8,
            400,
            3_000,
        );
        claim.model_params_b = MAX_RECEIPT_MODEL_B + 1;
        claim.responder_sig = responder.sign(&claim.claim_digest()).to_bytes();
        assert_eq!(claim.verify(), Err(ReceiptError::OutOfRange));
    }
}
