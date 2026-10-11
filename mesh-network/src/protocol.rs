// SPDX-License-Identifier: BUSL-1.1
use serde::{Deserialize, Serialize};

use core_crypto::network::{domains, signing_digest};

pub mod message_types {
    pub const HELLO: u8 = 0x05;
    pub const PEER_LIST: u8 = 0x10;
    pub const GOSSIP_PEER: u8 = 0x11;
    pub const PEER_LIST_REQUEST: u8 = 0x12;
    /// Other nodes' signed HELLOs, passed along so every node knows every
    /// node, including the ones it has no direct connection to.
    pub const PRESENCE: u8 = 0x13;
    /// Asks a public node to introduce two peers for a hole punch.
    pub const PUNCH_REQUEST: u8 = 0x14;
    /// From that public node: dial this peer at this address, now.
    pub const PUNCH_INTRO: u8 = 0x15;
    pub const ROOM_ENVELOPE: u8 = 0x24;
    pub const NODE_STATS: u8 = 0x41;
    pub const MODEL_ANNOUNCE: u8 = 0x43;
    pub const CHAIN_TX: u8 = 0x44;
    pub const BLOCK_BROADCAST: u8 = 0x4A;
    pub const BLOCK_SYNC_REQUEST: u8 = 0x4B;
    pub const FINALITY_VOTE: u8 = 0x4C;
    /// A light node asks for the proof it starts from (core_crypto::light).
    /// Answered on the same stream: a u32 length, then the encoded proof.
    pub const LIGHT_SYNC_REQUEST: u8 = 0x4D;
    pub const MODEL_REQUEST: u8 = 0x50;
    pub const MODEL_TRANSFER: u8 = 0x53;
    pub const SHARE_PUSH: u8 = 0x60;
    pub const SHARE_CLAIM: u8 = 0x61;
    pub const AGENT_DECLINE: u8 = 0x62;
    pub const AGENT_RESPONSE_DIRECT: u8 = 0x63;
    /// A claim for a custodian this node has no direct connection to. Its
    /// answer comes back as `SHARE_RETURN`, not on the same stream.
    pub const SHARE_CLAIM_RELAYED: u8 = 0x64;
    /// A custodian's answer to a relayed claim: the asker's own signed
    /// `SharePush`, so neither a relay nor the custodian can alter the share.
    pub const SHARE_RETURN: u8 = 0x65;
    pub const RELAY_OPEN: u8 = 0x70;
    /// A frame sealed to its destination, so the relays carrying it see only
    /// who it is for.
    pub const SEALED: u8 = 0x71;
    /// A decline signed by the candidate giving up its place, so it can
    /// reach ranks behind NAT through the relays (see `SignedDecline`).
    pub const SIGNED_DECLINE: u8 = 0x66;
}

pub const MAX_RELAY_HOPS: u8 = 2;
pub const MAX_RELAY_OPEN_BYTES: usize = 1024 * 1024;
pub const MAX_PEER_LIST_BYTES: usize = 64 * 1024;
pub const MAX_PEER_LIST_ENTRIES: usize = 256;
pub const MAX_GOSSIP_PEER_BYTES: usize = 64;
pub const MAX_HELLO_BYTES: usize = 2048;
pub const MAX_HELLO_ADDRS: usize = 8;
pub const MAX_SHARE_PUSH_BYTES: usize = 1024 * 1024;
pub const MAX_SHARE_CLAIM_BYTES: usize = 4 * 1024;
pub const MAX_AGENT_DECLINE_BYTES: usize = 128;
pub const MAX_SIGNED_DECLINE_BYTES: usize = 256;
pub const MAX_SHARE_RELEASE_BYTES: usize = 1024 * 1024;
pub const MAX_AGENT_RESPONSE_BYTES: usize = 512 * 1024;
pub const MAX_ROOM_ENVELOPE_BYTES: usize = 96 * 1024;
pub const MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;
/// The decoder's own limit (core_crypto::wire::MAX_DECODE_BYTES).
pub const MAX_LIGHT_PROOF_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CHAIN_TX_BYTES: usize = 32 * 1024;
pub const MAX_FINALITY_VOTE_BYTES: usize = 512;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_CLAIM_RANK: u8 = 2;
/// Every node holds a share of every query it is told about, up to this
/// many. Shamir indexes are a byte, so 255 is the hard ceiling.
pub const MAX_CUSTODIANS: usize = 64;
pub const MAX_PRESENCE_ENTRIES: usize = 256;
pub const MAX_PRESENCE_BYTES: usize = MAX_PRESENCE_ENTRIES * MAX_HELLO_BYTES;
/// How long a node's last HELLO keeps it known when it is heard of only
/// through others. HELLOs are refreshed every 30 seconds and each hop adds
/// up to another 30.
pub const PRESENCE_TTL_MS: u64 = 2 * 60 * 1_000;
pub const MAX_PUNCH_BYTES: usize = 128;
pub const MAX_SEALED_BYTES: usize = MAX_RELAY_OPEN_BYTES;
/// 4: SharePush names its custodians, custody reaches nodes behind NAT
/// through sealed relays, presence and hole punching.
/// 5: an answer carries the custodians it was rebuilt from, so the asker
/// can credit them on chain.
pub const PROTOCOL_VERSION: u16 = 5;
/// The oldest protocol this build still talks to.
///
/// A node accepts every peer from this version up, newer ones included, as
/// long as their HELLO decodes, and speaks to each in the newest dialect
/// both know (see `PeerCapabilities::protocol`). So a release that bumps
/// `PROTOCOL_VERSION` no longer cuts off every node that has not updated
/// yet. Raise this only to drop old nodes for good, once the window
/// announced with the release has passed.
pub const MIN_PROTOCOL_VERSION: u16 = 5;

/// Whether a peer announcing `version` can be talked to.
pub fn speaks(version: u16) -> bool {
    version >= MIN_PROTOCOL_VERSION
}

pub type NodeId = u64;

pub fn generate_node_id() -> NodeId {
    use rand::Rng;
    rand::thread_rng().gen()
}

pub fn random_message_id() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen()
}

/// "Not me, and not because I lost the draw."
///
/// A candidate that is already mid-inference used to return silently, so the
/// ranks behind it slept out their full delay - 8s, then 16s - waiting for an
/// answer that was never coming, and the asker gave up at its deadline. This
/// says so immediately instead, and carries nothing: no share, no plaintext,
/// nothing held. The receiver takes the responder's identity from the
/// authenticated TLS connection rather than from the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDecline {
    pub version: u8,
    pub sender_pid: [u8; 32],
    pub message_id: u64,
    /// The declining candidate's own rank, so the ranks behind it know
    /// whether everyone ahead of them has now stood down.
    pub rank: u8,
}

impl AgentDecline {
    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() > MAX_AGENT_DECLINE_BYTES {
            return None;
        }
        core_crypto::wire::decode(buf)
    }
}

/// `AgentDecline` signed by the decliner.
///
/// The unsigned decline is believed only from the TLS peer that sent it, so
/// it can travel one hop: ranks reached only through relays never saw the
/// ranks ahead of them stand down, and waited out their full delays. Signed,
/// it can be sealed and relayed to every ranked candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedDecline {
    pub version: u8,
    pub sender_pid: [u8; 32],
    pub message_id: u64,
    pub rank: u8,
    pub decliner: [u8; 32],
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl SignedDecline {
    pub const VERSION: u8 = 1;

    pub fn signing_digest(&self) -> [u8; 32] {
        signing_digest(
            domains::AGENT_DECLINE,
            &[
                &[self.version],
                &self.sender_pid,
                &self.message_id.to_le_bytes(),
                &[self.rank],
                &self.decliner,
            ],
        )
    }

    pub fn sign(
        key: &ed25519_dalek::SigningKey,
        sender_pid: [u8; 32],
        message_id: u64,
        rank: u8,
    ) -> Self {
        use ed25519_dalek::Signer;
        let mut d = Self {
            version: Self::VERSION,
            sender_pid,
            message_id,
            rank,
            decliner: key.verifying_key().to_bytes(),
            signature: [0u8; 64],
        };
        d.signature = key.sign(&d.signing_digest()).to_bytes();
        d
    }

    pub fn verify(&self) -> bool {
        self.version == Self::VERSION
            && ed25519_dalek::VerifyingKey::from_bytes(&self.decliner).is_ok_and(|vk| {
                vk.verify_strict(
                    &self.signing_digest(),
                    &ed25519_dalek::Signature::from_bytes(&self.signature),
                )
                .is_ok()
            })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        (buf.len() <= MAX_SIGNED_DECLINE_BYTES)
            .then(|| core_crypto::wire::decode(buf))
            .flatten()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub version: u16,
    pub peer_id: [u8; 32],
    pub listen_port: u16,
    pub caps: u8,
    pub model: Option<ModelInfo>,
    pub addrs: Vec<std::net::IpAddr>,
    pub sent_at_ms: u64,
    pub observed: Option<std::net::SocketAddr>,
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl Hello {
    pub fn signing_digest(&self) -> [u8; 32] {
        let mut addrs = Vec::with_capacity(self.addrs.len() * 17);
        for a in &self.addrs {
            match a {
                std::net::IpAddr::V4(ip) => {
                    addrs.push(4u8);
                    addrs.extend_from_slice(&ip.octets());
                }
                std::net::IpAddr::V6(ip) => {
                    addrs.push(6u8);
                    addrs.extend_from_slice(&ip.octets());
                }
            }
        }
        let model = core_crypto::wire::encode(&self.model);
        signing_digest(
            domains::HELLO,
            &[
                &self.version.to_be_bytes(),
                &self.peer_id,
                &self.listen_port.to_be_bytes(),
                &[self.caps],
                &(model.len() as u32).to_be_bytes(),
                &model,
                &(self.addrs.len() as u32).to_be_bytes(),
                &addrs,
                &self.sent_at_ms.to_be_bytes(),
            ],
        )
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        if buf.len() > MAX_HELLO_BYTES {
            return Err("Hello too large".to_string());
        }
        let hello: Hello =
            core_crypto::wire::decode(buf).ok_or_else(|| "Hello malformed".to_string())?;
        if hello.addrs.len() > MAX_HELLO_ADDRS {
            return Err("Hello lists too many addresses".to_string());
        }
        if hello.model.as_ref().is_some_and(|m| !m.is_bounded()) {
            return Err("Hello model name too long".to_string());
        }
        Ok(hello)
    }

    /// The protocol version a HELLO announces, read even when the rest of
    /// it is in a format this build does not know: the version leads every
    /// HELLO, so a node can tell a peer is newer and say so.
    pub fn peek_version(buf: &[u8]) -> Option<u16> {
        core_crypto::wire::decode::<u16>(buf.get(..buf.len().min(8))?)
    }

    pub fn verify(&self) -> Result<(), String> {
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&self.peer_id)
            .map_err(|e| format!("malformed peer id: {e}"))?;
        vk.verify_strict(
            &self.signing_digest(),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "HELLO signature does not match the claimed peer id".to_string())
    }

    /// Where this peer can be dialed, as far as this node can tell.
    ///
    /// Private, CGNAT and link-local addresses are kept only when the HELLO
    /// itself arrived from one, which means both nodes share that network (a
    /// LAN, a VPN). Otherwise they name some other network, and passing them
    /// on sends every node dialing an address that can never answer.
    pub fn dialable_addrs(&self, source_ip: std::net::IpAddr) -> Vec<std::net::SocketAddr> {
        self.dialable_addrs_from(source_ip, &[])
    }

    /// As `dialable_addrs`, knowing this node's own public addresses: a
    /// peer that shares one of them sits behind the same router, so its
    /// private addresses are on this node's network too and reach it
    /// without going out and back in (which many routers refuse).
    pub fn dialable_addrs_from(
        &self,
        source_ip: std::net::IpAddr,
        mine: &[std::net::IpAddr],
    ) -> Vec<std::net::SocketAddr> {
        let same_private_net = is_local_scope(&source_ip) || self.behind_same_router(mine);
        let keep =
            |ip: &std::net::IpAddr| !is_unusable(ip) && (same_private_net || !is_local_scope(ip));
        let mut out: Vec<std::net::SocketAddr> = self
            .addrs
            .iter()
            .filter(|ip| keep(ip))
            .map(|ip| std::net::SocketAddr::new(*ip, self.listen_port))
            .collect();

        let from_source = std::net::SocketAddr::new(source_ip, self.listen_port);
        if !out.contains(&from_source) && !is_unusable(&source_ip) {
            out.push(from_source);
        }

        out.sort_by_key(|a| u8::from(a.is_ipv4()));
        out
    }
}

impl Hello {
    /// Whether this peer announces a public address this node has too.
    pub fn behind_same_router(&self, mine: &[std::net::IpAddr]) -> bool {
        self.addrs
            .iter()
            .any(|a| !is_local_scope(a) && !is_unusable(a) && mine.contains(a))
    }
}

fn is_unusable(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_multicast(),
    }
}

/// An address that only means something inside one network: RFC 1918,
/// CGNAT (100.64/10), link-local, and IPv6 unique-local.
pub fn is_local_scope(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_link_local() || (o[0] == 100 && (o[1] & 0xC0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments()[0];
            (s & 0xFE00) == 0xFC00 || (s & 0xFFC0) == 0xFE80
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerList {
    pub peers: Vec<PeerEntry>,
}

#[derive(Debug, Clone)]
pub struct PeerEntry {
    pub addr: std::net::IpAddr,
    pub port: u16,
    pub last_seen_ms: u64, // When introducer last saw this peer
}

impl PeerList {
    pub fn new(peers: Vec<PeerEntry>) -> Self {
        Self { peers }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + (self.peers.len() * 25));
        buf.extend_from_slice(&(self.peers.len() as u32).to_be_bytes());

        for peer in &self.peers {
            buf.extend_from_slice(&peer.addr_to_bytes());
            buf.extend_from_slice(&peer.port.to_be_bytes());
            buf.extend_from_slice(&peer.last_seen_ms.to_be_bytes());
        }

        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < 4 {
            return Err("PeerList too short".to_string());
        }

        let count = u32::from_be_bytes(buf[0..4].try_into().map_err(|_| "Invalid count")?) as usize;
        if count > MAX_PEER_LIST_ENTRIES || count > (buf.len() - 4) / 15 {
            return Err(format!(
                "PeerList claims {count} entries in {} bytes",
                buf.len()
            ));
        }
        let mut peers = Vec::with_capacity(count);
        let mut offset = 4;

        for _ in 0..count {
            if offset + 11 > buf.len() {
                return Err("PeerList truncated".to_string());
            }

            let addr_type = buf[offset];
            offset += 1;

            let addr = match addr_type {
                4 => {
                    if offset + 4 > buf.len() {
                        return Err("IPv4 addr truncated".to_string());
                    }
                    let ip = std::net::Ipv4Addr::new(
                        buf[offset],
                        buf[offset + 1],
                        buf[offset + 2],
                        buf[offset + 3],
                    );
                    offset += 4;
                    std::net::IpAddr::V4(ip)
                }
                6 => {
                    if offset + 16 > buf.len() {
                        return Err("IPv6 addr truncated".to_string());
                    }
                    let octets: [u8; 16] = buf[offset..offset + 16]
                        .try_into()
                        .map_err(|_| "Invalid IPv6")?;
                    let ip = std::net::Ipv6Addr::from(octets);
                    offset += 16;
                    std::net::IpAddr::V6(ip)
                }
                _ => return Err(format!("Unknown addr type: {}", addr_type)),
            };

            if offset + 10 > buf.len() {
                return Err("PeerList entry truncated".to_string());
            }
            let port = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
            offset += 2;
            let last_seen_ms = u64::from_be_bytes(
                buf[offset..offset + 8]
                    .try_into()
                    .map_err(|_| "Invalid timestamp")?,
            );
            offset += 8;

            peers.push(PeerEntry {
                addr,
                port,
                last_seen_ms,
            });
        }

        Ok(Self { peers })
    }

    pub fn to_socket_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.peers
            .iter()
            .map(|p| std::net::SocketAddr::new(p.addr, p.port))
            .collect()
    }
}

impl PeerEntry {
    fn addr_to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self.addr {
            std::net::IpAddr::V4(ip) => {
                buf.push(4);
                buf.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                buf.push(6);
                buf.extend_from_slice(&ip.octets());
            }
        }
        buf
    }
}

#[derive(Debug, Clone)]
pub struct GossipPeer {
    pub addr: std::net::SocketAddr,
    pub quality_score: u32,
}

impl GossipPeer {
    pub fn new(addr: std::net::SocketAddr, quality_score: u32) -> Self {
        Self {
            addr,
            quality_score: quality_score.min(100),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.push(1);
        match self.addr.ip() {
            std::net::IpAddr::V4(ip) => {
                buf.push(4);
                buf.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                buf.push(16);
                buf.extend_from_slice(&ip.octets());
            }
        }
        buf.extend_from_slice(&self.addr.port().to_be_bytes());
        buf.extend_from_slice(&self.quality_score.to_be_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < 2 {
            return Err("GossipPeer too short".to_string());
        }
        let addr_len = buf[1] as usize;
        if addr_len != 4 && addr_len != 16 {
            return Err(format!("Invalid addr_len: {}", addr_len));
        }
        if buf.len() < 2 + addr_len + 6 {
            return Err("GossipPeer truncated".to_string());
        }
        let addr = if addr_len == 4 {
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(buf[2], buf[3], buf[4], buf[5]))
        } else {
            let octets: [u8; 16] = buf[2..18].try_into().map_err(|_| "Invalid IPv6")?;
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets))
        };
        let port_offset = 2 + addr_len;
        let port = u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]]);
        let quality_score = u32::from_be_bytes(
            buf[port_offset + 2..port_offset + 6]
                .try_into()
                .map_err(|_| "Invalid quality")?,
        );
        Ok(Self {
            addr: std::net::SocketAddr::new(addr, port),
            quality_score: quality_score.min(100),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub tier: u8,
    pub hash: [u8; 32],
    pub params_b: u16,
}

impl ModelInfo {
    pub const MAX_NAME_LEN: usize = 96;

    fn is_bounded(&self) -> bool {
        self.name.len() <= Self::MAX_NAME_LEN
    }
}

#[derive(Debug, Clone, Default)]
pub struct PeerCapabilities {
    pub has_ai: bool,
    pub can_relay: bool,
    pub model: Option<ModelInfo>,
    /// The protocol version the peer announced. Messages whose layout
    /// changed after it must be sent to it in the older form, or not at all.
    pub protocol: u16,
    /// A light node (the phone app): holds fragments and asks, but never
    /// answers, produces blocks or serves the chain.
    pub light: bool,
    /// Serves light nodes the chain proofs and state snapshots they sync from.
    pub serves_light: bool,
    /// What the node says seals its key.
    pub hw_seal: Option<HwSeal>,
    pub sealed_replies: bool,
}

pub mod caps_bits {
    pub const HAS_AI: u8 = 0b0000_0001;
    pub const CAN_RELAY: u8 = 0b0000_0010;
    /// Reserved for the phone app; nodes before 0.5.2 ignore unknown bits.
    pub const LIGHT: u8 = 0b0000_0100;
    pub const SERVES_LIGHT: u8 = 0b0000_1000;
    /// Bits 4-6: the hardware the node's own key is sealed in (`HwSeal`),
    /// as the node declares it; nodes before 0.5.3 ignore them.
    pub const HW_SEAL_SHIFT: u8 = 4;
    pub const HW_SEAL_MASK: u8 = 0b0111_0000;
    /// The node opens an answer sealed to it (`SEALED` carrying
    /// AGENT_RESPONSE_DIRECT): a relay carrying one then sees nothing of
    /// it. Nodes before 0.5.3 get theirs unsealed, as before.
    pub const SEALED_REPLIES: u8 = 0b1000_0000;
}

/// The hardware a node seals its own key with. Declared in HELLO; shown in
/// node lists as declared until a TPM quote can prove it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HwSeal {
    /// A TPM 2.0 chip.
    Tpm,
    /// A TPM 2.0 in the processor's firmware (Intel PTT, AMD fTPM).
    Ftpm,
    /// Microsoft Pluton.
    Pluton,
    /// A phone's trusted execution environment (Android Keystore).
    Tee,
    /// A phone's secure element (Android StrongBox).
    Strongbox,
}

impl HwSeal {
    pub fn bits(seal: Option<HwSeal>) -> u8 {
        let v: u8 = match seal {
            None => 0,
            Some(HwSeal::Tpm) => 1,
            Some(HwSeal::Ftpm) => 2,
            Some(HwSeal::Pluton) => 3,
            Some(HwSeal::Tee) => 4,
            Some(HwSeal::Strongbox) => 5,
        };
        v << caps_bits::HW_SEAL_SHIFT
    }

    pub fn from_caps(caps: u8) -> Option<HwSeal> {
        match (caps & caps_bits::HW_SEAL_MASK) >> caps_bits::HW_SEAL_SHIFT {
            1 => Some(HwSeal::Tpm),
            2 => Some(HwSeal::Ftpm),
            3 => Some(HwSeal::Pluton),
            4 => Some(HwSeal::Tee),
            5 => Some(HwSeal::Strongbox),
            _ => None,
        }
    }
}

impl PeerCapabilities {
    pub fn from_hello(hello: &Hello) -> Self {
        Self {
            has_ai: hello.caps & caps_bits::HAS_AI != 0,
            can_relay: hello.caps & caps_bits::CAN_RELAY != 0,
            model: hello.model.clone(),
            protocol: hello.version,
            light: hello.caps & caps_bits::LIGHT != 0,
            serves_light: hello.caps & caps_bits::SERVES_LIGHT != 0,
            hw_seal: HwSeal::from_caps(hello.caps),
            sealed_replies: hello.caps & caps_bits::SEALED_REPLIES != 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LotteryInputs {
    pub hour_epoch: u64,
    pub plaintext_len: u64,
    pub n_engines: u64,
    pub room_salt: [u8; 32],
}

impl LotteryInputs {
    pub fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"nepheral/lottery-inputs/v2");
        h.update(self.hour_epoch.to_le_bytes());
        h.update(self.plaintext_len.to_le_bytes());
        h.update(self.n_engines.to_le_bytes());
        h.update(self.room_salt);
        h.finalize().into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePush {
    pub version: u8,
    pub message_id: u64,
    pub sender_pid: [u8; 32],
    pub inputs: LotteryInputs,
    pub candidates: Vec<[u8; 32]>,
    pub candidate_weights: Vec<u8>,
    pub share_index: u8,
    pub total_shares: u8,
    pub threshold: u8,
    pub share_data: Vec<u8>,
    pub bundle_digest: [u8; 32],
    pub target_channel: Option<String>,
    pub session_token: Option<[u8; 32]>,
    pub min_responder_maturity: Option<u64>,
    pub vrf_seed: [u8; 32],
    pub sent_at_ms: u64,
    /// Who holds which share: `custodians[i]` holds share index `i + 1`. A
    /// claimant needs this to reach the custodians it has no direct
    /// connection to.
    pub custodians: Vec<[u8; 32]>,
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl SharePush {
    pub const VERSION: u8 = 4;

    pub fn signing_digest(&self) -> [u8; 32] {
        let mut candidates = Vec::with_capacity(self.candidates.len() * 32);
        for c in &self.candidates {
            candidates.extend_from_slice(c);
        }
        let channel = core_crypto::wire::encode(&self.target_channel);
        let maturity = core_crypto::wire::encode(&self.min_responder_maturity);
        let session = core_crypto::wire::encode(&self.session_token);
        signing_digest(
            domains::SHARE_PUSH,
            &[
                &[self.version],
                &self.message_id.to_le_bytes(),
                &self.sender_pid,
                &self.inputs.digest(),
                &(self.candidates.len() as u32).to_le_bytes(),
                &candidates,
                &self.candidate_weights,
                &[self.share_index, self.total_shares, self.threshold],
                &(self.share_data.len() as u32).to_le_bytes(),
                &self.share_data,
                &self.bundle_digest,
                &(channel.len() as u32).to_le_bytes(),
                &channel,
                &session,
                &maturity,
                &self.vrf_seed,
                &self.sent_at_ms.to_le_bytes(),
                &(self.custodians.len() as u32).to_le_bytes(),
                &self.custodians.concat(),
            ],
        )
    }

    pub fn expected_vrf_seed(&self) -> [u8; 32] {
        core_crypto::lottery::vrf_seed(self.message_id, &self.sender_pid, self.inputs.hour_epoch)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != Self::VERSION {
            return Err(format!("unsupported SharePush version {}", self.version));
        }
        if self.candidates.is_empty() || self.candidates.len() > MAX_CANDIDATES {
            return Err("SharePush candidate list out of range".to_string());
        }
        if self.candidate_weights.len() != self.candidates.len()
            || self
                .candidate_weights
                .iter()
                .any(|w| *w == 0 || *w > core_crypto::pallets::reputation::MAX_ROUTING_WEIGHT)
        {
            return Err("SharePush candidate weights out of range".to_string());
        }
        core_crypto::ShamirConfig::try_new(self.total_shares, self.threshold)?;
        if self.share_index == 0 || self.share_index > self.total_shares {
            return Err("SharePush share index out of range".to_string());
        }
        if self.custodians.len() != self.total_shares as usize {
            return Err("SharePush must name one custodian per share".to_string());
        }
        if self.vrf_seed != self.expected_vrf_seed() {
            return Err("SharePush VRF seed is not derived from the message".to_string());
        }
        if self.target_channel.as_ref().is_some_and(|c| c.len() > 64) {
            return Err("SharePush channel name too long".to_string());
        }
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&self.sender_pid)
            .map_err(|_| "SharePush sender is not an ed25519 key".to_string())?;
        vk.verify_strict(
            &self.signing_digest(),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "SharePush signature did not verify".to_string())
    }

    pub fn wrapped_for(&self) -> Vec<[u8; 32]> {
        core_crypto::lottery::rank_candidates_weighted(
            &self.candidates,
            &self.candidate_weights,
            &self.inputs.digest(),
            MAX_CLAIM_RANK as usize + 1,
        )
        .into_iter()
        .copied()
        .collect()
    }

    /// The custodian holding this push's share.
    pub fn custodian(&self) -> Option<[u8; 32]> {
        self.custodians
            .get((self.share_index as usize).checked_sub(1)?)
            .copied()
    }

    /// What a custodian hands a claimant who reached it directly.
    pub fn release(&self) -> ShareRelease {
        ShareRelease {
            version: 2,
            message_id: self.message_id,
            sender_pid: self.sender_pid,
            share_index: self.share_index,
            total_shares: self.total_shares,
            threshold: self.threshold,
            share_data: self.share_data.clone(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

/// Signed HELLOs of other nodes. Each still verifies under its own author's
/// key, so the node passing them on can only drop them, not alter them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub hellos: Vec<Hello>,
}

impl Presence {
    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        if buf.len() > MAX_PRESENCE_BYTES {
            return Err("Presence too large".to_string());
        }
        let p: Presence =
            core_crypto::wire::decode(buf).ok_or_else(|| "Presence malformed".to_string())?;
        if p.hellos.len() > MAX_PRESENCE_ENTRIES {
            return Err("Presence lists too many nodes".to_string());
        }
        if p.hellos.iter().any(|h| {
            h.addrs.len() > MAX_HELLO_ADDRS || h.model.as_ref().is_some_and(|m| !m.is_bounded())
        }) {
            return Err("Presence carries an oversized HELLO".to_string());
        }
        Ok(p)
    }
}

/// To a public node both peers are connected to: introduce me to `target`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunchRequest {
    pub version: u8,
    pub target: [u8; 32],
}

/// From that public node: `peer` is dialing you from `addr` right now, as
/// the public node sees it from outside; dial it back there. Unsigned: a
/// wrong address only costs a failed dial, because the dial pins `peer`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunchIntro {
    pub version: u8,
    pub peer: [u8; 32],
    pub addr: std::net::SocketAddr,
}

impl PunchRequest {
    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        (b.len() <= MAX_PUNCH_BYTES)
            .then(|| core_crypto::wire::decode(b))
            .flatten()
    }
}

impl PunchIntro {
    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        (b.len() <= MAX_PUNCH_BYTES)
            .then(|| core_crypto::wire::decode(b))
            .flatten()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentClaim {
    pub version: u8,
    pub message_id: u64,
    pub sender_pid: [u8; 32],
    pub claimant_pid: [u8; 32],
    pub inputs: LotteryInputs,
    pub rank: u8,
    #[serde(with = "core_crypto::wire::bytes64")]
    pub vrf_proof: [u8; 64],
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl AgentClaim {
    pub const VERSION: u8 = 2;

    pub fn signing_digest(&self) -> [u8; 32] {
        signing_digest(
            domains::SHARE_CLAIM,
            &[
                &[self.version],
                &self.message_id.to_le_bytes(),
                &self.sender_pid,
                &self.claimant_pid,
                &self.inputs.digest(),
                &[self.rank],
                &self.vrf_proof,
            ],
        )
    }

    pub fn verify_signature(&self) -> bool {
        let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&self.claimant_pid) else {
            return false;
        };
        vk.verify_strict(
            &self.signing_digest(),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .is_ok()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareRelease {
    pub version: u8,
    pub message_id: u64,
    pub sender_pid: [u8; 32],
    pub share_index: u8,
    pub total_shares: u8,
    pub threshold: u8,
    pub share_data: Vec<u8>,
}

impl ShareRelease {
    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReply {
    pub text: String,
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponseDirect {
    pub version: u8,
    pub query_message_id: u64,
    pub query_digest: [u8; 32],
    pub responder_pid: [u8; 32],
    pub sealed: core_crypto::agent_crypto::SealedMessage,
    pub service_claim: core_crypto::receipts::ServiceClaim,
    /// The custodians whose fragments the answer was rebuilt from, signed
    /// by the responder, for the asker to credit with its receipt.
    pub custody: Option<core_crypto::receipts::CustodyAttestation>,
    #[serde(with = "core_crypto::wire::bytes64")]
    pub signature: [u8; 64],
}

impl AgentResponseDirect {
    pub const VERSION: u8 = 3;

    pub fn signing_digest(&self) -> [u8; 32] {
        let sealed = core_crypto::wire::encode(&self.sealed);
        let custody = core_crypto::wire::encode(&self.custody);
        signing_digest(
            domains::AGENT_RESPONSE,
            &[
                &[self.version],
                &self.query_message_id.to_le_bytes(),
                &self.query_digest,
                &self.responder_pid,
                &(sealed.len() as u32).to_le_bytes(),
                &sealed,
                &self.service_claim.claim_digest(),
                &(custody.len() as u32).to_le_bytes(),
                &custody,
            ],
        )
    }

    pub fn verify(&self) -> Result<(), String> {
        if self.version != Self::VERSION {
            return Err(format!("unsupported response version {}", self.version));
        }
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&self.responder_pid)
            .map_err(|_| "responder is not an ed25519 key".to_string())?;
        vk.verify_strict(
            &self.signing_digest(),
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "response signature failed verification".to_string())?;
        if self.service_claim.responder_pid != self.responder_pid
            || self.service_claim.query_digest != self.query_digest
        {
            return Err("service claim does not describe this response".to_string());
        }
        if let Some(custody) = &self.custody {
            custody
                .verify(&self.service_claim)
                .map_err(|e| format!("custody attestation invalid: {e:?}"))?;
        }
        self.service_claim
            .verify()
            .map_err(|e| format!("service claim invalid: {e:?}"))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayOpen {
    pub version: u8,
    pub origin_pid: [u8; 32],
    pub dest_pid: [u8; 32],
    pub message_id: u64,
    pub hop_ttl: u8,
    pub inner_msg_type: u8,
    pub inner_bytes: Vec<u8>,
}

impl RelayOpen {
    pub fn inner_type_is_relayable(&self) -> bool {
        matches!(
            self.inner_msg_type,
            message_types::SHARE_PUSH
                | message_types::AGENT_RESPONSE_DIRECT
                | message_types::SEALED
        )
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        core_crypto::wire::encode(self)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        core_crypto::wire::decode(b)
    }
}

pub fn frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(msg_type);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn sample_hello() -> Hello {
        let k = key();
        let mut h = Hello {
            version: PROTOCOL_VERSION,
            peer_id: k.verifying_key().to_bytes(),
            listen_port: 443,
            caps: caps_bits::HAS_AI | caps_bits::CAN_RELAY,
            model: Some(ModelInfo {
                name: "qwen".into(),
                tier: 2,
                hash: [1u8; 32],
                params_b: 8,
            }),
            addrs: vec![
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
            ],
            sent_at_ms: 5,
            observed: Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2)),
                51234,
            )),
            signature: [0u8; 64],
        };
        h.signature = k.sign(&h.signing_digest()).to_bytes();
        h
    }

    #[test]
    fn hello_roundtrips_and_verifies() {
        let h = sample_hello();
        let back = Hello::from_bytes(&h.to_bytes()).expect("decodes");
        assert_eq!(back.model, h.model);
        assert_eq!(back.addrs, h.addrs);
        assert!(back.verify().is_ok());
    }

    #[test]
    fn a_signed_decline_verifies_and_resists_tampering() {
        let d = SignedDecline::sign(&key(), [3u8; 32], 42, 1);
        assert!(d.verify());
        let back = SignedDecline::from_bytes(&d.to_bytes()).expect("decodes");
        assert!(back.verify());
        let mut other_rank = d.clone();
        other_rank.rank = 0;
        assert!(
            !other_rank.verify(),
            "a decline cannot be moved to another rank"
        );
        let mut other_node = d.clone();
        other_node.decliner = SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(!other_node.verify(), "nor pinned on another node");
    }

    #[test]
    fn newer_peers_are_spoken_to_and_older_ones_are_not() {
        assert!(speaks(PROTOCOL_VERSION));
        assert!(speaks(PROTOCOL_VERSION + 1));
        assert!(speaks(MIN_PROTOCOL_VERSION));
        assert!(!speaks(MIN_PROTOCOL_VERSION - 1));
    }

    #[test]
    fn capabilities_carry_the_peer_protocol_and_light_bits() {
        let mut h = sample_hello();
        h.version = PROTOCOL_VERSION + 1;
        h.caps = caps_bits::LIGHT;
        let caps = PeerCapabilities::from_hello(&h);
        assert_eq!(caps.protocol, PROTOCOL_VERSION + 1);
        assert!(caps.light && !caps.serves_light && !caps.has_ai);
        assert_eq!(caps.hw_seal, None);
        for seal in [
            HwSeal::Tpm,
            HwSeal::Ftpm,
            HwSeal::Pluton,
            HwSeal::Tee,
            HwSeal::Strongbox,
        ] {
            h.caps = caps_bits::HAS_AI | HwSeal::bits(Some(seal));
            let c = PeerCapabilities::from_hello(&h);
            assert_eq!(c.hw_seal, Some(seal));
            assert!(c.has_ai && !c.light);
        }
    }

    #[test]
    fn hello_signature_covers_the_model() {
        let mut h = sample_hello();
        h.model.as_mut().unwrap().params_b = 405;
        assert!(h.verify().is_err());
    }

    #[test]
    fn observed_address_is_outside_the_signature() {
        let mut h = sample_hello();
        h.observed = None;
        assert!(h.verify().is_ok());
    }

    #[test]
    fn truncated_hello_is_rejected_not_panicking() {
        let bytes = sample_hello().to_bytes();
        for cut in 0..bytes.len() {
            let _ = Hello::from_bytes(&bytes[..cut]);
        }
    }

    #[test]
    fn a_nine_byte_peer_list_cannot_request_a_huge_allocation() {
        let frame = [0xFF, 0xFF, 0xFF, 0xFF, 4, 1, 2, 3, 4];
        assert!(PeerList::from_bytes(&frame).is_err());
    }

    #[test]
    fn peer_list_roundtrips() {
        let list = PeerList::new(vec![PeerEntry {
            addr: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)),
            port: 443,
            last_seen_ms: 12,
        }]);
        let back = PeerList::from_bytes(&list.to_bytes()).unwrap();
        assert_eq!(back.to_socket_addrs(), list.to_socket_addrs());
    }

    #[test]
    fn dialable_prefers_ipv6_and_uses_listen_port() {
        let h = sample_hello();
        let addrs = h.dialable_addrs(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)));
        assert!(addrs[0].is_ipv6());
        assert!(addrs.iter().all(|a| a.port() == 443));
    }

    fn sample_push() -> SharePush {
        let k = key();
        let sender = k.verifying_key().to_bytes();
        let inputs = LotteryInputs {
            hour_epoch: 10,
            plaintext_len: 5,
            n_engines: 1,
            room_salt: [3u8; 32],
        };
        let mut sp = SharePush {
            version: SharePush::VERSION,
            message_id: 99,
            sender_pid: sender,
            inputs,
            candidates: vec![[9u8; 32]],
            candidate_weights: vec![50],
            share_index: 1,
            total_shares: 1,
            threshold: 1,
            share_data: vec![1, 2, 3],
            bundle_digest: [4u8; 32],
            target_channel: None,
            session_token: None,
            min_responder_maturity: None,
            vrf_seed: core_crypto::lottery::vrf_seed(99, &sender, 10),
            sent_at_ms: 1,
            custodians: vec![[5u8; 32]],
            signature: [0u8; 64],
        };
        sp.signature = k.sign(&sp.signing_digest()).to_bytes();
        sp
    }

    #[test]
    fn a_valid_share_push_validates() {
        let sp = sample_push();
        let back = SharePush::from_bytes(&sp.to_bytes()).unwrap();
        assert!(back.validate().is_ok());
    }

    #[test]
    fn an_arbitrary_vrf_seed_is_refused() {
        let k = key();
        let mut sp = sample_push();
        sp.vrf_seed = [0xAA; 32];
        sp.signature = k.sign(&sp.signing_digest()).to_bytes();
        assert!(sp.validate().is_err());
    }

    #[test]
    fn a_zero_share_count_is_an_error_not_a_panic() {
        let k = key();
        let mut sp = sample_push();
        sp.total_shares = 0;
        sp.threshold = 0;
        sp.signature = k.sign(&sp.signing_digest()).to_bytes();
        assert!(sp.validate().is_err());
    }

    #[test]
    fn private_addresses_are_dropped_unless_the_hello_came_from_one() {
        let k = key();
        let mut h = sample_hello();
        h.addrs = vec![
            IpAddr::V4(Ipv4Addr::new(10, 2, 0, 2)),
            IpAddr::V4(Ipv4Addr::new(100, 72, 1, 1)),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        ];
        h.signature = k.sign(&h.signing_digest()).to_bytes();

        let from_outside = h.dialable_addrs(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)));
        assert!(
            from_outside.iter().all(|a| !is_local_scope(&a.ip())),
            "{from_outside:?}"
        );
        assert!(from_outside.contains(&SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
            443
        )));

        let from_the_lan = h.dialable_addrs(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(from_the_lan.contains(&SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(10, 2, 0, 2)),
            443
        )));
    }

    #[test]
    fn a_peer_behind_the_same_router_is_dialed_on_the_lan() {
        let mut h = sample_hello();
        h.addrs = vec![
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30)),
        ];
        // Heard of through a server: its LAN address names another network.
        let via_server = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));
        let others = [IpAddr::V4(Ipv4Addr::new(198, 51, 100, 50))];
        assert!(!h
            .dialable_addrs_from(via_server, &others)
            .iter()
            .any(|a| a.ip() == IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))));
        // Same public address as this node: the same router, so the same LAN.
        let mine = [IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))];
        assert!(h.behind_same_router(&mine));
        assert!(h
            .dialable_addrs_from(via_server, &mine)
            .iter()
            .any(|a| a.ip() == IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))));
    }

    #[test]
    fn local_scope_covers_private_cgnat_and_link_local() {
        for ip in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.0.1",
            "100.64.0.1",
            "100.127.255.1",
            "169.254.1.1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(is_local_scope(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.128.0.1", "2a07:b944::2:2", "93.189.89.201"] {
            assert!(!is_local_scope(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_push_names_one_custodian_per_share_and_its_own_holder() {
        let k = key();
        let mut sp = sample_push();
        assert_eq!(sp.custodian(), Some([5u8; 32]));
        sp.custodians.push([6u8; 32]);
        sp.signature = k.sign(&sp.signing_digest()).to_bytes();
        assert!(sp.validate().is_err(), "two custodians for one share");
    }

    #[test]
    fn the_custodian_list_is_signed() {
        let mut sp = sample_push();
        sp.custodians = vec![[6u8; 32]];
        assert!(sp.validate().is_err());
    }

    #[test]
    fn presence_roundtrips_and_refuses_oversized_lists() {
        let p = Presence {
            hellos: vec![sample_hello(), sample_hello()],
        };
        let back = Presence::from_bytes(&p.to_bytes()).unwrap();
        assert_eq!(back.hellos.len(), 2);
        assert!(back.hellos.iter().all(|h| h.verify().is_ok()));

        let too_many = Presence {
            hellos: vec![sample_hello(); MAX_PRESENCE_ENTRIES + 1],
        };
        assert!(Presence::from_bytes(&too_many.to_bytes()).is_err());
    }

    #[test]
    fn relay_only_forwards_signed_payload_types() {
        let mut ro = RelayOpen {
            version: 1,
            origin_pid: [0u8; 32],
            dest_pid: [1u8; 32],
            message_id: 1,
            hop_ttl: 1,
            inner_msg_type: message_types::SHARE_PUSH,
            inner_bytes: Vec::new(),
        };
        assert!(ro.inner_type_is_relayable());
        ro.inner_msg_type = message_types::CHAIN_TX;
        assert!(!ro.inner_type_is_relayable());
    }
}
