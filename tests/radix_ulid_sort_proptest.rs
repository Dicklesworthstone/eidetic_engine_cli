use ee::util::radix_ulid_sort::{RadixUlidSortErrorKind, sort_by_ulid_payload};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestCaseError};

const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn payload_strategy() -> impl Strategy<Value = String> {
    (
        0_usize..8,
        proptest::collection::vec(0_usize..DIGITS.len(), 25),
    )
        .prop_map(|(first, rest)| {
            let mut bytes = Vec::with_capacity(26);
            bytes.push(DIGITS[first]);
            bytes.extend(rest.into_iter().map(|index| DIGITS[index]));
            String::from_utf8(bytes).expect("strategy emits ASCII ULID payload")
        })
}

fn row_strategy() -> impl Strategy<Value = (String, usize)> {
    (payload_strategy(), 0_usize..8)
        .prop_map(|(payload, duplicate_bucket)| (format!("mem_{payload}"), duplicate_bucket))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn radix_sort_matches_stable_reference_sort(mut rows in proptest::collection::vec(row_strategy(), 0..256)) {
        let mut expected = rows.clone();
        expected.sort_by(|left, right| left.0[4..].cmp(&right.0[4..]));

        sort_by_ulid_payload(&mut rows, |row| row.0.as_str())
            .map_err(|error| TestCaseError::fail(error.to_string()))?;

        prop_assert_eq!(rows, expected);
    }

    #[test]
    fn radix_sort_accepts_bare_payloads(mut payloads in proptest::collection::vec(payload_strategy(), 0..256)) {
        let mut expected = payloads.clone();
        expected.sort();

        sort_by_ulid_payload(&mut payloads, String::as_str)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;

        prop_assert_eq!(payloads, expected);
    }
}

#[test]
fn rejects_missing_payload_without_mutating_input() {
    let original = vec![
        "mem_short".to_owned(),
        "mem_01J0000000000000000000000A".to_owned(),
    ];
    let mut rows = original.clone();

    let error = sort_by_ulid_payload(&mut rows, String::as_str)
        .expect_err("missing payload should fail before sorting");

    assert_eq!(rows, original);
    assert_eq!(error.index(), 0);
    assert_eq!(error.kind(), RadixUlidSortErrorKind::MissingPayload);
}
