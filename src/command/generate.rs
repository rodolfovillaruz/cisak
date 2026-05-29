use crate::fs;
use crate::r#const::CILIUM_VERSION;
use crate::r#const::CNI_VERSION;
use crate::r#const::CONFIG_FILENAME;
use crate::r#const::CONTAINERD_VERSION;
use crate::r#const::K8S_VERSION;
use crate::r#const::RUNC_VERSION;
use crate::r#struct::{CiliumConfig, CniConfig, Config, KubernetesConfig, RuntimeConfig};
use crate::ContainerdConfig;
use anyhow::{Context, Result};

/// Write a `config.toml` scaffold to the current working directory.
pub fn generate() -> Result<()> {
    let config = build_config(
        RUNC_VERSION,
        CNI_VERSION,
        CONTAINERD_VERSION,
        K8S_VERSION,
        CILIUM_VERSION,
    );
    let toml_string =
        toml::to_string_pretty(&config).with_context(|| "failed to serialize config to TOML")?; // 👈 serialize here
    fs::write(CONFIG_FILENAME, toml_string)
        .with_context(|| format!("failed to write `{CONFIG_FILENAME}`"))?;
    Ok(())
}

/// Return the rendered TOML configuration string.
pub fn build_config(
    runc_version: &str,
    cni_version: &str,
    containerd_version: &str,
    k8s_version: &str,
    cilium_version: &str,
) -> Config {
    Config {
        runtime: RuntimeConfig {
            name: "runc".to_string(), // or whatever the default is
            version: runc_version.to_string(),
            binary: None,
        },
        cni: Some(CniConfig {
            version: cni_version.to_string(),
            install_dir: None,
        }),
        containerd: Some(ContainerdConfig {
            version: containerd_version.to_string(),
            install_dir: None,
        }),
        network: None, // adjust to match your defaults
        kubernetes: Some(KubernetesConfig {
            version: k8s_version.to_string(),
            install_dir: None,
            kubelet_install_dir: None,
        }),
        cilium: Some(CiliumConfig {
            version: cilium_version.to_string(),
            install_dir: None,
        }),
    }
}
