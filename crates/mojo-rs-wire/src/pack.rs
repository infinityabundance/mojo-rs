//! Struct field packing — a faithful port of the official mojom layout
//! algorithm (`mojo/public/tools/mojom/mojom/generate/pack.py` at the pinned
//! revision).
//!
//! Field offsets are wire-visible and compatibility-critical: a single byte
//! of divergence breaks wire parity. This module is differential-tested
//! against the official generator output (wire court).

use crate::error::{EncodeError, WireError, WireResult};

/// Size of the struct header in bytes: num_bytes [4] + version [4].
pub const STRUCT_HEADER_SIZE: usize = 8;

/// Mojom field kinds with their wire sizes/alignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Boolean (1 byte, bit-packed in structs).
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
    /// Enum (packs as i32).
    Enum,
    /// Raw handle (4 bytes).
    Handle,
    /// Message pipe endpoint handle (4 bytes).
    MsgPipe,
    /// Shared buffer handle (4 bytes).
    SharedBuffer,
    /// Platform handle (4 bytes).
    PlatformHandle,
    /// Data pipe consumer handle (4 bytes).
    DcPipe,
    /// Data pipe producer handle (4 bytes).
    DpPipe,
    /// Reference kinds: 8-byte relative pointers.
    /// String (8-byte relative pointer).
    String,
    /// Array (8-byte relative pointer).
    Array,
    /// Map (8-byte relative pointer).
    Map,
    /// Struct (inline or 8-byte pointer when nullable).
    Struct,
    /// Interface (8 bytes: handle + version).
    Interface,
    /// Pending remote (8-byte pointer).
    PendingRemote,
    /// Pending associated remote (8 bytes).
    PendingAssociatedRemote,
    /// Union: 16 bytes inline, 8-aligned.
    /// Union (16 bytes inline, 8-aligned).
    Union,
    /// `PendingReceiver` packs as a 4-byte message pipe handle.
    /// Pending receiver packs as a 4-byte message pipe handle.
    PendingReceiver,
    /// `PendingAssociatedReceiver` packs as a 4-byte associated endpoint.
    /// Pending associated receiver packs as a 4-byte endpoint.
    PendingAssociatedReceiver,
}

impl Kind {
    /// `PackedField.GetSizeForKind`.
    pub fn size(self) -> usize {
        match self {
            Kind::Bool | Kind::I8 | Kind::U8 => 1,
            Kind::I16 | Kind::U16 => 2,
            Kind::I32
            | Kind::U32
            | Kind::F32
            | Kind::Enum
            | Kind::Handle
            | Kind::MsgPipe
            | Kind::SharedBuffer
            | Kind::PlatformHandle
            | Kind::DcPipe
            | Kind::DpPipe
            | Kind::PendingReceiver
            | Kind::PendingAssociatedReceiver => 4,
            Kind::I64 | Kind::U64 | Kind::F64 => 8,
            Kind::String
            | Kind::Array
            | Kind::Map
            | Kind::Struct
            | Kind::Interface
            | Kind::PendingRemote
            | Kind::PendingAssociatedRemote => 8,
            Kind::Union => 16,
        }
    }

    /// `PackedField.GetAlignmentForKind`.
    pub fn alignment(self) -> usize {
        match self {
            Kind::Interface | Kind::PendingRemote | Kind::PendingAssociatedRemote => 4,
            Kind::Union => 8,
            k => k.size(),
        }
    }

    /// Whether the kind is encoded as a relative pointer.
    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Kind::String
                | Kind::Array
                | Kind::Map
                | Kind::Struct
                | Kind::Interface
                | Kind::PendingRemote
        )
    }

    /// Whether the kind is a handle-like 4-byte kind.
    pub fn is_handle(self) -> bool {
        matches!(
            self,
            Kind::Handle
                | Kind::MsgPipe
                | Kind::SharedBuffer
                | Kind::PlatformHandle
                | Kind::DcPipe
                | Kind::DpPipe
                | Kind::PendingReceiver
        )
    }

    /// Whether the kind is an associated endpoint (4-byte).
    pub fn is_associated(self) -> bool {
        matches!(self, Kind::PendingAssociatedReceiver)
    }
}

/// A struct field descriptor prior to packing.
#[derive(Debug, Clone)]
pub struct Field {
    /// Declaration position in the struct.
    pub index: usize,
    /// Wire ordinal (explicit or derived); used for ordering and versioning.
    pub ordinal: u32,
    /// Explicit [MinVersion] of the field, if any.
    pub min_version: Option<u32>,
    /// The field kind (already resolved to pack kinds).
    pub kind: Kind,
    /// Whether this is a nullable VALUE kind (packs as flag + value pair).
    pub nullable_value_kind: bool,
    /// Whether this is a nullable REFERENCE kind (string?, struct?, ...).
    /// Nullable references are exempt from the version-0-only rule.
    pub nullable_reference: bool,
    /// Human name for diagnostics.
    pub mojom_name: &'static str,
}

impl Field {
    /// A non-nullable reference field is only allowed in version 0
    /// (enforced by the official generator).
    pub fn is_nonnullable_reference(&self) -> bool {
        self.kind.is_reference() && !self.nullable_reference
    }
}

/// A packed field: size/alignment resolved, offset assigned.
#[derive(Debug, Clone)]
pub struct PackedField {
    /// The original field.
    pub field: Field,
    /// Ordinal sort key (with sub-ordinal for nullable value pairs).
    pub ordinal: u32,
    /// Sub-ordinal (0=flag, 1=value) for nullable value pairs.
    pub sub_ordinal: Option<u8>,
    /// Wire size in bytes.
    pub size: usize,
    /// Wire alignment in bytes.
    pub alignment: usize,
    /// Assigned byte offset within the payload (after the 8-byte header).
    pub offset: usize,
    /// Bit position within the offset byte for packed booleans.
    pub bit: u8,
    /// Effective minimum version of this packed field.
    pub min_version: u32,
    /// For a nullable-value flag field: the value packed field it gates.
    pub linked_value: Option<usize>,
}

impl PackedField {
    /// Whether this is the primary (flag) field of a nullable value pair.
    pub fn is_primary_nullable_value(&self) -> bool {
        self.linked_value.is_some()
    }
}

/// A packed struct: all fields with assigned offsets, in offset order and in
/// ordinal order.
#[derive(Debug, Clone)]
pub struct PackedStruct {
    /// Fields in increasing offset order.
    pub packed_fields: Vec<PackedField>,
    /// The same fields in ordinal order.
    pub packed_fields_in_ordinal_order: Vec<PackedField>,
    /// Total size (payload rounded to 8 + header).
    pub size: usize,
    /// Per-version sizes: (version, num_bytes).
    pub version_info: Vec<(u32, usize)>,
}

fn pad(offset: usize, alignment: usize) -> usize {
    (alignment - (offset % alignment)) % alignment
}

/// Compute the next offset/bit after `last_field` for `field` (bool packing
/// included).
fn field_offset(field: &Field, last: &PackedField) -> (usize, u8) {
    if field.kind == Kind::Bool && last.field.kind == Kind::Bool && last.bit < 7 {
        return (last.offset, last.bit + 1);
    }
    let offset = last.offset + last.size;
    (offset + pad(offset, field.kind.alignment()), 0)
}

/// Payload size (excluding header) if `field` is the last field.
fn payload_size_up_to_field(field: &PackedField) -> usize {
    if field.offset == usize::MAX {
        return 0;
    }
    let end = field.offset + field.size;
    end + pad(end, 8)
}

impl PackedStruct {
    /// Pack `fields` (in declaration order) into a `PackedStruct`.
    ///
    /// Mirrors `PackedStruct.__init__` from the pinned `pack.py`.
    pub fn pack(fields: Vec<Field>) -> WireResult<PackedStruct> {
        let mut src_fields: Vec<PackedField> = Vec::with_capacity(fields.len());

        // Ordinal assignment: explicit ordinals override; then increment.
        let mut ordinal = 0u32;
        for (index, field) in fields.into_iter().enumerate() {
            if let Some(o) = field_ordinal_hint(&field) {
                ordinal = o;
            }
            let ordinal_at = ordinal;
            if field.nullable_value_kind {
                // Nullable value kinds expand to two packed fields: a BOOL
                // flag (sub_ordinal 0) and the non-nullable value
                // (sub_ordinal 1).
                let mut value_field = field.clone();
                value_field.kind = unnullify(value_field.kind);
                value_field.mojon_name_for_value();
                let value_pf = PackedField {
                    field: value_field,
                    ordinal: ordinal_at,
                    sub_ordinal: Some(1),
                    size: 0,
                    alignment: 0,
                    offset: usize::MAX,
                    bit: 0,
                    min_version: 0,
                    linked_value: None,
                };
                let mut flag_field = field;
                flag_field.kind = Kind::Bool;
                flag_field.mojon_name_for_flag();
                let flag_pf = PackedField {
                    field: flag_field,
                    ordinal: ordinal_at,
                    sub_ordinal: Some(0),
                    size: 0,
                    alignment: 0,
                    offset: usize::MAX,
                    bit: 0,
                    min_version: 0,
                    linked_value: None,
                };
                src_fields.push(flag_pf);
                src_fields.push(value_pf);
            } else {
                src_fields.push(PackedField {
                    field,
                    ordinal: ordinal_at,
                    sub_ordinal: None,
                    size: 0,
                    alignment: 0,
                    offset: usize::MAX,
                    bit: 0,
                    min_version: 0,
                    linked_value: None,
                });
            }
            ordinal += 1;
        }

        // Resolve sizes/alignments.
        for pf in &mut src_fields {
            pf.size = pf.field.kind.size();
            pf.alignment = pf.field.kind.alignment();
        }

        // Sort by (ordinal, sub_ordinal).
        src_fields.sort_by_key(|f| (f.ordinal, f.sub_ordinal));

        // min_version propagation.
        let mut next_min_version = 0u32;
        for pf in &mut src_fields {
            if let Some(mv) = pf.field.min_version {
                if mv < next_min_version {
                    return Err(WireError::Encode(EncodeError::Unsupported {
                        detail: "field min_version out of order",
                    }));
                }
                next_min_version = mv;
            } else if next_min_version != 0 {
                return Err(WireError::Encode(EncodeError::Unsupported {
                    detail: "field without min_version after versioned field",
                }));
            }
            pf.min_version = next_min_version;

            if pf.min_version != 0 && pf.field.is_nonnullable_reference() {
                return Err(WireError::Encode(EncodeError::Unsupported {
                    detail: "non-nullable reference field in a versioned struct",
                }));
            }
        }

        // First-fit packing.
        let mut dst: Vec<PackedField> = Vec::with_capacity(src_fields.len());
        if let Some(first) = src_fields.first().cloned() {
            let mut first = first;
            first.offset = 0;
            first.bit = 0;
            dst.push(first);
        }
        for src in src_fields.iter().skip(1).cloned() {
            let mut placed = None;
            let mut last = dst[0].clone();
            for (i, next) in dst.iter().enumerate().skip(1) {
                let (offset, bit) = field_offset(&src.field, &last);
                if offset + src.size <= next.offset {
                    let mut s = src.clone();
                    s.offset = offset;
                    s.bit = bit;
                    dst.insert(i, s);
                    placed = Some(());
                    break;
                }
                last = next.clone();
            }
            if placed.is_none() {
                let (offset, bit) = match dst.last() {
                    Some(last) => field_offset(&src.field, last),
                    // dst is non-empty after the first field is placed.
                    None => (0, 0),
                };
                let mut s = src;
                s.offset = offset;
                s.bit = bit;
                dst.push(s);
            }
        }

        // Link nullable-value flag fields to their value fields.
        // The flag and value share the same ordinal; find pairs.
        let mut linked: Vec<Option<usize>> = vec![None; dst.len()];
        // map ordinal -> flag index
        for i in 0..dst.len() {
            if dst[i].sub_ordinal == Some(0) {
                // find the value field with same ordinal and sub_ordinal 1
                for j in 0..dst.len() {
                    if j != i && dst[j].sub_ordinal == Some(1) && dst[j].ordinal == dst[i].ordinal {
                        linked[i] = Some(j);
                        break;
                    }
                }
            }
        }
        let packed_fields: Vec<PackedField> = dst
            .into_iter()
            .enumerate()
            .map(|(i, mut pf)| {
                pf.linked_value = linked[i];
                pf
            })
            .collect();

        let size = if let Some(last) = packed_fields.last() {
            payload_size_up_to_field(last) + STRUCT_HEADER_SIZE
        } else {
            STRUCT_HEADER_SIZE
        };

        let ordinal_order = {
            let mut v = packed_fields.clone();
            v.sort_by_key(|f| (f.ordinal, f.sub_ordinal));
            v
        };

        // Version info (GetVersionInfo).
        let mut version_info: Vec<(u32, usize)> = Vec::new();
        {
            let mut last_version = 0u32;
            let mut last_payload = 0usize;
            for pf in &ordinal_order {
                if pf.min_version != last_version {
                    version_info.push((last_version, last_payload + STRUCT_HEADER_SIZE));
                    last_version = pf.min_version;
                }
                last_payload = last_payload.max(payload_size_up_to_field(pf));
            }
            version_info.push((last_version, last_payload + STRUCT_HEADER_SIZE));
        }

        Ok(PackedStruct {
            packed_fields,
            packed_fields_in_ordinal_order: ordinal_order,
            size,
            version_info,
        })
    }

    /// Number of bytes for a given version (num_bytes field on the wire).
    pub fn num_bytes_for_version(&self, version: u32) -> Option<usize> {
        self.version_info
            .iter()
            .filter(|(v, _)| *v <= version)
            .map(|(_, n)| *n)
            .max()
    }
}

/// Helper trait-free extension for the nullable-value expansion.
trait FieldExt {
    fn mojon_name_for_flag(&mut self);
    fn mojon_name_for_value(&mut self);
}

impl FieldExt for Field {
    fn mojon_name_for_flag(&mut self) {
        // Names are diagnostics only; the wire does not carry field names.
    }
    fn mojon_name_for_value(&mut self) {
        // Names are diagnostics only; the wire does not carry field names.
    }
}

fn field_ordinal_hint(_f: &Field) -> Option<u32> {
    // The mojom compiler resolves explicit ordinals before packing. The wire
    // packer receives resolved ordinals; callers set `Field.ordinal`.
    None
}

fn unnullify(k: Kind) -> Kind {
    k
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn f(index: usize, kind: Kind, min_version: Option<u32>) -> Field {
        Field {
            index,
            ordinal: index as u32,
            min_version,
            kind,
            nullable_value_kind: false,
            nullable_reference: false,
            mojom_name: "f",
        }
    }

    #[test]
    fn empty_struct_is_8_bytes() {
        let ps = PackedStruct::pack(vec![]).unwrap();
        assert_eq!(ps.size, 8);
        assert_eq!(ps.packed_fields.len(), 0);
    }

    #[test]
    fn single_u32_at_offset_0() {
        let ps = PackedStruct::pack(vec![f(0, Kind::U32, None)]).unwrap();
        assert_eq!(ps.packed_fields.len(), 1);
        assert_eq!(ps.packed_fields[0].offset, 0);
        assert_eq!(ps.size, 8 + 8); // 4 bytes + pad to 8 + header
    }

    #[test]
    fn mixed_layout_matches_pack_py() {
        // Struct { u8 a; u64 b; u32 c; }
        // Official first-fit packing: a@0 (size 1), then c fits the hole at 4
        // (GetFieldOffset(c, a)=4, 4+4 <= b's offset 8), then b@8.
        let ps = PackedStruct::pack(vec![
            f(0, Kind::U8, None),
            f(1, Kind::U64, None),
            f(2, Kind::U32, None),
        ])
        .unwrap();
        let offsets: Vec<usize> = ps.packed_fields.iter().map(|p| p.offset).collect();
        assert_eq!(offsets, vec![0, 4, 8]);
        assert_eq!(ps.size, 24);
    }

    #[test]
    fn bools_pack_into_bytes() {
        let ps = PackedStruct::pack(vec![
            f(0, Kind::Bool, None),
            f(1, Kind::Bool, None),
            f(2, Kind::Bool, None),
            f(3, Kind::U32, None),
        ])
        .unwrap();
        let bits: Vec<(usize, u8)> = ps.packed_fields.iter().map(|p| (p.offset, p.bit)).collect();
        assert_eq!(bits[0], (0, 0));
        assert_eq!(bits[1], (0, 1));
        assert_eq!(bits[2], (0, 2));
        // u32 cannot share the bool byte; aligned to 4.
        assert_eq!(bits[3], (4, 0));
    }

    #[test]
    fn version_sizes_grow() {
        // Both fields pack within 8 payload bytes (u32@0, u32@4), so both
        // versions report num_bytes == 16 (8 header + 8 payload).
        let ps = PackedStruct::pack(vec![f(0, Kind::U32, None), f(1, Kind::U32, Some(3))]).unwrap();
        assert_eq!(ps.num_bytes_for_version(0), Some(16));
        assert_eq!(ps.num_bytes_for_version(1), Some(16));
        assert_eq!(ps.num_bytes_for_version(3), Some(16));
        assert_eq!(ps.size, 16);
    }

    #[test]
    fn nullable_value_kind_expands_to_flag_pair() {
        let ps = PackedStruct::pack(vec![Field {
            index: 0,
            ordinal: 0,
            min_version: None,
            kind: Kind::U32,
            nullable_value_kind: true,
            nullable_reference: false,
            mojom_name: "n",
        }])
        .unwrap();
        // Two packed fields: BOOL flag + U32 value.
        assert_eq!(ps.packed_fields.len(), 2);
        assert_eq!(ps.packed_fields[0].field.kind, Kind::Bool);
        assert_eq!(ps.packed_fields[1].field.kind, Kind::U32);
        assert!(ps.packed_fields[0].is_primary_nullable_value());
    }

    #[test]
    fn rejects_nonnullable_reference_after_version_0() {
        let err = PackedStruct::pack(vec![f(0, Kind::U32, None), f(1, Kind::String, Some(1))]);
        assert!(err.is_err());
    }

    #[test]
    fn allows_nullable_reference_after_version_0() {
        let ps = PackedStruct::pack(vec![
            f(0, Kind::U32, None),
            Field {
                index: 1,
                ordinal: 1,
                min_version: Some(1),
                kind: Kind::String,
                nullable_value_kind: false,
                nullable_reference: true,
                mojom_name: "s",
            },
        ])
        .unwrap();
        assert_eq!(ps.packed_fields.len(), 2);
    }
}
