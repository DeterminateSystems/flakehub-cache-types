use std::hint::black_box;
use std::sync::LazyLock;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use flakehub_cache_types::bloom::wire::ProbePositions;
use flakehub_cache_types::bloom::{BloomParameters, ConcurrentBloomFilter, SnapshotManifest};
use flakehub_cache_types::nix_store::StorePathHash;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

const BUILT_AT: u64 = 1_700_000_000;
const REPLAY_FROM: u64 = 1_699_999_700;
static PARAMETERS: LazyLock<BloomParameters> =
    LazyLock::new(|| BloomParameters::new(1 << 31, 10, 6 * 60 * 60, 5 * 60, 0.001).unwrap());

fn snapshot_fixture() -> (ConcurrentBloomFilter, Uuid) {
    let parameters = *PARAMETERS;
    let mut snapshot = ConcurrentBloomFilter::new(parameters, BUILT_AT).unwrap();
    for id in 0_u64..7_642 {
        let digest = Sha256::digest(id.to_le_bytes());
        let encoded = nix_base32::to_nix_base32(&digest[..20]);
        let hash = StorePathHash::new(&encoded).unwrap();
        snapshot
            .insert_positions(&ProbePositions::of(&hash, parameters).unwrap())
            .unwrap();
    }
    snapshot.set_source_max_row_id(456);

    let generation = "019bf5a7-f6e8-7ac0-b973-8536596bdb45".parse().unwrap();
    (snapshot, generation)
}

fn snapshot_build(c: &mut Criterion) {
    let (snapshot, generation) = snapshot_fixture();

    // Warm the sparse, physically backed body before measuring snapshot preparation.
    black_box(snapshot.stats());

    let mut group = c.benchmark_group("snapshot_build_256_mib");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(15))
        .throughput(Throughput::Bytes(snapshot.body_len() as u64));

    group.bench_function("legacy_repeated_passes", |b| {
        b.iter(|| {
            let fill_ratio = snapshot.fill_ratio();
            let estimated_fpr = snapshot.estimated_false_positive_rate();
            let estimated_distinct_items = snapshot.estimated_distinct_items();
            let encoded = snapshot.encode();
            let manifest = SnapshotManifest::for_snapshot(
                &snapshot,
                "benchmark-snapshot".into(),
                generation,
                REPLAY_FROM,
            );
            black_box((
                fill_ratio,
                estimated_fpr,
                estimated_distinct_items,
                encoded,
                manifest,
            ))
        });
    });

    group.bench_function("prepare_snapshot", |b| {
        b.iter(|| {
            let stats = snapshot.stats();
            let encoded = snapshot.encode_with_checksum();
            let manifest = SnapshotManifest::for_encoded_snapshot(
                &encoded,
                "benchmark-snapshot".into(),
                generation,
                REPLAY_FROM,
            );
            black_box((stats, encoded, manifest))
        });
    });

    group.finish();
}

fn snapshot_decode(c: &mut Criterion) {
    let (snapshot, generation) = snapshot_fixture();
    let encoded = snapshot.encode_with_checksum();
    let manifest = SnapshotManifest::for_encoded_snapshot(
        &encoded,
        "benchmark-snapshot".into(),
        generation,
        REPLAY_FROM,
    );

    let mut group = c.benchmark_group("snapshot_decode_256_mib");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(20))
        .throughput(Throughput::Bytes(snapshot.body_len() as u64));

    group.bench_function("decode", |b| {
        b.iter(|| {
            let decoded =
                ConcurrentBloomFilter::decode(*PARAMETERS, black_box(encoded.as_bytes())).unwrap();
            black_box(decoded)
        });
    });

    group.bench_function("decode_and_validate_manifest", |b| {
        b.iter(|| {
            let decoded =
                ConcurrentBloomFilter::decode(*PARAMETERS, black_box(encoded.as_bytes())).unwrap();
            manifest.validate_snapshot(&decoded).unwrap();
            black_box(decoded)
        });
    });

    group.bench_function("decode_validate_and_calculate_stats", |b| {
        b.iter(|| {
            let decoded =
                ConcurrentBloomFilter::decode(*PARAMETERS, black_box(encoded.as_bytes())).unwrap();
            manifest.validate_snapshot(&decoded).unwrap();
            let stats = decoded.stats();
            black_box((decoded, stats))
        });
    });

    group.bench_function("manifest_decode_to_concurrent", |b| {
        b.iter(|| {
            let decoded = ConcurrentBloomFilter::decode_with_manifest(
                &manifest,
                std::iter::once(black_box(encoded.as_bytes())),
            )
            .unwrap();
            black_box(decoded)
        });
    });

    group.bench_function("manifest_decode_chunks_to_concurrent", |b| {
        b.iter(|| {
            let decoded = ConcurrentBloomFilter::decode_with_manifest(
                &manifest,
                black_box(encoded.as_bytes()).chunks(64 * 1024),
            )
            .unwrap();
            black_box(decoded)
        });
    });

    group.finish();
}

criterion_group!(benches, snapshot_build, snapshot_decode);
criterion_main!(benches);
