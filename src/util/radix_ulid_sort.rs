//! Stable radix sorting for canonical ULID payload tie-breakers.
//!
//! Public `ee` IDs use a `<prefix>_<26-char-ulid-payload>` shape. Hot ranking
//! paths often need a deterministic final tie-break on that payload after
//! higher-priority scores have already tied. This module keeps that comparison
//! integer-only and stable for equal IDs.

use std::cmp::Ordering;
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

    let mut rows = items.drain(..).zip(payload_offsets).collect::<Vec<_>>();
    for position in (0..ULID_PAYLOAD_LEN).rev() {
        let mut counts = [0_usize; RADIX];
        for (item, offset) in &rows {
            let digit = digit_value(key(item).as_bytes()[*offset + position]);
            counts[usize::from(digit)] += 1;
        }

        let mut offsets = [0_usize; RADIX];
        let mut next = 0_usize;
        for (digit, count) in counts.into_iter().enumerate() {
            offsets[digit] = next;
            next += count;
        }

        let len = rows.len();
        let mut distributed = Vec::with_capacity(len);
        distributed.resize_with(len, || None);
        for (item, offset) in rows {
            let digit = usize::from(digit_value(key(&item).as_bytes()[offset + position]));
            let slot = offsets[digit];
            offsets[digit] += 1;
            distributed[slot] = Some((item, offset));
        }

        rows = Vec::with_capacity(len);
        for row in distributed.into_iter().flatten() {
            rows.push(row);
        }
        debug_assert_eq!(rows.len(), len);
    }

    sort_equal_payload_runs_by_full_key(&mut rows, &key);
    items.extend(rows.into_iter().map(|(item, _)| item));
    Ok(())
}

/// Stable-sort by canonical ULID payload when all keys support it, otherwise
/// fall back to ordinary lexical key ordering.
///
/// This is intended for production hot paths that should benefit from radix
/// sorting for normal public `ee` IDs while still accepting synthetic fixtures
/// and imported IDs that do not end in canonical ULID payloads.
pub fn sort_by_ulid_payload_or_lexical<T, F>(items: &mut Vec<T>, key: F)
where
    F: Fn(&T) -> &str,
{
    if items
        .iter()
        .enumerate()
        .all(|(index, item)| validate_payload_key(index, key(item)).is_ok())
    {
        let _ = sort_by_ulid_payload(items, key);
    } else {
        items.sort_by(|left, right| key(left).cmp(key(right)));
    }
}

/// Compare two keys by canonical ULID payload when both support it.
///
/// If either key is not a bare payload or public `<prefix>_<payload>` ID, this
/// falls back to ordinary lexical ordering so fixture and imported IDs keep the
/// same deterministic behavior as [`sort_by_ulid_payload_or_lexical`].
#[must_use]
pub fn compare_ulid_payload_or_lexical(left: &str, right: &str) -> Ordering {
    match (
        validate_payload_key(0, left),
        validate_payload_key(1, right),
    ) {
        (Ok(left_offset), Ok(right_offset)) => {
            compare_validated_payloads(left, left_offset, right, right_offset)
                .then_with(|| left.cmp(right))
        }
        _ => left.cmp(right),
    }
}

fn sort_equal_payload_runs_by_full_key<T, F>(rows: &mut [(T, usize)], key: &F)
where
    F: Fn(&T) -> &str,
{
    let mut run_start = 0;
    while run_start < rows.len() {
        let mut run_end = run_start + 1;
        let (first_item, first_offset) = &rows[run_start];
        while run_end < rows.len() {
            let (next_item, next_offset) = &rows[run_end];
            if compare_validated_payloads(
                key(first_item),
                *first_offset,
                key(next_item),
                *next_offset,
            ) != Ordering::Equal
            {
                break;
            }
            run_end += 1;
        }
        if run_end - run_start > 1 {
            rows[run_start..run_end].sort_by(|(left, _), (right, _)| key(left).cmp(key(right)));
        }
        run_start = run_end;
    }
}

fn compare_validated_payloads(
    left: &str,
    left_offset: usize,
    right: &str,
    right_offset: usize,
) -> Ordering {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    for position in 0..ULID_PAYLOAD_LEN {
        let ordering = digit_value(left_bytes[left_offset + position])
            .cmp(&digit_value(right_bytes[right_offset + position]));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
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

fn payload_offset_opt(key: &str) -> Option<usize> {
    let bytes = key.as_bytes();
    if bytes.len() == ULID_PAYLOAD_LEN {
        return Some(0);
    }
    let offset = bytes.len().checked_sub(ULID_PAYLOAD_LEN)?;
    (offset > 0 && bytes.get(offset - 1) == Some(&b'_')).then_some(offset)
}

fn digit_value(byte: u8) -> u8 {
    digit_value_checked(byte).unwrap_or(0)
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
    use super::{
        RadixUlidSortErrorKind, compare_ulid_payload_or_lexical, sort_by_ulid_payload,
        sort_by_ulid_payload_or_lexical,
    };

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
    fn same_payload_distinct_keys_sort_by_full_key() -> Result<(), String> {
        let payload = "01J0000000000000000000000A";
        let mut rows = vec![
            (format!("rule_{payload}"), 0_usize),
            (format!("mem_{payload}"), 1),
            (format!("pack_{payload}"), 2),
            (format!("mem_{payload}"), 3),
        ];

        sort_by_ulid_payload(&mut rows, |row| &row.0).map_err(|error| error.to_string())?;

        let sorted = rows
            .iter()
            .map(|row| (row.0.clone(), row.1))
            .collect::<Vec<_>>();
        assert_eq!(
            sorted,
            vec![
                (format!("mem_{payload}"), 1),
                (format!("mem_{payload}"), 3),
                (format!("pack_{payload}"), 2),
                (format!("rule_{payload}"), 0),
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

    #[test]
    fn fallback_sort_accepts_non_ulid_fixture_ids() {
        let mut rows = vec![
            ("mem_fixture_c".to_owned(), 0_usize),
            ("mem_fixture_a".to_owned(), 1),
            ("mem_fixture_b".to_owned(), 2),
        ];

        sort_by_ulid_payload_or_lexical(&mut rows, |row| &row.0);

        let sorted_ids = rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>();
        assert_eq!(
            sorted_ids,
            vec!["mem_fixture_a", "mem_fixture_b", "mem_fixture_c"]
        );
    }

    #[test]
    fn payload_comparator_orders_canonical_ids_and_keeps_fixture_fallback() {
        assert_eq!(
            compare_ulid_payload_or_lexical(
                "mem_01J0000000000000000000000A",
                "mem_01J0000000000000000000000B",
            ),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_ulid_payload_or_lexical("mem_fixture_b", "mem_fixture_a"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn payload_comparator_ties_same_payload_by_full_key() {
        let payload = "01J0000000000000000000000A";
        assert_eq!(
            compare_ulid_payload_or_lexical(&format!("rule_{payload}"), &format!("mem_{payload}")),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_ulid_payload_or_lexical(&format!("mem_{payload}"), &format!("rule_{payload}")),
            std::cmp::Ordering::Less
        );
    }
}
