// SPDX-License-Identifier: BUSL-1.1

//! NAT hole punching.
//!
//! Two nodes behind home routers cannot dial each other: each router drops
//! packets nobody inside asked for. Both are connected to the public nodes,
//! though, and a public node sees each of them at the address its router
//! gave it. Asked to, a public node tells each peer where the other is, and
//! both dial at once. Each outgoing packet opens its sender's router for the
//! reply, so a later retransmission from the other side gets in.
//!
//! This works through most home routers and fails through symmetric NAT and
//! carrier-grade NAT, which hand out a different outside port per
//! destination. Those peers keep reaching each other through the relays,
//! sealed so the relays learn nothing.
//!
//! Dials leave from the listening socket (`AppState::mesh_client_for`), so
//! the address a public node sees is the one the router will answer on.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_crypto::identity::PeerId;
use mesh_network::protocol::{frame, message_types, PunchIntro, PunchRequest};
use quinn::Connection;

use crate::state::{spawn_send, AppState};

/// How often a public node introduces the same pair.
const INTRO_EVERY: Duration = Duration::from_secs(20);
/// How long one punch may take. QUIC resends its first packet on its own
/// within this, which is what gets through once the other side has opened
/// its router.
const PUNCH_DIAL_TIMEOUT: Duration = Duration::from_secs(12);
/// Between attempts at the same peer: half a minute, doubling per failure.
const PUNCH_BACKOFF_BASE: Duration = Duration::from_secs(30);
const PUNCH_BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
/// Dials in one punch: the first, then again if it failed outright (an ICMP
/// "unreachable" from a router that had not opened yet ends a dial at once).
const PUNCH_WAVES: u32 = 3;
const PUNCH_WAVE_GAP: Duration = Duration::from_secs(2);
/// Peers asked about per round, so a node that knows a thousand others
/// through relays does not flood its introducers.
const PUNCH_PER_ROUND: usize = 32;
/// How many public nodes are asked per attempt.
const PUNCH_INTRODUCERS: usize = 3;
/// An inbound connection from an address a public node announced within
/// this window is a punch, not a sign this node is publicly reachable.
pub const EXPECTED_PUNCH_WINDOW: Duration = Duration::from_secs(30);
const PUNCH_ROUND_EVERY: Duration = Duration::from_secs(15);

pub fn punch_backoff(failures: u32) -> Duration {
    PUNCH_BACKOFF_BASE
        .saturating_mul(1u32 << failures.min(10))
        .min(PUNCH_BACKOFF_MAX)
}

impl AppState {
    /// Every live connection to `pid`, by address.
    pub async fn connections_of(&self, pid: &PeerId) -> Vec<(SocketAddr, Connection)> {
        let addrs: Vec<SocketAddr> = self
            .peer_id_by_addr
            .read()
            .await
            .iter()
            .filter(|(_, p)| *p == pid)
            .map(|(a, _)| *a)
            .collect();
        let conns = self.active_connections.read().await;
        addrs
            .into_iter()
            .filter_map(|a| conns.get(&a).map(|c| (a, c.clone())))
            .filter(|(_, c)| c.close_reason().is_none())
            .collect()
    }

    /// As a public node: introduce `asker` and `req.target` to each other,
    /// each at the address it is seen from here. Paired by address family,
    /// since a router's IPv4 mapping says nothing about the IPv6 path.
    pub async fn handle_punch_request(&self, req: PunchRequest, asker: PeerId) {
        if !self.can_relay.load(std::sync::atomic::Ordering::Relaxed) || req.target == asker {
            return;
        }
        {
            let mut sent = self.punch_intros_sent.lock().await;
            let key = (asker, req.target);
            if sent.get(&key).is_some_and(|at| at.elapsed() < INTRO_EVERY) {
                return;
            }
            if sent.len() > 4096 {
                sent.retain(|_, at| at.elapsed() < INTRO_EVERY);
            }
            sent.insert(key, Instant::now());
        }
        let asker_conns = self.connections_of(&asker).await;
        let target_conns = self.connections_of(&req.target).await;
        for (a_addr, a_conn) in &asker_conns {
            let Some((t_addr, t_conn)) = target_conns
                .iter()
                .find(|(t, _)| t.is_ipv4() == a_addr.is_ipv4())
            else {
                continue;
            };
            let to_target = PunchIntro {
                version: 1,
                peer: asker,
                addr: *a_addr,
            };
            let to_asker = PunchIntro {
                version: 1,
                peer: req.target,
                addr: *t_addr,
            };
            spawn_send(
                t_conn.clone(),
                Arc::new(frame(message_types::PUNCH_INTRO, &to_target.to_bytes())),
                "punch intro",
            );
            spawn_send(
                a_conn.clone(),
                Arc::new(frame(message_types::PUNCH_INTRO, &to_asker.to_bytes())),
                "punch intro",
            );
            tracing::debug!(
                asker = %hex::encode(&asker[..4]),
                target = %hex::encode(&req.target[..4]),
                "Introduced two peers for a hole punch"
            );
        }
    }

    /// Told by a public node to dial `intro.peer` now; it has been told the
    /// same about this node.
    pub async fn handle_punch_intro(&self, intro: PunchIntro) {
        let me = *self.my_peer_id.read().await;
        if intro.peer == me || self.connection_for_peer(&intro.peer).await.is_some() {
            return;
        }
        if !self.punches_in_flight.lock().await.insert(intro.peer) {
            return;
        }
        self.expected_punches
            .lock()
            .await
            .insert(intro.addr, Instant::now());
        // The address the introducer saw first, then every other this node
        // knows for the peer (its LAN address behind the same router, IPv6).
        let mut targets = vec![intro.addr];
        if let Some(known) = self.peer_dialable.read().await.get(&intro.peer) {
            for a in known {
                if !targets.contains(a) && a.is_ipv4() == intro.addr.is_ipv4() {
                    targets.push(*a);
                }
            }
        }
        targets.truncate(4);
        self.punch_tried
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let state = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut connected = None;
            let mut last_error = String::new();
            for wave in 0..PUNCH_WAVES {
                let left = PUNCH_DIAL_TIMEOUT.saturating_sub(started.elapsed());
                if left.is_zero() {
                    break;
                }
                // All addresses at once; the first that connects wins and
                // the others are dropped.
                let mut dials = tokio::task::JoinSet::new();
                for addr in targets.iter().copied() {
                    let state = state.clone();
                    dials.spawn(async move {
                        tokio::time::timeout(left, state.dial(addr, Some(intro.peer)))
                            .await
                            .map_err(|_| "timed out".to_string())
                            .and_then(|r| r)
                            .map(|_| addr)
                    });
                }
                while let Some(joined) = dials.join_next().await {
                    match joined {
                        Ok(Ok(addr)) => {
                            connected = Some(addr);
                            dials.abort_all();
                            break;
                        }
                        Ok(Err(e)) => last_error = e,
                        Err(_) => {}
                    }
                }
                if connected.is_some() || state.connection_for_peer(&intro.peer).await.is_some() {
                    break;
                }
                if wave + 1 < PUNCH_WAVES {
                    tokio::time::sleep(PUNCH_WAVE_GAP).await;
                }
            }
            let linked =
                connected.is_some() || state.connection_for_peer(&intro.peer).await.is_some();
            if linked {
                state
                    .punch_ok
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                state.punch_attempts.lock().await.remove(&intro.peer);
                tracing::info!(
                    peer = %hex::encode(&intro.peer[..4]),
                    addr = ?connected,
                    "Hole punch connected"
                );
                state
                    .log(format!(
                        "Direct connection to {} through NAT ({})",
                        hex::encode(&intro.peer[..4]),
                        connected.unwrap_or(intro.addr)
                    ))
                    .await;
            } else {
                tracing::info!(
                    peer = %hex::encode(&intro.peer[..4]),
                    tried = targets.len(),
                    error = %last_error,
                    "Hole punch failed; staying on relays"
                );
            }
            state.punches_in_flight.lock().await.remove(&intro.peer);
        });
    }

    /// Whether an inbound connection from `addr` is a punch this node was
    /// told to expect.
    pub async fn is_expected_punch(&self, addr: &SocketAddr) -> bool {
        let mut expected = self.expected_punches.lock().await;
        expected.retain(|_, at| at.elapsed() < EXPECTED_PUNCH_WINDOW);
        expected.contains_key(addr)
    }

    /// Asks the public nodes to introduce this node to every node it knows
    /// only through others.
    pub async fn punch_round(&self) {
        let caps = self.peer_capabilities.read().await.clone();
        let mut introducers: Vec<(PeerId, Connection, Duration)> = self
            .live_connections()
            .await
            .into_iter()
            .filter(|(_, p, _)| caps.get(p).is_some_and(|c| c.can_relay))
            .map(|(_, p, c)| {
                let rtt = c.rtt();
                (p, c, rtt)
            })
            .collect();
        if introducers.is_empty() {
            return;
        }
        introducers.sort_by_key(|(.., rtt)| *rtt);
        let mut seen = HashSet::new();
        introducers.retain(|(p, ..)| seen.insert(*p));
        introducers.truncate(PUNCH_INTRODUCERS);

        let now = Instant::now();
        let mut asked = 0usize;
        for (pid, _) in self.relayed_peers().await {
            if asked >= PUNCH_PER_ROUND {
                break;
            }
            {
                let mut attempts = self.punch_attempts.lock().await;
                let failures = match attempts.get(&pid) {
                    Some((last, failures)) => {
                        if now.duration_since(*last) < punch_backoff(*failures) {
                            continue;
                        }
                        failures + 1
                    }
                    None => 0,
                };
                attempts.insert(pid, (now, failures));
            }
            asked += 1;
            let bytes = Arc::new(frame(
                message_types::PUNCH_REQUEST,
                &PunchRequest {
                    version: 1,
                    target: pid,
                }
                .to_bytes(),
            ));
            for (introducer, conn, _) in &introducers {
                if *introducer != pid {
                    spawn_send(conn.clone(), bytes.clone(), "punch request");
                }
            }
        }
        let mut attempts = self.punch_attempts.lock().await;
        if attempts.len() > 4096 {
            attempts.retain(|_, (last, f)| now.duration_since(*last) < punch_backoff(*f));
        }
    }

    /// Distinct nodes with a live direct connection.
    pub async fn direct_peer_count(&self) -> usize {
        let mut pids: Vec<PeerId> = self
            .live_connections()
            .await
            .into_iter()
            .map(|(_, p, _)| p)
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids.len()
    }

    pub async fn run_hole_punch(self) {
        const REPORT_EVERY: u32 = 40;
        let mut rounds = 0u32;
        loop {
            if self.sleep_or_shutdown(PUNCH_ROUND_EVERY).await {
                break;
            }
            self.punch_round().await;
            rounds += 1;
            // Every ten minutes, how the node reaches the others: what the
            // relay rebuild is measured by.
            if rounds.is_multiple_of(REPORT_EVERY) {
                let direct = self.direct_peer_count().await;
                let relayed = self.relayed_peers().await.len();
                let tried = self.punch_tried.load(std::sync::atomic::Ordering::Relaxed);
                let connected = self.punch_ok.load(std::sync::atomic::Ordering::Relaxed);
                let mapped = *self.mapped_addr.read().expect("mapped addr");
                tracing::info!(
                    direct,
                    relayed,
                    punches_tried = tried,
                    punches_connected = connected,
                    mapped = ?mapped,
                    "NAT traversal"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(punch_backoff(0), Duration::from_secs(30));
        assert_eq!(punch_backoff(1), Duration::from_secs(60));
        assert_eq!(punch_backoff(3), Duration::from_secs(240));
        assert_eq!(punch_backoff(4), Duration::from_secs(480));
        assert_eq!(punch_backoff(5), PUNCH_BACKOFF_MAX);
        assert_eq!(punch_backoff(40), PUNCH_BACKOFF_MAX);
    }
}
