use thiserror::Error;

use std::{fmt::Display, io};

#[derive(Debug, Error)]
pub enum QuicRecvError {
    #[error("recv on stopped stream stopped stream")]
    Stopped,
    #[error("stream reset by peer: code {0}")]
    Reset(quinn_proto::VarInt),
}

impl h3::quic::Error for QuicRecvError {
    fn is_timeout(&self) -> bool {
        false
    }

    fn err_code(&self) -> Option<u64> {
        None
    }
}

impl From<QuicRecvError> for std::io::Error {
    fn from(value: QuicRecvError) -> Self {
        let kind = match value {
            QuicRecvError::Stopped => io::ErrorKind::ConnectionAborted,
            QuicRecvError::Reset(_) => io::ErrorKind::ConnectionReset,
        };
        io::Error::new(kind, value)
    }
}

#[derive(Debug, Error)]
pub enum QuicSendError {
    #[error("stream stopped by peer: code {0}")]
    Stopped(quinn_proto::VarInt),
    #[error("stream send queue full")]
    NotReady,
    #[error("send on reset stream")]
    Reset,
    #[error("send on finishing stream")]
    Finishing,
    #[error("send on finished stream")]
    Finished,
}

impl h3::quic::Error for QuicSendError {
    fn is_timeout(&self) -> bool {
        false
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            QuicSendError::Stopped(c) => Some(c),
            _ => None,
        }
        .copied()
        .map(quinn_proto::VarInt::into_inner)
    }
}

impl From<QuicSendError> for std::io::Error {
    fn from(value: QuicSendError) -> Self {
        let kind = match value {
            QuicSendError::Stopped(_) => io::ErrorKind::ConnectionAborted,
            QuicSendError::NotReady => io::ErrorKind::WouldBlock,
            QuicSendError::Reset => io::ErrorKind::ConnectionReset,
            QuicSendError::Finishing => io::ErrorKind::WriteZero,
            QuicSendError::Finished => io::ErrorKind::WriteZero,
        };
        io::Error::new(kind, value)
    }
}

#[derive(Debug)]
pub struct Infallible(std::convert::Infallible);

impl h3::quic::Error for Infallible {
    fn is_timeout(&self) -> bool {
        unreachable!()
    }

    fn err_code(&self) -> Option<u64> {
        unreachable!()
    }
}

impl Display for Infallible {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

impl std::error::Error for Infallible {}
