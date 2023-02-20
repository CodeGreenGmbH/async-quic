use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Instant,
};

use crate::streams::Streams;
use crate::{
    error::{Infallible, QuicSendError},
    streams::StreamHandle,
    EndpointInner, QuicConnectionDriver, QuicStream,
};
use async_io::Timer;
use bytes::Bytes;
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    prelude::*,
    ready,
};

#[derive(Debug)]
pub struct QuicConnection {
    pub(crate) inner: Arc<ConnectionInner>,
}

impl QuicConnection {
    pub(crate) fn inner(&self) -> Arc<ConnectionInner> {
        self.inner.clone()
    }
    pub fn driver(&self) -> QuicConnectionDriver {
        QuicConnectionDriver(self.inner.clone(), false)
    }
}

impl h3::quic::Connection<Bytes> for QuicConnection {
    type OpenStreams = Self;

    fn poll_accept_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        self.inner.poll_accept(cx)
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        self.inner.poll_accept(cx)
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
        self.inner.poll_open(cx)
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        self.inner.poll_open(cx)
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        let mut state = self.inner.state.lock().unwrap();
        log::debug!("{:?}: closed connection", state.conn.side());
        state.conn.close(
            Instant::now(),
            code.value().try_into().unwrap(),
            Bytes::copy_from_slice(reason),
        );
        state.drive_wake();
    }
}

#[derive(Debug)]

pub(crate) struct ConnectionInner {
    state: Mutex<ConnectionState>,
    pub handle: quinn_proto::ConnectionHandle,
    pub event_sender: Sender<quinn_proto::ConnectionEvent>,
}

impl ConnectionInner {
    pub(crate) fn new(
        handle: quinn_proto::ConnectionHandle,
        conn: quinn_proto::Connection,
        endpoint: &EndpointInner,
    ) -> Arc<Self> {
        let (event_sender, event_receiver) = channel(quinn_udp::BATCH_SIZE);
        let state = Mutex::new(ConnectionState {
            conn,
            timer: None,
            streams: Streams::default(),
            drive_waker: None,
            transmit_sender: endpoint.transmit_sender.clone(),
            queued_transmit: None,
            closed: None,
            event_sender: endpoint.event_sender.clone(),
            event_receiver,
            udp_state: endpoint.udp_state.clone(),
            queued_endpoint_event: None,
            opened_waker: [None, None],
            opening_waker: [None, None],
        });
        Arc::new(Self {
            state,
            handle,
            event_sender,
        })
    }
    fn poll_accept<const R: bool, const W: bool>(
        self: &Arc<Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<QuicStream<R, W>>, Infallible>> {
        let dir: quinn_proto::Dir = QuicStream::<R, W>::dir();
        let mut state = self.state.lock().unwrap();
        if let Some(id) = state.conn.streams().accept(dir) {
            log::debug!(
                "{:?}: accepted {:?} stream: {:?}",
                state.conn.side(),
                dir,
                id
            );
            let handle = state.streams.create(id, R, W);
            state.drive_wake();
            return Poll::Ready(Ok(Some(QuicStream::new(self.clone(), handle, id))));
        }
        state.opened_waker[quinn_proto::Dir::Uni as usize] = Some(cx.waker().clone());
        Poll::Pending
    }
    fn poll_open<const R: bool, const W: bool>(
        self: &Arc<Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<QuicStream<R, W>, Infallible>> {
        let dir: quinn_proto::Dir = QuicStream::<R, W>::dir();
        let mut state = self.state.lock().unwrap();
        if let Some(id) = state.conn.streams().open(dir) {
            let handle = state.streams.create(id, R, W);
            log::debug!("{:?}: opened {:?} stream: {:?}", state.conn.side(), dir, id);
            state.drive_wake();
            return Poll::Ready(Ok(QuicStream::new(self.clone(), handle, id)));
        }
        state.opening_waker[dir as usize] = Some(cx.waker().clone());
        Poll::Pending
    }
    pub(crate) fn is_handshaking(&self) -> bool {
        self.state.lock().unwrap().conn.is_handshaking()
    }
    pub(crate) fn poll_drive(
        self: &Arc<ConnectionInner>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<quinn_proto::ApplicationClose>, quinn_proto::ConnectionError>> {
        let mut state = self.state.lock().unwrap();
        state.drive_waker = Some(cx.waker().clone());
        state.poll_drive(cx, self.handle)
    }
    pub(crate) fn poll_recv(
        &self,
        handle: &StreamHandle,
        cx: &mut Context<'_>,
        max_size: usize,
    ) -> Poll<Result<Option<Bytes>, quinn_proto::VarInt>> {
        let mut state = self.state.lock().unwrap();
        let mut stream_state = state.streams.handle(handle);

        let mut stream = state.conn.recv_stream(id);
        let mut chunks = match stream.read(true) {
            Ok(chunks) => chunks,
            Err(_) => return Poll::Ready(Ok(None)),
        };
        let p = match chunks.next(max_size) {
            Ok(chunk) => Poll::Ready(Ok(chunk.map(|c| c.bytes))),
            Err(quinn_proto::ReadError::Blocked) => Poll::Pending,
            Err(quinn_proto::ReadError::Reset(err)) => Poll::Ready(Err(err)),
        };
        if chunks.finalize().should_transmit() {
            state.drive_wake();
        }
        if p.is_pending() {
            state.streams.get_mut(&id).unwrap().recv_waker = Some(cx.waker().clone());
        }
        p
    }
    pub(crate) fn poll_write(
        &self,
        handle: &StreamHandle,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, QuicSendError>> {
        let mut state = self.state.lock().unwrap();
        let mut stream_state = state.streams.handle(handle);
        let id = stream_state.send_id()?;
        let mut send_stream = state.conn.send_stream(id);
        match send_stream.write(buf) {
            Ok(n) => {
                state.drive_wake();
                Poll::Ready(Ok(n))
            }
            Err(quinn_proto::WriteError::Blocked) => {
                stream_state.set_send_wake(cx);
                Poll::Pending
            }
            Err(err) => unreachable!(err),
        }
    }

    pub(crate) fn reset(&self, handle: quinn_proto::StreamId, error_code: quinn_proto::VarInt) {
        let mut state = self.state.lock().unwrap();
        let mut stream_state = state.streams.handle(handle);
        if let Ok(id) = stream_state.send_id() {
            state.conn.send_stream(id).reset(error_code).unwrap();
            state.drive_wake();
        }
    }

    pub(crate) fn poll_finish(
        &self,
        handle: &StreamHandle,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), QuicSendError>> {
        let mut state = self.state.lock().unwrap();
        let mut stream_state = state.streams.handle(handle);
        let id = match stream_state.send_id() {
            Ok(id) => Some(id),
            Err(QuicSendError::Finishing) => None,
            Err(QuicSendError::Finished) => return Poll::Ready(Ok(())),
            err => return Poll::Ready(Err(err)),
        }?;
        if let Some(id) = id {
            stream_state.finished_send();
            state.conn.send_stream(id).finish().unwrap();
        }
        stream_state.set_send_wake(cx);
        Poll::Pending
    }
}

#[derive(Debug)]

struct ConnectionState {
    conn: quinn_proto::Connection,
    timer: Option<Timer>,
    drive_waker: Option<Waker>,
    streams: Streams,
    queued_transmit: Option<quinn_proto::Transmit>,
    transmit_sender: Sender<quinn_proto::Transmit>,
    queued_endpoint_event: Option<quinn_proto::EndpointEvent>,
    event_sender: Sender<(quinn_proto::ConnectionHandle, quinn_proto::EndpointEvent)>,
    event_receiver: Receiver<quinn_proto::ConnectionEvent>,
    closed: Option<quinn_proto::ConnectionError>,
    udp_state: Arc<quinn_udp::UdpState>,
    opened_waker: [Option<Waker>; 2],
    opening_waker: [Option<Waker>; 2],
}

impl ConnectionState {
    fn closed(
        &self,
    ) -> Result<Option<quinn_proto::ApplicationClose>, quinn_proto::ConnectionError> {
        match self.closed.clone() {
            Some(quinn_proto::ConnectionError::ApplicationClosed(ok)) => Ok(Some(ok)),
            Some(err) => Err(err),
            None => Ok(None),
        }
    }
    fn opened_wake(&mut self, dir: quinn_proto::Dir) {
        self.opened_waker[dir as usize].take().map(Waker::wake);
    }
    fn opening_wake(&mut self, dir: quinn_proto::Dir) {
        self.opening_waker[dir as usize].take().map(Waker::wake);
    }
    fn drive_wake(&mut self) {
        self.drive_waker.take().map(Waker::wake);
    }
    fn poll_drive(
        &mut self,
        cx: &mut Context,
        handle: quinn_proto::ConnectionHandle,
    ) -> Poll<Result<Option<quinn_proto::ApplicationClose>, quinn_proto::ConnectionError>> {
        log::trace!("{:?}: start poll_drive", self.conn.side());
        let p = loop {
            if let Poll::Pending = self.poll_transmit(cx) {
                break Poll::Pending;
            }
            self.poll_timeout(cx);
            if let Poll::Pending = self.poll_endpoint_events(cx, handle) {
                break Poll::Pending;
            };

            while let Some(event) = self.conn.poll() {
                log::trace!("{:?}: event: {:?}", self.conn.side(), event);
                match event {
                    quinn_proto::Event::HandshakeDataReady => {}
                    quinn_proto::Event::Connected => {}
                    quinn_proto::Event::ConnectionLost { reason } => self.closed = Some(reason),
                    quinn_proto::Event::DatagramReceived => {} // TODO: handle
                    quinn_proto::Event::Stream(event) => match event {
                        quinn_proto::StreamEvent::Opened { dir } => self.opened_wake(dir),
                        quinn_proto::StreamEvent::Readable { id } => {
                            self.streams.id(id).recv_wake()
                        }
                        quinn_proto::StreamEvent::Writable { id } => {
                            self.streams.id(id).send_wake()
                        }
                        quinn_proto::StreamEvent::Finished { id } => {
                            self.streams.id(id).finished_send()
                        }
                        quinn_proto::StreamEvent::Stopped { id, error_code } => {
                            self.streams.id(id).stopped_send(error_code)
                        }
                        quinn_proto::StreamEvent::Available { dir } => self.opening_wake(dir),
                    },
                }
            }

            if let Poll::Ready(Some(event)) = self.event_receiver.poll_next_unpin(cx) {
                log::trace!("{:?}: event from endpoint", self.conn.side());
                self.conn.handle_event(event);
                continue;
            }

            if let Some(timer) = &mut self.timer {
                if timer.poll_unpin(cx).is_ready() {
                    self.conn.handle_timeout(Instant::now());
                    continue;
                }
            }

            if self.conn.is_drained()
                && self.queued_endpoint_event.is_none()
                && self.queued_transmit.is_none()
            {
                break Poll::Ready(self.closed());
            }
            break Poll::Pending;
        };
        log::trace!("{:?}: finish poll_drive: {:?}", self.conn.side(), p);
        p
    }
    fn poll_endpoint_events(
        &mut self,
        cx: &mut Context,
        handle: quinn_proto::ConnectionHandle,
    ) -> Poll<()> {
        if self.queued_endpoint_event.is_none() {
            self.queued_endpoint_event = self.conn.poll_endpoint_events();
        }
        while let Some(queued) = self.queued_endpoint_event.take() {
            match self.event_sender.try_send((handle, queued)) {
                Ok(()) => {
                    log::trace!("{:?}: event for endpoint", self.conn.side());
                    self.queued_endpoint_event = self.conn.poll_endpoint_events()
                }
                Err(err) => {
                    self.queued_endpoint_event = Some(err.into_inner().1);
                    _ = ready!(self.event_sender.poll_ready(cx));
                }
            }
        }
        Poll::Ready(())
    }
    fn poll_transmit(&mut self, cx: &mut Context) -> Poll<()> {
        let mgs = self.udp_state.max_gso_segments();
        if self.queued_transmit.is_none() {
            self.queued_transmit = self.conn.poll_transmit(Instant::now(), mgs)
        }
        while let Some(queued) = self.queued_transmit.take() {
            match self.transmit_sender.try_send(queued) {
                Ok(()) => {
                    log::trace!("{:?}: sent transmit", self.conn.side());
                    self.queued_transmit = self.conn.poll_transmit(Instant::now(), mgs)
                }
                Err(err) => {
                    self.queued_transmit = Some(err.into_inner());
                    _ = ready!(self.transmit_sender.poll_ready(cx));
                }
            }
        }
        Poll::Ready(())
    }
    fn poll_timeout(&mut self, cx: &mut Context) {
        self.timer = self.conn.poll_timeout().map(Timer::at);
        if let Some(timer) = &mut self.timer {
            _ = timer.poll_unpin(cx);
        }
    }
}
