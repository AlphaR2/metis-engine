use criterion::{black_box, criterion_group, criterion_main, Criterion};
use openstake_core::settlement::payout::compute_payout;

fn bench_compute_payout(c: &mut Criterion) {
    c.bench_function("compute_payout at realistic scale", |b| {
        b.iter(|| {
            compute_payout(
                black_box(253_488_139u64),
                black_box(360_000_000u64),
                black_box(509_112_351u64),
            )
        })
    });
}

criterion_group!(benches, bench_compute_payout);
criterion_main!(benches);
