//! Offline configuration management on the config directory (no server needed).

use crate::ProfileArgs;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use rag2_config::{Severity, parse_profile_value, resolve, validate};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Validate a profile file; exits non-zero on errors.
    Validate {
        file: PathBuf,
        /// Validate with this environment overlay applied.
        #[arg(long)]
        env: Option<String>,
    },
    /// Show what applying a file would change.
    Diff {
        #[command(flatten)]
        profile: ProfileArgs,
        file: PathBuf,
    },
    /// Store and activate a profile file.
    Apply {
        #[command(flatten)]
        profile: ProfileArgs,
        file: PathBuf,
        #[arg(long, default_value = "")]
        comment: String,
    },
    /// Re-activate the previous version.
    Rollback {
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// List stored versions and activations.
    History {
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Export the active profile as YAML.
    Export {
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Print the profile JSON Schema (for editors and CI).
    Schema,
}

fn read(file: &Path) -> Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    Ok(parse_profile_value(&text)?)
}

fn author() -> String {
    std::env::var("USER")
        .map(|u| format!("cli:{u}"))
        .unwrap_or_else(|_| "cli".into())
}

pub fn run(cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::Validate { file, env } => {
            let p = resolve(&read(&file)?, env.as_deref())?;
            let issues = validate(&p);
            for i in &issues {
                let tag = if i.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                };
                println!("{tag}: {}: {}", i.path, i.message);
            }
            if issues.iter().any(|i| i.severity == Severity::Error) {
                bail!("{} is invalid", file.display());
            }
            println!("{} is valid", file.display());
        }
        ConfigCmd::Diff { profile, file } => {
            let preview = profile.manager()?.preview(&read(&file)?);
            println!("{}", serde_json::to_string_pretty(&preview)?);
            if !preview.valid {
                bail!("profile is invalid");
            }
        }
        ConfigCmd::Apply {
            profile,
            file,
            comment,
        } => {
            let a = profile
                .manager()?
                .apply(&read(&file)?, &author(), &comment)?;
            println!("active version {}", a.version);
        }
        ConfigCmd::Rollback { profile } => {
            let a = profile.manager()?.rollback(&author())?;
            println!("active version {}", a.version);
        }
        ConfigCmd::History { profile } => {
            let m = profile.manager()?;
            let active = m.current().version;
            for v in m.store().list(m.name())? {
                let mark = if v.version == active { "*" } else { " " };
                println!(
                    "{mark} v{:<5} {} {:<16} {}",
                    v.version, v.created_at_ms, v.author, v.comment
                );
            }
        }
        ConfigCmd::Export { profile } => {
            let m = profile.manager()?;
            print!("{}", serde_norway::to_string(m.current().profile.as_ref())?);
        }
        ConfigCmd::Schema => println!(
            "{}",
            serde_json::to_string_pretty(&rag2_config::json_schema())?
        ),
    }
    Ok(())
}
