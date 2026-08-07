//! Wire types for Bloom filter live updates.
//!
//! [`BloomStreamEvent`] provides a versioned JSON encoding for probe-position updates and payload-
//! free heartbeat markers.

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::bloom::{BloomError, BloomParameters, MAX_PROBE_COUNT};
use crate::nix_store::StorePathHash;

/// Validated probe positions of one store path, as carried by a path-created record.
///
/// Every value of this type upholds these invariants, whether it was calculated by
/// [`ProbePositions::of`] or deserialized from a record:
///
/// - `m_bits` is a power of two and at least 8,
/// - there is at least one and at most [`MAX_PROBE_COUNT`] positions,
/// - every position is below `m_bits`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(try_from = "UncheckedPositions")]
pub struct ProbePositions {
    m_bits: u64,
    positions: Vec<u64>,
}

/// The deserialization shape of [`ProbePositions`], before validation.
#[derive(Deserialize)]
struct UncheckedPositions {
    m_bits: u64,
    positions: Vec<u64>,
}

impl TryFrom<UncheckedPositions> for ProbePositions {
    type Error = BloomError;

    fn try_from(unchecked: UncheckedPositions) -> Result<Self, BloomError> {
        let UncheckedPositions { m_bits, positions } = unchecked;

        if m_bits < 8 || !m_bits.is_power_of_two() {
            return Err(BloomError::InvalidBitCount { m_bits });
        }
        if positions.is_empty() || positions.len() > usize::from(MAX_PROBE_COUNT) {
            return Err(BloomError::InvalidPositionCount {
                count: positions.len(),
            });
        }
        if let Some(&position) = positions.iter().find(|&&position| position >= m_bits) {
            return Err(BloomError::PositionOutOfRange { position, m_bits });
        }

        Ok(Self { m_bits, positions })
    }
}

impl ProbePositions {
    /// Calculates the probe positions of a store path hash under the supplied parameters.
    ///
    /// The result upholds the type's invariants by construction: [`BloomParameters`] is validated,
    /// and each calculated position is masked below its `m_bits`. The only failure is a store path
    /// hash that does not Nix-base32 decode to 20 bytes.
    pub fn of(
        store_path_hash: &StorePathHash,
        parameters: BloomParameters,
    ) -> Result<Self, BloomError> {
        Ok(Self {
            m_bits: parameters.m_bits(),
            positions: parameters
                .dimensions()
                .probe_positions(store_path_hash)?
                .collect(),
        })
    }

    /// The filter size the positions were calculated against.
    pub fn m_bits(&self) -> u64 {
        self.m_bits
    }

    /// The bit positions to set, each below [`Self::m_bits`].
    pub fn positions(&self) -> &[u64] {
        &self.positions
    }
}

/// One record on the Bloom update stream.
///
/// The encoding is JSON with two top-level discriminators: `version` and `kind`.
/// [`BloomStreamEvent::decode`] reads the version before the rest of the record, so it can
/// recognize a record of an unsupported version without interpreting its other fields.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BloomStreamEvent {
    /// An update for a store path whose probe positions are these.
    ///
    /// The record carries the bit positions to set rather than the raw store path hash. They are
    /// calculated with double hashing modulo the configured `m_bits`. Construct this variant with
    /// [`BloomStreamEvent::path_created`].
    PathCreated(ProbePositions),

    /// A payload-free heartbeat marker.
    Heartbeat,
}

/// The result of decoding one well-formed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedEvent {
    Event(BloomStreamEvent),

    /// The record carries an unsupported version. Its other fields stay uninterpreted, so a future
    /// version can change them freely.
    UnsupportedVersion(u8),
}

#[derive(Serialize)]
struct VersionedEvent<'a> {
    version: u8,
    #[serde(flatten)]
    event: &'a BloomStreamEvent,
}

#[derive(Deserialize)]
struct VersionOnly {
    version: u8,
}

impl BloomStreamEvent {
    /// The version this code encodes, and the only version it decodes.
    pub const CURRENT_VERSION: u8 = 1;

    /// Builds a path-created event from a store path hash.
    ///
    /// The positions come from the supplied [`BloomParameters`]. The hash itself stays out of the
    /// event.
    pub fn path_created(
        store_path_hash: &StorePathHash,
        parameters: BloomParameters,
    ) -> Result<Self, BloomError> {
        Ok(Self::PathCreated(ProbePositions::of(
            store_path_hash,
            parameters,
        )?))
    }

    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&VersionedEvent {
            version: Self::CURRENT_VERSION,
            event: self,
        })
    }

    /// Decodes one record, reading the version before the event itself.
    pub fn decode(encoded: &[u8]) -> Result<DecodedEvent, serde_json::Error> {
        let VersionOnly { version } = serde_json::from_slice(encoded)?;
        if version != Self::CURRENT_VERSION {
            return Ok(DecodedEvent::UnsupportedVersion(version));
        }

        Ok(DecodedEvent::Event(serde_json::from_slice(encoded)?))
    }
}
