use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{future::FusedFuture, Future};

use crate::{ConnectionInner, QuicConnection};

#[derive(Debug)]
pub struct QuicConnecting {
    pub(crate) inner: Option<Arc<ConnectionInner>>,
}

impl Future for QuicConnecting {
    type Output = QuicConnection;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.inner.take().unwrap();
        _ = inner.poll_drive(cx);
        if inner.is_handshaking() {
            self.inner = Some(inner);
            return Poll::Pending;
        }
        Poll::Ready(QuicConnection { inner })
    }
}

impl FusedFuture for QuicConnecting {
    fn is_terminated(&self) -> bool {
        self.inner.is_none()
    }
}
