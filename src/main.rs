use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "cisak",
    version,
    about = "Container Installation - Swiss Army Knife"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install a container image
    Install {
        /// Image name  (e.g. ubuntu:22.04)
        image: String,

        /// Optional tag override
        #[arg(short, long)]
        tag: Option<String>,
    },
}

// ── Functions ────────────────────────────────────────────────────────────────

/// Pull and register a container image on the host.
fn install(image: &str, tag: Option<&str>) -> Result<()> {
    let target = match tag {
        Some(t) => format!("{image}:{t}"),
        None => image.to_owned(),
    };

    println!("→ Installing: {target}");

    // TODO: shell out to `docker pull` / `podman pull` / containerd gRPC
    std::process::Command::new("docker")
        .args(["pull", &target])
        .status()
        .with_context(|| format!("failed to pull image `{target}`"))?;

    println!("✓ {target} installed.");
    Ok(())
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Install { image, tag } => {
            install(&image, tag.as_deref())?;
        }
    }

    Ok(())
}
