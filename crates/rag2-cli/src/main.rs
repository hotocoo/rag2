//! `rag2`: run the service, manage configuration offline, generate keys, and talk to a running
//! server (query, filesystem ingest).

mod client_cmds;
mod config_cmds;
mod init;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rag2_config::{ConfigManager, FsConfigStore, Profile};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "rag2", version, about = "RAG2 adaptive retrieval platform")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

/// Where profile versions live and which profile/environment to run.
#[derive(clap::Args, Clone)]
pub struct ProfileArgs {
    /// Directory holding versioned profiles.
    #[arg(long, env = "RAG2_CONFIG_DIR", default_value = "./rag2-config")]
    pub config_dir: PathBuf,
    /// Profile name.
    #[arg(long, env = "RAG2_PROFILE", default_value = "main")]
    pub profile: String,
    /// Environment overlay to apply (a key of the profile's `environments`).
    #[arg(long, env = "RAG2_ENV")]
    pub env: Option<String>,
}

impl ProfileArgs {
    pub fn manager(&self) -> Result<Arc<ConfigManager>> {
        let store = Arc::new(FsConfigStore::open(&self.config_dir)?);
        let name = self.profile.clone();
        Ok(Arc::new(ConfigManager::open(
            store,
            &self.profile,
            self.env.clone(),
            move || Profile::minimal(&name),
            rag2_config::defaults::CONFIG_HISTORY,
        )?))
    }
}

/// How to reach a running server.
#[derive(clap::Args, Clone)]
pub struct Remote {
    #[arg(long, env = "RAG2_URL", default_value = "http://127.0.0.1:8080")]
    pub url: String,
    /// API key or OIDC access token.
    #[arg(long, env = "RAG2_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the service.
    Serve {
        #[command(flatten)]
        profile: ProfileArgs,
        /// Profile file (YAML/JSON) to apply at startup.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Re-apply `--file` whenever it changes (hot reload).
        #[arg(long, requires = "file")]
        watch: bool,
        #[arg(long, value_enum, default_value = "text", env = "RAG2_LOG_FORMAT")]
        log_format: LogFormat,
    },
    /// Write a starter profile and generate credentials.
    Init {
        /// Directory to write into.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
    /// Validate, diff, apply and roll back profiles (offline, on the config directory).
    #[command(subcommand)]
    Config(config_cmds::ConfigCmd),
    /// Generate secrets.
    #[command(subcommand)]
    Key(KeyCmd),
    /// Print the OpenAPI document.
    Openapi,
    /// Query a running server.
    Query {
        #[command(flatten)]
        remote: Remote,
        #[arg(long, short)]
        tenant: String,
        query: String,
        #[arg(long)]
        top_k: Option<usize>,
        #[arg(long)]
        pipeline: Option<String>,
        /// Print the full JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Ingest files from a directory into a running server (filesystem connector).
    Ingest {
        #[command(flatten)]
        remote: Remote,
        #[arg(long, short)]
        tenant: String,
        /// File or directory (recursive).
        path: PathBuf,
        /// File extensions to include.
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "md,txt,rst,adoc,csv,json,html"
        )]
        ext: Vec<String>,
        /// Documents per request.
        #[arg(long, default_value_t = 64)]
        batch: usize,
    },
    /// Bridge MCP over stdio (one JSON-RPC message per line) to a server's `/mcp` endpoint, for
    /// desktop assistants that launch tools as subprocesses.
    Mcp {
        #[command(flatten)]
        remote: Remote,
    },
    /// Check a running server's readiness and list tenants.
    Status {
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// A random API key (base64url, 32 bytes of entropy).
    Api,
    /// A storage encryption key (64 hex characters).
    Encryption,
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("system RNG: {e}"))?;
    Ok(b)
}

pub fn api_key() -> Result<String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    Ok(random_bytes::<32>()?
        .iter()
        .map(|b| ALPHABET[(*b & 63) as usize] as char)
        .collect::<String>()
        + &random_bytes::<8>()?
            .iter()
            .map(|b| ALPHABET[(*b & 63) as usize] as char)
            .collect::<String>())
}

pub fn encryption_key() -> Result<String> {
    Ok(random_bytes::<32>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn init_tracing(f: LogFormat) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_env("RAG2_LOG").unwrap_or_else(|_| "info".into());
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    match f {
        LogFormat::Json => b.json().init(),
        LogFormat::Text => b.init(),
    }
}

async fn serve(
    profile: ProfileArgs,
    file: Option<PathBuf>,
    watch: bool,
    log: LogFormat,
) -> Result<()> {
    init_tracing(log);
    let metrics = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("installing metrics recorder")?;
    let manager = profile.manager()?;
    if let Some(f) = &file {
        let v = rag2_config::watch::reload_file(&manager, f)
            .with_context(|| format!("applying {}", f.display()))?;
        tracing::info!(file = %f.display(), version = v, "profile applied");
    }
    let engine =
        rag2_engine::Engine::open(manager.clone(), rag2::factories()).context("opening engine")?;
    let _guard = match (watch, &file) {
        (true, Some(f)) => Some(rag2_config::watch::watch_file(
            manager.clone(),
            f.clone(),
            std::time::Duration::from_millis(WATCH_DEBOUNCE_MS),
        )?),
        _ => None,
    };
    rag2_server::serve(engine, Some(metrics)).await?;
    Ok(())
}

/// Quiet period after the last file event before a hot reload.
const WATCH_DEBOUNCE_MS: u64 = 300;

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve {
            profile,
            file,
            watch,
            log_format,
        } => serve(profile, file, watch, log_format).await,
        Cmd::Init { dir, force } => init::run(&dir, force),
        Cmd::Config(c) => config_cmds::run(c),
        Cmd::Key(KeyCmd::Api) => {
            println!("{}", api_key()?);
            Ok(())
        }
        Cmd::Key(KeyCmd::Encryption) => {
            println!("{}", encryption_key()?);
            Ok(())
        }
        Cmd::Openapi => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rag2_server::openapi::spec(env!(
                    "CARGO_PKG_VERSION"
                )))?
            );
            Ok(())
        }
        Cmd::Query {
            remote,
            tenant,
            query,
            top_k,
            pipeline,
            json,
        } => client_cmds::query(&remote, &tenant, query, top_k, pipeline, json).await,
        Cmd::Ingest {
            remote,
            tenant,
            path,
            ext,
            batch,
        } => client_cmds::ingest(&remote, &tenant, &path, &ext, batch).await,
        Cmd::Status { remote } => client_cmds::status(&remote).await,
        Cmd::Mcp { remote } => client_cmds::mcp_stdio(&remote).await,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_keys_have_the_expected_shape() {
        let k = super::api_key().expect("key");
        assert!(k.len() >= rag2_server::auth::MIN_API_KEY_LEN);
        assert_ne!(k, super::api_key().expect("key"));
        let e = super::encryption_key().expect("key");
        assert_eq!(e.len(), 64);
        assert!(e.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
