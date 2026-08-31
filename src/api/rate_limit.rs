//! REST API Rate Limiting (CRITICAL-5)
//!
//! IP-based rate limiting for REST API endpoints.
//!
//! ## Configuration
//!
//! - Trading endpoints (orders, cancels): 100 req/min
//! - Read endpoints (orderbook, account): 1000 req/min
//!
//! ## Headers
//!
//! When rate limited, returns 429 Too Many Requests with:
//! - `Retry-After`: Seconds until rate limit resets
//! - `X-RateLimit-Limit`: Maximum requests allowed per window
//! - `X-RateLimit-Remaining`: Requests remaining in current window

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::middleware::Next;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Rate limit configuration per endpoint type
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum requests per window
    pub max_requests: u32,
    /// Window duration
    pub window: Duration,
}

impl RateLimitConfig {
    /// Default config for trading endpoints (100 req/min)
    pub fn trading() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
        }
    }

    /// Default config for read endpoints (1000 req/min)
    pub fn read() -> Self {
        Self {
            max_requests: 1000,
            window: Duration::from_secs(60),
        }
    }

    /// Default config for heavy endpoints like sync (20 req/min)
    pub fn heavy() -> Self {
        Self {
            max_requests: 20,
            window: Duration::from_secs(60),
        }
    }
}

/// Sliding window rate limit entry
#[derive(Debug)]
struct RateLimitEntry {
    /// Timestamps of requests in current window
    timestamps: Vec<Instant>,
}

impl RateLimitEntry {
    fn new() -> Self {
        Self {
            timestamps: Vec::new(),
        }
    }

    /// Check if request is allowed and record it if so
    fn check_and_record(&mut self, config: &RateLimitConfig, now: Instant) -> bool {
        // Remove timestamps outside the window
        let window_start = now - config.window;
        self.timestamps.retain(|&t| t > window_start);

        // Check if under limit
        if self.timestamps.len() >= config.max_requests as usize {
            return false;
        }

        // Record this request
        self.timestamps.push(now);
        true
    }

    /// Get remaining requests in current window
    fn remaining(&self, config: &RateLimitConfig, now: Instant) -> u32 {
        let window_start = now - config.window;
        let recent_count = self
            .timestamps
            .iter()
            .filter(|&&t| t > window_start)
            .count();
        config.max_requests.saturating_sub(recent_count as u32)
    }

    /// Get seconds until oldest request expires from window
    fn retry_after(&self, config: &RateLimitConfig, now: Instant) -> u64 {
        let window_start = now - config.window;
        let oldest_in_window = self
            .timestamps
            .iter()
            .filter(|&&t| t > window_start)
            .min()
            .copied();

        match oldest_in_window {
            Some(oldest) => {
                let expire_at = oldest + config.window;
                if expire_at > now {
                    (expire_at - now).as_secs() + 1
                } else {
                    1
                }
            }
            None => 1,
        }
    }
}

/// Rate limiter state shared across requests
#[derive(Debug)]
pub struct RateLimiter {
    /// Per-IP limits for trading endpoints
    trading_limits: RwLock<HashMap<IpAddr, RateLimitEntry>>,
    /// Per-IP limits for read endpoints
    read_limits: RwLock<HashMap<IpAddr, RateLimitEntry>>,
    /// Per-IP limits for heavy endpoints
    heavy_limits: RwLock<HashMap<IpAddr, RateLimitEntry>>,
    /// Trading config
    trading_config: RateLimitConfig,
    /// Read config
    read_config: RateLimitConfig,
    /// Heavy config
    heavy_config: RateLimitConfig,
    /// Last cleanup time
    last_cleanup: RwLock<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter with default configs
    pub fn new() -> Self {
        Self {
            trading_limits: RwLock::new(HashMap::new()),
            read_limits: RwLock::new(HashMap::new()),
            heavy_limits: RwLock::new(HashMap::new()),
            trading_config: RateLimitConfig::trading(),
            read_config: RateLimitConfig::read(),
            heavy_config: RateLimitConfig::heavy(),
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    /// Check rate limit for trading endpoint
    pub async fn check_trading(&self, ip: IpAddr) -> RateLimitResult {
        self.check_impl(&self.trading_limits, ip, &self.trading_config)
            .await
    }

    /// Check rate limit for read endpoint
    pub async fn check_read(&self, ip: IpAddr) -> RateLimitResult {
        self.check_impl(&self.read_limits, ip, &self.read_config)
            .await
    }

    /// Check rate limit for heavy endpoint
    pub async fn check_heavy(&self, ip: IpAddr) -> RateLimitResult {
        self.check_impl(&self.heavy_limits, ip, &self.heavy_config)
            .await
    }

    async fn check_impl(
        &self,
        limits: &RwLock<HashMap<IpAddr, RateLimitEntry>>,
        ip: IpAddr,
        config: &RateLimitConfig,
    ) -> RateLimitResult {
        let now = Instant::now();

        // Periodic cleanup (every 5 minutes)
        if now.duration_since(*self.last_cleanup.read().await) > Duration::from_secs(300) {
            self.cleanup().await;
        }

        let mut limits = limits.write().await;
        let entry = limits.entry(ip).or_insert_with(RateLimitEntry::new);

        if entry.check_and_record(config, now) {
            RateLimitResult::Allowed {
                remaining: entry.remaining(config, now),
                limit: config.max_requests,
            }
        } else {
            RateLimitResult::Limited {
                retry_after: entry.retry_after(config, now),
                limit: config.max_requests,
            }
        }
    }

    /// Cleanup old entries to prevent unbounded memory growth
    async fn cleanup(&self) {
        let now = Instant::now();
        let mut last_cleanup = self.last_cleanup.write().await;

        // Only one cleanup at a time
        if now.duration_since(*last_cleanup) < Duration::from_secs(300) {
            return;
        }
        *last_cleanup = now;
        drop(last_cleanup);

        // Clean up trading limits
        {
            let mut limits = self.trading_limits.write().await;
            let window = self.trading_config.window;
            limits.retain(|_, entry| {
                entry.timestamps.retain(|&t| now.duration_since(t) < window);
                !entry.timestamps.is_empty()
            });
        }

        // Clean up read limits
        {
            let mut limits = self.read_limits.write().await;
            let window = self.read_config.window;
            limits.retain(|_, entry| {
                entry.timestamps.retain(|&t| now.duration_since(t) < window);
                !entry.timestamps.is_empty()
            });
        }

        // Clean up heavy limits
        {
            let mut limits = self.heavy_limits.write().await;
            let window = self.heavy_config.window;
            limits.retain(|_, entry| {
                entry.timestamps.retain(|&t| now.duration_since(t) < window);
                !entry.timestamps.is_empty()
            });
        }

        debug!("Rate limiter cleanup complete");
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of rate limit check
#[derive(Debug)]
pub enum RateLimitResult {
    /// Request is allowed
    Allowed { remaining: u32, limit: u32 },
    /// Request is rate limited
    Limited { retry_after: u64, limit: u32 },
}

/// Shared rate limiter state
pub type SharedRateLimiter = Arc<RateLimiter>;

/// Extract client IP from request (considers X-Forwarded-For for reverse proxies)
pub fn extract_client_ip(req: &Request) -> Option<IpAddr> {
    // Check X-Forwarded-For header first (for reverse proxy setups)
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(s) = forwarded.to_str() {
            // Take the first IP in the chain
            if let Some(first_ip) = s.split(',').next() {
                if let Ok(ip) = first_ip.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }

    // Check X-Real-IP header
    if let Some(real_ip) = req.headers().get("x-real-ip") {
        if let Ok(s) = real_ip.to_str() {
            if let Ok(ip) = s.trim().parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }

    None
}

/// Middleware for trading endpoints (100 req/min)
pub async fn rate_limit_trading(
    State(rate_limiter): State<SharedRateLimiter>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let ip = extract_client_ip(&req).unwrap_or(addr.ip());

    match rate_limiter.check_trading(ip).await {
        RateLimitResult::Allowed { remaining, limit } => {
            let mut response = next.run(req).await;
            add_rate_limit_headers(response.headers_mut(), remaining, limit);
            response
        }
        RateLimitResult::Limited { retry_after, limit } => {
            warn!(ip = %ip, "Rate limited trading request");
            rate_limited_response(retry_after, limit)
        }
    }
}

/// Middleware for read endpoints (1000 req/min)
pub async fn rate_limit_read(
    State(rate_limiter): State<SharedRateLimiter>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let ip = extract_client_ip(&req).unwrap_or(addr.ip());

    match rate_limiter.check_read(ip).await {
        RateLimitResult::Allowed { remaining, limit } => {
            let mut response = next.run(req).await;
            add_rate_limit_headers(response.headers_mut(), remaining, limit);
            response
        }
        RateLimitResult::Limited { retry_after, limit } => {
            warn!(ip = %ip, "Rate limited read request");
            rate_limited_response(retry_after, limit)
        }
    }
}

/// Middleware for heavy endpoints (20 req/min)
pub async fn rate_limit_heavy(
    State(rate_limiter): State<SharedRateLimiter>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let ip = extract_client_ip(&req).unwrap_or(addr.ip());

    match rate_limiter.check_heavy(ip).await {
        RateLimitResult::Allowed { remaining, limit } => {
            let mut response = next.run(req).await;
            add_rate_limit_headers(response.headers_mut(), remaining, limit);
            response
        }
        RateLimitResult::Limited { retry_after, limit } => {
            warn!(ip = %ip, "Rate limited heavy request");
            rate_limited_response(retry_after, limit)
        }
    }
}

/// Add rate limit headers to response
fn add_rate_limit_headers(headers: &mut axum::http::HeaderMap, remaining: u32, limit: u32) {
    if let Ok(val) = HeaderValue::from_str(&limit.to_string()) {
        headers.insert("X-RateLimit-Limit", val);
    }
    if let Ok(val) = HeaderValue::from_str(&remaining.to_string()) {
        headers.insert("X-RateLimit-Remaining", val);
    }
}

/// Create 429 Too Many Requests response
fn rate_limited_response(retry_after: u64, limit: u32) -> Response<Body> {
    let body = serde_json::json!({
        "error": "Too Many Requests",
        "code": "RATE_LIMITED",
        "retry_after": retry_after,
        "limit": limit,
        "message": format!("Rate limit exceeded. Retry after {} seconds.", retry_after)
    });

    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "application/json")
        .header("Retry-After", retry_after.to_string())
        .header("X-RateLimit-Limit", limit.to_string())
        .header("X-RateLimit-Remaining", "0")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Should allow first request
        let result = limiter.check_trading(ip).await;
        assert!(matches!(result, RateLimitResult::Allowed { .. }));
    }

    #[tokio::test]
    async fn test_rate_limiter_blocks_over_limit() {
        // Create limiter with very low limit for testing
        let limiter = RateLimiter {
            trading_config: RateLimitConfig {
                max_requests: 2,
                window: Duration::from_secs(60),
            },
            ..RateLimiter::new()
        };

        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // First two should succeed
        assert!(matches!(
            limiter.check_trading(ip).await,
            RateLimitResult::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check_trading(ip).await,
            RateLimitResult::Allowed { .. }
        ));

        // Third should be limited
        let result = limiter.check_trading(ip).await;
        assert!(matches!(result, RateLimitResult::Limited { .. }));
    }

    #[tokio::test]
    async fn test_rate_limiter_per_ip() {
        let limiter = RateLimiter {
            trading_config: RateLimitConfig {
                max_requests: 1,
                window: Duration::from_secs(60),
            },
            ..RateLimiter::new()
        };

        let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        // First request from each IP should succeed
        assert!(matches!(
            limiter.check_trading(ip1).await,
            RateLimitResult::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check_trading(ip2).await,
            RateLimitResult::Allowed { .. }
        ));

        // Second request from each IP should be limited
        assert!(matches!(
            limiter.check_trading(ip1).await,
            RateLimitResult::Limited { .. }
        ));
        assert!(matches!(
            limiter.check_trading(ip2).await,
            RateLimitResult::Limited { .. }
        ));
    }

    #[tokio::test]
    async fn test_remaining_decrements() {
        let limiter = RateLimiter {
            trading_config: RateLimitConfig {
                max_requests: 3,
                window: Duration::from_secs(60),
            },
            ..RateLimiter::new()
        };

        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Check remaining decrements
        if let RateLimitResult::Allowed { remaining, .. } = limiter.check_trading(ip).await {
            assert_eq!(remaining, 2);
        } else {
            panic!("Expected Allowed");
        }

        if let RateLimitResult::Allowed { remaining, .. } = limiter.check_trading(ip).await {
            assert_eq!(remaining, 1);
        } else {
            panic!("Expected Allowed");
        }

        if let RateLimitResult::Allowed { remaining, .. } = limiter.check_trading(ip).await {
            assert_eq!(remaining, 0);
        } else {
            panic!("Expected Allowed");
        }

        // Fourth should be limited
        assert!(matches!(
            limiter.check_trading(ip).await,
            RateLimitResult::Limited { .. }
        ));
    }
}
