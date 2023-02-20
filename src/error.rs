use thiserror::Error;

use std::{fmt::Display, io};

#[derive(Debug, Error)]
pub enum QuicSendError {
    #[error("stream stopped by peer: code {0}")]
    Stopped(quinn_proto::VarInt),
    #[error("stream send queue full")]
    NotReady,
    #[error("stream reset by peer")]
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

#[derive(Debug)]
pub struct Error(std::io::Error);

impl h3::quic::Error for Error {
    fn is_timeout(&self) -> bool {
        unreachable!()
    }

    fn err_code(&self) -> Option<u64> {
        unreachable!()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(value: Error) -> Self {
        value.0
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value)
    }
}

impl From<std::io::ErrorKind> for Error {
    fn from(value: std::io::ErrorKind) -> Self {
        Self(value.into())
    }
}
