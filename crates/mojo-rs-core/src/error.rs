//! Core error taxonomy.
//!
//! These map 1:1 onto the public `MOJO_RESULT_*` codes of the pinned epoch
//! (see `atlas/api/mojo-c-system-api.json` for the ground-truth values). The C
//! ABI boundary converts `CoreError` into `MojoResult`; courts compare the
//! resulting codes exactly.

/// Core runtime errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreError {
    /// The operation succeeded (no error).
    Ok,
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
    /// A resource limit was exceeded (handle table full, fd exhaustion, ...).
    ResourceExhausted,
    /// The operation could not proceed given the current state (e.g. reading
    /// from an empty pipe whose peer is open).
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

impl CoreError {
    /// The official `MOJO_RESULT_*` name.
    pub fn name(self) -> &'static str {
        use CoreError::*;
        match self {
            Ok => "MOJO_RESULT_OK",
            Cancelled => "MOJO_RESULT_CANCELLED",
            Unknown => "MOJO_RESULT_UNKNOWN",
            InvalidArgument => "MOJO_RESULT_INVALID_ARGUMENT",
            DeadlineExceeded => "MOJO_RESULT_DEADLINE_EXCEEDED",
            NotFound => "MOJO_RESULT_NOT_FOUND",
            AlreadyExists => "MOJO_RESULT_ALREADY_EXISTS",
            PermissionDenied => "MOJO_RESULT_PERMISSION_DENIED",
            ResourceExhausted => "MOJO_RESULT_RESOURCE_EXHAUSTED",
            FailedPrecondition => "MOJO_RESULT_FAILED_PRECONDITION",
            Aborted => "MOJO_RESULT_ABORTED",
            OutOfRange => "MOJO_RESULT_OUT_OF_RANGE",
            Unimplemented => "MOJO_RESULT_UNIMPLEMENTED",
            Internal => "MOJO_RESULT_INTERNAL",
            Unavailable => "MOJO_RESULT_UNAVAILABLE",
            DataLoss => "MOJO_RESULT_DATA_LOSS",
            Busy => "MOJO_RESULT_BUSY",
            ShouldWait => "MOJO_RESULT_SHOULD_WAIT",
        }
    }

    /// The official numeric value of the code (ground truth from the pinned
    /// `types.h`: MOJO_RESULT_OK == 0, then sequential).
    pub fn code(self) -> u32 {
        use CoreError::*;
        match self {
            Ok => 0,
            Cancelled => 1,
            Unknown => 2,
            InvalidArgument => 3,
            DeadlineExceeded => 4,
            NotFound => 5,
            AlreadyExists => 6,
            PermissionDenied => 7,
            ResourceExhausted => 8,
            FailedPrecondition => 9,
            Aborted => 10,
            OutOfRange => 11,
            Unimplemented => 12,
            Internal => 13,
            Unavailable => 14,
            DataLoss => 15,
            Busy => 16,
            ShouldWait => 17,
        }
    }
}

impl core::fmt::Display for CoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl std::error::Error for CoreError {}

/// Result alias for core operations.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn codes_match_pinned_types_h() {
        // Ground truth: mojo/public/c/system/types.h (epoch 1).
        let expected = [
            (CoreError::Ok, 0),
            (CoreError::Cancelled, 1),
            (CoreError::Unknown, 2),
            (CoreError::InvalidArgument, 3),
            (CoreError::DeadlineExceeded, 4),
            (CoreError::NotFound, 5),
            (CoreError::AlreadyExists, 6),
            (CoreError::PermissionDenied, 7),
            (CoreError::ResourceExhausted, 8),
            (CoreError::FailedPrecondition, 9),
            (CoreError::Aborted, 10),
            (CoreError::OutOfRange, 11),
            (CoreError::Unimplemented, 12),
            (CoreError::Internal, 13),
            (CoreError::Unavailable, 14),
            (CoreError::DataLoss, 15),
            (CoreError::Busy, 16),
            (CoreError::ShouldWait, 17),
        ];
        for (e, code) in expected {
            assert_eq!(e.code(), code);
        }
    }
}
