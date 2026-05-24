use std::fmt;
use std::ops::Range;

use super::ContextResponse;

pub const PACK_BINARY_SCHEMA_V1: &str = "ee.pack.bin.v1";
pub const PACK_BINARY_MAGIC: [u8; 4] = *b"EEPK";
pub const PACK_BINARY_VERSION_V1: u16 = 1;
pub const PACK_BINARY_HEADER_LEN: usize = 56;
pub const PACK_BINARY_ITEM_TABLE_ENTRY_LEN: usize = 16;
pub const PACK_BINARY_TRAILER_LEN: usize = 20;
pub const PACK_BINARY_FLAG_EXPLAIN_INCLUDED: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackBinaryError {
    MagicMismatch {
        found: [u8; 4],
    },
    VersionTooNew {
        found: u16,
        supported: u16,
    },
    Truncated {
        needed: usize,
        actual: usize,
    },
    ItemCountTooLarge {
        count: u64,
    },
    TotalBytesMismatch {
        declared: u64,
        actual: usize,
    },
    InvalidOffset {
        index: usize,
        offset: u64,
        len: u32,
        total: usize,
    },
    ContentHashMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    NonUtf8Json,
}

impl PackBinaryError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MagicMismatch { .. } => "pack_bin_magic_mismatch",
            Self::VersionTooNew { .. } => "pack_bin_version_too_new",
            Self::Truncated { .. } => "pack_bin_truncated",
            Self::ItemCountTooLarge { .. } => "pack_bin_item_count_too_large",
            Self::TotalBytesMismatch { .. } => "pack_bin_total_bytes_mismatch",
            Self::InvalidOffset { .. } => "pack_bin_invalid_offset",
            Self::ContentHashMismatch { .. } => "pack_bin_content_hash_mismatch",
            Self::NonUtf8Json => "pack_bin_json_non_utf8",
        }
    }
}

impl fmt::Display for PackBinaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicMismatch { found } => write!(
                formatter,
                "binary pack magic mismatch: expected EEPK, found {}",
                hex_bytes(found)
            ),
            Self::VersionTooNew { found, supported } => write!(
                formatter,
                "binary pack version {found} is newer than supported version {supported}"
            ),
            Self::Truncated { needed, actual } => {
                write!(
                    formatter,
                    "binary pack is truncated: needed at least {needed} bytes, found {actual}"
                )
            }
            Self::ItemCountTooLarge { count } => {
                write!(
                    formatter,
                    "binary pack item count {count} does not fit this host"
                )
            }
            Self::TotalBytesMismatch { declared, actual } => write!(
                formatter,
                "binary pack total_bytes mismatch: header declares {declared}, file has {actual}"
            ),
            Self::InvalidOffset {
                index,
                offset,
                len,
                total,
            } => write!(
                formatter,
                "binary pack item offset {index} points outside the frame: offset={offset}, len={len}, total={total}"
            ),
            Self::ContentHashMismatch { .. } => {
                formatter.write_str("binary pack content_hash does not match canonical JSON")
            }
            Self::NonUtf8Json => formatter.write_str("binary pack canonical JSON is not UTF-8"),
        }
    }
}

impl std::error::Error for PackBinaryError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackBinaryHeader {
    pub version: u16,
    pub flags: u16,
    pub item_count: usize,
    pub total_bytes: usize,
    pub content_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PackBinaryItemEntry {
    offset: usize,
    len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackBinaryEvent {
    pub schema: &'static str,
    pub kind: &'static str,
    pub operation: &'static str,
    pub bytes: usize,
    pub items: usize,
    pub content_hash: String,
    pub elapsed_us: u64,
    pub endianness_swap_required: bool,
}

impl PackBinaryEvent {
    #[must_use]
    pub fn to_json_line(&self) -> String {
        format!(
            "{{\"schema\":\"{}\",\"kind\":\"{}\",\"operation\":\"{}\",\"bytes\":{},\"items\":{},\"content_hash\":\"{}\",\"elapsed_us\":{},\"endianness_swap_required\":{}}}",
            self.schema,
            self.kind,
            self.operation,
            self.bytes,
            self.items,
            self.content_hash,
            self.elapsed_us,
            self.endianness_swap_required
        )
    }
}

#[derive(Clone, Debug)]
pub struct PackBinaryView<'a> {
    bytes: &'a [u8],
    header: PackBinaryHeader,
    entries: Vec<PackBinaryItemEntry>,
    canonical_json_range: Range<usize>,
}

impl<'a> PackBinaryView<'a> {
    /// Parse and validate an `ee.pack.bin.v1` frame.
    ///
    /// # Errors
    ///
    /// Returns a [`PackBinaryError`] when the frame is truncated, has a bad
    /// magic/version, contains invalid item offsets, or its content hash does
    /// not match the canonical JSON footer.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PackBinaryError> {
        ensure_len(bytes, PACK_BINARY_HEADER_LEN + PACK_BINARY_TRAILER_LEN)?;
        let found_magic = read_magic(bytes);
        if found_magic != PACK_BINARY_MAGIC {
            return Err(PackBinaryError::MagicMismatch { found: found_magic });
        }

        let version = read_u16(bytes, 4)?;
        if version > PACK_BINARY_VERSION_V1 {
            return Err(PackBinaryError::VersionTooNew {
                found: version,
                supported: PACK_BINARY_VERSION_V1,
            });
        }
        let flags = read_u16(bytes, 6)?;
        let raw_item_count = read_u64(bytes, 8)?;
        let item_count =
            usize::try_from(raw_item_count).map_err(|_| PackBinaryError::ItemCountTooLarge {
                count: raw_item_count,
            })?;
        let declared_total = read_u64(bytes, 16)?;
        let declared_total_usize =
            usize::try_from(declared_total).map_err(|_| PackBinaryError::TotalBytesMismatch {
                declared: declared_total,
                actual: bytes.len(),
            })?;
        if declared_total_usize != bytes.len() {
            return Err(PackBinaryError::TotalBytesMismatch {
                declared: declared_total,
                actual: bytes.len(),
            });
        }

        let table_len = item_count
            .checked_mul(PACK_BINARY_ITEM_TABLE_ENTRY_LEN)
            .ok_or(PackBinaryError::ItemCountTooLarge {
                count: raw_item_count,
            })?;
        let blob_start = PACK_BINARY_HEADER_LEN.checked_add(table_len).ok_or(
            PackBinaryError::ItemCountTooLarge {
                count: raw_item_count,
            },
        )?;
        ensure_len(bytes, blob_start + PACK_BINARY_TRAILER_LEN)?;

        let trailer_offset = bytes.len() - PACK_BINARY_TRAILER_LEN;
        let canonical_json_offset = read_u64(bytes, trailer_offset)?;
        let canonical_json_len = read_u64(bytes, trailer_offset + 8)?;
        let canonical_json_start =
            usize::try_from(canonical_json_offset).map_err(|_| PackBinaryError::InvalidOffset {
                index: item_count,
                offset: canonical_json_offset,
                len: 0,
                total: bytes.len(),
            })?;
        let canonical_json_len_usize =
            usize::try_from(canonical_json_len).map_err(|_| PackBinaryError::InvalidOffset {
                index: item_count,
                offset: canonical_json_offset,
                len: u32::MAX,
                total: bytes.len(),
            })?;
        let canonical_json_end = checked_range_end(
            item_count,
            canonical_json_start,
            canonical_json_len_usize,
            bytes.len(),
        )?;
        if canonical_json_start < blob_start || canonical_json_end > trailer_offset {
            return Err(PackBinaryError::InvalidOffset {
                index: item_count,
                offset: canonical_json_offset,
                len: u32::try_from(canonical_json_len_usize).unwrap_or(u32::MAX),
                total: bytes.len(),
            });
        }

        let mut content_hash = [0_u8; 32];
        content_hash.copy_from_slice(&bytes[24..56]);
        let actual_hash =
            *blake3::hash(&bytes[canonical_json_start..canonical_json_end]).as_bytes();
        if content_hash != actual_hash {
            return Err(PackBinaryError::ContentHashMismatch {
                expected: content_hash,
                actual: actual_hash,
            });
        }

        let mut entries = Vec::with_capacity(item_count);
        for index in 0..item_count {
            let entry_offset = PACK_BINARY_HEADER_LEN + index * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
            let raw_offset = read_u64(bytes, entry_offset)?;
            let raw_len = read_u32(bytes, entry_offset + 8)?;
            let offset =
                usize::try_from(raw_offset).map_err(|_| PackBinaryError::InvalidOffset {
                    index,
                    offset: raw_offset,
                    len: raw_len,
                    total: bytes.len(),
                })?;
            let len = usize::try_from(raw_len).map_err(|_| PackBinaryError::InvalidOffset {
                index,
                offset: raw_offset,
                len: raw_len,
                total: bytes.len(),
            })?;
            let end = checked_range_end(index, offset, len, bytes.len())?;
            if offset < blob_start || end > canonical_json_start {
                return Err(PackBinaryError::InvalidOffset {
                    index,
                    offset: raw_offset,
                    len: raw_len,
                    total: bytes.len(),
                });
            }
            entries.push(PackBinaryItemEntry { offset, len });
        }

        Ok(Self {
            bytes,
            header: PackBinaryHeader {
                version,
                flags,
                item_count,
                total_bytes: declared_total_usize,
                content_hash,
            },
            entries,
            canonical_json_range: canonical_json_start..canonical_json_end,
        })
    }

    #[must_use]
    pub const fn schema(&self) -> &'static str {
        PACK_BINARY_SCHEMA_V1
    }

    #[must_use]
    pub const fn header(&self) -> PackBinaryHeader {
        self.header
    }

    #[must_use]
    pub const fn item_count(&self) -> usize {
        self.header.item_count
    }

    #[must_use]
    pub fn content_hash_hex(&self) -> String {
        format!("blake3:{}", hex32(&self.header.content_hash))
    }

    /// Return a borrowed item-content slice directly from the frame.
    ///
    /// # Errors
    ///
    /// Returns [`PackBinaryError::InvalidOffset`] if `index` is out of range.
    pub fn item_slice(&self, index: usize) -> Result<&'a [u8], PackBinaryError> {
        let Some(entry) = self.entries.get(index) else {
            return Err(PackBinaryError::InvalidOffset {
                index,
                offset: 0,
                len: 0,
                total: self.bytes.len(),
            });
        };
        Ok(&self.bytes[entry.offset..entry.offset + entry.len])
    }

    #[must_use]
    pub fn canonical_json_bytes(&self) -> &'a [u8] {
        &self.bytes[self.canonical_json_range.clone()]
    }

    /// Return the canonical JSON footer as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`PackBinaryError::NonUtf8Json`] if the footer bytes are not
    /// valid UTF-8.
    pub fn canonical_json(&self) -> Result<&'a str, PackBinaryError> {
        std::str::from_utf8(self.canonical_json_bytes()).map_err(|_| PackBinaryError::NonUtf8Json)
    }

    #[must_use]
    pub fn to_json_bytes(&self) -> &'a [u8] {
        self.canonical_json_bytes()
    }

    #[must_use]
    pub fn event(&self, operation: &'static str, elapsed_us: u64) -> PackBinaryEvent {
        PackBinaryEvent {
            schema: "ee.test_event.v1",
            kind: "pack_binary",
            operation,
            bytes: self.bytes.len(),
            items: self.item_count(),
            content_hash: self.content_hash_hex(),
            elapsed_us,
            endianness_swap_required: false,
        }
    }
}

#[must_use]
pub fn serialize_context_response_binary(
    response: &ContextResponse,
    canonical_json: &str,
) -> Vec<u8> {
    let item_contents = response
        .data
        .pack
        .items
        .iter()
        .map(|item| item.content.as_bytes())
        .collect::<Vec<_>>();
    let flags = if response.data.pack_dna.is_some() {
        PACK_BINARY_FLAG_EXPLAIN_INCLUDED
    } else {
        0
    };
    serialize_pack_binary(canonical_json, &item_contents, flags)
}

#[must_use]
pub fn serialize_pack_binary(canonical_json: &str, item_contents: &[&[u8]], flags: u16) -> Vec<u8> {
    let canonical_json_bytes = canonical_json.as_bytes();
    let content_hash = blake3::hash(canonical_json_bytes);
    let table_len = item_contents.len() * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
    let blob_start = PACK_BINARY_HEADER_LEN + table_len;
    let item_blob_bytes = item_contents.iter().map(|item| item.len()).sum::<usize>();
    let canonical_json_offset = blob_start + item_blob_bytes;
    let total_bytes = canonical_json_offset + canonical_json_bytes.len() + PACK_BINARY_TRAILER_LEN;

    let mut frame = Vec::with_capacity(total_bytes);
    frame.extend_from_slice(&PACK_BINARY_MAGIC);
    frame.extend_from_slice(&PACK_BINARY_VERSION_V1.to_le_bytes());
    frame.extend_from_slice(&flags.to_le_bytes());
    frame.extend_from_slice(&(item_contents.len() as u64).to_le_bytes());
    frame.extend_from_slice(&(total_bytes as u64).to_le_bytes());
    frame.extend_from_slice(content_hash.as_bytes());

    let mut next_offset = blob_start;
    for item in item_contents {
        frame.extend_from_slice(&(next_offset as u64).to_le_bytes());
        // bd-2frot: saturate the per-item length field at u32::MAX instead of
        // truncating via `as u32`. Items above 4 GiB cannot fit the frame's
        // `len: u32` slot; the prior `as u32` cast dropped the high bits and
        // produced a frame whose item slice was the wrong size, surfacing as a
        // confusing `ContentHashMismatch` at parse time. Saturating to
        // u32::MAX instead immediately trips `end > canonical_json_start` in
        // [`PackBinaryView::parse`] and returns a clean `InvalidOffset` naming
        // the offset, the over-cap len, and the frame size.
        let len_field = u32::try_from(item.len()).unwrap_or(u32::MAX);
        frame.extend_from_slice(&len_field.to_le_bytes());
        frame.extend_from_slice(&0_u32.to_le_bytes());
        next_offset += item.len();
    }
    for item in item_contents {
        frame.extend_from_slice(item);
    }
    frame.extend_from_slice(canonical_json_bytes);
    frame.extend_from_slice(&(canonical_json_offset as u64).to_le_bytes());
    frame.extend_from_slice(&(canonical_json_bytes.len() as u64).to_le_bytes());
    frame.extend_from_slice(&0_u32.to_le_bytes());
    debug_assert_eq!(frame.len(), total_bytes);
    frame
}

fn ensure_len(bytes: &[u8], needed: usize) -> Result<(), PackBinaryError> {
    if bytes.len() < needed {
        Err(PackBinaryError::Truncated {
            needed,
            actual: bytes.len(),
        })
    } else {
        Ok(())
    }
}

fn checked_range_end(
    index: usize,
    offset: usize,
    len: usize,
    total: usize,
) -> Result<usize, PackBinaryError> {
    offset
        .checked_add(len)
        .filter(|end| *end <= total)
        .ok_or(PackBinaryError::InvalidOffset {
            index,
            offset: offset as u64,
            len: u32::try_from(len).unwrap_or(u32::MAX),
            total,
        })
}

fn read_magic(bytes: &[u8]) -> [u8; 4] {
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PackBinaryError> {
    ensure_len(bytes, offset + 2)?;
    Ok(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PackBinaryError> {
    ensure_len(bytes, offset + 4)?;
    Ok(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, PackBinaryError> {
    ensure_len(bytes, offset + 8)?;
    Ok(u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ]))
}

fn hex32(bytes: &[u8; 32]) -> String {
    hex_bytes(bytes)
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        PACK_BINARY_HEADER_LEN, PACK_BINARY_ITEM_TABLE_ENTRY_LEN, PACK_BINARY_TRAILER_LEN,
        serialize_pack_binary,
    };

    fn fixture_json() -> &'static str {
        r#"{"schema":"ee.response.v2","success":true,"data":{"pack":{"items":[]}}}"#
    }

    fn fixture_items() -> [&'static [u8]; 3] {
        [b"alpha", b"bravo", b"charlie"]
    }

    // bd-2dz4o: pin the 20-byte trailer (canonical_json_offset u64, canonical_json_len
    // u64, reserved u32) as a byte-level invariant. A future writer that repurposes the
    // reserved u32 or swaps offset/len would silently pass the existing parser-reject
    // tests in tests/pack_binary_format_unit.rs but trips this pin at the byte layer.
    #[test]
    fn trailer_layout_matches_canonical_offset_len_and_reserved_zero() {
        let json = fixture_json();
        let items = fixture_items();
        let item_slices: Vec<&[u8]> = items.iter().copied().collect();
        let frame = serialize_pack_binary(json, &item_slices, 0);

        let trailer_start = frame.len() - PACK_BINARY_TRAILER_LEN;
        let trailer = &frame[trailer_start..];
        assert_eq!(trailer.len(), PACK_BINARY_TRAILER_LEN);

        let raw_offset = u64::from_le_bytes(trailer[0..8].try_into().expect("offset bytes"));
        let raw_len = u64::from_le_bytes(trailer[8..16].try_into().expect("len bytes"));
        let raw_reserved = u32::from_le_bytes(trailer[16..20].try_into().expect("reserved bytes"));

        let table_len = items.len() * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
        let items_blob_bytes: usize = items.iter().map(|item| item.len()).sum();
        let expected_offset = (PACK_BINARY_HEADER_LEN + table_len + items_blob_bytes) as u64;

        assert_eq!(
            raw_offset, expected_offset,
            "canonical_json_offset position"
        );
        assert_eq!(
            raw_len,
            json.as_bytes().len() as u64,
            "canonical_json_len value"
        );
        assert_eq!(raw_reserved, 0, "trailer reserved u32 must remain 0");
    }

    // bd-2dz4o: pin per-item-table-entry reserved u32 (bytes 12..16 of each 16-byte
    // slot) as zero. If a writer ever starts encoding metadata into the reserved
    // padding, this pin fires so the change is explicit rather than a silent drift.
    #[test]
    fn item_table_reserved_u32_is_zero_for_every_entry() {
        let items = fixture_items();
        let item_slices: Vec<&[u8]> = items.iter().copied().collect();
        let frame = serialize_pack_binary(fixture_json(), &item_slices, 0);

        for (index, _) in items.iter().enumerate() {
            let entry_start = PACK_BINARY_HEADER_LEN + index * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
            let reserved = u32::from_le_bytes(
                frame[entry_start + 12..entry_start + 16]
                    .try_into()
                    .expect("entry reserved bytes"),
            );
            assert_eq!(
                reserved, 0,
                "item table entry {index} reserved u32 must be 0"
            );
        }
    }
}
