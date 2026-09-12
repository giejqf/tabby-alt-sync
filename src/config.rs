use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use thiserror::Error;

pub const DEFAULT_BIND: &str = "127.0.0.1:9600";
pub const DEFAULT_DB: &str = "./tabby-alt-sync.db";
pub const DEFAULT_USERNAME: &str = "tabby";
pub const DEFAULT_MAX_BODY_BYTES: u64 = 8_388_608;
pub const DEFAULT_LOG: &str = "info";

/// Upstream generates tokens with `secrets.token_hex(64)` (see tabby-web
/// `backend/tabby/app/models.py`), i.e. 64 random bytes rendered as hex.
pub const TOKEN_RANDOM_BYTES: usize = 64;

/// Anything shorter is trivially guessable; upstream tokens are 128 hex chars.
pub const MIN_TOKEN_LEN: usize = 16;

#[derive(Parser, Debug)]
#[command(
    name = "tabby-alt-sync",
    about = "Minimal single-user config sync host for Tabby",
    version
)]
pub struct Cli {
    /// Static bearer token. Exactly one of --token / --token-file is required.
    #[arg(long, env = "TABBY_ALT_SYNC_TOKEN")]
    pub token: Option<String>,

    /// Read the token from a file (trailing newline is trimmed).
    #[arg(long, env = "TABBY_ALT_SYNC_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,

    /// Address to bind. Loopback by default: expose publicly only behind TLS.
    #[arg(long, env = "TABBY_ALT_SYNC_BIND", default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

    /// SQLite database file. Created on first run.
    #[arg(long, env = "TABBY_ALT_SYNC_DB", default_value = DEFAULT_DB)]
    pub db: PathBuf,

    /// Cosmetic; reported as `username` by GET /api/1/user.
    #[arg(long, env = "TABBY_ALT_SYNC_USERNAME", default_value = DEFAULT_USERNAME)]
    pub username: String,

    /// Request body size limit in bytes.
    #[arg(
        long,
        env = "TABBY_ALT_SYNC_MAX_BODY_BYTES",
        default_value_t = DEFAULT_MAX_BODY_BYTES
    )]
    pub max_body_bytes: u64,

    /// PEM certificate. Requires --tls-key.
    #[arg(long, env = "TABBY_ALT_SYNC_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,

    /// PEM private key. Requires --tls-cert.
    #[arg(long, env = "TABBY_ALT_SYNC_TLS_KEY")]
    pub tls_key: Option<PathBuf>,

    /// tracing_subscriber env filter, e.g. `info`, `tabby_alt_sync=debug`.
    #[arg(long, env = "TABBY_ALT_SYNC_LOG", default_value = DEFAULT_LOG)]
    pub log: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print a new random token and exit.
    GenToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsSettings {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub token: String,
    pub bind: SocketAddr,
    pub db: PathBuf,
    pub username: String,
    pub max_body_bytes: usize,
    pub tls: Option<TlsSettings>,
    pub log: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(
        "no token configured: set TABBY_ALT_SYNC_TOKEN (--token) or TABBY_ALT_SYNC_TOKEN_FILE (--token-file)"
    )]
    MissingToken,

    #[error("both TABBY_ALT_SYNC_TOKEN and TABBY_ALT_SYNC_TOKEN_FILE are set; use exactly one")]
    BothTokenSources,

    #[error("token is empty")]
    EmptyToken,

    #[error("token is shorter than {MIN_TOKEN_LEN} characters")]
    TokenTooShort,

    #[error("token file {path:?} could not be read: {source}")]
    TokenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("TABBY_ALT_SYNC_TLS_CERT/TLS_KEY: both must be set, or neither")]
    TlsIncomplete,

    #[error("TABBY_ALT_SYNC_MAX_BODY_BYTES must be greater than zero")]
    InvalidBodyLimit,

    #[error("TABBY_ALT_SYNC_MAX_BODY_BYTES is too large for this platform")]
    BodyLimitTooLarge,
}

/// Raw, unvalidated input. Kept separate from [`Settings`] so the validation
/// rules can be tested without going through clap and the process environment.
#[derive(Debug, Clone, Copy)]
pub struct SettingsInput<'a> {
    pub token: Option<&'a str>,
    pub token_file: Option<&'a Path>,
    pub bind: SocketAddr,
    pub db: &'a Path,
    pub username: &'a str,
    pub max_body_bytes: u64,
    pub tls_cert: Option<&'a Path>,
    pub tls_key: Option<&'a Path>,
    pub log: &'a str,
}

impl Settings {
    pub fn from_cli(cli: &Cli) -> Result<Self, ConfigError> {
        Self::build(SettingsInput {
            token: cli.token.as_deref(),
            token_file: cli.token_file.as_deref(),
            bind: cli.bind,
            db: &cli.db,
            username: &cli.username,
            max_body_bytes: cli.max_body_bytes,
            tls_cert: cli.tls_cert.as_deref(),
            tls_key: cli.tls_key.as_deref(),
            log: &cli.log,
        })
    }

    pub fn build(input: SettingsInput<'_>) -> Result<Self, ConfigError> {
        let token = Self::resolve_token(input.token, input.token_file)?;

        let tls = match (input.tls_cert, input.tls_key) {
            (Some(cert), Some(key)) => Some(TlsSettings {
                cert: cert.to_path_buf(),
                key: key.to_path_buf(),
            }),
            (None, None) => None,
            _ => return Err(ConfigError::TlsIncomplete),
        };

        if input.max_body_bytes == 0 {
            return Err(ConfigError::InvalidBodyLimit);
        }
        let max_body_bytes =
            usize::try_from(input.max_body_bytes).map_err(|_| ConfigError::BodyLimitTooLarge)?;

        Ok(Self {
            token,
            bind: input.bind,
            db: input.db.to_path_buf(),
            username: input.username.to_owned(),
            max_body_bytes,
            tls,
            log: input.log.to_owned(),
        })
    }

    fn resolve_token(
        token: Option<&str>,
        token_file: Option<&Path>,
    ) -> Result<String, ConfigError> {
        let raw = match (token, token_file) {
            (Some(_), Some(_)) => return Err(ConfigError::BothTokenSources),
            (Some(t), None) => t.to_owned(),
            (None, Some(path)) => std::fs::read_to_string(path)
                .map_err(|source| ConfigError::TokenFile {
                    path: path.to_path_buf(),
                    source,
                })?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
            (None, None) => return Err(ConfigError::MissingToken),
        };

        if raw.is_empty() {
            return Err(ConfigError::EmptyToken);
        }
        if raw.chars().count() < MIN_TOKEN_LEN {
            return Err(ConfigError::TokenTooShort);
        }
        Ok(raw)
    }
}

/// 64 random bytes as hex, matching upstream's `secrets.token_hex(64)`.
pub fn generate_token() -> String {
    use rand::Rng as _;

    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Write as _;

    const LONG_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn input<'a>() -> SettingsInput<'a> {
        SettingsInput {
            token: Some(LONG_TOKEN),
            token_file: None,
            bind: DEFAULT_BIND.parse().expect("default bind address"),
            db: Path::new("./tabby-alt-sync.db"),
            username: DEFAULT_USERNAME,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            tls_cert: None,
            tls_key: None,
            log: DEFAULT_LOG,
        }
    }

    #[test]
    fn defaults_are_applied() {
        let settings = Settings::build(input()).expect("valid settings");
        assert_eq!(settings.token, LONG_TOKEN);
        assert_eq!(settings.bind.to_string(), DEFAULT_BIND);
        assert_eq!(settings.username, DEFAULT_USERNAME);
        assert_eq!(settings.max_body_bytes, DEFAULT_MAX_BODY_BYTES as usize);
        assert_eq!(settings.tls, None);
        assert_eq!(settings.log, DEFAULT_LOG);
    }

    #[test]
    fn a_token_is_required() {
        let mut missing = input();
        missing.token = None;
        assert!(matches!(
            Settings::build(missing),
            Err(ConfigError::MissingToken)
        ));

        let mut empty = input();
        empty.token = Some("");
        assert!(matches!(
            Settings::build(empty),
            Err(ConfigError::EmptyToken)
        ));
    }

    #[test]
    fn short_tokens_are_refused() {
        let mut short = input();
        short.token = Some("sixteen");
        assert_eq!(
            Settings::build(short).err().map(|e| e.to_string()),
            Some(format!("token is shorter than {MIN_TOKEN_LEN} characters"))
        );
    }

    #[test]
    fn token_and_token_file_are_exclusive() {
        let mut both = input();
        both.token_file = Some(Path::new("/tmp/some-token"));
        assert!(matches!(
            Settings::build(both),
            Err(ConfigError::BothTokenSources)
        ));
    }

    #[test]
    fn token_file_is_read_and_newline_trimmed() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        writeln!(file, "{LONG_TOKEN}\r\n").expect("write token file");

        let mut from_file = input();
        from_file.token = None;
        from_file.token_file = Some(file.path());
        let settings = Settings::build(from_file).expect("settings from file");
        assert_eq!(settings.token, LONG_TOKEN);
    }

    #[test]
    fn missing_token_file_is_an_error_not_a_panic() {
        let mut from_file = input();
        from_file.token = None;
        from_file.token_file = Some(Path::new("/nonexistent/definitely/not/here"));
        assert!(matches!(
            Settings::build(from_file),
            Err(ConfigError::TokenFile { .. })
        ));
    }

    #[test]
    fn tls_requires_both_paths() {
        let mut only_cert = input();
        only_cert.tls_cert = Some(Path::new("/tmp/cert.pem"));
        assert!(matches!(
            Settings::build(only_cert),
            Err(ConfigError::TlsIncomplete)
        ));

        let mut only_key = input();
        only_key.tls_key = Some(Path::new("/tmp/key.pem"));
        assert!(matches!(
            Settings::build(only_key),
            Err(ConfigError::TlsIncomplete)
        ));

        let mut both = input();
        both.tls_cert = Some(Path::new("/tmp/cert.pem"));
        both.tls_key = Some(Path::new("/tmp/key.pem"));
        let settings = Settings::build(both).expect("tls settings");
        assert_eq!(
            settings.tls.map(|tls| (tls.cert, tls.key)),
            Some((
                PathBuf::from("/tmp/cert.pem"),
                PathBuf::from("/tmp/key.pem")
            ))
        );
    }

    #[test]
    fn a_zero_body_limit_is_refused() {
        let mut zero = input();
        zero.max_body_bytes = 0;
        assert!(matches!(
            Settings::build(zero),
            Err(ConfigError::InvalidBodyLimit)
        ));
    }

    #[test]
    #[cfg(target_pointer_width = "32")]
    fn a_body_limit_larger_than_usize_is_refused() {
        let mut huge = input();
        huge.max_body_bytes = u64::from(u32::MAX) + 1;
        assert!(matches!(
            Settings::build(huge),
            Err(ConfigError::BodyLimitTooLarge)
        ));
    }

    #[test]
    fn generated_tokens_match_upstream_length() {
        let first = generate_token();
        let second = generate_token();
        assert_eq!(first.len(), TOKEN_RANDOM_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn cli_flags_are_named_after_the_env_vars() {
        let cli = Cli::try_parse_from([
            "tabby-alt-sync",
            "--token",
            LONG_TOKEN,
            "--bind",
            "0.0.0.0:9000",
            "--db",
            "/tmp/x.db",
            "--username",
            "ops",
            "--max-body-bytes",
            "1024",
            "--tls-cert",
            "/tmp/c.pem",
            "--tls-key",
            "/tmp/k.pem",
            "--log",
            "debug",
        ])
        .expect("flags parse");

        let settings = Settings::from_cli(&cli).expect("settings");
        assert_eq!(settings.bind.to_string(), "0.0.0.0:9000");
        assert_eq!(settings.db, PathBuf::from("/tmp/x.db"));
        assert_eq!(settings.username, "ops");
        assert_eq!(settings.max_body_bytes, 1024);
        assert_eq!(settings.tls.expect("tls").cert, PathBuf::from("/tmp/c.pem"));
        assert_eq!(settings.log, "debug");
    }
}
