use crate::command::generate::build_config;
use crate::fs;
use crate::helper::{download, ensure_gpg_key, extract_tarball, load_config};
use crate::io;
use crate::r#const::{
    CILIUM_INSTALL_DIR, CILIUM_URL_BASE, CILIUM_VERSION, CNI_INSTALL_DIR, CNI_URL_BASE,
    CNI_VERSION, CONTAINERD_INSTALL_DIR, CONTAINERD_URL_BASE, CONTAINERD_VERSION, K8S_BINARIES,
    K8S_INSTALL_DIR, K8S_URL_BASE, K8S_VERSION, KUBEADM_CONF_URL, KUBEADM_DROP_IN_PATH,
    KUBELET_INSTALL_DIR, KUBELET_SERVICE_PATH, KUBELET_SERVICE_URL, RUNC_INSTALL_PATH,
    RUNC_URL_BASE, RUNC_VERSION, SYSCTL_CONF_CONTENT, SYSCTL_CONF_PATH,
};
use crate::r#struct::{CiliumConfig, CniConfig, NetworkConfig};
use crate::run_status;
use crate::verify::{verify_gpg_signature, verify_k8s_sha256, verify_sha256, verify_sha512};
use crate::{ContainerdConfig, KubernetesConfig, Path, ProcCommand};
use anyhow::{Context, Result};
use std::io::Write;
use std::path::PathBuf;

// ── Shared installation steps (control-plane AND worker) ─────────────────────

/// Install runc, CNI plugins, containerd, Kubernetes binaries, the Cilium CLI
/// binary, configure IPv4 forwarding, and pre-pull Kubernetes control-plane
/// images.
///
/// This is the common prefix shared by both `install control-plane` and
/// `install worker`.  Control-plane continues with [`kubeadm_init`],
/// [`cilium_cni_install`], and [`cilium_cni_status_wait`] afterwards; a worker
/// node stops here.
pub fn install_common(assume_yes: bool, use_default: bool) -> Result<()> {
    let cfg = if use_default {
        build_config(
            RUNC_VERSION,
            CNI_VERSION,
            CONTAINERD_VERSION,
            K8S_VERSION,
            CILIUM_VERSION,
        )
    } else {
        load_config()?
    };

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

    // ── Pre-pull control-plane images ─────────────────────────────────────────

    println!();
    kubeadm_pull_images(assume_yes)?;

    Ok(())
}

// ── containerd systemd ────────────────────────────────────────────────────────

/// Download and install the containerd.service systemd unit file, then reload
/// systemd and enable the service.
pub fn install_containerd_systemd_unit(assume_yes: bool) -> Result<()> {
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
pub fn install_kubernetes_systemd_units(assume_yes: bool) -> Result<()> {
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
        if !parent.exists() && fs::create_dir_all(parent).is_err() {
            let mut cmd = ProcCommand::new("sudo");
            cmd.args(["mkdir", "-p"]).arg(parent);

            let status = run_status(&mut cmd, assume_yes)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
            if !status.success() {
                anyhow::bail!("failed to create directory `{}`", parent.display());
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

// ── kubeadm / Cilium cluster steps ───────────────────────────────────────────

/// Run `sudo kubeadm config images pull` to pre-fetch all control-plane
/// container images before `kubeadm init` begins.
fn kubeadm_pull_images(assume_yes: bool) -> Result<()> {
    println!("→ Pre-pulling Kubernetes control-plane images…");

    let mut cmd = ProcCommand::new("sudo");
    cmd.args(["kubeadm", "config", "images", "pull"]);

    let status = run_status(&mut cmd, assume_yes)
        .context("failed to execute `kubeadm config images pull`")?;

    if !status.success() {
        anyhow::bail!("`kubeadm config images pull` failed (exit {status})");
    }

    println!("✓ Kubernetes control-plane images pulled");
    Ok(())
}
