use std::{
    collections::{BTreeMap, VecDeque},
    mem::replace,
    task::{Context, Waker},
};

use slab::Slab;

use crate::error::{QuicRecvError, QuicSendError};

#[derive(Default, Debug)]
pub(crate) struct Streams {
    id2idx: BTreeMap<quinn_proto::StreamId, usize>,
    states: Slab<StreamState>,
    opened: [VecDeque<StreamHandle>; 2],
}

impl Drop for Streams {
    fn drop(&mut self) {
        for i in 0..self.opened.len() {
            while let Some(mut handle) = self.opened[i].pop_front() {
                self.drop_handle(&mut handle);
            }
        }
        assert!(self.states.is_empty());
        assert!(self.id2idx.is_empty());
    }
}

#[derive(Debug)]
pub(crate) struct StreamHandle {
    idx: usize,
    id: quinn_proto::StreamId,
    r: bool,
    w: bool,
}

impl StreamHandle {
    pub fn id(&self) -> quinn_proto::StreamId {
        self.id
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
    pub(crate) fn accept(&mut self, dir: quinn_proto::Dir) -> Option<StreamHandle> {
        self.opened[dir as usize].pop_front()
    }
    pub(crate) fn opened(&mut self, id: quinn_proto::StreamId) {
        let handle = self.create(id, true, id.dir() == quinn_proto::Dir::Bi);
        self.opened[id.dir() as usize].push_back(handle);
    }
    pub(crate) fn create(&mut self, id: quinn_proto::StreamId, r: bool, w: bool) -> StreamHandle {
        let state = StreamState::new(id, r, w);
        let idx = self.states.insert(state);
        self.id2idx.insert(id, idx).ok_or(()).unwrap_err();
        StreamHandle { idx, id, r, w }
    }
    pub(crate) fn drop_handle(&mut self, handle: &mut StreamHandle) -> Option<StreamState> {
        let state = self.states.get_mut(handle.idx).unwrap();
        handle.r.then(|| state.r = Some(false));
        handle.w.then(|| state.w = Some(false));
        handle.idx = usize::MAX;
        if state.r.unwrap_or(false) || state.w.unwrap_or(false) {
            return None;
        }
        let idx = self.id2idx.remove(&state.id).unwrap();
        Some(self.states.remove(idx))
    }
    pub(crate) fn handle(&mut self, handle: &StreamHandle) -> &mut StreamState {
        self.states.get_mut(handle.idx).unwrap()
    }
    pub(crate) fn id(&mut self, id: quinn_proto::StreamId) -> &mut StreamState {
        self.states.get_mut(*self.id2idx.get(&id).unwrap()).unwrap()
    }
    pub(crate) fn wake_all(&mut self) {
        for (_, state) in self.states.iter_mut() {
            state.wake_all();
        }
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
    reset_send: Option<quinn_proto::VarInt>,
    finished_recv: bool,
    stopped_recv: Option<quinn_proto::VarInt>,
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
            stopped_recv: None,
            reset_send: None,
            reset_recv: None,
            finished_send: None,
        }
    }
    pub(crate) fn wake_all(&mut self) {
        if self.w.is_some() {
            self.send_wake()
        }
        if self.r.is_some() {
            self.recv_wake()
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
    pub(crate) fn reset_send(&mut self, error_code: quinn_proto::VarInt) {
        debug_assert!(self.w.is_some());
        debug_assert!(self.reset_send.replace(error_code).is_none());
        self.send_wake();
    }
    pub(crate) fn send_id(&self) -> Result<quinn_proto::StreamId, QuicSendError> {
        debug_assert!(self.w.is_some());
        if let Some(error_code) = self.stopped_send {
            return Err(QuicSendError::Stopped(error_code));
        }
        if let Some(complete) = self.finished_send {
            return match complete {
                false => Err(QuicSendError::Finishing),
                true => Err(QuicSendError::Finished),
            };
        }
        if let Some(error_code) = self.reset_send {
            return Err(QuicSendError::Reset(error_code));
        }
        Ok(self.id)
    }
    pub(crate) fn finished_recv(&mut self) {
        debug_assert!(self.r.is_some());
        debug_assert!(!replace(&mut self.finished_recv, true))
    }
    pub(crate) fn reset_recv(&mut self, error_code: quinn_proto::VarInt) {
        debug_assert!(self.r.is_some());
        debug_assert!(self.reset_recv.replace(error_code).is_none())
    }
    pub(crate) fn stop_recv(&mut self, error_code: quinn_proto::VarInt) {
        debug_assert!(self.r.is_some());
        debug_assert!(self.stopped_recv.replace(error_code).is_none())
    }
    pub(crate) fn recv_id(&self) -> Result<Option<quinn_proto::StreamId>, QuicRecvError> {
        debug_assert!(self.r.is_some());
        if let Some(error_code) = self.stopped_recv {
            return Err(QuicRecvError::Stopped(error_code));
        }
        if self.finished_recv {
            return Ok(None);
        }
        if let Some(error_code) = self.reset_recv {
            return Err(QuicRecvError::Reset(error_code));
        }
        Ok(Some(self.id))
    }
}
