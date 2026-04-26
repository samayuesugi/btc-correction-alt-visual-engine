// benches/analytics_bench.rs
// cargo bench — measures throughput of core statistical functions

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use btc_market_reaction_engine::analytics::{pearson, mean_abs};

fn bench_pearson(c: &mut Criterion) {
    let mut group = c.benchmark_group("pearson_correlation");
    for n in [20, 50, 80, 200].iter() {
        let a: Vec<f64> = (0..*n).map(|i| (i as f64 * 0.01).sin()).collect();
        let b: Vec<f64> = (0..*n).map(|i| (i as f64 * 0.01 + 0.5).cos()).collect();
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |bench, _| {
            bench.iter(|| pearson(black_box(&a), black_box(&b)));
        });
    }
    group.finish();
}

fn bench_lag_scan(c: &mut Criterion) {
    let n = 80_usize;
    let btc:  Vec<f64> = (0..n).map(|i| (i as f64 * 0.02).sin() * 0.01).collect();
    let coin: Vec<f64> = (0..n).map(|i| (i as f64 * 0.02 + 1.0).sin() * 0.015).collect();

    c.bench_function("lag_scan_6_ticks", |b| {
        b.iter(|| {
            for k in 0_usize..=6 {
                if k < n {
                    let _ = pearson(
                        black_box(&btc[k..]),
                        black_box(&coin[..coin.len()-k.max(1)]),
                    );
                }
            }
        });
    });
}

criterion_group!(benches, bench_pearson, bench_lag_scan);
criterion_main!(benches);
