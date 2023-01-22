use async_net::UdpSocket;
use async_quic::QuicEndpoint;
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, PrivateKey};
use smol::block_on;
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

    block_on(async {
        let client_udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await.unwrap();
        let server_udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await.unwrap();

        let client_endpoint = QuicEndpoint::new(client_udp, None);
        let server_endpoint = QuicEndpoint::new(server_udp, Some(server_config));
    });
}
