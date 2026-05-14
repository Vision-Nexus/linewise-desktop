use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Run a command, inheriting stdio. Bails on non-zero exit.
pub fn run<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let pretty = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("$ {program} {pretty}");
    let status = Command::new(program)
        .args(&args)
        .status()
        .with_context(|| format!("failed to spawn {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Run a command and capture stdout as a UTF-8 string.
pub fn capture<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = run_capture(program, args)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_capture<I, S>(program: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(program)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to spawn {program}"))?;
    if !out.status.success() {
        bail!("{program} exited with {}", out.status);
    }
    Ok(out)
}

/// True if `program` is found on PATH.
pub fn which(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Like `which`, but also accepts tools that respond to `-version` instead
/// of `--version` (iconutil, install_name_tool).
pub fn which_any(program: &str) -> bool {
    if which(program) {
        return true;
    }
    Command::new(program)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success() || s.code() == Some(1))
        .unwrap_or(false)
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p).with_context(|| format!("create_dir_all({})", p.display()))
}
