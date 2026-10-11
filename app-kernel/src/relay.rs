// SPDX-License-Identifier: BUSL-1.1
//! The relay layer: how a frame reaches a node this one has no direct
//! connection to.
//!
//! Relays are blind and interchangeable. A frame for a node out of reach is
//! sealed to that node (`SEALED`: ephemeral X25519 to its key, AEAD), so a
//! relay learns who it is for and how big it is, not what kind of message it
//! is. Any node that strangers can reach relays (`CAN_RELAY`: two outside
//! sockets reached it unasked, or its router forwards the mesh port), not
//! only the project's servers; each frame goes through the two closest
//! relays, and a relay with no link to the destination passes it on once
//! more (`MAX_RELAY_HOPS`). The first relay checks that the frame comes from
//! the node it names as origin. Answers to askers on 0.5.3 or later are
//! sealed too (`SEALED_REPLIES`); older askers get theirs as before, inside
//! a frame whose answer text was always sealed.
//!
//! Direct links are what this layer tries to make unnecessary: port mapping
//! (`portmap.rs`), hole punching (`punch.rs`) and LAN links behind one
//! router come first.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_crypto::identity::PeerId;
use mesh_network::protocol::{
    frame, message_types, AgentClaim, RelayOpen, SharePush, MAX_SHARE_PUSH_BYTES,
};
use quinn::Connection;

use crate::agent::{RELAY_FANOUT, RELAY_ROUTE_CAP, RELAY_ROUTE_TTL};
use crate::state::{send_frame, spawn_send, AppState};

#[derive(Clone)]
pub struct RelayRoute {
    pub origin_pid: PeerId,
    pub back_conn: Connection,
    pub created_at: Instant,
}

impl AppState {
    /// Sends one frame to `dest`: on a direct connection when there is one,
    /// otherwise sealed to `dest` and carried by the relays.
    pub async fn send_to_peer(&self, dest: PeerId, msg_type: u8, bytes: Vec<u8>, message_id: u64) {
        if let Some((_, conn)) = self.connection_for_peer(&dest).await {
            if let Err(e) = send_frame(&conn, &frame(msg_type, &bytes)).await {
                tracing::debug!(dest = %hex::encode(&dest[..4]), error = %e, "direct send failed");
            }
            return;
        }
        self.relay_send_sealed(dest, msg_type, &bytes, message_id)
            .await;
    }

    /// Relays a frame sealed to `dest`. The relays learn who it is for and
    /// how big it is, nothing else - not even which kind of message it is.
    pub async fn relay_send_sealed(
        &self,
        dest: PeerId,
        inner_msg_type: u8,
        inner_bytes: &[u8],
        message_id: u64,
    ) {
        let sealed = match core_crypto::agent_crypto::seal_to_peer(
            &dest,
            &frame(inner_msg_type, inner_bytes),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = ?e, "could not seal a frame for relaying");
                return;
            }
        };
        self.relay_send(
            dest,
            message_types::SEALED,
            core_crypto::wire::encode(&sealed),
            message_id,
        )
        .await;
    }

    /// A frame sealed to this node, handed over by the last relay on its
    /// path. Relays authenticate nothing, so only messages that carry their
    /// author's signature are accepted this way.
    pub async fn handle_sealed(&self, body: &[u8]) -> Result<(), String> {
        let sealed: core_crypto::agent_crypto::SealedMessage =
            core_crypto::wire::decode(body).ok_or("sealed frame malformed")?;
        let opened = core_crypto::agent_crypto::open_from_peer(&self.signing_key, &sealed)
            .map_err(|e| format!("sealed frame did not open: {e:?}"))?;
        if opened.len() < 5 {
            return Err("sealed frame too short".into());
        }
        let msg_type = opened[0];
        let len = u32::from_be_bytes([opened[1], opened[2], opened[3], opened[4]]) as usize;
        if len > MAX_SHARE_PUSH_BYTES {
            return Err("sealed frame too large".into());
        }
        let payload = opened.get(5..5 + len).ok_or("sealed frame truncated")?;
        match msg_type {
            message_types::SHARE_PUSH => {
                let sp = SharePush::from_bytes(payload).ok_or("sealed SHARE_PUSH malformed")?;
                let from = sp.sender_pid;
                self.process_share_push(sp, from).await
            }
            message_types::SHARE_CLAIM_RELAYED => {
                let claim = AgentClaim::from_bytes(payload).ok_or("sealed claim malformed")?;
                self.handle_relayed_claim(claim).await;
                Ok(())
            }
            message_types::SHARE_RETURN => {
                let sp = SharePush::from_bytes(payload).ok_or("sealed SHARE_RETURN malformed")?;
                self.handle_share_return(sp).await
            }
            message_types::SIGNED_DECLINE => {
                let d = mesh_network::protocol::SignedDecline::from_bytes(payload)
                    .ok_or("sealed decline malformed")?;
                self.handle_signed_decline(d).await;
                Ok(())
            }
            // An answer, signed by its responder: sealed so the relays
            // carrying it learn nothing (`SEALED_REPLIES`). Only this node's
            // own questions are answered this way, so it is never passed on.
            message_types::AGENT_RESPONSE_DIRECT => {
                let resp = mesh_network::protocol::AgentResponseDirect::from_bytes(payload)
                    .ok_or("sealed answer malformed")?;
                if !self
                    .pending_queries
                    .read()
                    .await
                    .contains_key(&resp.query_message_id)
                {
                    return Ok(());
                }
                self.process_agent_response(resp, payload, SocketAddr::from(([0, 0, 0, 0], 0)))
                    .await
            }
            other => Err(format!("message type 0x{other:02x} is not accepted sealed")),
        }
    }

    pub async fn best_relay_peer(&self, exclude: &[PeerId]) -> Option<(PeerId, Connection)> {
        self.best_relay_peers(exclude, 1).await.into_iter().next()
    }

    /// The `n` relays with the lowest round trip, each node once.
    pub async fn best_relay_peers(
        &self,
        exclude: &[PeerId],
        n: usize,
    ) -> Vec<(PeerId, Connection)> {
        let caps = self.peer_capabilities.read().await.clone();
        let mut relays: Vec<(PeerId, Connection, Duration)> = self
            .live_connections()
            .await
            .into_iter()
            .filter(|(_, pid, _)| {
                !exclude.contains(pid) && caps.get(pid).is_some_and(|c| c.can_relay)
            })
            .map(|(_, pid, conn)| {
                let rtt = conn.rtt();
                (pid, conn, rtt)
            })
            .collect();
        relays.sort_by_key(|(.., rtt)| *rtt);
        let mut seen = HashSet::new();
        relays.retain(|(pid, ..)| seen.insert(*pid));
        relays.truncate(n);
        relays
            .into_iter()
            .map(|(pid, conn, _)| (pid, conn))
            .collect()
    }

    pub async fn relay_send(
        &self,
        dest_pid: PeerId,
        inner_msg_type: u8,
        inner_bytes: Vec<u8>,
        message_id: u64,
    ) {
        let me = *self.my_peer_id.read().await;
        // Two relays, not one: the first may not reach the destination, and
        // a copy that arrives twice is dropped there (every relayed message
        // is identified and idempotent).
        let relays = self.best_relay_peers(&[me, dest_pid], RELAY_FANOUT).await;
        if relays.is_empty() {
            tracing::debug!(dest = %hex::encode(&dest_pid[..4]), "no relay to reach a peer through");
            return;
        }
        let ro = RelayOpen {
            version: 1,
            origin_pid: me,
            dest_pid,
            message_id,
            hop_ttl: mesh_network::protocol::MAX_RELAY_HOPS,
            inner_msg_type,
            inner_bytes,
        };
        let bytes = Arc::new(frame(message_types::RELAY_OPEN, &ro.to_bytes()));
        for (_, relay_conn) in relays {
            spawn_send(relay_conn, bytes.clone(), "relay open");
        }
    }

    pub async fn handle_relay_open(&self, ro: RelayOpen, from_addr: SocketAddr) {
        if !ro.inner_type_is_relayable()
            || ro.inner_bytes.len() > mesh_network::protocol::MAX_RELAY_OPEN_BYTES
            || ro.hop_ttl == 0
        {
            return;
        }
        let me = *self.my_peer_id.read().await;
        // The first relay knows who handed it the frame: it must be the
        // origin the frame names, or a node could pass its traffic off as
        // another's (and fill the route table in its name).
        if ro.hop_ttl == mesh_network::protocol::MAX_RELAY_HOPS {
            let sender = self.peer_id_by_addr.read().await.get(&from_addr).copied();
            if sender.is_some_and(|s| s != ro.origin_pid) {
                tracing::debug!("relay frame names an origin other than its sender");
                return;
            }
        }
        if ro.dest_pid == me {
            if ro.inner_msg_type == message_types::SEALED {
                if let Err(e) = self.handle_sealed(&ro.inner_bytes).await {
                    tracing::debug!(error = %e, "sealed frame rejected");
                }
            }
            return;
        }
        let Some(back_conn) = self
            .active_connections
            .read()
            .await
            .get(&from_addr)
            .cloned()
        else {
            return;
        };
        {
            let mut routes = self.relay_routes.write().await;
            if routes.len() >= RELAY_ROUTE_CAP {
                // Expired routes go first: a node whose sweeper does not run
                // (or runs late) must not stop relaying for good.
                routes.retain(|_, r| r.created_at.elapsed() < RELAY_ROUTE_TTL);
            }
            if routes.len() >= RELAY_ROUTE_CAP {
                return;
            }
            routes.insert(
                (ro.origin_pid, ro.message_id),
                RelayRoute {
                    origin_pid: ro.origin_pid,
                    back_conn,
                    created_at: Instant::now(),
                },
            );
        }
        if let Some((_, dest_conn)) = self.connection_for_peer(&ro.dest_pid).await {
            spawn_send(
                dest_conn,
                Arc::new(frame(ro.inner_msg_type, &ro.inner_bytes)),
                "relay final hop",
            );
            return;
        }
        if ro.hop_ttl <= 1 {
            return;
        }
        let Some((_, next_conn)) = self
            .best_relay_peer(&[me, ro.origin_pid, ro.dest_pid])
            .await
        else {
            return;
        };
        let mut next = ro;
        next.hop_ttl -= 1;
        spawn_send(
            next_conn,
            Arc::new(frame(message_types::RELAY_OPEN, &next.to_bytes())),
            "relay hop",
        );
    }

    pub(crate) async fn maybe_relay_agent_response(
        &self,
        message_id: u64,
        raw: &[u8],
        from_addr: SocketAddr,
    ) {
        let route = {
            let mut routes = self.relay_routes.write().await;
            let key = routes.keys().find(|(_, mid)| *mid == message_id).copied();
            key.and_then(|k| routes.remove(&k))
        };
        let Some(route) = route else {
            return;
        };
        if route.back_conn.remote_address() == from_addr {
            return;
        }
        tracing::debug!(origin = %hex::encode(&route.origin_pid[..4]), "Relaying agent response back");
        spawn_send(
            route.back_conn,
            Arc::new(frame(message_types::AGENT_RESPONSE_DIRECT, raw)),
            "relay response",
        );
    }
}
