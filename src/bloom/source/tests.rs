use super::*;

const GENERATION: Uuid = uuid::uuid!("019bf5a7-f6e8-7ac0-b973-8536596bdb45");

fn record(id: u64) -> SourceRecord {
    SourceRecord {
        id,
        store_path_hash: "ia70ss13m22znbl8khrf2hq72qmh5drr".to_owned(),
    }
}

#[test]
fn shard_keys_round_trip() {
    let key = ShardKey::new(GENERATION, ShardKind::Bootstrap, 12, 345, 1_700_000_000).unwrap();
    assert_eq!(
        key.to_key(),
        PathBuf::from(
            "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/bootstrap/12/345/1700000000.json"
        )
    );
    assert_eq!(key.kind(), ShardKind::Bootstrap);
    assert_eq!(ShardKey::parse(&key.to_key()).unwrap(), key);
}

/// A ready marker is not a shard key and must never parse as one.
#[test]
fn ready_markers_are_not_shard_keys() {
    let key = ShardKey::ready_marker_key(GENERATION);
    assert_eq!(
        key,
        PathBuf::from("bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ready")
    );
    assert!(ShardKey::parse(&key).is_err());
}

#[test]
fn malformed_shard_keys_fail_to_parse() {
    for key in [
        // A key without a shard kind is incomplete.
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/12/345.json",
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/12/345/1700000000.json",
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/other/12/345/1700000000.json",
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/12/345/1700000000",
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/12/345/1700000000.json/extra",
        "bloom/source/v1/not-a-uuid/ordinary/12/345/1700000000.json",
        "bloom/source/v2/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/12/345/1700000000.json",
        // Bounds violations.
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/0/345/1700000000.json",
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/12/11/1700000000.json",
        // Non-canonical numerals must not alias a canonical key.
        "bloom/source/v1/019bf5a7-f6e8-7ac0-b973-8536596bdb45/ordinary/012/345/1700000000.json",
    ] {
        assert!(ShardKey::parse(Path::new(key)).is_err(), "{key}");
    }
}

#[test]
fn shard_contents_validate_against_the_key() {
    let shard = SourceShard::new(
        GENERATION,
        ShardKind::Bootstrap,
        1_700_000_000,
        vec![record(1), record(4), record(9)],
    )
    .unwrap();
    assert_eq!(shard.key().first_id(), 1);
    assert_eq!(shard.key().last_id(), 9);
    assert_eq!(shard.kind(), ShardKind::Bootstrap);

    let decoded = SourceShard::decode(shard.key().clone(), &shard.encode().unwrap()).unwrap();
    assert_eq!(decoded, shard);

    // Contents whose bounds disagree with the key are rejected.
    let wrong_key = ShardKey::new(GENERATION, ShardKind::Bootstrap, 1, 10, 1_700_000_000).unwrap();
    assert!(matches!(
        SourceShard::decode(wrong_key, &shard.encode().unwrap()),
        Err(BloomError::ShardBoundsMismatch { .. })
    ));
}

#[test]
fn shards_reject_bad_orders_and_sizes() {
    assert!(matches!(
        SourceShard::new(GENERATION, ShardKind::Ordinary, 0, vec![]),
        Err(BloomError::EmptyShard)
    ));
    assert!(matches!(
        SourceShard::new(
            GENERATION,
            ShardKind::Ordinary,
            0,
            vec![record(2), record(2)]
        ),
        Err(BloomError::NonMonotonicShardRows)
    ));
}

#[test]
fn the_index_rejects_ordinary_overlap_and_exposes_the_watermark() {
    let shard =
        |kind, first, last, anchor| ShardKey::new(GENERATION, kind, first, last, anchor).unwrap();

    let index = ShardIndex::new(
        GENERATION,
        vec![
            shard(ShardKind::Ordinary, 200, 250, 40),
            shard(ShardKind::Ordinary, 1, 100, 10),
            shard(ShardKind::Ordinary, 101, 199, 20),
            shard(ShardKind::Bootstrap, 50, 300, 99),
            shard(ShardKind::Bootstrap, 50, 150, 98),
        ],
    )
    .unwrap();
    assert_eq!(index.watermark(), 250);
    assert_eq!(index.anchor_ts(), Some(40));
    assert_eq!(index.shards().len(), 5);
    assert_eq!(index.shards()[1].kind(), ShardKind::Bootstrap);
    assert_eq!(index.shards()[2].kind(), ShardKind::Bootstrap);
    assert_eq!(
        index
            .shards()
            .iter()
            .map(ShardKey::first_id)
            .collect::<Vec<_>>(),
        vec![1, 50, 50, 101, 200]
    );

    assert!(matches!(
        ShardIndex::new(
            GENERATION,
            vec![
                shard(ShardKind::Ordinary, 1, 100, 10),
                shard(ShardKind::Ordinary, 100, 150, 20)
            ]
        ),
        Err(BloomError::ShardOverlap { .. })
    ));

    let other_generation: Uuid = uuid::uuid!("019bf5a7-f6e8-7ac0-b973-8536596bdb46");
    assert!(matches!(
        ShardIndex::new(
            other_generation,
            vec![shard(ShardKind::Bootstrap, 1, 100, 10)]
        ),
        Err(BloomError::WrongShardGeneration { .. })
    ));
}

#[test]
fn an_empty_index_has_watermark_zero_and_no_anchor() {
    let index = ShardIndex::new(GENERATION, Vec::new()).unwrap();
    assert!(index.is_empty());
    assert_eq!(index.watermark(), 0);
    assert_eq!(index.anchor_ts(), None);
}

#[test]
fn an_overlay_only_index_is_not_empty_but_has_no_checkpoint() {
    let index = ShardIndex::new(
        GENERATION,
        vec![
            ShardKey::new(GENERATION, ShardKind::Bootstrap, 1, 100, 10).unwrap(),
            ShardKey::new(GENERATION, ShardKind::Bootstrap, 50, 150, 20).unwrap(),
        ],
    )
    .unwrap();

    assert!(!index.is_empty());
    assert_eq!(index.shards().len(), 2);
    assert_eq!(index.watermark(), 0);
    assert_eq!(index.anchor_ts(), None);
}
