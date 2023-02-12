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

impl std::fmt::Display for Infallible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
