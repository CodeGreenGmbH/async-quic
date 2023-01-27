use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::ConnectionInner;
use futures::prelude::*;

pub struct QuicStream<const R: bool, const W: bool> {
    conn: Arc<ConnectionInner>,
    id: quinn_proto::StreamId,
    read_err: Option<quinn_proto::VarInt>,
}

impl<const R: bool, const W: bool> QuicStream<R, W> {
    pub(crate) fn new(conn: Arc<ConnectionInner>, id: quinn_proto::StreamId) -> Self {
        Self {
            conn,
            id,
            read_err: None,
        }
    }
}

impl<const W: bool> AsyncRead for QuicStream<true, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.read_err.is_some() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
        }
        let (n, err) = match self.conn.poll_read(self.id, cx, buf) {
            Poll::Ready(ret) => ret,
            Poll::Pending => return Poll::Pending,
        };
        if let Some(err) = err {
            self.get_mut().read_err = Some(err);
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
            }
        }
        Poll::Ready(Ok(n))
    }
}

impl<const R: bool> AsyncWrite for QuicStream<R, true> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.conn.poll_write(self.id, cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::ErrorKind::ConnectionReset.into())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TODO: Check if all data has been acked?
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TODO: Check if all data and finish have been acked?
        match self.conn.close(self.id, cx) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(_) => Poll::Ready(Err(io::ErrorKind::ConnectionReset.into())),
        }
    }
}
