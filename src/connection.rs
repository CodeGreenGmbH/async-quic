use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Instant,
};

use crate::{EndpointInner, QuicStream};
use async_io::Timer;
use futures::prelude::*;

pub struct QuicConnection {
    inner: Arc<ConnectionInner>,
}

impl QuicConnection {
    pub(crate) fn new(
        handle: quinn_proto::ConnectionHandle,
        conn: quinn_proto::Connection,
        endpoint: Arc<EndpointInner>,
    ) -> Self {
        let state = Mutex::new(ConnectionState {
            conn,
            timer: None,
            stream_wakers: BTreeMap::new(),
            conn_waker: None,
        });
        let inner = Arc::new(ConnectionInner {
            state,
            endpoint,
            handle,
        });
        Self { inner }
    }
    pub(crate) fn inner(&self) -> Arc<ConnectionInner> {
        self.inner.clone()
    }
}

pub(crate) struct ConnectionInner {
    state: Mutex<ConnectionState>,
    endpoint: Arc<EndpointInner>,
    handle: quinn_proto::ConnectionHandle,
}

impl ConnectionInner {
    pub(crate) fn handle_event(&self, event: quinn_proto::ConnectionEvent) {
        let mut guard = self.state.lock().unwrap();
        guard.conn.handle_event(event);
        if let Some(waker) = guard.conn_waker.take() {
            waker.wake()
        }
    }
    fn poll(self: &Arc<ConnectionInner>, cx: &mut Context<'_>) -> Poll<QuicConnectionEvent> {
        let mut guard = self.state.lock().unwrap();
        guard.conn_waker = None;
        while let Some(transmit) = guard.conn.poll_transmit(Instant::now(), 1) {
            self.endpoint.transmit(transmit);
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
        guard.conn_waker = Some(cx.waker().clone());
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
            if let Some(w) = guard.conn_waker.take() {
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
    conn_waker: Option<Waker>,
    stream_wakers: BTreeMap<quinn_proto::StreamId, [Option<Waker>; 2]>,
}

impl ConnectionState {
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
