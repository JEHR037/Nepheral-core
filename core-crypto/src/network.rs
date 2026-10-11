// SPDX-License-Identifier: BUSL-1.1

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    pub fn name(self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "testnet" | "test" => Some(Network::Testnet),
            "mainnet" | "main" => Some(Network::Mainnet),
            _ => None,
        }
    }

    pub fn chain_id(self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"nepheral/chain-id/v1");
        h.update(self.name().as_bytes());
        h.update(crate::chain_file::CHAIN_GENERATION.to_be_bytes());
        h.finalize().into()
    }

    pub fn alpn(self) -> Vec<u8> {
        format!("nepheral/3/{}", self.name()).into_bytes()
    }

    pub fn default_bootstraps(self) -> &'static [&'static str] {
        match self {
            Network::Testnet => &["93.189.89.201:80"],
            Network::Mainnet => &[],
        }
    }

    pub fn data_subdir(self) -> &'static str {
        self.name()
    }
}

static ACTIVE: OnceLock<Network> = OnceLock::new();

fn resolve_default() -> Network {
    if let Some(n) = std::env::var("NEPHERAL_NETWORK")
        .ok()
        .and_then(|v| Network::parse(&v))
    {
        return n;
    }
    option_env!("NEPHERAL_DEFAULT_NETWORK")
        .and_then(Network::parse)
        .unwrap_or(Network::Testnet)
}

pub fn set_active(network: Network) -> Network {
    *ACTIVE.get_or_init(|| network)
}

pub fn active() -> Network {
    *ACTIVE.get_or_init(resolve_default)
}

pub fn signing_digest(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update((domain.len() as u32).to_be_bytes());
    h.update(domain);
    h.update(active().chain_id());
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub mod domains {
    pub const EXTRINSIC: &[u8] = b"nepheral/extrinsic/v2";
    pub const BLOCK_HEADER: &[u8] = b"nepheral/block-header/v2";
    pub const VRF: &[u8] = b"nepheral/vrf/v2";
    pub const HELLO: &[u8] = b"nepheral/hello/v3";
    pub const SHARE_PUSH: &[u8] = b"nepheral/share-push/v2";
    pub const SHARE_CLAIM: &[u8] = b"nepheral/share-claim/v2";
    pub const AGENT_RESPONSE: &[u8] = b"nepheral/agent-response/v2";
    pub const ROOM_ENVELOPE: &[u8] = b"nepheral/room-envelope/v1";
    pub const WIN_RECEIPT: &[u8] = b"nepheral/win-receipt/v1";
    pub const SERVICE_RECEIPT: &[u8] = b"nepheral/service-receipt/v1";
    pub const FINALITY_VOTE: &[u8] = b"nepheral/finality-vote/v1";
    pub const DELEGATION: &[u8] = b"nepheral/delegation/v1";
    pub const CUSTODY_ATTEST: &[u8] = b"nepheral/custody-attest/v1";
    pub const AGENT_DECLINE: &[u8] = b"nepheral/agent-decline/v1";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_have_distinct_chain_ids_and_alpns() {
        assert_ne!(Network::Testnet.chain_id(), Network::Mainnet.chain_id());
        assert_ne!(Network::Testnet.alpn(), Network::Mainnet.alpn());
    }

    #[test]
    fn domains_separate_otherwise_identical_payloads() {
        let a = signing_digest(domains::EXTRINSIC, &[b"x"]);
        let b = signing_digest(domains::VRF, &[b"x"]);
        assert_ne!(a, b);
    }

    #[test]
    fn parse_accepts_both_spellings() {
        assert_eq!(Network::parse(" MainNet "), Some(Network::Mainnet));
        assert_eq!(Network::parse("test"), Some(Network::Testnet));
        assert_eq!(Network::parse("devnet"), None);
    }
}
