//! Versioned Bloom-filter snapshot and manifest formats.
//!
//! There are three kinds of parameters:
//!
//! - The [`FilterDefinition`] version changes how bytes are interpreted. A version bump creates
//!   a separate namespace for incompatible snapshots.
//! - [`BloomParameters`] is configuration carried by each manifest and retained by the decoded
//!   filter. The snapshot header repeats `m_bits` and `probe_count`; decoding requires those
//!   dimensions to match the manifest before allocating the body.
//! - `built_at` and `source_max_row_id` describe one build and do not affect compatibility.
//!
//! The number of inserted items is deliberately not part of the format: what matters for
//! correctness is the false-positive rate, and both it and the distinct-item estimate are
//! calculated from the set bits.

#![deny(
    asm_sub_register,
    deprecated,
    missing_abi,
    unsafe_code,
    unused_macros,
    unused_must_use,
    unused_unsafe
)]
#![deny(clippy::from_over_into, clippy::needless_question_mark)]
#![cfg_attr(
    not(debug_assertions),
    deny(unused_imports, unused_mut, unused_variables)
)]

pub mod source;
#[cfg(test)]
mod tests;
pub mod wire;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zerocopy::byteorder::little_endian::U64;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::nix_store::StorePathHash;

/// Magic bytes at the start of a Bloom snapshot, stored verbatim on the wire.
pub const SNAPSHOT_MAGIC: [u8; 4] = *b"FBF1";

/// Arbitrary upper bound to protect against excessive CPU utilization.
pub const MAX_PROBE_COUNT: u8 = 64;

/// Construction and safety policy for one Bloom snapshot.
///
/// The manifest carries this complete value, and a decoded filter retains it together with its
/// bits.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", try_from = "UncheckedBloomParameters")]
pub struct BloomParameters {
    m_bits: u64,
    probe_count: u8,
    max_heartbeat_interval_seconds: u64,
    clock_skew_slack_seconds: u64,
    max_accepted_false_positive_rate: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct UncheckedBloomParameters {
    m_bits: u64,
    probe_count: u8,
    max_heartbeat_interval_seconds: u64,
    clock_skew_slack_seconds: u64,
    max_accepted_false_positive_rate: f64,
}

impl TryFrom<UncheckedBloomParameters> for BloomParameters {
    type Error = BloomError;

    fn try_from(unchecked: UncheckedBloomParameters) -> Result<Self, Self::Error> {
        Self::new(
            unchecked.m_bits,
            unchecked.probe_count,
            unchecked.max_heartbeat_interval_seconds,
            unchecked.clock_skew_slack_seconds,
            unchecked.max_accepted_false_positive_rate,
        )
    }
}

impl BloomParameters {
    pub fn new(
        m_bits: u64,
        probe_count: u8,
        max_heartbeat_interval_seconds: u64,
        clock_skew_slack_seconds: u64,
        max_accepted_false_positive_rate: f64,
    ) -> Result<Self, BloomError> {
        if max_heartbeat_interval_seconds == 0 {
            return Err(BloomError::InvalidHeartbeatInterval);
        }

        if !max_accepted_false_positive_rate.is_finite()
            || max_accepted_false_positive_rate <= 0.0
            || max_accepted_false_positive_rate >= 1.0
        {
            return Err(BloomError::InvalidFalsePositiveRate {
                rate: max_accepted_false_positive_rate,
            });
        }

        Dimensions::new(m_bits, probe_count)?;

        Ok(Self {
            m_bits,
            probe_count,
            max_heartbeat_interval_seconds,
            clock_skew_slack_seconds,
            max_accepted_false_positive_rate,
        })
    }

    pub fn m_bits(self) -> u64 {
        self.m_bits
    }

    pub fn probe_count(self) -> u8 {
        self.probe_count
    }

    pub fn max_heartbeat_interval(self) -> Duration {
        Duration::from_secs(self.max_heartbeat_interval_seconds)
    }

    pub fn clock_skew_slack(self) -> Duration {
        Duration::from_secs(self.clock_skew_slack_seconds)
    }

    pub fn max_accepted_false_positive_rate(self) -> f64 {
        self.max_accepted_false_positive_rate
    }

    fn dimensions(self) -> Dimensions {
        Dimensions::new(self.m_bits, self.probe_count)
            .expect("BloomParameters dimensions were validated at construction")
    }
}

/// The fixed header size for the current snapshot format, derived from the header struct's layout.
pub const SNAPSHOT_HEADER_LEN: usize = std::mem::size_of::<SnapshotHeader>();
const _: () = assert!(SNAPSHOT_HEADER_LEN == 62);

const MANIFEST_PREFIX: &str = "bloom";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterDefinition {
    pub version: u8,
}

impl FilterDefinition {
    pub const CURRENT: Self = Self { version: 1 };

    /// Relative directory containing snapshots compatible with this definition.
    pub fn object_prefix(self) -> PathBuf {
        Path::new(MANIFEST_PREFIX).join(format!("v{}", self.version))
    }

    /// Relative key of the mutable manifest for this definition.
    pub fn manifest_key(self) -> PathBuf {
        self.object_prefix().join("latest")
    }
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct SnapshotHeader {
    magic: [u8; 4],
    version: u8,
    m_bits: U64,
    k: u8,
    built_at: U64,
    source_max_row_id: U64,
    body_checksum: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct DecodedSnapshotHeader {
    dims: Dimensions,
    built_at: u64,
    source_max_row_id: u64,
    body_checksum: [u8; 32],
}

impl DecodedSnapshotHeader {
    fn decode(header: &SnapshotHeader, parameters: BloomParameters) -> Result<Self, BloomError> {
        if header.magic != SNAPSHOT_MAGIC {
            return Err(BloomError::InvalidMagic {
                actual: header.magic,
            });
        }

        if header.version != FilterDefinition::CURRENT.version {
            return Err(BloomError::IncompatibleDefinition {
                expected: FilterDefinition::CURRENT.version,
                actual: header.version,
            });
        }

        let dims = Dimensions::new(header.m_bits.get(), header.k)?;
        let expected = parameters.dimensions();
        if dims.m_bits != expected.m_bits || dims.k != expected.k {
            return Err(BloomError::SnapshotParameterMismatch {
                expected_m_bits: expected.m_bits,
                expected_k: expected.k,
                actual_m_bits: dims.m_bits,
                actual_k: dims.k,
            });
        }

        Ok(Self {
            dims,
            built_at: header.built_at.get(),
            source_max_row_id: header.source_max_row_id.get(),
            body_checksum: header.body_checksum,
        })
    }
}

/// Validated filter dimensions.
#[derive(Clone, Copy, Debug)]
struct Dimensions {
    m_bits: u64,
    k: u8,
    body_len: usize,
}

impl Dimensions {
    fn new(m_bits: u64, k: u8) -> Result<Self, BloomError> {
        // Each body byte stores eight filter bits. Thus, `m_bits` must be divisible by eight. The
        // probe calculation also requires `m_bits` to be a power of two. Each power of two that is
        // at least eight is divisible by eight. This check makes sure that `m_bits / 8` gives the
        // exact body length and does not discard a remainder.
        if m_bits < 8 || !m_bits.is_power_of_two() {
            return Err(BloomError::InvalidBitCount { m_bits });
        }

        if k == 0 || k > MAX_PROBE_COUNT {
            return Err(BloomError::InvalidProbeCount { k });
        }

        let body_len = usize::try_from(m_bits / 8).map_err(|_| BloomError::TooLarge { m_bits })?;

        Ok(Self {
            m_bits,
            k,
            body_len,
        })
    }

    /// Calculates the probe positions for one store path hash using double hashing. Probe `i` is
    /// `(h1 + i * h2) mod m_bits`. This gives `k` positions from one 20-byte hash.
    ///
    /// The store path hash is a cryptographic digest. Thus its bytes have a uniform distribution,
    /// as required by the bloom filter. The method divides the digest into two hash values. `h1` is
    /// bytes 0..8. `h2` is bytes 8..16. The method reads each value as a little-endian `u64`.
    ///
    /// Two operations use the property that `m_bits` is a power of two:
    ///
    /// - `h2 | 1` makes the stride an odd number. An odd number and a power of two have no common
    ///   divisor greater than one. Thus, the sequence can visit each bit position before it repeats.
    ///   Without this operation, the stride can share a divisor with `m_bits`. The sequence can then
    ///   repeat after it visits only a small group of positions. For example, with `m_bits = 16` and
    ///   a stride of 4, the sequence visits 0, 4, 8, and 12, and then returns to 0.
    /// - `& mask`, with `mask = m_bits - 1`, does a modulo operation by `m_bits`. When you subtract
    ///   1 from a power of two, all the lower bits of the result are set. Example: `1024 - 1 =
    ///   0b11_1111_1111`. The AND operation keeps only the low `log2(m_bits)` bits. The result is the
    ///   same as `% m_bits`, but a division is not necessary.
    ///
    /// `wrapping_add` and `wrapping_mul` give the overflow a defined behavior. The mask discards
    /// the high bits that overflow.
    fn probe_positions(
        self,
        store_path_hash: &StorePathHash,
    ) -> Result<impl Iterator<Item = u64>, BloomError> {
        let decoded = decode_store_path_hash(store_path_hash)?;
        let h1 = u64::from_le_bytes(decoded[0..8].try_into().expect("slice length is 8"));
        let h2 = u64::from_le_bytes(decoded[8..16].try_into().expect("slice length is 8")) | 1;
        let mask = self.m_bits - 1;

        Ok((0..u64::from(self.k)).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) & mask))
    }
}

/// Occupancy-derived statistics for a Bloom snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomSnapshotStats {
    pub fill_ratio: f64,
    pub estimated_false_positive_rate: f64,
    pub estimated_distinct_items: Option<f64>,
}

impl BloomSnapshotStats {
    fn from_set_bits(dims: Dimensions, set_bits: u64) -> Self {
        let fill_ratio = set_bits as f64 / dims.m_bits as f64;
        let estimated_false_positive_rate = fill_ratio.powi(i32::from(dims.k));
        let estimated_distinct_items = if fill_ratio >= 1.0 {
            None
        } else {
            let n = -(dims.m_bits as f64 / f64::from(dims.k)) * (1.0 - fill_ratio).ln();
            Some(n)
        };

        Self {
            fill_ratio,
            estimated_false_positive_rate,
            estimated_distinct_items,
        }
    }

    fn validate(self, limit: f64) -> Result<Self, BloomError> {
        if self.estimated_false_positive_rate > limit {
            return Err(BloomError::ExcessiveFalsePositiveRate {
                estimated_fpr: self.estimated_false_positive_rate,
                limit,
            });
        }

        Ok(self)
    }
}

/// Encoded snapshot bytes and the metadata captured in their header.
#[derive(Debug)]
pub struct EncodedBloomSnapshot {
    bytes: Vec<u8>,
    body_checksum: [u8; 32],
    parameters: BloomParameters,
    built_at: u64,
    source_max_row_id: u64,
}

impl EncodedBloomSnapshot {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn built_at(&self) -> u64 {
        self.built_at
    }

    pub fn source_max_row_id(&self) -> u64 {
        self.source_max_row_id
    }

    pub fn body_checksum_hex(&self) -> String {
        hex::encode(self.body_checksum)
    }
}

/// A Bloom filter that accepts concurrent probes and inserts without locks.
///
/// The bits live in atomic bytes, so any number of threads can probe and insert at the same time.
/// Every bit access uses relaxed ordering: the bits are independent of each other, a set bit never
/// becomes unset, and a probe that races an insert is indistinguishable from a probe that ran one
/// moment earlier. No access establishes an ordering that another location depends on.
#[derive(Debug)]
pub struct ConcurrentBloomFilter {
    parameters: BloomParameters,
    dims: Dimensions,
    built_at: u64,
    source_max_row_id: u64,
    bits: Vec<AtomicU8>,
    decoded_body_checksum: Option<DecodedBodyChecksum>,
}

/// A checksum verified while decoding, reusable until the first insertion.
#[derive(Debug)]
struct DecodedBodyChecksum {
    value: [u8; 32],
    invalidated: AtomicBool,
}

impl DecodedBodyChecksum {
    fn new(value: [u8; 32]) -> Self {
        Self {
            value,
            invalidated: AtomicBool::new(false),
        }
    }

    fn current(&self) -> Option<[u8; 32]> {
        if self.invalidated.load(Ordering::SeqCst) {
            None
        } else {
            Some(self.value)
        }
    }

    fn invalidate(&self) {
        if !self.invalidated.load(Ordering::Relaxed) {
            self.invalidated.store(true, Ordering::SeqCst);
        }
    }
}

impl ConcurrentBloomFilter {
    pub fn new(parameters: BloomParameters, built_at: u64) -> Result<Self, BloomError> {
        let dims = parameters.dimensions();
        let bits = std::iter::repeat_with(|| AtomicU8::new(0))
            .take(dims.body_len)
            .collect();

        Ok(Self {
            parameters,
            dims,
            built_at,
            source_max_row_id: 0,
            bits,
            decoded_body_checksum: None,
        })
    }

    /// Sets probe positions from a path-created event.
    ///
    /// The [`wire::ProbePositions`] type guarantees positions that are in range for a power-of-two
    /// `m_bits`, so the only failure left is a size mismatch. Both sizes are powers of two, and
    /// each probe position is `(h1 + i * h2) & (m_bits - 1)`. Masking such a position by a smaller
    /// filter's `m_bits - 1` keeps only the low bits, which gives exactly the position the smaller
    /// filter calculates itself. Because of this, the filter accepts positions calculated for an
    /// equal or larger size and masks them down. Positions calculated for a smaller size have
    /// already discarded high bits that this filter needs, so they are rejected and leave the
    /// filter unchanged.
    pub fn insert_positions(&self, positions: &wire::ProbePositions) -> Result<(), BloomError> {
        if positions.m_bits() < self.dims.m_bits {
            return Err(BloomError::IncompatiblePositions {
                event_m_bits: positions.m_bits(),
                filter_m_bits: self.dims.m_bits,
            });
        }

        // Position `i` depends only on `i` (`h1 + i*h2`), so a filter with a smaller `k` probes a
        // prefix of the event's positions. An event with fewer positions than this filter's `k`
        // leaves required probe bits unset, which would create a false negative.
        let k = usize::from(self.dims.k);
        if positions.positions().len() < k {
            return Err(BloomError::InsufficientPositions {
                count: positions.positions().len(),
                k: self.dims.k,
            });
        }

        // A decoded filter starts with a checksum that matches its bits. Mark that checksum stale
        // before setting the first bit so it is never used for modified data. New filters have no
        // cached checksum, and later inserts find an already-stale checksum.
        if let Some(checksum) = &self.decoded_body_checksum {
            checksum.invalidate();
        }

        let mask = self.dims.m_bits - 1;
        for position in positions.positions().iter().take(k) {
            // A bit position points to byte `position / 8`, and to bit `position % 8` in that byte.
            // The bit order is least-significant bit first. `1 << (position % 8)` makes a mask that
            // has one bit set.
            let position = position & mask;
            let byte_pos = (position / 8) as usize;
            let bit_pos = position % 8;
            self.bits[byte_pos].fetch_or(1 << bit_pos, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Tests one hash against this snapshot. A negative means the required bits are not all set;
    /// whether that proves absence from the represented set depends on external coverage and
    /// freshness guarantees.
    pub fn contains(&self, store_path_hash: &StorePathHash) -> Result<bool, BloomError> {
        // This uses the same byte and bit addresses as `insert_positions`. The AND operation with
        // the one-bit mask isolates the probed bit. The result is not zero only when that bit is
        // set. The key is possibly in the filter only if all `k` probed bits are set. If one probed
        // bit is not set, the key was never inserted.
        Ok(self.dims.probe_positions(store_path_hash)?.all(|position| {
            let byte_pos = (position / 8) as usize;
            let bit_pos = position % 8;
            self.bits[byte_pos].load(Ordering::Relaxed) & (1 << bit_pos) != 0
        }))
    }

    pub fn m_bits(&self) -> u64 {
        self.dims.m_bits
    }

    pub fn k(&self) -> u8 {
        self.dims.k
    }

    pub fn parameters(&self) -> BloomParameters {
        self.parameters
    }

    pub fn built_at(&self) -> u64 {
        self.built_at
    }

    /// Returns the source cursor recorded in this snapshot.
    pub fn source_max_row_id(&self) -> u64 {
        self.source_max_row_id
    }

    /// Sets the source cursor recorded in this snapshot.
    pub fn set_source_max_row_id(&mut self, source_max_row_id: u64) {
        self.source_max_row_id = source_max_row_id;
    }

    pub fn body_checksum(&self) -> [u8; 32] {
        if let Some(checksum) = self
            .decoded_body_checksum
            .as_ref()
            .and_then(|cs| cs.current())
        {
            return checksum;
        }

        // Copy atomic bytes in bounded chunks instead of allocating a second full filter body.
        let mut hasher = Sha256::new();
        let mut copied = Vec::with_capacity(64 * 1024);
        for chunk in self.bits.chunks(64 * 1024) {
            copied.clear();
            copied.extend(chunk.iter().map(|byte| byte.load(Ordering::Relaxed)));
            hasher.update(&copied);
        }

        hasher.finalize().into()
    }

    pub fn body_checksum_hex(&self) -> String {
        hex::encode(self.body_checksum())
    }

    pub fn body_len(&self) -> usize {
        self.bits.len()
    }

    /// Calculates all occupancy-derived statistics with one scan of the snapshot body.
    pub fn stats(&self) -> BloomSnapshotStats {
        let set_bits: u64 = self
            .bits
            .iter()
            .map(|byte| u64::from(byte.load(Ordering::Relaxed).count_ones()))
            .sum();
        BloomSnapshotStats::from_set_bits(self.dims, set_bits)
    }

    /// The fraction of body bits that are set. This is the fill ratio `p` that the two estimator
    /// methods below use.
    pub fn fill_ratio(&self) -> f64 {
        self.stats().fill_ratio
    }

    /// Estimates the false-positive probability from the measured bit occupancy.
    ///
    /// A false positive occurs when a key was never inserted, but all its `k` probed bits are set.
    /// For a key that was never inserted, the probe positions are effectively random. Thus each
    /// probe finds a set bit with a probability that is equal to the fill ratio `p`. The `k` probes
    /// are approximately independent. Thus the probability that all `k` probes find a set bit is
    /// `p^k`.
    ///
    /// This estimate uses the measured fill ratio, not a theoretical one. Thus the estimate is
    /// correct also when the filter contains more items than its sizing policy planned.
    pub fn estimated_false_positive_rate(&self) -> f64 {
        self.stats().estimated_false_positive_rate
    }

    /// Estimates the number of distinct inserted keys from the measured bit occupancy.
    ///
    /// The filter does not store the keys, and a duplicate insert does not change the bits. But the
    /// fill ratio `p` is a known function of the number of distinct keys `n`. Thus you can
    /// calculate `n` from the measured `p`.
    ///
    /// The forward direction: each insert sets `k` of the `m_bits` positions. After `n` distinct
    /// inserts, one given bit is 0 with probability `(1 - 1/m_bits)^(k * n)`, which is
    /// approximately `e^(-k * n / m_bits)`. Thus the expected fill ratio is `p = 1 - e^(-k * n /
    /// m_bits)`.
    ///
    /// The reverse direction, solved for `n`:
    ///
    /// `n = -(m_bits / k) * ln(1 - p)`
    ///
    /// When `p` is 1, all the bits are set. The formula then contains `ln(0)` and cannot give a
    /// finite estimate. The method returns `None` for this condition.
    pub fn estimated_distinct_items(&self) -> Option<f64> {
        self.stats().estimated_distinct_items
    }

    fn header(&self, body_checksum: [u8; 32]) -> SnapshotHeader {
        let definition = FilterDefinition::CURRENT;
        SnapshotHeader {
            magic: SNAPSHOT_MAGIC,
            version: definition.version,
            m_bits: self.m_bits().into(),
            k: self.k(),
            built_at: self.built_at().into(),
            source_max_row_id: self.source_max_row_id().into(),
            body_checksum,
        }
    }

    /// Serialize the fixed little-endian header followed by the bit array.
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_checksum().into_bytes()
    }

    /// Serializes the snapshot and returns the body checksum calculated for its header.
    pub fn encode_with_checksum(&self) -> EncodedBloomSnapshot {
        let mut encoded = Vec::with_capacity(SNAPSHOT_HEADER_LEN + self.body_len());
        encoded.resize(SNAPSHOT_HEADER_LEN, 0);
        encoded.extend(self.bits.iter().map(|byte| byte.load(Ordering::Relaxed)));

        let body_checksum = checksum(&encoded[SNAPSHOT_HEADER_LEN..]);
        let header = self.header(body_checksum);
        encoded[..SNAPSHOT_HEADER_LEN].copy_from_slice(header.as_bytes());

        EncodedBloomSnapshot {
            bytes: encoded,
            body_checksum,
            parameters: self.parameters,
            built_at: header.built_at.get(),
            source_max_row_id: header.source_max_row_id.get(),
        }
    }

    /// Writes a point-in-time copy of the filter. The output is identical to [`Self::encode`].
    pub fn write_to<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(self.encode_with_checksum().as_bytes())
    }

    #[cfg(test)]
    fn copied_bits(&self) -> Vec<u8> {
        self.bits
            .iter()
            .map(|byte| byte.load(Ordering::Relaxed))
            .collect()
    }

    /// Decode and fully validate a snapshot.
    pub fn decode(parameters: BloomParameters, encoded: &[u8]) -> Result<Self, BloomError> {
        let (header, body) =
            SnapshotHeader::ref_from_prefix(encoded).map_err(|_| BloomError::Truncated {
                expected_at_least: SNAPSHOT_HEADER_LEN,
                actual: encoded.len(),
            })?;

        let header = DecodedSnapshotHeader::decode(header, parameters)?;
        if body.len() != header.dims.body_len {
            return Err(BloomError::InvalidBodyLength {
                expected: header.dims.body_len,
                actual: body.len(),
            });
        }

        let actual_checksum = checksum(body);
        if actual_checksum != header.body_checksum {
            return Err(BloomError::ChecksumMismatch {
                expected: hex::encode(header.body_checksum),
                actual: hex::encode(actual_checksum),
            });
        }

        let snapshot = Self {
            parameters,
            dims: header.dims,
            built_at: header.built_at,
            source_max_row_id: header.source_max_row_id,
            bits: body.iter().copied().map(AtomicU8::new).collect(),
            decoded_body_checksum: Some(DecodedBodyChecksum::new(actual_checksum)),
        };

        snapshot
            .stats()
            .validate(parameters.max_accepted_false_positive_rate())?;

        Ok(snapshot)
    }

    /// Decodes segmented snapshot bytes directly into the concurrent representation and validates
    /// the final filter against both the snapshot header and its manifest.
    ///
    /// Chunks may split the fixed header or body at any byte. The filter is not returned until its
    /// dimensions, body length, checksum, manifest metadata, and false-positive rate all pass
    /// validation. The manifest's object key is trusted as the location from which the caller read
    /// the bytes. Statistics are calculated while the chunks are copied into atomic bytes.
    pub fn decode_with_manifest<I, C>(
        manifest: &SnapshotManifest,
        encoded_chunks: I,
    ) -> Result<(Self, BloomSnapshotStats), BloomError>
    where
        I: IntoIterator<Item = C>,
        C: AsRef<[u8]>,
    {
        manifest.validate_definition()?;
        let parameters = manifest.parameters;

        let mut header_bytes = [0; SNAPSHOT_HEADER_LEN];
        let mut header_len = 0;
        let mut total_len: usize = 0;
        let mut decoded_header = None;
        let mut bits = None;
        let mut body_hasher = Sha256::new();
        let mut set_bits = 0_u64;

        for chunk in encoded_chunks {
            let mut chunk = chunk.as_ref();
            total_len = total_len.saturating_add(chunk.len());

            if header_len < SNAPSHOT_HEADER_LEN {
                let copy_len = (SNAPSHOT_HEADER_LEN - header_len).min(chunk.len());
                header_bytes[header_len..header_len + copy_len].copy_from_slice(&chunk[..copy_len]);
                header_len += copy_len;
                chunk = &chunk[copy_len..];

                if header_len < SNAPSHOT_HEADER_LEN {
                    continue;
                }

                let (header, remainder) = SnapshotHeader::ref_from_prefix(&header_bytes)
                    .expect("the complete fixed-size header is valid");

                debug_assert!(remainder.is_empty());

                let header = DecodedSnapshotHeader::decode(header, parameters)?;
                let mut body = Vec::new();
                body.try_reserve_exact(header.dims.body_len)
                    .map_err(|_| BloomError::TooLarge {
                        m_bits: header.dims.m_bits,
                    })?;

                bits = Some(body);
                decoded_header = Some(header);
            }

            if chunk.is_empty() {
                continue;
            }

            let header = decoded_header.expect("a body chunk follows a decoded header");
            let bits = bits.as_mut().expect("a decoded header allocates its body");
            let actual_len = bits.len().saturating_add(chunk.len());
            if actual_len > header.dims.body_len {
                return Err(BloomError::InvalidBodyLength {
                    expected: header.dims.body_len,
                    actual: actual_len,
                });
            }

            body_hasher.update(chunk);
            set_bits += chunk
                .iter()
                .map(|byte| u64::from(byte.count_ones()))
                .sum::<u64>();
            bits.extend(chunk.iter().copied().map(AtomicU8::new));
        }

        if header_len < SNAPSHOT_HEADER_LEN {
            return Err(BloomError::Truncated {
                expected_at_least: SNAPSHOT_HEADER_LEN,
                actual: total_len,
            });
        }

        let header = decoded_header.expect("a complete header was decoded");
        let bits = bits.expect("a complete header allocated its body");
        if bits.len() != header.dims.body_len {
            return Err(BloomError::InvalidBodyLength {
                expected: header.dims.body_len,
                actual: bits.len(),
            });
        }

        let actual_checksum: [u8; 32] = body_hasher.finalize().into();
        if actual_checksum != header.body_checksum {
            return Err(BloomError::ChecksumMismatch {
                expected: hex::encode(header.body_checksum),
                actual: hex::encode(actual_checksum),
            });
        }

        manifest.validate_snapshot_parts(parameters, header.source_max_row_id, actual_checksum)?;

        let stats = BloomSnapshotStats::from_set_bits(header.dims, set_bits)
            .validate(parameters.max_accepted_false_positive_rate())?;

        Ok((
            Self {
                parameters,
                dims: header.dims,
                built_at: header.built_at,
                source_max_row_id: header.source_max_row_id,
                bits,
                decoded_body_checksum: Some(DecodedBodyChecksum::new(actual_checksum)),
            },
            stats,
        ))
    }

    /// Copies the current bits into a serializable snapshot.
    ///
    /// The copy loads each atomic byte independently. A concurrent multi-bit insert may therefore
    /// be only partly represented, so callers must account for concurrent updates. The caller
    /// supplies `built_at` as the copy's freshness metadata.
    pub fn to_snapshot(&self, built_at: u64) -> Self {
        Self {
            parameters: self.parameters,
            dims: self.dims,
            built_at,
            source_max_row_id: self.source_max_row_id,
            bits: self
                .bits
                .iter()
                .map(|byte| AtomicU8::new(byte.load(Ordering::Relaxed)))
                .collect(),
            decoded_body_checksum: None,
        }
    }
}

/// Metadata identifying and validating one snapshot object.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub version: u8,
    pub object_key: PathBuf,
    pub body_checksum: String,
    pub parameters: BloomParameters,
    pub source_generation: Uuid,

    /// The snapshot's source cursor within `source_generation`.
    pub source_max_row_id: u64,

    /// Unix timestamp from which updates after this snapshot may be required.
    pub replay_from: u64,
}

impl SnapshotManifest {
    pub fn for_snapshot(
        snapshot: &ConcurrentBloomFilter,
        object_key: PathBuf,
        source_generation: Uuid,
        replay_from: u64,
    ) -> Self {
        Self::new(
            object_key,
            snapshot.source_max_row_id(),
            snapshot.body_checksum_hex(),
            snapshot.parameters,
            source_generation,
            replay_from,
        )
    }

    pub fn for_encoded_snapshot(
        encoded: &EncodedBloomSnapshot,
        object_key: PathBuf,
        source_generation: Uuid,
        replay_from: u64,
    ) -> Self {
        Self::new(
            object_key,
            encoded.source_max_row_id,
            encoded.body_checksum_hex(),
            encoded.parameters,
            source_generation,
            replay_from,
        )
    }

    fn new(
        object_key: PathBuf,
        source_max_row_id: u64,
        body_checksum: String,
        parameters: BloomParameters,
        source_generation: Uuid,
        replay_from: u64,
    ) -> Self {
        let definition = FilterDefinition::CURRENT;

        Self {
            version: definition.version,
            object_key,
            body_checksum,
            parameters,
            source_generation,
            source_max_row_id,
            replay_from,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, BloomError> {
        let mut encoded = serde_json::to_vec(self).map_err(BloomError::ManifestJson)?;
        encoded.push(b'\n');
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, BloomError> {
        let manifest: Self = serde_json::from_slice(encoded).map_err(BloomError::ManifestJson)?;
        manifest.validate_definition()?;
        Ok(manifest)
    }

    pub fn validate_snapshot(&self, snapshot: &ConcurrentBloomFilter) -> Result<(), BloomError> {
        self.validate_snapshot_parts(
            snapshot.parameters,
            snapshot.source_max_row_id(),
            snapshot.body_checksum(),
        )
    }

    fn validate_snapshot_parts(
        &self,
        parameters: BloomParameters,
        source_max_row_id: u64,
        body_checksum: [u8; 32],
    ) -> Result<(), BloomError> {
        self.validate_definition()?;

        if self.body_checksum != hex::encode(body_checksum)
            || self.parameters != parameters
            || self.source_max_row_id != source_max_row_id
        {
            return Err(BloomError::ManifestSnapshotMismatch);
        }

        Ok(())
    }

    /// Validates the manifest version.
    fn validate_definition(&self) -> Result<(), BloomError> {
        let definition = FilterDefinition::CURRENT;
        if self.version != definition.version {
            return Err(BloomError::IncompatibleDefinition {
                expected: definition.version,
                actual: self.version,
            });
        }

        Ok(())
    }
}

fn checksum(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn decode_store_path_hash(store_path_hash: &StorePathHash) -> Result<[u8; 20], BloomError> {
    nix_base32::from_nix_base32(store_path_hash.as_str())
        .and_then(|decoded| decoded.try_into().ok())
        .ok_or_else(|| BloomError::InvalidStorePathHash {
            hash: store_path_hash.to_string(),
        })
}

#[derive(Debug, displaydoc::Display)]
pub enum BloomError {
    /// invalid Bloom magic: {actual:02x?}
    InvalidMagic { actual: [u8; 4] },

    /// truncated Bloom snapshot: expected at least {expected_at_least} bytes, got {actual}
    Truncated {
        expected_at_least: usize,
        actual: usize,
    },

    /// incompatible Bloom definition version: expected {expected}, got {actual}
    IncompatibleDefinition { expected: u8, actual: u8 },

    /// Bloom bit count must be a byte-aligned power of two, got {m_bits}
    InvalidBitCount { m_bits: u64 },

    /// Bloom probe count must be between 1 and 64, got {k}
    InvalidProbeCount { k: u8 },

    /// Bloom maximum heartbeat interval must be greater than zero
    InvalidHeartbeatInterval,

    /// Bloom maximum accepted false-positive rate must be finite and between 0 and 1, got {rate}
    InvalidFalsePositiveRate { rate: f64 },

    /// Bloom filter with {m_bits} bits does not fit in memory
    TooLarge { m_bits: u64 },

    /// Bloom snapshot dimensions {actual_m_bits}/{actual_k} do not match configured dimensions {expected_m_bits}/{expected_k}
    SnapshotParameterMismatch {
        expected_m_bits: u64,
        expected_k: u8,
        actual_m_bits: u64,
        actual_k: u8,
    },

    /// invalid Bloom body length: expected {expected} bytes, got {actual}
    InvalidBodyLength { expected: usize, actual: usize },

    /// Bloom body checksum mismatch: expected {expected}, got {actual}
    ChecksumMismatch { expected: String, actual: String },

    /// Bloom snapshot estimated FPR {estimated_fpr:.6} exceeds safety limit {limit:.6}
    ExcessiveFalsePositiveRate { estimated_fpr: f64, limit: f64 },

    /// could not Nix-base32 decode store path hash {hash} to 20 bytes
    InvalidStorePathHash { hash: String },

    /// Bloom positions use {event_m_bits} bits, smaller than this filter's {filter_m_bits}
    IncompatiblePositions {
        event_m_bits: u64,
        filter_m_bits: u64,
    },

    /// Bloom update carries {count} probe positions, fewer than this filter's {k}
    InsufficientPositions { count: usize, k: u8 },

    /// Bloom position count must be between 1 and 64, got {count}
    InvalidPositionCount { count: usize },

    /// Bloom position {position} is out of range for {m_bits} bits
    PositionOutOfRange { position: u64, m_bits: u64 },

    /// invalid Bloom manifest JSON: {0}
    ManifestJson(serde_json::Error),

    /// Bloom manifest does not describe the decoded snapshot
    ManifestSnapshotMismatch,

    /// invalid Bloom source shard key {key}
    InvalidShardKey { key: PathBuf },

    /// invalid Bloom source shard bounds {first_id}..={last_id}
    InvalidShardBounds { first_id: u64, last_id: u64 },

    /// invalid Bloom source shard JSON: {0}
    ShardJson(serde_json::Error),

    /// Bloom source shards must not be empty
    EmptyShard,

    /// Bloom source shard row IDs are not strictly increasing
    NonMonotonicShardRows,

    /// Bloom source shard contents do not match the bounds of key {key}
    ShardBoundsMismatch { key: PathBuf },

    /// Bloom source shard {key} overlaps another shard of its generation
    ShardOverlap { key: PathBuf },

    /// Bloom source shard {key} does not belong to generation {expected}
    WrongShardGeneration { key: PathBuf, expected: String },
}

impl std::error::Error for BloomError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ManifestJson(error) | Self::ShardJson(error) => Some(error),
            _ => None,
        }
    }
}
