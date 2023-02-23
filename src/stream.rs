use std::{
    fmt, io,
    pin::Pin,
    process::abort,
    sync::Arc,
    task::{Context, Poll},
};

use crate::{error::QuicRecvError, streams::StreamHandle};
use crate::{error::QuicSendError, ConnectionInner};
use bytes::{Buf, Bytes};
use futures::{prelude::*, ready};

pub struct QuicStream<const R: bool, const W: bool> {
    conn: Arc<ConnectionInner>,
    handle: StreamHandle,
    id: quinn_proto::StreamId,
    writing: Option<h3::quic::WriteBuf<Bytes>>,
}

impl<const R: bool, const W: bool> Drop for QuicStream<R, W> {
    fn drop(&mut self) {
        self.conn.drop_stream_handle(self.handle.extract());
    }
}

impl<const R: bool, const W: bool> QuicStream<R, W> {
    pub(crate) const fn dir() -> quinn_proto::Dir {
        match R & W {
            true => quinn_proto::Dir::Bi,
            false => quinn_proto::Dir::Uni,
        }
    }
    pub(crate) fn new(conn: Arc<ConnectionInner>, handle: StreamHandle, id: quinn_proto::StreamId) -> Self {
        Self {
            conn,
            handle,
            id,
            writing: None,
        }
    }
}

impl<const R: bool> h3::quic::SendStream<Bytes> for QuicStream<R, true> {
    type Error = QuicSendError;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        while let Some(data) = &mut self.writing {
            let n = ready!(self.conn.poll_write(&self.handle, cx, data.chunk()))?;
            data.advance(n);
            if !data.has_remaining() {
                self.writing = None;
            }
        }
        Poll::Ready(Ok(()))
    }

    fn send_data<T: Into<h3::quic::WriteBuf<Bytes>>>(&mut self, data: T) -> Result<(), Self::Error> {
        if self.writing.is_some() {
            return Err(QuicSendError::NotReady);
        }
        self.writing = Some(data.into());
        Ok(())
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_ready(cx))?;
        self.conn.poll_finish(&self.handle, cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.conn.reset(&self.handle, quinn_proto::VarInt::try_from(reset_code).unwrap())
    }

    fn id(&self) -> h3::quic::StreamId {
        self.id.index().try_into().unwrap()
    }
}

impl<const R: bool> AsyncWrite for QuicStream<R, true> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        use h3::quic::SendStream;
        _ = self.poll_ready(cx)?;
        self.conn.poll_write(&self.handle, cx, buf).map_err(Into::into)
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

    type Error = QuicRecvError;

    fn poll_data(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.conn.poll_recv(&self.handle, cx, usize::MAX)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.conn.stop(&self.handle, error_code.try_into().unwrap())
    }
}

impl<const W: bool> AsyncRead for QuicStream<true, W> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(match ready!(self.conn.poll_recv(&self.handle, cx, buf.len())) {
            Ok(None) => Ok(0),
            Err(err) => Err(err.into()),
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

impl<const W: bool, const R: bool> fmt::Debug for QuicStream<R, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = format!("QuickStream<{},{}>", R, W);
        f.debug_struct(&name).field("id", &self.id).finish_non_exhaustive()
    }
}
