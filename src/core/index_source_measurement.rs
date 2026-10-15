//! Cancellable, integer-only sizing of a caller-owned source snapshot.
//!
//! A complete sum is needed only to admit a source. Once a nonnegative prefix
//! exceeds the allowance, the remaining rows cannot make it fit. Read at most
//! 128 row IDs and charges per page, never source text, using the rowid tables
//! in the compiled collector inventory. No OFFSET, aggregate over the whole
//! corpus, nested transaction, cache repair or new request context belongs here.
//! A synchronous database call or one large SQL value is not preemptible here.

use super::{DbConnection, IndexRebuildError};
use sqlmodel_core::Value;

const PAGE_ROWS: usize = 128;

fn invalid_page() -> IndexRebuildError {
    IndexRebuildError::Index("invalid live source measurement page".to_owned())
}

/// Return an exact total when it fits, or an over-budget prefix otherwise.
/// An over-budget value is exclusively a refusal witness, not a corpus total.
/// Identifiers, expressions and scope come from the binary's source inventory;
/// only the workspace value and cursor are bound caller data. Keep parentheses
/// around scope: an OR arm must not bypass the advancing rowid predicate.
pub(super) fn sum(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace: &str,
    table: &str,
    expression: &str,
    scope: &str,
    ceiling: u64,
) -> Result<u64, IndexRebuildError> {
    // Sources use either a table name or `table alias`, never a join here.
    // Qualifying the rowid also keeps inner ownership subqueries unambiguous.
    let qualifier = table.split_whitespace().last().ok_or_else(invalid_page)?;
    let cursor = format!("{qualifier}.rowid");
    let sql = format!(
        "SELECT {cursor}, ({expression}) FROM {table} WHERE ({scope}) AND {cursor} >= ?2 ORDER BY {cursor} LIMIT ?3"
    );
    sum_pages_with(cx, ceiling, |next| {
        let rows = db.query(
            &sql,
            &[
                Value::Text(workspace.to_owned()),
                Value::BigInt(next),
                Value::BigInt(PAGE_ROWS as i64),
            ],
        )?;
        rows.iter()
            .map(|row| {
                let id = row
                    .get(0)
                    .and_then(Value::as_i64)
                    .ok_or_else(invalid_page)?;
                let charge = row
                    .get(1)
                    .and_then(Value::as_i64)
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or_else(invalid_page)?;
                Ok((id, charge))
            })
            .collect()
    })
}

fn sum_pages_with(
    cx: &asupersync::Cx,
    ceiling: u64,
    mut page: impl FnMut(i64) -> Result<Vec<(i64, u64)>, IndexRebuildError>,
) -> Result<u64, IndexRebuildError> {
    let mut next = i64::MIN;
    let mut total = 0_u64;
    loop {
        super::super::index_checkpoint(cx)?;
        let measured = page(next);
        // Preserve cancellation even when the interrupted backend reports an
        // ordinary storage error, or the page would otherwise prove refusal.
        super::super::index_checkpoint(cx)?;
        let rows = measured?;
        if rows.len() > PAGE_ROWS {
            return Err(invalid_page());
        }
        let mut last = None;
        for &(id, charge) in &rows {
            super::super::index_checkpoint(cx)?;
            if id < next || last.is_some_and(|previous| id <= previous) {
                return Err(invalid_page());
            }
            last = Some(id);
            total = total.checked_add(charge).ok_or_else(invalid_page)?;
            if total > ceiling {
                return Ok(total);
            }
        }
        if rows.len() < PAGE_ROWS || last == Some(i64::MAX) {
            return Ok(total);
        }
        // Include negative rowids and zero, and never wrap at the upper bound.
        next = last
            .and_then(|id| id.checked_add(1))
            .ok_or_else(invalid_page)?;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn complete_pages_match_reference_sums_including_zero_cost_rows() {
        let cx = asupersync::Cx::for_testing();
        for count in [0, 1, PAGE_ROWS - 1, PAGE_ROWS, PAGE_ROWS + 1, 1025] {
            let rows: Vec<_> = (0..count)
                .map(|number| (number as i64 - 128, (number % 7) as u64))
                .collect();
            let expected = rows.iter().map(|(_, value)| value).sum::<u64>();
            for ceiling in [expected, expected + 1, u64::MAX] {
                let mut calls = 0;
                let actual = sum_pages_with(&cx, ceiling, |after| {
                    calls += 1;
                    Ok(rows
                        .iter()
                        .copied()
                        .filter(|(id, _)| *id >= after)
                        .take(PAGE_ROWS)
                        .collect())
                })
                .expect("complete numeric scan");
                assert_eq!(actual, expected);
                assert_eq!(calls, count / PAGE_ROWS + 1);
            }
        }
    }

    #[test]
    fn over_budget_prefix_does_not_request_the_rest_of_the_source() {
        let cx = asupersync::Cx::for_testing();
        let mut calls = 0;
        let result = sum_pages_with(&cx, 10, |_| {
            calls += 1;
            assert_eq!(calls, 1, "refusal must stop before the next page");
            Ok((0..PAGE_ROWS).map(|id| (id as i64, 1)).collect())
        })
        .expect("prefix refusal");
        assert_eq!(result, 11);
        assert_eq!(calls, 1);
    }

    #[test]
    fn rowid_extremes_are_included_without_overflow_or_repeating_the_final_page() {
        let cx = asupersync::Cx::for_testing();
        let rows = [(i64::MIN, 1), (-1, 2), (0, 3), (i64::MAX, 4)];
        assert_eq!(
            sum_pages_with(&cx, 10, |after| {
                assert_eq!(after, i64::MIN);
                Ok(rows.to_vec())
            })
            .expect("full-width rowids"),
            10
        );
        let mut calls = 0;
        let result = sum_pages_with(&cx, PAGE_ROWS as u64, |_| {
            calls += 1;
            assert_eq!(calls, 1);
            Ok((0..PAGE_ROWS)
                .map(|number| (i64::MAX - (PAGE_ROWS - 1 - number) as i64, 1))
                .collect())
        })
        .expect("full final page");
        assert_eq!(result, PAGE_ROWS as u64);
    }

    #[test]
    fn malformed_or_nonadvancing_pages_cannot_be_complete_measurements() {
        let cx = asupersync::Cx::for_testing();
        for rows in [
            vec![(0, 1), (0, 2)],
            vec![(1, 1), (0, 2)],
            vec![(0, 0); PAGE_ROWS + 1],
        ] {
            assert!(sum_pages_with(&cx, u64::MAX, |_| Ok(rows.clone())).is_err());
        }
        let mut calls = 0;
        assert!(
            sum_pages_with(&cx, u64::MAX, |_| {
                calls += 1;
                Ok((0..PAGE_ROWS).map(|id| (id as i64, 0)).collect())
            })
            .is_err()
        );
        assert_eq!(calls, 2);
        assert!(sum_pages_with(&cx, u64::MAX, |_| Ok(vec![(0, u64::MAX), (1, 1)])).is_err());
    }

    #[test]
    fn cancellation_precedes_query_failure_refusal_and_further_reads() {
        for failed in [false, true] {
            let cx = asupersync::Cx::for_testing();
            let mut calls = 0;
            let result = sum_pages_with(&cx, 0, |_| {
                calls += 1;
                cx.set_cancel_reason(asupersync::CancelReason::user("stop source measurement"));
                if failed {
                    Err(invalid_page())
                } else {
                    Ok(vec![(0, 1)])
                }
            });
            assert!(matches!(
                result,
                Err(IndexRebuildError::Cancelled(reason))
                    if reason.message.as_deref() == Some("stop source measurement")
            ));
            assert_eq!(calls, 1);
            assert!(matches!(
                sum_pages_with(&cx, u64::MAX, |_| {
                    panic!("cancelled measurement must not issue a query")
                }),
                Err(IndexRebuildError::Cancelled(_))
            ));
        }
    }
}
