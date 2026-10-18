use fleetix_sidecar::{
    CacheOptions, EvalOptions, check, check_sync, options_with_http, write_with_cache,
};
use std::fs;

#[tokio::test]
async fn http_proxy_works_on_a_single_thread_runtime_without_publishing_cache_entries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let body = "value = 42\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        for (expected, reply) in [
            (
                "CONNECT fixture.invalid:80 HTTP/1.1\r\n",
                "HTTP/1.1 200 Connection established\r\n\r\n",
            ),
            ("GET /remote.pkl HTTP/1.1\r\n", response.as_str()),
        ] {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                stream.readable().await.unwrap();
                match stream.try_read(&mut buffer) {
                    Ok(0) => panic!("proxy connection closed before request headers"),
                    Ok(count) => request.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => panic!("{error}"),
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(expected), "{request}");
            let mut bytes = reply.as_bytes();
            while !bytes.is_empty() {
                stream.writable().await.unwrap();
                match stream.try_write(bytes) {
                    Ok(count) => bytes = &bytes[count..],
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => panic!("{error}"),
                }
            }
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("Source.pkl");
    let output = directory.path().join("output.nix");
    let cache = directory.path().join("cache");
    fs::write(
        &source,
        "import \"http://fixture.invalid/remote.pkl\" as Remote\nvalue = Remote.value\n",
    )
    .unwrap();
    let outcome = write_with_cache(
        &source,
        &output,
        options_with_http(Vec::new(), Some(&proxy)).unwrap(),
        CacheOptions {
            enabled: true,
            directory: Some(cache.clone()),
        },
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert!(!outcome.cached);
    assert!(!cache.exists());
    assert!(fs::read_to_string(output).unwrap().contains("value = 42"));
    assert!(options_with_http(Vec::new(), Some("not a proxy URL")).is_err());
}

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
