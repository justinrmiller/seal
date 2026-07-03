use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

/// URI schemes that address object storage rather than the local filesystem.
/// A `database.path` beginning with one of these is handed to LanceDB verbatim
/// (no project-root joining, no local directory creation). `s3+ddb://` is S3
/// with DynamoDB commit locking, which is the safe way to run concurrent
/// writers against S3.
const OBJECT_STORE_SCHEMES: &[&str] = &[
    "s3://",
    "s3a://",
    "s3+ddb://",
    "gs://",
    "gcs://",
    "az://",
    "azure://",
    "abfs://",
    "abfss://",
];

/// Returns true if `location` is an object-store URI (S3/GCS/Azure) rather than
/// a local filesystem path. Shared with `db::connect` so both agree on which
/// locations are remote.
pub fn is_object_store_uri(location: &str) -> bool {
    OBJECT_STORE_SCHEMES
        .iter()
        .any(|scheme| location.starts_with(scheme))
}

/// Redact anything credential-like from a database location so it is safe to
/// log. Object-store URIs don't normally embed credentials, but strip any
/// `scheme://user:pass@host` userinfo defensively so a secret can never reach
/// the logs.
pub fn redact_location(location: &str) -> String {
    if let Some((scheme, rest)) = location.split_once("://") {
        if let Some((_userinfo, host)) = rest.split_once('@') {
            return format!("{scheme}://***@{host}");
        }
    }
    location.to_string()
}

/// Validate the storage configuration for a resolved database location and
/// surface operational caveats. Errors on unambiguous misconfiguration; warns
/// on risky-but-valid setups. Called from [`Config::load`] so problems fail
/// fast at startup rather than as an opaque connect error later.
fn validate_storage_config(
    location: &str,
    storage_options: &HashMap<String, String>,
) -> anyhow::Result<()> {
    if !is_object_store_uri(location) {
        // `storage:` options only apply to object storage. Providing them for a
        // local path is almost certainly a mistake — they'd be silently dropped.
        if !storage_options.is_empty() {
            anyhow::bail!(
                "`storage:` options were provided but the database path ({location}) is a local \
                 filesystem path, so those options would be silently ignored. Remove the \
                 `storage:` block, or point the database path at an object-store URI \
                 (e.g. s3://, gs://, az://)."
            );
        }
        return Ok(());
    }

    // Plain S3 has no safe concurrent-write story: without DynamoDB commit
    // locking (the `s3+ddb://` scheme), concurrent writers can clobber one
    // another's commits. Seal writes messages concurrently, so warn loudly.
    if location.starts_with("s3://") || location.starts_with("s3a://") {
        tracing::warn!(
            "database is on S3 ({}) without DynamoDB commit locking; concurrent writers can \
             clobber each other's commits. Use the `s3+ddb://` scheme with a lock table (or a \
             single writer) for production S3.",
            redact_location(location)
        );
    }

    Ok(())
}

/// Default `config.yaml` baked into the binary at build time. Disk
/// `config.yaml` at the project root overrides it; env vars override either.
const EMBEDDED_CONFIG_YAML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config.yaml"));

#[derive(Debug, Clone)]
pub struct Config {
    pub app_title: String,
    pub app_host: String,
    pub app_port: u16,
    pub database_path: PathBuf,
    /// Options passed to LanceDB's storage layer (S3/GCS/Azure credentials,
    /// endpoint, region, etc.). Empty for local filesystem storage.
    pub storage_options: HashMap<String, String>,
    pub jwt_secret: String,
    pub jwt_algorithm: String,
    pub token_expire_minutes: i64,
    pub username_max_length: usize,
    pub id_max_length: usize,
    pub max_image_size_bytes: usize,
    pub safe_name_re: Regex,
    pub safe_id_re: Regex,
}

#[derive(Debug, Deserialize)]
struct YamlConfig {
    app: AppSection,
    database: DatabaseSection,
    auth: AuthSection,
    validation: ValidationSection,
    #[serde(default)]
    attachments: AttachmentsSection,
    /// Free-form key/value options forwarded to LanceDB's storage layer. Used
    /// to configure object storage (e.g. `endpoint`, `region`, `allow_http`,
    /// `aws_access_key_id`). Standard cloud env vars work too when this is empty.
    #[serde(default)]
    storage: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AppSection {
    title: String,
    host: String,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct DatabaseSection {
    path: String,
}

#[derive(Debug, Deserialize)]
struct AuthSection {
    jwt_algorithm: String,
    token_expire_minutes: i64,
}

#[derive(Debug, Deserialize)]
struct ValidationSection {
    username_max_length: usize,
    id_max_length: usize,
}

#[derive(Debug, Deserialize, Default)]
struct AttachmentsSection {
    #[serde(default = "default_image_size_mb")]
    max_image_size_mb: usize,
}

fn default_image_size_mb() -> usize {
    5
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // Load .env. If SEAL_PROJECT_ROOT is set, look there explicitly;
        // otherwise walk up from CWD (standard dotenvy behavior). Missing
        // .env is fine — env vars may already be set in the process.
        if let Ok(dir) = std::env::var("SEAL_PROJECT_ROOT") {
            let _ = dotenvy::from_path(PathBuf::from(&dir).join(".env"));
        } else {
            let _ = dotenvy::dotenv();
        }

        let project_root = Self::project_root();
        let yaml_text = Self::read_yaml_or_embedded(&project_root)?;
        let yaml: YamlConfig = serde_yml::from_str(&yaml_text)?;

        let app_title = env_or("APP_TITLE", &yaml.app.title);
        let app_host = env_or("APP_HOST", &yaml.app.host);
        let app_port: u16 = env_or_parse("APP_PORT", yaml.app.port)?;

        let database_location = env_or("DATABASE_PATH", &yaml.database.path);
        let remote = is_object_store_uri(&database_location);
        let database_path = if remote {
            // Object-store URIs are handed to LanceDB verbatim — never joined
            // onto the project root or otherwise normalized as a local path.
            PathBuf::from(&database_location)
        } else {
            let database_path = PathBuf::from(&database_location);
            if database_path.is_absolute() {
                database_path
            } else {
                project_root.join(database_path)
            }
        };

        // Fail fast on misconfigured storage, and warn on risky-but-valid setups.
        validate_storage_config(&database_location, &yaml.storage)?;
        tracing::info!(
            "database location: {} ({})",
            redact_location(&database_path.to_string_lossy()),
            if remote {
                "object storage"
            } else {
                "local filesystem"
            }
        );

        let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| {
            tracing::warn!(
                "JWT_SECRET is using the default value 'change-me'. \
                Set JWT_SECRET in your .env file for production use."
            );
            "change-me".to_string()
        });

        let jwt_algorithm = env_or("AUTH_JWT_ALGORITHM", &yaml.auth.jwt_algorithm);
        let token_expire_minutes: i64 =
            env_or_parse("AUTH_TOKEN_EXPIRE_MINUTES", yaml.auth.token_expire_minutes)?;

        let max_image_size_bytes = yaml.attachments.max_image_size_mb * 1024 * 1024;

        let safe_name_re = Regex::new(&format!(
            r"^[a-zA-Z0-9_\-]{{1,{}}}$",
            yaml.validation.username_max_length
        ))?;
        let safe_id_re = Regex::new(&format!(
            r"^[a-zA-Z0-9_\-]{{1,{}}}$",
            yaml.validation.id_max_length
        ))?;

        Ok(Self {
            app_title,
            app_host,
            app_port,
            database_path,
            storage_options: yaml.storage,
            jwt_secret,
            jwt_algorithm,
            token_expire_minutes,
            username_max_length: yaml.validation.username_max_length,
            id_max_length: yaml.validation.id_max_length,
            max_image_size_bytes,
            safe_name_re,
            safe_id_re,
        })
    }

    /// Test-only: construct a Config from `config.yaml` with explicit
    /// overrides for the database path and JWT secret, ignoring env vars.
    /// Used by integration tests so concurrent tests don't race on process-
    /// global environment state.
    pub fn for_test(database_path: PathBuf, jwt_secret: String) -> anyhow::Result<Self> {
        let project_root = Self::project_root();
        let yaml_text = Self::read_yaml_or_embedded(&project_root)?;
        let yaml: YamlConfig = serde_yml::from_str(&yaml_text)?;
        let max_image_size_bytes = yaml.attachments.max_image_size_mb * 1024 * 1024;
        let safe_name_re = Regex::new(&format!(
            r"^[a-zA-Z0-9_\-]{{1,{}}}$",
            yaml.validation.username_max_length
        ))?;
        let safe_id_re = Regex::new(&format!(
            r"^[a-zA-Z0-9_\-]{{1,{}}}$",
            yaml.validation.id_max_length
        ))?;
        Ok(Self {
            app_title: yaml.app.title,
            app_host: yaml.app.host,
            app_port: yaml.app.port,
            database_path,
            storage_options: HashMap::new(),
            jwt_secret,
            jwt_algorithm: yaml.auth.jwt_algorithm,
            token_expire_minutes: yaml.auth.token_expire_minutes,
            username_max_length: yaml.validation.username_max_length,
            id_max_length: yaml.validation.id_max_length,
            max_image_size_bytes,
            safe_name_re,
            safe_id_re,
        })
    }

    /// Prefer disk `config.yaml` at the project root if it exists; fall back
    /// to the version embedded at compile time so the binary stays portable.
    fn read_yaml_or_embedded(project_root: &Path) -> anyhow::Result<String> {
        let yaml_path = project_root.join("config.yaml");
        match std::fs::read_to_string(&yaml_path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(EMBEDDED_CONFIG_YAML.to_string())
            }
            Err(e) => Err(anyhow::anyhow!(
                "failed to read {}: {e}",
                yaml_path.display()
            )),
        }
    }

    /// Where to look for an on-disk `config.yaml` and what to anchor relative
    /// `DATABASE_PATH` values to. Honors `SEAL_PROJECT_ROOT`; otherwise falls
    /// back to the process's current working directory so a deployed binary
    /// picks up files next to wherever it's invoked from.
    fn project_root() -> PathBuf {
        if let Ok(dir) = std::env::var("SEAL_PROJECT_ROOT") {
            return PathBuf::from(dir);
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate process-global env state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn project_root_honors_env_override_and_falls_back_to_cwd() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("SEAL_PROJECT_ROOT").ok();

        std::env::set_var("SEAL_PROJECT_ROOT", "/tmp/seal-test-root-xyz");
        assert_eq!(
            Config::project_root(),
            PathBuf::from("/tmp/seal-test-root-xyz"),
            "SEAL_PROJECT_ROOT should take precedence"
        );

        std::env::remove_var("SEAL_PROJECT_ROOT");
        assert_eq!(
            Config::project_root(),
            std::env::current_dir().expect("cwd"),
            "without SEAL_PROJECT_ROOT, project_root should be CWD"
        );

        match prev {
            Some(v) => std::env::set_var("SEAL_PROJECT_ROOT", v),
            None => std::env::remove_var("SEAL_PROJECT_ROOT"),
        }
    }

    #[test]
    fn detects_object_store_uris() {
        for uri in [
            "s3://bucket/chat.lance",
            "s3a://bucket/chat.lance",
            "gs://bucket/chat.lance",
            "gcs://bucket/chat.lance",
            "az://container/chat.lance",
            "azure://container/chat.lance",
            "abfs://container/chat.lance",
            "abfss://container/chat.lance",
        ] {
            assert!(is_object_store_uri(uri), "{uri} should be a remote URI");
        }

        // S3 with DynamoDB commit locking is still object storage.
        assert!(is_object_store_uri("s3+ddb://bucket/chat.lance"));

        for path in ["data/chat.lance", "/abs/path/chat.lance", "chat.lance"] {
            assert!(!is_object_store_uri(path), "{path} should be local");
        }
    }

    #[test]
    fn redact_location_strips_userinfo_but_keeps_plain_locations() {
        // No credentials to hide: returned unchanged.
        assert_eq!(
            redact_location("s3://bucket/chat.lance"),
            "s3://bucket/chat.lance"
        );
        assert_eq!(redact_location("data/chat.lance"), "data/chat.lance");
        // Any embedded userinfo is masked so it can't leak into logs.
        assert_eq!(
            redact_location("s3://key:secret@bucket/chat.lance"),
            "s3://***@bucket/chat.lance"
        );
    }

    #[test]
    fn validate_storage_rejects_options_on_a_local_path() {
        let mut opts = HashMap::new();
        opts.insert("aws_region".to_string(), "us-east-1".to_string());
        // Options on a local path are a misconfiguration: they'd be ignored.
        assert!(validate_storage_config("data/chat.lance", &opts).is_err());
        // Same options on a remote URI are fine.
        assert!(validate_storage_config("s3://bucket/chat.lance", &opts).is_ok());
    }

    #[test]
    fn validate_storage_allows_local_path_without_options() {
        assert!(validate_storage_config("data/chat.lance", &HashMap::new()).is_ok());
    }

    #[test]
    fn remote_database_path_is_stored_verbatim() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_db = std::env::var("DATABASE_PATH").ok();
        let prev_root = std::env::var("SEAL_PROJECT_ROOT").ok();

        // A project root that would corrupt the URI if it were joined.
        std::env::set_var("SEAL_PROJECT_ROOT", "/tmp/seal-test-root-xyz");
        std::env::set_var("DATABASE_PATH", "s3://my-bucket/chat.lance");

        let cfg = Config::load().expect("load config");
        assert_eq!(
            cfg.database_path,
            PathBuf::from("s3://my-bucket/chat.lance"),
            "remote URI must be passed through without project-root joining"
        );

        match prev_db {
            Some(v) => std::env::set_var("DATABASE_PATH", v),
            None => std::env::remove_var("DATABASE_PATH"),
        }
        match prev_root {
            Some(v) => std::env::set_var("SEAL_PROJECT_ROOT", v),
            None => std::env::remove_var("SEAL_PROJECT_ROOT"),
        }
    }

    /// All process-global env vars that `Config::load` reads. Snapshotted and
    /// restored around each load() test so they don't leak between tests.
    const LOAD_ENV_KEYS: &[&str] = &[
        "SEAL_PROJECT_ROOT",
        "APP_TITLE",
        "APP_HOST",
        "APP_PORT",
        "DATABASE_PATH",
        "JWT_SECRET",
        "AUTH_JWT_ALGORITHM",
        "AUTH_TOKEN_EXPIRE_MINUTES",
    ];

    fn snapshot(keys: &[&str]) -> Vec<(String, Option<String>)> {
        keys.iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect()
    }

    fn restore(saved: &[(String, Option<String>)]) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn default_image_size_mb_is_five() {
        assert_eq!(default_image_size_mb(), 5);
    }

    #[test]
    fn load_uses_embedded_defaults_with_isolated_root() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot(LOAD_ENV_KEYS);

        // Point at an empty temp dir: no config.yaml (forces the embedded
        // fallback) and no .env (so dotenvy can't pull in real values).
        let root = tempfile::tempdir().expect("tempdir");
        for k in LOAD_ENV_KEYS {
            std::env::remove_var(k);
        }
        std::env::set_var("SEAL_PROJECT_ROOT", root.path());

        let cfg = Config::load().expect("load with embedded config");
        assert_eq!(cfg.app_title, "Seal");
        assert_eq!(cfg.app_port, 8000);
        assert_eq!(cfg.token_expire_minutes, 1440);
        assert_eq!(cfg.max_image_size_bytes, 5 * 1024 * 1024);
        // JWT_SECRET unset -> the documented dev default.
        assert_eq!(cfg.jwt_secret, "change-me");
        // Relative database path is anchored to the project root.
        assert_eq!(cfg.database_path, root.path().join("data/chat.lance"));
        // No object-store options for a plain local path.
        assert!(cfg.storage_options.is_empty());

        restore(&saved);
    }

    #[test]
    fn load_applies_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot(LOAD_ENV_KEYS);

        let root = tempfile::tempdir().expect("tempdir");
        for k in LOAD_ENV_KEYS {
            std::env::remove_var(k);
        }
        std::env::set_var("SEAL_PROJECT_ROOT", root.path());
        std::env::set_var("APP_TITLE", "Custom Title");
        std::env::set_var("APP_PORT", "9999");
        std::env::set_var("DATABASE_PATH", "/absolute/db.lance");
        std::env::set_var("JWT_SECRET", "s3cr3t");
        std::env::set_var("AUTH_JWT_ALGORITHM", "HS512");
        std::env::set_var("AUTH_TOKEN_EXPIRE_MINUTES", "30");

        let cfg = Config::load().expect("load with overrides");
        assert_eq!(cfg.app_title, "Custom Title");
        assert_eq!(cfg.app_port, 9999);
        assert_eq!(cfg.jwt_secret, "s3cr3t");
        assert_eq!(cfg.jwt_algorithm, "HS512");
        assert_eq!(cfg.token_expire_minutes, 30);
        // An absolute DATABASE_PATH is used verbatim, not joined to the root.
        assert_eq!(cfg.database_path, PathBuf::from("/absolute/db.lance"));

        restore(&saved);
    }

    #[test]
    fn load_rejects_unparseable_port() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot(LOAD_ENV_KEYS);

        let root = tempfile::tempdir().expect("tempdir");
        for k in LOAD_ENV_KEYS {
            std::env::remove_var(k);
        }
        std::env::set_var("SEAL_PROJECT_ROOT", root.path());
        std::env::set_var("APP_PORT", "not-a-number");

        assert!(
            Config::load().is_err(),
            "an unparseable APP_PORT should fail load()"
        );

        restore(&saved);
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_or_parse<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(val) => val
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("env var {key}={val:?} not parseable: {e}")),
        Err(_) => Ok(default),
    }
}
