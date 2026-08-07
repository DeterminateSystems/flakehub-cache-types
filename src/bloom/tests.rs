use std::sync::LazyLock;

use super::*;

static HASH_A: LazyLock<StorePathHash> =
    LazyLock::new(|| StorePathHash::new("ia70ss13m22znbl8khrf2hq72qmh5drr").unwrap());
static HASH_B: LazyLock<StorePathHash> =
    LazyLock::new(|| StorePathHash::new("00000000000000000000000000000000").unwrap());

fn parameters(m_bits: u64, probe_count: u8) -> BloomParameters {
    parameters_with_fpr(m_bits, probe_count, 0.001)
}

fn parameters_with_fpr(
    m_bits: u64,
    probe_count: u8,
    max_accepted_false_positive_rate: f64,
) -> BloomParameters {
    BloomParameters::new(
        m_bits,
        probe_count,
        6 * 60 * 60,
        5 * 60,
        max_accepted_false_positive_rate,
    )
    .unwrap()
}

#[test]
fn inserted_items_are_never_negative_and_duplicates_are_free() {
    let parameters = parameters(1024, 10);
    let filter = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let positions = wire::ProbePositions::of(&HASH_A, parameters).unwrap();
    filter.insert_positions(&positions).unwrap();
    let after_first = filter.copied_bits();
    filter.insert_positions(&positions).unwrap();

    assert!(filter.contains(&HASH_A).unwrap());
    assert!(!filter.contains(&HASH_B).unwrap());
    assert_eq!(filter.copied_bits(), after_first);
}

/// The concurrent filter accepts inserts through a shared reference.
#[test]
fn concurrent_filter_probes_and_inserts() {
    let parameters = parameters(1024, 10);
    let filter = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let positions = wire::ProbePositions::of(&HASH_A, parameters).unwrap();
    filter.insert_positions(&positions).unwrap();

    assert!(filter.contains(&HASH_A).unwrap());
    assert!(!filter.contains(&HASH_B).unwrap());
    assert_eq!(filter.built_at(), 123);

    let positions = wire::ProbePositions::of(&HASH_B, parameters).unwrap();
    filter.insert_positions(&positions).unwrap();
    assert!(filter.contains(&HASH_B).unwrap());
}

#[test]
fn insertion_invalidates_a_decoded_snapshot_checksum() {
    let parameters = parameters(1024, 10);
    let source = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let decoded = ConcurrentBloomFilter::decode(parameters, &source.encode()).unwrap();
    let empty_checksum = decoded.body_checksum();

    let positions = wire::ProbePositions::of(&HASH_A, parameters).unwrap();
    decoded.insert_positions(&positions).unwrap();

    assert_ne!(decoded.body_checksum(), empty_checksum);
}

/// Positions calculated for an equal or larger filter mask down to exactly the positions this
/// filter calculates itself, so a probe of the same hash turns positive.
#[test]
fn streamed_positions_mask_down_to_local_probes() {
    let filter = ConcurrentBloomFilter::new(parameters(1024, 10), 123).unwrap();

    let larger_parameters = parameters(1 << 20, 12);
    let positions = wire::ProbePositions::of(&HASH_A, larger_parameters).unwrap();

    filter.insert_positions(&positions).unwrap();
    assert!(filter.contains(&HASH_A).unwrap());
    assert!(!filter.contains(&HASH_B).unwrap());
}

/// Positions calculated for a smaller filter have discarded high bits, so the record is rejected
/// and no bit changes.
#[test]
fn positions_from_a_smaller_filter_are_rejected() {
    let filter = ConcurrentBloomFilter::new(parameters(1024, 10), 123).unwrap();
    let empty_bits = filter.copied_bits();

    // Valid in itself, but calculated against a smaller filter.
    let positions: wire::ProbePositions =
        serde_json::from_value(serde_json::json!({ "m_bits": 8, "positions": [3] })).unwrap();

    assert!(matches!(
        filter.insert_positions(&positions),
        Err(BloomError::IncompatiblePositions {
            event_m_bits: 8,
            filter_m_bits: 1024,
        })
    ));
    assert_eq!(filter.copied_bits(), empty_bits);
}

#[test]
fn positions_with_fewer_probes_than_the_filter_are_rejected() {
    let filter = ConcurrentBloomFilter::new(parameters(1024, 10), 123).unwrap();
    let empty_bits = filter.copied_bits();

    // Valid in itself, but calculated with a smaller probe count: applying it would leave probe
    // bits 10 and higher unset, leading to a false negative.
    let positions: wire::ProbePositions = serde_json::from_value(serde_json::json!({
        "m_bits": 1024,
        "positions": [1, 2, 3, 4],
    }))
    .unwrap();

    assert!(matches!(
        filter.insert_positions(&positions),
        Err(BloomError::InsufficientPositions { count: 4, k: 10 })
    ));
    assert_eq!(filter.copied_bits(), empty_bits);
}

/// A concurrent filter round-trips through a snapshot: the copy keeps the source bits and the live
/// inserts, carries the caller's freshness time, and encodes into decodable bytes.
#[test]
fn concurrent_filter_round_trips_through_a_snapshot() {
    let parameters = parameters(1024, 10);
    let mut filter = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let positions = wire::ProbePositions::of(&HASH_A, parameters).unwrap();
    filter.insert_positions(&positions).unwrap();
    filter.set_source_max_row_id(456);

    let positions = wire::ProbePositions::of(&HASH_B, parameters).unwrap();
    filter.insert_positions(&positions).unwrap();

    let copy = filter.to_snapshot(789);
    assert_eq!(copy.built_at(), 789);
    assert_eq!(copy.source_max_row_id(), 456);

    let decoded = ConcurrentBloomFilter::decode(parameters, &copy.encode()).unwrap();
    assert!(decoded.contains(&HASH_A).unwrap());
    assert!(decoded.contains(&HASH_B).unwrap());

    let mut streamed = Vec::new();
    copy.write_to(&mut streamed).unwrap();
    assert_eq!(streamed, copy.encode());
}

#[test]
fn full_filter_has_no_distinct_item_estimate() {
    let snapshot = ConcurrentBloomFilter::new(parameters(8, 1), 123).unwrap();
    for byte in &snapshot.bits {
        byte.store(u8::MAX, Ordering::Relaxed);
    }

    assert_eq!(snapshot.estimated_distinct_items(), None);
}

#[test]
fn snapshot_and_manifest_round_trip() {
    let parameters = parameters(1024, 10);
    let mut snapshot = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let positions = wire::ProbePositions::of(&HASH_A, parameters).unwrap();
    snapshot.insert_positions(&positions).unwrap();
    snapshot.set_source_max_row_id(456);

    let encoded = snapshot.encode_with_checksum();
    assert!(encoded.as_bytes().starts_with(&SNAPSHOT_MAGIC));
    assert_eq!(encoded.as_bytes(), snapshot.encode());

    let decoded = ConcurrentBloomFilter::decode(parameters, encoded.as_bytes()).unwrap();
    // The encoded value owns the metadata from its header, so later changes to the source filter
    // cannot produce a manifest that disagrees with the encoded bytes.
    snapshot.set_source_max_row_id(789);
    let generation: Uuid = "019bf5a7-f6e8-7ac0-b973-8536596bdb45".parse().unwrap();
    let manifest = SnapshotManifest::for_encoded_snapshot(
        &encoded,
        PathBuf::from("producer-chosen/location"),
        generation,
        1_700_000_000,
    );
    let decoded_manifest = SnapshotManifest::decode(&manifest.encode().unwrap()).unwrap();
    decoded_manifest.validate_snapshot(&decoded).unwrap();
    let (concurrent, stats) = ConcurrentBloomFilter::decode_with_manifest(
        &decoded_manifest,
        encoded.as_bytes().chunks(7),
    )
    .unwrap();

    assert!(decoded.contains(&HASH_A).unwrap());
    assert!(concurrent.contains(&HASH_A).unwrap());
    assert_eq!(stats, snapshot.stats());
    assert_eq!(decoded.built_at(), 123);
    assert_eq!(concurrent.built_at(), 123);
    assert_eq!(decoded.source_max_row_id(), 456);
    assert_eq!(concurrent.source_max_row_id(), 456);
    assert_eq!(decoded_manifest.source_generation, generation);
    assert_eq!(decoded_manifest.source_max_row_id, 456);
    assert_eq!(decoded_manifest.replay_from, 1_700_000_000);
    assert_eq!(decoded_manifest.parameters, parameters);
    assert_eq!(concurrent.parameters(), parameters);

    // Parameters and the replay timestamp are required manifest metadata.
    for required in ["parameters", "replay_from"] {
        let mut incomplete: serde_json::Value =
            serde_json::from_slice(&manifest.encode().unwrap()).unwrap();
        incomplete
            .as_object_mut()
            .unwrap()
            .remove(required)
            .unwrap();
        SnapshotManifest::decode(&serde_json::to_vec(&incomplete).unwrap()).unwrap_err();
    }
}

#[test]
fn corruption_is_rejected() {
    let parameters = parameters(1024, 10);
    let snapshot = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let generation: Uuid = "019bf5a7-f6e8-7ac0-b973-8536596bdb45".parse().unwrap();
    let manifest = SnapshotManifest::for_snapshot(
        &snapshot,
        PathBuf::from("producer-chosen/location"),
        generation,
        1_700_000_000,
    );
    let mut encoded = snapshot.encode();
    encoded[SNAPSHOT_HEADER_LEN] ^= 1;

    assert!(matches!(
        ConcurrentBloomFilter::decode(parameters, &encoded),
        Err(BloomError::ChecksumMismatch { .. })
    ));
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(&manifest, std::iter::once(encoded.as_slice())),
        Err(BloomError::ChecksumMismatch { .. })
    ));
}

#[test]
fn manifest_aware_decode_rejects_wrong_manifest_and_lengths() {
    let mut snapshot = ConcurrentBloomFilter::new(parameters(1024, 10), 123).unwrap();
    snapshot.set_source_max_row_id(456);
    let generation: Uuid = "019bf5a7-f6e8-7ac0-b973-8536596bdb45".parse().unwrap();
    let encoded = snapshot.encode_with_checksum();
    let manifest = SnapshotManifest::for_encoded_snapshot(
        &encoded,
        PathBuf::from("producer-chosen/location"),
        generation,
        1_700_000_000,
    );

    SnapshotManifest::decode(&manifest.encode().unwrap()).unwrap();
    ConcurrentBloomFilter::decode_with_manifest(&manifest, std::iter::once(encoded.as_bytes()))
        .unwrap();

    let mut wrong_manifest = manifest.clone();
    wrong_manifest.body_checksum = "0".repeat(64);
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(
            &wrong_manifest,
            std::iter::once(encoded.as_bytes())
        ),
        Err(BloomError::ManifestSnapshotMismatch)
    ));

    let mut wrong_manifest = manifest.clone();
    wrong_manifest.parameters = parameters(2048, 10);
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(
            &wrong_manifest,
            std::iter::once(encoded.as_bytes())
        ),
        Err(BloomError::SnapshotParameterMismatch { .. })
    ));

    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(
            &manifest,
            std::iter::once(&encoded.as_bytes()[..SNAPSHOT_HEADER_LEN - 1])
        ),
        Err(BloomError::Truncated { .. })
    ));
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(
            &manifest,
            std::iter::once(&encoded.as_bytes()[..encoded.as_bytes().len() - 1])
        ),
        Err(BloomError::InvalidBodyLength { .. })
    ));

    let mut overlong = encoded.as_bytes().to_vec();
    overlong.push(0);
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(&manifest, overlong.chunks(13)),
        Err(BloomError::InvalidBodyLength { .. })
    ));

    let mut oversized = encoded.as_bytes().to_vec();
    oversized[5..13].copy_from_slice(&(1_u64 << 63).to_le_bytes());
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(&manifest, std::iter::once(oversized)),
        Err(BloomError::SnapshotParameterMismatch { .. })
    ));
}

#[test]
fn configured_false_positive_rate_is_enforced() {
    let permissive = parameters_with_fpr(8, 1, 0.6);
    let snapshot = ConcurrentBloomFilter::new(permissive, 123).unwrap();
    snapshot.bits[0].store(0b0000_1111, Ordering::Relaxed);
    let generation: Uuid = "019bf5a7-f6e8-7ac0-b973-8536596bdb45".parse().unwrap();
    let manifest = SnapshotManifest::for_snapshot(
        &snapshot,
        PathBuf::from("producer-chosen/location"),
        generation,
        1_700_000_000,
    );
    let encoded = snapshot.encode();

    ConcurrentBloomFilter::decode(permissive, &encoded).unwrap();
    ConcurrentBloomFilter::decode_with_manifest(&manifest, std::iter::once(encoded.as_slice()))
        .unwrap();

    let restrictive = parameters_with_fpr(8, 1, 0.4);
    assert!(matches!(
        ConcurrentBloomFilter::decode(restrictive, &encoded),
        Err(BloomError::ExcessiveFalsePositiveRate { .. })
    ));
    let mut restrictive_manifest = manifest;
    restrictive_manifest.parameters = restrictive;
    assert!(matches!(
        ConcurrentBloomFilter::decode_with_manifest(
            &restrictive_manifest,
            std::iter::once(encoded.as_slice())
        ),
        Err(BloomError::ExcessiveFalsePositiveRate { .. })
    ));
}

#[test]
fn incompatible_definition_is_rejected() {
    let parameters = parameters(1024, 10);
    let snapshot = ConcurrentBloomFilter::new(parameters, 123).unwrap();
    let mut encoded = snapshot.encode();
    // Byte 4 holds the version, directly after the 4-byte magic.
    encoded[4] = FilterDefinition::CURRENT.version + 1;

    assert!(matches!(
        ConcurrentBloomFilter::decode(parameters, &encoded),
        Err(BloomError::IncompatibleDefinition { .. })
    ));
}

#[test]
fn parameters_validate_and_round_trip() {
    let parameters = BloomParameters::new(1024, 10, 21_600, 0, 0.001).unwrap();
    assert_eq!(parameters.m_bits(), 1024);
    assert_eq!(parameters.probe_count(), 10);
    assert_eq!(
        parameters.max_heartbeat_interval(),
        Duration::from_secs(21_600)
    );
    assert_eq!(parameters.clock_skew_slack(), Duration::ZERO);
    assert_eq!(parameters.max_accepted_false_positive_rate(), 0.001);

    let encoded = serde_json::to_vec(&parameters).unwrap();
    assert_eq!(
        serde_json::from_slice::<BloomParameters>(&encoded).unwrap(),
        parameters
    );

    let mut value = serde_json::to_value(parameters).unwrap();
    value.as_object_mut().unwrap().remove("probe-count");
    assert!(serde_json::from_value::<BloomParameters>(value).is_err());

    let mut value = serde_json::to_value(parameters).unwrap();
    value["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BloomParameters>(value).is_err());

    for result in [
        BloomParameters::new(7, 10, 1, 0, 0.001),
        BloomParameters::new(1000, 10, 1, 0, 0.001),
        BloomParameters::new(1024, 0, 1, 0, 0.001),
        BloomParameters::new(1024, MAX_PROBE_COUNT + 1, 1, 0, 0.001),
        BloomParameters::new(1024, 10, 0, 0, 0.001),
        BloomParameters::new(1024, 10, 1, 0, 0.0),
        BloomParameters::new(1024, 10, 1, 0, 1.0),
        BloomParameters::new(1024, 10, 1, 0, f64::NAN),
        BloomParameters::new(1024, 10, 1, 0, f64::INFINITY),
    ] {
        assert!(result.is_err());
    }
}

#[test]
fn snapshot_dimensions_must_match_parameters() {
    let encoded_parameters = parameters(1024, 10);
    let snapshot = ConcurrentBloomFilter::new(encoded_parameters, 123).unwrap();
    let encoded = snapshot.encode();

    for mismatched in [parameters(2048, 10), parameters(1024, 11)] {
        assert!(matches!(
            ConcurrentBloomFilter::decode(mismatched, &encoded),
            Err(BloomError::SnapshotParameterMismatch { .. })
        ));
    }
}

// XXX: These golden values pin the format contract. An incompatible change must also bump
// `FilterDefinition::CURRENT.version`, which places new snapshots in a separate namespace.
#[test]
fn definition_and_probe_behavior_are_golden() {
    assert_eq!(SNAPSHOT_MAGIC, *b"FBF1");
    assert_eq!(
        FilterDefinition::CURRENT.object_prefix(),
        Path::new("bloom/v1")
    );
    assert_eq!(
        decode_store_path_hash(&HASH_A).unwrap(),
        [
            57, 183, 2, 43, 22, 7, 67, 225, 50, 156, 136, 46, 251, 133, 168, 35, 104, 13, 142, 138,
        ]
    );
    assert_eq!(
        Dimensions::new(1024, 10)
            .unwrap()
            .probe_positions(&HASH_A)
            .unwrap()
            .collect::<Vec<_>>(),
        vec![825, 876, 927, 978, 5, 56, 107, 158, 209, 260]
    );
}

#[test]
fn compatibility_change_uses_a_new_namespace() {
    let current = FilterDefinition::CURRENT;
    let changed = FilterDefinition {
        version: current.version + 1,
    };

    assert_ne!(changed.object_prefix(), current.object_prefix());
}
