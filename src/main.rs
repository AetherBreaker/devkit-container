//! `devkit-container` — the image-side helper: build-time pyproject queries and the
//! container entrypoint. No Python is needed for either.

// Off Unix only the query subcommands exist, so the entrypoint's helpers would be flagged
// as dead code there; the attribute keeps the Windows build warning-free.
#[cfg_attr(not(unix), allow(dead_code))]
mod mounts;
#[cfg_attr(not(unix), allow(dead_code))]
mod prepare;
mod pyproject;
#[cfg(unix)]
mod run;

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
    #[cfg(unix)]
    Command::Run {
      pyproject,
      app_root,
      mountinfo,
    } => run::run(&run::RunArgs {
      pyproject,
      app_root,
      mountinfo,
    }),
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
