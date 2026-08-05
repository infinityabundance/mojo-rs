//! Error taxonomy for the idiomatic system API.

use std::fmt;

/// Errors surfaced by the system API. Maps 1:1 onto the core error taxonomy
/// (and therefore the official `MOJO_RESULT_*` codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemError {
    /// The operation was cancelled (typically by the caller).
    Cancelled,
    /// An unknown error occurred.
    Unknown,
    /// The caller passed an invalid argument (handle value, option struct,
    /// null pointer, ...).
    InvalidArgument,
    /// The deadline expired before the operation completed.
    DeadlineExceeded,
    /// The requested entity was not found.
    NotFound,
    /// The entity already exists.
    AlreadyExists,
    /// The caller does not have permission.
    PermissionDenied,
    /// A resource limit was exceeded.
    ResourceExhausted,
    /// The operation could not proceed given the current state.
    FailedPrecondition,
    /// The operation was aborted.
    Aborted,
    /// The argument was out of range.
    OutOfRange,
    /// The operation is unimplemented.
    Unimplemented,
    /// An internal error occurred.
    Internal,
    /// The service is unavailable.
    Unavailable,
    /// Data was lost.
    DataLoss,
    /// A resource is busy.
    Busy,
    /// The operation would block (non-blocking mode).
    ShouldWait,
}

/// Result alias for system API operations.
pub type SystemResult<T> = Result<T, SystemError>;

impl fmt::Display for SystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SystemError {}

impl From<mojo_rs_core::error::CoreError> for SystemError {
    fn from(e: mojo_rs_core::error::CoreError) -> SystemError {
        use SystemError as S;
        use mojo_rs_core::error::CoreError as C;
        match e {
            C::Ok => S::Unknown, // Ok is not an error; callers map Ok explicitly.
            C::Cancelled => S::Cancelled,
            C::Unknown => S::Unknown,
            C::InvalidArgument => S::InvalidArgument,
            C::DeadlineExceeded => S::DeadlineExceeded,
            C::NotFound => S::NotFound,
            C::AlreadyExists => S::AlreadyExists,
            C::PermissionDenied => S::PermissionDenied,
            C::ResourceExhausted => S::ResourceExhausted,
            C::FailedPrecondition => S::FailedPrecondition,
            C::Aborted => S::Aborted,
            C::OutOfRange => S::OutOfRange,
            C::Unimplemented => S::Unimplemented,
            C::Internal => S::Internal,
            C::Unavailable => S::Unavailable,
            C::DataLoss => S::DataLoss,
            C::Busy => S::Busy,
            C::ShouldWait => S::ShouldWait,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn core_mapping_roundtrip() {
        assert_eq!(
            SystemError::from(mojo_rs_core::error::CoreError::Busy),
            SystemError::Busy
        );
        assert_eq!(
            SystemError::from(mojo_rs_core::error::CoreError::FailedPrecondition),
            SystemError::FailedPrecondition
        );
    }
}
