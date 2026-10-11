// SPDX-License-Identifier: BUSL-1.1
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::{Endpoint, IdleTimeout, Incoming, ServerConfig, TransportConfig, VarInt};
use tracing::{info, warn};

use crate::tls::TlsIdentity;

#[derive(Debug, Clone)]
pub struct EndpointConfig {
    pub bind_addr: SocketAddr,
    /// Optional second listener for IPv6, normally `[::]:<same port>`. Kept
    /// separate from `bind_addr` rather than a dual-stack socket, since
    /// IPv4-mapped addresses (`::ffff:a.b.c.d`) would split one peer across
    /// two entries in every `SocketAddr`-keyed map in `AppState`.
    pub bind_addr_v6: Option<SocketAddr>,
    pub identity: TlsIdentity,
    pub max_concurrent_streams: u64,
}

pub struct FragmentEndpoint {
    endpoint: Endpoint,
    endpoint_v6: Option<Endpoint>,
    /// Set if the IPv6 listener stops yielding. Without it, a closed endpoint
    /// returns `None` on every poll and the select below would spin.
    v6_closed: std::sync::atomic::AtomicBool,
}

impl FragmentEndpoint {
    pub async fn new(config: EndpointConfig) -> Result<Self, String> {
        let max_streams = u32::try_from(config.max_concurrent_streams)
            .map_err(|_| "max_concurrent_streams exceeds u32::MAX")?;

        let mut transport_config = TransportConfig::default();
        transport_config
            .max_concurrent_bidi_streams(VarInt::from(max_streams))
            .max_concurrent_uni_streams(VarInt::from(max_streams))
            .keep_alive_interval(Some(Duration::from_secs(10)))
            .max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(30_000))));

        let tls_config = crate::tls::server_config(&config.identity)
            .map_err(|e| format!("Failed to build TLS config: {}", e))?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
                .map_err(|e| format!("Failed to build QUIC server config: {}", e))?,
        ));
        server_config.transport_config(Arc::new(transport_config));

        let endpoint = Endpoint::server(server_config.clone(), config.bind_addr)
            .map_err(|e| format!("Failed to bind QUIC endpoint: {}", e))?;

        // A failure here is not fatal: a host with no IPv6 stack, or one where
        // something already holds the port, should still serve IPv4 rather
        // than refuse to start.
        let endpoint_v6 = match config.bind_addr_v6 {
            Some(addr) => match Self::bind_v6_only(server_config, addr) {
                Ok(ep) => {
                    info!(addr = %addr, "QUIC IPv6 listener bound");
                    Some(ep)
                }
                Err(e) => {
                    warn!(addr = %addr, error = %e, "IPv6 listener unavailable — IPv4 only");
                    None
                }
            },
            None => None,
        };

        info!(
            addr = %config.bind_addr,
            v6 = ?config.bind_addr_v6.filter(|_| endpoint_v6.is_some()),
            max_streams = config.max_concurrent_streams,
            "QUIC FragmentEndpoint initialized"
        );

        Ok(Self {
            endpoint,
            endpoint_v6,
            v6_closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Bind an IPv6 listener with `IPV6_V6ONLY` forced on — `Endpoint::server()`
    /// uses OS defaults, which vary the address family by platform.
    fn bind_v6_only(server_config: ServerConfig, addr: SocketAddr) -> Result<Endpoint, String> {
        use socket2::{Domain, Protocol, Socket, Type};

        let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| format!("socket: {e}"))?;
        sock.set_only_v6(true).map_err(|e| format!("v6only: {e}"))?;
        sock.set_nonblocking(true)
            .map_err(|e| format!("nonblocking: {e}"))?;
        sock.bind(&addr.into()).map_err(|e| format!("bind: {e}"))?;

        let runtime = quinn::default_runtime().ok_or("no async runtime for quinn")?;
        Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config),
            sock.into(),
            runtime,
        )
        .map_err(|e| format!("endpoint: {e}"))
    }

    /// The IPv4 listener, for dialing IPv4 peers from the port we listen on.
    /// See `MeshClient::from_endpoint` for why outbound must share it.
    pub fn endpoint_v4(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// The IPv6 listener, when one bound. Dialing an IPv6 peer has to go out
    /// of this one — the v4 socket cannot reach an IPv6 address, and the two
    /// are deliberately kept separate (see `EndpointConfig::bind_addr_v6`).
    pub fn endpoint_v6(&self) -> Option<Endpoint> {
        self.endpoint_v6.clone()
    }

    /// Accept from either listener. `None` means IPv4 is gone (shutdown);
    /// a lost IPv6 endpoint is latched off instead, so IPv4 keeps serving.
    pub async fn accept(&self) -> Option<Incoming> {
        use std::sync::atomic::Ordering;

        let Some(v6) = self.endpoint_v6.as_ref() else {
            return self.endpoint.accept().await;
        };
        if self.v6_closed.load(Ordering::Relaxed) {
            return self.endpoint.accept().await;
        }

        tokio::select! {
            incoming = self.endpoint.accept() => incoming,
            incoming = v6.accept() => match incoming {
                Some(i) => Some(i),
                None => {
                    warn!("IPv6 listener closed — continuing on IPv4 only");
                    self.v6_closed.store(true, Ordering::Relaxed);
                    self.endpoint.accept().await
                }
            },
        }
    }

    pub async fn shutdown(self) {
        info!("Shutting down QUIC endpoints");
        self.endpoint.close(0u32.into(), b"shutdown");
        // Closing only IPv4 left IPv6 peers to discover the shutdown by
        // timeout instead of a CONNECTION_CLOSE frame.
        if let Some(v6) = &self.endpoint_v6 {
            v6.close(0u32.into(), b"shutdown");
        }
    }
}
