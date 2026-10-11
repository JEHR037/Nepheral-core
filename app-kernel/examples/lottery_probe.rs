// SPDX-License-Identifier: BUSL-1.1
//! Joins the live network the way the phone app does and reports, for every
//! AI-capable node it hears of, whether an asker would put it in the draw,
//! and if not, which rule keeps it out.
//!
//! cargo run --example lottery_probe -p app-kernel --features node_only

use std::collections::HashSet;
use std::time::Duration;

use app_kernel::state::AppState;

#[tokio::main]
async fn main() {
    let network = core_crypto::network::active();
    let home = std::env::temp_dir().join(format!("nepheral-lottery-probe-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    let mnemonic = core_crypto::NodeMnemonic::generate();
    let identity = core_crypto::NodeIdentity::from_mnemonic(&mnemonic).unwrap();
    let tls = app_kernel::boot::tls_identity(&identity);
    let bound = app_kernel::boot::bind_mesh(0, &tls).await.expect("bind");
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&mnemonic.to_seed());
    let mut state = AppState::new(bound.listen_addr, Vec::new(), signing_key, tls);
    state.data_dir = home.clone();
    app_kernel::node_runtime::install_stream_handler(&state);
    state
        .install_mesh_endpoints(bound.endpoint.endpoint_v4(), bound.endpoint.endpoint_v6())
        .await;
    state
        .light
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let endpoint = bound.endpoint;
    let s = state.clone();
    tokio::spawn(async move {
        let _ = app_kernel::node_runtime::run_node_with_state(endpoint, s).await;
    });

    let mut synced = false;
    for addr in network.default_bootstraps() {
        let addr: std::net::SocketAddr = addr.to_string().parse().unwrap();
        match state.dial(addr, None).await {
            Ok(pid) => {
                println!("connected to {addr} ({})", hex::encode(&pid[..4]));
                if !synced {
                    if let Some((_, conn)) = state.connection_for_peer(&pid).await {
                        synced = state.light_sync_from(&conn).await.is_ok();
                    }
                }
            }
            Err(e) => println!("could not reach {addr}: {e}"),
        }
    }
    println!("listening for presence for 60 s...");
    tokio::time::sleep(Duration::from_secs(60)).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let direct: HashSet<[u8; 32]> = state
        .live_connections()
        .await
        .into_iter()
        .map(|(_, p, _)| p)
        .collect();
    let caps = state.known_peer_caps().await;
    let chain = state.chain.read().await;
    let height = chain.head_height();
    println!(
        "\nchain height {height}; nodes heard of: {} ({} direct)",
        caps.len(),
        direct.len()
    );
    println!(
        "\n{:<10} {:<6} {:<4} {:<34} {:>10} {:<7} {:<8} {:>5}",
        "node", "how", "ai", "model", "1st work", "tenure", "eligible", "qual"
    );
    let mut rows: Vec<_> = caps.iter().collect();
    rows.sort_by_key(|(_, c)| (!c.has_ai, c.light));
    for (pid, c) in rows {
        let first = chain.contributions.first_seen_of(pid);
        let age = first
            .map(|f| format!("{}m ago", now.saturating_sub(f) / 60_000))
            .unwrap_or_else(|| "never".into());
        let q = chain.reputation.quality(pid, height);
        println!(
            "{:<10} {:<6} {:<4} {:<34} {:>10} {:<7} {:<8} {:>4}%{}",
            hex::encode(&pid[..4]),
            if direct.contains(pid) {
                "direct"
            } else {
                "relay"
            },
            if c.has_ai { "yes" } else { "-" },
            c.model
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or("-")
                .chars()
                .take(33)
                .collect::<String>(),
            age,
            app_kernel::sybil::has_tenure(&chain, pid, now),
            chain.contributions.is_lottery_eligible_at(pid, Some(now)),
            q.percent,
            if c.light { "  (phone)" } else { "" },
        );
    }
    // Answers credited on chain in the hours the ledger still keeps per pair:
    // who asked, who answered.
    let mut pairs: Vec<_> = chain.contributions.receipts_by_pair.iter().collect();
    pairs.sort_by_key(|((h, s, r), _)| (*h, *s, *r));
    println!("\nanswers credited, by hour (UTC) / asker -> responder:");
    for ((hour, sender, responder), n) in pairs {
        println!(
            "  {:02}:00  {} -> {}  x{n}",
            hour % 24,
            hex::encode(&sender[..4]),
            hex::encode(&responder[..4])
        );
    }
    println!("total wins on record:");
    for pid in caps.keys() {
        let w = chain.contributions.wins_of(pid);
        if w > 0 {
            println!("  {}  {w}", hex::encode(&pid[..4]));
        }
    }
    drop(chain);

    let picked = state.eligible_candidates(None).await;
    println!("\nwhat this build would put in the draw (with the tenure rule):");
    for (pid, w) in &picked {
        println!("  {}  weight {w}", hex::encode(&pid[..4]));
    }
    let _ = std::fs::remove_dir_all(&home);
}
