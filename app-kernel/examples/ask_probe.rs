// SPDX-License-Identifier: BUSL-1.1
//! Asks the live network a few questions the way the phone app does and
//! shows, for each, who it was dispatched to and who answered.
//!
//! cargo run --example ask_probe -p app-kernel --features node_only -- [count]

use std::time::Duration;

use app_kernel::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn,app_kernel::agent=info")
        .without_time()
        .with_target(false)
        .init();
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(8);
    let network = core_crypto::network::active();
    let home = std::env::temp_dir().join(format!("nepheral-ask-probe-{}", std::process::id()));
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
        if let Ok(pid) = state.dial(addr, None).await {
            if !synced {
                if let Some((_, conn)) = state.connection_for_peer(&pid).await {
                    synced = state.light_sync_from(&conn).await.is_ok();
                }
            }
        }
    }
    println!("synced: {synced}; waiting 40 s for presence...");
    tokio::time::sleep(Duration::from_secs(40)).await;
    let candidates = state.eligible_candidates(None).await;
    println!(
        "in the draw: {}",
        candidates
            .iter()
            .map(|(p, w)| format!("{} (w{w})", hex::encode(&p[..4])))
            .collect::<Vec<_>>()
            .join(", ")
    );

    for i in 1..=count {
        let started = std::time::Instant::now();
        let prompt = format!("Reply with just the number {i}.");
        match state
            .ask_mesh(&prompt, None, None, Duration::from_secs(120))
            .await
        {
            Ok(answer) => println!(
                "q{i}: {:?} in {:.1}s",
                answer.trim().chars().take(40).collect::<String>(),
                started.elapsed().as_secs_f32()
            ),
            Err(e) => println!(
                "q{i}: FAILED after {:.1}s: {e}",
                started.elapsed().as_secs_f32()
            ),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}
