// SPDX-License-Identifier: BUSL-1.1
//! Syncs from a live node the way the phone app does, with a throwaway
//! identity and a temporary data directory: fetches its light proof,
//! verifies it from this build's genesis, then catches up block by block.
//!
//! cargo run --example light_probe -p app-kernel --features node_only -- [host:port]
//! (default: the first testnet bootstrap node)

use std::time::{Duration, Instant};

use app_kernel::state::AppState;

#[tokio::main]
async fn main() {
    let network = core_crypto::network::active();
    let target: std::net::SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| network.default_bootstraps()[0].to_string())
        .parse()
        .expect("host:port");
    let home = std::env::temp_dir().join(format!("nepheral-light-probe-{}", std::process::id()));
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

    let pid = state.dial(target, None).await.expect("dial");
    let (_, conn) = state.connection_for_peer(&pid).await.expect("connected");
    println!("connected to {target} ({})", hex::encode(&pid[..8]));

    let t = Instant::now();
    match state.light_sync_from(&conn).await {
        Ok(height) => println!(
            "verified a light proof and started at {height} in {:?}",
            t.elapsed()
        ),
        Err(e) => {
            println!("light sync failed: {e}");
            std::process::exit(1);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = 0;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let head = state.chain.read().await.head_height();
        if head == last && head > 0 {
            break;
        }
        last = head;
    }
    let chain = state.chain.read().await;
    println!(
        "caught up to {} (base {:?}, validators {}, state root {})",
        chain.head_height(),
        chain.base.as_ref().map(|h| h.height),
        chain.authority.authorities.len(),
        hex::encode(&chain.state_root()[..8]),
    );
    let _ = std::fs::remove_dir_all(&home);
}
