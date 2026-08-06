//! Parsed types for versioned, immutable Bloom source shards.
//!
//! JSON shard keys have the form
//! `bloom/source/v1/{generation}/{kind}/{first_id}/{last_id}/{anchor_ts}.json`. Ordinary shard
//! ranges must not overlap; bootstrap shards are overlays and may overlap other ranges.

#[cfg(test)]
mod tests;

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bloom::BloomError;

/// Maximum records in one source shard.
pub const SOURCE_SHARD_FLUSH_ROWS: usize = 100_000;

const SHARD_EXT: &str = "json";

/// One indexed store-path hash carried by a source shard.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    /// A positive, monotonically ordered source ID.
    pub id: u64,
    /// The textual store path hash. Validation is deferred until insertion into a filter, so one
    /// invalid record does not fail a whole shard.
    pub store_path_hash: String,
}

/// Whether a source shard contributes to the ordinary watermark or is an overlapping overlay.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShardKind {
    Ordinary,
    Bootstrap,
}

impl ShardKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Bootstrap => "bootstrap",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ordinary" => Some(Self::Ordinary),
            "bootstrap" => Some(Self::Bootstrap),
            _ => None,
        }
    }
}

/// The parsed key of one immutable source shard:
/// `bloom/source/v1/{generation}/{kind}/{first_id}/{last_id}/{anchor_ts}.json`.
///
/// Invariants, enforced at construction and at parse:
///
/// - `first_id` is at least 1,
/// - `last_id` is at least `first_id`,
/// - the string form round-trips exactly (no leading zeros, no aliases).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardKey {
    generation: Uuid,
    kind: ShardKind,
    first_id: u64,
    last_id: u64,
    anchor_ts: u64,
}

impl ShardKey {
    pub fn new(
        generation: Uuid,
        kind: ShardKind,
        first_id: u64,
        last_id: u64,
        anchor_ts: u64,
    ) -> Result<Self, BloomError> {
        if first_id == 0 || last_id < first_id {
            return Err(BloomError::InvalidShardBounds { first_id, last_id });
        }

        Ok(Self {
            generation,
            kind,
            first_id,
            last_id,
            anchor_ts,
        })
    }

    /// Canonical root key prefix spanning every generation.
    pub fn source_prefix() -> PathBuf {
        Path::new("bloom").join("source").join("v1")
    }

    /// Canonical key of a generation's ready marker.
    pub fn ready_marker_key(generation: Uuid) -> PathBuf {
        Self::source_prefix()
            .join(generation.to_string())
            .join("ready")
    }

    pub fn generation(&self) -> Uuid {
        self.generation
    }

    pub fn kind(&self) -> ShardKind {
        self.kind
    }

    /// The smallest row ID in the shard.
    pub fn first_id(&self) -> u64 {
        self.first_id
    }

    /// The largest row ID in the shard: the watermark contribution of this shard.
    pub fn last_id(&self) -> u64 {
        self.last_id
    }

    /// The Unix timestamp associated with the shard's last record.
    pub fn anchor_ts(&self) -> u64 {
        self.anchor_ts
    }

    /// The canonical key of this shard.
    pub fn to_key(&self) -> PathBuf {
        Self::source_prefix()
            .join(self.generation().to_string())
            .join(self.kind().as_str())
            .join(self.first_id().to_string())
            .join(self.last_id().to_string())
            .join(self.anchor_ts().to_string())
            .with_extension(SHARD_EXT)
    }

    /// Parses one shard key. A key that does not round-trip through [`Self::to_key`] fails, so
    /// every parsed key has exactly one string form.
    pub fn parse(key: &Path) -> Result<Self, BloomError> {
        let invalid = || BloomError::InvalidShardKey {
            key: key.to_owned(),
        };

        let relative = key
            .strip_prefix(Self::source_prefix())
            .map_err(|_| invalid())?;
        let mut components = relative.components();
        let mut next = || match components.next() {
            Some(Component::Normal(component)) => component.to_str().ok_or_else(invalid),
            _ => Err(invalid()),
        };

        let generation: Uuid = next()?.parse().map_err(|_| invalid())?;
        let kind = ShardKind::parse(next()?).ok_or_else(invalid)?;
        let first_id: u64 = next()?.parse().map_err(|_| invalid())?;
        let last_id: u64 = next()?.parse().map_err(|_| invalid())?;
        let file_name = next()?;
        if components.next().is_some() {
            return Err(invalid());
        }

        let anchor_ts: u64 = file_name
            .strip_suffix(&format!(".{SHARD_EXT}"))
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;

        let parsed =
            Self::new(generation, kind, first_id, last_id, anchor_ts).map_err(|_| invalid())?;
        if parsed.to_key() != key {
            return Err(invalid());
        }

        Ok(parsed)
    }
}

/// The decoded contents of one source shard, validated against its key.
///
/// Invariants: at least one and at most [`SOURCE_SHARD_FLUSH_ROWS`] records, strictly increasing
/// row IDs, and first/last IDs that equal the key's bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceShard {
    key: ShardKey,
    records: Vec<SourceRecord>,
}

impl SourceShard {
    /// Builds a shard from freshly scanned records. The key bounds come from the records
    /// themselves; `anchor_ts` is the `created_at` of the last record.
    pub fn new(
        generation: Uuid,
        kind: ShardKind,
        anchor_ts: u64,
        records: Vec<SourceRecord>,
    ) -> Result<Self, BloomError> {
        let (first_id, last_id) = match (records.first(), records.last()) {
            (Some(first), Some(last)) => (first.id, last.id),
            _ => return Err(BloomError::EmptyShard),
        };

        let key = ShardKey::new(generation, kind, first_id, last_id, anchor_ts)?;
        validate_records(&records, &key)?;

        Ok(Self { key, records })
    }

    /// Decodes shard contents and validates them against the parsed key.
    pub fn decode(key: ShardKey, encoded: &[u8]) -> Result<Self, BloomError> {
        let records: Vec<SourceRecord> =
            serde_json::from_slice(encoded).map_err(BloomError::ShardJson)?;
        validate_records(&records, &key)?;

        Ok(Self { key, records })
    }

    pub fn encode(&self) -> Result<Vec<u8>, BloomError> {
        let mut encoded = serde_json::to_vec(&self.records).map_err(BloomError::ShardJson)?;
        encoded.push(b'\n');
        Ok(encoded)
    }

    pub fn key(&self) -> &ShardKey {
        &self.key
    }

    pub fn kind(&self) -> ShardKind {
        self.key.kind()
    }

    pub fn records(&self) -> &[SourceRecord] {
        &self.records
    }
}

fn validate_records(records: &[SourceRecord], key: &ShardKey) -> Result<(), BloomError> {
    if records.is_empty() {
        return Err(BloomError::EmptyShard);
    }

    if records.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(BloomError::NonMonotonicShardRows);
    }

    let first = records.first().map(|record| record.id);
    let last = records.last().map(|record| record.id);
    if first != Some(key.first_id()) || last != Some(key.last_id()) {
        return Err(BloomError::ShardBoundsMismatch { key: key.to_key() });
    }

    Ok(())
}

/// An indexed collection of the shards in one generation.
///
/// Construction rejects overlapping ordinary row ranges. Bootstrap shards are overlays and may
/// overlap any shard; duplicate Bloom insertions are harmless. Gaps between ordinary shards are
/// legal because source IDs need not be contiguous.
#[derive(Clone, Debug)]
pub struct ShardIndex {
    generation: Uuid,
    /// Deterministically sorted by range, kind, and anchor. Ordinary ranges are pairwise disjoint.
    shards: Vec<ShardKey>,
}

impl ShardIndex {
    pub fn new(generation: Uuid, mut shards: Vec<ShardKey>) -> Result<Self, BloomError> {
        if let Some(foreign) = shards.iter().find(|shard| shard.generation() != generation) {
            return Err(BloomError::WrongShardGeneration {
                key: foreign.to_key(),
                expected: generation.to_string(),
            });
        }

        shards.sort_unstable_by_key(|shard| {
            (
                shard.first_id(),
                shard.last_id(),
                shard.kind(),
                shard.anchor_ts(),
            )
        });

        if let Some(overlapping) = shards
            .iter()
            .filter(|shard| shard.kind() == ShardKind::Ordinary)
            .collect::<Vec<_>>()
            .windows(2)
            .find(|pair| pair[1].first_id() <= pair[0].last_id())
        {
            return Err(BloomError::ShardOverlap {
                key: overlapping[1].to_key(),
            });
        }

        Ok(Self { generation, shards })
    }

    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// The largest row ID any ordinary shard covers, or 0 when there are no ordinary shards.
    pub fn watermark(&self) -> u64 {
        self.shards
            .iter()
            .filter(|shard| shard.kind() == ShardKind::Ordinary)
            .map(ShardKey::last_id)
            .max()
            .unwrap_or(0)
    }

    /// The anchor timestamp of the ordinary shard that holds the watermark row, or `None` when
    /// there are no ordinary shards.
    pub fn anchor_ts(&self) -> Option<u64> {
        self.shards
            .iter()
            .filter(|shard| shard.kind() == ShardKind::Ordinary)
            .max_by_key(|shard| shard.last_id())
            .map(ShardKey::anchor_ts)
    }

    /// The shards in ascending row-ID order.
    pub fn shards(&self) -> &[ShardKey] {
        &self.shards
    }

    pub fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }
}
