//! Health checking for the daemon process
//!
//! Periodically checks if the daemon is responsive via HTTP endpoint.

use crate::visor::config::VisorConfig;
use std::time::Duration;
use thiserror::Error;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check failed: {0}")]
    CheckFailed(String),
    #[error("health check timed out")]
    Timeout,
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),
}

/// Health check result
#[derive(Debug, Clone)]
pub struct HealthStatus {
    /// Whether the daemon is healthy
    pub healthy: bool,
    /// Response time in milliseconds
    pub response_time_ms: u64,
    /// Optional status message
    pub message: Option<String>,
    /// Block height if available
    pub block_height: Option<u64>,
    /// View number if available
    pub view: Option<u64>,
}

/// Health checker for the daemon
pub struct HealthChecker {
    config: VisorConfig,
    client: reqwest::Client,
    consecutive_failures: u32,
}

impl HealthChecker {
    /// Create a new health checker
    pub fn new(config: VisorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.health_check_timeout_ms))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            consecutive_failures: 0,
        }
    }

    /// Perform a single health check
    pub async fn check(&mut self) -> Result<HealthStatus, HealthError> {
        let start = std::time::Instant::now();

        let response = self
            .client
            .get(&self.config.health_check_endpoint)
            .send()
            .await?;

        let response_time_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            self.consecutive_failures += 1;
            return Err(HealthError::CheckFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }

        // Try to parse response body for additional info
        let (block_height, view, message) = match response.json::<serde_json::Value>().await {
            Ok(json) => {
                let height = json.get("height").and_then(|v| v.as_u64());
                let view = json.get("view").and_then(|v| v.as_u64());
                let msg = json
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                (height, view, msg)
            }
            Err(_) => (None, None, None),
        };

        self.consecutive_failures = 0;

        Ok(HealthStatus {
            healthy: true,
            response_time_ms,
            message,
            block_height,
            view,
        })
    }

    /// Get the number of consecutive failures
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Reset failure counter
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Check if daemon is considered unhealthy (too many failures)
    pub fn is_unhealthy(&self, threshold: u32) -> bool {
        self.consecutive_failures >= threshold
    }

    /// Run continuous health checking
    ///
    /// Returns when the daemon becomes unhealthy or the stop signal is received.
    pub async fn run(
        &mut self,
        mut stop_rx: tokio::sync::mpsc::Receiver<()>,
        status_tx: tokio::sync::mpsc::Sender<HealthStatus>,
    ) {
        let mut ticker = interval(Duration::from_millis(self.config.health_check_interval_ms));
        let unhealthy_threshold = 3; // Configurable in future

        loop {
            tokio::select! {
                _ = stop_rx.recv() => {
                    info!("Health checker stopping");
                    return;
                }
                _ = ticker.tick() => {
                    match self.check().await {
                        Ok(status) => {
                            debug!(
                                "Health check OK: {}ms, height={:?}, view={:?}",
                                status.response_time_ms,
                                status.block_height,
                                status.view
                            );
                            let _ = status_tx.send(status).await;
                        }
                        Err(e) => {
                            warn!(
                                "Health check failed ({}/{}): {}",
                                self.consecutive_failures,
                                unhealthy_threshold,
                                e
                            );

                            let status = HealthStatus {
                                healthy: false,
                                response_time_ms: 0,
                                message: Some(e.to_string()),
                                block_height: None,
                                view: None,
                            };
                            let _ = status_tx.send(status).await;

                            if self.is_unhealthy(unhealthy_threshold) {
                                error!("Daemon is unhealthy after {} consecutive failures",
                                    self.consecutive_failures);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Response from the /health endpoint
#[derive(Debug, serde::Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub view: Option<u64>,
    #[serde(default)]
    pub peers: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_status() {
        let status = HealthStatus {
            healthy: true,
            response_time_ms: 50,
            message: Some("ok".to_string()),
            block_height: Some(100),
            view: Some(200),
        };

        assert!(status.healthy);
        assert_eq!(status.response_time_ms, 50);
    }

    #[test]
    fn test_unhealthy_threshold() {
        let config = VisorConfig::default();
        let mut checker = HealthChecker::new(config);

        assert!(!checker.is_unhealthy(3));

        checker.consecutive_failures = 2;
        assert!(!checker.is_unhealthy(3));

        checker.consecutive_failures = 3;
        assert!(checker.is_unhealthy(3));
    }
}
