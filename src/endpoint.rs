use async_io::Async;
use bytes::BytesMut;
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    prelude::*,
    ready,
    stream::FusedStream,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, IoSliceMut},
    mem::MaybeUninit,
    net::{IpAddr, SocketAddr, UdpSocket},
    pin::Pin,
    process::abort,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

use crate::{ConnectionInner, QuicConnecting};

pub struct QuicEndpoint {
    inner: Arc<EndpointInner>,
}

impl QuicEndpoint {
    pub fn new(
        udp: UdpSocket,
        server_config: Option<Arc<rustls::ServerConfig>>,
    ) -> io::Result<Self> {
        quinn_udp::UdpSocketState::configure((&udp).into())?;
        let config = server_config.map(|c| Arc::new(quinn_proto::ServerConfig::with_crypto(c)));
        let endpoint = quinn_proto::Endpoint::new(Arc::new(Default::default()), config);
        let udp_state = Arc::new(quinn_udp::UdpState::new());
        let recv_buf = vec![
            0u8;
            endpoint.config().get_max_udp_payload_size().min(64 * 1024) as usize
                * udp_state.gro_segments()
                * quinn_udp::BATCH_SIZE
        ]
        .into_boxed_slice();
        let (transmit_sender, transmit_receiver) = channel(quinn_udp::BATCH_SIZE);
        let (event_sender, event_receiver) = channel(quinn_udp::BATCH_SIZE);
        let state = Mutex::new(EndpointState {
            connections: BTreeMap::new(),
            endpoint,
            udp: (Async::new(udp)?, quinn_udp::UdpSocketState::new(), recv_buf),
            recv_buffer: VecDeque::new(),
            transmit_buffer: VecDeque::with_capacity(quinn_udp::BATCH_SIZE),
            transmit_receiver,
            event_receiver,
            reject_new_connections: false,
        });
        let inner = Arc::new(EndpointInner {
            state,
            transmit_sender,
            udp_state,
            event_sender,
        });
        Ok(Self { inner })
    }
    pub fn connect(
        &self,
        config: Arc<rustls::ClientConfig>,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<QuicConnecting, quinn_proto::ConnectError> {
        let mut state = self.inner.state.lock().unwrap();
        let config = quinn_proto::ClientConfig::new(config);
        let (handle, conn) = state.endpoint.connect(config, addr, server_name)?;
        let inner = ConnectionInner::new(handle, conn, &self.inner);
        state.connections.insert(handle, inner.clone());
        let inner = Some(inner);
        return Ok(QuicConnecting { inner });
    }
    pub fn reject_new_connections(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.reject_new_connections = true;
        state.endpoint.reject_new_connections();
    }
}

impl Stream for QuicEndpoint {
    type Item = QuicConnecting;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        log::trace!("start poll_next");
        let mut state = self.inner.state.lock().unwrap();
        loop {
            state.poll_transmit(&self.inner.udp_state, cx);
            while let Some(msg) = state.recv_buffer.pop_front() {
                match state
                    .endpoint
                    .handle(Instant::now(), msg.0, msg.1, msg.2, msg.3)
                {
                    Some((handle, quinn_proto::DatagramEvent::ConnectionEvent(event))) => {
                        log::trace!("incoming datagram for connection {:?}", handle);
                        state.send_event(handle, event);
                    }
                    Some((handle, quinn_proto::DatagramEvent::NewConnection(conn))) => {
                        log::trace!("incoming connection: {:?}", handle);
                        let inner = ConnectionInner::new(handle, conn, &self.inner);
                        state.connections.insert(handle, inner.clone());
                        let inner = Some(inner);
                        return Poll::Ready(Some(QuicConnecting { inner }));
                    }
                    None => {}
                }
            }
            while let Poll::Ready(Some((handle, event))) = state.event_receiver.poll_next_unpin(cx)
            {
                log::trace!("event from connection {:?}: {:?}", handle, event);
                if event.is_drained() {
                    state.connections.remove(&handle);
                }
                if let Some(event) = state.endpoint.handle_event(handle, event) {
                    state.send_event(handle, event);
                }
            }
            if state.reject_new_connections && state.connections.is_empty() {
                return Poll::Ready(None);
            }
            match state.fill_recv_buffer(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(err)) => log::error!("endpoint receive error: {:?}", err),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl FusedStream for QuicEndpoint {
    fn is_terminated(&self) -> bool {
        let state = self.inner.state.lock().unwrap();
        state.reject_new_connections && state.connections.is_empty()
    }
}

pub(crate) struct EndpointInner {
    state: Mutex<EndpointState>,
    pub(crate) transmit_sender: Sender<quinn_proto::Transmit>,
    pub(crate) event_sender: Sender<(quinn_proto::ConnectionHandle, quinn_proto::EndpointEvent)>,
    pub(crate) udp_state: Arc<quinn_udp::UdpState>,
}

struct EndpointState {
    connections: BTreeMap<quinn_proto::ConnectionHandle, Arc<ConnectionInner>>,
    udp: (Async<UdpSocket>, quinn_udp::UdpSocketState, Box<[u8]>),
    transmit_receiver: Receiver<quinn_proto::Transmit>,
    event_receiver: Receiver<(quinn_proto::ConnectionHandle, quinn_proto::EndpointEvent)>,
    transmit_buffer: VecDeque<quinn_proto::Transmit>,
    recv_buffer: VecDeque<(
        SocketAddr,
        Option<IpAddr>,
        Option<quinn_proto::EcnCodepoint>,
        BytesMut,
    )>,
    reject_new_connections: bool,
    endpoint: quinn_proto::Endpoint,
}

impl EndpointState {
    pub fn send_event(
        &mut self,
        handle: quinn_proto::ConnectionHandle,
        event: quinn_proto::ConnectionEvent,
    ) {
        self.connections
            .get_mut(&handle)
            .unwrap()
            .event_sender
            .clone()
            .try_send(event)
            .unwrap();
    }
    fn poll_transmit(&mut self, udp_state: &quinn_udp::UdpState, cx: &mut Context) {
        for _ in 0..3 {
            while self.transmit_buffer.len() < self.transmit_buffer.capacity() {
                match self.endpoint.poll_transmit() {
                    Some(t) => self.transmit_buffer.push_back(t),
                    None => break,
                }
            }
            while self.transmit_buffer.len() < self.transmit_buffer.capacity() {
                match self.transmit_receiver.poll_next_unpin(cx) {
                    Poll::Ready(Some(t)) => self.transmit_buffer.push_back(t),
                    Poll::Ready(None) => unreachable!(),
                    Poll::Pending => break,
                }
            }
            if self.transmit_buffer.is_empty() {
                break;
            }
            match poll_send(
                &mut self.udp.1,
                udp_state,
                &self.udp.0,
                cx,
                self.transmit_buffer.make_contiguous(),
            ) {
                Poll::Ready(Ok(n)) => {
                    log::trace!("sent {} transmits", n);
                    drop(self.transmit_buffer.drain(0..n))
                }
                Poll::Ready(Err(err)) => log::error!("endpoint send error: {:?}", err),
                Poll::Pending => break,
            }
        }
    }

    fn fill_recv_buffer<'a>(&'a mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        loop {
            let mut metas = [quinn_udp::RecvMeta::default(); quinn_udp::BATCH_SIZE];
            let mut iovs = MaybeUninit::<[IoSliceMut<'a>; quinn_udp::BATCH_SIZE]>::uninit();
            self.udp
                .2
                .chunks_mut(self.udp.2.len() / quinn_udp::BATCH_SIZE)
                .enumerate()
                .for_each(|(i, buf)| unsafe {
                    iovs.as_mut_ptr()
                        .cast::<IoSliceMut>()
                        .add(i)
                        .write(IoSliceMut::<'a>::new(buf));
                });
            let mut iovs = unsafe { iovs.assume_init() };
            match poll_recv(&self.udp.1, &self.udp.0, cx, &mut iovs, &mut metas) {
                Poll::Ready(Ok(n)) => {
                    for (meta, buf) in metas.iter().zip(iovs.iter()).take(n) {
                        let mut b: BytesMut = buf[0..meta.len].into();
                        while !b.is_empty() {
                            let b = b.split_to(meta.stride.min(b.len()));
                            self.recv_buffer
                                .push_back((meta.addr, meta.dst_ip, meta.ecn, b));
                        }
                    }
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(err)) => {
                    if err.kind() != io::ErrorKind::ConnectionReset {
                        return Poll::Ready(Err(err));
                    }
                    dbg!(err);
                    abort();
                }
            }
        }
    }
}

fn poll_send(
    uss: &mut quinn_udp::UdpSocketState,
    us: &quinn_udp::UdpState,
    io: &Async<UdpSocket>,
    cx: &mut Context,
    t: &[quinn_proto::Transmit],
) -> Poll<io::Result<usize>> {
    loop {
        ready!(io.poll_writable(cx))?;
        if let Ok(n) = uss.send(io.into(), us, t) {
            return Poll::Ready(Ok(n));
        }
    }
}
fn poll_recv(
    uss: &quinn_udp::UdpSocketState,
    io: &Async<UdpSocket>,
    cx: &mut Context,
    b: &mut [IoSliceMut<'_>],
    m: &mut [quinn_udp::RecvMeta],
) -> Poll<io::Result<usize>> {
    loop {
        ready!(io.poll_readable(cx))?;
        if let Ok(res) = uss.recv(io.into(), b, m) {
            return Poll::Ready(Ok(res));
        }
    }
}
