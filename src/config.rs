use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

/// Default `config.yaml` baked into the binary at build time. Disk
/// `config.yaml` at the project root overrides it; env vars override either.
const EMBEDDED_CONFIG_YAML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config.yaml"));

#[derive(Debug, Clone)]
pub struct Config {
    pub app_title: String,
    pub app_host: String,
    pub app_port: u16,
    pub database_path: PathBuf,
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

        let database_path = PathBuf::from(env_or("DATABASE_PATH", &yaml.database.path));
        let database_path = if database_path.is_absolute() {
            database_path
        } else {
            project_root.join(database_path)
        };

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
