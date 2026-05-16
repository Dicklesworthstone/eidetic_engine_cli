use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ee::util::radix_ulid_sort::sort_by_ulid_payload;

#[derive(Clone)]
struct TieBreakRow {
    memory_id: String,
    ordinal: usize,
}

fn bench_rows(count: usize) -> Vec<TieBreakRow> {
    let mut rows = (0..count)
        .map(|index| TieBreakRow {
            memory_id: format!("mem_{index:026}"),
            ordinal: index,
        })
        .collect::<Vec<_>>();
    deterministic_shuffle(&mut rows);
    rows
}

fn deterministic_shuffle(rows: &mut [TieBreakRow]) {
    if rows.len() < 2 {
        return;
    }
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for index in (1..rows.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let swap_index = usize::try_from(state % u64::try_from(index + 1).unwrap())
            .expect("shuffle index fits usize");
        rows.swap(index, swap_index);
    }
}

fn bench_ulid_tiebreak_radix_vs_compare(criterion: &mut Criterion) {
    let input = bench_rows(10_000);
    let mut group = criterion.benchmark_group("ulid_tiebreak");

    group.bench_function("sort_by_key_compare_10k", |bench| {
        bench.iter(|| {
            let mut rows = black_box(input.clone());
            rows.sort_by_key(|row| row.memory_id.clone());
            black_box(rows.iter().map(|row| row.ordinal).sum::<usize>())
        });
    });

    group.bench_function("radix_payload_10k", |bench| {
        bench.iter(|| {
            let mut rows = black_box(input.clone());
            sort_by_ulid_payload(&mut rows, |row| row.memory_id.as_str())
                .expect("benchmark rows use canonical ULID-like payloads");
            black_box(rows.iter().map(|row| row.ordinal).sum::<usize>())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_ulid_tiebreak_radix_vs_compare);
criterion_main!(benches);
