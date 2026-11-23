use fleetix::oauth_client::{
    ClientSourceOrigin, GOOGLE_AUTH_URI, GOOGLE_DRIVE_API, GOOGLE_TOKEN_URI,
    GcpDriveBootstrapConfig, GoogleOAuthClient, OAuthClientError, RbwClientSource, ResolveRequest,
    SecretDecryptor, SecretEncryptor, bootstrap_gcp_drive_project, discover_downloaded_client,
    fetch_from_rbw, persist_google_oauth_client, resolve_google_oauth_client, secure_remove_file,
    validate_gcp_project_id, wait_for_downloaded_client,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SAMPLE_INSTALLED: &str = r#"{
  "installed": {
    "client_id": "1234567890-abcdef.apps.googleusercontent.com",
    "project_id": "sample-drive-backup",
    "auth_uri": "https://accounts.google.com/o/oauth2/auth",
    "token_uri": "https://oauth2.googleapis.com/token",
    "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
    "client_secret": "GOCSPX-fixture-secret-value-999",
    "redirect_uris": ["http://localhost"]
  }
}"#;

struct MockDecryptor {
    plaintext: Vec<u8>,
}

impl SecretDecryptor for MockDecryptor {
    fn decrypt(&self, _path: &Path) -> Result<Vec<u8>, OAuthClientError> {
        Ok(self.plaintext.clone())
    }
}

struct MockEncryptor {
    writes: Mutex<Vec<(PathBuf, Vec<u8>)>>,
}

impl SecretEncryptor for MockEncryptor {
    fn encrypt(&self, path: &Path, plaintext: &[u8]) -> Result<(), OAuthClientError> {
        self.writes
            .lock()
            .unwrap()
            .push((path.to_path_buf(), plaintext.to_vec()));
        fs::write(path, b"age-fixture-ciphertext")?;
        Ok(())
    }
}

#[test]
fn parses_installed_and_flat_json_and_redacts_debug() {
    let client = GoogleOAuthClient::parse_json(SAMPLE_INSTALLED.as_bytes()).unwrap();
    assert_eq!(
        client.client_id(),
        "1234567890-abcdef.apps.googleusercontent.com"
    );
    assert_eq!(client.client_secret(), "GOCSPX-fixture-secret-value-999");
    assert_eq!(client.project_id(), Some("sample-drive-backup"));
    assert_eq!(client.auth_uri(), GOOGLE_AUTH_URI);
    assert_eq!(client.token_uri(), GOOGLE_TOKEN_URI);

    let debug = format!("{client:?}");
    assert!(!debug.contains("GOCSPX-fixture-secret-value-999"));
    assert!(debug.contains("[REDACTED]"));

    let flat =
        r#"{"client_id":"flat-id.apps.googleusercontent.com","client_secret":"flat-secret"}"#;
    let parsed_flat = GoogleOAuthClient::parse_json(flat.as_bytes()).unwrap();
    let canonical = parsed_flat.to_installed_json().unwrap();
    let reparsed = GoogleOAuthClient::parse_json(&canonical).unwrap();
    assert_eq!(reparsed.client_id(), "flat-id.apps.googleusercontent.com");
    assert_eq!(reparsed.client_secret(), "flat-secret");
}

#[test]
fn rejects_web_client_and_bad_endpoints() {
    let web = r#"{"web":{"client_id":"x","client_secret":"y"}}"#;
    assert!(matches!(
        GoogleOAuthClient::parse_json(web.as_bytes()),
        Err(OAuthClientError::UnsupportedClientType(_))
    ));

    let bad_token = r#"{"installed":{"client_id":"x","client_secret":"y","token_uri":"https://evil.example/token"}}"#;
    assert!(matches!(
        GoogleOAuthClient::parse_json(bad_token.as_bytes()),
        Err(OAuthClientError::Validation(_))
    ));

    let bad_auth = r#"{"installed":{"client_id":"x","client_secret":"y","auth_uri":"https://evil.example/auth"}}"#;
    assert!(matches!(
        GoogleOAuthClient::parse_json(bad_auth.as_bytes()),
        Err(OAuthClientError::Validation(_))
    ));
}

#[test]
fn parses_rbw_notes_custom_fields_and_login_pairs() {
    let notes_item = serde_json::json!({
        "name": "google-drive-oauth",
        "notes": SAMPLE_INSTALLED,
        "fields": []
    });
    let from_notes =
        GoogleOAuthClient::parse_rbw_raw(notes_item.to_string().as_bytes(), None).unwrap();
    assert_eq!(from_notes.project_id(), Some("sample-drive-backup"));

    let custom_json_item = serde_json::json!({
        "name": "google-drive-oauth",
        "notes": null,
        "fields": [
            {"name": "client_json", "value": SAMPLE_INSTALLED}
        ]
    });
    let from_named_field = GoogleOAuthClient::parse_rbw_raw(
        custom_json_item.to_string().as_bytes(),
        Some("client_json"),
    )
    .unwrap();
    assert_eq!(
        from_named_field.client_secret(),
        "GOCSPX-fixture-secret-value-999"
    );

    let pair_item = serde_json::json!({
        "name": "google-drive-oauth",
        "notes": null,
        "data": {
            "username": "pair-id.apps.googleusercontent.com",
            "password": "pair-secret-value"
        },
        "fields": [
            {"name": "project_id", "value": "pair-project-01"}
        ]
    });
    let from_pair =
        GoogleOAuthClient::parse_rbw_raw(pair_item.to_string().as_bytes(), None).unwrap();
    assert_eq!(from_pair.client_id(), "pair-id.apps.googleusercontent.com");
    assert_eq!(from_pair.client_secret(), "pair-secret-value");
    assert_eq!(from_pair.project_id(), Some("pair-project-01"));
}

#[cfg(unix)]
#[test]
fn materialize_requires_0700_dir_and_wipes_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let client = GoogleOAuthClient::parse_json(SAMPLE_INSTALLED.as_bytes()).unwrap();

    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        client.materialize_in(dir.path()),
        Err(OAuthClientError::Validation(_))
    ));

    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let materialized = client.materialize_in(dir.path()).unwrap();
    let path = materialized.path().to_path_buf();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let reparsed = GoogleOAuthClient::from_file(&path).unwrap();
    assert_eq!(reparsed, client);

    drop(materialized);
    assert!(!path.exists());
}

#[test]
fn resolves_in_priority_order_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let explicit_path = dir.path().join("explicit.json");
    fs::write(&explicit_path, SAMPLE_INSTALLED).unwrap();
    let encrypted_path = dir.path().join("stored.age");
    fs::write(&encrypted_path, b"age-ciphertext").unwrap();

    let decryptor = MockDecryptor {
        plaintext: r#"{"client_id":"stored-id.apps.googleusercontent.com","client_secret":"stored-secret"}"#
            .as_bytes()
            .to_vec(),
    };

    let resolved_explicit = resolve_google_oauth_client(&ResolveRequest {
        explicit_file: Some(&explicit_path),
        encrypted_store_path: Some(&encrypted_path),
        decryptor: Some(&decryptor),
        rbw: None,
    })
    .unwrap();
    assert!(matches!(
        resolved_explicit.origin,
        ClientSourceOrigin::ExplicitFile { .. }
    ));
    assert_eq!(
        resolved_explicit.client.client_id(),
        "1234567890-abcdef.apps.googleusercontent.com"
    );

    let resolved_encrypted = resolve_google_oauth_client(&ResolveRequest {
        explicit_file: None,
        encrypted_store_path: Some(&encrypted_path),
        decryptor: Some(&decryptor),
        rbw: None,
    })
    .unwrap();
    assert!(matches!(
        resolved_encrypted.origin,
        ClientSourceOrigin::EncryptedStore { .. }
    ));
    assert_eq!(
        resolved_encrypted.client.client_id(),
        "stored-id.apps.googleusercontent.com"
    );

    let missing_encrypted = dir.path().join("absent.age");
    assert!(matches!(
        resolve_google_oauth_client(&ResolveRequest {
            explicit_file: None,
            encrypted_store_path: Some(&missing_encrypted),
            decryptor: Some(&decryptor),
            rbw: None,
        }),
        Err(OAuthClientError::NotFound(_))
    ));

    let encryptor = MockEncryptor {
        writes: Mutex::new(Vec::new()),
    };
    let target = dir.path().join("saved.age");
    persist_google_oauth_client(&resolved_explicit.client, &target, &encryptor).unwrap();
    assert!(target.is_file());
    let writes = encryptor.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    let saved = GoogleOAuthClient::parse_json(&writes[0].1).unwrap();
    assert_eq!(saved, resolved_explicit.client);
}

#[cfg(unix)]
#[test]
fn fetches_from_mock_rbw_and_bootstraps_mock_gcloud() {
    let dir = tempfile::tempdir().unwrap();
    let mock_rbw = dir.path().join("mock-rbw");
    let rbw_payload = serde_json::json!({
        "name": "drive-oauth",
        "notes": SAMPLE_INSTALLED,
        "fields": []
    });
    fs::write(
        &mock_rbw,
        format!(
            "#!/bin/sh\nset -eu\nif [ \"$1\" = sync ]; then exit 0; fi\nif [ \"$1\" = get ]; then printf '%s' '{}'; exit 0; fi\nexit 1\n",
            rbw_payload.to_string().replace('\'', "'\\''")
        ),
    )
    .unwrap();
    fs::set_permissions(&mock_rbw, fs::Permissions::from_mode(0o755)).unwrap();

    let mut source = RbwClientSource::new("drive-oauth");
    source.sync = true;
    source.rbw_bin = mock_rbw;
    let fetched = fetch_from_rbw(&source).unwrap();
    assert_eq!(fetched.project_id(), Some("sample-drive-backup"));

    let state_file = dir.path().join("gcloud-state");
    let mock_gcloud = dir.path().join("mock-gcloud");
    fs::write(
        &mock_gcloud,
        format!(
            "#!/bin/sh\nset -eu\nSTATE='{}'\ncase \"$1 $2\" in\n  'auth list') echo 'operator@example.test' ;;\n  'projects describe') [ -f \"$STATE.created\" ] && echo 'sample-drive-backup' || exit 1 ;;\n  'projects create') touch \"$STATE.created\" ;;\n  'services list') [ -f \"$STATE.api\" ] && echo 'drive.googleapis.com' || true ;;\n  'services enable') touch \"$STATE.api\" ;;\n  *) exit 2 ;;\nesac\n",
            state_file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&mock_gcloud, fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = GcpDriveBootstrapConfig::new("sample-drive-backup", "Sample Drive Backup");
    cfg.gcloud_bin = mock_gcloud.clone();
    cfg.interactive_login = false;
    let first = bootstrap_gcp_drive_project(&cfg).unwrap();
    assert_eq!(first.account, "operator@example.test");
    assert!(first.project_created);
    assert_eq!(first.newly_enabled_apis, vec![GOOGLE_DRIVE_API.to_owned()]);
    assert!(
        first
            .console_urls
            .create_oauth_client_url
            .contains("project=sample-drive-backup")
    );

    let second = bootstrap_gcp_drive_project(&cfg).unwrap();
    assert!(!second.project_created);
    assert!(second.newly_enabled_apis.is_empty());
}

#[test]
fn discovers_waits_for_and_securely_removes_downloaded_client() {
    assert!(validate_gcp_project_id("sample-drive-backup").is_ok());
    assert!(validate_gcp_project_id("Bad_Project").is_err());
    assert!(validate_gcp_project_id("short").is_err());

    let dir = tempfile::tempdir().unwrap();
    let start = SystemTime::now() - Duration::from_secs(1);
    assert!(
        discover_downloaded_client(
            &[dir.path().to_path_buf()],
            Some("sample-drive-backup"),
            None
        )
        .unwrap()
        .is_none()
    );

    let target = dir
        .path()
        .join("client_secret_1234567890.apps.googleusercontent.com.json");
    let writer_path = target.clone();
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        fs::write(&writer_path, SAMPLE_INSTALLED).unwrap();
    });

    let (found_path, client) = wait_for_downloaded_client(
        &[dir.path().to_path_buf()],
        Some("sample-drive-backup"),
        start,
        Duration::from_secs(3),
        Duration::from_millis(25),
    )
    .unwrap();
    handle.join().unwrap();
    assert_eq!(found_path, target);
    assert_eq!(client.project_id(), Some("sample-drive-backup"));

    secure_remove_file(&found_path).unwrap();
    assert!(!found_path.exists());
}
