use super::*;

const STORE_DIR: &str = "/nix/store";

#[test]
fn test_to_base_name() {
    let base_name = to_base_name(
        Path::new(STORE_DIR),
        Path::new("/nix/store/ia70ss13m22znbl8khrf2hq72qmh5drr-ruby-2.7.5"),
    )
    .unwrap();

    assert_eq!(
        PathBuf::from("ia70ss13m22znbl8khrf2hq72qmh5drr-ruby-2.7.5"),
        base_name
    );
}

#[test]
fn test_to_base_name_invalid_base_name() {
    // Long enough to pass the length check, but not a valid base name
    let e = to_base_name(
        Path::new(STORE_DIR),
        Path::new("/nix/store/ia70ss13m22znbl8khrf2hq72qmh5drr-foo@"),
    )
    .unwrap_err();

    assert!(matches!(e, StoreError::InvalidStorePath { .. }));
}

#[test]
fn test_to_base_name_too_short() {
    let e = to_base_name(Path::new(STORE_DIR), Path::new("/nix/store/foo")).unwrap_err();

    assert!(matches!(
        e,
        StoreError::InvalidStorePath {
            reason: "Path is too short",
            ..
        }
    ));
}

#[test]
fn test_to_base_name_not_in_store() {
    let e = to_base_name(Path::new(STORE_DIR), Path::new("/tmp/foo")).unwrap_err();

    assert!(matches!(e, StoreError::InvalidStorePath { .. }));
}
