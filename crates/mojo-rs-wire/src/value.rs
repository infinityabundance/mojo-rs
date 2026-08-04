//! Typed value model, encoder, and decoder for the Mojo wire format.
//!
//! The encoder produces the exact bytes the official implementation produces;
//! the decoder validates (alignment, bounds, overlap, recursion, handle
//! indices) and produces typed values. Both directions are schema-driven
//! (`Type`) so the same layout logic serves encode and decode.

use crate::error::{EncodeError, ValidationError, WireError, WireResult};
use crate::layout;
use crate::pack::{Field, Kind, PackedStruct};
use crate::pointer::Pointer;

/// Schema for a Mojom type.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Boolean.
    Bool,
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 16-bit integer.
    I16,
    /// Unsigned 16-bit integer.
    U16,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit float.
    F32,
    /// 64-bit float.
    F64,
    /// Enum.
    Enum,
    /// Handle-like kinds (message pipe, shared buffer, platform handle, ...).
    Handle,
    /// String (8-byte relative pointer).
    String {
        /// Whether nullable.
        nullable: bool,
    },
    /// Array (8-byte relative pointer).
    Array {
        /// Element type.
        element: Box<Type>,
        /// Whether nullable.
        nullable: bool,
    },
    /// Map (8-byte relative pointer).
    Map {
        /// Key type.
        key: Box<Type>,
        /// Value type.
        value: Box<Type>,
        /// Whether nullable.
        nullable: bool,
    },
    /// Struct (inline, or pointer when nullable).
    Struct {
        /// Fields in declaration order.
        fields: Vec<FieldType>,
        /// Version.
        version: u32,
        /// Whether nullable.
        nullable: bool,
    },
    /// Union (16 bytes inline).
    Union {
        /// Members in tag order.
        members: Vec<UnionMember>,
        /// Whether the union may be null (nullable unions are non-inlined).
        nullable: bool,
        /// Inlined unions occupy 16 bytes at the field; non-inlined unions are
        /// an 8-byte pointer to a 16-byte union object. The mojom compiler
        /// decides per the official rule; the wire layer honors it.
        inlined: bool,
    },
    /// Interface (8 bytes: handle + version).
    Interface {
        /// Interface version.
        version: u32,
    },
    /// An associated endpoint (4-byte id).
    AssociatedEndpoint,
    /// Associated interface (8 bytes: id + version).
    AssociatedInterface {
        /// Interface version.
        version: u32,
    },
    /// A nullable VALUE kind (e.g. `int32?`): encoded as a bool flag plus the
    /// non-nullable value (wire-compat expansion).
    NullableScalar(Box<Type>),
}

/// A struct field in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldType {
    /// Field name (diagnostics only; the wire carries no names).
    pub name: &'static str,
    /// The type.
    pub ty: Type,
    /// Explicit [MinVersion], if any.
    pub min_version: Option<u32>,
}

/// A union member.
#[derive(Debug, Clone, PartialEq)]
pub struct UnionMember {
    /// Member name (diagnostics only).
    pub name: &'static str,
    /// The type.
    pub ty: Type,
}

impl Type {
    /// The pack kind for this type.
    pub fn pack_kind(&self) -> Kind {
        match self {
            Type::Bool => Kind::Bool,
            Type::I8 => Kind::I8,
            Type::U8 => Kind::U8,
            Type::I16 => Kind::I16,
            Type::U16 => Kind::U16,
            Type::I32 => Kind::I32,
            Type::U32 => Kind::U32,
            Type::I64 => Kind::I64,
            Type::U64 => Kind::U64,
            Type::F32 => Kind::F32,
            Type::F64 => Kind::F64,
            Type::Enum => Kind::Enum,
            Type::Handle => Kind::Handle,
            Type::String { .. } => Kind::String,
            Type::Array { .. } => Kind::Array,
            Type::Map { .. } => Kind::Map,
            Type::Struct { .. } => Kind::Struct,
            Type::Union { inlined: true, .. } => Kind::Union,
            Type::Union { inlined: false, .. } => Kind::Map,
            Type::Interface { .. } => Kind::Interface,
            Type::AssociatedEndpoint => Kind::PendingAssociatedReceiver,
            Type::AssociatedInterface { .. } => Kind::PendingAssociatedRemote,
            Type::NullableScalar(inner) => inner.pack_kind(),
        }
    }

    /// Whether this type is a reference kind (pointer-encoded).
    pub fn is_reference(&self) -> bool {
        matches!(
            self,
            Type::String { .. }
                | Type::Array { .. }
                | Type::Map { .. }
                | Type::Struct { .. }
                | Type::Interface { .. }
        )
    }

    /// Whether this type is nullable.
    pub fn is_nullable(&self) -> bool {
        match self {
            Type::String { nullable }
            | Type::Array { nullable, .. }
            | Type::Map { nullable, .. } => *nullable,
            Type::Struct { nullable, .. } | Type::Union { nullable, .. } => *nullable,
            Type::NullableScalar(_) => true,
            _ => false,
        }
    }
}

/// A typed value conforming to a [`Type`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Boolean.
    Bool(bool),
    /// Signed 8-bit integer.
    I8(i8),
    /// Unsigned 8-bit integer.
    U8(u8),
    /// Signed 16-bit integer.
    I16(i16),
    /// Unsigned 16-bit integer.
    U16(u16),
    /// Signed 32-bit integer.
    I32(i32),
    /// Unsigned 32-bit integer.
    U32(u32),
    /// Signed 64-bit integer.
    I64(i64),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// f32 bit pattern.
    F32(u32),
    /// f64 bit pattern.
    F64(u64),
    /// Enum value (i64 on the wire as i32).
    Enum(i64),
    /// String.
    String(String),
    /// Array of values.
    Array(Vec<Value>),
    /// Map: parallel key/value arrays.
    Map {
        /// Keys.
        keys: Vec<Value>,
        /// Values (same length as keys).
        values: Vec<Value>,
    },
    /// Struct with per-version fields.
    Struct {
        /// Version of the encoded struct.
        version: u32,
        /// Field values in declaration order.
        fields: Vec<Value>,
    },
    /// Union with a tag and member value.
    Union {
        /// Member tag.
        tag: u32,
        /// Member value.
        value: Box<Value>,
    },
    /// A handle index into the message's attached handle list.
    Handle {
        /// Index into the attached handle list.
        index: u32,
    },
    /// An interface endpoint (handle index + version).
    Interface {
        /// Index into the attached handle list.
        index: u32,
        /// Interface version.
        version: u32,
    },
    /// An associated endpoint id.
    AssociatedEndpoint {
        /// Associated endpoint id.
        id: u32,
    },
    /// An associated interface (id + version).
    AssociatedInterface {
        /// Associated endpoint id.
        id: u32,
        /// Interface version.
        version: u32,
    },
    /// Null for a nullable slot.
    Null,
    /// Nullable scalar: (present, value).
    NullableScalar {
        /// Whether the scalar is present.
        present: bool,
        /// The value when present, else Null.
        value: Box<Value>,
    },
}

/// A fully encoded message: payload bytes plus the ordered handle list.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedMessage {
    /// The exact wire bytes (header + payload).
    pub bytes: Vec<u8>,
    /// Number of handles attached (indices 0..n are valid).
    pub handle_count: usize,
}

const MAX_ENCODE_DEPTH: usize = 64;

/// Little-endian writer macro for primitives with `.to_le_bytes()`.
macro_rules! write_le {
    ($enc:expr, $offset:expr, $v:expr) => {{
        let bytes = $v.to_le_bytes();
        let offset = $offset;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > $enc.buf.len() {
            $enc.buf.resize(end, 0);
        }
        $enc.buf[offset..end].copy_from_slice(&bytes);
        Ok(())
    }};
}

/// Encodes a payload struct (per `ty`, which must be `Type::Struct`) plus an
/// optional base header into exact wire bytes.
pub fn encode_message(
    header: &[u8],
    ty: &Type,
    value: &Value,
    handle_count_hint: usize,
) -> WireResult<EncodedMessage> {
    let Type::Struct { .. } = ty else {
        return Err(WireError::Encode(EncodeError::Unsupported {
            detail: "payload must be a struct",
        }));
    };
    let mut enc = Encoder {
        buf: header.to_vec(),
        handles: Vec::with_capacity(handle_count_hint),
        depth: 0,
    };
    let payload_offset = layout::align_up(enc.buf.len(), layout::MESSAGE_ALIGNMENT)
        .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
    if payload_offset > enc.buf.len() {
        enc.buf.resize(payload_offset, 0);
    }
    enc.encode_struct_at(payload_offset, ty, value)?;

    // Patch the v2+ header payload pointer.
    if header.len() >= layout::MESSAGE_HEADER_V2_SIZE {
        let ptr_addr = layout::HEADER_PAYLOAD_OFFSET as u64;
        let raw = Pointer::encode(ptr_addr, payload_offset as u64)?;
        enc.buf[layout::HEADER_PAYLOAD_OFFSET..layout::HEADER_PAYLOAD_OFFSET + 8]
            .copy_from_slice(&raw.to_le_bytes());
    }

    Ok(EncodedMessage {
        bytes: enc.buf,
        handle_count: enc.handles.len(),
    })
}

struct Encoder {
    buf: Vec<u8>,
    handles: Vec<()>,
    depth: usize,
}

impl Encoder {
    fn enter(&mut self) -> WireResult<()> {
        self.depth += 1;
        if self.depth > MAX_ENCODE_DEPTH {
            return Err(WireError::Encode(EncodeError::RecursionLimitExceeded {
                depth: self.depth,
            }));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Write a struct value at a specific offset.
    fn encode_struct_at(&mut self, offset: usize, ty: &Type, value: &Value) -> WireResult<()> {
        let (fields, version) = match (ty, value) {
            (Type::Struct { fields, .. }, Value::Struct { version, .. }) => (fields, *version),
            (Type::Struct { nullable: true, .. }, Value::Null) => {
                return Err(WireError::Encode(EncodeError::UnexpectedNull));
            }
            _ => {
                return Err(WireError::Encode(EncodeError::Unsupported {
                    detail: "value/type mismatch in struct encode",
                }));
            }
        };
        let _ = version;

        let packed = self.pack_struct(fields)?;
        let size = packed.size;
        let end = offset
            .checked_add(size)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        // Struct header.
        write_le!(self, offset, size as u32)?;
        write_le!(self, offset + 4, version)?;

        // Field values by declaration index.
        let values = match value {
            Value::Struct { fields, .. } => fields.as_slice(),
            _ => &[],
        };

        for pf in &packed.packed_fields {
            let idx = pf.field.index;
            let val = values.get(idx).unwrap_or(&Value::Null);
            let field_ty = &fields[idx].ty;
            // Pack offsets are payload-relative: the 8-byte struct header
            // precedes the first field byte.
            let field_off = offset + crate::pack::STRUCT_HEADER_SIZE + pf.offset;

            if pf.field.nullable_value_kind {
                // Two packed fields per mojom field: BOOL flag + value.
                if pf.field.kind == Kind::Bool {
                    // Flag field.
                    let present = matches!(val, Value::NullableScalar { present: true, .. });
                    let byte = self.buf[field_off];
                    let mask = 1u8 << pf.bit;
                    self.buf[field_off] = if present { byte | mask } else { byte & !mask };
                } else {
                    // Value field.
                    if let Value::NullableScalar {
                        present: true,
                        value,
                    } = val
                    {
                        self.encode_field_at(field_off, 0, &unbox_scalar(field_ty), value)?;
                    }
                    // absent -> zero bytes already in place
                }
            } else {
                self.encode_field_at(field_off, pf.bit, field_ty, val)?;
            }
        }
        Ok(())
    }

    fn pack_struct(&self, fields: &[FieldType]) -> WireResult<PackedStruct> {
        let pack_fields: Vec<Field> = fields
            .iter()
            .enumerate()
            .map(|(index, ft)| Field {
                index,
                ordinal: index as u32,
                min_version: ft.min_version,
                kind: ft.ty.pack_kind(),
                nullable_value_kind: matches!(ft.ty, Type::NullableScalar(_)),
                nullable_reference: ft.ty.is_nullable(),
                mojom_name: ft.name,
            })
            .collect();
        PackedStruct::pack(pack_fields)
    }

    fn encode_field_at(
        &mut self,
        offset: usize,
        bit: u8,
        ty: &Type,
        value: &Value,
    ) -> WireResult<()> {
        match (ty, value) {
            (Type::Bool, Value::Bool(b)) => {
                let byte = self.buf[offset];
                let mask = 1u8 << bit;
                self.buf[offset] = if *b { byte | mask } else { byte & !mask };
                Ok(())
            }
            (Type::I8, Value::I8(v)) => write_le!(self, offset, *v),
            (Type::U8, Value::U8(v)) => write_le!(self, offset, *v),
            (Type::I16, Value::I16(v)) => write_le!(self, offset, *v),
            (Type::U16, Value::U16(v)) => write_le!(self, offset, *v),
            (Type::I32, Value::I32(v)) => write_le!(self, offset, *v),
            (Type::U32, Value::U32(v)) => write_le!(self, offset, *v),
            (Type::I64, Value::I64(v)) => write_le!(self, offset, *v),
            (Type::U64, Value::U64(v)) => write_le!(self, offset, *v),
            (Type::F32, Value::F32(v)) => write_le!(self, offset, *v),
            (Type::F64, Value::F64(v)) => write_le!(self, offset, *v),
            (Type::Enum, Value::Enum(v)) => write_le!(self, offset, *v as i32),
            (Type::Handle, Value::Handle { index }) => {
                let encoded = if *index == layout::ENCODED_INVALID_HANDLE_VALUE {
                    layout::ENCODED_INVALID_HANDLE_VALUE
                } else {
                    self.handles.push(());
                    *index
                };
                write_le!(self, offset, encoded)
            }
            (Type::String { nullable: false }, Value::String(s)) => self.encode_string(offset, s),
            (Type::String { nullable: true }, Value::String(s)) => self.encode_string(offset, s),
            (Type::String { nullable: true }, Value::Null) => write_le!(self, offset, 0u64),
            (Type::Array { element, .. }, Value::Array(items)) => {
                self.encode_array(offset, element, items)
            }
            (Type::Array { nullable: true, .. }, Value::Null) => write_le!(self, offset, 0u64),
            (Type::Map { key, value, .. }, Value::Map { keys, values }) => {
                self.encode_map(offset, key, value, keys, values)
            }
            (Type::Map { nullable: true, .. }, Value::Null) => write_le!(self, offset, 0u64),
            (
                Type::Struct {
                    nullable: false, ..
                },
                Value::Struct { .. },
            ) => {
                // Non-nullable structs are encoded INLINE (not pointers).
                self.encode_struct_at(offset, ty, value)
            }
            (Type::Struct { nullable: true, .. }, Value::Struct { .. }) => {
                let target = self.append_object_start()?;
                self.encode_struct_at(target, ty, value)?;
                let raw = Pointer::encode(offset as u64, target as u64)?;
                write_le!(self, offset, raw)
            }
            (Type::Struct { nullable: true, .. }, Value::Null) => write_le!(self, offset, 0u64),
            (
                Type::Union {
                    members,
                    inlined: true,
                    ..
                },
                Value::Union { tag, value },
            ) => {
                let member = members
                    .get(*tag as usize)
                    .ok_or(WireError::Encode(EncodeError::InvalidUnionValue))?;
                self.encode_union_inline(offset, *tag, &member.ty, value)
            }
            (
                Type::Union {
                    members, nullable, ..
                },
                Value::Union { tag, value },
            ) => {
                // Non-inlined union: pointer to a 16-byte union object.
                let member = members
                    .get(*tag as usize)
                    .ok_or(WireError::Encode(EncodeError::InvalidUnionValue))?;
                let target = self.append_object_start()?;
                self.encode_union_inline(target, *tag, &member.ty, value)?;
                let raw = Pointer::encode(offset as u64, target as u64)?;
                write_le!(self, offset, raw)
            }
            (Type::Union { nullable: true, .. }, Value::Null) => write_le!(self, offset, 0u64),
            (Type::Interface { version }, Value::Interface { index, version: v }) => {
                let encoded = if *index == layout::ENCODED_INVALID_HANDLE_VALUE {
                    layout::ENCODED_INVALID_HANDLE_VALUE
                } else {
                    self.handles.push(());
                    *index
                };
                write_le!(self, offset, encoded)?;
                write_le!(self, offset + 4, v.max(version))
            }
            (Type::AssociatedEndpoint, Value::AssociatedEndpoint { id }) => {
                write_le!(self, offset, *id)
            }
            (
                Type::AssociatedInterface { version },
                Value::AssociatedInterface { id, version: v },
            ) => {
                write_le!(self, offset, *id)?;
                write_le!(self, offset + 4, v.max(version))
            }
            (
                Type::NullableScalar(inner),
                Value::NullableScalar {
                    present: true,
                    value,
                },
            ) => self.encode_field_at(offset, bit, inner, value),
            (Type::NullableScalar(_), Value::NullableScalar { present: false, .. }) => Ok(()),
            (Type::NullableScalar(_), Value::Null) => Ok(()),
            _ => Err(WireError::Encode(EncodeError::Unsupported {
                detail: "value/type mismatch",
            })),
        }
    }

    /// Encode a union inline at `offset` (always 16 bytes on the wire).
    fn encode_union_inline(
        &mut self,
        offset: usize,
        tag: u32,
        member_ty: &Type,
        value: &Value,
    ) -> WireResult<()> {
        let end = offset
            .checked_add(16)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        write_le!(self, offset, layout::UNION_DATA_SIZE as u32)?;
        write_le!(self, offset + 4, tag)?;
        // The 8-byte inline payload: PODs inline; reference kinds hold a
        // relative pointer to the object.
        let data_off = offset + 8;
        self.encode_union_payload(data_off, member_ty, value)
    }

    fn encode_union_payload(
        &mut self,
        data_off: usize,
        member_ty: &Type,
        value: &Value,
    ) -> WireResult<()> {
        match (member_ty, value) {
            (
                Type::Struct {
                    nullable: false, ..
                },
                Value::Struct { .. },
            ) => {
                // Nested structs are encoded inline in the union payload.
                self.encode_struct_at(data_off, member_ty, value)
            }
            (Type::String { .. }, Value::String(_)) => {
                self.encode_string(data_off, &string_of(value))
            }
            (Type::Array { .. }, Value::Array(_)) | (Type::Map { .. }, Value::Map { .. }) => {
                // Pointer to the object.
                let target = self.append_object_start()?;
                match value {
                    Value::Array(items) => {
                        let Type::Array { element, .. } = member_ty else {
                            unreachable!()
                        };
                        self.encode_array_body(target, element, items)?;
                    }
                    Value::Map { keys, values } => {
                        let Type::Map { key, value, .. } = member_ty else {
                            unreachable!()
                        };
                        self.encode_map_at(target, key, value, keys, values)?;
                    }
                    _ => unreachable!(),
                }
                let raw = Pointer::encode(data_off as u64, target as u64)?;
                write_le!(self, data_off, raw)
            }
            _ => self.encode_field_at(data_off, 0, member_ty, value),
        }
    }

    /// Write a string OBJECT (StringHeader + data + NUL) at `target`.
    fn encode_string_object_at(&mut self, target: usize, s: &str) -> WireResult<()> {
        let data_len = s.len();
        let total = layout::align_up(
            layout::ARRAY_HEADER_SIZE + data_len + 1,
            layout::MESSAGE_ALIGNMENT,
        )
        .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let end = target
            .checked_add(total)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        write_le!(self, target, total as u32)?;
        write_le!(self, target + 4, data_len as u32)?;
        self.buf[target + 8..target + 8 + data_len].copy_from_slice(s.as_bytes());
        self.buf[target + 8 + data_len] = 0; // NUL
        Ok(())
    }

    /// Write a string pointer slot at `offset` pointing to a fresh object.
    fn encode_string(&mut self, offset: usize, s: &str) -> WireResult<()> {
        let target = self.append_object_start()?;
        self.encode_string_object_at(target, s)?;
        let raw = Pointer::encode(offset as u64, target as u64)?;
        write_le!(self, offset, raw)
    }

    fn encode_array(&mut self, offset: usize, element: &Type, items: &[Value]) -> WireResult<()> {
        let target = self.append_object_start()?;
        self.enter()?;
        let r = self.encode_array_body(target, element, items);
        self.leave();
        r?;
        let raw = Pointer::encode(offset as u64, target as u64)?;
        write_le!(self, offset, raw)
    }

    fn encode_array_body(
        &mut self,
        target: usize,
        element: &Type,
        items: &[Value],
    ) -> WireResult<()> {
        let is_pod = matches!(
            element,
            Type::Bool
                | Type::I8
                | Type::U8
                | Type::I16
                | Type::U16
                | Type::I32
                | Type::U32
                | Type::I64
                | Type::U64
                | Type::F32
                | Type::F64
                | Type::Enum
                | Type::Handle
        );
        let elem_size = if is_pod {
            element.pack_kind().size()
        } else {
            layout::POINTER_SIZE
        };
        let raw = elem_size
            .checked_mul(items.len())
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let total = layout::align_up(layout::ARRAY_HEADER_SIZE + raw, layout::MESSAGE_ALIGNMENT)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let end = target
            .checked_add(total)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        write_le!(self, target, total as u32)?;
        write_le!(self, target + 4, items.len() as u32)?;

        let data_off = target + layout::ARRAY_HEADER_SIZE;
        for (i, item) in items.iter().enumerate() {
            let item_off = data_off + i * elem_size;
            if is_pod {
                self.encode_field_at(item_off, 0, element, item)?;
            } else {
                match item {
                    Value::Null => write_le!(self, item_off, 0u64)?,
                    _ => {
                        let o = self.append_object_start()?;
                        self.encode_array_element_at(o, element, item)?;
                        let raw = Pointer::encode(item_off as u64, o as u64)?;
                        write_le!(self, item_off, raw)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn encode_array_element_at(
        &mut self,
        o: usize,
        element: &Type,
        item: &Value,
    ) -> WireResult<()> {
        match element {
            Type::Struct {
                nullable: false, ..
            } => self.encode_struct_at(o, element, item),
            Type::Struct { nullable: true, .. } => match item {
                Value::Null => Ok(()),
                _ => self.encode_struct_at(o, element, item),
            },
            Type::String { .. } => self.encode_string_object_at(o, &string_of(item)),
            Type::Array { element: inner, .. } => self.encode_array_body(o, inner, &array_of(item)),
            Type::Map { key, value, .. } => match item {
                Value::Map { keys, values } => self.encode_map_at(o, key, value, keys, values),
                _ => Err(WireError::Encode(EncodeError::TypeMismatch)),
            },
            _ => Err(WireError::Encode(EncodeError::Unsupported {
                detail: "array element encode",
            })),
        }
    }

    fn encode_map(
        &mut self,
        offset: usize,
        key: &Type,
        value: &Type,
        keys: &[Value],
        values: &[Value],
    ) -> WireResult<()> {
        let target = self.append_object_start()?;
        self.encode_map_at(target, key, value, keys, values)?;
        let raw = Pointer::encode(offset as u64, target as u64)?;
        write_le!(self, offset, raw)
    }

    fn encode_map_at(
        &mut self,
        target: usize,
        key: &Type,
        value: &Type,
        keys: &[Value],
        values: &[Value],
    ) -> WireResult<()> {
        if keys.len() != values.len() {
            return Err(WireError::Encode(EncodeError::MapKeyCountMismatch {
                keys: keys.len(),
                values: values.len(),
            }));
        }
        let end = target
            .checked_add(24)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        write_le!(self, target, 24u32)?;
        write_le!(self, target + 4, 0u32)?; // version
        self.enter()?;
        let r = (|| -> WireResult<()> {
            let ko = self.append_object_start()?;
            self.encode_array_body(ko, key, keys)?;
            let rawk = Pointer::encode((target + 8) as u64, ko as u64)?;
            write_le!(self, target + 8, rawk)?;
            let vo = self.append_object_start()?;
            self.encode_array_body(vo, value, values)?;
            let rawv = Pointer::encode((target + 16) as u64, vo as u64)?;
            write_le!(self, target + 16, rawv)?;
            Ok(())
        })();
        self.leave();
        r
    }

    /// Reserve 8-aligned space at the end of the buffer and return its offset.
    fn append_object_start(&mut self) -> WireResult<usize> {
        let aligned = layout::align_up(self.buf.len(), layout::MESSAGE_ALIGNMENT)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if aligned > self.buf.len() {
            self.buf.resize(aligned, 0);
        }
        Ok(aligned)
    }
}

fn string_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn array_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    }
}

fn unbox_scalar(ty: &Type) -> Type {
    match ty {
        Type::NullableScalar(inner) => (**inner).clone(),
        t => t.clone(),
    }
}

// ---------------------------------------------------------------------------
// Decoder + validator
// ---------------------------------------------------------------------------

/// Decoding/validation context over one message buffer.
pub struct Decoder<'a> {
    bytes: &'a [u8],
    handle_count: usize,
    /// The validation frontier (`data_begin_` in the official
    /// `ValidationContext`): claims must start at or after this offset and
    /// advance it to the claim end. Duplicate and out-of-order references are
    /// rejected exactly like the official validator.
    frontier: usize,
    depth: usize,
    pub(crate) max_depth: usize,
}

impl<'a> Decoder<'a> {
    /// Create a decoder over a message buffer with `handle_count` attached handles.
    pub fn new(bytes: &'a [u8], handle_count: usize) -> Self {
        Decoder {
            bytes,
            handle_count,
            frontier: 0,
            depth: 0,
            max_depth: layout::DEFAULT_MAX_RECURSION_DEPTH,
        }
    }

    /// Claim a memory range per the official moving-frontier rule
    /// (`InternalIsValidRange` + `data_begin_ = end`): the claim must be
    /// non-empty, start at or after the frontier, and end within the message.
    fn claim(&mut self, start: usize, len: usize) -> WireResult<()> {
        let end = start
            .checked_add(len)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if end > self.bytes.len() {
            eprintln!(
                "DBG claim oob start={start} end={end} len={}",
                self.bytes.len()
            );
            return Err(WireError::illegal_memory_range());
        }
        if end <= start {
            return Err(WireError::illegal_memory_range());
        }
        if start < self.frontier {
            eprintln!(
                "DBG claim out-of-order start={start} frontier={}",
                self.frontier
            );
            return Err(WireError::illegal_memory_range());
        }
        self.frontier = end;
        Ok(())
    }

    fn enter(&mut self) -> WireResult<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(WireError::max_recursion_depth());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn read_u16(&self, offset: usize) -> WireResult<u16> {
        let end = offset
            .checked_add(2)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let bytes = self
            .bytes
            .get(offset..end)
            .ok_or(WireError::illegal_memory_range())?;
        let mut b = [0u8; 2];
        b.copy_from_slice(bytes);
        Ok(u16::from_le_bytes(b))
    }

    fn read_u32(&self, offset: usize) -> WireResult<u32> {
        let end = offset
            .checked_add(4)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let bytes = self
            .bytes
            .get(offset..end)
            .ok_or(WireError::illegal_memory_range())?;
        let mut b = [0u8; 4];
        b.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(b))
    }

    fn read_u64(&self, offset: usize) -> WireResult<u64> {
        let end = offset
            .checked_add(8)
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        let bytes = self
            .bytes
            .get(offset..end)
            .ok_or(WireError::illegal_memory_range())?;
        let mut b = [0u8; 8];
        b.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(b))
    }

    fn read_i64(&self, offset: usize) -> WireResult<i64> {
        Ok(self.read_u64(offset)? as i64)
    }

    /// Decode and validate a struct payload at `offset` per `ty`, claiming
    /// its memory (top-level payload and pointer-referenced structs).
    pub fn decode_struct(&mut self, offset: usize, ty: &Type) -> WireResult<Value> {
        let Type::Struct { fields, .. } = ty else {
            return Err(WireError::Encode(EncodeError::Unsupported {
                detail: "expected struct type",
            }));
        };
        self.enter()?;
        let r = self.decode_struct_body(offset, fields, true);
        self.leave();
        r
    }

    /// Decode an INLINE (non-nullable) struct field: header and fields are
    /// validated but the memory is NOT claimed (the enclosing struct's claim
    /// covers it), matching the official moving-frontier validator.
    fn decode_struct_inline(&mut self, offset: usize, ty: &Type) -> WireResult<Value> {
        let Type::Struct { fields, .. } = ty else {
            return Err(WireError::Encode(EncodeError::Unsupported {
                detail: "expected struct type",
            }));
        };
        self.enter()?;
        let r = self.decode_struct_body(offset, fields, false);
        self.leave();
        r
    }

    fn decode_struct_body(
        &mut self,
        offset: usize,
        fields: &[FieldType],
        claim_self: bool,
    ) -> WireResult<Value> {
        if offset + layout::STRUCT_HEADER_SIZE > self.bytes.len() {
            return Err(WireError::illegal_memory_range());
        }
        let num_bytes = self.read_u32(offset)? as usize;
        let version = self.read_u32(offset + 4)?;
        if num_bytes < layout::STRUCT_HEADER_SIZE {
            return Err(WireError::unexpected_struct_header());
        }
        if offset + num_bytes > self.bytes.len() {
            return Err(WireError::illegal_memory_range());
        }
        if claim_self {
            self.claim(offset, num_bytes)?;
        }

        let pack_fields: Vec<Field> = fields
            .iter()
            .enumerate()
            .map(|(index, ft)| Field {
                index,
                ordinal: index as u32,
                min_version: ft.min_version,
                kind: ft.ty.pack_kind(),
                nullable_value_kind: matches!(ft.ty, Type::NullableScalar(_)),
                nullable_reference: ft.ty.is_nullable(),
                mojom_name: ft.name,
            })
            .collect();
        let packed = PackedStruct::pack(pack_fields)?;

        // Exact version/size matrix (ValidateStructHeaderAndVersionSize...):
        // for version <= max, num_bytes must EQUAL the version's size; for
        // future versions, num_bytes must be >= the maximum known size.
        let max_version = fields
            .iter()
            .filter_map(|f| f.min_version)
            .max()
            .unwrap_or(0);
        if version <= max_version {
            if let Some(expected) = packed.num_bytes_for_version(version) {
                if num_bytes != expected {
                    return Err(WireError::unexpected_struct_header());
                }
            }
        } else if num_bytes < packed.size {
            return Err(WireError::unexpected_struct_header());
        }

        // Decode per packed field; nullable-value fields need both halves.
        let mut scalars: Vec<Value> = fields.iter().map(default_value).collect();
        let mut presence: Vec<bool> = vec![false; fields.len()];

        for pf in &packed.packed_fields {
            let idx = pf.field.index;
            let in_version = pf.min_version <= version;
            // Pack offsets are payload-relative; add the struct header size.
            let abs_offset = crate::pack::STRUCT_HEADER_SIZE + pf.offset;
            let in_bounds = abs_offset + pf.size <= num_bytes;
            if !in_version || !in_bounds {
                continue;
            }
            let field_off = offset + abs_offset;
            let ft = &fields[idx];
            if pf.field.nullable_value_kind {
                if pf.field.kind == Kind::Bool {
                    let byte = self
                        .bytes
                        .get(field_off)
                        .copied()
                        .ok_or(WireError::illegal_memory_range())?;
                    presence[idx] = (byte >> pf.bit) & 1 != 0;
                } else {
                    scalars[idx] = self.decode_field(field_off, 0, &unbox_scalar(&ft.ty))?;
                }
            } else {
                scalars[idx] = self.decode_field(field_off, pf.bit, &ft.ty)?;
            }
        }

        // Assemble: nullable-value fields wrap presence + scalar.
        let mut values = Vec::with_capacity(fields.len());
        for (i, ft) in fields.iter().enumerate() {
            if matches!(ft.ty, Type::NullableScalar(_)) {
                let present = presence[i];
                let value = scalars[i].clone();
                values.push(Value::NullableScalar {
                    present,
                    value: Box::new(if present { value } else { Value::Null }),
                });
            } else {
                values.push(scalars[i].clone());
            }
        }

        Ok(Value::Struct {
            version,
            fields: values,
        })
    }

    fn decode_field(&mut self, offset: usize, bit: u8, ty: &Type) -> WireResult<Value> {
        match ty {
            Type::Bool => {
                let byte = self
                    .bytes
                    .get(offset)
                    .copied()
                    .ok_or(WireError::illegal_memory_range())?;
                Ok(Value::Bool((byte >> bit) & 1 != 0))
            }
            Type::I8 => Ok(Value::I8(
                self.bytes
                    .get(offset)
                    .copied()
                    .ok_or(WireError::illegal_memory_range())? as i8,
            )),
            Type::U8 => Ok(Value::U8(
                self.bytes
                    .get(offset)
                    .copied()
                    .ok_or(WireError::illegal_memory_range())?,
            )),
            Type::I16 => Ok(Value::I16(self.read_u16(offset)? as i16)),
            Type::U16 => Ok(Value::U16(self.read_u16(offset)?)),
            Type::I32 => Ok(Value::I32(self.read_u32(offset)? as i32)),
            Type::U32 => Ok(Value::U32(self.read_u32(offset)?)),
            Type::I64 => Ok(Value::I64(self.read_i64(offset)?)),
            Type::U64 => Ok(Value::U64(self.read_u64(offset)?)),
            Type::F32 => Ok(Value::F32(self.read_u32(offset)?)),
            Type::F64 => Ok(Value::F64(self.read_u64(offset)?)),
            Type::Enum => Ok(Value::Enum(self.read_u32(offset)? as i64)),
            Type::Handle => {
                let idx = self.read_u32(offset)?;
                if idx != layout::ENCODED_INVALID_HANDLE_VALUE && idx as usize >= self.handle_count
                {
                    return Err(WireError::illegal_handle());
                }
                Ok(Value::Handle { index: idx })
            }
            Type::String { nullable: false } => self.decode_string(offset, false),
            Type::String { nullable: true } => self.decode_string(offset, true),
            Type::Array { element, nullable } => self.decode_array(offset, element, *nullable),
            Type::Map {
                key,
                value,
                nullable,
            } => self.decode_map(offset, key, value, *nullable),
            Type::Struct {
                nullable: false, ..
            } => self.decode_struct_inline(offset, ty),
            Type::Struct { nullable: true, .. } => {
                let raw = self.read_u64(offset)?;
                let ptr = Pointer::decode(offset as u64, raw)?;
                match ptr {
                    Pointer::Null => Ok(Value::Null),
                    Pointer::Offset(target) => {
                        let t =
                            usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
                        self.decode_struct(t, ty)
                    }
                }
            }
            Type::Union { inlined: true, .. } => {
                // Inlined union: 16 bytes at the field offset, NOT claimed
                // (covered by the enclosing struct's claim).
                self.decode_union_at(offset, ty, false)
            }
            Type::Union { inlined: false, .. } => {
                // Non-inlined union: a pointer to a 16-byte union object.
                let raw = self.read_u64(offset)?;
                let ptr = Pointer::decode(offset as u64, raw)?;
                match ptr {
                    Pointer::Null => {
                        if ty.is_nullable() {
                            Ok(Value::Null)
                        } else {
                            Err(WireError::unexpected_null_pointer())
                        }
                    }
                    Pointer::Offset(target) => {
                        let t =
                            usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
                        self.decode_union_at(t, ty, true)
                    }
                }
            }
            Type::Interface { version } => {
                let idx = self.read_u32(offset)?;
                let v = self.read_u32(offset + 4)?;
                if idx != layout::ENCODED_INVALID_HANDLE_VALUE && idx as usize >= self.handle_count
                {
                    return Err(WireError::illegal_handle());
                }
                Ok(Value::Interface {
                    index: idx,
                    version: v.max(*version),
                })
            }
            Type::AssociatedEndpoint => Ok(Value::AssociatedEndpoint {
                id: self.read_u32(offset)?,
            }),
            Type::AssociatedInterface { version } => {
                let id = self.read_u32(offset)?;
                let v = self.read_u32(offset + 4)?;
                Ok(Value::AssociatedInterface {
                    id,
                    version: v.max(*version),
                })
            }
            Type::NullableScalar(inner) => self.decode_field(offset, bit, inner),
        }
    }

    /// Decode a union object at `target` (the union is inline at `target`).
    /// Decode a union at `target`. `claim_self` is true for non-inlined
    /// (pointer-referenced) unions: exactly kUnionDataSize is claimed and the
    /// size field must equal kUnionDataSize (official
    /// ValidateNonInlinedUnionHeaderAndClaimMemory).
    fn decode_union_at(&mut self, target: usize, ty: &Type, claim_self: bool) -> WireResult<Value> {
        let Type::Union { members, .. } = ty else {
            return Err(WireError::Encode(EncodeError::Unsupported {
                detail: "expected union type",
            }));
        };
        if target + 8 > self.bytes.len() {
            return Err(WireError::illegal_memory_range());
        }
        let size = self.read_u32(target)? as usize;
        let tag = self.read_u32(target + 4)?;
        if tag as usize >= members.len() {
            return Err(WireError::unknown_union_tag());
        }
        if claim_self {
            if size != layout::UNION_DATA_SIZE {
                return Err(WireError::illegal_memory_range());
            }
            self.claim(target, layout::UNION_DATA_SIZE)?;
        }
        let member = &members[tag as usize];
        let data_off = target + 8;
        let value = self.decode_union_payload(data_off, &member.ty)?;
        Ok(Value::Union {
            tag,
            value: Box::new(value),
        })
    }

    fn decode_union_payload(&mut self, data_off: usize, member_ty: &Type) -> WireResult<Value> {
        match member_ty {
            // Struct members are encoded as a POINTER within the union payload.
            Type::Struct { .. } => self.decode_struct_ptr(data_off, member_ty),
            Type::String { nullable } => self.decode_string(data_off, *nullable),
            Type::Array { element, nullable } => self.decode_array(data_off, element, *nullable),
            Type::Map {
                key,
                value,
                nullable,
            } => self.decode_map(data_off, key, value, *nullable),
            _ => self.decode_field(data_off, 0, member_ty),
        }
    }

    /// Decode a pointer-to-struct member within a union payload.
    fn decode_struct_ptr(&mut self, offset: usize, ty: &Type) -> WireResult<Value> {
        let raw = self.read_u64(offset)?;
        let ptr = Pointer::decode(offset as u64, raw)?;
        match ptr {
            Pointer::Null => {
                if ty.is_nullable() {
                    Ok(Value::Null)
                } else {
                    Err(WireError::unexpected_null_pointer())
                }
            }
            Pointer::Offset(target) => {
                let t = usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
                self.decode_struct(t, ty)
            }
        }
    }

    fn decode_string(&mut self, offset: usize, nullable: bool) -> WireResult<Value> {
        let raw = self.read_u64(offset)?;
        let ptr = Pointer::decode(offset as u64, raw)?;
        let Pointer::Offset(target) = ptr else {
            if nullable {
                return Ok(Value::Null);
            }
            return Err(WireError::unexpected_null_pointer());
        };
        let target = usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
        self.decode_string_at(target)
    }

    fn decode_string_at(&mut self, target: usize) -> WireResult<Value> {
        if target + layout::ARRAY_HEADER_SIZE > self.bytes.len() {
            return Err(WireError::illegal_memory_range());
        }
        let num_bytes = self.read_u32(target)? as usize;
        let num_elements = self.read_u32(target + 4)? as usize;
        if num_bytes < layout::ARRAY_HEADER_SIZE
            || num_bytes > self.bytes.len().saturating_sub(target)
        {
            return Err(WireError::unexpected_array_header());
        }
        if num_elements > num_bytes.saturating_sub(layout::ARRAY_HEADER_SIZE) {
            return Err(WireError::unexpected_array_header());
        }
        let data_start = target + layout::ARRAY_HEADER_SIZE;
        let data = self
            .bytes
            .get(data_start..data_start + num_elements)
            .ok_or(WireError::illegal_memory_range())?;
        if data.contains(&0) {
            return Err(WireError::deserialization_failed());
        }
        if self.bytes.get(data_start + num_elements).copied() != Some(0) {
            return Err(WireError::deserialization_failed());
        }
        self.claim(target, num_bytes)?;
        let s = core::str::from_utf8(data).map_err(|_| WireError::deserialization_failed())?;
        Ok(Value::String(s.to_owned()))
    }

    fn decode_array(&mut self, offset: usize, element: &Type, nullable: bool) -> WireResult<Value> {
        let raw = self.read_u64(offset)?;
        let ptr = Pointer::decode(offset as u64, raw)?;
        let Pointer::Offset(target) = ptr else {
            if nullable {
                return Ok(Value::Null);
            }
            return Err(WireError::unexpected_null_pointer());
        };
        let target = usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
        self.enter()?;
        let r = self.decode_array_body(target, element);
        self.leave();
        r
    }

    fn decode_array_body(&mut self, target: usize, element: &Type) -> WireResult<Value> {
        if target + layout::ARRAY_HEADER_SIZE > self.bytes.len() {
            return Err(WireError::illegal_memory_range());
        }
        let num_bytes = self.read_u32(target)? as usize;
        let num_elements = self.read_u32(target + 4)? as usize;
        eprintln!(
            "DBG array target={target} nb={num_bytes} ne={num_elements} elem={element:?} len={}",
            self.bytes.len()
        );
        let is_pod = matches!(
            element,
            Type::Bool
                | Type::I8
                | Type::U8
                | Type::I16
                | Type::U16
                | Type::I32
                | Type::U32
                | Type::I64
                | Type::U64
                | Type::F32
                | Type::F64
                | Type::Enum
                | Type::Handle
        );
        let elem_size = if is_pod {
            element.pack_kind().size()
        } else {
            layout::POINTER_SIZE
        };
        let min_bytes = layout::ARRAY_HEADER_SIZE
            .checked_add(
                elem_size
                    .checked_mul(num_elements)
                    .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?,
            )
            .ok_or(WireError::Encode(EncodeError::ArithmeticOverflow))?;
        if num_bytes < min_bytes {
            return Err(WireError::unexpected_array_header());
        }
        if num_bytes > self.bytes.len().saturating_sub(target) {
            return Err(WireError::illegal_memory_range());
        }
        self.claim(target, num_bytes)?;

        let mut out = Vec::with_capacity(num_elements);
        let data_off = target + layout::ARRAY_HEADER_SIZE;
        for i in 0..num_elements {
            let item_off = data_off + i * elem_size;
            let v = if is_pod {
                self.decode_field(item_off, 0, element)?
            } else {
                let raw = self.read_u64(item_off)?;
                let ptr = Pointer::decode(item_off as u64, raw)?;
                match ptr {
                    Pointer::Null => {
                        if element.is_nullable() {
                            Value::Null
                        } else {
                            return Err(WireError::unexpected_null_pointer());
                        }
                    }
                    Pointer::Offset(t) => {
                        let t = usize::try_from(t).map_err(|_| WireError::illegal_pointer())?;
                        self.decode_array_element(t, element)?
                    }
                }
            };
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn decode_array_element(&mut self, target: usize, element: &Type) -> WireResult<Value> {
        match element {
            Type::Struct { .. } => self.decode_struct(target, element),
            Type::String { .. } => self.decode_string_at(target),
            Type::Map { key, value, .. } => self.decode_map_at(target, key, value),
            Type::Array { element: inner, .. } => self.decode_array_body(target, inner),
            _ => Err(WireError::Encode(EncodeError::Unsupported {
                detail: "array element decode",
            })),
        }
    }

    fn decode_map(
        &mut self,
        offset: usize,
        key: &Type,
        value: &Type,
        nullable: bool,
    ) -> WireResult<Value> {
        let raw = self.read_u64(offset)?;
        let ptr = Pointer::decode(offset as u64, raw)?;
        let Pointer::Offset(target) = ptr else {
            if nullable {
                return Ok(Value::Null);
            }
            return Err(WireError::unexpected_null_pointer());
        };
        let target = usize::try_from(target).map_err(|_| WireError::illegal_pointer())?;
        self.decode_map_at(target, key, value)
    }

    fn decode_map_at(&mut self, target: usize, key: &Type, value: &Type) -> WireResult<Value> {
        self.enter()?;
        let r = (|| -> WireResult<Value> {
            if target + 24 > self.bytes.len() {
                return Err(WireError::illegal_memory_range());
            }
            let num_bytes = self.read_u32(target)? as usize;
            if num_bytes < 24 {
                return Err(WireError::unexpected_struct_header());
            }
            self.claim(target, num_bytes)?;
            let kp = Pointer::decode((target + 8) as u64, self.read_u64(target + 8)?)?;
            let vp = Pointer::decode((target + 16) as u64, self.read_u64(target + 16)?)?;
            let (Pointer::Offset(ko), Pointer::Offset(vo)) = (kp, vp) else {
                return Err(WireError::unexpected_null_pointer());
            };
            let ko = usize::try_from(ko).map_err(|_| WireError::illegal_pointer())?;
            let vo = usize::try_from(vo).map_err(|_| WireError::illegal_pointer())?;
            let keys = self.decode_array_body(ko, key)?;
            let values = self.decode_array_body(vo, value)?;
            let (Value::Array(keys), Value::Array(values)) = (keys, values) else {
                return Err(WireError::Encode(EncodeError::Unsupported {
                    detail: "map arrays",
                }));
            };
            if keys.len() != values.len() {
                return Err(WireError::different_sized_arrays_in_map());
            }
            Ok(Value::Map { keys, values })
        })();
        self.leave();
        r
    }
}

fn default_value(ft: &FieldType) -> Value {
    use Type::*;
    match &ft.ty {
        Bool => Value::Bool(false),
        I8 => Value::I8(0),
        U8 => Value::U8(0),
        I16 => Value::I16(0),
        U16 => Value::U16(0),
        I32 => Value::I32(0),
        U32 => Value::U32(0),
        I64 => Value::I64(0),
        U64 => Value::U64(0),
        F32 => Value::F32(0),
        F64 => Value::F64(0),
        Enum => Value::Enum(0),
        Handle => Value::Handle {
            index: layout::ENCODED_INVALID_HANDLE_VALUE,
        },
        Interface { version } => Value::Interface {
            index: layout::ENCODED_INVALID_HANDLE_VALUE,
            version: *version,
        },
        AssociatedEndpoint => Value::AssociatedEndpoint {
            id: layout::ENCODED_INVALID_HANDLE_VALUE,
        },
        AssociatedInterface { version } => Value::AssociatedInterface {
            id: layout::ENCODED_INVALID_HANDLE_VALUE,
            version: *version,
        },
        NullableScalar(_) => Value::NullableScalar {
            present: false,
            value: Box::new(Value::Null),
        },
        _ => Value::Null,
    }
}

/// Convenience accessor used by tests and the harness.
pub fn value_to_u32(v: &Value) -> Option<u32> {
    match v {
        Value::U32(x) => Some(*x),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::layout;

    fn simple_struct(version: u32) -> Type {
        Type::Struct {
            fields: vec![
                FieldType {
                    name: "a",
                    ty: Type::U32,
                    min_version: None,
                },
                FieldType {
                    name: "b",
                    ty: Type::U64,
                    min_version: None,
                },
                FieldType {
                    name: "s",
                    ty: Type::String { nullable: true },
                    min_version: Some(1),
                },
            ],
            version,
            nullable: false,
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let ty = simple_struct(1);
        let val = Value::Struct {
            version: 1,
            fields: vec![
                Value::U32(0xDEADBEEF),
                Value::U64(0x1122334455667788),
                Value::String("hello".to_owned()),
            ],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        assert_eq!(enc.handle_count, 0);

        let mut dec = Decoder::new(&enc.bytes, 0);
        let payload_off = layout::align_up(layout::MESSAGE_HEADER_V0_SIZE, 8).unwrap();
        let got = dec.decode_struct(payload_off, &ty).unwrap();
        assert_eq!(got, val);
    }

    #[test]
    fn string_wire_layout_is_exact() {
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "s",
                ty: Type::String { nullable: false },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::String("hi".to_owned())],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();

        let payload_off = 24usize;
        assert_eq!(enc.bytes.len() % 8, 0);
        assert_eq!(
            &enc.bytes[payload_off..payload_off + 4],
            &16u32.to_le_bytes()
        );
        assert_eq!(
            &enc.bytes[payload_off + 4..payload_off + 8],
            &0u32.to_le_bytes()
        );
        let raw = u64::from_le_bytes(
            enc.bytes[payload_off + 8..payload_off + 16]
                .try_into()
                .unwrap(),
        );
        let target = ((payload_off + 8) as u64 + raw) as usize;
        assert_eq!(&enc.bytes[target..target + 4], &16u32.to_le_bytes());
        assert_eq!(&enc.bytes[target + 4..target + 8], &2u32.to_le_bytes());
        assert_eq!(&enc.bytes[target + 8..target + 10], b"hi");
        assert_eq!(enc.bytes[target + 10], 0);
    }

    #[test]
    fn pod_array_is_inline() {
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "arr",
                ty: Type::Array {
                    element: Box::new(Type::U8),
                    nullable: false,
                },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::Array(vec![Value::U8(1), Value::U8(2), Value::U8(3)])],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        let payload_off = 24usize;
        let raw = u64::from_le_bytes(
            enc.bytes[payload_off + 8..payload_off + 16]
                .try_into()
                .unwrap(),
        );
        let target = ((payload_off + 8) as u64 + raw) as usize;
        assert_eq!(&enc.bytes[target..target + 4], &16u32.to_le_bytes()); // 8 + 3 + pad
        assert_eq!(&enc.bytes[target + 4..target + 8], &3u32.to_le_bytes());
        assert_eq!(&enc.bytes[target + 8..target + 11], &[1, 2, 3]);
    }

    #[test]
    fn nullable_scalar_roundtrip() {
        let ty = Type::Struct {
            fields: vec![
                FieldType {
                    name: "n",
                    ty: Type::NullableScalar(Box::new(Type::I32)),
                    min_version: None,
                },
                FieldType {
                    name: "m",
                    ty: Type::NullableScalar(Box::new(Type::U8)),
                    min_version: None,
                },
            ],
            version: 0,
            nullable: false,
        };
        // n present = -7, m absent.
        let val = Value::Struct {
            version: 0,
            fields: vec![
                Value::NullableScalar {
                    present: true,
                    value: Box::new(Value::I32(-7)),
                },
                Value::NullableScalar {
                    present: false,
                    value: Box::new(Value::Null),
                },
            ],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        let mut dec = Decoder::new(&enc.bytes, 0);
        let got = dec.decode_struct(24, &ty).unwrap();
        assert_eq!(
            got,
            Value::Struct {
                version: 0,
                fields: vec![
                    Value::NullableScalar {
                        present: true,
                        value: Box::new(Value::I32(-7))
                    },
                    Value::NullableScalar {
                        present: false,
                        value: Box::new(Value::Null)
                    },
                ],
            }
        );
    }

    #[test]
    fn map_roundtrip_and_count_mismatch() {
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "m",
                ty: Type::Map {
                    key: Box::new(Type::String { nullable: false }),
                    value: Box::new(Type::U32),
                    nullable: false,
                },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::Map {
                keys: vec![Value::String("k1".into()), Value::String("k2".into())],
                values: vec![Value::U32(1), Value::U32(2)],
            }],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        let mut dec = Decoder::new(&enc.bytes, 0);
        let got = dec.decode_struct(24, &ty).unwrap();
        assert_eq!(got, val);
    }

    #[test]
    fn union_roundtrip() {
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "u",
                ty: Type::Union {
                    members: vec![
                        UnionMember {
                            name: "i",
                            ty: Type::I32,
                        },
                        UnionMember {
                            name: "s",
                            ty: Type::String { nullable: false },
                        },
                    ],
                    nullable: false,
                    inlined: true,
                },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        // Union with tag 1 (string member).
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::Union {
                tag: 1,
                value: Box::new(Value::String("union!".into())),
            }],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        let mut dec = Decoder::new(&enc.bytes, 0);
        let got = dec.decode_struct(24, &ty).unwrap();
        assert_eq!(got, val);
    }

    #[test]
    fn union_unknown_tag_rejected() {
        // Craft a message with an out-of-range union tag.
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "u",
                ty: Type::Union {
                    members: vec![UnionMember {
                        name: "i",
                        ty: Type::I32,
                    }],
                    nullable: false,
                    inlined: true,
                },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::Union {
                tag: 0,
                value: Box::new(Value::I32(5)),
            }],
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let mut enc = encode_message(&header, &ty, &val, 0).unwrap();
        // Corrupt the tag (payload struct at 24; union field at 24+8+0=32).
        enc.bytes[32 + 4..32 + 8].copy_from_slice(&9u32.to_le_bytes());
        let mut dec = Decoder::new(&enc.bytes, 0);
        assert_eq!(
            dec.decode_struct(24, &ty).unwrap_err(),
            WireError::unknown_union_tag()
        );
    }

    #[test]
    fn decode_rejects_out_of_bounds_pointer() {
        let mut b = vec![0u8; 64];
        b[24..28].copy_from_slice(&16u32.to_le_bytes());
        b[28..32].copy_from_slice(&0u32.to_le_bytes());
        b[32..40].copy_from_slice(&0xFFFFu64.to_le_bytes());
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "s",
                ty: Type::String { nullable: true },
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let mut dec = Decoder::new(&b, 0);
        assert!(dec.decode_struct(24, &ty).is_err());
    }

    #[test]
    fn decode_rejects_overlapping_objects() {
        let ty = Type::Struct {
            fields: vec![
                FieldType {
                    name: "s1",
                    ty: Type::String { nullable: true },
                    min_version: None,
                },
                FieldType {
                    name: "s2",
                    ty: Type::String { nullable: true },
                    min_version: None,
                },
            ],
            version: 0,
            nullable: false,
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::String("a".to_owned()), Value::String("b".to_owned())],
        };
        let enc = encode_message(&header, &ty, &val, 0).unwrap();
        let mut bytes = enc.bytes.clone();
        // Make s2's pointer (slot at 40) target the same object as s1's
        // (slot at 32 -> object at 48): relative offset 48 - 40 = 8.
        bytes[40..48].copy_from_slice(&8u64.to_le_bytes());
        let mut dec = Decoder::new(&bytes, 0);
        assert_eq!(
            dec.decode_struct(24, &ty).unwrap_err(),
            WireError::illegal_memory_range()
        );
    }

    #[test]
    fn decode_rejects_handle_index_out_of_range() {
        // A struct with a handle field whose index exceeds handle_count.
        let ty = Type::Struct {
            fields: vec![FieldType {
                name: "h",
                ty: Type::Handle,
                min_version: None,
            }],
            version: 0,
            nullable: false,
        };
        let header = vec![0u8; layout::MESSAGE_HEADER_V0_SIZE];
        let val = Value::Struct {
            version: 0,
            fields: vec![Value::Handle { index: 3 }],
        };
        let enc = encode_message(&header, &ty, &val, 3).unwrap();
        let mut dec = Decoder::new(&enc.bytes, 0); // 0 attached handles
        assert_eq!(
            dec.decode_struct(24, &ty).unwrap_err(),
            WireError::illegal_handle()
        );
    }

    #[test]
    fn validation_error_names_match_official() {
        assert_eq!(
            ValidationError::MisalignedObject.name(),
            "VALIDATION_ERROR_MISALIGNED_OBJECT"
        );
        assert_eq!(
            ValidationError::IllegalMemoryRange.name(),
            "VALIDATION_ERROR_ILLEGAL_MEMORY_RANGE"
        );
        assert_eq!(
            ValidationError::UnexpectedStructHeader.name(),
            "VALIDATION_ERROR_UNEXPECTED_STRUCT_HEADER"
        );
        assert_eq!(
            ValidationError::UnexpectedArrayHeader.name(),
            "VALIDATION_ERROR_UNEXPECTED_ARRAY_HEADER"
        );
        assert_eq!(
            ValidationError::IllegalHandle.name(),
            "VALIDATION_ERROR_ILLEGAL_HANDLE"
        );
        assert_eq!(
            ValidationError::UnexpectedInvalidHandle.name(),
            "VALIDATION_ERROR_UNEXPECTED_INVALID_HANDLE"
        );
        assert_eq!(
            ValidationError::IllegalPointer.name(),
            "VALIDATION_ERROR_ILLEGAL_POINTER"
        );
        assert_eq!(
            ValidationError::UnexpectedNullPointer.name(),
            "VALIDATION_ERROR_UNEXPECTED_NULL_POINTER"
        );
        assert_eq!(
            ValidationError::IllegalInterfaceId.name(),
            "VALIDATION_ERROR_ILLEGAL_INTERFACE_ID"
        );
        assert_eq!(
            ValidationError::UnexpectedInvalidInterfaceId.name(),
            "VALIDATION_ERROR_UNEXPECTED_INVALID_INTERFACE_ID"
        );
        assert_eq!(
            ValidationError::MessageHeaderInvalidFlags.name(),
            "VALIDATION_ERROR_MESSAGE_HEADER_INVALID_FLAGS"
        );
        assert_eq!(
            ValidationError::MessageHeaderMissingRequestId.name(),
            "VALIDATION_ERROR_MESSAGE_HEADER_MISSING_REQUEST_ID"
        );
        assert_eq!(
            ValidationError::DifferentSizedArraysInMap.name(),
            "VALIDATION_ERROR_DIFFERENT_SIZED_ARRAYS_IN_MAP"
        );
        assert_eq!(
            ValidationError::UnknownUnionTag.name(),
            "VALIDATION_ERROR_UNKNOWN_UNION_TAG"
        );
        assert_eq!(
            ValidationError::UnknownEnumValue.name(),
            "VALIDATION_ERROR_UNKNOWN_ENUM_VALUE"
        );
        assert_eq!(
            ValidationError::MaxRecursionDepth.name(),
            "VALIDATION_ERROR_MAX_RECURSION_DEPTH"
        );
    }
}
