use futures::{future::FusedFuture, prelude::*};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::{
    error::{QuicApplicationClose, QuicConnectionError},
    ConnectionInner,
};

pub struct QuicConnectionDriver(pub(crate) Arc<ConnectionInner>, pub(crate) bool);

impl Future for QuicConnectionDriver {
    type Output = Result<QuicApplicationClose, QuicConnectionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let p = self.0.poll_drive(cx);
        self.1 |= p.is_ready();
        p
    }
}

impl FusedFuture for QuicConnectionDriver {
    fn is_terminated(&self) -> bool {
        self.1
    }
}
