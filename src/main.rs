use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
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
    /// Generate a config.toml file in the current directory
    Generate,

    /// Download, verify, and install container runtime components
    Install {
        #[command(subcommand)]
        target: InstallTarget,
    },

    /// Check for newer versions of installed components
    Outdated,
}

#[derive(Subcommand)]
enum InstallTarget {
    /// Download, verify, and install runc + CNI plugins + containerd +
    /// Kubernetes + Cilium CLI defined in config.toml
    ControlPlane,

    /// Install worker node components (not yet implemented)
    Worker,
}

// ── Constants ────────────────────────────────────────────────────────────────

const RUNC_VERSION: &str = "v1.4.2";
const CNI_VERSION: &str = "v1.9.1";
const CONTAINERD_VERSION: &str = "v2.3.0";
const K8S_VERSION: &str = "v1.36.0";
const CILIUM_VERSION: &str = "v0.19.2";

const CONFIG_FILENAME: &str = "config.toml";

const RUNC_INSTALL_PATH: &str = "/usr/local/sbin/runc";
const CNI_INSTALL_DIR: &str = "/opt/cni/bin";
const CONTAINERD_INSTALL_DIR: &str = "/usr/local";
const K8S_INSTALL_DIR: &str = "/usr/local/bin";
const CILIUM_INSTALL_DIR: &str = "/usr/local/bin";

const RUNC_URL_BASE: &str = "https://github.com/opencontainers/runc/releases/download";
const CNI_URL_BASE: &str = "https://github.com/containernetworking/plugins/releases/download";
const CONTAINERD_URL_BASE: &str = "https://github.com/containerd/containerd/releases/download";
const K8S_URL_BASE: &str = "https://dl.k8s.io";
const CILIUM_URL_BASE: &str = "https://github.com/cilium/cilium-cli/releases/download";

const SYSCTL_CONF_PATH: &str = "/etc/sysctl.d/99-cisak.conf";
const SYSCTL_CONF_CONTENT: &str = "net.ipv4.ip_forward = 1\n";

/// Kubernetes binaries to install. Each tuple is `(binary name, sha256 file suffix)`.
///
/// The download URLs are assembled as:
/// `<K8S_URL_BASE>/<version>/bin/linux/amd64/<name>`
/// `<K8S_URL_BASE>/<version>/bin/linux/amd64/<name>.sha256`
const K8S_BINARIES: &[(&str, &str)] = &[
    ("kubelet", KUBELET_INSTALL_DIR), // /usr/bin
    ("kubeadm", K8S_INSTALL_DIR),     // /usr/local/bin
    ("kubectl", K8S_INSTALL_DIR),     // /usr/local/bin
];

const KUBELET_SERVICE_URL: &str = "https://raw.githubusercontent.com/kubernetes/release/master/\
     cmd/krel/templates/latest/kubelet/kubelet.service";
const KUBEADM_CONF_URL: &str = "https://raw.githubusercontent.com/kubernetes/release/master/\
     cmd/krel/templates/latest/kubeadm/10-kubeadm.conf";

/// Where `kubelet.service` is installed on a systemd host.
const KUBELET_SERVICE_PATH: &str = "/usr/lib/systemd/system/kubelet.service";
/// Drop-in that `kubeadm` uses to inject environment variables into kubelet.
const KUBEADM_DROP_IN_PATH: &str = "/etc/systemd/system/kubelet.service.d/10-kubeadm.conf";

const KUBELET_INSTALL_DIR: &str = "/usr/bin";

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Config {
    runtime: RuntimeConfig,
    cni: Option<CniConfig>,
    containerd: Option<ContainerdConfig>,
    network: Option<NetworkConfig>,
    kubernetes: Option<KubernetesConfig>,
    cilium: Option<CiliumConfig>,
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

#[derive(Debug, Deserialize)]
struct ContainerdConfig {
    version: String,
    /// Prefix under which the tarball's `bin/` subtree is unpacked.
    /// Defaults to `/usr/local`; binaries end up in `<install_dir>/bin/`.
    install_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NetworkConfig {
    /// Enable net.ipv4.ip_forward (required for container networking).
    /// Defaults to `true` when the [network] section is present.
    ipv4_forward: Option<bool>,
    /// Drop-in file used to persist the setting across reboots.
    /// Defaults to `/etc/sysctl.d/99-cisak.conf`.
    sysctl_conf_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KubernetesConfig {
    version: String,
    /// Directory for `kubeadm` and `kubectl`. Defaults to `/usr/local/bin`.
    install_dir: Option<String>,
    /// Directory for `kubelet`. Defaults to `/usr/bin`.
    kubelet_install_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CiliumConfig {
    version: String,
    /// Directory for the `cilium` binary. Defaults to `/usr/local/bin`.
    install_dir: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────

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

/// Write a `config.toml` scaffold to the current working directory.
fn generate() -> Result<()> {
    let path = Path::new(CONFIG_FILENAME);

    if path.exists() {
        anyhow::bail!("`{CONFIG_FILENAME}` already exists in the current directory");
    }

    let content = build_config(
        RUNC_VERSION,
        CNI_VERSION,
        CONTAINERD_VERSION,
        K8S_VERSION,
        CILIUM_VERSION,
    );

    fs::write(path, &content).with_context(|| format!("failed to write `{CONFIG_FILENAME}`"))?;

    println!(
        "✓ Created {CONFIG_FILENAME}  \
         (runc {RUNC_VERSION}, CNI plugins {CNI_VERSION}, \
         containerd {CONTAINERD_VERSION}, Kubernetes {K8S_VERSION}, \
         Cilium CLI {CILIUM_VERSION})"
    );
    Ok(())
}

/// Download, verify, and install runc + CNI plugins + containerd + Kubernetes
/// binaries + Cilium CLI as declared in config.toml.
fn install_control_plane(assume_yes: bool) -> Result<()> {
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

    println!();
    if let Some(cni_cfg) = &cfg.cni {
        install_cni(cni_cfg, assume_yes)?;
    } else {
        println!("  (no [cni] section in config – skipping CNI install)");
    }

    // ── containerd ────────────────────────────────────────────────────────────

    println!();
    if let Some(containerd_cfg) = &cfg.containerd {
        install_containerd(containerd_cfg, assume_yes)?;
    } else {
        println!("  (no [containerd] section in config – skipping containerd install)");
    }

    // ── Kubernetes binaries ───────────────────────────────────────────────────

    println!();
    if let Some(k8s_cfg) = &cfg.kubernetes {
        install_kubernetes(k8s_cfg, assume_yes)?;
    } else {
        println!("  (no [kubernetes] section in config – skipping kubelet/kubeadm install)");
    }

    // ── Cilium CLI ────────────────────────────────────────────────────────────

    println!();
    if let Some(cilium_cfg) = &cfg.cilium {
        install_cilium(cilium_cfg, assume_yes)?;
    } else {
        println!("  (no [cilium] section in config – skipping Cilium CLI install)");
    }

    // ── IPv4 forwarding ───────────────────────────────────────────────────────

    println!();
    if let Some(net_cfg) = &cfg.network {
        enable_ipv4_forward(net_cfg, assume_yes)?;
    } else {
        println!("  (no [network] section in config – skipping ipv4.ip_forward)");
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

    println!("→ Extracting CNI plugins to {install_dir}…");
    extract_tarball(&tgz_path, Path::new(install_dir), assume_yes)
        .context("failed to extract CNI tarball")?;

    println!("✓ CNI plugins {version} installed to {install_dir}");
    Ok(())
}

// ── containerd installation ───────────────────────────────────────────────────

/// Download, verify (SHA-256), and extract the containerd tarball.
///
/// The upstream tarball has the layout:
/// ```text
/// bin/containerd
/// bin/containerd-shim-runc-v2
/// bin/ctr
/// …
/// ```
/// so extracting into `install_dir` (default `/usr/local`) places all
/// binaries under `<install_dir>/bin/`.
fn install_containerd(cfg: &ContainerdConfig, assume_yes: bool) -> Result<()> {
    let version = &cfg.version;
    let install_dir = cfg.install_dir.as_deref().unwrap_or(CONTAINERD_INSTALL_DIR);
    let version_bare = version.trim_start_matches('v');

    println!("→ Using containerd version: {version}");

    let filename = format!("containerd-{version_bare}-linux-amd64.tar.gz");
    let sha256_filename = format!("{filename}.sha256sum");

    let tgz_url = format!("{CONTAINERD_URL_BASE}/{version}/{filename}");
    let sha256_url = format!("{CONTAINERD_URL_BASE}/{version}/{sha256_filename}");

    let tmp = tempfile::tempdir().context("failed to create temporary directory for containerd")?;
    let tgz_path = tmp.path().join(&filename);
    let sha256_path = tmp.path().join(&sha256_filename);

    download(&tgz_url, &tgz_path, assume_yes)?;
    download(&sha256_url, &sha256_path, assume_yes)?;

    verify_sha256(&tgz_path, &sha256_path, assume_yes)?;

    println!("→ Extracting containerd to {install_dir}/bin/…");
    extract_tarball(&tgz_path, Path::new(install_dir), assume_yes)
        .context("failed to extract containerd tarball")?;

    println!("✓ containerd {version} installed to {install_dir}/bin/");

    install_containerd_systemd_unit(assume_yes)?;

    Ok(())
}

// ── Kubernetes installation ───────────────────────────────────────────────────

/// Download, verify (SHA-256), and install `kubelet`, `kubeadm`, and `kubectl`.
fn install_kubernetes(cfg: &KubernetesConfig, assume_yes: bool) -> Result<()> {
    let version = &cfg.version;
    let install_dir = cfg.install_dir.as_deref().unwrap_or(K8S_INSTALL_DIR);
    let kubelet_dir = cfg
        .kubelet_install_dir
        .as_deref()
        .unwrap_or(KUBELET_INSTALL_DIR);

    println!("→ Using Kubernetes version: {version}");

    let base = format!("{K8S_URL_BASE}/{version}/bin/linux/amd64");

    for &(binary, _) in K8S_BINARIES {
        let binary_dir = if binary == "kubelet" {
            kubelet_dir
        } else {
            install_dir
        };

        println!();
        println!("→ Installing {binary} → {binary_dir}…");

        let bin_url = format!("{base}/{binary}");
        let sha256_url = format!("{base}/{binary}.sha256");

        let tmp = tempfile::tempdir()
            .with_context(|| format!("failed to create temporary directory for {binary}"))?;
        let bin_path = tmp.path().join(binary);
        let sha256_path = tmp.path().join(format!("{binary}.sha256"));

        download(&bin_url, &bin_path, assume_yes)?;
        download(&sha256_url, &sha256_path, assume_yes)?;

        verify_k8s_sha256(&bin_path, &sha256_path, assume_yes)
            .with_context(|| format!("checksum verification failed for {binary}"))?;

        let dest = Path::new(binary_dir).join(binary);
        install_binary(&bin_path, &dest, assume_yes)
            .with_context(|| format!("failed to install {binary}"))?;

        println!("✓ {binary} {version} installed to {}", dest.display());
    }

    println!();
    println!(
        "✓ Kubernetes binaries installed \
         (kubelet → {kubelet_dir}, kubeadm/kubectl → {install_dir})"
    );

    println!();
    install_kubernetes_systemd_units(assume_yes)?;

    Ok(())
}

// ── Cilium CLI installation ───────────────────────────────────────────────────

/// Download, verify (SHA-256), and install the Cilium CLI binary.
///
/// Release layout on GitHub:
/// ```text
/// https://github.com/cilium/cilium-cli/releases/download/<version>/cilium-linux-amd64.tar.gz
/// https://github.com/cilium/cilium-cli/releases/download/<version>/cilium-linux-amd64.tar.gz.sha256sum
/// ```
///
/// The tarball contains a single `cilium` binary at its root.  After
/// extraction the binary is moved to `<install_dir>/cilium` (default
/// `/usr/local/bin/cilium`).
fn install_cilium(cfg: &CiliumConfig, assume_yes: bool) -> Result<()> {
    let version = &cfg.version;
    let install_dir = cfg.install_dir.as_deref().unwrap_or(CILIUM_INSTALL_DIR);

    println!("→ Using Cilium CLI version: {version}");

    let filename = "cilium-linux-amd64.tar.gz";
    let sha256_filename = format!("{filename}.sha256sum");

    let tgz_url = format!("{CILIUM_URL_BASE}/{version}/{filename}");
    let sha256_url = format!("{CILIUM_URL_BASE}/{version}/{sha256_filename}");

    let tmp = tempfile::tempdir().context("failed to create temporary directory for Cilium")?;
    let tgz_path = tmp.path().join(filename);
    let sha256_path = tmp.path().join(&sha256_filename);

    download(&tgz_url, &tgz_path, assume_yes)?;
    download(&sha256_url, &sha256_path, assume_yes)?;

    verify_sha256(&tgz_path, &sha256_path, assume_yes)?;

    let extract_dir = tmp.path().join("cilium-extract");
    fs::create_dir_all(&extract_dir).context("failed to create Cilium extraction directory")?;

    println!("→ Extracting Cilium CLI…");
    extract_tarball(&tgz_path, &extract_dir, assume_yes)
        .context("failed to extract Cilium tarball")?;

    let bin_src = extract_dir.join("cilium");
    let bin_dest = Path::new(install_dir).join("cilium");

    install_binary(&bin_src, &bin_dest, assume_yes).context("failed to install cilium binary")?;

    println!("✓ Cilium CLI {version} installed to {}", bin_dest.display());
    Ok(())
}

// ── Checksum verification ─────────────────────────────────────────────────────

/// Verify a SHA-512 checksum file produced by `sha512sum`.
fn verify_sha512(tgz: &Path, sha512_file: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying SHA-512 checksum…");

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

/// Verify a SHA-256 checksum file produced by `sha256sum`.
fn verify_sha256(tgz: &Path, sha256_file: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying SHA-256 checksum…");

    let dir = tgz
        .parent()
        .context("tarball path has no parent directory")?;
    let sha256_filename = sha256_file
        .file_name()
        .context("SHA-256 file path has no filename component")?;

    let mut cmd = ProcCommand::new("sha256sum");
    cmd.arg("--check").arg(sha256_filename).current_dir(dir);

    let output = run_output(&mut cmd, assume_yes)
        .context("failed to execute `sha256sum --check` (is sha256sum installed?)")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "SHA-256 verification FAILED for `{}`:\n{}\n{}",
            tgz.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    println!("✓ SHA-256 checksum verified");
    Ok(())
}

/// Verify a Kubernetes-style SHA-256 file against a downloaded binary.
///
/// Unlike the `sha256sum` format used by containerd, Kubernetes `.sha256` files
/// contain **only the bare hex digest** with no filename.  This function
/// synthesises a proper `sha256sum`-format line before running the check.
fn verify_k8s_sha256(binary: &Path, sha256_file: &Path, assume_yes: bool) -> Result<()> {
    println!("→ Verifying SHA-256 checksum…");

    let raw = fs::read_to_string(sha256_file)
        .with_context(|| format!("failed to read `{}`", sha256_file.display()))?;
    let digest = raw.trim();

    let dir = binary
        .parent()
        .context("binary path has no parent directory")?;
    let binary_filename = binary
        .file_name()
        .context("binary path has no filename component")?
        .to_string_lossy();

    let check_filename = format!("{binary_filename}.sha256check");
    let check_path = dir.join(&check_filename);
    let check_content = format!("{digest}  {binary_filename}\n");

    fs::write(&check_path, &check_content)
        .context("failed to write formatted SHA-256 check file")?;

    let mut cmd = ProcCommand::new("sha256sum");
    cmd.arg("--check").arg(&check_filename).current_dir(dir);

    let output = run_output(&mut cmd, assume_yes)
        .context("failed to execute `sha256sum --check` (is sha256sum installed?)")?;

    let _ = fs::remove_file(&check_path);

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "SHA-256 verification FAILED for `{}`:\n{}\n{}",
            binary.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    println!("✓ SHA-256 checksum verified");
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

/// Extract a `.tar.gz` archive into `dest`, escalating to `sudo tar` when the
/// unprivileged attempt fails.
fn extract_tarball(tgz: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
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
        Command::Generate => generate()?,
        Command::Install { target } => match target {
            InstallTarget::ControlPlane => install_control_plane(assume_yes)?,
            InstallTarget::Worker => {
                println!("  (worker node installation is not yet implemented)");
            }
        },
        Command::Outdated => outdated()?,
    }

    Ok(())
}

// ── containerd systemd ────────────────────────────────────────────────────────

/// Download and install the containerd.service systemd unit file, then reload
/// systemd and enable the service.
fn install_containerd_systemd_unit(assume_yes: bool) -> Result<()> {
    if !assume_yes {
        print!("  Install and enable containerd systemd unit? [y/N] ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read user input")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => {}
            _ => {
                println!("  (skipping systemd unit installation)");
                return Ok(());
            }
        }
    }

    println!("→ Installing containerd systemd unit…");

    let unit_url =
        "https://raw.githubusercontent.com/containerd/containerd/main/containerd.service";
    let unit_dir = Path::new("/usr/local/lib/systemd/system");
    let unit_path = unit_dir.join("containerd.service");

    let tmp =
        tempfile::tempdir().context("failed to create temporary directory for systemd unit")?;
    let tmp_unit_path = tmp.path().join("containerd.service");

    download(unit_url, &tmp_unit_path, assume_yes)?;

    if !unit_dir.exists() {
        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["mkdir", "-p"]).arg(unit_dir);

        let status =
            run_status(&mut cmd, assume_yes).context("failed to create systemd directory")?;

        if !status.success() {
            anyhow::bail!(
                "failed to create systemd directory `{}`",
                unit_dir.display()
            );
        }
    }

    if fs::copy(&tmp_unit_path, &unit_path).is_err() {
        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["cp"]).arg(&tmp_unit_path).arg(&unit_path);

        let status =
            run_status(&mut cmd, assume_yes).context("failed to install systemd unit file")?;

        if !status.success() {
            anyhow::bail!("failed to install systemd unit file (exit {status})");
        }
    }

    println!("→ Reloading systemd daemon…");
    let mut reload_cmd = ProcCommand::new("sudo");
    reload_cmd.arg("systemctl").arg("daemon-reload");

    let reload_status = run_status(&mut reload_cmd, assume_yes)
        .context("failed to execute `systemctl daemon-reload`")?;

    if !reload_status.success() {
        anyhow::bail!("`systemctl daemon-reload` failed (exit {reload_status})");
    }

    println!("→ Enabling and starting containerd service…");
    let mut enable_cmd = ProcCommand::new("sudo");
    enable_cmd.args(["systemctl", "enable", "--now", "containerd"]);

    let enable_status = run_status(&mut enable_cmd, assume_yes)
        .context("failed to execute `systemctl enable --now containerd`")?;

    if !enable_status.success() {
        anyhow::bail!("`systemctl enable --now containerd` failed (exit {enable_status})");
    }

    println!("✓ containerd systemd unit installed and service enabled");
    Ok(())
}

// ── Kubernetes systemd ────────────────────────────────────────────────────────

/// Download and install the kubelet systemd unit and kubeadm drop-in, then
/// reload systemd and enable the service.
fn install_kubernetes_systemd_units(assume_yes: bool) -> Result<()> {
    if !assume_yes {
        print!("  Install and enable kubelet systemd unit? [y/N] ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read user input")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => {}
            _ => {
                println!("  (skipping kubelet systemd unit installation)");
                return Ok(());
            }
        }
    }

    println!("→ Installing kubelet systemd unit files…");

    let tmp = tempfile::tempdir()
        .context("failed to create temporary directory for kubelet systemd units")?;

    // ── kubelet.service ───────────────────────────────────────────────────────

    let tmp_service = tmp.path().join("kubelet.service");
    download(KUBELET_SERVICE_URL, &tmp_service, assume_yes)?;

    install_system_file(&tmp_service, Path::new(KUBELET_SERVICE_PATH), assume_yes)
        .context("failed to install kubelet.service")?;

    // ── 10-kubeadm.conf (drop-in) ─────────────────────────────────────────────

    let tmp_conf = tmp.path().join("10-kubeadm.conf");
    download(KUBEADM_CONF_URL, &tmp_conf, assume_yes)?;

    install_system_file(&tmp_conf, Path::new(KUBEADM_DROP_IN_PATH), assume_yes)
        .context("failed to install 10-kubeadm.conf")?;

    // ── systemd reload ────────────────────────────────────────────────────────

    println!("→ Reloading systemd daemon…");
    let mut cmd = ProcCommand::new("sudo");
    cmd.args(["systemctl", "daemon-reload"]);

    let status =
        run_status(&mut cmd, assume_yes).context("failed to execute `systemctl daemon-reload`")?;
    if !status.success() {
        anyhow::bail!("`systemctl daemon-reload` failed (exit {status})");
    }

    // ── enable kubelet ────────────────────────────────────────────────────────

    println!("→ Enabling kubelet service…");
    let mut cmd = ProcCommand::new("sudo");
    cmd.args(["systemctl", "enable", "kubelet"]);

    let status =
        run_status(&mut cmd, assume_yes).context("failed to execute `systemctl enable kubelet`")?;
    if !status.success() {
        anyhow::bail!("`systemctl enable kubelet` failed (exit {status})");
    }

    println!("✓ kubelet systemd unit installed and service enabled");
    Ok(())
}

/// Ensure `dest`'s parent directory exists, then copy `src` → `dest`.
///
/// Falls back to `sudo cp` when an unprivileged copy fails.
fn install_system_file(src: &Path, dest: &Path, assume_yes: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            if fs::create_dir_all(parent).is_err() {
                let mut cmd = ProcCommand::new("sudo");
                cmd.args(["mkdir", "-p"]).arg(parent);

                let status = run_status(&mut cmd, assume_yes)
                    .with_context(|| format!("failed to create `{}`", parent.display()))?;
                if !status.success() {
                    anyhow::bail!("failed to create directory `{}`", parent.display());
                }
            }
        }
    }

    if fs::copy(src, dest).is_err() {
        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["cp"]).arg(src).arg(dest);

        let status = run_status(&mut cmd, assume_yes)
            .with_context(|| format!("failed to copy to `{}`", dest.display()))?;
        if !status.success() {
            anyhow::bail!(
                "failed to install file to `{}` (exit {status})",
                dest.display()
            );
        }
    }

    Ok(())
}

/// Return the rendered TOML configuration string.
fn build_config(
    runc_version: &str,
    cni_version: &str,
    containerd_version: &str,
    k8s_version: &str,
    cilium_version: &str,
) -> String {
    format!(
        r#"# Generated by cisak

[runtime]
name    = "runc"
version = "{runc_version}"
binary  = "/usr/local/sbin/runc"

[cni]
version     = "{cni_version}"
install_dir = "/opt/cni/bin"

[containerd]
# Binaries are unpacked into <install_dir>/bin/ (e.g. /usr/local/bin/containerd)
version     = "{containerd_version}"
install_dir = "/usr/local"

[kubernetes]
# kubelet is installed to kubelet_install_dir; kubeadm + kubectl to install_dir.
version             = "{k8s_version}"
install_dir         = "/usr/local/bin"
kubelet_install_dir = "/usr/bin"

[cilium]
# Cilium CLI binary installed to <install_dir>/cilium.
version     = "{cilium_version}"
install_dir = "/usr/local/bin"

[network]
# Set net.ipv4.ip_forward = 1 (required for container networking).
# The setting is written to sysctl_conf_path for persistence across reboots.
ipv4_forward     = true
sysctl_conf_path = "/etc/sysctl.d/99-cisak.conf"
"#
    )
}

// ── IPv4 forwarding ───────────────────────────────────────────────────────────

/// Enable `net.ipv4.ip_forward` at runtime and persist it via a sysctl drop-in file.
fn enable_ipv4_forward(cfg: &NetworkConfig, assume_yes: bool) -> Result<()> {
    if !cfg.ipv4_forward.unwrap_or(true) {
        println!("  (ipv4_forward = false in config – skipping)");
        return Ok(());
    }

    let conf_path_str = cfg.sysctl_conf_path.as_deref().unwrap_or(SYSCTL_CONF_PATH);
    let conf_path = Path::new(conf_path_str);

    println!("→ Checking net.ipv4.ip_forward…");

    let current = ProcCommand::new("sysctl")
        .args(["-n", "net.ipv4.ip_forward"])
        .output()
        .context("failed to execute `sysctl -n net.ipv4.ip_forward` (is sysctl installed?)")?;

    if current.status.success() && String::from_utf8_lossy(&current.stdout).trim() == "1" {
        println!("✓ net.ipv4.ip_forward is already 1");
    } else {
        println!("→ Enabling net.ipv4.ip_forward (runtime)…");

        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["sysctl", "-w", "net.ipv4.ip_forward=1"]);

        let status = run_status(&mut cmd, assume_yes)
            .context("failed to execute `sysctl -w net.ipv4.ip_forward=1`")?;

        if !status.success() {
            anyhow::bail!("`sysctl -w net.ipv4.ip_forward=1` failed (exit {status})");
        }

        println!("✓ net.ipv4.ip_forward = 1 (active until next reboot)");
    }

    println!("→ Persisting setting in {conf_path_str}…");

    let write_result = (|| -> Result<()> {
        if let Some(parent) = conf_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
        fs::write(conf_path, SYSCTL_CONF_CONTENT)
            .with_context(|| format!("failed to write `{conf_path_str}`"))
    })();

    if write_result.is_err() {
        println!("  (write failed – retrying with sudo tee)");

        let tmp =
            tempfile::tempdir().context("failed to create temporary directory for sysctl conf")?;
        let tmp_conf = tmp.path().join("99-cisak.conf");
        fs::write(&tmp_conf, SYSCTL_CONF_CONTENT)
            .context("failed to write temporary sysctl conf")?;

        if let Some(parent) = conf_path.parent() {
            if !parent.exists() {
                let mut mkdir = ProcCommand::new("sudo");
                mkdir.args(["mkdir", "-p"]).arg(parent);
                let s = run_status(&mut mkdir, assume_yes)
                    .context("failed to create sysctl.d directory")?;
                if !s.success() {
                    anyhow::bail!("failed to create `{}`", parent.display());
                }
            }
        }

        let mut cmd = ProcCommand::new("sudo");
        cmd.args(["cp"]).arg(&tmp_conf).arg(conf_path);

        let status =
            run_status(&mut cmd, assume_yes).context("failed to install sysctl conf file")?;

        if !status.success() {
            anyhow::bail!("failed to write `{conf_path_str}` even with sudo (exit {status})");
        }
    }

    println!("✓ Wrote {conf_path_str}");

    println!("→ Applying sysctl settings (`sysctl --system`)…");

    let mut cmd = ProcCommand::new("sudo");
    cmd.args(["sysctl", "--system"]);

    let status = run_status(&mut cmd, assume_yes).context("failed to execute `sysctl --system`")?;

    if !status.success() {
        anyhow::bail!("`sysctl --system` failed (exit {status})");
    }

    println!("✓ net.ipv4.ip_forward = 1 (persistent via {conf_path_str})");
    Ok(())
}

// ── Outdated command ──────────────────────────────────────────────────────────

/// Check for newer versions of installed components.
fn outdated() -> Result<()> {
    let cfg = load_config()?;

    println!(
        "{}\n",
        "Checking for newer versions...".bright_blue().bold()
    );

    match fetch_latest_github_release("opencontainers", "runc") {
        Ok(latest) => check_and_display_version("runc", &cfg.runtime.version, &latest),
        Err(e) => println!("  {}: {}", "runc".red(), format!("✗ {}", e).red()),
    }

    if let Some(cni_cfg) = &cfg.cni {
        match fetch_latest_github_release("containernetworking", "plugins") {
            Ok(latest) => check_and_display_version("CNI plugins", &cni_cfg.version, &latest),
            Err(e) => println!("  {}: {}", "CNI plugins".red(), format!("✗ {}", e).red()),
        }
    }

    if let Some(containerd_cfg) = &cfg.containerd {
        match fetch_latest_github_release("containerd", "containerd") {
            Ok(latest) => check_and_display_version("containerd", &containerd_cfg.version, &latest),
            Err(e) => println!("  {}: {}", "containerd".red(), format!("✗ {}", e).red()),
        }
    }

    if let Some(k8s_cfg) = &cfg.kubernetes {
        match fetch_latest_k8s_version() {
            Ok(latest) => check_and_display_version("Kubernetes", &k8s_cfg.version, &latest),
            Err(e) => println!("  {}: {}", "Kubernetes".red(), format!("✗ {}", e).red()),
        }
    }

    if let Some(cilium_cfg) = &cfg.cilium {
        match fetch_latest_github_release("cilium", "cilium-cli") {
            Ok(latest) => check_and_display_version("Cilium CLI", &cilium_cfg.version, &latest),
            Err(e) => println!("  {}: {}", "Cilium CLI".red(), format!("✗ {}", e).red()),
        }
    }

    println!();
    Ok(())
}

/// Fetch the latest release tag from a GitHub repository.
fn fetch_latest_github_release(owner: &str, repo: &str) -> Result<String> {
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
fn fetch_latest_k8s_version() -> Result<String> {
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

/// Display version comparison with colour highlighting.
fn check_and_display_version(name: &str, current: &str, latest: &str) {
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
