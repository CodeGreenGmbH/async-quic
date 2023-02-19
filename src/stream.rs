use std::{
    io,
    pin::Pin,
    process::abort,
    sync::Arc,
    task::{Context, Poll},
};

use crate::{error::Error, ConnectionInner};
use bytes::{Buf, Bytes};
use futures::{prelude::*, ready};

pub struct QuicStream<const R: bool, const W: bool> {
    conn: Arc<ConnectionInner>,
    id: quinn_proto::StreamId,
    writing: Option<h3::quic::WriteBuf<Bytes>>,
}

impl<const R: bool, const W: bool> QuicStream<R, W> {
    pub(crate) const fn dir() -> quinn_proto::Dir {
        match R & W {
            true => quinn_proto::Dir::Bi,
            false => quinn_proto::Dir::Uni,
        }
    }
    pub(crate) fn new(conn: Arc<ConnectionInner>, id: quinn_proto::StreamId) -> Self {
        Self {
            conn,
            id,
            writing: None,
        }
    }
}

impl<const R: bool> h3::quic::SendStream<Bytes> for QuicStream<R, true> {
    type Error = Error;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        while let Some(data) = &mut self.writing {
            match ready!(self.conn.poll_write(self.id, cx, data.chunk())) {
                Ok(n) => data.advance(n),
                Err(err) => {
                    return Poll::Ready(Err(match err {
                        Some(reason) => io::ErrorKind::BrokenPipe.into(),
                        None => io::ErrorKind::BrokenPipe.into(),
                    }))
                }
            }
            if !data.has_remaining() {
                self.writing = None;
            }
        }
        Poll::Ready(Ok(()))
    }

    fn send_data<T: Into<h3::quic::WriteBuf<Bytes>>>(
        &mut self,
        data: T,
    ) -> Result<(), Self::Error> {
        if self.writing.is_some() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.writing = Some(data.into());
        Ok(())
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_ready(cx))?;
        self.conn.poll_finish(self.id, cx)
    }

    fn reset(&mut self, reset_code: u64) {
        dbg!(reset_code);
        abort()
    }

    fn id(&self) -> h3::quic::StreamId {
        self.id.index().try_into().unwrap()
    }
}

impl<const R: bool> AsyncWrite for QuicStream<R, true> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        use h3::quic::SendStream;
        Poll::Ready(match ready!(self.poll_ready(cx)) {
            Ok(_) => match ready!(self.conn.poll_write(self.id, cx, buf)) {
                Ok(n) => Ok(n),
                Err(_) => Err(io::ErrorKind::BrokenPipe.into()),
            },
            Err(err) => Err(err.into()),
        })
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TODO: Check if all data has been acked?
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        use h3::quic::SendStream;
        self.poll_finish(cx).map_err(|err| err.into())
    }
}

impl<const W: bool> h3::quic::RecvStream for QuicStream<true, W> {
    type Buf = Bytes;

    type Error = Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        Poll::Ready(
            ready!(self.conn.poll_recv(self.id, cx, usize::MAX))
                .map_err(|err| io::ErrorKind::BrokenPipe.into()),
        )
    }

    fn stop_sending(&mut self, error_code: u64) {
        dbg!(error_code);
        abort()
    }
}

impl<const W: bool> AsyncRead for QuicStream<true, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(match ready!(self.conn.poll_recv(self.id, cx, buf.len())) {
            Ok(None) => Ok(0),
            Err(_) => Err(io::ErrorKind::BrokenPipe.into()),
            Ok(Some(mut b)) => {
                let n = b.len();
                b.copy_to_slice(&mut buf[0..n]);
                Ok(n)
            }
        })
    }
}

impl h3::quic::BidiStream<Bytes> for QuicStream<true, true> {
    type SendStream = QuicStream<false, true>;

    type RecvStream = QuicStream<true, false>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        dbg!();
        abort()
    }
}
