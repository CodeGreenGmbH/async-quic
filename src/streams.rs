use std::{
    collections::BTreeMap,
    mem::{forget, replace},
    task::{Context, Waker},
};

use slab::Slab;

use crate::error::{QuicRecvError, QuicSendError};

#[derive(Default, Debug)]
pub(crate) struct Streams {
    id2idx: BTreeMap<quinn_proto::StreamId, usize>,
    states: Slab<StreamState>,
}

#[derive(Debug)]
pub(crate) struct StreamHandle {
    idx: usize,
    r: bool,
    w: bool,
}

impl StreamHandle {
    pub(crate) fn extract(&mut self) -> StreamHandle {
        StreamHandle {
            idx: replace(&mut self.idx, usize::MAX),
            r: self.r,
            w: self.w,
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if self.idx != usize::MAX {
            unreachable!()
        }
    }
}

impl Streams {
    pub(crate) fn create(&mut self, id: quinn_proto::StreamId, r: bool, w: bool) -> StreamHandle {
        let state = StreamState::new(id, r, w);
        let idx = self.states.insert(state);
        // TODO: May panic because quinn-proto doesn't seem to emit reset events.
        self.id2idx.insert(id, idx).ok_or(()).unwrap_err();
        StreamHandle { idx, r, w }
    }
    pub(crate) fn drop_handle(&mut self, handle: StreamHandle) -> Option<StreamState> {
        let state = self.states.get_mut(handle.idx).unwrap();
        handle.r.then(|| state.r = Some(false));
        handle.w.then(|| state.w = Some(false));
        forget(handle);
        if state.r.unwrap_or(false) | state.w.unwrap_or(false) {
            let idx = self.id2idx.remove(&state.id).unwrap();
            return Some(self.states.remove(idx));
        }
        None
    }
    pub(crate) fn handle(&mut self, handle: &StreamHandle) -> &mut StreamState {
        self.states.get_mut(handle.idx).unwrap()
    }
    pub(crate) fn id(&mut self, id: quinn_proto::StreamId) -> &mut StreamState {
        let idx = *self.id2idx.get(&id).unwrap();
        self.states.get_mut(idx).unwrap()
    }
}

#[derive(Debug)]
pub(crate) struct StreamState {
    r: Option<bool>,
    w: Option<bool>,
    id: quinn_proto::StreamId,
    send_waker: Option<Waker>,
    recv_waker: Option<Waker>,
    finished_send: Option<bool>,
    stopped_send: Option<quinn_proto::VarInt>,
    reset_send: bool,
    finished_recv: bool,
    stopped_recv: bool,
    reset_recv: Option<quinn_proto::VarInt>,
}

impl StreamState {
    fn new(id: quinn_proto::StreamId, r: bool, w: bool) -> Self {
        Self {
            r: r.then_some(true),
            w: w.then_some(true),
            send_waker: None,
            recv_waker: None,
            stopped_send: None,
            id,
            finished_recv: false,
            stopped_recv: false,
            reset_send: false,
            reset_recv: None,
            finished_send: None,
        }
    }
    pub(crate) fn send_wake(&mut self) {
        debug_assert!(self.w.is_some());
        self.send_waker.take().map(Waker::wake);
    }
    pub(crate) fn recv_wake(&mut self) {
        debug_assert!(self.r.is_some());
        self.recv_waker.take().map(Waker::wake);
    }
    pub(crate) fn set_send_wake(&mut self, cx: &Context) {
        debug_assert!(self.w.is_some());
        self.send_waker = Some(cx.waker().clone());
    }
    pub(crate) fn set_recv_wake(&mut self, cx: &Context) {
        debug_assert!(self.r.is_some());
        self.recv_waker = Some(cx.waker().clone());
    }
    pub(crate) fn finishing_send(&mut self) {
        debug_assert!(self.w.is_some());
        debug_assert_eq!(self.finished_send.replace(false), None);
        self.send_wake();
    }
    pub(crate) fn finished_send(&mut self) {
        debug_assert!(self.w.is_some());
        debug_assert_eq!(self.finished_send.replace(true), Some(false));
        self.send_wake();
    }
    pub(crate) fn stopped_send(&mut self, error_code: quinn_proto::VarInt) {
        debug_assert!(self.w.is_some());
        debug_assert!(self.stopped_send.replace(error_code).is_none());
        self.send_wake();
    }
    pub(crate) fn reset_send(&mut self) {
        debug_assert!(self.w.is_some());
        debug_assert!(!replace(&mut self.reset_send, true));
        self.send_wake();
    }
    pub(crate) fn send_id(&self) -> Result<quinn_proto::StreamId, QuicSendError> {
        debug_assert!(self.w.is_some());
        if let Some(error_code) = self.stopped_send {
            return Err(QuicSendError::Stopped(error_code));
        }
        if self.reset_send {
            return Err(QuicSendError::Reset);
        }
        match self.finished_send {
            Some(false) => Err(QuicSendError::Finishing),
            Some(true) => Err(QuicSendError::Finished),
            None => Ok(self.id),
        }
    }
    pub(crate) fn finished_recv(&mut self) {
        debug_assert!(self.r.is_some());
        debug_assert!(!replace(&mut self.finished_recv, true))
    }
    pub(crate) fn reset_recv(&mut self, error_code: quinn_proto::VarInt) {
        debug_assert!(self.r.is_some());
        debug_assert!(self.reset_recv.replace(error_code).is_none())
    }
    pub(crate) fn stop_recv(&mut self) {
        debug_assert!(self.r.is_some());
        debug_assert!(!replace(&mut self.stopped_recv, true))
    }
    pub(crate) fn recv_id(&self) -> Result<Option<quinn_proto::StreamId>, QuicRecvError> {
        debug_assert!(self.r.is_some());
        if self.stopped_recv {
            return Err(QuicRecvError::Stopped);
        }
        if let Some(error_code) = self.reset_recv {
            return Err(QuicRecvError::Reset(error_code));
        }
        Ok((!self.finished_recv).then_some(self.id))
    }
}
