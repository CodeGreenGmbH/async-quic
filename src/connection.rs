use std::sync::{Arc, Mutex};

pub struct QuicConnection {
    inner: Arc<ConnectionInner>,
}

impl QuicConnection {
    pub(crate) fn new(conn: quinn_proto::Connection) -> Self {
        let state = Mutex::new(conn);
        let inner = Arc::new(ConnectionInner { state });
        Self { inner }
    }
    pub(crate) fn inner(&self) -> Arc<ConnectionInner> {
        self.inner.clone()
    }
}

pub(crate) struct ConnectionInner {
    state: Mutex<quinn_proto::Connection>,
}

impl ConnectionInner {
    pub(crate) fn handle_event(&self, event: quinn_proto::ConnectionEvent) {
        let mut guard = self.state.lock().unwrap();
        guard.handle_event(event)
    }
}
