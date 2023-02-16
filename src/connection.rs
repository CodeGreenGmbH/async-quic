use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Instant,
};

use crate::{
    error::{Error, Infallible},
    EndpointInner, QuicConnectionDriver, QuicStream,
};
use async_io::Timer;
use bytes::Bytes;
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    prelude::*,
    ready,
};

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

        // TODO: return error
        let id = state.conn.streams().open(quinn_proto::Dir::Uni).unwrap();
        state.streams.insert(id, StreamState::default());
        log::debug!("opened send stream: {:?}", id);
        state.drive_wake();
        Poll::Ready(Ok(QuicStream::new(self.inner.clone(), id)))
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        todo!()
    }
}

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
            streams: BTreeMap::new(),
            drive_waker: None,
            transmit_sender: endpoint.transmit_sender.clone(),
            queued_transmit: None,
            error: None,
            event_sender: endpoint.event_sender.clone(),
            event_receiver,
            udp_state: endpoint.udp_state.clone(),
            queued_endpoint_event: None,
        });
        Arc::new(Self {
            state,
            handle,
            event_sender,
        })
    }
    pub(crate) fn is_handshaking(&self) -> bool {
        self.state.lock().unwrap().conn.is_handshaking()
    }
    pub(crate) fn handle_event(&self, event: quinn_proto::ConnectionEvent) {
        let mut state = self.state.lock().unwrap();
        state.conn.handle_event(event);
        state.drive_wake()
    }
    pub(crate) fn poll_drive(self: &Arc<ConnectionInner>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.state.lock().unwrap();
        state.drive_waker = Some(cx.waker().clone());
        state.poll_drive(cx, self.handle)
    }
    pub(crate) fn poll_read(
        &self,
        id: quinn_proto::StreamId,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<(usize, Option<quinn_proto::VarInt>)> {
        let mut guard = self.state.lock().unwrap();
        guard.streams.get_mut(&id).unwrap().send_waker = None;
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
            guard.drive_wake()
        }
        if n == 0 && blocked {
            guard.streams.get_mut(&id).unwrap().send_waker = Some(cx.waker().clone());
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
        guard.streams.get_mut(&id).unwrap().send_waker = None;
        let mut send_stream = guard.conn.send_stream(id);
        match send_stream.write(buf) {
            Ok(n) => {
                guard.drive_wake();
                Poll::Ready(Ok(n))
            }
            Err(quinn_proto::WriteError::Blocked) => {
                guard.streams.get_mut(&id).unwrap().send_waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Err(quinn_proto::WriteError::Stopped(err_code)) => Poll::Ready(Err(Some(err_code))),
            Err(quinn_proto::WriteError::UnknownStream) => Poll::Ready(Err(None)),
        }
    }
    pub(crate) fn poll_finish(
        &self,
        id: quinn_proto::StreamId,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        let mut state = self.state.lock().unwrap();
        match state.stream_state(id).finished {
            Some(true) => Poll::Ready(Ok(())),
            Some(false) => {
                state.stream_state(id).send_waker = Some(cx.waker().clone());
                Poll::Pending
            }
            None => {
                state.conn.send_stream(id).finish().unwrap();
                state.drive_wake();
                let stream_state = state.stream_state(id);
                stream_state.finished = Some(false);
                stream_state.send_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
    pub(crate) fn close(
        &self,
        id: quinn_proto::StreamId,
        _cx: &mut Context<'_>,
    ) -> Result<(), Option<quinn_proto::VarInt>> {
        let mut guard = self.state.lock().unwrap();
        guard.streams.get_mut(&id).unwrap().send_waker = None;
        let mut send_stream = guard.conn.send_stream(id);
        match send_stream.finish() {
            Ok(()) => Ok(()),
            Err(quinn_proto::FinishError::Stopped(err_code)) => Err(Some(err_code)),
            Err(quinn_proto::FinishError::UnknownStream) => Err(None),
        }
    }
}

struct ConnectionState {
    conn: quinn_proto::Connection,
    timer: Option<Timer>,
    drive_waker: Option<Waker>,
    streams: BTreeMap<quinn_proto::StreamId, StreamState>,
    queued_transmit: Option<quinn_proto::Transmit>,
    transmit_sender: Sender<quinn_proto::Transmit>,
    queued_endpoint_event: Option<quinn_proto::EndpointEvent>,
    event_sender: Sender<(quinn_proto::ConnectionHandle, quinn_proto::EndpointEvent)>,
    event_receiver: Receiver<quinn_proto::ConnectionEvent>,
    error: Option<quinn_proto::ConnectionError>,
    udp_state: Arc<quinn_udp::UdpState>,
}

impl ConnectionState {
    fn drive_wake(&mut self) {
        self.drive_waker.take().map(Waker::wake);
    }
    fn stream_state(&mut self, id: quinn_proto::StreamId) -> &mut StreamState {
        self.streams.get_mut(&id).unwrap()
    }
    fn poll_drive(&mut self, cx: &mut Context, handle: quinn_proto::ConnectionHandle) -> Poll<()> {
        loop {
            ready!(self.poll_transmit(cx));
            self.poll_timeout(cx);
            ready!(self.poll_endpoint_events(cx, handle));

            while let Some(event) = self.conn.poll() {
                log::trace!("event: {:?}", event);
                match event {
                    quinn_proto::Event::HandshakeDataReady => {}
                    quinn_proto::Event::Connected => {}
                    quinn_proto::Event::ConnectionLost { reason } => {
                        // TODO: handle?
                        self.error = Some(reason)
                    }
                    quinn_proto::Event::DatagramReceived => {} // TODO: handle
                    quinn_proto::Event::Stream(event) => match event {
                        quinn_proto::StreamEvent::Opened { dir } => {}
                        quinn_proto::StreamEvent::Readable { id } => {
                            self.stream_state(id).recv_wake()
                        }
                        quinn_proto::StreamEvent::Writable { id } => {
                            self.stream_state(id).send_wake()
                        }
                        quinn_proto::StreamEvent::Finished { id } => {
                            let stream_state = self.stream_state(id);
                            stream_state.send_wake();
                            stream_state.finished = Some(true);
                        }
                        quinn_proto::StreamEvent::Stopped { id, .. } => {
                            self.stream_state(id).recv_wake()
                        }
                        quinn_proto::StreamEvent::Available { dir } => todo!("available: {}", dir),
                    },
                }
            }

            if let Poll::Ready(Some(event)) = self.event_receiver.poll_next_unpin(cx) {
                log::trace!("event from endpoint: {:?}", event);
                self.conn.handle_event(event);
                continue;
            }

            if let Some(timer) = &mut self.timer {
                if timer.poll_unpin(cx).is_ready() {
                    self.conn.handle_timeout(Instant::now());
                    continue;
                }
            }
            break;
        }
        Poll::Pending
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
                Ok(()) => self.queued_endpoint_event = self.conn.poll_endpoint_events(),
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
                Ok(()) => self.queued_transmit = self.conn.poll_transmit(Instant::now(), mgs),
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

#[derive(Default, Debug)]
struct StreamState {
    send_waker: Option<Waker>,
    recv_waker: Option<Waker>,
    finished: Option<bool>,
}

impl StreamState {
    fn send_wake(&mut self) {
        dbg!("send_wake");
        self.send_waker.take().map(Waker::wake);
    }
    fn recv_wake(&mut self) {
        dbg!("recv_wake");
        self.recv_waker.take().map(Waker::wake);
    }
}
