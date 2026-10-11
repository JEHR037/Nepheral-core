// SPDX-License-Identifier: BUSL-1.1
//! Asking the router to forward the mesh port.
//!
//! Most home routers take a request to forward a port: UPnP-IGD, NAT-PMP or
//! PCP. With one, a node at home is reachable like a server: others dial it
//! directly instead of reaching it through relays, and it can relay for
//! them. The request is n0's `portmapper` (the one iroh uses, MIT/Apache-2.0);
//! a router that refuses, or has none of the three, changes nothing.
//!
//! A phone does not ask: it moves between networks, and it does not relay.

use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::state::{AppState, ADVERTISED_ADDRS_CAP};

impl AppState {
    pub async fn run_port_mapping(self) {
        if self.is_light() {
            return;
        }
        let Some(port) = NonZeroU16::new(self.local_addr.port()) else {
            return;
        };
        let client = portmapper::Client::new(portmapper::Config::default());
        client.update_local_port(port);
        let mut external = client.watch_external_address();
        loop {
            tokio::select! {
                changed = external.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let mapped = external.borrow_and_update().map(SocketAddr::V4);
                    self.apply_port_mapping(mapped).await;
                }
                stop = self.sleep_or_shutdown(Duration::from_secs(600)) => {
                    if stop {
                        break;
                    }
                    // Mappings are leased: ask again now and then.
                    client.procure_mapping();
                }
            }
        }
        client.deactivate();
    }

    async fn apply_port_mapping(&self, mapped: Option<SocketAddr>) {
        // A router behind another one (or the carrier's NAT) hands out an
        // address nobody outside can reach.
        let mapped = mapped.filter(|a| !mesh_network::protocol::is_local_scope(&a.ip()));
        let before = *self.mapped_addr.read().expect("mapped addr");
        if before == mapped {
            return;
        }
        *self.mapped_addr.write().expect("mapped addr") = mapped;
        let Some(addr) = mapped else {
            tracing::info!("The router no longer forwards the mesh port");
            return;
        };
        tracing::info!(external = %addr, "The router forwards the mesh port: reachable directly");
        self.log(format!(
            "Router forwards the mesh port ({addr}): reachable directly"
        ))
        .await;
        // The same port outside as inside: the address HELLO announces
        // (public IP + listen port) is right as it is. Another port is
        // announced through gossip and punch introductions.
        if addr.port() == self.local_addr.port() {
            let mut adv = self.advertised_addrs.write().await;
            if !adv.contains(&addr.ip()) {
                adv.insert(0, addr.ip());
                adv.truncate(ADVERTISED_ADDRS_CAP);
            }
        }
        self.can_relay.store(true, Ordering::Relaxed);
        self.announce_capabilities().await;
        self.gossip_flood().await;
    }
}
