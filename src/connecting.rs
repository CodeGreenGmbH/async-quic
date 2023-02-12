use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{future::FusedFuture, Future};

use crate::{ConnectionInner, QuicConnection};

pub struct QuicConnecting {
    pub(crate) inner: Option<Arc<ConnectionInner>>,
}

impl Future for QuicConnecting {
    type Output = Result<QuicConnection, quinn_proto::ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.inner.as_ref().unwrap();
        if inner.poll_drive(cx).is_ready() {
            todo!()
        }
        if inner.is_handshaking() {
            return Poll::Pending;
        }
        let inner = inner.clone();
        let conn = QuicConnection { inner };
        if let Some(err) = conn.error() {
            return Poll::Ready(Err(err));
        }
        Poll::Ready(Ok(conn))
    }
}

impl FusedFuture for QuicConnecting {
    fn is_terminated(&self) -> bool {
        self.inner.is_none()
    }
}
