// SPDX-License-Identifier: BUSL-1.1

//! Every node knows every node.
//!
//! A node used to learn another's identity and capabilities only from a
//! HELLO on a direct connection, and forgot them when it closed. Two nodes
//! behind NAT never connect directly, so neither ever learned the other
//! existed: neither held shares for the other's questions, and neither could
//! be drawn to answer them. Each node now passes on the newest signed HELLO
//! of every node it has heard of. A HELLO verifies under its author's key
//! wherever it travels, so passing one on cannot change what it says.

use std::collections::HashMap;
use std::sync::Arc;

use core_crypto::identity::PeerId;
use mesh_network::protocol::{
    frame, message_types, Hello, PeerCapabilities, Presence, MAX_PRESENCE_ENTRIES, PRESENCE_TTL_MS,
};

use crate::state::{now_ms, spawn_send, AppState, HELLO_MAX_SKEW_MS};

/// Nodes kept in the presence table at most.
pub const MAX_PRESENCE_KNOWN: usize = MAX_PRESENCE_ENTRIES * 4;

/// A HELLO still describes a live node.
pub fn is_fresh(hello: &Hello, now: u64) -> bool {
    hello.sent_at_ms <= now + HELLO_MAX_SKEW_MS
        && now.saturating_sub(hello.sent_at_ms) <= PRESENCE_TTL_MS
}

impl AppState {
    /// Keeps `hello` if it is newer than what is known of its author.
    ///
    /// The table is bounded. When it is full, a node the chain knows pushes
    /// out the stalest node it does not; a fresh key is turned away. So a
    /// flood of new keys cannot crowd out the nodes that have a history.
    pub async fn remember_presence(&self, hello: Hello) {
        // The chain before the presence table, the order everywhere else.
        let chain = self.chain.read().await;
        let mut presence = self.presence.write().await;
        let newer = presence
            .get(&hello.peer_id)
            .is_none_or(|known| hello.sent_at_ms > known.sent_at_ms);
        if !newer {
            return;
        }
        if presence.len() >= MAX_PRESENCE_KNOWN && !presence.contains_key(&hello.peer_id) {
            if !crate::sybil::is_established(&chain, &hello.peer_id) {
                return;
            }
            let stalest_fresh = presence
                .iter()
                .filter(|(pid, _)| !crate::sybil::is_established(&chain, pid))
                .min_by_key(|(_, h)| h.sent_at_ms)
                .map(|(pid, _)| *pid);
            match stalest_fresh {
                Some(pid) => {
                    presence.remove(&pid);
                }
                None => return,
            }
        }
        presence.insert(hello.peer_id, hello);
    }

    /// HELLOs another node passed on. Each is checked as if it had arrived
    /// from its author: same protocol, valid signature, recent.
    pub async fn ingest_presence(&self, presence: Presence) {
        let me = *self.my_peer_id.read().await;
        let now = now_ms();
        let mine = self.advertised_addrs.read().await.clone();
        let mut same_router = Vec::new();
        for hello in presence.hellos {
            if hello.peer_id == me
                || !mesh_network::protocol::speaks(hello.version)
                || !is_fresh(&hello, now)
                || hello.verify().is_err()
            {
                continue;
            }
            // Behind the same router: its private addresses are on this
            // network, and dialing them beats hairpinning through the
            // router or relaying through a server.
            if hello.behind_same_router(&mine) {
                same_router.extend(
                    hello
                        .addrs
                        .iter()
                        .filter(|a| mesh_network::protocol::is_local_scope(a))
                        .map(|a| std::net::SocketAddr::new(*a, hello.listen_port)),
                );
            }
            self.remember_presence(hello).await;
        }
        if !same_router.is_empty() {
            let mut discovered = self.discovered_peers.lock().await;
            for addr in same_router {
                discovered.entry(addr).or_insert(100);
            }
            drop(discovered);
            self.discovery_notify.notify_waiters();
        }
    }

    /// What this node passes on: its own HELLO and every fresh one it holds,
    /// newest first.
    pub async fn presence_snapshot(&self) -> Presence {
        let now = now_ms();
        let mut hellos: Vec<Hello> = self
            .presence
            .read()
            .await
            .values()
            .filter(|h| is_fresh(h, now))
            .cloned()
            .collect();
        hellos.sort_by_key(|h| std::cmp::Reverse(h.sent_at_ms));
        hellos.truncate(MAX_PRESENCE_ENTRIES - 1);
        hellos.insert(0, self.build_hello(None).await);
        Presence { hellos }
    }

    pub async fn broadcast_presence(&self) {
        let bytes = Arc::new(frame(
            message_types::PRESENCE,
            &self.presence_snapshot().await.to_bytes(),
        ));
        for (_, _, conn) in self.live_connections().await {
            spawn_send(conn, bytes.clone(), "presence");
        }
    }

    /// Every node known to be alive, with what it can do. A direct
    /// connection's HELLO wins over a passed-on one: it is the same signed
    /// statement, but known to be current.
    pub async fn known_peer_caps(&self) -> HashMap<PeerId, PeerCapabilities> {
        let me = *self.my_peer_id.read().await;
        let now = now_ms();
        let mut out: HashMap<PeerId, PeerCapabilities> = self
            .presence
            .read()
            .await
            .iter()
            .filter(|(pid, h)| **pid != me && is_fresh(h, now))
            .map(|(pid, h)| (*pid, PeerCapabilities::from_hello(h)))
            .collect();
        for (pid, caps) in self.peer_capabilities.read().await.iter() {
            out.insert(*pid, caps.clone());
        }
        out
    }

    /// Nodes known only through others: alive, but with no direct connection.
    pub async fn relayed_peers(&self) -> Vec<(PeerId, Hello)> {
        let me = *self.my_peer_id.read().await;
        let now = now_ms();
        let direct: std::collections::HashSet<PeerId> = self
            .live_connections()
            .await
            .into_iter()
            .map(|(_, p, _)| p)
            .collect();
        self.presence
            .read()
            .await
            .iter()
            .filter(|(pid, h)| **pid != me && !direct.contains(*pid) && is_fresh(h, now))
            .map(|(pid, h)| (*pid, h.clone()))
            .collect()
    }

    pub async fn prune_presence(&self) {
        let now = now_ms();
        self.presence.write().await.retain(|_, h| is_fresh(h, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_network::protocol::PROTOCOL_VERSION;

    fn hello(sent_at_ms: u64) -> Hello {
        Hello {
            version: PROTOCOL_VERSION,
            peer_id: [1u8; 32],
            listen_port: 443,
            caps: 0,
            model: None,
            addrs: Vec::new(),
            sent_at_ms,
            observed: None,
            signature: [0u8; 64],
        }
    }

    #[test]
    fn a_hello_older_than_the_ttl_is_not_fresh() {
        let now = 10 * PRESENCE_TTL_MS;
        assert!(is_fresh(&hello(now - PRESENCE_TTL_MS), now));
        assert!(!is_fresh(&hello(now - PRESENCE_TTL_MS - 1), now));
    }

    #[test]
    fn a_hello_from_far_in_the_future_is_not_fresh() {
        let now = 10 * PRESENCE_TTL_MS;
        assert!(is_fresh(&hello(now + HELLO_MAX_SKEW_MS), now));
        assert!(!is_fresh(&hello(now + HELLO_MAX_SKEW_MS + 1), now));
    }
}
