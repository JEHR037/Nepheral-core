// SPDX-License-Identifier: BUSL-1.1

use std::net::SocketAddr;
use std::time::Duration;

use core_crypto::identity::PeerId;
use mesh_network::protocol::{self as proto, message_types};
use tracing::{debug, error, info, warn};

use crate::state::AppState;

const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub fn install_stream_handler(app_state: &AppState) {
    app_state.set_stream_handler(std::sync::Arc::new(|send, recv, state, addr, pid| {
        Box::pin(async move {
            if let Err(e) = handle_stream_with_state(send, recv, state, addr, pid).await {
                debug!(remote = %addr, error = %e, "Stream processing error");
            }
        })
    }));
}

async fn read_body(recv: &mut quinn::RecvStream, max: usize) -> Result<Vec<u8>, String> {
    tokio::time::timeout(FRAME_READ_TIMEOUT, async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read len: {e}"))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > max {
            return Err(format!("frame too large: {len} > {max}"));
        }
        let mut body = vec![0u8; len];
        recv.read_exact(&mut body)
            .await
            .map_err(|e| format!("read body: {e}"))?;
        Ok(body)
    })
    .await
    .map_err(|_| "frame read timed out".to_string())?
}

fn greet_new_connection(app_state: &AppState, connection: &quinn::Connection) {
    let conn_for_announce = connection.clone();
    let state_for_announce = app_state.clone();
    tokio::spawn(async move {
        state_for_announce
            .announce_model_to_peer(&conn_for_announce)
            .await;
    });
    let state_for_sync = app_state.clone();
    let conn_for_sync = connection.clone();
    tokio::spawn(async move {
        state_for_sync.request_block_sync(conn_for_sync).await;
    });
}

pub async fn run_node_with_state(
    endpoint: mesh_network::quic_transport::FragmentEndpoint,
    app_state: AppState,
) -> Result<(), String> {
    loop {
        let shutdown = app_state.shutdown_notify.notified();
        tokio::pin!(shutdown);
        shutdown.as_mut().enable();
        if !app_state.running().await {
            break;
        }
        let incoming = tokio::select! {
            incoming = endpoint.accept() => match incoming {
                Some(conn) => conn,
                None => {
                    warn!("Endpoint closed");
                    break;
                }
            },
            _ = &mut shutdown => break,
        };
        let state_clone = app_state.clone();
        tokio::spawn(async move {
            let connection = match tokio::time::timeout(Duration::from_secs(15), incoming).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(e)) => {
                    debug!(error = %e, "Inbound handshake failed");
                    return;
                }
                Err(_) => return,
            };
            let remote_addr = connection.remote_address();
            // A hole punch arrives inbound too, but says nothing about whether
            // strangers can reach this node: a home node behind NAT must not
            // start offering itself as everyone's relay.
            // Nor does a LAN neighbour reaching it, and a phone never
            // relays: its battery and data are not the network's.
            let from_outside = !mesh_network::protocol::is_local_scope(&remote_addr.ip())
                && !state_clone.is_light();
            // And one stranger is not enough: a punch whose other end
            // dialed before this node read its own introduction looks like
            // one. Two outside sockets reaching it unasked are.
            let strangers = if from_outside && !state_clone.is_expected_punch(&remote_addr).await {
                let mut seen = state_clone.inbound_strangers.lock().expect("strangers");
                if seen.len() < 16 {
                    seen.insert(remote_addr);
                }
                seen.len()
            } else {
                0
            };
            if strangers >= 2
                && !state_clone
                    .can_relay
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                info!("First inbound connection accepted — this node now advertises CAN_RELAY");
            }
            state_clone
                .register_connection(remote_addr, connection.clone())
                .await;
            greet_new_connection(&state_clone, &connection);
        });
    }
    Ok(())
}

pub async fn handle_stream_with_state(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    app_state: AppState,
    remote_addr: SocketAddr,
    remote_pid: PeerId,
) -> Result<(), String> {
    let mut type_buf = [0u8; 1];
    tokio::time::timeout(FRAME_READ_TIMEOUT, recv.read_exact(&mut type_buf))
        .await
        .map_err(|_| "type read timed out".to_string())?
        .map_err(|_| "Failed to read message type".to_string())?;

    match type_buf[0] {
        message_types::HELLO => {
            let body = read_body(&mut recv, proto::MAX_HELLO_BYTES).await?;
            let hello = match proto::Hello::from_bytes(&body) {
                Ok(h) => h,
                Err(e) => {
                    if let Some(v) = proto::Hello::peek_version(&body) {
                        app_state.note_peer_protocol(v);
                    }
                    return Err(e);
                }
            };
            app_state
                .process_hello(hello, remote_addr, remote_pid)
                .await?;
        }

        message_types::PEER_LIST_REQUEST => {
            let peers = app_state.dialable_peer_entries(remote_pid).await;
            let serialized = proto::PeerList::new(peers).to_bytes();
            let bytes = proto::frame(message_types::PEER_LIST, &serialized);
            tokio::time::timeout(FRAME_READ_TIMEOUT, send.write_all(&bytes))
                .await
                .map_err(|_| "peer list write timed out".to_string())?
                .map_err(|e| format!("peer list write: {e}"))?;
            let _ = send.finish();
        }

        message_types::PEER_LIST => {
            let body = read_body(&mut recv, proto::MAX_PEER_LIST_BYTES).await?;
            let peers: Vec<SocketAddr> = proto::PeerList::from_bytes(&body)?
                .to_socket_addrs()
                .into_iter()
                .filter(|a| crate::state::reachable_from_here(a, &remote_addr.ip()))
                .collect();
            app_state.record_discovered(&peers, 50).await;
        }

        message_types::PRESENCE => {
            let body = read_body(&mut recv, proto::MAX_PRESENCE_BYTES).await?;
            app_state
                .ingest_presence(proto::Presence::from_bytes(&body)?)
                .await;
        }

        message_types::PUNCH_REQUEST => {
            let body = read_body(&mut recv, proto::MAX_PUNCH_BYTES).await?;
            let req = proto::PunchRequest::from_bytes(&body).ok_or("PUNCH_REQUEST malformed")?;
            app_state.handle_punch_request(req, remote_pid).await;
        }

        message_types::PUNCH_INTRO => {
            let body = read_body(&mut recv, proto::MAX_PUNCH_BYTES).await?;
            let intro = proto::PunchIntro::from_bytes(&body).ok_or("PUNCH_INTRO malformed")?;
            // Only a node that can actually see both sides from outside may
            // say where to dial.
            let introducer_relays = app_state
                .peer_capabilities
                .read()
                .await
                .get(&remote_pid)
                .is_some_and(|c| c.can_relay);
            if introducer_relays {
                app_state.handle_punch_intro(intro).await;
            }
        }

        message_types::SEALED => {
            let body = read_body(&mut recv, proto::MAX_SEALED_BYTES).await?;
            app_state.handle_sealed(&body).await?;
        }

        message_types::SHARE_CLAIM_RELAYED => {
            let body = read_body(&mut recv, proto::MAX_SHARE_CLAIM_BYTES).await?;
            let claim = proto::AgentClaim::from_bytes(&body).ok_or("claim malformed")?;
            if claim.claimant_pid == remote_pid {
                app_state.handle_relayed_claim(claim).await;
            }
        }

        message_types::SHARE_RETURN => {
            let body = read_body(&mut recv, proto::MAX_SHARE_PUSH_BYTES).await?;
            let sp = proto::SharePush::from_bytes(&body).ok_or("SHARE_RETURN malformed")?;
            app_state.handle_share_return(sp).await?;
        }

        message_types::GOSSIP_PEER => {
            let body = read_body(&mut recv, proto::MAX_GOSSIP_PEER_BYTES).await?;
            app_state
                .process_gossip_peer(&body, remote_addr.ip())
                .await?;
        }

        message_types::NODE_STATS => {
            let body = read_body(&mut recv, 256).await?;
            if let Some(stats) = crate::state::NodeStats::from_bytes(&body) {
                app_state.peer_stats.write().await.insert(remote_pid, stats);
            }
        }

        message_types::ROOM_ENVELOPE => {
            let body = read_body(&mut recv, proto::MAX_ROOM_ENVELOPE_BYTES).await?;
            app_state.process_room_envelope(&body, remote_addr).await?;
        }

        message_types::CHAIN_TX => {
            let body = read_body(&mut recv, proto::MAX_CHAIN_TX_BYTES).await?;
            if let Some(ext) = core_crypto::Extrinsic::from_bytes(&body) {
                if app_state.handle_incoming_chain_tx(ext.clone()).await {
                    let bytes = ext.to_bytes();
                    app_state
                        .broadcast(message_types::CHAIN_TX, &bytes, Some(remote_addr))
                        .await;
                }
            }
        }

        message_types::BLOCK_BROADCAST => {
            let body = read_body(&mut recv, proto::MAX_BLOCK_BYTES).await?;
            let block: core_crypto::Block =
                core_crypto::wire::decode(&body).ok_or("block malformed")?;
            let height = block.header.height;
            let local_head = app_state.chain.read().await.head_height();
            match app_state.apply_remote_block(block).await {
                Ok(()) => {
                    info!(height, "Applied remote block");
                    app_state
                        .broadcast(message_types::BLOCK_BROADCAST, &body, Some(remote_addr))
                        .await;
                }
                Err(core_crypto::ChainError::BadBlockHeight)
                | Err(core_crypto::ChainError::BadParentHash)
                    if height > local_head =>
                {
                    let conn = app_state
                        .active_connections
                        .read()
                        .await
                        .get(&remote_addr)
                        .cloned();
                    if let Some(c) = conn {
                        app_state.request_block_sync(c).await;
                    }
                }
                Err(core_crypto::ChainError::BadBlockHeight) => {}
                // Logged once by note_upgrade_required, not per block.
                Err(core_crypto::ChainError::UpgradeRequired { .. }) => {}
                Err(e) => warn!(error = ?e, height, "Remote block rejected"),
            }
        }

        message_types::FINALITY_VOTE => {
            let body = read_body(&mut recv, proto::MAX_FINALITY_VOTE_BYTES).await?;
            let vote = core_crypto::finality::FinalityVote::from_bytes(&body)
                .ok_or("finality vote malformed")?;
            if app_state.handle_finality_vote(vote).await {
                app_state
                    .broadcast(message_types::FINALITY_VOTE, &body, Some(remote_addr))
                    .await;
            }
        }

        message_types::BLOCK_SYNC_REQUEST => {
            let mut buf = [0u8; 16];
            tokio::time::timeout(FRAME_READ_TIMEOUT, recv.read_exact(&mut buf))
                .await
                .map_err(|_| "sync request timed out".to_string())?
                .map_err(|e| format!("read sync request: {e}"))?;
            let from = u64::from_be_bytes(buf[..8].try_into().map_err(|_| "bad from")?);
            let to = u64::from_be_bytes(buf[8..].try_into().map_err(|_| "bad to")?);
            app_state.serve_block_sync(&mut send, from, to).await?;
        }

        message_types::LIGHT_SYNC_REQUEST => {
            app_state.serve_light_sync(&mut send).await?;
        }

        message_types::MODEL_ANNOUNCE => {
            let body = read_body(&mut recv, 64).await?;
            if body.len() == 40 {
                let hash: [u8; 32] = body[..32].try_into().map_err(|_| "bad hash")?;
                let size = u64::from_be_bytes(body[32..].try_into().map_err(|_| "bad size")?);
                app_state.on_model_announce(remote_pid, hash, size).await;
            }
        }

        message_types::MODEL_REQUEST => {
            let body = read_body(&mut recv, 32).await?;
            let hash: [u8; 32] = body.as_slice().try_into().map_err(|_| "bad hash")?;
            app_state
                .serve_model_to_peer(remote_addr, remote_pid, hash)
                .await;
        }

        message_types::MODEL_TRANSFER => {
            let mut header = [0u8; 40];
            tokio::time::timeout(FRAME_READ_TIMEOUT, recv.read_exact(&mut header))
                .await
                .map_err(|_| "transfer header timed out".to_string())?
                .map_err(|e| format!("read transfer header: {e}"))?;
            let hash: [u8; 32] = header[..32].try_into().map_err(|_| "bad hash")?;
            let size = u64::from_be_bytes(header[32..].try_into().map_err(|_| "bad size")?);
            app_state
                .receive_model_transfer(remote_pid, hash, size, recv)
                .await;
        }

        message_types::SHARE_PUSH => {
            let body = read_body(&mut recv, proto::MAX_SHARE_PUSH_BYTES).await?;
            let sp = proto::SharePush::from_bytes(&body).ok_or("SHARE_PUSH decode failed")?;
            app_state.process_share_push(sp, remote_pid).await?;
        }

        message_types::AGENT_DECLINE => {
            let body = read_body(&mut recv, proto::MAX_AGENT_DECLINE_BYTES).await?;
            let d = proto::AgentDecline::from_bytes(&body).ok_or("AGENT_DECLINE decode failed")?;
            // Counted against the TLS identity, not the payload: a decline
            // shortens the queue for whoever is behind, so it must come from
            // the node whose place it gives up.
            app_state
                .note_observed_decline((d.sender_pid, d.message_id), remote_pid)
                .await;
        }

        message_types::SIGNED_DECLINE => {
            let body = read_body(&mut recv, proto::MAX_SIGNED_DECLINE_BYTES).await?;
            let d =
                proto::SignedDecline::from_bytes(&body).ok_or("SIGNED_DECLINE decode failed")?;
            app_state.handle_signed_decline(d).await;
        }

        message_types::SHARE_CLAIM => {
            let body = read_body(&mut recv, proto::MAX_SHARE_CLAIM_BYTES).await?;
            let claim = proto::AgentClaim::from_bytes(&body).ok_or("SHARE_CLAIM decode failed")?;
            app_state.observe_claim(&claim).await;
            match app_state.process_share_claim(claim, remote_pid).await {
                Some(release) => {
                    let bytes = release.to_bytes();
                    let mut out = Vec::with_capacity(4 + bytes.len());
                    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                    out.extend_from_slice(&bytes);
                    tokio::time::timeout(FRAME_READ_TIMEOUT, send.write_all(&out))
                        .await
                        .map_err(|_| "release write timed out".to_string())?
                        .map_err(|e| format!("release write: {e}"))?;
                    let _ = send.finish();
                }
                None => {
                    let _ = send.finish();
                }
            }
        }

        message_types::AGENT_RESPONSE_DIRECT => {
            let body = read_body(&mut recv, proto::MAX_AGENT_RESPONSE_BYTES).await?;
            let resp = proto::AgentResponseDirect::from_bytes(&body)
                .ok_or("AGENT_RESPONSE_DIRECT decode failed")?;
            app_state
                .process_agent_response(resp, &body, remote_addr)
                .await?;
        }

        message_types::RELAY_OPEN => {
            let body = read_body(&mut recv, proto::MAX_RELAY_OPEN_BYTES + 1024).await?;
            let ro = proto::RelayOpen::from_bytes(&body).ok_or("RELAY_OPEN decode failed")?;
            app_state.handle_relay_open(ro, remote_addr).await;
        }

        other => {
            return Err(format!("Unknown message type: 0x{other:02x}"));
        }
    }
    Ok(())
}

pub async fn run_peer_connection_manager(app_state: AppState) -> Result<(), String> {
    loop {
        if !app_state.running().await {
            break;
        }
        let bootstrap = app_state.bootstrap_peers.read().await.clone();
        for peer_addr in bootstrap {
            let connected = app_state
                .active_connections
                .read()
                .await
                .get(&peer_addr)
                .is_some_and(|c| c.close_reason().is_none());
            if connected {
                continue;
            }
            let state = app_state.clone();
            tokio::spawn(async move {
                match state.dial(peer_addr, None).await {
                    Ok(_) => {
                        state.clear_net_problem();
                        if let Some(conn) = state
                            .active_connections
                            .read()
                            .await
                            .get(&peer_addr)
                            .cloned()
                        {
                            greet_new_connection(&state, &conn);
                        }
                    }
                    Err(e) => {
                        state.note_net_problem(crate::state::NetProblem::of_dial_error(&e));
                        debug!(peer = %peer_addr, error = %e, "Bootstrap dial failed")
                    }
                }
            });
        }
        if app_state.sleep_or_shutdown(Duration::from_secs(10)).await {
            break;
        }
    }
    Ok(())
}

pub async fn run_peer_discovery(app_state: AppState) {
    const QUERY_BUDGET: Duration = Duration::from_secs(25);
    let mut first_run = true;
    loop {
        if !app_state.running().await {
            break;
        }
        if !first_run && app_state.sleep_or_shutdown(Duration::from_secs(30)).await {
            break;
        }
        first_run = false;
        let introducers = app_state.bootstrap_introducers.read().await.clone();
        for introducer in introducers {
            match tokio::time::timeout(
                QUERY_BUDGET,
                app_state.query_bootstrap_introducer(&introducer),
            )
            .await
            {
                Ok(Ok(peers)) => {
                    debug!(count = peers.len(), introducer = %introducer, "Bootstrap discovery")
                }
                Ok(Err(e)) => {
                    debug!(error = %e, introducer = %introducer, "Bootstrap query failed")
                }
                Err(_) => debug!(introducer = %introducer, "Bootstrap query timed out"),
            }
        }
        for (_, _, conn) in app_state.live_connections().await {
            let _ = tokio::time::timeout(QUERY_BUDGET, app_state.request_peer_list(&conn)).await;
        }
        app_state.connect_discovered_peers().await;
    }
}

pub async fn run_capability_heartbeat(app_state: AppState) {
    loop {
        if app_state.sleep_or_shutdown(Duration::from_secs(30)).await {
            break;
        }
        app_state.announce_capabilities().await;
    }
}

pub async fn run_gossip_flood(app_state: AppState) {
    loop {
        if !app_state.running().await {
            break;
        }
        app_state.gossip_flood().await;
        app_state.connect_discovered_peers().await;
        if app_state.sleep_or_shutdown(Duration::from_secs(30)).await {
            break;
        }
    }
}

pub async fn run_ai_session_decay(app_state: AppState) {
    loop {
        if !app_state.running().await {
            break;
        }
        app_state.decay_ai_sessions().await;
        if app_state.sleep_or_shutdown(Duration::from_secs(5)).await {
            break;
        }
    }
}

pub async fn run_stale_peer_cleanup(app_state: AppState) {
    loop {
        if app_state.sleep_or_shutdown(Duration::from_secs(60)).await {
            break;
        }
        app_state.prune_stale_peers().await;
    }
}

/// How often stats go out when nothing changes. Busy and free are also pushed
/// the moment they change; this only keeps uptime and scores fresh. It was a
/// minute, which left a restarted or newly busy node misjudged for that long.
pub const STATS_EVERY: Duration = Duration::from_secs(15);

pub async fn run_stats_broadcast(app_state: AppState) {
    loop {
        if !app_state.running().await {
            break;
        }
        app_state.broadcast_node_stats().await;
        if app_state.sleep_or_shutdown(STATS_EVERY).await {
            break;
        }
    }
}

pub async fn run_mdns_announce_and_discover(app_state: AppState, port: u16) {
    use mdns_sd::{ServiceDaemon, ServiceInfo};
    use std::net::IpAddr;

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "mDNS daemon failed - LAN discovery disabled");
            return;
        }
    };
    let local_ip =
        local_ip_address::local_ip().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let service_type = format!("_nepheral-{}._udp.local.", app_state.network.name());
    if let Ok(info) = ServiceInfo::new(
        &service_type,
        &format!("nepheral-{:016x}", app_state.node_id),
        &format!("nepheral-{:016x}.local.", app_state.node_id),
        local_ip,
        port,
        None,
    ) {
        if let Err(e) = daemon.register(info) {
            warn!(error = %e, "Failed to register mDNS service");
        }
    }
    let receiver = match daemon.browse(&service_type) {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "Failed to start mDNS browsing");
            return;
        }
    };
    loop {
        if !app_state.running().await {
            break;
        }
        while let Ok(event) = receiver.try_recv() {
            if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                let discovered_port = info.get_port();
                let addrs: Vec<SocketAddr> = info
                    .get_addresses()
                    .iter()
                    .map(|a| SocketAddr::new(*a, discovered_port))
                    .filter(|a| !(a.port() == port && a.ip() == local_ip))
                    .take(4)
                    .collect();
                app_state.record_discovered(&addrs, 75).await;
            }
        }
        if app_state.sleep_or_shutdown(Duration::from_secs(2)).await {
            break;
        }
    }
    if let Err(e) = daemon.shutdown() {
        error!(error = %e, "mDNS shutdown error");
    }
}
