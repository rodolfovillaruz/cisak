use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command as ProcCommand,
};

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "cisak",
    version,
    about = "Container Installation - Swiss Army Knife"
)]
struct Cli {
    /// Skip all y/N confirmations (assume yes)
    #[arg(short = 'y', long = "assume-yes", global = true)]
    assume_yes: bool,

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

    /// Generate a config.toml file in the current directory
    Generate,

    /// Download, verify, and install the runc binary defined in config.toml
    Run,
}

// ── Constants ────────────────────────────────────────────────────────────────

const RUNC_VERSION: &str = "v1.4.2";
const CONFIG_FILENAME: &str = "config.toml";
const RUNC_INSTALL_PATH: &str = "/usr/local/sbin/runc";
const RUNC_URL_BASE: &str = "https://github.com/opencontainers/runc/releases/download";

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Config {
    runtime: RuntimeConfig,
}

#[derive(Debug, Deserialize)]
struct RuntimeConfig {
    #[allow(dead_code)]
    name: String,
    version: String,
    binary: Option<String>,
}

fn load_config() -> Result<Config> {
    let raw = fs::read_to_string(CONFIG_FILENAME)
        .with_context(|| format!("failed to read `{CONFIG_FILENAME}`"))?;
    toml::from_str::<Config>(&raw).with_context(|| format!("failed to parse `{CONFIG_FILENAME}`"))
}

// ── Command confirmation ──────────────────────────────────────────────────────

/// Render a `Command` as a human-readable shell-like string.
fn fmt_cmd(cmd: &ProcCommand) -> String {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| {
            let s = a.to_string_lossy();
            // Wrap args that contain whitespace in quotes so the display is unambiguous.
            if s.contains(char::is_whitespace) {
                format!("\"{}\"", s)
            } else {
                s.into_owned()
            }
        })
        .collect();

    if args.is_empty() {
        prog
    } else {
        format!("{} {}", prog, args.join(" "))
    }
}

/// Print the exact command that is about to run, then ask the user to confirm.
///
/// * If `assume_yes` is `true` the interactive prompt is skipped and the
///   function returns `Ok(())` immediately.
/// * If the user types anything other than `y` / `yes` (case-insensitive)
///   the function returns `Err` and the caller should propagate it.
fn prompt(cmd: &ProcCommand, assume_yes: bool) -> Result<()> {
    println!("  $ {}", fmt_cmd(cmd));

    if assume_yes {
        println!("  (--assume-yes: auto-confirmed)");
        return Ok(());
    }

    print!("  Run this command? [y/N] ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read user input")?;

    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => anyhow::bail!("aborted by user"),
    }
}

/// Prompt, then spawn the command and return its `ExitStatus`.
fn run_status(cmd: &mut ProcCommand, assume_yes: bool) -> Result<std::process::ExitStatus> {
    prompt(cmd, assume_yes)?;
    cmd.status().context("failed to spawn process")
}

/// Prompt, then spawn the command and capture its output.
fn run_output(cmd: &mut ProcCommand, assume_yes: bool) -> Result<std::process::Output> {
    prompt(cmd, assume_yes)?;
    cmd.output().context("failed to spawn process")
}

// ── GPG helper ───────────────────────────────────────────────────────────────

/// Import the runc signing key into the local keyring if it is not already present.
fn ensure_gpg_key(key_id: &str, assume_yes: bool) -> Result<()> {
    println!("→ Checking for GPG key {key_id}…");

    let mut check = ProcCommand::new("gpg");
    check.args(["--list-keys", key_id]);

    let output =
        run_output(&mut check, assume_yes).context("failed to execute `gpg --list-keys`")?;

    if output.status.success() {
        println!("✓ GPG key already present");
        return Ok(());
    }

    println!("→ Importing GPG key {key_id}…");

    let mut import = ProcCommand::new("gpg");
    import.args(["--keyserver", "keyserver.ubuntu.com", "--recv-keys", key_id]);

    let status =
        run_status(&mut import, assume_yes).context("failed to execute `gpg --recv-keys`")?;

    if !status.success() {
        anyhow::bail!(
            "failed to import GPG key {key_id}.\n\
             Try manually: gpg --keyserver keyserver.ubuntu.com --recv-keys {key_id}"
        );
    }

    println!("✓ GPG key imported");
    Ok(())
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Pull and register a container image on the host.
fn install(image: &str, tag: Option<&str>, assume_yes: bool) -> Result<()> {
    let target = match tag {
        Some(t) => format!("{image}:{t}"),
        None => image.to_owned(),
    };

    println!("→ Installing: {target}");

    let mut cmd = ProcCommand::new("docker");
    cmd.args(["pull", &target]);

    let status = run_status(&mut cmd, assume_yes)
        .with_context(|| format!("failed to execute `docker pull` for `{target}`"))?;

    if !status.success() {
        anyhow::bail!("`docker pull {target}` exited with {status}");
    }

    println!("✓ {target} installed.");
    Ok(())
}

/// Write a `config.toml` scaffold to the current working directory.
///
/// No external programs are invoked here, so no confirmation is needed.
fn generate() -> Result<()> {
    let path = Path::new(CONFIG_FILENAME);

    if path.exists() {
        anyhow::bail!("`{CONFIG_FILENAME}` already exists in the current directory");
    }

    let content = build_config(RUNC_VERSION);

    fs::write(path, &content).with_context(|| format!("failed to write `{CONFIG_FILENAME}`"))?;

    println!("✓ Created {CONFIG_FILENAME}  (runc {RUNC_VERSION})");
    Ok(())
}

/// Return the rendered TOML configuration string.
fn build_config(runc_version: &str) -> String {
    format!(
        r#"# Generated by cisak

[runtime]
name    = "runc"
version = "{runc_version}"
binary  = "/usr/local/sbin/runc"

[container]
rootfs     = "./rootfs"
log_file   = "./container.log"
log_format = "json"

[namespaces]
pid     = true
network = true
ipc     = true
mount   = true
uts     = true

[resources]
memory_limit_mb = 512
cpu_shares      = 1024
"#
    )
}

/// Download runc + signature, verify, and install.
fn run(assume_yes: bool) -> Result<()> {
    let cfg = load_config()?;
    let version = &cfg.runtime.version;
    let install_path = cfg
        .runtime
        .binary
        .clone()
        .unwrap_or_else(|| RUNC_INSTALL_PATH.to_string());

    println!("→ Using runc version: {version}");

    ensure_gpg_key("C2428CD75720FACDCF76B6EA17DE5ECB75A1100E", assume_yes)?;

    let bin_url = format!("{RUNC_URL_BASE}/{version}/runc.amd64");
    let sig_url = format!("{RUNC_URL_BASE}/{version}/runc.amd64.asc");

    let tmp = tempfile::tempdir().context("failed to create temporary directory")?;
    let bin_path = tmp.path().join("runc.amd64");
    let sig_path = tmp.path().join("runc.amd64.asc");

    download(&bin_url, &bin_path, assume_yes)?;
    download(&sig_url, &sig_path, assume_yes)?;

    verify_signature(&bin_path, &sig_path, assume_yes)?;

    install_binary(&bin_path, Path::new(&install_path), assume_yes)?;

    println!("✓ runc {version} installed to {install_path}");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Download a URL to a local path using `curl`.
fn download(url: &str, dest: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Downloading {url}");

    let mut cmd = ProcCommand::new("curl");
    cmd.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--output",
    ])
    .arg(dest)
    .arg(url);

    let status = run_status(&mut cmd, assume_yes)
        .with_context(|| format!("failed to execute `curl` for {url}"))?;

    if !status.success() {
        anyhow::bail!("`curl` failed to download {url} (exit {status})");
    }

    Ok(())
}

/// Verify a detached GPG signature.
fn verify_signature(bin: &Path, sig: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying signature…");

    let mut cmd = ProcCommand::new("gpg");
    cmd.arg("--verify").arg(sig).arg(bin);

    let output = run_output(&mut cmd, assume_yes)
        .context("failed to execute `gpg --verify` (is gpg installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "signature verification FAILED for `{}` against `{}`:\n{}",
            bin.display(),
            sig.display(),
            stderr.trim()
        );
    }

    println!("✓ Signature verified");
    Ok(())
}

/// Copy the verified binary into place and make it executable.
///
/// Tries a direct `fs::copy` first; if that fails (e.g. the destination is
/// owned by root) it falls back to `sudo install`.
fn install_binary(src: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
    }

    if fs::copy(src, dest).is_err() {
        // Destination is likely in a root-owned directory; escalate with sudo.
        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["install", "-m", "0755"]).arg(src).arg(dest);

        let status = run_status(&mut cmd, assume_yes)
            .with_context(|| format!("failed to install binary to `{}`", dest.display()))?;

        if !status.success() {
            anyhow::bail!("`sudo install` failed (exit {status})");
        }
    } else {
        chmod_executable(dest)?;
    }

    Ok(())
}

#[cfg(unix)]
fn chmod_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .with_context(|| format!("failed to chmod `{}`", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn chmod_executable(_path: &Path) -> Result<()> {
    Ok(())
}

// Silence dead-code warnings for the PathBuf import on some targets.
#[allow(dead_code)]
fn _unused(_: PathBuf) {}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    let assume_yes = cli.assume_yes;

    match cli.command {
        Command::Install { image, tag } => install(&image, tag.as_deref(), assume_yes)?,
        Command::Generate => generate()?,
        Command::Run => run(assume_yes)?,
    }

    Ok(())
}
