// SPDX-License-Identifier: BUSL-1.1
#![cfg(feature = "node_runtime")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_engine::{InferenceBackend, InferenceEngine, Message, ModelConfig};
use app_kernel::state::AppState;

struct EchoBackend {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl InferenceBackend for EchoBackend {
    async fn health_check(&self) -> bool {
        true
    }

    async fn chat(
        &self,
        messages: &[Message],
        _config: &ModelConfig,
    ) -> Result<(String, usize, u64), ai_engine::backend::InferenceError> {
        let transcript: String = messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.seen.lock().unwrap().push(transcript);
        let last = messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok((format!("ECHO {last}"), 8, 5))
    }

    fn backend_name(&self) -> &str {
        "echo"
    }
}

struct Node {
    state: AppState,
    addr: std::net::SocketAddr,
}

fn test_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("nepheral-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        home
    })
}

async fn spawn_node(port: u16) -> Node {
    let home = test_home();
    let mnemonic = core_crypto::NodeMnemonic::generate();
    let identity = core_crypto::NodeIdentity::from_mnemonic(&mnemonic).unwrap();
    let tls = app_kernel::boot::tls_identity(&identity);
    let bound = app_kernel::boot::bind_mesh(port, &tls).await.expect("bind");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", bound.listen_addr.port())
        .parse()
        .unwrap();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&mnemonic.to_seed());
    let mut state = AppState::new(addr, Vec::new(), signing_key, tls);
    state.data_dir = home.join(format!("node-{port}"));
    app_kernel::node_runtime::install_stream_handler(&state);
    state
        .install_mesh_endpoints(bound.endpoint.endpoint_v4(), bound.endpoint.endpoint_v6())
        .await;
    let s = state.clone();
    let endpoint = bound.endpoint;
    tokio::spawn(async move {
        let _ = app_kernel::node_runtime::run_node_with_state(endpoint, s).await;
    });
    Node { state, addr }
}

async fn give_engine(node: &Node) -> Arc<Mutex<Vec<String>>> {
    give_engine_named(node, "echo-8b").await
}

/// The model name sets the size the node reports, and with it what each
/// answer earns.
async fn give_engine_named(node: &Node, model_name: &str) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = InferenceEngine::new_with_backend(
        ModelConfig {
            model_name: model_name.into(),
            context_window: 4096,
            max_tokens: 256,
            ..Default::default()
        },
        Arc::new(EchoBackend { seen: seen.clone() }),
    );
    engine.initialize().await.unwrap();
    *node.state.ai_engine.write().await = Some(Arc::new(engine));
    // What the node advertises and signs into its receipts.
    node.state.ai_model_config.write().await.model_name = model_name.into();
    seen
}

/// How long a question that must be answered may take. As generous as
/// `wait_until`, for the same reason: answered in a few seconds on an
/// idle machine, the 30 s these used to allow ran out on a loaded laptop
/// with eighteen of these tests running at once.
const ANSWER_DEADLINE: Duration = Duration::from_secs(90);

async fn wait_until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    // Generous on purpose. These tests settle in about a second when the
    // machine is idle; the ceiling only bounds how long a genuine failure
    // takes to report. A shared CI runner mid-build needs the headroom —
    // at 20s this timed out on a loaded laptop while the rest of the
    // workspace was compiling.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_queries_and_encrypted_rooms_work_end_to_end() {
    test_home();

    let a = spawn_node(47311).await;
    let b = spawn_node(47312).await;
    let c = spawn_node(47313).await;
    let seen_b = give_engine(&b).await;
    let seen_c = give_engine(&c).await;

    a.state.dial(b.addr, None).await.expect("a->b");
    a.state.dial(c.addr, None).await.expect("a->c");
    b.state.dial(c.addr, None).await.expect("b->c");

    wait_until("A to see two AI-capable peers", || async {
        a.state
            .peer_capabilities
            .read()
            .await
            .values()
            .filter(|c| c.has_ai)
            .count()
            == 2
    })
    .await;
    wait_until("B and C to know each other", || async {
        b.state.live_connections().await.len() >= 2 && c.state.live_connections().await.len() >= 2
    })
    .await;

    let answer = a
        .state
        .ask_mesh(
            "my email is ana@example.com — what is 2+2?",
            None,
            None,
            ANSWER_DEADLINE,
        )
        .await
        .expect("mesh answer");
    assert!(answer.starts_with("ECHO"), "{answer}");
    assert!(
        answer.contains("ana@example.com"),
        "the placeholder is restored on the sender: {answer}"
    );
    let transcripts: Vec<String> = seen_b
        .lock()
        .unwrap()
        .iter()
        .chain(seen_c.lock().unwrap().iter())
        .cloned()
        .collect();
    assert!(!transcripts.is_empty(), "a responder ran inference");
    assert!(
        transcripts.iter().all(|t| !t.contains("ana@example.com")),
        "no responder ever saw the real email"
    );

    let code = mesh_network::rooms::RoomCode::generate();
    a.state.join_room(code.clone()).await.unwrap();
    b.state.join_room(code.clone()).await.unwrap();
    wait_until("A to see B as a room member", || async {
        a.state
            .rooms
            .read()
            .await
            .get(code.as_str())
            .is_some_and(|r| !r.members.is_empty())
    })
    .await;

    a.state.send_room_message(&code, "hola sala").await.unwrap();
    wait_until("B to read A's room message", || async {
        b.state
            .rooms
            .read()
            .await
            .get(code.as_str())
            .is_some_and(|r| r.chat.iter().any(|e| e.content == "hola sala"))
    })
    .await;
    assert!(
        c.state.rooms.read().await.is_empty(),
        "a node without the code relays but never learns the room"
    );

    b.state
        .send_room_message(&code, "@agente resume la sala")
        .await
        .unwrap();
    wait_until("the agent's answer to reach A through the room", || async {
        a.state
            .rooms
            .read()
            .await
            .get(code.as_str())
            .is_some_and(|r| {
                r.chat
                    .iter()
                    .any(|e| e.is_ai && e.content.starts_with("ECHO"))
            })
    })
    .await;

    a.state
        .send_room_message(&code, "no mention here")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ai_count = a
        .state
        .rooms
        .read()
        .await
        .get(code.as_str())
        .map(|r| r.chat.iter().filter(|e| e.is_ai).count())
        .unwrap_or(0);
    assert_eq!(
        ai_count, 1,
        "a message without a mention spends no inference"
    );

    a.state.leave_room(code.as_str()).await;
    assert!(a.state.rooms.read().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_where_nobody_contributes_compute_closes_after_the_grace() {
    test_home();

    let lonely = spawn_node(47321).await;
    let code = mesh_network::rooms::RoomCode::generate();
    lonely.state.join_room(code.clone()).await.unwrap();

    lonely.state.sweep_rooms(false).await;
    assert!(
        lonely.state.rooms.read().await.contains_key(code.as_str()),
        "the grace period starts, the room survives the first sweep"
    );
    {
        let mut rooms = lonely.state.rooms.write().await;
        let room = rooms.get_mut(code.as_str()).unwrap();
        room.no_compute_since = Some(
            std::time::Instant::now()
                - app_kernel::rooms::NO_COMPUTE_GRACE
                - Duration::from_secs(1),
        );
    }
    lonely.state.sweep_rooms(false).await;
    assert!(
        lonely.state.rooms.read().await.is_empty(),
        "past the grace, a room with no compute is destroyed"
    );

    let contributor = spawn_node(47322).await;
    let _seen = give_engine(&contributor).await;
    let kept = mesh_network::rooms::RoomCode::generate();
    contributor.state.join_room(kept.clone()).await.unwrap();
    contributor.state.sweep_rooms(false).await;
    let room_alive = contributor
        .state
        .rooms
        .read()
        .await
        .get(kept.as_str())
        .is_some_and(|r| r.no_compute_since.is_none());
    assert!(
        room_alive,
        "a member contributing compute keeps the room alive"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_chain_keeps_running_after_the_founder_resigns_and_goes_offline() {
    use core_crypto::extrinsic::{PALLET_AUTHORITY, PALLET_BALANCES};
    use core_crypto::pallets::authority::AuthorityCall;
    use core_crypto::pallets::balances::BalancesCall;

    test_home();

    let founder = spawn_node(47331).await;
    let partner = spawn_node(47332).await;
    let founder_key = founder.state.signing_key.clone();
    let founder_pid = founder_key.verifying_key().to_bytes();
    let partner_pid = partner.state.signing_key.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(founder_pid);
    chain.governance.oracle_replaced_by_vote = true;
    let add = core_crypto::Extrinsic::sign(
        &founder_key,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: partner_pid,
            channels: vec!["chat".into()],
            attestation_hash: [0u8; 32],
        },
        founder_pid,
        1,
    );
    let dissolve = core_crypto::Extrinsic::sign(
        &founder_key,
        PALLET_AUTHORITY,
        &AuthorityCall::Dissolve,
        founder_pid,
        2,
    );
    let slot = app_kernel::state::now_ms() / core_crypto::block::SLOT_DURATION_MS - 10;
    let mut block = core_crypto::Block {
        header: core_crypto::block::BlockHeader {
            version: 1,
            height: 1,
            slot,
            parent_hash: chain.head_hash(),
            state_root: [0u8; 32],
            extrinsics_root: [0u8; 32],
            lottery_root: [0u8; 32],
            author: core_crypto::validators::authority_peer_id(&founder_key.verifying_key()),
            signature: [0u8; 64],
            timestamp_ms: slot * core_crypto::block::SLOT_DURATION_MS,
        },
        body: core_crypto::block::BlockBody {
            extrinsics: vec![add, dissolve],
            lottery_results: Vec::new(),
        },
    };
    block.header.extrinsics_root = block.extrinsics_root();
    block.header.lottery_root = block.lottery_root();
    let mut probe = chain.clone();
    probe.apply_block_unverified(block.clone()).unwrap();
    block.header.state_root = probe.state_root();
    block.header.sign(&founder_key);
    chain.apply_block(block).unwrap();
    assert!(chain.authority.is_dissolved());
    assert_eq!(chain.validator_set().len(), 2);

    *founder.state.chain.write().await = chain.clone();
    *partner.state.chain.write().await = chain;
    tokio::spawn(founder.state.clone().run_block_producer());
    tokio::spawn(partner.state.clone().run_block_producer());
    founder
        .state
        .dial(partner.addr, None)
        .await
        .expect("founder->partner");
    wait_until("both nodes to see each other", || async {
        !founder.state.live_connections().await.is_empty()
            && !partner.state.live_connections().await.is_empty()
    })
    .await;

    founder
        .state
        .sign_and_submit(
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: partner_pid,
                amount: 10,
            },
        )
        .await;
    wait_until("the transfer to land on both nodes", || async {
        founder.state.chain.read().await.balance_of(&partner_pid) == 10
            && partner.state.chain.read().await.balance_of(&partner_pid) == 10
    })
    .await;

    founder.state.begin_shutdown().await;
    let height_before = partner.state.chain.read().await.head_height();
    partner
        .state
        .sign_and_submit(
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: founder_pid,
                amount: 3,
            },
        )
        .await;
    wait_until("the partner alone to produce the next block", || async {
        let chain = partner.state.chain.read().await;
        chain.balance_of(&partner_pid) == 7 && chain.head_height() > height_before
    })
    .await;
}

fn seal_block(
    chain: &core_crypto::ChainState,
    key: &ed25519_dalek::SigningKey,
    slot: u64,
    extrinsics: Vec<core_crypto::Extrinsic>,
) -> core_crypto::Block {
    let mut block = core_crypto::Block {
        header: core_crypto::block::BlockHeader {
            version: 1,
            height: chain.head_height() + 1,
            slot,
            parent_hash: chain.head_hash(),
            state_root: [0u8; 32],
            extrinsics_root: [0u8; 32],
            lottery_root: [0u8; 32],
            author: core_crypto::validators::authority_peer_id(&key.verifying_key()),
            signature: [0u8; 64],
            timestamp_ms: slot * core_crypto::block::SLOT_DURATION_MS,
        },
        body: core_crypto::block::BlockBody {
            extrinsics,
            lottery_results: Vec::new(),
        },
    };
    block.header.extrinsics_root = block.extrinsics_root();
    block.header.lottery_root = block.lottery_root();
    let mut probe = chain.clone();
    probe.apply_block_unverified(block.clone()).unwrap();
    block.header.state_root = probe.state_root();
    block.header.sign(key);
    block
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_of_four_validators_finalize_blocks_while_one_is_offline() {
    use core_crypto::extrinsic::PALLET_AUTHORITY;
    use core_crypto::pallets::authority::AuthorityCall;

    test_home();

    let a = spawn_node(47341).await;
    let b = spawn_node(47342).await;
    let c = spawn_node(47343).await;
    let offline = ed25519_dalek::SigningKey::from_bytes(&[0x77u8; 32]);
    let founder = a.state.signing_key.clone();
    let founder_pid = founder.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(founder_pid);
    let members = [
        b.state.signing_key.verifying_key().to_bytes(),
        c.state.signing_key.verifying_key().to_bytes(),
        offline.verifying_key().to_bytes(),
    ];
    let adds: Vec<core_crypto::Extrinsic> = members
        .iter()
        .enumerate()
        .map(|(i, pk)| {
            core_crypto::Extrinsic::sign(
                &founder,
                PALLET_AUTHORITY,
                &AuthorityCall::Add {
                    pubkey: *pk,
                    channels: vec!["chat".into()],
                    attestation_hash: [0u8; 32],
                },
                founder_pid,
                i as u64 + 1,
            )
        })
        .collect();
    let now_slot = app_kernel::state::now_ms() / core_crypto::block::SLOT_DURATION_MS;
    let mut slot = now_slot - now_slot % 4 - 400;
    let first = seal_block(&chain, &founder, slot, adds);
    chain.apply_block(first).unwrap();
    assert_eq!(chain.validator_set().len(), 4);
    while chain.head_height() < 17 {
        slot += 4;
        let block = seal_block(&chain, &founder, slot, Vec::new());
        chain.apply_block(block).unwrap();
    }

    for node in [&a, &b, &c] {
        *node.state.chain.write().await = chain.clone();
        tokio::spawn(node.state.clone().run_block_producer());
        tokio::spawn(node.state.clone().run_finality_voter());
    }
    a.state.dial(b.addr, None).await.expect("a->b");
    a.state.dial(c.addr, None).await.expect("a->c");
    b.state.dial(c.addr, None).await.expect("b->c");

    wait_until("height 16 to be final on every live validator", || async {
        let mut all = true;
        for node in [&a, &b, &c] {
            all &= node.state.chain.read().await.finalized().0 == 16;
        }
        all
    })
    .await;
    let expected = chain.hash_at(16).unwrap();
    for node in [&a, &b, &c] {
        assert_eq!(node.state.chain.read().await.finalized().1, expected);
    }
    // Only the producer of the next block submits the justification. No
    // validator may keep one at or below the final height in its resend list,
    // where a losing copy used to be rebroadcast and rejected for minutes.
    for node in [&a, &b, &c] {
        node.state.prune_inflight_txs().await;
        let finalized = node.state.chain.read().await.finalized().0;
        let stale = node
            .state
            .my_inflight_txs
            .lock()
            .await
            .iter()
            .filter(|(_, e)| app_kernel::chain_node::finalizes_no_later_than(e, finalized))
            .count();
        assert_eq!(stale, 0, "no justification at or below the final height");
    }

    let offline_pid = offline.verifying_key().to_bytes();
    let first = core_crypto::finality::FinalityVote::sign(&offline, 32, [1u8; 32]);
    let second = core_crypto::finality::FinalityVote::sign(&offline, 32, [2u8; 32]);
    assert!(a.state.handle_finality_vote(first).await);
    assert!(!a.state.handle_finality_vote(second).await);
    wait_until("the equivocating validator to be removed", || async {
        let mut all = true;
        for node in [&a, &b, &c] {
            all &= !node
                .state
                .chain
                .read()
                .await
                .authority
                .is_authority_signer(&offline_pid);
        }
        all
    })
    .await;
    assert_eq!(a.state.chain.read().await.validator_set().len(), 3);

    let path = a.state.data_dir.join("chain.bin");
    std::fs::create_dir_all(&a.state.data_dir).unwrap();
    *a.state.chain_path.write().await = Some(path.clone());
    a.state.persist_chain_now().await.unwrap();
    let written = std::fs::metadata(&path).unwrap().len();
    let before = a.state.chain.read().await.head_height();
    a.state
        .sign_and_submit(
            core_crypto::extrinsic::PALLET_BALANCES,
            &core_crypto::pallets::balances::BalancesCall::Transfer {
                to: members[0],
                amount: 1,
            },
        )
        .await;
    wait_until("another block on the founder node", || async {
        a.state.chain.read().await.head_height() > before
    })
    .await;
    a.state.persist_chain_now().await.unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > written);
    let data = std::fs::read(&path).unwrap();
    let blocks = core_crypto::chain_file::strip_header(&data)
        .and_then(core_crypto::ChainState::deserialize_blocks)
        .expect("appended file parses");
    let mut replayed = core_crypto::ChainState::with_genesis_authority(founder_pid);
    for block in blocks {
        replayed.apply_block(block).unwrap();
    }
    let live = a.state.chain.read().await;
    assert_eq!(replayed.head_height(), live.head_height());
    assert_eq!(replayed.state_root(), live.state_root());
}

/// The whole paid path on a live mesh: a user burns N-AI for a Top-Tier
/// session, only the node registered for that channel answers, the user's
/// receipt credits the answer to that node, and the node turns it into
/// N-AI it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_top_tier_session_pays_the_node_that_answers_it() {
    use core_crypto::extrinsic::{PALLET_AUTHORITY, PALLET_NAI};
    use core_crypto::pallets::authority::AuthorityCall;
    use core_crypto::pallets::nai::{
        NaiCall, DEFAULT_CHANNEL_SESSION_COST, INFERENCES_PER_SESSION,
    };

    test_home();

    let user = spawn_node(47351).await;
    let top = spawn_node(47352).await;
    let other = spawn_node(47353).await;
    let user_pid = user.state.signing_key.verifying_key().to_bytes();
    let top_pid = top.state.signing_key.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(user_pid);
    let add = core_crypto::Extrinsic::sign(
        &user.state.signing_key,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: top_pid,
            channels: vec!["frontier".into()],
            attestation_hash: [0u8; 32],
        },
        user_pid,
        1,
    );
    let slot = app_kernel::state::now_ms() / core_crypto::block::SLOT_DURATION_MS - 10;
    let first = seal_block(&chain, &user.state.signing_key, slot, vec![add]);
    chain.apply_block(first).unwrap();
    assert!(chain.authority.serves_channel(&top_pid, "frontier"));

    for node in [&user, &top, &other] {
        *node.state.chain.write().await = chain.clone();
    }
    tokio::spawn(user.state.clone().run_block_producer());
    tokio::spawn(top.state.clone().run_block_producer());
    let seen_top = give_engine_named(&top, "echo-70b").await;
    let seen_other = give_engine_named(&other, "echo-8b").await;

    user.state.dial(top.addr, None).await.expect("user->top");
    user.state
        .dial(other.addr, None)
        .await
        .expect("user->other");
    top.state.dial(other.addr, None).await.expect("top->other");
    wait_until("the user to see both AI nodes", || async {
        user.state
            .peer_capabilities
            .read()
            .await
            .values()
            .filter(|c| c.has_ai)
            .count()
            == 2
    })
    .await;

    // Paying needs the wallet open; this node is its own wallet.
    user.state
        .unlock_wallet(
            app_kernel::wallet::WalletKey::from_signing_key((*user.state.signing_key).clone()),
            None,
        )
        .await;
    let before = user.state.chain.read().await.balance_of(&user_pid);
    let token = user
        .state
        .open_session_onchain(Some("frontier".into()))
        .await
        .expect("session opens");
    {
        let chain = user.state.chain.read().await;
        assert_eq!(
            chain.balance_of(&user_pid),
            before - DEFAULT_CHANNEL_SESSION_COST,
            "opening the session burns its price"
        );
        assert_eq!(chain.nai.total_spent, DEFAULT_CHANNEL_SESSION_COST);
        let session = chain.nai.session(&token).expect("session on chain");
        assert_eq!(session.channel.as_deref(), Some("frontier"));
        assert_eq!(session.inferences_remaining, INFERENCES_PER_SESSION);
    }

    let answer = user
        .state
        .ask_mesh(
            "explain entropy",
            Some("frontier".into()),
            Some(token),
            ANSWER_DEADLINE,
        )
        .await
        .expect("top-tier answer");
    assert!(answer.starts_with("ECHO"), "{answer}");
    assert!(
        !seen_top.lock().unwrap().is_empty(),
        "the top-tier node answered"
    );
    assert!(
        seen_other.lock().unwrap().is_empty(),
        "a node outside the channel never sees a paid query"
    );

    user.state
        .sign_and_submit(
            PALLET_NAI,
            &NaiCall::UseSession {
                session_token: token,
            },
        )
        .await;
    wait_until("the receipt and the metering to land", || async {
        let chain = top.state.chain.read().await;
        chain.wins_of(&top_pid) == 1
            && chain
                .nai
                .session(&token)
                .is_some_and(|s| s.inferences_remaining == INFERENCES_PER_SESSION - 1)
    })
    .await;

    let due = {
        let chain = top.state.chain.read().await;
        let (shares, dim) = chain.redeemable(&top_pid);
        assert!(dim > 0, "the answer was credited to the node that gave it");
        app_kernel::chain_node::redeem_due(shares, dim, chain.economy.current_shares_per_credit)
    };
    let (shares, dim) = due.expect("a 70B answer is worth at least one credit");
    let minted_before = top.state.chain.read().await.nai.total_minted;
    top.state.request_redeem(shares, dim).await;
    wait_until("the top-tier node to hold what it earned", || async {
        top.state.chain.read().await.balance_of(&top_pid) > 0
    })
    .await;
    let chain = top.state.chain.read().await;
    let earned = chain.balance_of(&top_pid);
    assert_eq!(earned, dim / chain.economy.current_shares_per_credit);
    assert_eq!(chain.nai.total_minted, minted_before + earned);
}

/// History the market gate accepts: six days of contribution, enough shares
/// and one lottery win.
fn grant_standing(chain: &mut core_crypto::ChainState, peer: [u8; 32]) {
    use core_crypto::pallets::market::{MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS};
    let now = app_kernel::state::now_ms();
    chain
        .contributions
        .first_seen_ms
        .insert(peer, now - (MIN_DAYS_ACTIVE + 1) * 24 * 60 * 60 * 1_000);
    chain.contributions.share_counts.insert(peer, MIN_SHARES);
    chain.contributions.wins_by_peer.insert(peer, MIN_WINS);
}

/// The founder burns its treasury down to a target once; asked again it
/// burns nothing, and a node whose key is not the founder's burns nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_founder_burns_its_treasury_down_to_a_target_once() {
    test_home();

    let founder = spawn_node(47361).await;
    let other = spawn_node(47362).await;
    let founder_key = founder.state.signing_key.clone();
    let founder_pid = founder_key.verifying_key().to_bytes();

    let chain = core_crypto::ChainState::with_genesis_authority(founder_pid);
    let minted = chain.nai.total_minted;
    for node in [&founder, &other] {
        *node.state.chain.write().await = chain.clone();
    }
    let mut authority = founder.state.clone();
    authority.authority_key = Some(founder_key);
    tokio::spawn(authority.clone().run_block_producer());

    let burned = authority.apply_founder_keep(100_000).await;
    assert_eq!(burned, minted - 100_000);
    wait_until("the burn to land", || async {
        founder.state.chain.read().await.balance_of(&founder_pid) == 100_000
    })
    .await;
    {
        let chain = founder.state.chain.read().await;
        assert_eq!(
            chain.nai.total_spent, burned,
            "a burn counts as burned supply"
        );
    }
    assert_eq!(authority.apply_founder_keep(100_000).await, 0);

    let mut impostor = other.state.clone();
    impostor.authority_key = Some(other.state.signing_key.clone());
    assert_eq!(impostor.apply_founder_keep(0).await, 0);
}

/// What a headless node does every ten minutes: redeem what it earned, never
/// a second time while the first is in flight, and nothing below a credit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_redeems_its_earnings_once_and_never_twice_in_flight() {
    test_home();

    let producer = spawn_node(47363).await;
    let worker = spawn_node(47364).await;
    let producer_pid = producer.state.signing_key.verifying_key().to_bytes();
    let worker_pid = worker.state.signing_key.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(producer_pid);
    chain.economy.current_shares_per_credit = 100;
    chain.contributions.record_lottery_win(worker_pid, 917);
    for node in [&producer, &worker] {
        *node.state.chain.write().await = chain.clone();
    }
    tokio::spawn(producer.state.clone().run_block_producer());
    worker
        .state
        .dial(producer.addr, None)
        .await
        .expect("worker->producer");
    wait_until("the two nodes to see each other", || async {
        !worker.state.live_connections().await.is_empty()
            && !producer.state.live_connections().await.is_empty()
    })
    .await;

    assert!(worker.state.redeem_if_due().await, "917 at 100 is due");
    assert!(
        !worker.state.redeem_if_due().await,
        "nothing more while the first is in flight"
    );
    wait_until("the redeem to land", || async {
        worker.state.chain.read().await.balance_of(&worker_pid) == 9
    })
    .await;
    worker.state.prune_inflight_txs().await;
    assert_eq!(
        worker.state.chain.read().await.redeemable(&worker_pid),
        (0, 17)
    );
    assert!(
        !worker.state.redeem_if_due().await,
        "17 left is under a credit"
    );
}

/// A node whose inference slot is taken declines at once, and every query
/// is answered by someone else instead of waiting it out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_node_declines_and_the_query_is_answered_elsewhere() {
    test_home();

    let asker = spawn_node(47365).await;
    let busy = spawn_node(47366).await;
    let free = spawn_node(47367).await;
    let seen_busy = give_engine(&busy).await;
    let seen_free = give_engine(&free).await;
    *busy.state.inference_busy.lock().await = true;

    asker.state.dial(busy.addr, None).await.expect("a->busy");
    asker.state.dial(free.addr, None).await.expect("a->free");
    busy.state.dial(free.addr, None).await.expect("busy->free");
    wait_until("the asker to see both AI nodes", || async {
        asker
            .state
            .peer_capabilities
            .read()
            .await
            .values()
            .filter(|c| c.has_ai)
            .count()
            == 2
    })
    .await;

    for i in 0..3 {
        let answer = asker
            .state
            .ask_mesh(&format!("question {i}"), None, None, ANSWER_DEADLINE)
            .await
            .expect("answered by the free node");
        assert!(answer.starts_with("ECHO"), "{answer}");
    }
    assert!(
        seen_busy.lock().unwrap().is_empty(),
        "the busy node never ran inference"
    );
    assert_eq!(seen_free.lock().unwrap().len(), 3);
}

/// A Top-Tier node serves a session no more times than it paid for, and
/// never serves a channel query that carries no session at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_serves_no_more_answers_than_it_paid_for() {
    use core_crypto::extrinsic::PALLET_AUTHORITY;
    use core_crypto::pallets::authority::AuthorityCall;
    use core_crypto::pallets::nai::NaiSession;

    test_home();

    let user = spawn_node(47368).await;
    let top = spawn_node(47369).await;
    let user_pid = user.state.signing_key.verifying_key().to_bytes();
    let top_pid = top.state.signing_key.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(user_pid);
    let add = core_crypto::Extrinsic::sign(
        &user.state.signing_key,
        PALLET_AUTHORITY,
        &AuthorityCall::Add {
            pubkey: top_pid,
            channels: vec!["frontier".into()],
            attestation_hash: [0u8; 32],
        },
        user_pid,
        1,
    );
    let slot = app_kernel::state::now_ms() / core_crypto::block::SLOT_DURATION_MS - 10;
    let first = seal_block(&chain, &user.state.signing_key, slot, vec![add]);
    chain.apply_block(first).unwrap();
    let token = [0x5au8; 32];
    chain.nai.sessions.insert(
        token,
        NaiSession {
            owner: user_pid,
            expires_at_ms: app_kernel::state::now_ms() + 60 * 60 * 1_000,
            inferences_remaining: 2,
            channel: Some("frontier".into()),
        },
    );
    for node in [&user, &top] {
        *node.state.chain.write().await = chain.clone();
    }
    let seen_top = give_engine_named(&top, "echo-70b").await;
    user.state.dial(top.addr, None).await.expect("user->top");
    wait_until("the user to see the top-tier node", || async {
        user.state
            .peer_capabilities
            .read()
            .await
            .values()
            .any(|c| c.has_ai)
    })
    .await;

    let ask = |token| {
        let user = user.state.clone();
        async move {
            user.ask_mesh(
                "paid question",
                Some("frontier".into()),
                token,
                Duration::from_secs(10),
            )
            .await
        }
    };
    assert!(ask(None).await.is_err(), "no session, no answer");
    assert!(seen_top.lock().unwrap().is_empty());
    assert!(ask(Some(token)).await.is_ok());
    assert!(ask(Some(token)).await.is_ok());
    assert!(ask(Some(token)).await.is_err(), "the session is spent");
    assert_eq!(seen_top.lock().unwrap().len(), 2);
}

/// A channel nobody serves is refused straight away rather than after the
/// whole deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_nobody_serves_is_refused_at_once() {
    test_home();

    let user = spawn_node(47370).await;
    let started = std::time::Instant::now();
    let result = user
        .state
        .ask_mesh(
            "anyone?",
            Some("frontier".into()),
            Some([1u8; 32]),
            Duration::from_secs(30),
        )
        .await;
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused at once, not after the deadline: {:?}",
        started.elapsed()
    );
}

/// A board sale across the mesh: the seller's credits go into escrow, the
/// buyer claims its payment, the sale oracle confirms it, and every node
/// ends up agreeing the buyer holds the credits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_settles_across_the_mesh() {
    use core_crypto::extrinsic::PALLET_MARKET;
    use core_crypto::pallets::market::MarketCall;

    test_home();

    let seller = spawn_node(47371).await;
    let buyer = spawn_node(47372).await;
    let oracle = spawn_node(47373).await;
    let seller_pid = seller.state.signing_key.verifying_key().to_bytes();
    let buyer_pid = buyer.state.signing_key.verifying_key().to_bytes();
    let oracle_pid = oracle.state.signing_key.verifying_key().to_bytes();

    let mut chain = core_crypto::ChainState::with_genesis_authority(seller_pid);
    grant_standing(&mut chain, seller_pid);
    grant_standing(&mut chain, buyer_pid);
    chain.nai.sale_oracle = Some(oracle_pid);
    for node in [&seller, &buyer, &oracle] {
        *node.state.chain.write().await = chain.clone();
    }
    tokio::spawn(seller.state.clone().run_block_producer());
    buyer
        .state
        .dial(seller.addr, None)
        .await
        .expect("buyer->seller");
    oracle
        .state
        .dial(seller.addr, None)
        .await
        .expect("oracle->seller");
    wait_until("everyone to be connected", || async {
        seller.state.live_connections().await.len() >= 2
    })
    .await;

    let before = chain.balance_of(&seller_pid);
    seller
        .state
        .sign_and_submit(
            PALLET_MARKET,
            &MarketCall::PostAsk {
                credits: 400,
                ask_sats: 8_000_000,
                ltc_address: "ltc1qseller".into(),
            },
        )
        .await;
    wait_until("the listing to reach the buyer, escrowed", || async {
        let chain = buyer.state.chain.read().await;
        chain.market.asks.contains_key(&0) && chain.market.escrowed == 400
    })
    .await;
    assert_eq!(
        seller.state.chain.read().await.balance_of(&seller_pid),
        before - 400
    );

    buyer
        .state
        .sign_and_submit(
            PALLET_MARKET,
            &MarketCall::ClaimFill {
                ask_id: 0,
                buyer: buyer_pid,
                payment_ref: "ltc-board-payment".into(),
            },
        )
        .await;
    wait_until("the oracle to see the claim", || async {
        oracle
            .state
            .chain
            .read()
            .await
            .market
            .claims
            .contains_key("ltc-board-payment")
    })
    .await;

    oracle
        .state
        .sign_and_submit(
            PALLET_MARKET,
            &MarketCall::Settle {
                id: 0,
                buyer: buyer_pid,
                payment_ref: "ltc-board-payment".into(),
            },
        )
        .await;
    wait_until(
        "every node to agree the buyer holds the credits",
        || async {
            let mut all = true;
            for node in [&seller, &buyer, &oracle] {
                let chain = node.state.chain.read().await;
                all &= chain.balance_of(&buyer_pid) == 400
                    && chain.market.escrowed == 0
                    && chain.market.fills.contains_key("ltc-board-payment")
                    && chain.market.claims.is_empty();
            }
            all
        },
    )
    .await;
}

/// The circuit behind "I rated it and nothing changed": an answer in a room
/// is rated by the member who asked, and the like reaches the responder's
/// standing on chain once the receipt it rates has landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_like_on_a_room_answer_reaches_the_responders_quality() {
    use core_crypto::pallets::reputation::Verdict;

    test_home();

    let asker = spawn_node(47374).await;
    let responder = spawn_node(47375).await;
    let asker_pid = asker.state.signing_key.verifying_key().to_bytes();
    let responder_pid = responder.state.signing_key.verifying_key().to_bytes();

    let chain = core_crypto::ChainState::with_genesis_authority(asker_pid);
    for node in [&asker, &responder] {
        *node.state.chain.write().await = chain.clone();
    }
    tokio::spawn(asker.state.clone().run_block_producer());
    let _seen = give_engine(&responder).await;
    asker
        .state
        .dial(responder.addr, None)
        .await
        .expect("asker->responder");
    wait_until("the asker to see the responder", || async {
        asker
            .state
            .peer_capabilities
            .read()
            .await
            .values()
            .any(|c| c.has_ai)
    })
    .await;

    let code = mesh_network::rooms::RoomCode::generate();
    asker.state.join_room(code.clone()).await.unwrap();
    asker
        .state
        .send_room_message(&code, "@agente resume esto")
        .await
        .unwrap();
    let rateable = |rooms: &std::collections::HashMap<String, app_kernel::rooms::RoomState>| {
        rooms
            .get(code.as_str())
            .and_then(|r| r.chat.iter().find(|e| e.is_ai && e.rating.is_some()))
            .map(|e| e.id)
    };
    wait_until("a rateable answer in the room", || async {
        rateable(&*asker.state.rooms.read().await).is_some()
    })
    .await;
    let answer_id = rateable(&*asker.state.rooms.read().await).unwrap();
    wait_until("the receipt to land", || async {
        asker.state.chain.read().await.wins_of(&responder_pid) == 1
    })
    .await;

    asker
        .state
        .rate_room_reply(code.as_str(), answer_id, Verdict::Like)
        .await
        .expect("the asker may rate");
    assert!(
        asker
            .state
            .rate_room_reply(code.as_str(), answer_id, Verdict::Like)
            .await
            .is_err(),
        "once only"
    );
    wait_until("the like to count for the responder", || async {
        let chain = asker.state.chain.read().await;
        chain
            .reputation
            .quality(&responder_pid, chain.head_height())
            .likes
            >= 1
    })
    .await;
}

/// The node runs with a key of its own; the wallet opens with its password.
/// Unlocking links the two on chain, what the node held and what it earns
/// land in the wallet, and once locked again nothing the wallet signs goes
/// out - while the node keeps redeeming on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_earns_into_the_wallet_it_was_unlocked_with() {
    use app_kernel::wallet::{WalletKey, LOCKED};
    test_home();

    let producer = spawn_node(47381).await;
    let worker = spawn_node(47382).await;
    let producer_pid = producer.state.signing_key.verifying_key().to_bytes();
    let node_pid = worker.state.signing_key.verifying_key().to_bytes();
    let phrase = core_crypto::NodeMnemonic::generate();
    let wallet_pid = WalletKey::address_of(&phrase);

    let mut chain = core_crypto::ChainState::with_genesis_authority(producer_pid);
    chain.economy.current_shares_per_credit = 100;
    chain.balances.mint(node_pid, 25);
    chain.contributions.record_lottery_win(node_pid, 500);
    for node in [&producer, &worker] {
        *node.state.chain.write().await = chain.clone();
    }
    tokio::spawn(producer.state.clone().run_block_producer());
    worker
        .state
        .dial(producer.addr, None)
        .await
        .expect("worker->producer");
    wait_until("the two nodes to see each other", || async {
        !worker.state.live_connections().await.is_empty()
            && !producer.state.live_connections().await.is_empty()
    })
    .await;

    // Locked: the wallet signs nothing.
    assert_eq!(
        worker.state.transfer_credits(producer_pid, 1).await,
        Err(LOCKED.to_string())
    );

    worker
        .state
        .unlock_wallet(WalletKey::from_mnemonic(&phrase), None)
        .await;
    wait_until("the link to land", || async {
        worker
            .state
            .chain
            .read()
            .await
            .identity
            .wallet_of(&node_pid)
            == Some(wallet_pid)
    })
    .await;
    {
        let chain = worker.state.chain.read().await;
        assert_eq!(chain.balance_of(&node_pid), 0, "the node keeps nothing");
        assert_eq!(chain.balance_of(&wallet_pid), 25, "what it held moved over");
    }

    // The node redeems with its own key; the wallet is paid.
    worker.state.prune_inflight_txs().await;
    assert!(worker.state.redeem_if_due().await, "500 at 100 is due");
    wait_until("the earnings to reach the wallet", || async {
        worker.state.chain.read().await.balance_of(&wallet_pid) == 30
    })
    .await;

    // Unlocked, the wallet spends; locked again, it does not.
    worker
        .state
        .transfer_credits(producer_pid, 2)
        .await
        .expect("an open wallet spends");
    wait_until("the transfer to land", || async {
        worker.state.chain.read().await.balance_of(&wallet_pid) == 28
    })
    .await;
    worker.state.lock_wallet().await;
    assert_eq!(
        worker.state.transfer_credits(producer_pid, 1).await,
        Err(LOCKED.to_string())
    );
    assert_eq!(worker.state.chain.read().await.balance_of(&node_pid), 0);
}

/// Two home nodes behind NAT never connect to each other, only to a public
/// node. The asker still reaches the one with a model through the relay, and
/// custody reaches every node it knows of - including one with no model at
/// all, and none of it travels in the clear through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nodes_behind_nat_hold_custody_and_answer_through_a_relay() {
    test_home();

    let relay = spawn_node(47401).await;
    let asker = spawn_node(47402).await;
    let answerer = spawn_node(47403).await;
    let keeper = spawn_node(47404).await;
    let seen = give_engine(&answerer).await;

    for n in [&asker, &answerer, &keeper] {
        n.state
            .dial(relay.addr, None)
            .await
            .expect("dial the public node");
    }
    wait_until("the public node to hold all three", || async {
        relay.state.live_connections().await.len() == 3
    })
    .await;

    let answerer_pid = *answerer.state.my_peer_id.read().await;
    let keeper_pid = *keeper.state.my_peer_id.read().await;
    wait_until(
        "the asker to learn of both home nodes through presence",
        || async {
            relay.state.announce_capabilities().await;
            let known = asker.state.known_peer_caps().await;
            known.get(&answerer_pid).is_some_and(|c| c.has_ai) && known.contains_key(&keeper_pid)
        },
    )
    .await;
    assert!(
        asker
            .state
            .connection_for_peer(&answerer_pid)
            .await
            .is_none(),
        "no direct connection: everything below goes through the relay"
    );

    let answer = asker
        .state
        .ask_mesh("what is 2+2?", None, None, ANSWER_DEADLINE)
        .await
        .expect("answered through the relay");
    assert!(answer.starts_with("ECHO"), "{answer}");
    assert_eq!(seen.lock().unwrap().len(), 1);

    let asker_pid = *asker.state.my_peer_id.read().await;
    // The answer names the custodians that released, and the asker submits
    // them with its receipt: the keeper, with no model, gets credited.
    wait_until("the asker's receipt to carry the custody list", || async {
        asker.state.my_inflight_txs.lock().await.iter().any(|(_, ext)| {
            matches!(
                core_crypto::DecodedCall::decode(ext.pallet_idx, &ext.args),
                Ok(core_crypto::DecodedCall::Contributions(
                    core_crypto::pallets::contributions::ContributionsCall::SubmitReceiptWithCustody { custody, .. }
                )) if custody.custodians.contains(&keeper_pid) && !custody.custodians.contains(&answerer_pid)
            )
        })
    })
    .await;
    for (name, node) in [
        ("keeper", &keeper),
        ("relay", &relay),
        ("answerer", &answerer),
    ] {
        let holds = node
            .state
            .custody
            .read()
            .await
            .keys()
            .any(|(sender, _)| *sender == asker_pid);
        assert!(holds, "the {name} holds a share of the question");
    }
}

/// Two nodes that know each other only through a public node ask it for an
/// introduction and end up with exactly one direct connection, both ends
/// keeping the same one. Neither starts offering itself as a relay because
/// of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hole_punch_connects_two_nodes_known_only_through_a_relay() {
    test_home();

    let relay = spawn_node(47411).await;
    let a = spawn_node(47412).await;
    let b = spawn_node(47413).await;
    a.state.dial(relay.addr, None).await.expect("a->relay");
    b.state.dial(relay.addr, None).await.expect("b->relay");

    let a_pid = *a.state.my_peer_id.read().await;
    let b_pid = *b.state.my_peer_id.read().await;
    wait_until("both to know of each other", || async {
        relay.state.announce_capabilities().await;
        a.state.known_peer_caps().await.contains_key(&b_pid)
            && b.state.known_peer_caps().await.contains_key(&a_pid)
    })
    .await;

    a.state.punch_round().await;
    wait_until("a direct connection between the two", || async {
        a.state.connection_for_peer(&b_pid).await.is_some()
            && b.state.connection_for_peer(&a_pid).await.is_some()
    })
    .await;
    // Both dialed. Had each end kept a different connection, each would
    // close the one the other kept and the pair would end with none; one
    // each, still there after the losers closed, means they agreed.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(a.state.connections_of(&b_pid).await.len(), 1, "A keeps one");
    assert_eq!(b.state.connections_of(&a_pid).await.len(), 1, "B keeps one");
    for (name, n) in [("A", &a), ("B", &b)] {
        assert!(
            !n.state.can_relay.load(std::sync::atomic::Ordering::Relaxed),
            "{name} took a punched connection for public reachability"
        );
    }
}

/// A chain of 20 blocks sealed by `founder`, its only validator, with 16
/// finalized.
fn finalized_chain(founder: &ed25519_dalek::SigningKey) -> core_crypto::ChainState {
    use core_crypto::extrinsic::PALLET_FINALITY;
    use core_crypto::finality::{FinalityCall, FinalityVote, Justification};

    let founder_pid = founder.verifying_key().to_bytes();
    let mut chain = core_crypto::ChainState::with_genesis_authority(founder_pid);
    let now_slot = app_kernel::state::now_ms() / core_crypto::block::SLOT_DURATION_MS;
    let mut slot = now_slot - 400;
    while chain.head_height() < 17 {
        slot += 1;
        let block = seal_block(&chain, founder, slot, Vec::new());
        chain.apply_block(block).unwrap();
    }
    let hash = chain.hash_at(16).unwrap();
    let j = Justification::from_votes(16, hash, &[FinalityVote::sign(founder, 16, hash)]);
    let finalize = core_crypto::Extrinsic::sign(
        founder,
        PALLET_FINALITY,
        &FinalityCall::Finalize { justification: j },
        founder_pid,
        1,
    );
    slot += 1;
    let block = seal_block(&chain, founder, slot, vec![finalize]);
    chain.apply_block(block).unwrap();
    while chain.head_height() < 20 {
        slot += 1;
        let block = seal_block(&chain, founder, slot, Vec::new());
        chain.apply_block(block).unwrap();
    }
    assert_eq!(chain.finalized().0, 16);
    chain
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_node_starts_from_a_light_proof_and_catches_up() {
    test_home();
    let a = spawn_node(47361).await;
    let b = spawn_node(47362).await;
    let founder = a.state.signing_key.clone();
    let founder_pid = founder.verifying_key().to_bytes();
    for node in [&a, &b] {
        *node.state.genesis_authority.write().unwrap() = founder_pid;
    }
    let chain = finalized_chain(&founder);
    *a.state.chain.write().await = chain.clone();
    *b.state.chain.write().await = core_crypto::ChainState::with_genesis_authority(founder_pid);
    b.state.load_chain_from_disk().await;

    // A connection B does not register, so the only sync is the light one.
    let client = b.state.mesh_client_for(a.addr).await.expect("client");
    let (conn, _) = client.connect_to_peer(a.addr, None).await.expect("connect");
    let height = b.state.light_sync_from(&conn).await.expect("light sync");
    assert_eq!(height, 16, "starts at the finalized tip");
    assert!(
        b.state.chain.read().await.base.is_some(),
        "with no history before it"
    );

    wait_until("B to catch up with A's head through block sync", || async {
        b.state.chain.read().await.head_height() == 20
    })
    .await;
    assert_eq!(
        b.state.chain.read().await.state_root(),
        chain.state_root(),
        "the same state as a full replay"
    );

    // Restarting, B rebuilds the same chain from its base and chain file.
    b.state.persist_chain_now().await.expect("persist");
    let path = b.state.chain_file_path();
    assert!(app_kernel::light_sync::base_path(&path).exists());
    *b.state.chain.write().await = core_crypto::ChainState::with_genesis_authority(founder_pid);
    b.state.load_chain_from_disk().await;
    let reloaded = b.state.chain.read().await;
    assert_eq!(reloaded.head_height(), 20);
    assert_eq!(reloaded.state_root(), chain.state_root());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_light_node_syncs_from_a_proof_and_never_from_genesis() {
    test_home();
    let a = spawn_node(47363).await;
    let phone = spawn_node(47364).await;
    let founder = a.state.signing_key.clone();
    let founder_pid = founder.verifying_key().to_bytes();
    for node in [&a, &phone] {
        *node.state.genesis_authority.write().unwrap() = founder_pid;
    }
    let chain = finalized_chain(&founder);
    *a.state.chain.write().await = chain.clone();
    phone
        .state
        .light
        .store(true, std::sync::atomic::Ordering::Relaxed);
    *phone.state.chain.write().await = core_crypto::ChainState::with_genesis_authority(founder_pid);
    phone.state.load_chain_from_disk().await;

    phone.state.dial(a.addr, None).await.expect("phone->a");
    // An ordinary block sync from genesis is refused while it waits.
    let conn = phone
        .state
        .connection_for_peer(&founder_pid)
        .await
        .map(|(_, c)| c)
        .expect("connected");
    phone.state.request_block_sync(conn).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(phone.state.chain.read().await.head_height(), 0);

    tokio::spawn(phone.state.clone().run_light_bootstrap());
    wait_until("the phone to start from the proof and catch up", || async {
        phone.state.chain.read().await.head_height() == 20
    })
    .await;
    let synced = phone.state.chain.read().await;
    assert_eq!(synced.base.as_ref().map(|h| h.height), Some(16));
    assert_eq!(synced.state_root(), chain.state_root());
    let caps = phone.state.caps_byte().await;
    assert_ne!(caps & mesh_network::protocol::caps_bits::LIGHT, 0);
    assert_eq!(caps & mesh_network::protocol::caps_bits::SERVES_LIGHT, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decline_reaches_a_ranked_node_sealed_through_the_relays() {
    test_home();
    let ranked = spawn_node(47371).await;
    let decliner = ed25519_dalek::SigningKey::from_bytes(&[0x5au8; 32]);
    let decliner_pid = decliner.verifying_key().to_bytes();
    let ranked_pid = ranked.state.signing_key.verifying_key().to_bytes();
    let key = ([0x11u8; 32], 99u64);
    let seal = |d: &mesh_network::protocol::SignedDecline| {
        let sealed = core_crypto::agent_crypto::seal_to_peer(
            &ranked_pid,
            &mesh_network::protocol::frame(
                mesh_network::protocol::message_types::SIGNED_DECLINE,
                &d.to_bytes(),
            ),
        )
        .unwrap();
        core_crypto::wire::encode(&sealed)
    };
    let declined = |state: AppState| async move {
        state
            .observed_declines
            .read()
            .await
            .get(&key)
            .is_some_and(|set| set.contains(&decliner_pid))
    };

    // Tampered: not counted.
    let mut forged = mesh_network::protocol::SignedDecline::sign(&decliner, key.0, key.1, 0);
    forged.rank = 2;
    ranked.state.handle_sealed(&seal(&forged)).await.unwrap();
    assert!(!declined(ranked.state.clone()).await);

    // Signed: counted for the node that signed it, whoever carried it.
    let real = mesh_network::protocol::SignedDecline::sign(&decliner, key.0, key.1, 0);
    ranked.state.handle_sealed(&seal(&real)).await.unwrap();
    assert!(declined(ranked.state.clone()).await);
}
