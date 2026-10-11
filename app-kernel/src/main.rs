// SPDX-License-Identifier: BUSL-1.1

mod cli_setup;
mod cli_wallet;
mod control;

use app_kernel::node_runtime;
use app_kernel::state::AppState;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `nepheral-node unlock`, `status`, `wallet ...`: answered and done.
    if let Some(code) = cli_wallet::dispatch() {
        std::process::exit(code);
    }
    utils::init_tracing();
    let network = app_kernel::boot::select_network();

    let resolved = cli_setup::resolve()?;
    let identity = core_crypto::NodeIdentity::from_mnemonic(&resolved.mnemonic)
        .map_err(|e| format!("Failed to create identity: {e}"))?;
    let tls = app_kernel::boot::tls_identity(&identity);
    info!(peer_id = %identity.fingerprint_hex(), network = network.name(), "Node identity ready");

    let bound = app_kernel::boot::bind_mesh(resolved.port, &tls).await?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&resolved.mnemonic.to_seed());
    let app_state = AppState::new(
        bound.listen_addr,
        resolved.bootstrap_peers,
        signing_key,
        tls,
    );
    node_runtime::install_stream_handler(&app_state);
    app_state
        .install_mesh_endpoints(bound.endpoint.endpoint_v4(), bound.endpoint.endpoint_v6())
        .await;
    // The node serves the network with its own key; the wallet it works for
    // stays locked until `nepheral-node unlock`.
    let wallet_address = app_kernel::nepheral_config::load_config()
        .and_then(|c| c.wallet_address)
        .and_then(|h| hex::decode(h.trim()).ok())
        .and_then(|b| <[u8; 32]>::try_from(b).ok());
    *app_state.wallet_address.write().await = wallet_address;
    *app_state.api_token_hash.write().expect("api hash lock") =
        app_kernel::wallet::load_api_token_hash();
    app_state.set_auto_lock_minutes(
        std::env::var("NEPHERAL_AUTO_LOCK_MINUTES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(app_kernel::wallet::DEFAULT_AUTO_LOCK_MINUTES),
    );
    info!(listen = %bound.listen_addr, "Mesh listener bound");

    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let endpoint = bound.endpoint;
    let s = app_state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = node_runtime::run_node_with_state(endpoint, s).await {
            error!(error = %e, "QUIC endpoint failed");
        }
    }));
    let s = app_state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = node_runtime::run_peer_connection_manager(s).await {
            error!(error = %e, "Peer connection manager failed");
        }
    }));
    handles.push(tokio::spawn(node_runtime::run_gossip_flood(
        app_state.clone(),
    )));
    handles.push(tokio::spawn(node_runtime::run_capability_heartbeat(
        app_state.clone(),
    )));
    handles.push(tokio::spawn(app_state.clone().run_hole_punch()));
    handles.push(tokio::spawn(app_state.clone().run_port_mapping()));
    handles.push(tokio::spawn(node_runtime::run_stale_peer_cleanup(
        app_state.clone(),
    )));
    handles.push(tokio::spawn(app_state.clone().run_auto_lock()));
    #[cfg(unix)]
    handles.push(tokio::spawn(control::serve(app_state.clone())));

    // Relay routes expire through the sweeper: a bootstrap relay needs it
    // as much as a node that answers.
    handles.push(tokio::spawn(app_state.clone().run_agent_sweeper()));
    if resolved.is_bootstrap {
        info!(
            "Bootstrap mode — relays peer lists, gossip and room envelopes; no chain or inference"
        );
    } else {
        let ai_config = if let Ok(model_path) = std::env::var("NEPHERAL_MODEL_PATH") {
            ai_engine::ModelConfig {
                model_name: app_kernel::models::model_name_for(
                    &model_path,
                    std::env::var("NEPHERAL_AI_MODEL").ok(),
                ),
                model_path: Some(model_path),
                ..Default::default()
            }
        } else if let Ok(url) =
            std::env::var("NEPHERAL_INFERENCE_URL").or_else(|_| std::env::var("NEPHERAL_AI_URL"))
        {
            ai_engine::ModelConfig {
                model_name: std::env::var("NEPHERAL_AI_MODEL")
                    .unwrap_or_else(|_| "qwen3:4b".into()),
                backend_url: url,
                ..Default::default()
            }
        } else {
            ai_engine::ModelConfig::default()
        };
        if let Err(e) = app_state.update_ai_config(ai_config).await {
            warn!(error = %e, "Failed to set AI config");
        }
        app_state.load_chain_from_disk().await;
        app_state.bootstrap_direct_sale_wallet().await;
        app_state.bootstrap_sale_oracle().await;
        app_state.bootstrap_top_tier_nodes().await;
        app_state.bootstrap_founder_keep().await;
        app_state.bootstrap_dissolution().await;

        handles.push(tokio::spawn(node_runtime::run_peer_discovery(
            app_state.clone(),
        )));
        handles.push(tokio::spawn(node_runtime::run_ai_session_decay(
            app_state.clone(),
        )));
        handles.push(tokio::spawn(node_runtime::run_stats_broadcast(
            app_state.clone(),
        )));
        handles.push(tokio::spawn(app_state.clone().run_ai_startup()));
        handles.push(tokio::spawn(app_state.clone().run_chain_status_log()));
        handles.push(tokio::spawn(app_state.clone().run_block_producer()));
        handles.push(tokio::spawn(app_state.clone().run_finality_voter()));
        handles.push(tokio::spawn(app_state.clone().run_inflight_resend()));
        handles.push(tokio::spawn(app_state.clone().run_room_sweeper()));
        handles.push(tokio::spawn(app_state.clone().run_chain_persist_task()));
        // On by default: a headless node has no other way to turn what it
        // earns into credits. NEPHERAL_AUTO_REDEEM=0 leaves it to the operator.
        if app_kernel::chain_node::auto_redeem_enabled() {
            handles.push(tokio::spawn(app_state.clone().run_auto_redeem()));
        }

        #[cfg(feature = "local-api-server")]
        {
            let cfg = app_kernel::nepheral_config::load_config().unwrap_or_default();
            if cfg.local_api_server_enabled {
                // Checks the token against its hash, so it answers while locked too.
                let security = nepheral_api::server::ApiSecurity {
                    auth_token: None,
                    allowed_origins: cfg.local_api_server_allowed_origins.clone(),
                    allow_unauthenticated: false,
                    gate: Some(std::sync::Arc::new(app_kernel::api_backend::TokenGate(
                        app_state.clone(),
                    ))),
                };
                let addr_str = format!(
                    "{}:{}",
                    cfg.local_api_server_host, cfg.local_api_server_port
                );
                let state_clone = app_state.clone();
                tokio::spawn(async move {
                    let backend = std::sync::Arc::new(
                        app_kernel::api_backend::AppStateBackend::new(state_clone),
                    );
                    let router = nepheral_api::server::router(backend, security);
                    match tokio::net::TcpListener::bind(&addr_str).await {
                        Ok(listener) => {
                            info!(addr = %addr_str, "Local API server listening");
                            if let Err(e) = axum::serve(listener, router).await {
                                warn!(error = %e, "Local API server error");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, addr = %addr_str, "Failed to bind local API server")
                        }
                    }
                });
            }
        }
    }

    shutdown_signal().await;
    info!("Shutdown signal received — stopping subsystems");
    app_state.begin_shutdown().await;
    for h in handles {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), h).await;
    }
    let _ = app_state.persist_chain_now().await;
    info!("Nepheral Node stopped");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
