use crate::helper::check_and_display_version;
use crate::helper::fetch_latest_github_release;
use crate::helper::fetch_latest_k8s_version;
use crate::helper::load_config;
use anyhow::Result;
use colored::Colorize;

// ── Outdated command ──────────────────────────────────────────────────────────

/// Check for newer versions of installed components.
pub fn outdated() -> Result<()> {
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
