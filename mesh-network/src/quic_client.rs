// SPDX-License-Identifier: BUSL-1.1
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use core_crypto::identity::PeerId;
use quinn::{Connection, Endpoint, IdleTimeout, TransportConfig, VarInt};
use tracing::{info, instrument};

use crate::tls::{client_config, peer_id_of, TlsIdentity};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct MeshClient {
    endpoint: Endpoint,
    identity: TlsIdentity,
}

fn transport() -> Arc<TransportConfig> {
    let mut transport_config = TransportConfig::default();
    transport_config
        .max_concurrent_bidi_streams(VarInt::from(100u32))
        .max_concurrent_uni_streams(VarInt::from(100u32))
        .keep_alive_interval(Some(Duration::from_secs(10)))
        .max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(30_000))));
    Arc::new(transport_config)
}

impl MeshClient {
    pub fn from_endpoint(endpoint: Endpoint, identity: TlsIdentity) -> Self {
        Self { endpoint, identity }
    }

    pub fn ephemeral(target: SocketAddr, identity: TlsIdentity) -> Result<Self, String> {
        let bind: SocketAddr = if target.is_ipv6() {
            "[::]:0".parse().map_err(|e| format!("bind addr: {e}"))?
        } else {
            "0.0.0.0:0".parse().map_err(|e| format!("bind addr: {e}"))?
        };
        let endpoint =
            Endpoint::client(bind).map_err(|e| format!("Failed to create endpoint: {e}"))?;
        Ok(Self { endpoint, identity })
    }

    fn quinn_config(&self, expected: Option<PeerId>) -> Result<quinn::ClientConfig, String> {
        let tls = client_config(&self.identity, expected)
            .map_err(|e| format!("Failed to build client TLS: {e}"))?;
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| format!("Failed to build QUIC client config: {e}"))?;
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        config.transport_config(transport());
        Ok(config)
    }

    #[instrument(skip(self))]
    pub async fn connect_to_peer(
        &self,
        addr: SocketAddr,
        expected: Option<PeerId>,
    ) -> Result<(Connection, PeerId), String> {
        let config = self.quinn_config(expected)?;
        let connecting = self
            .endpoint
            .connect_with(config, addr, "nepheral-mesh-node")
            .map_err(|e| format!("Failed to initiate connection: {e}"))?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| format!("timed out connecting to {addr}"))?
            .map_err(|e| format!("Connection failed: {e}"))?;
        let pid = peer_id_of(&conn).ok_or_else(|| {
            conn.close(0u32.into(), b"no identity");
            "peer presented no usable identity".to_string()
        })?;
        info!(peer = %addr, peer_fp = %hex::encode(&pid[..8]), "Connected to authenticated mesh peer");
        Ok((conn, pid))
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}
