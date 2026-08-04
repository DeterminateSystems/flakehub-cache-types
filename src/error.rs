use std::error::Error as StdError;
use std::io;
use std::path::PathBuf;

use crate::nix_store::StorePath;

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, displaydoc::Display)]
pub enum StoreError {
    /// Invalid store path {path:?}: {reason}
    InvalidStorePath { path: PathBuf, reason: &'static str },

    /// Invalid store path base name {base_name:?}: {reason}
    InvalidStorePathName {
        base_name: PathBuf,
        reason: &'static str,
    },

    /// Invalid store path hash "{hash}": {reason}
    InvalidStorePathHash { hash: String, reason: &'static str },

    /// I/O error: {error}.
    IoError { error: io::Error },

    /// Unknown C++ exception: {exception}.
    CxxError { exception: String },

    /// Provenance for {path:?} was not valid JSON: {error_display}: {invalid_string}
    InvalidProvenance {
        path: StorePath,
        error_display: String,
        invalid_string: String,
    },
}

impl StoreError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::InvalidStorePath { .. } => "InvalidStorePath",
            Self::InvalidStorePathName { .. } => "InvalidStorePathName",
            Self::InvalidStorePathHash { .. } => "InvalidStorePathHash",
            Self::IoError { .. } => "IoError",
            Self::CxxError { .. } => "CxxError",
            Self::InvalidProvenance { .. } => "InvalidProvenance",
        }
    }
}

impl StdError for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::IoError { error }
    }
}

#[cfg(feature = "cxx")]
impl From<cxx::Exception> for StoreError {
    fn from(exception: cxx::Exception) -> Self {
        Self::CxxError {
            exception: exception.what().to_string(),
        }
    }
}
