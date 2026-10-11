// SPDX-License-Identifier: BUSL-1.1

#[cfg(feature = "local-api-server")]
use crate::api_backend;
use crate::nepheral_config;
use crate::node_runtime;
use crate::state::AppState;
use tracing::{error, info};

pub struct ClientNode {
    pub rt: tokio::runtime::Runtime,
    pub state: AppState,
    pub first_run: bool,
    handles: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl ClientNode {
    pub fn shutdown(&self) {
        let state = self.state.clone();
        self.rt
            .block_on(async move { state.begin_shutdown().await });
        let handles = std::mem::take(&mut *self.handles.lock().expect("handles lock"));
        self.rt.block_on(async {
            for h in handles {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
            }
        });
    }
}

/// What the node needs to start. The node's own key opens without a
/// password, so the node serves the network from launch; the wallet stays
/// locked until its owner opens it (`AppState::unlock_wallet`).
pub struct NodeStart {
    pub node_key: ed25519_dalek::SigningKey,
    /// The wallet this node works for, if one exists yet. Public: shown and
    /// used for balances while the wallet is locked.
    pub wallet_address: Option<core_crypto::PeerId>,
    /// Idle minutes before the wallet locks itself; 0 is never.
    pub auto_lock_minutes: u64,
    /// The local model's system prompt, from the node's own store.
    pub system_prompt: Option<String>,
    /// Run as a light node: the phone app. See `AppState::light`.
    pub light: bool,
}

pub fn start(start: NodeStart) -> Result<ClientNode, Box<dyn std::error::Error>> {
    let network = crate::boot::select_network();
    let cfg = nepheral_config::load_config().unwrap_or_default();
    let first_run = !cfg.desktop_setup_done;

    let NodeStart {
        node_key,
        wallet_address,
        auto_lock_minutes,
        system_prompt,
        light,
    } = start;
    let identity = core_crypto::NodeIdentity::from_signing_key(&node_key)
        .map_err(|e| format!("Failed to create identity: {e}"))?;
    let tls = crate::boot::tls_identity(&identity);
    info!(peer_id = %identity.fingerprint_hex(), network = network.name(), "Node identity ready");

    let bootstrap_peers: Vec<std::net::SocketAddr> = std::env::var("NEPHERAL_BOOTSTRAP_PEER")
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_else(|| {
            network
                .default_bootstraps()
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect()
        });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let port = std::env::var("NEPHERAL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(cfg.mesh_port);
    let bound = rt.block_on(crate::boot::bind_mesh(port, &tls))?;
    let local_ip =
        local_ip_address::local_ip().unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let display_addr = std::net::SocketAddr::new(local_ip, bound.listen_addr.port());

    let app_state = AppState::new(display_addr, bootstrap_peers, node_key, tls);
    app_state
        .light
        .store(light, std::sync::atomic::Ordering::Relaxed);
    node_runtime::install_stream_handler(&app_state);
    app_state.set_auto_lock_minutes(auto_lock_minutes);
    *app_state.api_token_hash.write().expect("api hash lock") =
        crate::wallet::load_api_token_hash();
    {
        let st = app_state.clone();
        rt.block_on(async move {
            *st.wallet_address.write().await = wallet_address;
            *st.local_system_prompt.write().await = system_prompt;
        });
    }

    {
        let st = app_state.clone();
        let (v4, v6) = (bound.endpoint.endpoint_v4(), bound.endpoint.endpoint_v6());
        rt.block_on(async move {
            st.install_mesh_endpoints(v4, v6).await;
            st.load_chain_from_disk().await;
            st.bootstrap_direct_sale_wallet().await;
            st.bootstrap_sale_oracle().await;
            st.bootstrap_top_tier_nodes().await;
            st.bootstrap_founder_keep().await;
        });
    }
    info!(listen = %bound.listen_addr, "Mesh listener bound");

    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let spawn =
        |handles: &mut Vec<tokio::task::JoinHandle<()>>,
         fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>| {
            handles.push(rt.spawn(fut));
        };

    let s = app_state.clone();
    spawn(&mut handles, Box::pin(node_runtime::run_peer_discovery(s)));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(node_runtime::run_gossip_flood(s)));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_hole_punch()));
    let s = app_state.clone();
    spawn(
        &mut handles,
        Box::pin(node_runtime::run_capability_heartbeat(s)),
    );
    // A phone finds the network through the bootstrap nodes; announcing
    // itself on every Wi-Fi it joins would only cost battery.
    if !light {
        let s = app_state.clone();
        let mdns_port = bound.listen_addr.port();
        spawn(
            &mut handles,
            Box::pin(node_runtime::run_mdns_announce_and_discover(s, mdns_port)),
        );
    }
    let s = app_state.clone();
    spawn(
        &mut handles,
        Box::pin(node_runtime::run_ai_session_decay(s)),
    );
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(node_runtime::run_stats_broadcast(s)));
    let s = app_state.clone();
    spawn(
        &mut handles,
        Box::pin(node_runtime::run_stale_peer_cleanup(s)),
    );
    // On a first run the wizard asks which model to install; fetching the
    // recommended one meanwhile would hold the only download slot and keep
    // the user from picking another. The wizard starts this itself. With an
    // external backend there is no local model to fetch or load.
    if !first_run && !light && !cfg.uses_external_backend() {
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_model_sync()));
    }
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_chain_persist_task()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_chain_status_log()));
    if light {
        // No model to load, no blocks to seal or votes to cast: a phone
        // holds fragments, asks, and follows the chain from a light proof.
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_light_bootstrap()));
    } else {
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_ai_startup()));
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_block_producer()));
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_finality_voter()));
    }
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_inflight_resend()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_agent_sweeper()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_room_sweeper()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_auto_lock()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_conversation_saver()));
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_port_mapping()));
    // The memory model: the desktop fetches it on its own; a phone asks
    // first, since it may be on mobile data.
    let s = app_state.clone();
    spawn(&mut handles, Box::pin(s.run_recall_setup(!light)));
    // The desktop serves the mesh too, and used to keep its earnings until
    // someone pressed Redeem.
    if crate::chain_node::auto_redeem_enabled() {
        let s = app_state.clone();
        spawn(&mut handles, Box::pin(s.run_auto_redeem()));
    }
    let s = app_state.clone();
    let endpoint = bound.endpoint;
    spawn(
        &mut handles,
        Box::pin(async move {
            if let Err(e) = node_runtime::run_node_with_state(endpoint, s).await {
                error!(error = %e, "QUIC endpoint failed");
            }
        }),
    );
    let s = app_state.clone();
    spawn(
        &mut handles,
        Box::pin(async move {
            if let Err(e) = node_runtime::run_peer_connection_manager(s).await {
                error!(error = %e, "Peer connection manager failed");
            }
        }),
    );

    #[cfg(feature = "local-api-server")]
    if cfg.local_api_server_enabled && !light {
        let state_clone = app_state.clone();
        // Checks the token against its hash, so it answers while locked too.
        let security = nepheral_api::server::ApiSecurity {
            auth_token: None,
            allowed_origins: cfg.local_api_server_allowed_origins.clone(),
            allow_unauthenticated: false,
            gate: Some(std::sync::Arc::new(api_backend::TokenGate(
                app_state.clone(),
            ))),
        };
        let addr_str = format!(
            "{}:{}",
            cfg.local_api_server_host, cfg.local_api_server_port
        );
        rt.spawn(async move {
            let backend = std::sync::Arc::new(api_backend::AppStateBackend::new(state_clone));
            let router = nepheral_api::server::router(backend, security);
            match tokio::net::TcpListener::bind(&addr_str).await {
                Ok(listener) => {
                    info!(addr = %addr_str, "Local API server listening");
                    if let Err(e) = axum::serve(listener, router).await {
                        tracing::warn!(error = %e, "Local API server error");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, addr = %addr_str, "Failed to bind local API server")
                }
            }
        });
    }

    Ok(ClientNode {
        rt,
        state: app_state,
        first_run,
        handles: std::sync::Mutex::new(handles),
    })
}
