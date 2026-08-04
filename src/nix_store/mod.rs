use std::ffi::OsStr;
#[cfg(target_family = "unix")]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use lazy_static::lazy_static;
use regex::Regex;
use serde::{de, Deserialize, Serialize};

use crate::error::{StoreError, StoreResult};
use crate::hash::Hash;

/// Length of the hash in a store path.
pub const STORE_PATH_HASH_LEN: usize = 32;

/// Regex that matches a store path hash, without anchors.
pub const STORE_PATH_HASH_REGEX_FRAGMENT: &str = "[0123456789abcdfghijklmnpqrsvwxyz]{32}";

lazy_static! {
    /// Regex for a valid store path hash.
    ///
    /// This is the path portion of a base name.
    static ref STORE_PATH_HASH_REGEX: Regex = {
        Regex::new(&format!("^{}$", STORE_PATH_HASH_REGEX_FRAGMENT)).unwrap()
    };

    /// Regex for a valid store base name.
    ///
    /// A base name consists of two parts: A hash and a human-readable
    /// label/name. The format of the hash is described in `StorePathHash`.
    ///
    /// The human-readable name can only contain the following characters:
    ///
    /// - A-Za-z0-9
    /// - `+-._?=`
    ///
    /// See the Nix implementation in `src/libstore/path.cc`.
    static ref STORE_BASE_NAME_REGEX: Regex = {
        Regex::new(r"^[0123456789abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+-._?=]+$").unwrap()
    };
}

/// Information on a valid store path.
#[derive(Debug)]
pub struct ValidPathInfo {
    /// The store path.
    pub path: StorePath,

    /// Hash of the NAR.
    pub nar_hash: Hash,

    /// Size of the NAR.
    pub nar_size: u64,

    /// References.
    ///
    /// This list only contains base names of the paths.
    pub references: Vec<PathBuf>,

    /// Signatures.
    pub sigs: Vec<String>,

    /// Content Address.
    pub ca: Option<String>,

    /// Provenance.
    pub provenance: Option<serde_json::Value>,
}

/// A path in a Nix store.
///
/// This must be a direct child of the store. This path may or
/// may not actually exist.
///
/// This guarantees that the base name is of valid format.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct StorePath {
    /// Base name of the store path.
    ///
    /// For example, for `/nix/store/ia70ss13m22znbl8khrf2hq72qmh5drr-ruby-2.7.5`,
    /// this would be `ia70ss13m22znbl8khrf2hq72qmh5drr-ruby-2.7.5`.
    base_name: PathBuf,
}

impl FromStr for StorePath {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base_name(PathBuf::from(s))
    }
}

impl std::fmt::Display for StorePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.base_name.display().fmt(f)
    }
}

impl Serialize for StorePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}

impl<'de> Deserialize<'de> for StorePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        String::deserialize(deserializer).and_then(|base_name_str| {
            StorePath::from_str(&base_name_str).map_err(|e| de::Error::custom(e.to_string()))
        })
    }
}

impl StorePath {
    /// Creates a StorePath with a base name.
    pub fn from_base_name(base_name: PathBuf) -> StoreResult<Self> {
        let s = base_name
            .as_os_str()
            .to_str()
            .ok_or_else(|| StoreError::InvalidStorePathName {
                base_name: base_name.clone(),
                reason: "Name contains non-UTF-8 characters",
            })?;

        if !STORE_BASE_NAME_REGEX.is_match(s) {
            return Err(StoreError::InvalidStorePathName {
                base_name,
                reason: "Name is of invalid format",
            });
        }

        Ok(Self { base_name })
    }

    /// Creates a StorePath with a known valid base name.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the name is of a valid format (refer
    /// to the documentations for `STORE_BASE_NAME_REGEX`). Other operations
    /// with this object will assume it's valid.
    #[allow(unsafe_code)]
    pub unsafe fn from_base_name_unchecked(base_name: PathBuf) -> Self {
        Self { base_name }
    }

    /// Gets the hash portion of the store path.
    #[cfg(target_family = "unix")]
    pub fn to_hash(&self) -> StorePathHash {
        // Safety: We have already validated the format of the base name,
        // including the hash part. The name is guaranteed valid UTF-8.
        #[allow(unsafe_code)]
        unsafe {
            let s = std::str::from_utf8_unchecked(self.base_name.as_os_str().as_bytes());
            let hash = s[..STORE_PATH_HASH_LEN].to_string();
            StorePathHash::new_unchecked(hash)
        }
    }

    /// Returns the human-readable name.
    #[cfg(target_family = "unix")]
    pub fn name(&self) -> String {
        // Safety: Already checked
        #[allow(unsafe_code)]
        unsafe {
            let s = std::str::from_utf8_unchecked(self.base_name.as_os_str().as_bytes());
            s[STORE_PATH_HASH_LEN + 1..].to_string()
        }
    }

    pub fn as_os_str(&self) -> &OsStr {
        self.base_name.as_os_str()
    }

    /// Returns the bytes of the base name.
    #[cfg(target_family = "unix")]
    pub fn as_base_name_bytes(&self) -> &[u8] {
        self.base_name.as_os_str().as_bytes()
    }
}

/// A fixed-length store path hash.
///
/// For example, for `/nix/store/ia70ss13m22znbl8khrf2hq72qmh5drr-ruby-2.7.5`,
/// this would be `ia70ss13m22znbl8khrf2hq72qmh5drr`.
///
/// It must contain exactly 32 "base-32 characters". Nix's special scheme
/// include the following valid characters: "0123456789abcdfghijklmnpqrsvwxyz"
/// ('e', 'o', 'u', 't' are banned).
///
/// Examples of invalid store path hashes:
///
/// - "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
/// - "IA70SS13M22ZNBL8KHRF2HQ72QMH5DRR"
/// - "whatevenisthisthing"
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StorePathHash(String);

impl<'de> Deserialize<'de> for StorePathHash {
    /// Deserializes a potentially-invalid store path hash.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        use de::Error;
        String::deserialize(deserializer)
            .and_then(|s| Self::new(&s).map_err(|e| Error::custom(e.to_string())))
    }
}

impl std::fmt::Display for StorePathHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl StorePathHash {
    /// Creates a store path hash from a string.
    pub fn new(hash: &str) -> StoreResult<Self> {
        let hash = hash.to_owned();
        if hash.as_bytes().len() != STORE_PATH_HASH_LEN {
            return Err(StoreError::InvalidStorePathHash {
                hash,
                reason: "Hash is of invalid length",
            });
        }

        if !STORE_PATH_HASH_REGEX.is_match(&hash) {
            return Err(StoreError::InvalidStorePathHash {
                hash,
                reason: "Hash is of invalid format",
            });
        }

        Ok(Self(hash))
    }

    /// Creates a store path hash from a string, without checking its validity.
    ///
    /// # Safety
    ///
    /// The caller must make sure that it is of expected length and format.
    #[allow(unsafe_code)]
    pub unsafe fn new_unchecked(hash: String) -> Self {
        Self(hash)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_string(&self) -> String {
        self.0.clone()
    }
}

/// Returns the base store name of a path relative to a store root.
pub fn to_base_name(store_dir: &Path, path: &Path) -> StoreResult<PathBuf> {
    if let Ok(remaining) = path.strip_prefix(store_dir) {
        let first = remaining
            .iter()
            .next()
            .ok_or_else(|| StoreError::InvalidStorePath {
                path: path.to_owned(),
                reason: "Path is store directory itself",
            })?;

        if first.len() < STORE_PATH_HASH_LEN {
            Err(StoreError::InvalidStorePath {
                path: path.to_owned(),
                reason: "Path is too short",
            })
        } else {
            Ok(PathBuf::from(first))
        }
    } else {
        Err(StoreError::InvalidStorePath {
            path: path.to_owned(),
            reason: "Path is not in store directory",
        })
    }
}
