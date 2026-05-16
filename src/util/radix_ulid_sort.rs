//! Stable radix sorting for canonical ULID payload tie-breakers.
//!
//! Public `ee` IDs use a `<prefix>_<26-char-ulid-payload>` shape. Hot ranking
//! paths often need a deterministic final tie-break on that payload after
//! higher-priority scores have already tied. This module keeps that comparison
//! integer-only and stable for equal IDs.

use std::fmt;

/// Canonical Crockford/ULID payload length in public `ee` IDs.
pub const ULID_PAYLOAD_LEN: usize = 26;

const RADIX: usize = 32;

/// Error returned when an input key does not contain a canonical ULID payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadixUlidSortError {
    index: usize,
    key: String,
    kind: RadixUlidSortErrorKind,
}

impl RadixUlidSortError {
    /// Input index whose key failed validation.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Key that failed validation.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Stable error classification.
    #[must_use]
    pub const fn kind(&self) -> RadixUlidSortErrorKind {
        self.kind
    }
}

impl fmt::Display for RadixUlidSortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RadixUlidSortErrorKind::MissingPayload => write!(
                formatter,
                "key at index {} does not contain a {}-character ULID payload: {}",
                self.index, ULID_PAYLOAD_LEN, self.key
            ),
            RadixUlidSortErrorKind::InvalidDigit { offset, byte } => write!(
                formatter,
                "key at index {} has invalid ULID digit 0x{byte:02x} at payload offset {offset}: {}",
                self.index, self.key
            ),
            RadixUlidSortErrorKind::InvalidLeadingDigit { byte } => write!(
                formatter,
                "key at index {} has invalid leading ULID digit 0x{byte:02x}: {}",
                self.index, self.key
            ),
        }
    }
}

impl std::error::Error for RadixUlidSortError {}

/// Stable error categories for ULID radix-sort validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadixUlidSortErrorKind {
    /// The key was neither a bare 26-character payload nor a prefixed ID whose
    /// final segment is a 26-character payload.
    MissingPayload,
    /// A payload byte is outside the canonical Crockford alphabet.
    InvalidDigit { offset: usize, byte: u8 },
    /// The first payload digit is outside `0..=7`, which would exceed 128 bits.
    InvalidLeadingDigit { byte: u8 },
}

/// Stable-sort `items` by each item's canonical ULID payload.
///
/// The key may be either a bare 26-character payload or a public `ee` ID ending
/// in `_<payload>`. All keys are validated before any item is moved, so errors
/// leave `items` in its original order.
///
/// # Errors
///
/// Returns [`RadixUlidSortError`] when any key is missing a payload or contains
/// a non-canonical payload digit.
pub fn sort_by_ulid_payload<T, F>(items: &mut Vec<T>, key: F) -> Result<(), RadixUlidSortError>
where
    F: Fn(&T) -> &str,
{
    let mut payload_offsets = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        payload_offsets.push(validate_payload_key(index, key(item))?);
    }

    for position in (0..ULID_PAYLOAD_LEN).rev() {
        let mut buckets: [Vec<T>; RADIX] = std::array::from_fn(|_| Vec::new());
        for (item, offset) in items.drain(..).zip(payload_offsets.drain(..)) {
            let digit = digit_value(key(&item).as_bytes()[offset + position]);
            buckets[usize::from(digit)].push(item);
        }
        for bucket in buckets {
            for item in bucket {
                payload_offsets.push(payload_offset(key(&item)));
                items.push(item);
            }
        }
    }

    Ok(())
}

fn validate_payload_key(index: usize, key: &str) -> Result<usize, RadixUlidSortError> {
    let offset = payload_offset_opt(key).ok_or_else(|| RadixUlidSortError {
        index,
        key: key.to_owned(),
        kind: RadixUlidSortErrorKind::MissingPayload,
    })?;
    let payload = &key.as_bytes()[offset..offset + ULID_PAYLOAD_LEN];
    for (position, byte) in payload.iter().copied().enumerate() {
        let value = digit_value_checked(byte).ok_or_else(|| RadixUlidSortError {
            index,
            key: key.to_owned(),
            kind: RadixUlidSortErrorKind::InvalidDigit {
                offset: position,
                byte,
            },
        })?;
        if position == 0 && value > 7 {
            return Err(RadixUlidSortError {
                index,
                key: key.to_owned(),
                kind: RadixUlidSortErrorKind::InvalidLeadingDigit { byte },
            });
        }
    }
    Ok(offset)
}

fn payload_offset(key: &str) -> usize {
    payload_offset_opt(key).expect("payload offset was validated before sorting")
}

fn payload_offset_opt(key: &str) -> Option<usize> {
    let bytes = key.as_bytes();
    if bytes.len() == ULID_PAYLOAD_LEN {
        return Some(0);
    }
    let offset = bytes.len().checked_sub(ULID_PAYLOAD_LEN)?;
    (offset > 0 && bytes.get(offset - 1) == Some(&b'_')).then_some(offset)
}

fn digit_value(byte: u8) -> u8 {
    digit_value_checked(byte).expect("ULID digit was validated before sorting")
}

fn digit_value_checked(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'H' => Some(byte - b'A' + 10),
        b'J'..=b'K' => Some(byte - b'J' + 18),
        b'M'..=b'N' => Some(byte - b'M' + 20),
        b'P'..=b'T' => Some(byte - b'P' + 22),
        b'V'..=b'Z' => Some(byte - b'V' + 27),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RadixUlidSortErrorKind, sort_by_ulid_payload};

    #[test]
    fn stable_radix_sort_matches_lexical_payload_order() -> Result<(), String> {
        let mut rows = vec![
            ("mem_01J0000000000000000000000C".to_owned(), 0_usize),
            ("mem_01J0000000000000000000000A".to_owned(), 1),
            ("mem_01J0000000000000000000000B".to_owned(), 2),
            ("mem_01J0000000000000000000000A".to_owned(), 3),
        ];

        sort_by_ulid_payload(&mut rows, |row| &row.0).map_err(|error| error.to_string())?;

        let sorted_ids = rows
            .iter()
            .map(|row| (row.0.as_str(), row.1))
            .collect::<Vec<_>>();
        assert_eq!(
            sorted_ids,
            vec![
                ("mem_01J0000000000000000000000A", 1),
                ("mem_01J0000000000000000000000A", 3),
                ("mem_01J0000000000000000000000B", 2),
                ("mem_01J0000000000000000000000C", 0),
            ]
        );
        Ok(())
    }

    #[test]
    fn invalid_payload_leaves_input_order_unchanged() {
        let original = vec![
            "mem_01J0000000000000000000000B".to_owned(),
            "mem_01J0000000000000000000000I".to_owned(),
            "mem_01J0000000000000000000000A".to_owned(),
        ];
        let mut rows = original.clone();

        let error = sort_by_ulid_payload(&mut rows, String::as_str)
            .expect_err("invalid Crockford digit should be rejected");

        assert_eq!(rows, original);
        assert_eq!(error.index(), 1);
        assert_eq!(
            error.kind(),
            RadixUlidSortErrorKind::InvalidDigit {
                offset: 25,
                byte: b'I'
            }
        );
    }
}
