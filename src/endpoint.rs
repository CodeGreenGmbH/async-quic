use async_net::UdpSocket;
use bytes::BytesMut;
use futures::prelude::*;
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

use crate::{ConnectionInner, QuicConnection};

pub struct QuicEndpoint {
    inner: Arc<EndpointInner>,
}

impl QuicEndpoint {
    pub fn new(udp: UdpSocket, server_config: Option<Arc<quinn_proto::ServerConfig>>) -> Self {
        let endpoint = quinn_proto::Endpoint::new(Arc::new(Default::default()), server_config);
        let state = Mutex::new(EndpointState {
            connections: BTreeMap::new(),
            endpoint,
        });
        let inner = Arc::new(EndpointInner { udp, state });
        Self { inner }
    }
}

impl Stream for QuicEndpoint {
    type Item = QuicConnection;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            while let Some(transmit) = state.endpoint.poll_transmit() {
                self.inner.transmit(transmit);
            }
            let mut b = BytesMut::zeroed(1600);
            let (n, addr) = match Box::pin(self.inner.udp.recv_from(&mut b)).poll_unpin(cx) {
                Poll::Ready(Ok((n, addr))) => (n, addr),
                Poll::Ready(Err(err)) => {
                    log::error!("udp receive failed: {:?}", err);
                    continue;
                }
                Poll::Pending => return Poll::Pending,
            };
            b.truncate(n);
            match state.endpoint.handle(Instant::now(), addr, None, None, b) {
                Some((handle, quinn_proto::DatagramEvent::ConnectionEvent(event))) => {
                    match state.connections.get(&handle) {
                        Some(conn) => conn.handle_event(event),
                        None => log::error!("connection not found"),
                    }
                }
                Some((handle, quinn_proto::DatagramEvent::NewConnection(conn))) => {
                    let conn = QuicConnection::new(handle, conn, self.inner.clone());
                    state.connections.insert(handle, conn.inner());
                    return Poll::Ready(Some(conn));
                }
                None => {}
            }
        }
    }
}

pub(crate) struct EndpointInner {
    udp: UdpSocket,
    state: Mutex<EndpointState>,
}

impl EndpointInner {
    pub(crate) fn transmit(&self, transmit: quinn_proto::Transmit) {
        let future = self.udp.send_to(&transmit.contents, transmit.destination);
        let waker = noop_waker::noop_waker();
        let mut ctx = Context::from_waker(&waker);
        match Box::pin(future).poll_unpin(&mut ctx) {
            Poll::Ready(Ok(_)) => todo!(),
            Poll::Ready(Err(err)) => log::error!("transmit error: {:?}", err),
            Poll::Pending => log::error!("transmit queue full"),
        }
    }
    pub(crate) fn handle_enpoint_event(
        &self,
        handle: quinn_proto::ConnectionHandle,
        event: quinn_proto::EndpointEvent,
    ) -> Option<quinn_proto::ConnectionEvent> {
        self.state
            .lock()
            .unwrap()
            .endpoint
            .handle_event(handle, event)
    }
}

struct EndpointState {
    connections: BTreeMap<quinn_proto::ConnectionHandle, Arc<ConnectionInner>>,
    endpoint: quinn_proto::Endpoint,
}
