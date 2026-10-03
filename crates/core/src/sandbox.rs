//! Runs untrusted commands (tests, regression scripts) away from the host.
//! Docker mode: no network, no host files except the mounted directory, dropped capabilities,
//! resource limits. Local mode runs the command directly and is clearly labeled as unisolated.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Docker,
    Local,
}

fn image() -> String {
    std::env::var("AUTORESOLVE_DOCKER_IMAGE").unwrap_or_else(|_| "python:3.12-slim".into())
}

fn docker_works() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// AUTORESOLVE_SANDBOX=docker | local. Unset: Docker when it works, otherwise Local with a loud warning.
pub fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("AUTORESOLVE_SANDBOX").as_deref() {
        Ok("local") => Mode::Local,
        // forced: if Docker is missing, commands fail instead of silently running unisolated
        Ok("docker") => Mode::Docker,
        _ => {
            if docker_works() {
                Mode::Docker
            } else {
                eprintln!(
                    "[sandbox] WARNING: Docker is not usable (installed? are you in the docker group?); \
                     running commands WITHOUT isolation"
                );
                Mode::Local
            }
        }
    })
}

pub fn describe() -> String {
    match mode() {
        Mode::Docker => format!(
            "docker, image {}, no network, 1 cpu, 512 MB, read-only root filesystem",
            image()
        ),
        Mode::Local => "LOCAL: no isolation, commands run directly on this machine".to_string(),
    }
}

fn docker_args(dir: &Path, cmd: &str, read_only: bool, uid: u32, gid: u32, image: &str) -> Vec<String> {
    let mount = format!("{}:/work{}", dir.display(), if read_only { ":ro" } else { "" });
    let mut args: Vec<String> = [
        "run", "--rm", "--init",
        "--network", "none",
        "--memory", "512m", "--memory-swap", "512m",
        "--cpus", "1",
        "--pids-limit", "128",
        "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges",
        "--read-only",
        "--tmpfs", "/tmp:rw,size=64m",
        "-e", "HOME=/tmp",
        "-e", "PYTHONDONTWRITEBYTECODE=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend([
        "--user".to_string(),
        format!("{uid}:{gid}"),
        "-v".to_string(),
        mount,
        "-w".to_string(),
        "/work".to_string(),
        image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        cmd.to_string(),
    ]);
    args
}

/// Run `cmd` with `dir` as the working directory. `read_only` mounts it read-only (Docker mode).
/// Returns (success, last 1500 characters of combined output).
pub fn run(dir: &Path, cmd: &str, read_only: bool) -> (bool, String) {
    run_in(mode(), dir, cmd, read_only)
}

fn run_in(mode: Mode, dir: &Path, cmd: &str, read_only: bool) -> (bool, String) {
    let out = match mode {
        Mode::Local => Command::new("timeout")
            .args(["120", "sh", "-c", cmd])
            .current_dir(dir)
            .env_remove("GEMINI_API_KEY") // never hand the API key to code under test
            .output(),
        Mode::Docker => {
            let (uid, gid) = std::fs::metadata(dir)
                .map(|m| (m.uid(), m.gid()))
                .unwrap_or((1000, 1000));
            Command::new("timeout")
                .arg("150")
                .arg("docker")
                .args(docker_args(dir, cmd, read_only, uid, gid, &image()))
                .output()
        }
    };
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let tail: String = text.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            (o.status.success(), tail)
        }
        Err(e) => (false, format!("could not run: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_command_is_locked_down() {
        let a = docker_args(Path::new("/tmp/x"), "echo hi", false, 1000, 1000, "python:3.12-slim").join(" ");
        assert!(a.contains("--network none"));
        assert!(a.contains("--cap-drop ALL"));
        assert!(a.contains("--read-only"));
        assert!(a.contains("--user 1000:1000"));
        assert!(a.contains("-v /tmp/x:/work -w /work"));
        assert!(a.ends_with("python:3.12-slim sh -c echo hi"));
        let ro = docker_args(Path::new("/tmp/x"), "true", true, 1, 1, "img").join(" ");
        assert!(ro.contains("-v /tmp/x:/work:ro -w /work"));
    }

    #[test]
    fn local_mode_runs_and_reports_failure() {
        let d = std::env::temp_dir();
        let (ok, out) = run_in(Mode::Local, &d, "echo hi", false);
        assert!(ok && out.contains("hi"));
        let (ok, _) = run_in(Mode::Local, &d, "exit 3", false);
        assert!(!ok);
    }
}