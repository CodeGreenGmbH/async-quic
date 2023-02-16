use std::{
    io,
    pin::Pin,
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
    read_err: Option<quinn_proto::VarInt>,
    read_buf: Option<h3::quic::WriteBuf<Bytes>>,
}

impl<const R: bool, const W: bool> QuicStream<R, W> {
    pub(crate) fn new(conn: Arc<ConnectionInner>, id: quinn_proto::StreamId) -> Self {
        Self {
            conn,
            id,
            writing: None,
            read_err: None,
            read_buf: None,
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
                        Some(reason) => io::ErrorKind::ConnectionReset.into(),
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
        self.conn.poll_finish(self.id, cx)
    }

    fn reset(&mut self, reset_code: u64) {
        todo!()
    }

    fn id(&self) -> h3::quic::StreamId {
        todo!()
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

impl<const W: bool> h3::quic::RecvStream for QuicStream<true, W> {
    type Buf = Bytes;

    type Error = Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        todo!()
    }

    fn stop_sending(&mut self, error_code: u64) {
        todo!()
    }
}

//impl<const W: bool> AsyncRead for QuicStream<true, W> {
//    fn poll_read(
//            self: Pin<&mut Self>,
//            cx: &mut Context<'_>,
//            buf: &mut [u8],
//            ) -> Poll<io::Result<usize>> {
//        if self.read_err.is_some() {
//            return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
//        }
//        use h3::quic::RecvStream;
//        loop {
//            match self.read_buf {
//                None => self.poll_data(cx)
//            }
//        }
//        self.read_buf.split_to(at)
//
//        let b = ready!(self.poll_data(cx))?;
//        let (n, err) = match self.conn.poll_read(self.id, cx, buf) {
//            Poll::Ready(ret) => ret,
//            Poll::Pending => return Poll::Pending,
//        };
//        if let Some(err) = err {
//            self.get_mut().read_err = Some(err);
//            if n == 0 {
//                return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
//            }
//        }
//        Poll::Ready(Ok(n))
//    }
//}

impl h3::quic::BidiStream<Bytes> for QuicStream<true, true> {
    type SendStream = QuicStream<false, true>;

    type RecvStream = QuicStream<true, false>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        todo!()
    }
}
