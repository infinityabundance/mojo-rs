//! Wire error taxonomy.
//!
//! Rejection classes mirror the official bindings validator enum
//! (`mojo/public/cpp/bindings/lib/validation_errors.h`, pinned) so courts can
//! compare rejection CAUSES, not only accept/reject. Encode-side errors are
//! separate (they are not externally observable).

/// Exact official validation error classes (pinned epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValidationError {
    /// No error.
    None,
    /// An object is not aligned to 8 bytes.
    MisalignedObject,
    /// A referenced memory range is out of bounds or overlaps.
    IllegalMemoryRange,
    /// A struct header is invalid (size/version mismatch).
    UnexpectedStructHeader,
    /// An array header is invalid (size/element-count mismatch).
    UnexpectedArrayHeader,
    /// A handle index is out of range for the message.
    IllegalHandle,
    /// An invalid (sentinel) handle appeared where a real one was required.
    UnexpectedInvalidHandle,
    /// A relative pointer is misaligned or out of bounds.
    IllegalPointer,
    /// A non-nullable pointer is null.
    UnexpectedNullPointer,
    /// An interface id is invalid in context.
    IllegalInterfaceId,
    /// The primary/invalid interface id appeared unexpectedly.
    UnexpectedInvalidInterfaceId,
    /// Message flags are mutually exclusive.
    MessageHeaderInvalidFlags,
    /// Message requires a request id but the header version is too low.
    MessageHeaderMissingRequestId,
    /// The message name is unknown for the interface.
    MessageHeaderUnknownMethod,
    /// A map's key and value arrays differ in size.
    DifferentSizedArraysInMap,
    /// The union tag is unknown.
    UnknownUnionTag,
    /// The enum value is out of range.
    UnknownEnumValue,
    /// Deserialization failed for another reason.
    DeserializationFailed,
    /// The nesting depth exceeded the recursion limit.
    MaxRecursionDepth,
}

impl ValidationError {
    /// The official enum name (for court artifacts).
    pub fn name(self) -> &'static str {
        use ValidationError::*;
        match self {
            None => "VALIDATION_ERROR_NONE",
            MisalignedObject => "VALIDATION_ERROR_MISALIGNED_OBJECT",
            IllegalMemoryRange => "VALIDATION_ERROR_ILLEGAL_MEMORY_RANGE",
            UnexpectedStructHeader => "VALIDATION_ERROR_UNEXPECTED_STRUCT_HEADER",
            UnexpectedArrayHeader => "VALIDATION_ERROR_UNEXPECTED_ARRAY_HEADER",
            IllegalHandle => "VALIDATION_ERROR_ILLEGAL_HANDLE",
            UnexpectedInvalidHandle => "VALIDATION_ERROR_UNEXPECTED_INVALID_HANDLE",
            IllegalPointer => "VALIDATION_ERROR_ILLEGAL_POINTER",
            UnexpectedNullPointer => "VALIDATION_ERROR_UNEXPECTED_NULL_POINTER",
            IllegalInterfaceId => "VALIDATION_ERROR_ILLEGAL_INTERFACE_ID",
            UnexpectedInvalidInterfaceId => "VALIDATION_ERROR_UNEXPECTED_INVALID_INTERFACE_ID",
            MessageHeaderInvalidFlags => "VALIDATION_ERROR_MESSAGE_HEADER_INVALID_FLAGS",
            MessageHeaderMissingRequestId => "VALIDATION_ERROR_MESSAGE_HEADER_MISSING_REQUEST_ID",
            MessageHeaderUnknownMethod => "VALIDATION_ERROR_MESSAGE_HEADER_UNKNOWN_METHOD",
            DifferentSizedArraysInMap => "VALIDATION_ERROR_DIFFERENT_SIZED_ARRAYS_IN_MAP",
            UnknownUnionTag => "VALIDATION_ERROR_UNKNOWN_UNION_TAG",
            UnknownEnumValue => "VALIDATION_ERROR_UNKNOWN_ENUM_VALUE",
            DeserializationFailed => "VALIDATION_ERROR_DESERIALIZATION_FAILED",
            MaxRecursionDepth => "VALIDATION_ERROR_MAX_RECURSION_DEPTH",
        }
    }
}

/// A wire failure: either an exact official validation class or an
/// encode-side (not externally observable) error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireError {
    /// An exact official validation class.
    Validation(ValidationError),
    /// Encode-side errors (never observed by receivers).
    Encode(EncodeError),
}

/// Encode-side failures with structured detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodeError {
    /// A non-nullable object was null.
    UnexpectedNull,
    /// The message would exceed the configured maximum size.
    MessageTooLarge {
        /// Encoded size.
        size: usize,
        /// The configured limit.
        limit: usize,
    },
    /// The nesting depth exceeds the configured recursion limit.
    RecursionLimitExceeded {
        /// Depth at which the limit was exceeded.
        depth: usize,
    },
    /// A map's key and value arrays have different lengths.
    MapKeyCountMismatch {
        /// Key count.
        keys: usize,
        /// Value count.
        values: usize,
    },
    /// A string contains an embedded NUL (forbidden on the wire).
    EmbeddedNulInString,
    /// A union member value does not match any member type.
    InvalidUnionValue,
    /// An enum value is out of the declared range.
    InvalidEnumValue {
        /// The out-of-range value.
        value: i64,
    },
    /// An offset/pointer arithmetic overflowed.
    ArithmeticOverflow,
    /// A value did not conform to its declared type.
    TypeMismatch,
    /// An unsupported (future or not-yet-implemented) encoding was requested.
    Unsupported {
        /// Reason the encoding is unsupported.
        detail: &'static str,
    },
}

impl WireError {
    /// Convenience constructors for the official validation classes.
    /// Returns the official MISALIGNED_OBJECT class.
    pub fn misaligned_object() -> WireError {
        WireError::Validation(ValidationError::MisalignedObject)
    }
    /// Returns the official ILLEGAL_MEMORY_RANGE class.
    pub fn illegal_memory_range() -> WireError {
        WireError::Validation(ValidationError::IllegalMemoryRange)
    }
    /// Returns the official UNEXPECTED_STRUCT_HEADER class.
    pub fn unexpected_struct_header() -> WireError {
        WireError::Validation(ValidationError::UnexpectedStructHeader)
    }
    /// Returns the official UNEXPECTED_ARRAY_HEADER class.
    pub fn unexpected_array_header() -> WireError {
        WireError::Validation(ValidationError::UnexpectedArrayHeader)
    }
    /// Returns the official ILLEGAL_HANDLE class.
    pub fn illegal_handle() -> WireError {
        WireError::Validation(ValidationError::IllegalHandle)
    }
    /// Returns the official UNEXPECTED_INVALID_HANDLE class.
    pub fn unexpected_invalid_handle() -> WireError {
        WireError::Validation(ValidationError::UnexpectedInvalidHandle)
    }
    /// Returns the official ILLEGAL_POINTER class.
    pub fn illegal_pointer() -> WireError {
        WireError::Validation(ValidationError::IllegalPointer)
    }
    /// Returns the official UNEXPECTED_NULL_POINTER class.
    pub fn unexpected_null_pointer() -> WireError {
        WireError::Validation(ValidationError::UnexpectedNullPointer)
    }
    /// Returns the official ILLEGAL_INTERFACE_ID class.
    pub fn illegal_interface_id() -> WireError {
        WireError::Validation(ValidationError::IllegalInterfaceId)
    }
    /// Returns the official UNEXPECTED_INVALID_INTERFACE_ID class.
    pub fn unexpected_invalid_interface_id() -> WireError {
        WireError::Validation(ValidationError::UnexpectedInvalidInterfaceId)
    }
    /// Returns the official MESSAGE_HEADER_INVALID_FLAGS class.
    pub fn message_header_invalid_flags() -> WireError {
        WireError::Validation(ValidationError::MessageHeaderInvalidFlags)
    }
    /// Returns the official MESSAGE_HEADER_MISSING_REQUEST_ID class.
    pub fn message_header_missing_request_id() -> WireError {
        WireError::Validation(ValidationError::MessageHeaderMissingRequestId)
    }
    /// Returns the official DIFFERENT_SIZED_ARRAYS_IN_MAP class.
    pub fn different_sized_arrays_in_map() -> WireError {
        WireError::Validation(ValidationError::DifferentSizedArraysInMap)
    }
    /// Returns the official UNKNOWN_UNION_TAG class.
    pub fn unknown_union_tag() -> WireError {
        WireError::Validation(ValidationError::UnknownUnionTag)
    }
    /// Returns the official UNKNOWN_ENUM_VALUE class.
    pub fn unknown_enum_value() -> WireError {
        WireError::Validation(ValidationError::UnknownEnumValue)
    }
    /// Returns the official DESERIALIZATION_FAILED class.
    pub fn deserialization_failed() -> WireError {
        WireError::Validation(ValidationError::DeserializationFailed)
    }
    /// Returns the official MAX_RECURSION_DEPTH class.
    pub fn max_recursion_depth() -> WireError {
        WireError::Validation(ValidationError::MaxRecursionDepth)
    }
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WireError::Validation(v) => write!(f, "{}", v.name()),
            WireError::Encode(e) => match e {
                EncodeError::UnexpectedNull => write!(f, "encode: unexpected null"),
                EncodeError::MessageTooLarge { size, limit } => {
                    write!(f, "encode: message {size} bytes exceeds limit {limit}")
                }
                EncodeError::RecursionLimitExceeded { depth } => {
                    write!(f, "encode: recursion limit exceeded at {depth}")
                }
                EncodeError::MapKeyCountMismatch { keys, values } => {
                    write!(f, "encode: map key count {keys} != value count {values}")
                }
                EncodeError::EmbeddedNulInString => write!(f, "encode: embedded NUL in string"),
                EncodeError::InvalidUnionValue => write!(f, "encode: invalid union value"),
                EncodeError::InvalidEnumValue { value } => {
                    write!(f, "encode: invalid enum value {value}")
                }
                EncodeError::ArithmeticOverflow => write!(f, "encode: arithmetic overflow"),
                EncodeError::TypeMismatch => write!(f, "encode: value/type mismatch"),
                EncodeError::Unsupported { detail } => write!(f, "encode: unsupported: {detail}"),
            },
        }
    }
}

impl std::error::Error for WireError {}

/// Result alias for wire operations.
pub type WireResult<T> = Result<T, WireError>;
