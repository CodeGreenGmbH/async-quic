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
        guard.conn.handle_event(event)
    }
    fn poll(self: &Arc<ConnectionInner>, cx: &mut Context<'_>) -> Poll<QuicConnectionEvent> {
        let mut guard = self.state.lock().unwrap();
        while let Some(transmit) = guard.conn.poll_transmit(Instant::now(), 1) {
            self.endpoint.transmit(transmit)
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
                guard.conn.handle_event(event)
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
                    quinn_proto::StreamEvent::Opened { dir } => log::info!("incoming: {:?}", dir),
                    quinn_proto::StreamEvent::Readable { id } => todo!(),
                    quinn_proto::StreamEvent::Writable { id } => todo!(),
                    quinn_proto::StreamEvent::Finished { id } => todo!(),
                    quinn_proto::StreamEvent::Stopped { id, error_code } => todo!(),
                    quinn_proto::StreamEvent::Available { dir } => todo!(),
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
        return Poll::Pending;
    }
    pub(crate) fn recv_stream<F, R>(&self, id: quinn_proto::StreamId, f: F) -> R
    where
        F: FnOnce(quinn_proto::RecvStream) -> (R, Option<Waker>),
    {
        let mut guard = self.state.lock().unwrap();
        let stream = guard.conn.recv_stream(id);
        let (ret, waker) = f(stream);
        guard.stream_wakers.get_mut(&id).unwrap()[0] = waker;
        ret
    }
    pub(crate) fn send_stream<F, R>(&self, id: quinn_proto::StreamId, f: F) -> R
    where
        F: FnOnce(quinn_proto::SendStream) -> (R, Option<Waker>),
    {
        let mut guard = self.state.lock().unwrap();
        let stream = guard.conn.send_stream(id);
        let (ret, waker) = f(stream);
        guard.stream_wakers.get_mut(&id).unwrap()[1] = waker;
        ret
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

pub enum QuicConnectionEvent {
    StreamR(QuicStream<true, false>),
    StreamRW(QuicStream<true, true>),
}
