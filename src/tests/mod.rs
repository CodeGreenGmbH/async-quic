use crate::QuicEndpoint;
use async_io::{Async, Timer};
use futures::prelude::*;
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, ClientConfig, PrivateKey, RootCertStore, ServerConfig};
use smol::{block_on, spawn, Task};
use std::{
    net::{Ipv6Addr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};
use test_log::test;

lazy_static::lazy_static! {
    static ref CERT_KEY: (Certificate, PrivateKey) = {
        let cert = generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        (Certificate(cert.serialize_der().unwrap()),PrivateKey(cert.serialize_private_key_der()))
    };
    static ref SERVER_CONFIG: ServerConfig = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(vec![CERT_KEY.0.clone()],CERT_KEY.1.clone())
            .unwrap();
    static ref CLIENT_CONFIG: ClientConfig = {
        let mut cert_store = RootCertStore::empty();
        cert_store.add(&CERT_KEY.0).unwrap();
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(cert_store)
            .with_no_client_auth()
    };
}

pub(crate) fn server() -> (QuicEndpoint, u16) {
    let config = Arc::new(quinn_proto::ServerConfig::with_crypto(Arc::new(
        SERVER_CONFIG.clone(),
    )));
    block_on(async {
        let server_udp = Async::<UdpSocket>::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
        let port = server_udp.get_ref().local_addr().unwrap().port();
        (QuicEndpoint::new(server_udp, Some(config)), port)
    })
}

pub(crate) fn quinn_client() -> quinn::Endpoint {
    let mut endpoint = quinn::Endpoint::client((Ipv6Addr::UNSPECIFIED, 0).into()).unwrap();
    let config = quinn::ClientConfig::new(Arc::new(CLIENT_CONFIG.clone()));
    endpoint.set_default_client_config(config);
    endpoint
}

pub struct Tasks {
    tasks: Arc<Mutex<Vec<Task<()>>>>,
}

impl Tasks {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn push_fn(&self) -> impl Fn(Task<()>) + Clone + Send {
        let weak = Arc::downgrade(&self.tasks);
        move |t| weak.upgrade().unwrap().lock().unwrap().push(t)
    }
}

pub async fn sleep() {
    Timer::after(Duration::from_millis(10)).await;
}

#[test]
fn echo_server() {
    let tasks = Tasks::new();
    let push_task0 = tasks.push_fn();

    let (mut server, port) = server();
    let client = quinn_client();

    let push_task1 = push_task0.clone();
    push_task0(spawn(async move {
        while let Some(mut conn) = server.next().await {
            let push_task2 = push_task1.clone();
            push_task1(spawn(async move {
                while let Some(stream) = conn.next().await {
                    push_task2(spawn(async move {
                        let mut stream = stream.stream_rw().unwrap();
                        let mut msg = Vec::new();
                        stream.read_to_end(&mut msg).await.unwrap();
                        stream.write_all(&msg).await.unwrap();
                        stream.close().await.unwrap();
                    }));
                }
            }));
        }
    }));

    let test_msg = b"abcdefghijklmnop";
    block_on(async move {
        let conn = client
            .connect((Ipv6Addr::LOCALHOST, port).into(), "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap();
        sleep().await;

        let m = test_msg.len() / 2;
        send.write_all(&test_msg[0..m]).await.unwrap();
        send.flush().await.unwrap();
        sleep().await;

        send.write_all(&test_msg[m..]).await.unwrap();
        send.finish().await.unwrap();

        sleep().await;
        let msg = recv.read_to_end(1024).await.unwrap();
        assert_eq!(msg, test_msg)
    });

    drop(tasks)
}
