# flakehub-cache-types

Nix store path, hash, and error types shared by FlakeHub Cache services.

The crate provides:

- `nix_store::StorePath` and `nix_store::StorePathHash`: parsed, validated
  store paths and their 32-character base name hashes.
- `hash::Hash`: SHA-256 hashes with Nix's base16 and base32 encodings.
- `hash::Error`: the parsing error returned by `Hash::from_typed`.
- `StoreError` and `StoreResult`: the errors returned by the store path
  operations above.

## Usage

```console
cargo add --git https://github.com/DeterminateSystems/flakehub-cache-types flakehub-cache-types
```

```rust
use flakehub_cache_types::nix_store::StorePathHash;

let hash = StorePathHash::new("ib3sh3pcz10wsmavxvkdbayhqivbghlq")?;
```

### Features

- `cxx` (off by default): implements `From<cxx::Exception>` for
  `StoreError`, for use with C++ bindings to the Nix libraries.

## Development

`nix develop` provides a Rust toolchain, and `direnv` loads it
automatically. Run the tests with `cargo test`. CI enforces `cargo fmt`,
`cargo clippy`, `cargo deny`, and editorconfig conformance.

## Provenance and license

These types derive from [Attic](https://github.com/zhaofengli/attic) by
Zhaofeng Li and the Attic contributors. Licensed under the Apache License,
Version 2.0; see [LICENSE](LICENSE).
