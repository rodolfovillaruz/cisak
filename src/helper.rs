use crate::fs;
use crate::r#const::CONFIG_FILENAME;
use crate::r#struct::Config;
use anyhow::{Context, Result};
use colored::Colorize;
use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Command as ProcCommand;

/// Import the runc signing key into the local keyring if it is not already present.
pub fn ensure_gpg_key(key_id: &str, assume_yes: bool) -> Result<()> {
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

/// Prompt, then spawn the command and return its `ExitStatus`.
pub fn run_status(cmd: &mut ProcCommand, assume_yes: bool) -> Result<std::process::ExitStatus> {
    prompt(cmd, assume_yes)?;
    cmd.status().context("failed to spawn process")
}

/// Prompt, then spawn the command and capture its output.
pub fn run_output(cmd: &mut ProcCommand, assume_yes: bool) -> Result<std::process::Output> {
    prompt(cmd, assume_yes)?;
    cmd.output().context("failed to spawn process")
}

/// Print the exact command that is about to run, then ask the user to confirm.
pub fn prompt(cmd: &ProcCommand, assume_yes: bool) -> Result<()> {
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

/// Download a URL to a local path using `curl`.
pub fn download(url: &str, dest: &Path, assume_yes: bool) -> Result<()> {
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

pub fn load_config() -> Result<Config> {
    let raw = fs::read_to_string(CONFIG_FILENAME)
        .with_context(|| format!("failed to read `{CONFIG_FILENAME}`"))?;
    toml::from_str::<Config>(&raw).with_context(|| format!("failed to parse `{CONFIG_FILENAME}`"))
}

/// Fetch the latest release tag from a GitHub repository.
pub fn fetch_latest_github_release(owner: &str, repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");

    let output = ProcCommand::new("curl")
        .args([
            "--fail",
            "--silent",
            "--location",
            "--header",
            "Accept: application/vnd.github.v3+json",
        ])
        .arg(&url)
        .output()
        .context("failed to execute curl for GitHub API")?;

    if !output.status.success() {
        anyhow::bail!("HTTP request failed");
    }

    let json_str =
        String::from_utf8(output.stdout).context("failed to parse GitHub API response as UTF-8")?;

    let json: serde_json::Value =
        serde_json::from_str(&json_str).context("failed to parse GitHub API response as JSON")?;

    json["tag_name"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("tag_name not found in GitHub API response"))
}

/// Fetch the latest Kubernetes release version from `dl.k8s.io/release/stable.txt`.
pub fn fetch_latest_k8s_version() -> Result<String> {
    let url = "https://dl.k8s.io/release/stable.txt";

    let output = ProcCommand::new("curl")
        .args(["--fail", "--silent", "--location"])
        .arg(url)
        .output()
        .context("failed to fetch Kubernetes version")?;

    if !output.status.success() {
        anyhow::bail!("HTTP request failed");
    }

    let version = String::from_utf8(output.stdout)
        .context("failed to parse response as UTF-8")?
        .trim()
        .to_string();

    Ok(version)
}

/// Display version comparison with colour highlighting.
pub fn check_and_display_version(name: &str, current: &str, latest: &str) {
    if is_version_newer(latest, current) {
        println!(
            "  {} {} {} {}",
            name.bright_yellow(),
            current,
            "→".bright_yellow(),
            latest.bright_green()
        );
    } else {
        println!(
            "  {} {} {}",
            name.bright_black(),
            current,
            "(up to date)".bright_black()
        );
    }
}

/// Returns `true` if `latest` is numerically newer than `current`.
fn is_version_newer(latest: &str, current: &str) -> bool {
    let latest_clean = latest.trim_start_matches('v');
    let current_clean = current.trim_start_matches('v');

    if latest_clean == current_clean {
        return false;
    }

    let latest_parts: Vec<&str> = latest_clean.split('.').collect();
    let current_parts: Vec<&str> = current_clean.split('.').collect();

    for i in 0..latest_parts.len().max(current_parts.len()) {
        let l = latest_parts
            .get(i)
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(0);
        let c = current_parts
            .get(i)
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(0);

        if l > c {
            return true;
        } else if l < c {
            return false;
        }
    }

    false
}

/// Extract a `.tar.gz` archive into `dest`, escalating to `sudo tar` when the
/// unprivileged attempt fails.
pub fn extract_tarball(tgz: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
    if !dest.exists() && fs::create_dir_all(dest).is_err() {
        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["mkdir", "-p"]).arg(dest);

        let status =
            run_status(&mut cmd, assume_yes).context("failed to create install directory")?;

        if !status.success() {
            anyhow::bail!("failed to create install directory `{}`", dest.display());
        }
    }

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
