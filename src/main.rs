//! `devkit-container` — the image-side helper: build-time pyproject queries and the
//! container entrypoint. No Python is needed for either.

// Off Unix only the query subcommands exist, so the entrypoint's helpers would be flagged
// as dead code there; the attribute keeps the Windows build warning-free.
#[cfg_attr(not(unix), allow(dead_code))] // the fetch, the cache and the supervisor (Linux) are its callers
mod bundle;
#[cfg_attr(not(unix), allow(dead_code))]
mod cache;
#[cfg(unix)]
mod consent;
#[cfg(unix)]
mod fetch;
#[cfg_attr(not(unix), allow(dead_code))]
mod health;
mod healthcheck;
#[cfg_attr(not(unix), allow(dead_code))] // `write` and the tunnel file serve the supervisor (Unix)
mod heartbeat;
#[cfg_attr(not(unix), allow(dead_code))]
mod logfile;
#[cfg_attr(not(unix), allow(dead_code))]
mod mounts;
#[cfg_attr(not(unix), allow(dead_code))]
mod ping;
#[cfg_attr(not(unix), allow(dead_code))]
mod prepare;
mod pyproject;
#[cfg(unix)]
mod run;
#[cfg_attr(not(unix), allow(dead_code))]
mod supervisor;
#[cfg_attr(not(unix), allow(dead_code))]
mod wireguard;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "devkit-container", version, about)]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
  /// Print `--extra app` when pyproject declares an `app` optional-dependency group.
  AppExtra {
    #[arg(long, default_value = "/app/pyproject.toml")]
    pyproject: PathBuf,
  },
  /// Print `project.readme` (nothing when unset), without a trailing newline.
  Readme {
    #[arg(long, default_value = "/app/pyproject.toml")]
    pyproject: PathBuf,
  },
  /// Exit 0 when every heartbeat file is fresh, else 1 with a reason per file on stderr.
  Healthcheck {
    /// A heartbeat file to check; repeatable. Default: the app's, under --app-root.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Seconds a timestamp may be old before it counts as stale.
    #[arg(long, default_value_t = heartbeat::DEFAULT_MAX_AGE_SECS)]
    max_age: u64,
    #[arg(long, default_value = "/app")]
    app_root: PathBuf,
  },
  /// The entrypoint: check mounts, prepare the persisted dirs, drop to nonroot, exec.
  Run {
    #[arg(long, default_value = "/app/pyproject.toml")]
    pyproject: PathBuf,
    #[arg(long, default_value = "/app")]
    app_root: PathBuf,
    #[arg(long, default_value = "/proc/self/mountinfo")]
    mountinfo: PathBuf,
  },
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  let result = match cli.command {
    Command::AppExtra { pyproject } => pyproject::load(&pyproject).map(|d| {
      if pyproject::app_extra(&d) {
        // `print!` (no newline): the Dockerfile splices this into a `uv sync` line.
        print!("--extra app");
      }
    }),
    Command::Readme { pyproject } => pyproject::load(&pyproject).map(|d| print!("{}", pyproject::readme(&d).unwrap_or_default())),
    Command::Healthcheck { files, max_age, app_root } => {
      let files = if files.is_empty() {
        vec![heartbeat::logs_dir(&app_root).join(heartbeat::APP_FILE)]
      } else {
        files
      };
      return ExitCode::from(healthcheck::run(&files, max_age));
    }
    #[cfg(unix)]
    Command::Run {
      pyproject,
      app_root,
      mountinfo,
    } => {
      return match run::run(&run::RunArgs {
        pyproject,
        app_root,
        mountinfo,
      }) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
          eprintln!("error: {e:#}");
          ExitCode::from(1)
        }
      };
    }
    #[cfg(not(unix))]
    Command::Run { .. } => Err(anyhow::anyhow!("unsupported platform: `run` is the Linux container entrypoint")),
  };
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("error: {e:#}");
      ExitCode::from(1)
    }
  }
}
