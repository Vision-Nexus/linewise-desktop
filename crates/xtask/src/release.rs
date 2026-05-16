//! `cargo xtask release vX.Y.Z[-suffix]` — single-source-of-truth release flow.
//!
//! Walks the workspace through five steps in order:
//!
//!   1. **Validate clean state.** Refuses to run with a dirty working tree —
//!      a release commit shouldn't accidentally pick up unrelated edits.
//!      Skipped under `--allow-dirty` for emergencies.
//!   2. **Rewrite the workspace `[workspace.package].version`** to match
//!      the tag (without the leading `v`). The line edit is intentionally
//!      narrow — we don't want a generic TOML rewrite to reflow comments
//!      or quote styles in unrelated sections.
//!   3. **Refresh `Cargo.lock`** by running `cargo check`. This is the only
//!      way to get the lockfile to record the new workspace version
//!      without depending on the `cargo set-version` plugin.
//!   4. **Create the release commit** that touches exactly `Cargo.toml`
//!      and `Cargo.lock`. Conventional message form: `chore(release): vX.Y.Z`.
//!   5. **Tag the release commit** with `git tag -a v...` so future
//!      `git describe --tags` and the supermodule pointer agree.
//!
//! Push is opt-in via `--push` — the default is local-only so the user can
//! sanity-check the commit before publishing. The CI pipeline can validate
//! "tag matches Cargo.toml at HEAD" as a downstream check; this command
//! makes them agree at creation time.
use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::cmd;

pub fn run(root: &Path, tag: &str, allow_dirty: bool, push: bool) -> Result<()> {
    let version = parse_tag(tag)?;
    eprintln!("Release flow: tag {tag} → version {version}");

    if !allow_dirty {
        ensure_clean_tree(root)?;
    } else {
        eprintln!("warning: --allow-dirty bypassed the clean-tree check");
    }
    ensure_tag_unused(root, tag)?;

    let manifest = root.join("Cargo.toml");
    rewrite_workspace_version(&manifest, &version)?;

    // `cargo check` rewrites Cargo.lock; nothing else publishes the new
    // workspace version into the lockfile reliably without an external
    // plugin.
    cmd::run("cargo", ["check", "--workspace", "--all-targets"])?;

    git_in(root, ["add", "Cargo.toml"])?;
    // `Cargo.lock` may be `.gitignore`d (the convention for library
    // crates and for binaries that intentionally don't pin transitive
    // versions). `git add` would error out on an ignored path; skip
    // the stage step in that case rather than fail the release. The
    // line edit to `Cargo.toml` is the load-bearing change.
    let lockfile = root.join("Cargo.lock");
    if lockfile.exists() && !is_path_ignored(root, "Cargo.lock")? {
        git_in(root, ["add", "Cargo.lock"])?;
    }
    git_in(root, ["commit", "-m", &format!("chore(release): {tag}")])?;
    git_in(root, ["tag", "-a", tag, "-m", &format!("Release {tag}")])?;

    if push {
        git_in(root, ["push", "origin", "HEAD"])?;
        git_in(root, ["push", "origin", tag])?;
    } else {
        eprintln!();
        eprintln!("Release commit and tag staged locally.");
        eprintln!("Push with: git push origin HEAD && git push origin {tag}");
        eprintln!("Or re-run with --push.");
    }

    Ok(())
}

/// `vX.Y.Z` or `vX.Y.Z-suffix` → `X.Y.Z[-suffix]`. We require the leading
/// `v` so a slipped `cargo xtask release 0.0.10` doesn't quietly produce
/// a bare-number tag that breaks `git describe --match 'v*'` queries.
fn parse_tag(tag: &str) -> Result<String> {
    let stripped = tag
        .strip_prefix('v')
        .with_context(|| format!("tag must start with 'v': {tag}"))?;
    if stripped.is_empty() {
        bail!("tag has no version after 'v': {tag}");
    }
    // Light shape check — real semver validation lives in `semver` already
    // depended on for the version-check feature, but pulling it in for
    // xtask is overkill. Catch the obvious typos (spaces, slashes) here.
    if stripped.chars().any(|c| c.is_whitespace() || c == '/') {
        bail!("tag contains illegal characters: {tag}");
    }
    Ok(stripped.to_string())
}

fn ensure_clean_tree(root: &Path) -> Result<()> {
    let status = git_capture_in(root, ["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        bail!(
            "working tree is not clean. Commit or stash first, or pass --allow-dirty:\n{}",
            status.trim()
        );
    }
    Ok(())
}

/// True when `path` (relative to `root`) is matched by a `.gitignore`
/// rule. `git check-ignore` exits 0 when the path is ignored, 1 when
/// it isn't, and >1 on real errors — we map those three states to
/// `Ok(true)`, `Ok(false)`, and `Err`.
fn is_path_ignored(root: &Path, relative: &str) -> Result<bool> {
    use std::process::{Command, Stdio};
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--quiet", relative])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("git check-ignore {relative}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(other) => bail!("git check-ignore exited with {other}"),
        None => bail!("git check-ignore terminated by signal"),
    }
}

fn ensure_tag_unused(root: &Path, tag: &str) -> Result<()> {
    let listing = git_capture_in(root, ["tag", "--list", tag])?;
    if !listing.trim().is_empty() {
        bail!("tag {tag} already exists locally");
    }
    Ok(())
}

/// Surgical edit of the `version = "..."` line that lives directly under
/// `[workspace.package]`. We deliberately avoid a generic TOML rewrite —
/// a `toml_edit` round-trip would reformat the whole file.
fn rewrite_workspace_version(manifest: &Path, new_version: &str) -> Result<()> {
    let content = std::fs::read_to_string(manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    let mut out = String::with_capacity(content.len());
    let mut in_workspace_package = false;
    let mut replaced = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
        }
        if in_workspace_package
            && !replaced
            && let Some(rest) = trimmed.strip_prefix("version")
            && rest.trim_start().starts_with('=')
        {
            out.push_str(&format!("version = \"{new_version}\""));
            out.push('\n');
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        bail!(
            "could not find a `version = ...` line under [workspace.package] in {}",
            manifest.display()
        );
    }
    // `lines()` drops the trailing newline; restore it only if the
    // original ended with one, so our edit doesn't quietly add or
    // remove a final newline.
    if !content.ends_with('\n')
        && let Some(stripped) = out.strip_suffix('\n')
    {
        out = stripped.to_string();
    }
    std::fs::write(manifest, out).with_context(|| format!("write {}", manifest.display()))?;
    eprintln!(
        "rewrote {} → version = \"{new_version}\"",
        manifest.display()
    );
    Ok(())
}

fn git_in<I, S>(root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut full: Vec<std::ffi::OsString> = Vec::new();
    full.push(std::ffi::OsString::from("-C"));
    full.push(root.as_os_str().to_owned());
    for a in args {
        full.push(a.as_ref().to_owned());
    }
    cmd::run("git", full)
}

fn git_capture_in<I, S>(root: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut full: Vec<std::ffi::OsString> = Vec::new();
    full.push(std::ffi::OsString::from("-C"));
    full.push(root.as_os_str().to_owned());
    for a in args {
        full.push(a.as_ref().to_owned());
    }
    cmd::capture("git", full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_tag_strips_v_prefix() {
        assert_eq!(parse_tag("v0.0.9").unwrap(), "0.0.9");
        assert_eq!(parse_tag("v1.2.3-rc4").unwrap(), "1.2.3-rc4");
        assert_eq!(parse_tag("v0.0.9-test").unwrap(), "0.0.9-test");
    }

    #[test]
    fn parse_tag_requires_v_prefix() {
        assert!(parse_tag("0.0.9").is_err());
        assert!(parse_tag("release-0.0.9").is_err());
    }

    #[test]
    fn parse_tag_rejects_whitespace_and_slashes() {
        assert!(parse_tag("v0.0.9 ").is_err());
        assert!(parse_tag("v0.0/9").is_err());
        assert!(parse_tag("v").is_err());
    }

    #[test]
    fn rewrite_replaces_only_workspace_package_version() {
        let dir = tempdir();
        let manifest = dir.join("Cargo.toml");
        let original = r#"[workspace]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
# version = "1" must not be touched
foo = { version = "1", features = ["a"] }
"#;
        std::fs::write(&manifest, original).unwrap();
        rewrite_workspace_version(&manifest, "0.2.5-test").unwrap();
        let updated = std::fs::read_to_string(&manifest).unwrap();
        assert!(updated.contains("version = \"0.2.5-test\"\nedition = \"2024\""));
        assert!(updated.contains("foo = { version = \"1\""));
        assert!(!updated.contains("version = \"0.1.0\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rewrite_preserves_trailing_newline_state() {
        let dir = tempdir();
        let manifest = dir.join("Cargo.toml");
        let no_trailing = "[workspace.package]\nversion = \"0.1.0\"";
        std::fs::write(&manifest, no_trailing).unwrap();
        rewrite_workspace_version(&manifest, "0.2.0").unwrap();
        let updated = std::fs::read_to_string(&manifest).unwrap();
        assert!(!updated.ends_with('\n'));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("xtask-release-test-{}", uuid_like()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{nanos}")
    }
}
