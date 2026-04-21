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

    /// Download, verify, and install runc + CNI plugins defined in config.toml
    Run,
}

// ── Constants ────────────────────────────────────────────────────────────────

const RUNC_VERSION: &str = "v1.4.2";
const CNI_VERSION: &str = "v1.9.1";

const CONFIG_FILENAME: &str = "config.toml";

const RUNC_INSTALL_PATH: &str = "/usr/local/sbin/runc";
const CNI_INSTALL_DIR: &str = "/opt/cni/bin";

const RUNC_URL_BASE: &str = "https://github.com/opencontainers/runc/releases/download";
const CNI_URL_BASE: &str = "https://github.com/containernetworking/plugins/releases/download";

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Config {
    runtime: RuntimeConfig,
    cni: Option<CniConfig>,
}

#[derive(Debug, Deserialize)]
struct RuntimeConfig {
    #[allow(dead_code)]
    name: String,
    version: String,
    binary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CniConfig {
    version: String,
    install_dir: Option<String>,
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
fn generate() -> Result<()> {
    let path = Path::new(CONFIG_FILENAME);

    if path.exists() {
        anyhow::bail!("`{CONFIG_FILENAME}` already exists in the current directory");
    }

    let content = build_config(RUNC_VERSION, CNI_VERSION);

    fs::write(path, &content).with_context(|| format!("failed to write `{CONFIG_FILENAME}`"))?;

    println!("✓ Created {CONFIG_FILENAME}  (runc {RUNC_VERSION}, CNI plugins {CNI_VERSION})");
    Ok(())
}

/// Return the rendered TOML configuration string.
fn build_config(runc_version: &str, cni_version: &str) -> String {
    format!(
        r#"# Generated by cisak

[runtime]
name    = "runc"
version = "{runc_version}"
binary  = "/usr/local/sbin/runc"

[cni]
version     = "{cni_version}"
install_dir = "/opt/cni/bin"

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

/// Download runc + GPG signature, verify, install; then optionally install CNI plugins.
fn run(assume_yes: bool) -> Result<()> {
    let cfg = load_config()?;

    // ── runc ─────────────────────────────────────────────────────────────────

    let runc_version = &cfg.runtime.version;
    let runc_install_path = cfg
        .runtime
        .binary
        .clone()
        .unwrap_or_else(|| RUNC_INSTALL_PATH.to_string());

    println!("→ Using runc version: {runc_version}");

    ensure_gpg_key("C2428CD75720FACDCF76B6EA17DE5ECB75A1100E", assume_yes)?;

    let bin_url = format!("{RUNC_URL_BASE}/{runc_version}/runc.amd64");
    let sig_url = format!("{RUNC_URL_BASE}/{runc_version}/runc.amd64.asc");

    let tmp = tempfile::tempdir().context("failed to create temporary directory")?;
    let bin_path = tmp.path().join("runc.amd64");
    let sig_path = tmp.path().join("runc.amd64.asc");

    download(&bin_url, &bin_path, assume_yes)?;
    download(&sig_url, &sig_path, assume_yes)?;

    verify_gpg_signature(&bin_path, &sig_path, assume_yes)?;
    install_binary(&bin_path, Path::new(&runc_install_path), assume_yes)?;

    println!("✓ runc {runc_version} installed to {runc_install_path}");

    // ── CNI plugins ───────────────────────────────────────────────────────────

    if let Some(cni_cfg) = &cfg.cni {
        println!();
        install_cni(cni_cfg, assume_yes)?;
    } else {
        println!("  (no [cni] section in config – skipping CNI install)");
    }

    Ok(())
}

// ── CNI installation ──────────────────────────────────────────────────────────

/// Download, verify, and extract the CNI plugins tarball.
fn install_cni(cfg: &CniConfig, assume_yes: bool) -> Result<()> {
    let version = &cfg.version;
    let install_dir = cfg.install_dir.as_deref().unwrap_or(CNI_INSTALL_DIR);

    println!("→ Using CNI plugins version: {version}");

    let filename = format!("cni-plugins-linux-amd64-{version}.tgz");
    let sha512_filename = format!("{filename}.sha512");

    let tgz_url = format!("{CNI_URL_BASE}/{version}/{filename}");
    let sha512_url = format!("{CNI_URL_BASE}/{version}/{sha512_filename}");

    let tmp = tempfile::tempdir().context("failed to create temporary directory for CNI")?;
    let tgz_path = tmp.path().join(&filename);
    let sha512_path = tmp.path().join(&sha512_filename);

    download(&tgz_url, &tgz_path, assume_yes)?;
    download(&sha512_url, &sha512_path, assume_yes)?;

    verify_sha512(&tgz_path, &sha512_path, assume_yes)?;
    extract_cni(&tgz_path, Path::new(install_dir), assume_yes)?;

    println!("✓ CNI plugins {version} installed to {install_dir}");
    Ok(())
}

/// Verify a SHA-512 checksum file produced by `sha512sum`.
///
/// The checksum file downloaded from GitHub contains a line of the form:
/// ```text
/// <hex-digest>  cni-plugins-linux-amd64-<version>.tgz
/// ```
/// Running `sha512sum --check <checksum-file>` from the directory that
/// contains *both* the tarball and the checksum file resolves the bare
/// filename correctly.
fn verify_sha512(tgz: &Path, sha512_file: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying SHA-512 checksum…");

    // Both files live in the same tempdir; run from there so that the bare
    // filename inside the checksum file resolves to the downloaded tarball.
    let dir = tgz
        .parent()
        .context("tarball path has no parent directory")?;

    let sha512_filename = sha512_file
        .file_name()
        .context("SHA-512 file path has no filename component")?;

    let mut cmd = ProcCommand::new("sha512sum");
    cmd.arg("--check").arg(sha512_filename).current_dir(dir);

    let output = run_output(&mut cmd, assume_yes)
        .context("failed to execute `sha512sum --check` (is sha512sum installed?)")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "SHA-512 verification FAILED for `{}`:\n{}\n{}",
            tgz.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    println!("✓ SHA-512 checksum verified");
    Ok(())
}

/// Extract the CNI tarball into `dest`, escalating to `sudo tar` when needed.
fn extract_cni(tgz: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Extracting CNI plugins to {}…", dest.display());

    // Ensure the destination directory exists (try unprivileged first).
    if !dest.exists() {
        if fs::create_dir_all(dest).is_err() {
            let mut cmd = ProcCommand::new("sudo");
            cmd.args(["mkdir", "-p"]).arg(dest);

            let status = run_status(&mut cmd, assume_yes)
                .context("failed to create CNI install directory")?;

            if !status.success() {
                anyhow::bail!(
                    "failed to create CNI install directory `{}`",
                    dest.display()
                );
            }
        }
    }

    // Try an unprivileged extraction first; fall back to sudo on failure.
    let mut cmd = ProcCommand::new("tar");
    cmd.arg("-C").arg(dest).arg("-xzf").arg(tgz);

    let status = run_status(&mut cmd, assume_yes).context("failed to execute `tar`")?;

    if !status.success() {
        println!("  (extraction failed – retrying with sudo)");

        let mut sudo_cmd = ProcCommand::new("sudo");
        sudo_cmd.args(["tar", "-C"]).arg(dest).arg("-xzf").arg(tgz);

        let sudo_status =
            run_status(&mut sudo_cmd, assume_yes).context("failed to execute `sudo tar`")?;

        if !sudo_status.success() {
            anyhow::bail!("`sudo tar` extraction failed (exit {sudo_status})");
        }
    }

    Ok(())
}

// ── Shared helpers ────────────────────────────────────────────────────────────

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
fn verify_gpg_signature(bin: &Path, sig: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying GPG signature…");

    let mut cmd = ProcCommand::new("gpg");
    cmd.arg("--verify").arg(sig).arg(bin);

    let output = run_output(&mut cmd, assume_yes)
        .context("failed to execute `gpg --verify` (is gpg installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "GPG signature verification FAILED for `{}` against `{}`:\n{}",
            bin.display(),
            sig.display(),
            stderr.trim()
        );
    }

    println!("✓ GPG signature verified");
    Ok(())
}

/// Copy the verified binary into place and make it executable.
///
/// Tries a direct `fs::copy` first; falls back to `sudo install` when the
/// destination is in a root-owned directory.
fn install_binary(src: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
    }

    if fs::copy(src, dest).is_err() {
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

// Keep the PathBuf import exercised so the compiler doesn't warn.
fn _assert_pathbuf_used(_: PathBuf) {}

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
