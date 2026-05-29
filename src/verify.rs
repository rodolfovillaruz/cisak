use crate::helper::run_output;
use anyhow::{Context, Result};
use std::{fs, path::Path, process::Command as ProcCommand};

// ── Checksum verification ─────────────────────────────────────────────────────

/// Verify a SHA-512 checksum file produced by `sha512sum`.
pub fn verify_sha512(tgz: &Path, sha512_file: &Path, assume_yes: bool) -> Result<()> {
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
pub fn verify_sha256(tgz: &Path, sha256_file: &Path, assume_yes: bool) -> Result<()> {
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
pub fn verify_k8s_sha256(binary: &Path, sha256_file: &Path, assume_yes: bool) -> Result<()> {
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

/// Verify a detached GPG signature.
pub fn verify_gpg_signature(bin: &Path, sig: &Path, assume_yes: bool) -> Result<()> {
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
