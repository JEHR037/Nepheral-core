//! Frozen wire bytes.
//!
//! postcard encodes fields by position, with no names or tags, so adding,
//! removing or reordering a field silently changes what every other node
//! reads. Signing digests are frozen too: a changed preimage makes every
//! signature from an older node fail. When a check here fails, the change
//! breaks compatibility with the deployed network. Make it on purpose (bump
//! the message's `VERSION` and `PROTOCOL_VERSION`, or keep the old layout),
//! then update the constant.

use core_crypto::agent_crypto::SealedMessage;
use core_crypto::block::BlockHeader;
use core_crypto::finality::{FinalityVote, Justification};
use core_crypto::network::{set_active, Network};
use core_crypto::receipts::{CustodyAttestation, ServiceClaim};
use mesh_network::protocol::{
    AgentDecline, AgentResponseDirect, Hello, LotteryInputs, ModelInfo, Presence, PunchIntro,
    PunchRequest, SharePush, SignedDecline,
};
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hello() -> Hello {
    Hello {
        version: 5,
        peer_id: [0x11; 32],
        listen_port: 4433,
        caps: 0b11,
        model: Some(ModelInfo {
            name: "qwen2.5-3b-instruct-q4_k_m".into(),
            tier: 2,
            hash: [0x22; 32],
            params_b: 3,
        }),
        addrs: vec![
            "203.0.113.7".parse().unwrap(),
            "2001:db8::7".parse().unwrap(),
        ],
        sent_at_ms: 1_790_000_000_000,
        observed: Some("198.51.100.9:50123".parse().unwrap()),
        signature: [0x33; 64],
    }
}

fn inputs() -> LotteryInputs {
    LotteryInputs {
        hour_epoch: 497_222,
        plaintext_len: 1_234,
        n_engines: 3,
        room_salt: [0x44; 32],
    }
}

fn share_push() -> SharePush {
    SharePush {
        version: SharePush::VERSION,
        message_id: 0x0102_0304_0506_0708,
        sender_pid: [0x55; 32],
        inputs: inputs(),
        candidates: vec![[0x66; 32], [0x67; 32]],
        candidate_weights: vec![3, 1],
        share_index: 2,
        total_shares: 3,
        threshold: 2,
        share_data: vec![9, 8, 7, 6, 5],
        bundle_digest: [0x77; 32],
        target_channel: Some("chat".into()),
        session_token: Some([0x78; 32]),
        min_responder_maturity: Some(42),
        vrf_seed: [0x79; 32],
        sent_at_ms: 1_790_000_000_500,
        custodians: vec![[0x7a; 32], [0x7b; 32], [0x7c; 32]],
        signature: [0x7d; 64],
    }
}

fn claim() -> ServiceClaim {
    ServiceClaim {
        query_digest: [0x88; 32],
        sender_pid: [0x55; 32],
        responder_pid: [0x66; 32],
        hour_epoch: 497_222,
        model_hash: [0x22; 32],
        model_params_b: 3,
        output_tokens: 321,
        gen_ms: 4_567,
        responder_sig: [0x89; 64],
    }
}

fn custody() -> CustodyAttestation {
    CustodyAttestation {
        custodians: vec![[0x7a; 32], [0x7b; 32]],
        responder_sig: [0x8a; 64],
    }
}

fn response() -> AgentResponseDirect {
    AgentResponseDirect {
        version: AgentResponseDirect::VERSION,
        query_message_id: 0x0102_0304_0506_0708,
        query_digest: [0x88; 32],
        responder_pid: [0x66; 32],
        sealed: SealedMessage {
            eph_pub: [0x99; 32],
            nonce: [0x9a; 24],
            ciphertext: vec![1, 2, 3, 4],
        },
        service_claim: claim(),
        custody: Some(custody()),
        signature: [0x9b; 64],
    }
}

fn vote() -> FinalityVote {
    FinalityVote {
        height: 6_560,
        block_hash: [0xaa; 32],
        voter: [0xab; 32],
        signature: [0xac; 64],
    }
}

fn header() -> BlockHeader {
    BlockHeader {
        version: 1,
        height: 6_561,
        slot: 895_000_000,
        parent_hash: [0xb1; 32],
        state_root: [0xb2; 32],
        extrinsics_root: [0xb3; 32],
        lottery_root: [0xb4; 32],
        author: [0xb5; 32],
        signature: [0xb6; 64],
        timestamp_ms: 1_790_000_002_000,
    }
}

fn signed_decline() -> SignedDecline {
    SignedDecline {
        version: SignedDecline::VERSION,
        sender_pid: [0x55; 32],
        message_id: 7,
        rank: 1,
        decliner: [0x66; 32],
        signature: [0xd1; 64],
    }
}

/// Every frozen value, by name. A failure lists every mismatch at once, so
/// one run shows the whole extent of a change.
fn actual() -> Vec<(&'static str, String)> {
    set_active(Network::Testnet);
    let justification = Justification {
        height: 6_560,
        block_hash: [0xaa; 32],
        votes: vec![([0xab; 32], core_crypto::finality::Signed([0xac; 64]))],
    };
    vec![
        ("hello.bytes", sha(&hello().to_bytes())),
        ("hello.digest", hex::encode(hello().signing_digest())),
        (
            "presence.bytes",
            sha(&Presence {
                hellos: vec![hello()],
            }
            .to_bytes()),
        ),
        ("share_push.bytes", sha(&share_push().to_bytes())),
        (
            "share_push.digest",
            hex::encode(share_push().signing_digest()),
        ),
        ("lottery_inputs.digest", hex::encode(inputs().digest())),
        (
            "punch_request.bytes",
            sha(&PunchRequest {
                version: 1,
                target: [0xc1; 32],
            }
            .to_bytes()),
        ),
        (
            "punch_intro.bytes",
            sha(&PunchIntro {
                version: 1,
                peer: [0xc2; 32],
                addr: "198.51.100.9:50123".parse().unwrap(),
            }
            .to_bytes()),
        ),
        (
            "agent_decline.bytes",
            sha(&AgentDecline {
                version: 1,
                sender_pid: [0x55; 32],
                message_id: 7,
                rank: 1,
            }
            .to_bytes()),
        ),
        ("agent_response.bytes", sha(&response().to_bytes())),
        (
            "agent_response.digest",
            hex::encode(response().signing_digest()),
        ),
        ("service_claim.digest", hex::encode(claim().claim_digest())),
        ("custody.bytes", sha(&core_crypto::wire::encode(&custody()))),
        (
            "finality_vote.bytes",
            sha(&core_crypto::wire::encode(&vote())),
        ),
        (
            "justification.bytes",
            sha(&core_crypto::wire::encode(&justification)),
        ),
        (
            "block_header.bytes",
            sha(&core_crypto::wire::encode(&header())),
        ),
        ("signed_decline.bytes", sha(&signed_decline().to_bytes())),
        (
            "signed_decline.digest",
            hex::encode(signed_decline().signing_digest()),
        ),
    ]
}

/// As deployed in 0.5.1 (protocol v5, testnet), plus what later releases add.
const EXPECTED: &[(&str, &str)] = &[
    (
        "hello.bytes",
        "648b2eabd764c71dce1368d43c70d204a59b45b3c8fca5dd0908487f840c88aa",
    ),
    (
        "hello.digest",
        "e01d133a2ed1169ce365464ebfb7fbebc98c4798302ebf59937f78e60d920fd3",
    ),
    (
        "presence.bytes",
        "655eb3d2c5c232d508151183a2ed287f87a73ef240b14870431fe4237c823bd3",
    ),
    (
        "share_push.bytes",
        "1107c4843257e3b07fb285befc166fe19482bb3c0fcce2954a9f8213e1eed541",
    ),
    (
        "share_push.digest",
        "d731d31aa77be9c26db9dd950447c397ed7dfbc281d98e1210dc7d67a2c0bab8",
    ),
    (
        "lottery_inputs.digest",
        "717cabfc83843bfca4aa9735f4f3fec8ffca4994669e1f5ef58c15d78bfa09f1",
    ),
    (
        "punch_request.bytes",
        "fdf40a40f23062cbe2257616e5e635a274f8459a269118d2985ba4cec4609902",
    ),
    (
        "punch_intro.bytes",
        "312701d0ccac3bf088bc174f7dde4445f06ac9e808951e03e33221615dda7c85",
    ),
    (
        "agent_decline.bytes",
        "a2d28ded77149c6fd9f34f2a95a1a42497a1d4a731759820a3bde226d42e0916",
    ),
    (
        "agent_response.bytes",
        "511bf5cfbd9492d2ee9105a1af9e02b12a7bc68887f3894c2d09d6d77a994751",
    ),
    (
        "agent_response.digest",
        "b262ee75e5e7ad8ff603e05c8f379b1c39c91a2f62fc1b561d36f77b00525f08",
    ),
    (
        "service_claim.digest",
        "337d23bccefa7f1758b3e4dda9035691925c4342edb6d388bd5a7b43e3707670",
    ),
    (
        "custody.bytes",
        "2c3ba50cb42ec520a1c3af68b90770eebcaf780b66cbd2adb46afed96713a1f8",
    ),
    (
        "finality_vote.bytes",
        "51f364e7f3d48e33e8c6cdaf7788162d0801bfbd4984bb6c0d242f30ae44e8ec",
    ),
    (
        "justification.bytes",
        "498203ec2b3cbf886b82f4c038cc782816f3f2953e0de517281da7dd099ff523",
    ),
    (
        "block_header.bytes",
        "cbb8c6a106b088a09ca4f1e041ace353aa79ae1ac80935388af941c9a7c7b624",
    ),
    // New in 0.5.3.
    (
        "signed_decline.bytes",
        "080181f754a0ee2e02f17e3b35121cc48e031b91778f4e3d3f1e6b50c221dfed",
    ),
    (
        "signed_decline.digest",
        "a6bccf745c5caa8160064a6814f89b368c4094dc8e85d216003467c03ef88033",
    ),
];

#[test]
fn wire_formats_and_signing_digests_are_unchanged() {
    let actual = actual();
    let mut mismatches = Vec::new();
    for (name, got) in &actual {
        let want = EXPECTED
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or("<missing>");
        if want != got {
            mismatches.push(format!("    (\"{name}\", \"{got}\"), // was {want}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "wire bytes changed; if on purpose, update EXPECTED:\n{}",
        mismatches.join("\n")
    );
}
