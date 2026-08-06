use std::sync::LazyLock;

use super::*;
use crate::bloom::BloomParameters;

static HASH: LazyLock<StorePathHash> =
    LazyLock::new(|| StorePathHash::new("s66mzxpvicwk07gjbjfw9izjfa797vsw").unwrap());
static PARAMETERS: LazyLock<BloomParameters> =
    LazyLock::new(|| BloomParameters::new(1 << 20, 10, 6 * 60 * 60, 5 * 60, 0.001).unwrap());

#[test]
fn events_round_trip() {
    for event in [
        BloomStreamEvent::path_created(&HASH, *PARAMETERS).unwrap(),
        BloomStreamEvent::Heartbeat,
    ] {
        let encoded = event.encode().unwrap();
        assert_eq!(
            BloomStreamEvent::decode(&encoded).unwrap(),
            DecodedEvent::Event(event)
        );
    }
}

#[test]
fn encoding_carries_version_and_kind() {
    let event = BloomStreamEvent::path_created(&HASH, *PARAMETERS).unwrap();
    let encoded: serde_json::Value = serde_json::from_slice(&event.encode().unwrap()).unwrap();

    assert_eq!(encoded["version"], BloomStreamEvent::CURRENT_VERSION);
    assert_eq!(encoded["kind"], "path_created");
}

/// A path-created event never carries the store path hash, in any encoding of it. It carries the
/// probe positions calculated from the supplied parameters instead.
#[test]
fn path_created_carries_positions_and_not_the_hash() {
    let parameters = *PARAMETERS;
    let positions = ProbePositions::of(&HASH, parameters).unwrap();
    assert_eq!(positions.m_bits(), parameters.m_bits());
    assert_eq!(
        positions.positions().len(),
        usize::from(parameters.probe_count())
    );

    let event = BloomStreamEvent::PathCreated(positions);
    let encoded = String::from_utf8(event.encode().unwrap()).unwrap();

    // The record carries exactly these fields; no hash field rides along.
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 4, "{encoded}");
    for key in ["version", "kind", "m_bits", "positions"] {
        assert!(object.contains_key(key), "{key} is missing: {encoded}");
    }

    // The raw record text never contains the hash, in any position.
    assert!(!encoded.contains(HASH.as_str()), "{encoded}");
}

/// A record whose positions violate an invariant fails to decode, so decoding never produces an
/// invalid [`ProbePositions`] value.
#[test]
fn invalid_positions_fail_to_decode() {
    for (reason, m_bits, positions) in [
        ("not a power of two", 1000, vec![3]),
        ("no positions at all", 1024, vec![]),
        ("a position beyond the claimed size", 1024, vec![1024]),
        (
            "more positions than the format permits",
            1024,
            (0..65).collect(),
        ),
    ] {
        let record = serde_json::json!({
            "version": BloomStreamEvent::CURRENT_VERSION,
            "kind": "path_created",
            "m_bits": m_bits,
            "positions": positions,
        });
        let encoded = serde_json::to_vec(&record).unwrap();
        assert!(BloomStreamEvent::decode(&encoded).is_err(), "{reason}");
    }
}

/// Heartbeats carry only the wire discriminators.
#[test]
fn heartbeat_has_no_payload() {
    let encoded = BloomStreamEvent::Heartbeat.encode().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let object = value.as_object().unwrap();

    assert_eq!(object.len(), 2);
    assert_eq!(object["version"], BloomStreamEvent::CURRENT_VERSION);
    assert_eq!(object["kind"], "heartbeat");
}

/// An unsupported version is recognized from the version alone; the other fields of the record are
/// free to change shape.
#[test]
fn unsupported_version_is_recognized_without_the_fields() {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "version": 255,
        "kind": "path_created",
        "store_path_hash": { "reshaped": true },
    }))
    .unwrap();

    assert_eq!(
        BloomStreamEvent::decode(&encoded).unwrap(),
        DecodedEvent::UnsupportedVersion(255)
    );
}
