use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

use crate::{ConnectionInner, EndpointInner};
use async_io::Timer;
use futures::prelude::*;

pub struct QuicStream<const R: bool, const W: bool> {
    conn: Arc<ConnectionInner>,
    id: quinn_proto::StreamId,
}

impl<const R: bool, const W: bool> QuicStream<R, W> {
    pub(crate) fn new(conn: Arc<ConnectionInner>, id: quinn_proto::StreamId) -> Self {
        Self { conn, id }
    }
}

impl<const W: bool> AsyncRead for QuicStream<true, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.conn
            .recv_stream(self.id, |mut stream| match stream.read(true) {
                Ok(chunks) => todo!(),
                Err(_) => return (Poll::Ready(Ok(0)), None),
            })
    }
}

impl<const R: bool> AsyncWrite for QuicStream<R, true> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }
}
