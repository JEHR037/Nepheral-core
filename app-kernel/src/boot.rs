// SPDX-License-Identifier: BUSL-1.1

use std::net::SocketAddr;

use mesh_network::quic_transport::{EndpointConfig, FragmentEndpoint};
use mesh_network::tls::TlsIdentity;

use crate::nepheral_config::FALLBACK_MESH_PORT;

pub struct BoundMesh {
    pub endpoint: FragmentEndpoint,
    pub listen_addr: SocketAddr,
}

pub fn select_network() -> core_crypto::Network {
    let wanted = crate::nepheral_config::configured_network();
    let active = core_crypto::network::set_active(wanted);
    tracing::info!(network = active.name(), "Network selected");
    active
}

pub fn tls_identity(identity: &core_crypto::NodeIdentity) -> TlsIdentity {
    TlsIdentity {
        cert_der: identity.cert_der.clone(),
        key_der: identity.key_der().to_vec(),
    }
}

async fn try_bind(port: u16, identity: &TlsIdentity) -> Result<FragmentEndpoint, String> {
    FragmentEndpoint::new(EndpointConfig {
        bind_addr: format!("0.0.0.0:{port}")
            .parse()
            .map_err(|e| format!("bind addr: {e}"))?,
        bind_addr_v6: format!("[::]:{port}").parse().ok(),
        identity: identity.clone(),
        max_concurrent_streams: 1000,
    })
    .await
}

pub async fn bind_mesh(port: u16, identity: &TlsIdentity) -> Result<BoundMesh, String> {
    match try_bind(port, identity).await {
        Ok(endpoint) => Ok(BoundMesh {
            endpoint,
            listen_addr: SocketAddr::from(([0, 0, 0, 0], port)),
        }),
        Err(first) if port != FALLBACK_MESH_PORT => {
            tracing::warn!(
                port,
                fallback = FALLBACK_MESH_PORT,
                error = %first,
                "Could not bind the mesh port — falling back"
            );
            let endpoint = try_bind(FALLBACK_MESH_PORT, identity).await.map_err(|e| {
                format!("could not bind port {port} ({first}) nor {FALLBACK_MESH_PORT} ({e})")
            })?;
            Ok(BoundMesh {
                endpoint,
                listen_addr: SocketAddr::from(([0, 0, 0, 0], FALLBACK_MESH_PORT)),
            })
        }
        Err(e) => Err(e),
    }
}
