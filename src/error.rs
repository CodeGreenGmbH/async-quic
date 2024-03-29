use bytes::Bytes;
use thiserror::Error;

use std::{fmt::Display, io};

#[derive(Clone, Debug, Error)]
pub struct QuicApplicationClose {
    /// Application-specific reason code
    pub error_code: quinn_proto::VarInt,
    /// Human-readable reason
    pub reason: Bytes,
    /// Closed by peer if true
    pub remote: bool,
}

impl Display for QuicApplicationClose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = match self.remote {
            true => "by peer",
            false => "locally",
        };
        f.write_fmt(format_args!("closed {}: code {}", side, self.error_code))
    }
}

impl h3::quic::Error for QuicApplicationClose {
    fn is_timeout(&self) -> bool {
        false
    }

    fn err_code(&self) -> Option<u64> {
        Some(self.error_code.into_inner())
    }
}

#[derive(Clone, Debug, Error)]
pub enum QuicConnectionError {
    #[error("peer doesn't implement any supported version")]
    VersionMismatch,
    #[error(transparent)]
    TransportError(quinn_proto::TransportError),
    #[error("aborted by peer: {0}")]
    Abort(quinn_proto::ConnectionClose),
    #[error("reset by peer")]
    Reset,
    #[error("timed out")]
    TimedOut,
}

impl QuicConnectionError {
    pub(crate) fn from_close_reason(reason: quinn_proto::ConnectionError) -> Result<QuicApplicationClose, QuicConnectionError> {
        match reason {
            quinn_proto::ConnectionError::VersionMismatch => Err(QuicConnectionError::VersionMismatch),
            quinn_proto::ConnectionError::TransportError(err) => Err(QuicConnectionError::TransportError(err)),
            quinn_proto::ConnectionError::ConnectionClosed(abort) => Err(QuicConnectionError::Abort(abort)),
            quinn_proto::ConnectionError::ApplicationClosed(close) => Ok(QuicApplicationClose {
                error_code: close.error_code,
                reason: close.reason,
                remote: true,
            }),
            quinn_proto::ConnectionError::Reset => Err(QuicConnectionError::Reset),
            quinn_proto::ConnectionError::TimedOut => Err(QuicConnectionError::TimedOut),
            quinn_proto::ConnectionError::LocallyClosed => unreachable!(),
        }
    }
}

impl h3::quic::Error for QuicConnectionError {
    fn is_timeout(&self) -> bool {
        if let QuicConnectionError::TimedOut = self {
            return true;
        }
        false
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            QuicConnectionError::VersionMismatch => None,
            QuicConnectionError::TransportError(err) => Some(err.code.into()),
            QuicConnectionError::Abort(err) => Some(err.error_code.into()),
            QuicConnectionError::Reset => None,
            QuicConnectionError::TimedOut => None,
        }
    }
}

#[derive(Clone, Debug, Error)]
pub enum QuicOpenStreamError {
    #[error(transparent)]
    ConnectionError(QuicConnectionError),
    #[error(transparent)]
    ApplicationClose(QuicApplicationClose),
}

impl From<Result<QuicApplicationClose, QuicConnectionError>> for QuicOpenStreamError {
    fn from(value: Result<QuicApplicationClose, QuicConnectionError>) -> Self {
        match value {
            Ok(close) => Self::ApplicationClose(close),
            Err(err) => Self::ConnectionError(err),
        }
    }
}

impl h3::quic::Error for QuicOpenStreamError {
    fn is_timeout(&self) -> bool {
        if let Self::ConnectionError(err) = self {
            return err.is_timeout();
        }
        false
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            Self::ConnectionError(err) => err.err_code(),
            Self::ApplicationClose(close) => Some(close.error_code.into()),
        }
    }
}

#[derive(Debug, Error)]
pub enum QuicRecvError {
    #[error("recv on stopped stream")]
    Stopped(quinn_proto::VarInt),
    #[error("stream reset by peer: code {0}")]
    Reset(quinn_proto::VarInt),
    #[error("connection terminated")]
    Terminated(Result<QuicApplicationClose, QuicConnectionError>),
}

impl h3::quic::Error for QuicRecvError {
    fn is_timeout(&self) -> bool {
        if let Self::Terminated(res) = self {
            return match res {
                Ok(ok) => ok.is_timeout(),
                Err(err) => err.is_timeout(),
            };
        }
        false
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            QuicRecvError::Stopped(code) => Some(code.into_inner()),
            QuicRecvError::Reset(code) => Some(code.into_inner()),
            QuicRecvError::Terminated(res) => match res {
                Ok(ok) => ok.err_code(),
                Err(err) => err.err_code(),
            },
        }
    }
}

impl From<QuicRecvError> for std::io::Error {
    fn from(value: QuicRecvError) -> Self {
        io::Error::new(io::ErrorKind::Other, value)
    }
}

#[derive(Clone, Debug, Error)]
pub enum QuicSendError {
    #[error("stream stopped by peer: code {0}")]
    Stopped(quinn_proto::VarInt),
    #[error("stream send queue full")]
    NotReady,
    #[error("send on reset stream")]
    Reset(quinn_proto::VarInt),
    #[error("send on finishing stream")]
    Finishing,
    #[error("send on finished stream")]
    Finished,
    #[error("connection terminated")]
    Terminated(Result<QuicApplicationClose, QuicConnectionError>),
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
        io::Error::new(io::ErrorKind::Other, value)
    }
}
