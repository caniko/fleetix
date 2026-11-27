//! Reusable Google OAuth client credential validation, resolution, and GCP
//! Drive bootstrap helpers.
//!
//! Google provides no public REST API or `gcloud` command to create consumer
//! "TVs and Limited Input devices" or "Desktop app" OAuth 2.0 client IDs.
//! This module automates everything around that boundary:
//!
//! - Parsing, validating, and canonicalizing Google Cloud Console OAuth client
//!   JSON (`{"installed": ...}` or flat JSON) and Bitwarden (`rbw get --raw`)
//!   vault items without leaking `client_secret` in debug/error output.
//! - Resolving reusable OAuth client credentials from an explicit file, an
//!   encrypted store (via [`SecretDecryptor`]), or Bitwarden (`rbw`), and
//!   materializing them into a private `0600` file wiped on [`Drop`].
//! - Bootstrapping a dedicated Google Cloud project and enabling required APIs
//!   (such as `drive.googleapis.com`) via `gcloud`, returning exact Console
//!   deep links and watching a download directory to capture and securely wipe
//!   the newly downloaded `client_secret_*.json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{Ordering, compiler_fence};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// Canonical Google OAuth 2.0 authorization endpoint.
pub const GOOGLE_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/auth";
/// Canonical Google OAuth 2.0 token endpoint (fixed to prevent credential redirection).
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Default Google Drive API service name enabled during Drive bootstrap.
pub const GOOGLE_DRIVE_API: &str = "drive.googleapis.com";
/// Default per-command deadline for `rbw` vault queries.
pub const DEFAULT_RBW_TIMEOUT: Duration = Duration::from_secs(120);

const MAX_CLIENT_JSON_BYTES: usize = 64 * 1024;
const MAX_RBW_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_CREDENTIAL_LEN: usize = 512;

/// Errors returned by Google OAuth client resolution, validation, and bootstrap.
#[derive(Debug)]
pub enum OAuthClientError {
    /// Input JSON could not be parsed or exceeded size bounds.
    InvalidJson(String),
    /// The JSON declared an unsupported OAuth client type (such as `"web"`).
    UnsupportedClientType(String),
    /// Parsed credentials or configuration failed validation.
    Validation(String),
    /// Filesystem or process I/O error.
    Io(io::Error),
    /// Consumer-supplied secret decryption failed.
    Decrypt(String),
    /// Consumer-supplied secret encryption failed.
    Encrypt(String),
    /// Bitwarden (`rbw`) retrieval failed.
    Rbw(String),
    /// `gcloud` has no active authenticated account and interactive login was disabled.
    GcloudUnauthenticated,
    /// A `gcloud` CLI step failed.
    Gcloud(String),
    /// Waiting for an external command or downloaded file timed out.
    Timeout(String),
    /// No configured source yielded Google OAuth client credentials.
    NotFound(String),
}

impl fmt::Display for OAuthClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(msg)
            | Self::UnsupportedClientType(msg)
            | Self::Validation(msg)
            | Self::Decrypt(msg)
            | Self::Encrypt(msg)
            | Self::Rbw(msg)
            | Self::Gcloud(msg)
            | Self::Timeout(msg)
            | Self::NotFound(msg) => f.write_str(msg),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::GcloudUnauthenticated => f.write_str(
                "gcloud has no active account; run `gcloud auth login` or enable interactive login",
            ),
        }
    }
}

impl std::error::Error for OAuthClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for OAuthClientError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Validated Google OAuth 2.0 installed/device client credentials.
///
/// `client_secret` is redacted from [`fmt::Debug`] and zeroed in memory on [`Drop`].
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleOAuthClient {
    client_id: String,
    client_secret: Vec<u8>,
    project_id: Option<String>,
    auth_uri: String,
    token_uri: String,
    auth_provider_x509_cert_url: Option<String>,
    redirect_uris: Vec<String>,
}

impl Drop for GoogleOAuthClient {
    fn drop(&mut self) {
        wipe_bytes(&mut self.client_secret);
    }
}

impl fmt::Debug for GoogleOAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleOAuthClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("project_id", &self.project_id)
            .field("auth_uri", &self.auth_uri)
            .field("token_uri", &self.token_uri)
            .field(
                "auth_provider_x509_cert_url",
                &self.auth_provider_x509_cert_url,
            )
            .field("redirect_uris", &self.redirect_uris)
            .finish()
    }
}

#[derive(Deserialize)]
struct RawInstalledEnvelope {
    installed: Option<RawClientFields>,
    web: Option<Value>,
    client_id: Option<String>,
    client_secret: Option<String>,
    project_id: Option<String>,
    auth_uri: Option<String>,
    token_uri: Option<String>,
    auth_provider_x509_cert_url: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
}

#[derive(Deserialize)]
struct RawClientFields {
    client_id: String,
    client_secret: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    auth_uri: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    auth_provider_x509_cert_url: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
}

#[derive(Serialize)]
struct CanonicalEnvelope<'a> {
    installed: CanonicalInstalled<'a>,
}

#[derive(Serialize)]
struct CanonicalInstalled<'a> {
    client_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<&'a str>,
    auth_uri: &'a str,
    token_uri: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_provider_x509_cert_url: Option<&'a str>,
    client_secret: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    redirect_uris: &'a Vec<String>,
}

/// Public non-secret summary of a validated [`GoogleOAuthClient`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoogleOAuthClientSummary {
    /// Public OAuth client identifier.
    pub client_id: String,
    /// Optional Google Cloud project identifier from the client JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Token endpoint URI (always Google's canonical token endpoint).
    pub token_uri: String,
}

impl GoogleOAuthClient {
    /// Construct and validate Google OAuth client credentials directly.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        project_id: Option<String>,
    ) -> Result<Self, OAuthClientError> {
        let client = Self {
            client_id: client_id.into(),
            client_secret: client_secret.into().into_bytes(),
            project_id,
            auth_uri: GOOGLE_AUTH_URI.to_owned(),
            token_uri: GOOGLE_TOKEN_URI.to_owned(),
            auth_provider_x509_cert_url: None,
            redirect_uris: Vec::new(),
        };
        client.validate()?;
        Ok(client)
    }

    /// Parse and validate a Google Cloud Console `client_secret_*.json` document
    /// (either `{"installed": ...}` or flat `{"client_id", "client_secret"}`).
    pub fn parse_json(bytes: &[u8]) -> Result<Self, OAuthClientError> {
        if bytes.is_empty() {
            return Err(OAuthClientError::InvalidJson(
                "Google OAuth client JSON is empty".to_owned(),
            ));
        }
        if bytes.len() > MAX_CLIENT_JSON_BYTES {
            return Err(OAuthClientError::InvalidJson(format!(
                "Google OAuth client JSON exceeds {MAX_CLIENT_JSON_BYTES} bytes"
            )));
        }
        let raw: RawInstalledEnvelope = serde_json::from_slice(bytes).map_err(|_| {
            OAuthClientError::InvalidJson("invalid Google OAuth client JSON".to_owned())
        })?;
        if raw.installed.is_none() && raw.web.is_some() {
            return Err(OAuthClientError::UnsupportedClientType(
                "Google Web OAuth client JSON is not supported; create a 'TVs and Limited Input devices' or 'Desktop app' OAuth client".to_owned(),
            ));
        }
        let fields = match raw.installed {
            Some(installed) => installed,
            None => RawClientFields {
                client_id: raw.client_id.ok_or_else(|| {
                    OAuthClientError::InvalidJson(
                        "Google OAuth client JSON is missing installed.client_id or client_id"
                            .to_owned(),
                    )
                })?,
                client_secret: raw.client_secret.ok_or_else(|| {
                    OAuthClientError::InvalidJson(
                        "Google OAuth client JSON is missing installed.client_secret or client_secret"
                            .to_owned(),
                    )
                })?,
                project_id: raw.project_id,
                auth_uri: raw.auth_uri,
                token_uri: raw.token_uri,
                auth_provider_x509_cert_url: raw.auth_provider_x509_cert_url,
                redirect_uris: raw.redirect_uris,
            },
        };
        let client = Self {
            client_id: fields.client_id.trim().to_owned(),
            client_secret: fields.client_secret.trim().as_bytes().to_vec(),
            project_id: fields
                .project_id
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty()),
            auth_uri: fields
                .auth_uri
                .map(|u| u.trim().to_owned())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| GOOGLE_AUTH_URI.to_owned()),
            token_uri: fields
                .token_uri
                .map(|u| u.trim().to_owned())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| GOOGLE_TOKEN_URI.to_owned()),
            auth_provider_x509_cert_url: fields
                .auth_provider_x509_cert_url
                .map(|u| u.trim().to_owned())
                .filter(|u| !u.is_empty()),
            redirect_uris: fields.redirect_uris,
        };
        client.validate()?;
        Ok(client)
    }

    /// Parse Google OAuth client credentials from `rbw get --raw <item>` JSON output.
    ///
    /// When `field` is `Some(name)`, reads JSON from `"notes"` or the named custom
    /// field. Otherwise attempts:
    /// 1. `notes` as a Google OAuth client JSON document,
    /// 2. any custom field value that parses as a Google OAuth client JSON document,
    /// 3. paired `client_id` / `client_secret` custom fields (or `data.username` / `data.password`).
    pub fn parse_rbw_raw(bytes: &[u8], field: Option<&str>) -> Result<Self, OAuthClientError> {
        if bytes.is_empty() || bytes.len() > MAX_RBW_OUTPUT_BYTES {
            return Err(OAuthClientError::Rbw(
                "invalid rbw item snapshot size".to_owned(),
            ));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| OAuthClientError::Rbw("rbw did not return valid item JSON".to_owned()))?;

        if let Some(target_field) = field {
            let text = extract_named_rbw_field(&value, target_field).ok_or_else(|| {
                OAuthClientError::Rbw(format!(
                    "Bitwarden item does not contain non-empty field '{target_field}'"
                ))
            })?;
            return Self::parse_json(text.as_bytes());
        }

        if let Some(notes) = value.get("notes").and_then(Value::as_str) {
            let trimmed = notes.trim();
            if trimmed.starts_with('{') {
                return Self::parse_json(trimmed.as_bytes());
            }
        }

        if let Some(fields) = value.get("fields").and_then(Value::as_array) {
            for entry in fields {
                if let Some(val) = entry.get("value").and_then(Value::as_str) {
                    let trimmed = val.trim();
                    if trimmed.starts_with('{') {
                        if let Ok(parsed) = Self::parse_json(trimmed.as_bytes()) {
                            return Ok(parsed);
                        }
                    }
                }
            }
        }

        let client_id = find_rbw_field_any(&value, &["client_id", "client-id", "clientId"])
            .or_else(|| {
                value
                    .pointer("/data/username")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });
        let client_secret =
            find_rbw_field_any(&value, &["client_secret", "client-secret", "clientSecret"])
                .or_else(|| {
                    value
                        .pointer("/data/password")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                });
        let project_id = find_rbw_field_any(&value, &["project_id", "project-id", "projectId"])
            .map(str::to_owned);

        match (client_id, client_secret) {
            (Some(id), Some(secret)) => Self::new(id, secret, project_id),
            _ => Err(OAuthClientError::Rbw(
                "Bitwarden item must store the Google client JSON in notes/custom field, or provide client_id and client_secret fields".to_owned(),
            )),
        }
    }

    /// Read and validate a Google OAuth client JSON file from disk.
    pub fn from_file(path: &Path) -> Result<Self, OAuthClientError> {
        let meta = fs::metadata(path)?;
        if !meta.is_file() {
            return Err(OAuthClientError::Validation(format!(
                "Google OAuth client path is not a regular file: {}",
                path.display()
            )));
        }
        if meta.len() > MAX_CLIENT_JSON_BYTES as u64 {
            return Err(OAuthClientError::InvalidJson(format!(
                "Google OAuth client file exceeds {MAX_CLIENT_JSON_BYTES} bytes: {}",
                path.display()
            )));
        }
        let mut bytes = fs::read(path)?;
        let result = Self::parse_json(&bytes);
        wipe_bytes(&mut bytes);
        result
    }

    /// Validate endpoints, client ID, client secret, and optional project ID.
    pub fn validate(&self) -> Result<(), OAuthClientError> {
        validate_credential_token("client_id", &self.client_id)?;
        let secret = self.client_secret();
        validate_credential_token("client_secret", secret)?;
        if self.token_uri != GOOGLE_TOKEN_URI {
            return Err(OAuthClientError::Validation(format!(
                "Google OAuth token_uri must be {GOOGLE_TOKEN_URI}"
            )));
        }
        if !self.auth_uri.starts_with("https://accounts.google.com/") {
            return Err(OAuthClientError::Validation(
                "Google OAuth auth_uri must point to https://accounts.google.com/".to_owned(),
            ));
        }
        if let Some(project_id) = &self.project_id {
            validate_gcp_project_id(project_id)?;
        }
        Ok(())
    }

    /// Public OAuth client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// OAuth client secret (keep private; never log or pass via argv/env).
    pub fn client_secret(&self) -> &str {
        std::str::from_utf8(&self.client_secret).unwrap_or_default()
    }

    /// Optional Google Cloud project ID recorded in the client JSON.
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// Authorization endpoint URI.
    pub fn auth_uri(&self) -> &str {
        &self.auth_uri
    }

    /// Token endpoint URI.
    pub fn token_uri(&self) -> &str {
        &self.token_uri
    }

    /// Redirect URIs recorded in the client JSON.
    pub fn redirect_uris(&self) -> &[String] {
        &self.redirect_uris
    }

    /// Public non-secret summary suitable for status output or JSON envelopes.
    pub fn summary(&self) -> GoogleOAuthClientSummary {
        GoogleOAuthClientSummary {
            client_id: self.client_id.clone(),
            project_id: self.project_id.clone(),
            token_uri: self.token_uri.clone(),
        }
    }

    /// Serialize to canonical Google Cloud Console `{"installed": ...}` JSON.
    pub fn to_installed_json(&self) -> Result<Vec<u8>, OAuthClientError> {
        self.validate()?;
        let envelope = CanonicalEnvelope {
            installed: CanonicalInstalled {
                client_id: &self.client_id,
                project_id: self.project_id.as_deref(),
                auth_uri: &self.auth_uri,
                token_uri: &self.token_uri,
                auth_provider_x509_cert_url: self.auth_provider_x509_cert_url.as_deref(),
                client_secret: self.client_secret(),
                redirect_uris: &self.redirect_uris,
            },
        };
        serde_json::to_vec_pretty(&envelope).map_err(|err| {
            OAuthClientError::InvalidJson(format!("serialize Google OAuth client JSON: {err}"))
        })
    }

    /// Write canonical installed JSON into a private `0600` temporary file inside
    /// `private_dir`. The file is overwritten with zeros and unlinked on [`Drop`].
    pub fn materialize_in(
        &self,
        private_dir: &Path,
    ) -> Result<MaterializedClientFile, OAuthClientError> {
        verify_private_directory(private_dir)?;
        let mut bytes = self.to_installed_json()?;
        let mut temp = tempfile::Builder::new()
            .prefix("google-oauth-client-")
            .suffix(".json")
            .tempfile_in(private_dir)?;
        #[cfg(unix)]
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o600))?;
        let write_result = temp
            .as_file_mut()
            .write_all(&bytes)
            .and_then(|()| temp.as_file().sync_all());
        wipe_bytes(&mut bytes);
        write_result?;
        let (_, path) = temp.keep().map_err(|err| OAuthClientError::Io(err.error))?;
        Ok(MaterializedClientFile { path })
    }
}

/// Private `0600` materialized Google OAuth client JSON file that overwrites
/// its contents with zeros and unlinks itself on [`Drop`].
#[derive(Debug)]
pub struct MaterializedClientFile {
    path: PathBuf,
}

impl MaterializedClientFile {
    /// Path to the materialized client JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MaterializedClientFile {
    fn drop(&mut self) {
        let _ = secure_remove_file(&self.path);
    }
}

/// Hook for decrypting an encrypted client credential file (for example agenix/age).
pub trait SecretDecryptor {
    /// Decrypt `path` and return plaintext bytes.
    fn decrypt(&self, path: &Path) -> Result<Vec<u8>, OAuthClientError>;
}

/// Hook for encrypting canonical client JSON into a durable store (for example agenix/age).
pub trait SecretEncryptor {
    /// Encrypt `plaintext` and persist the ciphertext at `path`.
    fn encrypt(&self, path: &Path, plaintext: &[u8]) -> Result<(), OAuthClientError>;
}

/// Configuration for resolving a Google OAuth client JSON from Bitwarden via `rbw`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbwClientSource {
    /// Exact Bitwarden item name or UUID.
    pub item: String,
    /// Optional custom field name (or `"notes"`) holding the client JSON.
    pub field: Option<String>,
    /// Run `rbw sync` before reading the item.
    pub sync: bool,
    /// Maximum runtime per `rbw` invocation.
    pub timeout: Duration,
    /// Path to the `rbw` executable (defaults to `"rbw"`).
    pub rbw_bin: PathBuf,
}

impl RbwClientSource {
    /// Construct a Bitwarden source using the default `rbw` executable and timeout.
    pub fn new(item: impl Into<String>) -> Self {
        Self {
            item: item.into(),
            field: None,
            sync: false,
            timeout: DEFAULT_RBW_TIMEOUT,
            rbw_bin: PathBuf::from("rbw"),
        }
    }
}

/// Where resolved Google OAuth client credentials came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClientSourceOrigin {
    /// Supplied directly via `--client-file`.
    ExplicitFile {
        /// Path to the supplied file.
        path: PathBuf,
    },
    /// Decrypted from the durable encrypted secret store (for example agenix).
    EncryptedStore {
        /// Path to the encrypted source file.
        path: PathBuf,
    },
    /// Retrieved on demand from Bitwarden through `rbw`.
    BitwardenRbw {
        /// Bitwarden item selector.
        item: String,
    },
}

/// Resolved Google OAuth client credentials paired with provenance metadata.
#[derive(Debug)]
pub struct ResolvedGoogleOAuthClient {
    /// Validated OAuth client credentials.
    pub client: GoogleOAuthClient,
    /// Provenance of the resolved credentials.
    pub origin: ClientSourceOrigin,
}

/// Ordered resolution inputs for [`resolve_google_oauth_client`].
pub struct ResolveRequest<'a> {
    /// Explicit local client JSON path override (`--client-file`).
    pub explicit_file: Option<&'a Path>,
    /// Canonical encrypted secret path (for example `age/secrets/.../*.age`).
    pub encrypted_store_path: Option<&'a Path>,
    /// Decryptor invoked when `encrypted_store_path` exists.
    pub decryptor: Option<&'a dyn SecretDecryptor>,
    /// Bitwarden (`rbw`) fallback source.
    pub rbw: Option<&'a RbwClientSource>,
}

/// Resolve Google OAuth client credentials in priority order:
/// 1. explicit file,
/// 2. encrypted store file (if present),
/// 3. Bitwarden (`rbw`) item.
pub fn resolve_google_oauth_client(
    request: &ResolveRequest<'_>,
) -> Result<ResolvedGoogleOAuthClient, OAuthClientError> {
    if let Some(path) = request.explicit_file {
        let client = GoogleOAuthClient::from_file(path)?;
        return Ok(ResolvedGoogleOAuthClient {
            client,
            origin: ClientSourceOrigin::ExplicitFile {
                path: path.to_path_buf(),
            },
        });
    }

    if let Some(encrypted_path) = request.encrypted_store_path {
        if encrypted_path.is_file() {
            let decryptor = request.decryptor.ok_or_else(|| {
                OAuthClientError::Decrypt(format!(
                    "encrypted Google OAuth client exists at {} but no decryptor was configured",
                    encrypted_path.display()
                ))
            })?;
            let mut plaintext = decryptor.decrypt(encrypted_path)?;
            let parsed = GoogleOAuthClient::parse_json(&plaintext);
            wipe_bytes(&mut plaintext);
            let client = parsed?;
            return Ok(ResolvedGoogleOAuthClient {
                client,
                origin: ClientSourceOrigin::EncryptedStore {
                    path: encrypted_path.to_path_buf(),
                },
            });
        }
    }

    if let Some(rbw) = request.rbw {
        let client = fetch_from_rbw(rbw)?;
        return Ok(ResolvedGoogleOAuthClient {
            client,
            origin: ClientSourceOrigin::BitwardenRbw {
                item: rbw.item.clone(),
            },
        });
    }

    Err(OAuthClientError::NotFound(
        "no Google OAuth client credentials found; bootstrap and store one, pass --client-from-rbw <item>, or pass --client-file <path>".to_owned(),
    ))
}

/// Retrieve and validate Google OAuth client credentials from Bitwarden using `rbw`.
pub fn fetch_from_rbw(source: &RbwClientSource) -> Result<GoogleOAuthClient, OAuthClientError> {
    if source.item.trim().is_empty() {
        return Err(OAuthClientError::Validation(
            "Bitwarden item name/UUID cannot be empty".to_owned(),
        ));
    }
    if source.sync {
        let mut sync_cmd = Command::new(&source.rbw_bin);
        sync_cmd
            .arg("sync")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let output = run_with_timeout(&mut sync_cmd, source.timeout, "rbw sync")?;
        if !output.status.success() {
            return Err(OAuthClientError::Rbw(
                "rbw sync failed (check rbw login/unlock status)".to_owned(),
            ));
        }
    }

    let mut get_cmd = Command::new(&source.rbw_bin);
    get_cmd
        .args(["get", "--raw", "--"])
        .arg(&source.item)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut output = run_with_timeout(&mut get_cmd, source.timeout, "rbw get --raw")?;
    if !output.status.success() {
        wipe_bytes(&mut output.stdout);
        return Err(OAuthClientError::Rbw(format!(
            "rbw get --raw failed for item '{}' (check rbw unlock status and item name/UUID)",
            source.item
        )));
    }
    let parsed = GoogleOAuthClient::parse_rbw_raw(&output.stdout, source.field.as_deref());
    wipe_bytes(&mut output.stdout);
    parsed
}

/// Encrypt canonical Google OAuth client JSON into `target_path` via `encryptor`.
pub fn persist_google_oauth_client(
    client: &GoogleOAuthClient,
    target_path: &Path,
    encryptor: &dyn SecretEncryptor,
) -> Result<(), OAuthClientError> {
    let mut plaintext = client.to_installed_json()?;
    let result = encryptor.encrypt(target_path, &plaintext);
    wipe_bytes(&mut plaintext);
    result
}

/// Google Cloud Console deep links for configuring the OAuth consent screen
/// and creating the OAuth client inside a bootstrapped GCP project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcpConsoleUrls {
    /// OAuth consent screen / Audience page (publish to In production).
    pub audience_url: String,
    /// Direct "Create OAuth client ID" form URL.
    pub create_oauth_client_url: String,
    /// Credentials overview page for downloading an existing client JSON.
    pub credentials_url: String,
}

impl GcpConsoleUrls {
    /// Construct Console URLs scoped to `project_id`.
    pub fn for_project(project_id: &str) -> Self {
        Self {
            audience_url: format!(
                "https://console.cloud.google.com/auth/audience?project={project_id}"
            ),
            create_oauth_client_url: format!(
                "https://console.cloud.google.com/apis/credentials/oauthclient?project={project_id}"
            ),
            credentials_url: format!(
                "https://console.cloud.google.com/apis/credentials?project={project_id}"
            ),
        }
    }
}

/// Configuration for [`bootstrap_gcp_drive_project`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpDriveBootstrapConfig {
    /// Target Google Cloud project ID (6-30 lowercase letters, digits, hyphens).
    pub project_id: String,
    /// Human-readable display name when creating the project.
    pub display_name: String,
    /// Path to `gcloud` executable (defaults to `"gcloud"`).
    pub gcloud_bin: PathBuf,
    /// Invoke interactive `gcloud auth login` when no active account is present.
    pub interactive_login: bool,
    /// Google Cloud APIs to enable (defaults to `["drive.googleapis.com"]`).
    pub enable_apis: Vec<String>,
}

impl GcpDriveBootstrapConfig {
    /// Construct default bootstrap settings for enabling Google Drive on `project_id`.
    pub fn new(project_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            display_name: display_name.into(),
            gcloud_bin: PathBuf::from("gcloud"),
            interactive_login: true,
            enable_apis: vec![GOOGLE_DRIVE_API.to_owned()],
        }
    }
}

/// Structured outcome from [`bootstrap_gcp_drive_project`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcpBootstrapReport {
    /// Active authenticated `gcloud` account.
    pub account: String,
    /// Bootstrapped GCP project ID.
    pub project_id: String,
    /// True if the project did not exist and was created during this run.
    pub project_created: bool,
    /// All requested APIs confirmed enabled after bootstrap.
    pub enabled_apis: Vec<String>,
    /// Subset of APIs that were newly enabled during this run.
    pub newly_enabled_apis: Vec<String>,
    /// Direct Google Cloud Console links for the final OAuth client creation click.
    pub console_urls: GcpConsoleUrls,
}

/// Validate a Google Cloud project ID against GCP naming constraints.
pub fn validate_gcp_project_id(project_id: &str) -> Result<(), OAuthClientError> {
    let bytes = project_id.as_bytes();
    if !(6..=30).contains(&bytes.len()) {
        return Err(OAuthClientError::Validation(format!(
            "GCP project ID '{project_id}' must be 6-30 characters long"
        )));
    }
    if !bytes[0].is_ascii_lowercase() || bytes[bytes.len() - 1] == b'-' {
        return Err(OAuthClientError::Validation(format!(
            "GCP project ID '{project_id}' must start with a lowercase letter and not end with a hyphen"
        )));
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
    {
        return Err(OAuthClientError::Validation(format!(
            "GCP project ID '{project_id}' may only contain lowercase letters, digits, and hyphens"
        )));
    }
    Ok(())
}

/// Ensure a dedicated Google Cloud project exists and has the required Drive APIs
/// enabled through `gcloud`, returning Console deep links for OAuth client creation.
pub fn bootstrap_gcp_drive_project(
    config: &GcpDriveBootstrapConfig,
) -> Result<GcpBootstrapReport, OAuthClientError> {
    validate_gcp_project_id(&config.project_id)?;
    if config.display_name.trim().is_empty() {
        return Err(OAuthClientError::Validation(
            "GCP project display name cannot be empty".to_owned(),
        ));
    }

    let mut account = active_gcloud_account(&config.gcloud_bin)?;
    if account.is_none() {
        if !config.interactive_login {
            return Err(OAuthClientError::GcloudUnauthenticated);
        }
        let status = Command::new(&config.gcloud_bin)
            .args(["auth", "login"])
            .status()
            .map_err(|err| OAuthClientError::Gcloud(format!("launch gcloud auth login: {err}")))?;
        if !status.success() {
            return Err(OAuthClientError::Gcloud(
                "gcloud auth login failed".to_owned(),
            ));
        }
        account = active_gcloud_account(&config.gcloud_bin)?;
    }
    let account = account.ok_or(OAuthClientError::GcloudUnauthenticated)?;

    let describe = Command::new(&config.gcloud_bin)
        .args([
            "projects",
            "describe",
            &config.project_id,
            "--format=value(projectId)",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| OAuthClientError::Gcloud(format!("run gcloud projects describe: {err}")))?;

    let project_created = if describe.status.success() {
        false
    } else {
        let create = Command::new(&config.gcloud_bin)
            .args([
                "projects",
                "create",
                &config.project_id,
                "--name",
                &config.display_name,
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                OAuthClientError::Gcloud(format!("run gcloud projects create: {err}"))
            })?;
        if !create.status.success() {
            let stderr = String::from_utf8_lossy(&create.stderr).trim().to_owned();
            return Err(OAuthClientError::Gcloud(format!(
                "gcloud projects create '{}' failed: {stderr}",
                config.project_id
            )));
        }
        true
    };

    let list_services = Command::new(&config.gcloud_bin)
        .args([
            "services",
            "list",
            "--enabled",
            "--project",
            &config.project_id,
            "--format=value(config.name)",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| OAuthClientError::Gcloud(format!("run gcloud services list: {err}")))?;
    if !list_services.status.success() {
        let stderr = String::from_utf8_lossy(&list_services.stderr)
            .trim()
            .to_owned();
        return Err(OAuthClientError::Gcloud(format!(
            "gcloud services list failed for '{}': {stderr}",
            config.project_id
        )));
    }
    let already_enabled: Vec<String> = String::from_utf8_lossy(&list_services.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    let newly_enabled_apis: Vec<String> = config
        .enable_apis
        .iter()
        .filter(|api| !already_enabled.iter().any(|existing| existing == *api))
        .cloned()
        .collect();

    if !newly_enabled_apis.is_empty() {
        let mut enable_cmd = Command::new(&config.gcloud_bin);
        enable_cmd
            .args(["services", "enable"])
            .args(&newly_enabled_apis)
            .args(["--project", &config.project_id])
            .stdin(Stdio::null());
        let enabled = enable_cmd.output().map_err(|err| {
            OAuthClientError::Gcloud(format!("run gcloud services enable: {err}"))
        })?;
        if !enabled.status.success() {
            let stderr = String::from_utf8_lossy(&enabled.stderr).trim().to_owned();
            return Err(OAuthClientError::Gcloud(format!(
                "gcloud services enable failed for '{}': {stderr}",
                config.project_id
            )));
        }
    }

    Ok(GcpBootstrapReport {
        account,
        project_id: config.project_id.clone(),
        project_created,
        enabled_apis: config.enable_apis.clone(),
        newly_enabled_apis,
        console_urls: GcpConsoleUrls::for_project(&config.project_id),
    })
}

/// Scan `dirs` (non-recursively) for the newest valid Google `client_secret_*.json`
/// download, optionally filtered by `expected_project_id` and modification time.
pub fn discover_downloaded_client(
    dirs: &[PathBuf],
    expected_project_id: Option<&str>,
    not_before: Option<SystemTime>,
) -> Result<Option<(PathBuf, GoogleOAuthClient)>, OAuthClientError> {
    let mut best: Option<(SystemTime, PathBuf, GoogleOAuthClient)> = None;
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("client_secret")
                || path.extension().is_none_or(|ext| ext != "json")
            {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(meta) if meta.is_file() && meta.len() <= MAX_CLIENT_JSON_BYTES as u64 => meta,
                _ => continue,
            };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(floor) = not_before {
                if modified < floor {
                    continue;
                }
            }
            let Ok(client) = GoogleOAuthClient::from_file(&path) else {
                continue;
            };
            if let Some(expected) = expected_project_id {
                if client.project_id() != Some(expected) {
                    continue;
                }
            }
            let is_newer = best
                .as_ref()
                .is_none_or(|(best_time, _, _)| modified > *best_time);
            if is_newer {
                best = Some((modified, path, client));
            }
        }
    }
    Ok(best.map(|(_, path, client)| (path, client)))
}

/// Poll `dirs` until a newly downloaded `client_secret_*.json` matching
/// `expected_project_id` appears (or `timeout` expires).
pub fn wait_for_downloaded_client(
    dirs: &[PathBuf],
    expected_project_id: Option<&str>,
    not_before: SystemTime,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(PathBuf, GoogleOAuthClient), OAuthClientError> {
    let deadline = Instant::now() + timeout;
    let step = if poll_interval.is_zero() {
        Duration::from_millis(250)
    } else {
        poll_interval
    };
    loop {
        if let Some(found) =
            discover_downloaded_client(dirs, expected_project_id, Some(not_before))?
        {
            return Ok(found);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(OAuthClientError::Timeout(
                "timed out waiting for downloaded client_secret_*.json".to_owned(),
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        thread::sleep(step.min(remaining));
    }
}

/// Overwrite a plaintext credential file with zero bytes, sync, and unlink it.
pub fn secure_remove_file(path: &Path) -> Result<(), OAuthClientError> {
    if !path.exists() {
        return Ok(());
    }
    if let Ok(mut file) = fs::OpenOptions::new().read(true).write(true).open(path) {
        if let Ok(len) = file.metadata().map(|m| m.len()) {
            let zeros = [0u8; 4096];
            let _ = file.seek(SeekFrom::Start(0));
            let mut remaining = len;
            while remaining > 0 {
                let chunk = usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(zeros.len());
                if file.write_all(&zeros[..chunk]).is_err() {
                    break;
                }
                remaining -= chunk as u64;
            }
            let _ = file.sync_all();
        }
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(OAuthClientError::Io(err)),
    }
}

fn active_gcloud_account(gcloud_bin: &Path) -> Result<Option<String>, OAuthClientError> {
    let output = Command::new(gcloud_bin)
        .args([
            "auth",
            "list",
            "--filter=status:ACTIVE",
            "--format=value(account)",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| OAuthClientError::Gcloud(format!("run gcloud auth list: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(OAuthClientError::Gcloud(format!(
            "gcloud auth list failed: {stderr}"
        )));
    }
    let account = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned);
    Ok(account)
}

fn validate_credential_token(label: &str, value: &str) -> Result<(), OAuthClientError> {
    if value.is_empty() || value.len() > MAX_CREDENTIAL_LEN {
        return Err(OAuthClientError::Validation(format!(
            "Google OAuth {label} must be 1-{MAX_CREDENTIAL_LEN} characters"
        )));
    }
    if value
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(OAuthClientError::Validation(format!(
            "Google OAuth {label} contains invalid whitespace or control characters"
        )));
    }
    Ok(())
}

fn extract_named_rbw_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    if name == "notes" {
        return value
            .get("notes")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
    }
    let fields = value.get("fields").and_then(Value::as_array)?;
    for entry in fields {
        if entry.get("name").and_then(Value::as_str) == Some(name) {
            if let Some(val) = entry
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(val);
            }
        }
    }
    None
}

fn find_rbw_field_any<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    for name in names {
        if let Some(found) = extract_named_rbw_field(value, name) {
            return Some(found);
        }
    }
    None
}

fn verify_private_directory(dir: &Path) -> Result<(), OAuthClientError> {
    if !dir.is_absolute() || dir.starts_with("/nix/store") {
        return Err(OAuthClientError::Validation(format!(
            "private materialization directory must be an absolute writable path outside /nix/store: {}",
            dir.display()
        )));
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(OAuthClientError::Validation(format!(
            "private materialization path is not a directory: {}",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        if meta.mode() & 0o077 != 0 {
            return Err(OAuthClientError::Validation(format!(
                "private materialization directory must have mode 0700: {}",
                dir.display()
            )));
        }
    }
    Ok(())
}

fn run_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
) -> Result<Output, OAuthClientError> {
    let mut child = command
        .spawn()
        .map_err(|err| OAuthClientError::Rbw(format!("launch {label}: {err}")))?;
    let stdout_handle = child.stdout.take().map(|mut reader| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = reader
                .by_ref()
                .take((MAX_RBW_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut buf);
            buf
        })
    });
    let status = wait_child(&mut child, timeout, label)?;
    let stdout = match stdout_handle {
        Some(handle) => handle.join().unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn wait_child(
    child: &mut Child,
    timeout: Duration,
    label: &str,
) -> Result<std::process::ExitStatus, OAuthClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(OAuthClientError::Timeout(format!(
                "{label} timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wipe_bytes(buf: &mut Vec<u8>) {
    for byte in buf.iter_mut() {
        *byte = 0;
    }
    compiler_fence(Ordering::SeqCst);
    buf.clear();
}
