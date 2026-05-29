mod command;
mod r#const;
mod helper;
mod r#struct;
mod verify;

use crate::command::install::install_common;
use crate::command::outdated::outdated;
use crate::helper::run_status;
use crate::r#struct::{Cli, ContainerdConfig, KubernetesConfig};
use anyhow::Result;
use clap::Parser;
use command::generate::generate;
use r#struct::Command;
use std::{fs, io, path::Path, process::Command as ProcCommand};

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    let assume_yes = cli.assume_yes;

    match cli.command {
        Command::Generate => generate()?,
        Command::Install { default } => {
            install_common(assume_yes, default)?;
        }
        Command::Outdated => outdated()?,
    }

    Ok(())
}
