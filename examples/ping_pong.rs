use async_net::UdpSocket;
use async_quic::QuicEndpoint;
use futures::prelude::*;
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, PrivateKey};
use smol::{block_on, spawn};
use std::{net::Ipv6Addr, sync::Arc};

fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();

    let server_config = {
        let server_cert = generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let rustls_server_config = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(
                vec![Certificate(server_cert.serialize_der().unwrap())],
                PrivateKey(server_cert.serialize_private_key_der()),
            )
            .unwrap();
        Arc::new(quinn_proto::ServerConfig::with_crypto(Arc::new(
            rustls_server_config,
        )))
    };

    let client_endpoint = block_on(async {
        let client_udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await.unwrap();
        QuicEndpoint::new(client_udp, None)
    });
    let mut server_endpoint = block_on(async {
        let server_udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await.unwrap();
        QuicEndpoint::new(server_udp, Some(server_config))
    });

    spawn(async move {
        while let Some(conn) = server_endpoint.next().await {
            spawn(async move { while let Some(event) = conn.next().await {} }).detach()
        }
    })
    .detach();
}
