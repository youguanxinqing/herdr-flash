use crate::herdr::HerdrAdapter;
use crate::model::PaneId;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "herdr-flash",
    version,
    about = "Inline hint picker for Herdr panes"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Action entrypoint: apply an argv-backed temporary picker layout.
    ///
    /// Hidden: the published plugin exposes only the flash action; the pattern-hint pickers
    /// stay functional for compatibility but are not part of the advertised surface.
    #[command(hide = true)]
    Open {
        /// Override the pane to act on. Defaults to Herdr invocation context.
        #[arg(long)]
        target_pane: Option<String>,
    },

    /// Action entrypoint: open a selected visible URL in the default browser.
    #[command(hide = true)]
    OpenUrl {
        /// Override the pane to act on. Defaults to Herdr invocation context.
        #[arg(long)]
        target_pane: Option<String>,
    },

    /// Action entrypoint: incremental-search picker over visible text.
    Flash {
        /// Override the pane to act on. Defaults to Herdr invocation context.
        #[arg(long)]
        target_pane: Option<String>,
    },

    /// Picker entrypoint: run inside the temporary layout-tab target pane.
    Pick {
        /// Temp JSON snapshot path produced by `open`.
        #[arg(long)]
        snapshot: PathBuf,
        /// One-shot launch barrier released after the painted tab receives focus.
        #[arg(long)]
        ready: PathBuf,
        /// One-shot marker published after the hidden tab paints its first frame.
        #[arg(long)]
        painted: PathBuf,
    },

    /// Internal shell-free placeholder for non-picker panes.
    #[command(hide = true)]
    Idle,
}

pub fn run() -> Result<()> {
    run_with(Cli::parse())
}

pub fn run_with(cli: Cli) -> Result<()> {
    let adapter = HerdrAdapter::from_env();

    match cli.command {
        Command::Open { target_pane } => {
            let target = resolve_target(&adapter, target_pane)?;
            adapter.open_copy_picker(&target)?;
        }
        Command::OpenUrl { target_pane } => {
            let target = resolve_target(&adapter, target_pane)?;
            adapter.open_url_picker(&target)?;
        }
        Command::Flash { target_pane } => {
            let target = resolve_target(&adapter, target_pane)?;
            adapter.open_flash_picker(&target)?;
        }
        Command::Pick {
            snapshot,
            ready,
            painted,
        } => {
            adapter.run_picker_from_snapshot(&snapshot, &ready, &painted)?;
        }
        Command::Idle => crate::herdr::run_idle()?,
    }

    Ok(())
}

fn resolve_target(adapter: &HerdrAdapter, target_pane: Option<String>) -> Result<PaneId> {
    target_pane
        .map(PaneId::new)
        .or_else(|| adapter.target_pane_from_context())
        .context("could not determine target pane from --target-pane, HERDR_PANE_ID, HERDR_ACTIVE_PANE_ID, or Herdr context")
}
