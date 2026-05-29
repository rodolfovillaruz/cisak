use clap::{Parser, Subcommand};
use serde::Deserialize;

#[derive(Parser)]
#[command(
    name = "cisak",
    version,
    about = "Container Installation - Swiss Army Knife"
)]
pub struct Cli {
    /// Skip all y/N confirmations (assume yes)
    #[arg(short = 'y', long = "assume-yes", global = true)]
    pub assume_yes: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Generate a config.toml file in the current directory
    Generate,

    /// Download, verify, and install container runtime components
    Install,

    /// Check for newer versions of installed components
    Outdated,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub cni: Option<CniConfig>,
    pub containerd: Option<ContainerdConfig>,
    pub network: Option<NetworkConfig>,
    pub kubernetes: Option<KubernetesConfig>,
    pub cilium: Option<CiliumConfig>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeConfig {
    #[allow(dead_code)]
    name: String,
    pub version: String,
    pub binary: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CniConfig {
    pub version: String,
    pub install_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContainerdConfig {
    pub version: String,
    /// Prefix under which the tarball's `bin/` subtree is unpacked.
    /// Defaults to `/usr/local`; binaries end up in `<install_dir>/bin/`.
    pub install_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NetworkConfig {
    /// Enable net.ipv4.ip_forward (required for container networking).
    /// Defaults to `true` when the [network] section is present.
    pub ipv4_forward: Option<bool>,
    /// Drop-in file used to persist the setting across reboots.
    /// Defaults to `/etc/sysctl.d/99-cisak.conf`.
    pub sysctl_conf_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KubernetesConfig {
    pub version: String,
    /// Directory for `kubeadm` and `kubectl`. Defaults to `/usr/local/bin`.
    pub install_dir: Option<String>,
    /// Directory for `kubelet`. Defaults to `/usr/bin`.
    pub kubelet_install_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CiliumConfig {
    pub version: String,
    /// Directory for the `cilium` binary. Defaults to `/usr/local/bin`.
    pub install_dir: Option<String>,
}
