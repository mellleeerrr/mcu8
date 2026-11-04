//! Hardened subprocess spawning and environment scrubbing for media pipelines.
//!
//! Prevents credential and secret leakage (`VKS_JWT_SECRET`, `VKS_DATABASE_URL`,
//! tokens) into child processes (e.g. FFmpeg) and ensures child termination via
//! `PR_SET_PDEATHSIG` and dedicated process groups.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Hierarchical process budget separating inbound SIP decoders, encoder farm groups,
/// and standalone transcoder workers to prevent cross-domain starvation.
#[derive(Debug, Clone)]
pub struct ProcessBudget {
    decoders: Arc<Semaphore>,
    farm_groups: Arc<Semaphore>,
    workers: Arc<Semaphore>,
    max_decoders: usize,
    max_farm_groups: usize,
    max_workers: usize,
}

impl ProcessBudget {
    /// Creates a process budget with explicit capacity for each domain.
    #[must_use]
    pub fn new(max_decoders: usize, max_farm_groups: usize, max_workers: usize) -> Self {
        let max_decoders = max_decoders.max(1);
        let max_farm_groups = max_farm_groups.max(1);
        let max_workers = max_workers.max(1);
        Self {
            decoders: Arc::new(Semaphore::new(max_decoders)),
            farm_groups: Arc::new(Semaphore::new(max_farm_groups)),
            workers: Arc::new(Semaphore::new(max_workers)),
            max_decoders,
            max_farm_groups,
            max_workers,
        }
    }

    /// Reads limits from standard environment variables with production defaults:
    /// - `VKS_MAX_VIDEO_DECODERS` (default 128)
    /// - `VKS_VIDEO_FARM_MAX_GROUPS` (default 128)
    /// - `VKS_CODEC_WORKER_POOL` (default 8)
    #[must_use]
    pub fn from_env() -> Self {
        let decoders = std::env::var("VKS_MAX_VIDEO_DECODERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(128);
        let farm_groups = std::env::var("VKS_VIDEO_FARM_MAX_GROUPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(128);
        let workers = std::env::var("VKS_CODEC_WORKER_POOL")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(8);
        Self::new(decoders, farm_groups, workers)
    }

    /// Non-blocking attempt to acquire a slot for an inbound video decoder.
    pub fn try_acquire_decoder(&self) -> Option<OwnedSemaphorePermit> {
        self.decoders.clone().try_acquire_owned().ok()
    }

    /// Non-blocking attempt to acquire a slot for an encoder farm group.
    pub fn try_acquire_farm_group(&self) -> Option<OwnedSemaphorePermit> {
        self.farm_groups.clone().try_acquire_owned().ok()
    }

    /// Non-blocking attempt to acquire a slot for an isolated codec worker.
    pub fn try_acquire_worker(&self) -> Option<OwnedSemaphorePermit> {
        self.workers.clone().try_acquire_owned().ok()
    }

    /// Configured maximum capacity for inbound decoders.
    #[must_use]
    pub fn max_decoders(&self) -> usize {
        self.max_decoders
    }

    /// Configured maximum capacity for encoder farm groups.
    #[must_use]
    pub fn max_farm_groups(&self) -> usize {
        self.max_farm_groups
    }

    /// Configured maximum capacity for transcoder workers.
    #[must_use]
    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    /// Currently available permits for decoders.
    #[must_use]
    pub fn available_decoders(&self) -> usize {
        self.decoders.available_permits()
    }

    /// Currently available permits for farm groups.
    #[must_use]
    pub fn available_farm_groups(&self) -> usize {
        self.farm_groups.available_permits()
    }

    /// Currently available permits for workers.
    #[must_use]
    pub fn available_workers(&self) -> usize {
        self.workers.available_permits()
    }
}

impl Default for ProcessBudget {
    fn default() -> Self {
        Self::from_env()
    }
}

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

    #[test]
    fn process_budget_enforces_limits_and_raii_return() {
        let budget = ProcessBudget::new(2, 2, 1);
        assert_eq!(budget.available_decoders(), 2);
        assert_eq!(budget.max_decoders(), 2);

        let p1 = budget.try_acquire_decoder();
        assert!(p1.is_some());
        assert_eq!(budget.available_decoders(), 1);

        let p2 = budget.try_acquire_decoder();
        assert!(p2.is_some());
        assert_eq!(budget.available_decoders(), 0);

        let p3 = budget.try_acquire_decoder();
        assert!(p3.is_none(), "decoder quota exceeded must reject");

        drop(p1);
        assert_eq!(budget.available_decoders(), 1, "dropping permit must restore quota");

        let p4 = budget.try_acquire_decoder();
        assert!(p4.is_some());
        assert_eq!(budget.available_decoders(), 0);
    }

    #[test]
    fn process_budget_domains_are_isolated() {
        let budget = ProcessBudget::new(1, 1, 1);

        let d = budget.try_acquire_decoder().unwrap();
        assert_eq!(budget.available_decoders(), 0);

        // Farm groups and workers must remain unaffected by decoder saturation.
        assert_eq!(budget.available_farm_groups(), 1);
        assert_eq!(budget.available_workers(), 1);

        let f = budget.try_acquire_farm_group().unwrap();
        assert_eq!(budget.available_farm_groups(), 0);
        assert_eq!(budget.available_workers(), 1);

        let w = budget.try_acquire_worker().unwrap();
        assert_eq!(budget.available_workers(), 0);

        drop(d);
        drop(f);
        drop(w);
        assert_eq!(budget.available_decoders(), 1);
        assert_eq!(budget.available_farm_groups(), 1);
        assert_eq!(budget.available_workers(), 1);
    }
}
