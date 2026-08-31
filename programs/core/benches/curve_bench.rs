// Pure wall-clock benchmark of the curve math — no VM, no CU accounting
// (that lives in tests/ against the real assembled program). This just
// tells you whether the math itself is getting cheaper or more expensive
// as it evolves.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use openstake_core::amm::curve::{buy_shares, isqrt, symmetric_seed};

fn bench_isqrt(c: &mut Criterion) {
    c.bench_function("isqrt on a realistic sum-of-squares", |b| {
        b.iter(|| isqrt(black_box(253_488_139u128.pow(2) + 255_624_212u128.pow(2))))
    });
}

fn bench_buy_shares(c: &mut Criterion) {
    let pool = [black_box(253_488_139u64), black_box(255_624_212u64)];
    c.bench_function("buy_shares on a mature pool", |b| {
        b.iter(|| buy_shares(&pool, 0, black_box(100_000_000u64)))
    });
}

fn bench_symmetric_seed(c: &mut Criterion) {
    c.bench_function("symmetric_seed", |b| {
        b.iter(|| symmetric_seed(black_box(10_000_000u64), black_box(2u8)))
    });
}

criterion_group!(benches, bench_isqrt, bench_buy_shares, bench_symmetric_seed);
criterion_main!(benches);
