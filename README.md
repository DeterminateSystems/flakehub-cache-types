# flakehub-cache-types

Nix store path, hash, and error types shared by FlakeHub Cache services.

The crate provides:

- `nix_store::StorePath` and `nix_store::StorePathHash`: parsed, validated
  store paths and their 32-character base name hashes.
- `hash::Hash`: SHA-256 hashes with Nix's base16 and base32 encodings.
- `StoreError` and `StoreResult`: the error type the above share.

## Usage

```console
cargo add flakehub-cache-types
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

## Releasing

Bump the version in `Cargo.toml`, then push a matching tag:

```console
git tag v0.1.1
git push origin v0.1.1
```

The release workflow checks that the tag matches the crate version, runs
the tests, and publishes to crates.io using [trusted
publishing](https://crates.io/docs/trusted-publishing).

## Provenance and license

These types derive from [Attic](https://github.com/zhaofengli/attic) by
Zhaofeng Li and the Attic contributors. Licensed under the Apache License,
Version 2.0; see [LICENSE](LICENSE).
