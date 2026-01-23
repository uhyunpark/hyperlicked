//! Process manager for supervising the daemon process
//!
//! Handles:
//! - Spawning the child process
//! - Capturing stdout/stderr to log files
//! - Automatic restart on crash with exponential backoff
//! - Graceful shutdown handling

use crate::visor::config::VisorConfig;
use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("binary not found at {0}")]
    BinaryNotFound(PathBuf),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[from] io::Error),
    #[error("process already running")]
    AlreadyRunning,
    #[error("process not running")]
    NotRunning,
    #[error("max restart attempts exceeded")]
    MaxRestartsExceeded,
}

/// Exit status from the child process
#[derive(Debug, Clone)]
pub enum ExitStatus {
    /// Process exited normally with code
    Exited(i32),
    /// Process was killed by signal
    Signal(i32),
    /// Process was stopped by visor
    Stopped,
    /// Unknown exit status
    Unknown,
}

/// Process manager for the daemon
pub struct ProcessManager {
    config: VisorConfig,
    child: Option<Child>,
    restart_count: u32,
    last_restart: Option<SystemTime>,
    shutdown: Arc<AtomicBool>,
    is_validator: bool,
}

impl ProcessManager {
    /// Create a new process manager
    pub fn new(config: VisorConfig, is_validator: bool) -> Self {
        Self {
            config,
            child: None,
            restart_count: 0,
            last_restart: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            is_validator,
        }
    }

    /// Get a shutdown signal handle
    pub fn shutdown_signal(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Check if the process is currently running
    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    false
                }
                Ok(None) => true,
                Err(_) => {
                    self.child = None;
                    false
                }
            }
        } else {
            false
        }
    }

    /// Get the PID of the running process
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Spawn the daemon process
    pub fn spawn(&mut self) -> Result<(), ProcessError> {
        if self.is_running() {
            return Err(ProcessError::AlreadyRunning);
        }

        let binary_path = self.config.current_binary();
        if !binary_path.exists() {
            return Err(ProcessError::BinaryNotFound(binary_path));
        }

        // Ensure log directory exists
        let _ = self.config.ensure_dirs();

        // Open log files
        let stdout_path = self.config.logs_dir().join("child_stdout.log");
        let stderr_path = self.config.logs_dir().join("child_stderr.log");

        let stdout_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stdout_path)?;

        let stderr_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_path)?;

        // Build command
        let mut cmd = Command::new(&binary_path);

        // Add validator flag if needed
        if self.is_validator {
            // The actual validator flag will depend on hl-server implementation
            cmd.env("VALIDATOR_MODE", "true");
        }

        // Add extra arguments
        cmd.args(&self.config.daemon_args);

        // Add environment variables
        for (key, value) in &self.config.daemon_env {
            cmd.env(key, value);
        }

        // Set data directory
        cmd.env("DATA_DIR", self.config.data_dir());

        // Redirect output
        cmd.stdout(Stdio::from(stdout_file));
        cmd.stderr(Stdio::from(stderr_file));

        info!(
            "Spawning daemon: {} (validator={})",
            binary_path.display(),
            self.is_validator
        );

        let child = cmd.spawn()?;
        let pid = child.id();

        self.child = Some(child);
        self.last_restart = Some(SystemTime::now());

        info!("Daemon started with PID {}", pid);
        Ok(())
    }

    /// Stop the daemon process gracefully
    pub async fn stop(&mut self, timeout: Duration) -> Result<ExitStatus, ProcessError> {
        let child = self.child.as_mut().ok_or(ProcessError::NotRunning)?;
        let pid = child.id();

        info!("Stopping daemon (PID {})", pid);

        // Send SIGTERM
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }

        #[cfg(not(unix))]
        {
            let _ = child.kill();
        }

        // Wait for graceful shutdown
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    let exit_status = if let Some(code) = status.code() {
                        ExitStatus::Exited(code)
                    } else {
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::ExitStatusExt;
                            if let Some(signal) = status.signal() {
                                ExitStatus::Signal(signal)
                            } else {
                                ExitStatus::Unknown
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            ExitStatus::Unknown
                        }
                    };
                    info!("Daemon stopped: {:?}", exit_status);
                    return Ok(exit_status);
                }
                Ok(None) => {
                    if start.elapsed() > timeout {
                        warn!("Daemon did not stop gracefully, sending SIGKILL");
                        let _ = child.kill();
                        let _ = child.wait();
                        self.child = None;
                        return Ok(ExitStatus::Signal(9));
                    }
                    sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!("Error waiting for daemon: {}", e);
                    self.child = None;
                    return Ok(ExitStatus::Unknown);
                }
            }
        }
    }

    /// Restart the daemon
    pub async fn restart(&mut self) -> Result<(), ProcessError> {
        if self.is_running() {
            self.stop(Duration::from_secs(10)).await?;
        }

        // Apply restart delay with exponential backoff
        let delay = self.calculate_restart_delay();
        if delay > Duration::ZERO {
            info!("Waiting {:?} before restart", delay);
            sleep(delay).await;
        }

        self.restart_count += 1;
        self.spawn()
    }

    /// Calculate restart delay with exponential backoff
    fn calculate_restart_delay(&self) -> Duration {
        if self.restart_count == 0 {
            return Duration::ZERO;
        }

        // Check if enough time has passed to reset the counter
        if let Some(last) = self.last_restart {
            if let Ok(elapsed) = last.elapsed() {
                // Reset counter if process ran for > 5 minutes
                if elapsed > Duration::from_secs(300) {
                    return Duration::from_millis(self.config.restart_delay_ms);
                }
            }
        }

        // Exponential backoff: delay * 2^(restart_count - 1), capped at 60 seconds
        let base = self.config.restart_delay_ms;
        let multiplier = 2u64.pow(self.restart_count.min(6));
        let delay_ms = (base * multiplier).min(60_000);
        Duration::from_millis(delay_ms)
    }

    /// Run the supervision loop
    pub async fn run(&mut self, mut stop_rx: mpsc::Receiver<()>) -> Result<(), ProcessError> {
        // Initial spawn
        self.spawn()?;

        loop {
            tokio::select! {
                _ = stop_rx.recv() => {
                    info!("Received stop signal");
                    self.shutdown.store(true, Ordering::SeqCst);
                    self.stop(Duration::from_secs(30)).await?;
                    return Ok(());
                }
                _ = self.wait_for_exit() => {
                    if self.shutdown.load(Ordering::SeqCst) {
                        return Ok(());
                    }

                    // Process crashed
                    self.write_crash_report().await;

                    if self.restart_count >= self.config.max_restart_attempts {
                        error!("Max restart attempts ({}) exceeded", self.config.max_restart_attempts);
                        return Err(ProcessError::MaxRestartsExceeded);
                    }

                    warn!("Daemon exited unexpectedly, restarting (attempt {}/{})",
                        self.restart_count + 1,
                        self.config.max_restart_attempts
                    );

                    self.restart().await?;
                }
            }
        }
    }

    /// Wait for the child process to exit
    async fn wait_for_exit(&mut self) {
        loop {
            if !self.is_running() {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    /// Write a crash report
    async fn write_crash_report(&self) {
        let crash_dir = self.config.logs_dir().join("crash_reports");
        let _ = std::fs::create_dir_all(&crash_dir);

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let report_path = crash_dir.join(format!("crash_{}.txt", timestamp));

        let report = format!(
            "Crash Report\n\
             ============\n\
             Time: {}\n\
             Restart Count: {}\n\
             Binary: {}\n\
             Validator Mode: {}\n",
            chrono_lite(timestamp),
            self.restart_count,
            self.config.current_binary().display(),
            self.is_validator,
        );

        if let Err(e) = std::fs::write(&report_path, report) {
            error!("Failed to write crash report: {}", e);
        } else {
            info!("Crash report written to {}", report_path.display());
        }
    }

    /// Reset restart counter (call after successful health check)
    pub fn reset_restart_count(&mut self) {
        if self.restart_count > 0 {
            debug!("Resetting restart counter");
            self.restart_count = 0;
        }
    }
}

/// Simple timestamp formatting without external dependency
fn chrono_lite(secs: u64) -> String {
    // Very basic UTC timestamp
    let days_since_epoch = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Approximate year/month/day (good enough for logs)
    let year = 1970 + (days_since_epoch / 365);
    let day_of_year = days_since_epoch % 365;
    let month = day_of_year / 30 + 1;
    let day = day_of_year % 30 + 1;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hours, minutes, seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_delay_calculation() {
        let config = VisorConfig {
            restart_delay_ms: 1000,
            ..Default::default()
        };

        let mut pm = ProcessManager::new(config, false);

        // First restart: base delay
        pm.restart_count = 1;
        let delay = pm.calculate_restart_delay();
        assert_eq!(delay, Duration::from_secs(2)); // 1000 * 2

        // Second restart: 4x base
        pm.restart_count = 2;
        let delay = pm.calculate_restart_delay();
        assert_eq!(delay, Duration::from_secs(4)); // 1000 * 4
    }
}
