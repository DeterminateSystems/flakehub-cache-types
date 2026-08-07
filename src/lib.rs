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

#[cfg(feature = "bloom")]
pub mod bloom;
pub mod error;
pub mod hash;
pub mod nix_store;

pub use error::{StoreError, StoreResult};
