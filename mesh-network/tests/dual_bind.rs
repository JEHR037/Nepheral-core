// SPDX-License-Identifier: BUSL-1.1

use std::time::Duration;

use mesh_network::quic_client::MeshClient;
use mesh_network::quic_transport::{EndpointConfig, FragmentEndpoint};
use mesh_network::tls::{peer_id_of, TlsIdentity};

fn identity() -> (TlsIdentity, [u8; 32]) {
    let words = core_crypto::NodeMnemonic::generate();
    let id = core_crypto::NodeIdentity::from_mnemonic(&words).expect("identity");
    (
        TlsIdentity {
            cert_der: id.cert_der.clone(),
            key_der: id.key_der().to_vec(),
        },
        id.peer_id,
    )
}

fn ipv6_available() -> bool {
    std::net::UdpSocket::bind("[::1]:0").is_ok()
}

#[tokio::test]
async fn dual_listener_binds_both_families() {
    let (tls, _) = identity();
    let cfg = EndpointConfig {
        bind_addr: "127.0.0.1:45871".parse().unwrap(),
        bind_addr_v6: Some("[::1]:45871".parse().unwrap()),
        identity: tls,
        max_concurrent_streams: 16,
    };
    let endpoint = FragmentEndpoint::new(cfg).await;
    assert!(endpoint.is_ok(), "dual bind failed: {:?}", endpoint.err());
}

#[tokio::test]
async fn missing_v6_is_not_fatal() {
    let (tls, _) = identity();
    let cfg = EndpointConfig {
        bind_addr: "127.0.0.1:45872".parse().unwrap(),
        bind_addr_v6: None,
        identity: tls,
        max_concurrent_streams: 16,
    };
    assert!(FragmentEndpoint::new(cfg).await.is_ok());
}

#[tokio::test]
async fn both_ends_learn_the_authenticated_peer_id() {
    let (server_tls, server_pid) = identity();
    let (client_tls, client_pid) = identity();
    let server = FragmentEndpoint::new(EndpointConfig {
        bind_addr: "127.0.0.1:45880".parse().unwrap(),
        bind_addr_v6: if ipv6_available() {
            Some("[::1]:45880".parse().unwrap())
        } else {
            None
        },
        identity: server_tls,
        max_concurrent_streams: 16,
    })
    .await
    .expect("bind");

    let mut targets = vec!["127.0.0.1:45880"];
    if ipv6_available() {
        targets.push("[::1]:45880");
    }
    for target in targets {
        let addr: std::net::SocketAddr = target.parse().unwrap();
        let client = MeshClient::ephemeral(addr, client_tls.clone()).expect("client");
        let dial =
            tokio::spawn(async move { client.connect_to_peer(addr, Some(server_pid)).await });

        let incoming = tokio::time::timeout(Duration::from_secs(5), server.accept())
            .await
            .unwrap_or_else(|_| panic!("no inbound connection on {target}"))
            .unwrap_or_else(|| panic!("listener closed while dialing {target}"));
        let conn = incoming.await.expect("handshake");
        assert_eq!(peer_id_of(&conn), Some(client_pid));
        let (_, seen) = dial.await.unwrap().expect("dial");
        assert_eq!(seen, server_pid);
    }
}

#[tokio::test]
async fn dialing_with_the_wrong_expected_identity_fails() {
    let (server_tls, _) = identity();
    let (client_tls, _) = identity();
    let (_, someone_else) = identity();
    let server = FragmentEndpoint::new(EndpointConfig {
        bind_addr: "127.0.0.1:45881".parse().unwrap(),
        bind_addr_v6: None,
        identity: server_tls,
        max_concurrent_streams: 16,
    })
    .await
    .expect("bind");
    let accept = tokio::spawn(async move {
        if let Some(incoming) = server.accept().await {
            let _ = incoming.await;
        }
    });
    let addr: std::net::SocketAddr = "127.0.0.1:45881".parse().unwrap();
    let client = MeshClient::ephemeral(addr, client_tls).expect("client");
    assert!(client
        .connect_to_peer(addr, Some(someone_else))
        .await
        .is_err());
    accept.abort();
}
