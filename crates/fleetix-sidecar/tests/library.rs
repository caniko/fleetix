use fleetix_sidecar::{CacheOptions, EvalOptions, check, check_sync, write_with_cache};
use std::fs;

#[tokio::test]
async fn checks_report_read_errors_and_sync_calls_refuse_nested_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Source.pkl");
    fs::write(&source, "value = 42\n").unwrap();
    assert!(
        check(&source, dir.path(), EvalOptions::default())
            .await
            .is_err()
    );
    let error = check_sync(&source, dir.path(), EvalOptions::default()).unwrap_err();
    assert!(error.to_string().contains("inside a Tokio runtime"));
}

#[tokio::test]
async fn failed_write_does_not_publish_a_cache_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Source.pkl");
    let cache = dir.path().join("cache");
    fs::write(&source, "value = 42\n").unwrap();
    let result = write_with_cache(
        &source,
        dir.path(),
        EvalOptions::default(),
        CacheOptions {
            enabled: true,
            directory: Some(cache.clone()),
        },
    )
    .await;
    assert!(result.is_err());
    assert!(!cache.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_writes_preserve_permissions_and_unchanged_outputs_keep_their_inode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Source.pkl");
    let output = dir.path().join("output.nix");
    fs::write(&source, "value = 42\n").unwrap();
    fs::write(&output, "old").unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o640)).unwrap();
    let generate = || {
        write_with_cache(
            &source,
            &output,
            EvalOptions::default(),
            CacheOptions::default(),
        )
    };
    assert!(generate().await.unwrap().changed);
    let metadata = fs::metadata(&output).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    assert!(!generate().await.unwrap().changed);
    assert_eq!(fs::metadata(&output).unwrap().ino(), metadata.ino());
    assert!(
        check(&source, &output, EvalOptions::default())
            .await
            .unwrap()
    );
}
