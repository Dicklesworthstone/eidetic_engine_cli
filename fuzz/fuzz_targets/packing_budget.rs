#![no_main]

use std::time::{Duration, Instant};

use ee::core::{BudgetDimension, BudgetExceeded, RequestBudget};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 65_536;
const MAX_RECORDS: usize = 64;
const MAX_CHECK_MILLIS: u64 = 365 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug)]
struct BudgetCase {
    wall_clock_ms: Option<u64>,
    tokens_limit: Option<u64>,
    memory_limit_bytes: Option<u64>,
    io_limit_bytes: Option<u64>,
    elapsed_ms: u64,
    records: [RecordOp; MAX_RECORDS],
    record_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct RecordOp {
    kind: RecordKind,
    amount: u64,
}

#[derive(Clone, Copy, Debug)]
enum RecordKind {
    Tokens,
    Memory,
    Io,
}

impl BudgetCase {
    const fn empty() -> Self {
        Self {
            wall_clock_ms: None,
            tokens_limit: None,
            memory_limit_bytes: None,
            io_limit_bytes: None,
            elapsed_ms: 0,
            records: [RecordOp {
                kind: RecordKind::Tokens,
                amount: 0,
            }; MAX_RECORDS],
            record_count: 0,
        }
    }

    fn push_record(&mut self, kind: RecordKind, amount: u64) {
        if self.record_count >= MAX_RECORDS {
            return;
        }
        self.records[self.record_count] = RecordOp { kind, amount };
        self.record_count += 1;
    }

    fn records(&self) -> &[RecordOp] {
        &self.records[..self.record_count]
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let case = serde_json::from_slice::<Value>(data)
        .ok()
        .map(|value| case_from_json(&value))
        .unwrap_or_else(|| case_from_raw(data));

    run_case(&case);
});

fn run_case(case: &BudgetCase) {
    let anchor = Instant::now();
    let mut budget = RequestBudget::unbounded_at(anchor);

    if let Some(limit) = case.wall_clock_ms {
        budget = budget.with_wall_clock(Duration::from_millis(limit));
    }
    if let Some(limit) = case.tokens_limit {
        budget = budget.with_tokens(limit);
    }
    if let Some(limit) = case.memory_limit_bytes {
        budget = budget.with_memory_bytes(limit);
    }
    if let Some(limit) = case.io_limit_bytes {
        budget = budget.with_io_bytes(limit);
    }

    let mut expected_tokens = 0_u64;
    let mut expected_memory = 0_u64;
    let mut expected_io = 0_u64;
    for record in case.records() {
        match record.kind {
            RecordKind::Tokens => {
                expected_tokens = expected_tokens.saturating_add(record.amount);
                budget.record_tokens(record.amount);
            }
            RecordKind::Memory => {
                expected_memory = expected_memory.saturating_add(record.amount);
                budget.record_memory_bytes(record.amount);
            }
            RecordKind::Io => {
                expected_io = expected_io.saturating_add(record.amount);
                budget.record_io_bytes(record.amount);
            }
        }
    }

    assert_eq!(budget.tokens_used(), expected_tokens);
    assert_eq!(budget.memory_used_bytes(), expected_memory);
    assert_eq!(budget.io_used_bytes(), expected_io);
    assert_snapshot(
        &budget,
        BudgetDimension::Tokens,
        case.tokens_limit,
        expected_tokens,
    );
    assert_snapshot(
        &budget,
        BudgetDimension::Memory,
        case.memory_limit_bytes,
        expected_memory,
    );
    assert_snapshot(
        &budget,
        BudgetDimension::Io,
        case.io_limit_bytes,
        expected_io,
    );

    let elapsed = Duration::from_millis(case.elapsed_ms.min(MAX_CHECK_MILLIS));
    let now = anchor.checked_add(elapsed).unwrap_or(anchor);

    let first = budget.check_at(now);
    let second = budget.check_at(now);
    assert_eq!(first, second);

    let expected = expected_budget_result(
        case,
        anchor,
        elapsed,
        expected_tokens,
        expected_memory,
        expected_io,
    );
    assert_eq!(first, expected);

    let remaining = budget.remaining_wall_clock_at(now);
    match wall_limit_millis(case, anchor) {
        Some(limit) => {
            let expected_remaining = limit.saturating_sub(elapsed.as_millis());
            assert_eq!(
                remaining.map(|value| value.as_millis()),
                Some(expected_remaining)
            );
        }
        None => assert!(remaining.is_none()),
    }
}

fn expected_budget_result(
    case: &BudgetCase,
    anchor: Instant,
    elapsed: Duration,
    tokens_used: u64,
    memory_used: u64,
    io_used: u64,
) -> Result<(), BudgetExceeded> {
    if let Some(limit) = wall_limit_millis(case, anchor) {
        let used = elapsed.as_millis();
        if used > limit {
            return Err(BudgetExceeded {
                dimension: BudgetDimension::WallClock,
                limit,
                used,
            });
        }
    }

    if let Some(limit) = case.tokens_limit
        && tokens_used > limit
    {
        return Err(BudgetExceeded {
            dimension: BudgetDimension::Tokens,
            limit: u128::from(limit),
            used: u128::from(tokens_used),
        });
    }

    if let Some(limit) = case.memory_limit_bytes
        && memory_used > limit
    {
        return Err(BudgetExceeded {
            dimension: BudgetDimension::Memory,
            limit: u128::from(limit),
            used: u128::from(memory_used),
        });
    }

    if let Some(limit) = case.io_limit_bytes
        && io_used > limit
    {
        return Err(BudgetExceeded {
            dimension: BudgetDimension::Io,
            limit: u128::from(limit),
            used: u128::from(io_used),
        });
    }

    Ok(())
}

fn wall_limit_millis(case: &BudgetCase, anchor: Instant) -> Option<u128> {
    let limit = case.wall_clock_ms?;
    let duration = Duration::from_millis(limit);
    anchor.checked_add(duration)?;
    Some(duration.as_millis())
}

fn assert_snapshot(
    budget: &RequestBudget,
    dimension: BudgetDimension,
    limit: Option<u64>,
    used: u64,
) {
    match (limit, budget.snapshot(dimension)) {
        (Some(limit), Some(snapshot)) => {
            assert_eq!(snapshot.dimension, dimension);
            assert_eq!(snapshot.limit, u128::from(limit));
            assert_eq!(snapshot.used, u128::from(used));
            assert_eq!(snapshot.is_exceeded(), used > limit);
        }
        (None, None) => {}
        other => panic!("unexpected snapshot state for {dimension:?}: {other:?}"),
    }
}

fn case_from_json(value: &Value) -> BudgetCase {
    let mut case = BudgetCase::empty();
    case.wall_clock_ms = field_u64(value, &["wallClockMs", "wall_clock_ms", "wall_ms"]);
    case.tokens_limit = field_u64(value, &["tokensLimit", "tokens_limit", "tokens"]);
    case.memory_limit_bytes =
        field_u64(value, &["memoryLimitBytes", "memory_limit_bytes", "memory"]);
    case.io_limit_bytes = field_u64(value, &["ioLimitBytes", "io_limit_bytes", "io"]);
    case.elapsed_ms = field_u64(value, &["elapsedMs", "elapsed_ms"]).unwrap_or_default();

    append_records_from_array(value.get("records"), &mut case);
    append_amounts_from_array(
        value
            .get("tokenRecords")
            .or_else(|| value.get("token_records")),
        RecordKind::Tokens,
        &mut case,
    );
    append_amounts_from_array(
        value
            .get("memoryRecords")
            .or_else(|| value.get("memory_records")),
        RecordKind::Memory,
        &mut case,
    );
    append_amounts_from_array(
        value.get("ioRecords").or_else(|| value.get("io_records")),
        RecordKind::Io,
        &mut case,
    );

    case
}

fn append_records_from_array(value: Option<&Value>, case: &mut BudgetCase) {
    let Some(records) = value.and_then(Value::as_array) else {
        return;
    };

    for record in records.iter().take(MAX_RECORDS) {
        if let Some(kind) = record
            .get("kind")
            .and_then(Value::as_str)
            .and_then(record_kind_from_str)
        {
            let amount = record
                .get("amount")
                .or_else(|| record.get("value"))
                .and_then(value_as_u64)
                .unwrap_or_default();
            case.push_record(kind, amount);
        }
    }
}

fn append_amounts_from_array(value: Option<&Value>, kind: RecordKind, case: &mut BudgetCase) {
    let Some(records) = value.and_then(Value::as_array) else {
        return;
    };

    for amount in records.iter().take(MAX_RECORDS).filter_map(value_as_u64) {
        case.push_record(kind, amount);
    }
}

fn case_from_raw(data: &[u8]) -> BudgetCase {
    let mut case = BudgetCase::empty();
    let flags = data.first().copied().unwrap_or_default();
    let mut offset = 1;

    let wall = read_u64(data, &mut offset);
    let tokens = read_u64(data, &mut offset);
    let memory = read_u64(data, &mut offset);
    let io = read_u64(data, &mut offset);
    let elapsed = read_u64(data, &mut offset);

    if flags & 0b0000_0001 != 0 {
        case.wall_clock_ms = Some(wall);
    }
    if flags & 0b0000_0010 != 0 {
        case.tokens_limit = Some(tokens);
    }
    if flags & 0b0000_0100 != 0 {
        case.memory_limit_bytes = Some(memory);
    }
    if flags & 0b0000_1000 != 0 {
        case.io_limit_bytes = Some(io);
    }
    case.elapsed_ms = elapsed;

    while offset < data.len() && case.record_count < MAX_RECORDS {
        let kind = match data.get(offset).copied().unwrap_or_default() % 3 {
            0 => RecordKind::Tokens,
            1 => RecordKind::Memory,
            _ => RecordKind::Io,
        };
        offset = offset.saturating_add(1);
        let amount = read_u64(data, &mut offset);
        case.push_record(kind, amount);
    }

    case
}

fn field_u64(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name))
        .and_then(value_as_u64)
}

fn value_as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

fn record_kind_from_str(value: &str) -> Option<RecordKind> {
    match value {
        "token" | "tokens" => Some(RecordKind::Tokens),
        "memory" | "memory_bytes" => Some(RecordKind::Memory),
        "io" | "io_bytes" => Some(RecordKind::Io),
        _ => None,
    }
}

fn read_u64(data: &[u8], offset: &mut usize) -> u64 {
    let mut bytes = [0_u8; 8];
    if let Some(slice) = data.get(*offset..offset.saturating_add(8)) {
        for (target, source) in bytes.iter_mut().zip(slice.iter().copied()) {
            *target = source;
        }
    }
    *offset = offset.saturating_add(8);
    u64::from_le_bytes(bytes)
}
