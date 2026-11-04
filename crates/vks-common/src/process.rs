//! Hardened subprocess spawning and environment scrubbing for media pipelines.
//!
//! Prevents credential and secret leakage (`VKS_JWT_SECRET`, `VKS_DATABASE_URL`,
//! tokens) into child processes (e.g. FFmpeg) and ensures child termination via
//! `PR_SET_PDEATHSIG` and dedicated process groups.

use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Returns the configured FFmpeg executable path from `VKS_FFMPEG_BIN`
/// or defaults to `"ffmpeg"`.
#[must_use]
pub fn ffmpeg_bin_path() -> PathBuf {
    std::env::var_os("VKS_FFMPEG_BIN").map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from)
}

/// Clears the process environment and restores only the minimal safe variables
/// needed for FFmpeg / external tools to run correctly.
pub fn scrubbed_env(command: &mut Command) -> &mut Command {
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()));
    if let Some(loader) = std::env::var_os("LD_LIBRARY_PATH") {
        command.env("LD_LIBRARY_PATH", loader);
    }
    command
}

/// Creates a hardened `tokio::process::Command` for executing `program`.
///
/// Features:
/// 1. Environment scrubbing (`env_clear`) preventing credential leakage.
/// 2. `kill_on_drop(true)`.
/// 3. `process_group(0)` so child is leader of its own PGID.
/// 4. Linux `PR_SET_PDEATHSIG` (SIGKILL) with parent check inside `pre_exec`.
pub fn safe_command(program: impl AsRef<Path>) -> Command {
    let mut command = Command::new(program.as_ref());
    command.kill_on_drop(true);
    scrubbed_env(&mut command);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            let parent = libc::getppid();
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                libc::_exit(1);
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    command
}

/// Hardened FFmpeg command constructor using `ffmpeg_bin_path()`.
#[must_use]
pub fn safe_ffmpeg_command() -> Command {
    safe_command(ffmpeg_bin_path())
}

/// Kills an entire process group by PGID (`kill(-pgid, SIGKILL)`).
///
/// Guarded against `pid <= 1` to prevent killing parent or init process.
pub fn kill_process_group(pid: u32) {
    if pid <= 1 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_path_defaults_or_env() {
        let p = ffmpeg_bin_path();
        assert!(!p.as_os_str().is_empty());

        std::env::set_var("VKS_FFMPEG_BIN", "/opt/custom/bin/ffmpeg");
        let p_custom = ffmpeg_bin_path();
        assert_eq!(p_custom, PathBuf::from("/opt/custom/bin/ffmpeg"));
        std::env::remove_var("VKS_FFMPEG_BIN");
    }

    #[tokio::test]
    async fn safe_command_scrubs_env() {
        std::env::set_var("VKS_SECRET_LEAK_TEST", "super_secret_value");
        let mut cmd = safe_command("printenv");
        let output = cmd.output().await;
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                !stdout.contains("VKS_SECRET_LEAK_TEST"),
                "Environment variable leaked into child process!"
            );
        }
    }

    #[test]
    fn kill_invalid_pid_is_noop() {
        kill_process_group(0);
        kill_process_group(1);
    }
}
