mod http3;
mod quic;

use crate::{QuicConnection, QuicEndpoint};
use futures::{future::FusedFuture, prelude::*, select, stream::FuturesUnordered};
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, ClientConfig, PrivateKey, RootCertStore, ServerConfig};
use std::{
    net::{Ipv6Addr, UdpSocket},
    ops::ControlFlow,
    sync::Arc,
};

lazy_static::lazy_static! {
    pub static ref CERT_KEY: (Certificate, PrivateKey) = {
        let cert = generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        (Certificate(cert.serialize_der().unwrap()),PrivateKey(cert.serialize_private_key_der()))
    };
    pub static ref RUSTLS_SERVER_CONFIG: ServerConfig = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(vec![CERT_KEY.0.clone()],CERT_KEY.1.clone())
            .unwrap();
    pub static ref RUSTLS_CLIENT_CONFIG: ClientConfig = {
        let mut cert_store = RootCertStore::empty();
        cert_store.add(&CERT_KEY.0).unwrap();
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(cert_store)
            .with_no_client_auth()
    };
//    pub static ref QUIC_CLIENT_CONFIG: quinn_proto::ClientConfig = quinn_proto::ClientConfig::new(Arc::new(RUSTLS_CLIENT_CONFIG));
}

pub(crate) fn server() -> (QuicEndpoint, u16) {
    let config = Arc::new(RUSTLS_SERVER_CONFIG.clone());
    let udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
    let port = udp.local_addr().unwrap().port();
    let endpoint = QuicEndpoint::new(udp, Some(config)).unwrap();
    (endpoint, port)
}

//pub(crate) fn quinn_client() -> quinn::Endpoint {
//    let mut endpoint = quinn::Endpoint::client((Ipv6Addr::UNSPECIFIED, 0).into()).unwrap();
//    let config = quinn::ClientConfig::new(Arc::new(RUSTLS_CLIENT_CONFIG.clone()));
//    endpoint.set_default_client_config(config);
//    endpoint
//}

pub(crate) fn client() -> QuicEndpoint {
    let udp = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
    let endpoint = QuicEndpoint::new(udp, None).unwrap();
    endpoint
}

pub async fn connect<F, T, R>(mut ep: QuicEndpoint, port: u16, f: F) -> R
where
    F: FnOnce(QuicConnection) -> T,
    T: Future<Output = R> + Unpin + FusedFuture,
{
    let mut connecting = ep
        .connect(
            Arc::new(RUSTLS_CLIENT_CONFIG.clone()),
            (Ipv6Addr::LOCALHOST, port).into(),
            "localhost",
        )
        .unwrap();
    let connection = select! {
        _ = ep.next() => panic!("unexpeced endpoint item"),
        c = connecting => c,
    };
    let mut driver_fut = connection.driver();
    let mut return_fut = f(connection);
    let (mut driver_out, mut return_out) = (None, None);
    let (driver_out, return_out) = loop {
        select! {
            _ = ep.next() => panic!("unexpeced endpoint item"),
            d = driver_fut => driver_out = Some(d),
            r = return_fut => return_out = Some(r),
        };
        if driver_out.is_some() && return_out.is_some() {
            break (driver_out.unwrap(), return_out.unwrap());
        }
    };
    driver_out.unwrap();
    return_out
}

pub async fn handle<F, T, R>(mut ep: QuicEndpoint, f: F) -> R
where
    F: Fn(QuicConnection) -> T,
    T: Future<Output = ControlFlow<R>>,
{
    let mut connecting = FuturesUnordered::new();
    let mut handling = FuturesUnordered::new();
    let mut driving = FuturesUnordered::new();
    let mut first_break = None;
    loop {
        select! {
            c = ep.next() => if let Some(c) = c {
                connecting.push(c);
            },
            c = connecting.next() => if let Some(c) = c {
                driving.push(c.driver());
                handling.push(f(c));
            },
            r = driving.next() => if let Some(r) = r {
                r.unwrap();
            },
            r = handling.next() => if let Some(ControlFlow::Break(b)) = r {
                ep.reject_new_connections();
                first_break.get_or_insert(b);
            },
            complete => break,
        };
    }
    first_break.unwrap()
}
