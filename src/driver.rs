use futures::prelude::*;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::ConnectionInner;

pub struct QuicConnectionDriver(pub(crate) Arc<ConnectionInner>);

impl Future for QuicConnectionDriver {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_drive(cx)
    }
}
