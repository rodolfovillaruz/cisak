// ── Constants ────────────────────────────────────────────────────────────────

pub const RUNC_VERSION: &str = "v1.4.2";
pub const CNI_VERSION: &str = "v1.9.1";
pub const CONTAINERD_VERSION: &str = "v2.3.1";
pub const K8S_VERSION: &str = "v1.36.1";
pub const CILIUM_VERSION: &str = "v0.19.4";

pub const CONFIG_FILENAME: &str = "config.toml";

pub const RUNC_INSTALL_PATH: &str = "/usr/local/sbin/runc";
pub const CNI_INSTALL_DIR: &str = "/opt/cni/bin";
pub const CONTAINERD_INSTALL_DIR: &str = "/usr/local";
pub const K8S_INSTALL_DIR: &str = "/usr/local/bin";
pub const CILIUM_INSTALL_DIR: &str = "/usr/local/bin";

pub const RUNC_URL_BASE: &str = "https://github.com/opencontainers/runc/releases/download";
pub const CNI_URL_BASE: &str = "https://github.com/containernetworking/plugins/releases/download";
pub const CONTAINERD_URL_BASE: &str = "https://github.com/containerd/containerd/releases/download";
pub const K8S_URL_BASE: &str = "https://dl.k8s.io";
pub const CILIUM_URL_BASE: &str = "https://github.com/cilium/cilium-cli/releases/download";

pub const SYSCTL_CONF_PATH: &str = "/etc/sysctl.d/99-cisak.conf";
pub const SYSCTL_CONF_CONTENT: &str = "net.ipv4.ip_forward = 1\n";

/// Kubernetes binaries to install. Each tuple is `(binary name, install-dir constant)`.
///
/// The download URLs are assembled as:
/// `<K8S_URL_BASE>/<version>/bin/linux/amd64/<name>`
/// `<K8S_URL_BASE>/<version>/bin/linux/amd64/<name>.sha256`
pub const K8S_BINARIES: &[(&str, &str)] = &[
    ("kubelet", KUBELET_INSTALL_DIR), // /usr/bin
    ("kubeadm", K8S_INSTALL_DIR),     // /usr/local/bin
    ("kubectl", K8S_INSTALL_DIR),     // /usr/local/bin
];

pub const KUBELET_SERVICE_URL: &str =
    "https://raw.githubusercontent.com/kubernetes/release/master/\
     cmd/krel/templates/latest/kubelet/kubelet.service";
pub const KUBEADM_CONF_URL: &str = "https://raw.githubusercontent.com/kubernetes/release/master/\
     cmd/krel/templates/latest/kubeadm/10-kubeadm.conf";

/// Where `kubelet.service` is installed on a systemd host.
pub const KUBELET_SERVICE_PATH: &str = "/usr/lib/systemd/system/kubelet.service";
/// Drop-in that `kubeadm` uses to inject environment variables into kubelet.
pub const KUBEADM_DROP_IN_PATH: &str = "/etc/systemd/system/kubelet.service.d/10-kubeadm.conf";

pub const KUBELET_INSTALL_DIR: &str = "/usr/bin";
