use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Instant,
};

use crate::{error::Infallible, EndpointInner, QuicConnectionDriver, QuicStream};
use async_io::Timer;
use bytes::Bytes;
use futures::{channel::mpsc::Sender, prelude::*, ready};

pub struct QuicConnection {
    pub(crate) inner: Arc<ConnectionInner>,
}

impl QuicConnection {
    pub(crate) fn inner(&self) -> Arc<ConnectionInner> {
        self.inner.clone()
    }
    pub fn driver(&self) -> QuicConnectionDriver {
        QuicConnectionDriver(self.inner.clone())
    }
    pub fn error(&self) -> Option<quinn_proto::ConnectionError> {
        self.inner.state.lock().unwrap().error.clone()
    }
}

impl h3::quic::Connection<Bytes> for QuicConnection {
    type OpenStreams = Self;

    fn poll_accept_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        todo!()
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        todo!()
    }

    fn opener(&self) -> Self::OpenStreams {
        let inner = self.inner();
        QuicConnection { inner }
    }
}

impl h3::quic::OpenStreams<Bytes> for QuicConnection {
    type BidiStream = QuicStream<true, true>;
    type SendStream = QuicStream<false, true>;
    type RecvStream = QuicStream<true, false>;
    type Error = Infallible;

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        todo!()
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        let mut state = self.inner.state.lock().unwrap();

        if state.conn.is_handshaking() {
            panic!("adfdf")
        }
        // TODO: return error
        let id = state.conn.streams().open(quinn_proto::Dir::Uni).unwrap();
        Poll::Ready(Ok(QuicStream::new(self.inner.clone(), id)))
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        todo!()
    }
}

pub(crate) struct ConnectionInner {
    state: Mutex<ConnectionState>,
    endpoint: Arc<EndpointInner>,
    handle: quinn_proto::ConnectionHandle,
}

impl ConnectionInner {
    pub(crate) fn new(
        handle: quinn_proto::ConnectionHandle,
        conn: quinn_proto::Connection,
        endpoint: Arc<EndpointInner>,
        transmit_sender: Sender<quinn_proto::Transmit>,
    ) -> Arc<Self> {
        let state = Mutex::new(ConnectionState {
            conn,
            timer: None,
            stream_wakers: BTreeMap::new(),
            drive_waker: None,
            transmit_sender,
            queued_transmit: None,
            error: None,
        });
        Arc::new(Self {
            state,
            endpoint,
            handle,
        })
    }
    pub(crate) fn is_handshaking(&self) -> bool {
        self.state.lock().unwrap().conn.is_handshaking()
    }
    pub(crate) fn handle_event(&self, event: quinn_proto::ConnectionEvent) {
        let mut guard = self.state.lock().unwrap();
        guard.conn.handle_event(event);
        if let Some(waker) = guard.drive_waker.take() {
            waker.wake()
        }
    }
    pub(crate) fn poll_drive(self: &Arc<ConnectionInner>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.state.lock().unwrap();
        state.drive_waker = Some(cx.waker().clone());
        state.poll_drive(cx, &self.endpoint, self.handle)
    }
    fn poll(self: &Arc<ConnectionInner>, cx: &mut Context<'_>) -> Poll<QuicConnectionEvent> {
        let mut guard = self.state.lock().unwrap();
        guard.drive_waker = None;
        let mgs = self.endpoint.udp_state().max_gso_segments();
        while let Some(t) = guard.conn.poll_transmit(Instant::now(), mgs) {
            if let Poll::Ready(Ok(())) = guard.transmit_sender.poll_ready(cx) {
                guard.transmit_sender.start_send(t).unwrap()
            }
        }
        loop {
            guard.timer = guard.conn.poll_timeout().map(Timer::at);
            if let Some(timer) = &mut guard.timer {
                match timer.poll_unpin(cx) {
                    Poll::Ready(_) => guard.conn.handle_timeout(Instant::now()),
                    Poll::Pending => break,
                }
            }
        }
        while let Some(event) = guard.conn.poll_endpoint_events() {
            if let Some(event) = self.endpoint.handle_enpoint_event(self.handle, event) {
                guard.conn.handle_event(event);
            }
        }
        while let Some(event) = guard.conn.poll() {
            match event {
                quinn_proto::Event::HandshakeDataReady => log::info!("handshake data ready"),
                quinn_proto::Event::Connected => log::info!("connected"),
                quinn_proto::Event::ConnectionLost { reason } => {
                    log::error!("connection lost: {:?}", reason)
                }
                quinn_proto::Event::DatagramReceived => log::error!("ignoring datagram"),
                quinn_proto::Event::Stream(event) => match event {
                    quinn_proto::StreamEvent::Opened { .. } => {} // ignore, because we check anyway
                    quinn_proto::StreamEvent::Readable { id } => guard.wake(id, true, false),
                    quinn_proto::StreamEvent::Writable { id } => guard.wake(id, false, true),
                    quinn_proto::StreamEvent::Finished { id } => guard.wake(id, false, true),
                    quinn_proto::StreamEvent::Stopped { id, .. } => guard.wake(id, true, false),
                    quinn_proto::StreamEvent::Available { dir } => todo!("available: {}", dir),
                },
            }
        }
        let mut streams = guard.conn.streams();
        if let Some(id) = streams.accept(quinn_proto::Dir::Uni) {
            guard.stream_wakers.insert(id, [None, None]);
            return Poll::Ready(QuicConnectionEvent::StreamR(QuicStream::new(
                self.clone(),
                id,
            )));
        }
        if let Some(id) = streams.accept(quinn_proto::Dir::Bi) {
            guard.stream_wakers.insert(id, [None, None]);
            return Poll::Ready(QuicConnectionEvent::StreamRW(QuicStream::new(
                self.clone(),
                id,
            )));
        }
        guard.drive_waker = Some(cx.waker().clone());
        Poll::Pending
    }
    pub(crate) fn poll_read(
        &self,
        id: quinn_proto::StreamId,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<(usize, Option<quinn_proto::VarInt>)> {
        let mut guard = self.state.lock().unwrap();
        guard.stream_wakers.get_mut(&id).unwrap()[0] = None;
        let mut recv_stream = guard.conn.recv_stream(id);
        let mut chunks = match recv_stream.read(true) {
            Ok(chunks) => chunks,
            Err(_) => return Poll::Ready((0, None)),
        };
        let mut n = 0usize;
        let (blocked, err_code) = loop {
            if buf.len() == n {
                break (false, None);
            }
            match chunks.next(buf.len() - n) {
                Ok(Some(chunk)) => {
                    let m = n + chunk.bytes.len();
                    buf[n..m].copy_from_slice(&chunk.bytes);
                    n = m;
                }
                Ok(None) => break (false, None),
                Err(quinn_proto::ReadError::Blocked) => break (true, None),
                Err(quinn_proto::ReadError::Reset(err)) => break (false, Some(err)),
            }
        };
        if chunks.finalize().should_transmit() {
            if let Some(w) = guard.drive_waker.take() {
                w.wake();
            }
        }
        if n == 0 && blocked {
            guard.stream_wakers.get_mut(&id).unwrap()[0] = Some(cx.waker().clone());
            return Poll::Pending;
        }
        return Poll::Ready((n, err_code));
    }
    pub(crate) fn poll_write(
        &self,
        id: quinn_proto::StreamId,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Option<quinn_proto::VarInt>>> {
        let mut guard = self.state.lock().unwrap();
        guard.stream_wakers.get_mut(&id).unwrap()[1] = None;
        let mut send_stream = guard.conn.send_stream(id);
        match send_stream.write(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(quinn_proto::WriteError::Blocked) => {
                guard.stream_wakers.get_mut(&id).unwrap()[1] = Some(cx.waker().clone());
                Poll::Pending
            }
            Err(quinn_proto::WriteError::Stopped(err_code)) => Poll::Ready(Err(Some(err_code))),
            Err(quinn_proto::WriteError::UnknownStream) => Poll::Ready(Err(None)),
        }
    }
    pub(crate) fn close(
        &self,
        id: quinn_proto::StreamId,
        _cx: &mut Context<'_>,
    ) -> Result<(), Option<quinn_proto::VarInt>> {
        let mut guard = self.state.lock().unwrap();
        guard.stream_wakers.get_mut(&id).unwrap()[1] = None;
        let mut send_stream = guard.conn.send_stream(id);
        match send_stream.finish() {
            Ok(()) => Ok(()),
            Err(quinn_proto::FinishError::Stopped(err_code)) => Err(Some(err_code)),
            Err(quinn_proto::FinishError::UnknownStream) => Err(None),
        }
    }
}

impl Stream for QuicConnection {
    type Item = QuicConnectionEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll(cx).map(Option::Some)
    }
}

struct ConnectionState {
    conn: quinn_proto::Connection,
    timer: Option<Timer>,
    drive_waker: Option<Waker>,
    stream_wakers: BTreeMap<quinn_proto::StreamId, [Option<Waker>; 2]>,
    queued_transmit: Option<quinn_proto::Transmit>,
    transmit_sender: Sender<quinn_proto::Transmit>,
    error: Option<quinn_proto::ConnectionError>,
}

impl ConnectionState {
    fn poll_drive(
        &mut self,
        cx: &mut Context,
        endpoint: &EndpointInner,
        handle: quinn_proto::ConnectionHandle,
    ) -> Poll<()> {
        let mut control_flow = ControlFlow::Continue(());
        while control_flow.is_continue() {
            control_flow = ControlFlow::Break(());
            loop {
                self.conn.handle_timeout(Instant::now());
                ready!(self.transmit(cx, endpoint));
                self.timer = self.conn.poll_timeout().map(Timer::at);
                if let Some(timer) = &mut self.timer {
                    if timer.poll_unpin(cx).is_ready() {
                        continue;
                    }
                }
                break;
            }
            while let Some(event) = self.conn.poll_endpoint_events() {
                if let Some(event) = endpoint.handle_enpoint_event(handle, event) {
                    self.conn.handle_event(event);
                    control_flow = ControlFlow::Continue(());
                }
            }
            while let Some(event) = self.conn.poll() {
                match event {
                    quinn_proto::Event::HandshakeDataReady => {
                        control_flow = ControlFlow::Continue(())
                    }
                    quinn_proto::Event::Connected => control_flow = ControlFlow::Continue(()),
                    quinn_proto::Event::ConnectionLost { reason } => {
                        control_flow = ControlFlow::Continue(());
                        self.error = Some(reason)
                    }
                    quinn_proto::Event::DatagramReceived => log::error!("ignoring datagram"),
                    quinn_proto::Event::Stream(event) => match event {
                        quinn_proto::StreamEvent::Opened { .. } => {} // ignore, because we check anyway
                        quinn_proto::StreamEvent::Readable { id } => self.wake(id, true, false),
                        quinn_proto::StreamEvent::Writable { id } => self.wake(id, false, true),
                        quinn_proto::StreamEvent::Finished { id } => self.wake(id, false, true),
                        quinn_proto::StreamEvent::Stopped { id, .. } => self.wake(id, true, false),
                        quinn_proto::StreamEvent::Available { dir } => todo!("available: {}", dir),
                    },
                }
            }
        }
        Poll::Pending
    }
    fn wake(&mut self, id: quinn_proto::StreamId, r: bool, w: bool) {
        if let Some(wakers) = self.stream_wakers.get_mut(&id) {
            if r {
                if let Some(waker) = wakers[0].take() {
                    waker.wake();
                }
            }
            if w {
                if let Some(waker) = wakers[1].take() {
                    waker.wake();
                }
            }
        }
    }
    fn transmit(&mut self, cx: &mut Context, endpoint: &EndpointInner) -> Poll<()> {
        let mgs = endpoint.udp_state().max_gso_segments();
        if self.queued_transmit.is_none() {
            self.queued_transmit = self.conn.poll_transmit(Instant::now(), mgs)
        }
        while let Some(queued) = self.queued_transmit.take() {
            match self.transmit_sender.try_send(queued) {
                Ok(()) => self.queued_transmit = self.conn.poll_transmit(Instant::now(), mgs),
                Err(err) => {
                    self.queued_transmit = Some(err.into_inner());
                    ready!(self.transmit_sender.poll_ready(cx));
                }
            }
        }
        Poll::Ready(())
    }
}
pub enum QuicConnectionEvent {
    StreamR(QuicStream<true, false>),
    StreamRW(QuicStream<true, true>),
}

impl QuicConnectionEvent {
    pub fn stream_r(self) -> Option<QuicStream<true, false>> {
        match self {
            Self::StreamR(stream) => Some(stream),
            _ => None,
        }
    }
    pub fn stream_rw(self) -> Option<QuicStream<true, true>> {
        match self {
            Self::StreamRW(stream) => Some(stream),
            _ => None,
        }
    }
}
