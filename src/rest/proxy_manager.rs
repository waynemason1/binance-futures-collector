use anyhow::Result;
use dashmap::DashMap;
use log::{debug, info, warn};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;

use crate::config::Config;

/// Manages proxy rotation and health tracking
pub struct ProxyManager {
    config: Arc<Config>,
    proxies: Vec<ProxyEndpoint>,
    current_index: AtomicUsize,
    health_stats: Arc<DashMap<String, ProxyHealth>>,
}

#[derive(Debug, Clone)]
pub struct ProxyEndpoint {
    pub name: String,
    pub url: String,
    pub region: String,
}

#[derive(Debug)]
pub struct ProxyHealth {
    pub success_count: AtomicU64,
    pub failure_count: AtomicU64,
    pub last_success: RwLock<Option<Instant>>,
    pub last_failure: RwLock<Option<Instant>>,
    pub health_score: AtomicU8, // 0-100 score instead of binary
    pub last_used: RwLock<Instant>,
}

impl ProxyManager {
    pub fn new(config: Arc<Config>) -> Self {
        let mut proxies = Vec::new();

        if config.proxy.enabled {
            for port in config.proxy.au_ports.iter() {
                proxies.push(ProxyEndpoint {
                    name: format!("AU-{}", port),
                    url: format!(
                        "http://{}:{}@{}:{}",
                        config.proxy.user, config.proxy.password, config.proxy.host, port
                    ),
                    region: "AU".to_string(),
                });
            }

            for port in config.proxy.jp_ports.iter() {
                proxies.push(ProxyEndpoint {
                    name: format!("JP-{}", port),
                    url: format!(
                        "http://{}:{}@{}:{}",
                        config.proxy.user, config.proxy.password, config.proxy.host, port
                    ),
                    region: "JP".to_string(),
                });
            }

            for port in config.proxy.dk_ports.iter() {
                proxies.push(ProxyEndpoint {
                    name: format!("DK-{}", port),
                    url: format!(
                        "http://{}:{}@{}:{}",
                        config.proxy.user, config.proxy.password, config.proxy.host, port
                    ),
                    region: "DK".to_string(),
                });
            }

            for port in config.proxy.uk_ports.iter() {
                proxies.push(ProxyEndpoint {
                    name: format!("UK-{}", port),
                    url: format!(
                        "http://{}:{}@{}:{}",
                        config.proxy.user, config.proxy.password, config.proxy.host, port
                    ),
                    region: "UK".to_string(),
                });
            }
        }

        info!("Initialized proxy manager with {} proxies", proxies.len());

        let health_stats = Arc::new(DashMap::new());
        for proxy in &proxies {
            health_stats.insert(
                proxy.name.clone(),
                ProxyHealth {
                    success_count: AtomicU64::new(0),
                    failure_count: AtomicU64::new(0),
                    last_success: RwLock::new(None),
                    last_failure: RwLock::new(None),
                    health_score: AtomicU8::new(100), // Start at maximum health
                    last_used: RwLock::new(Instant::now()),
                },
            );
        }

        Self {
            config: config.clone(),
            proxies,
            current_index: AtomicUsize::new(0),
            health_stats,
        }
    }

    /// Get next proxy in round-robin fashion, preferring healthier proxies
    pub fn get_next_proxy(&self) -> Option<ProxyEndpoint> {
        if self.proxies.is_empty() {
            return None;
        }

        let mut attempts = 0;
        let max_attempts = self.proxies.len() * 2;

        while attempts < max_attempts {
            let index = self.current_index.fetch_add(1, Ordering::Relaxed) % self.proxies.len();
            let proxy = &self.proxies[index];

            // Check if proxy is healthy (health > 10)
            if let Some(health) = self.health_stats.get(&proxy.name) {
                let score = health.health_score.load(Ordering::Relaxed);
                if score > 10 {
                    debug!("Selected proxy: {} (health: {})", proxy.name, score);
                    *health.last_used.write() = Instant::now();
                    return Some(proxy.clone());
                }
            }

            attempts += 1;
        }

        // If no healthy proxy found, return the next one anyway
        warn!("No healthy proxy found, using next in rotation");
        let index = self.current_index.fetch_add(1, Ordering::Relaxed) % self.proxies.len();
        if let Some(health) = self.health_stats.get(&self.proxies[index].name) {
            *health.last_used.write() = Instant::now();
        }
        Some(self.proxies[index].clone())
    }

    /// Report successful request
    pub fn report_success(&self, proxy_name: &str) {
        if let Some(health) = self.health_stats.get(proxy_name) {
            health.success_count.fetch_add(1, Ordering::Relaxed);
            *health.last_success.write() = Some(Instant::now());
            *health.last_used.write() = Instant::now();

            let old_score = health.health_score.load(Ordering::Relaxed);
            let new_score = std::cmp::min(100, old_score.saturating_add(5));
            health.health_score.store(new_score, Ordering::Relaxed);

            if old_score < 50 && new_score >= 50 {
                info!(
                    "Proxy '{}' recovered to healthy status (health: {}→{})",
                    proxy_name, old_score, new_score
                );
            } else if old_score < 80 && new_score >= 80 {
                info!(
                    "Proxy '{}' fully recovered (health: {}→{})",
                    proxy_name, old_score, new_score
                );
            }
        }
    }

    /// Report failed request with reason
    pub fn report_failure(&self, proxy_name: &str, reason: &str) {
        if let Some(health) = self.health_stats.get(proxy_name) {
            health.failure_count.fetch_add(1, Ordering::Relaxed);
            *health.last_failure.write() = Some(Instant::now());
            *health.last_used.write() = Instant::now();

            let penalty = if reason.contains("unsuccessful tunnel")
                || reason.contains("connection refused")
            {
                20 // Severe penalty for connection failures
            } else if reason.contains("timeout") {
                10 // Moderate penalty for timeouts
            } else {
                5 // Minor penalty for other failures
            };

            let old_score = health.health_score.load(Ordering::Relaxed);
            let new_score = old_score.saturating_sub(penalty);
            health.health_score.store(new_score, Ordering::Relaxed);

            let success = health.success_count.load(Ordering::Relaxed);
            let failure = health.failure_count.load(Ordering::Relaxed);
            let total = success + failure;
            let failure_rate = if total > 0 {
                (failure as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            warn!(
                "Proxy '{}' FAILED - Health: {}→{} (Failures: {}, Rate: {:.1}%, Reason: {})",
                proxy_name, old_score, new_score, failure, failure_rate, reason
            );

            if old_score > 30 && new_score <= 30 {
                warn!(
                    "Proxy '{}' is now DEGRADED (health dropped to {})",
                    proxy_name, new_score
                );
            } else if old_score > 10 && new_score <= 10 {
                warn!(
                    "Proxy '{}' is now UNHEALTHY (health dropped to {})",
                    proxy_name, new_score
                );
            }
        }
    }

    /// Start background health check and healing task
    pub async fn start_health_checker(&self) -> Result<()> {
        let health_stats = self.health_stats.clone();
        let check_interval = Duration::from_secs(60); // Check every minute
        let healing_interval = Duration::from_secs(60); // Heal every minute
        let min_unused_time = Duration::from_secs(300); // 5 minutes

        tokio::spawn(async move {
            let mut interval = interval(check_interval);
            let mut heal_counter = 0;

            loop {
                interval.tick().await;
                heal_counter += 1;

                // Gradual healing: slowly recover proxy health
                if heal_counter % (healing_interval.as_secs() / check_interval.as_secs()) == 0 {
                    let mut healed_count = 0;

                    for entry in health_stats.iter() {
                        let name = entry.key();
                        let health = entry.value();

                        let current_score = health.health_score.load(Ordering::Relaxed);

                        if current_score < 100 {
                            let last_used = *health.last_used.read();
                            let time_since_use = last_used.elapsed();

                            // Only heal if unused for minimum period
                            if time_since_use > min_unused_time {
                                let old_score = current_score;
                                let new_score = std::cmp::min(100, current_score + 2); // +2 per healing cycle
                                health.health_score.store(new_score, Ordering::Relaxed);

                                if new_score != old_score {
                                    healed_count += 1;
                                    let mins = time_since_use.as_secs() as f64 / 60.0;
                                    info!(
                                        "Proxy '{}' healed: {}→{} (unused for {:.1} min)",
                                        name, old_score, new_score, mins
                                    );
                                }
                            }
                        }
                    }

                    if healed_count > 0 {
                        info!("Healing cycle complete: {} proxies healed", healed_count);
                    }
                }

                // Periodic health summary (every 10 minutes)
                if heal_counter % 10 == 0 {
                    let mut healthy_count = 0;
                    let mut degraded_count = 0;
                    let mut unhealthy_count = 0;

                    for entry in health_stats.iter() {
                        let score = entry.value().health_score.load(Ordering::Relaxed);
                        if score >= 70 {
                            healthy_count += 1;
                        } else if score >= 30 {
                            degraded_count += 1;
                        } else {
                            unhealthy_count += 1;
                        }
                    }

                    info!(
                        "Proxy Health Summary: {} healthy, {} degraded, {} unhealthy",
                        healthy_count, degraded_count, unhealthy_count
                    );
                }
            }
        });

        Ok(())
    }

    /// Get all proxy statistics
    pub fn get_stats(&self) -> FxHashMap<String, ProxyStats> {
        let mut stats = FxHashMap::default();

        for proxy in &self.proxies {
            if let Some(health) = self.health_stats.get(&proxy.name) {
                let success = health.success_count.load(Ordering::Relaxed);
                let failure = health.failure_count.load(Ordering::Relaxed);
                let total = success + failure;
                let success_rate = if total > 0 {
                    (success as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                let health_score = health.health_score.load(Ordering::Relaxed);

                stats.insert(
                    proxy.name.clone(),
                    ProxyStats {
                        name: proxy.name.clone(),
                        region: proxy.region.clone(),
                        success_count: success,
                        failure_count: failure,
                        success_rate,
                        health_score,
                        last_success: *health.last_success.read(),
                        last_failure: *health.last_failure.read(),
                    },
                );
            }
        }

        stats
    }

    /// Check if proxies are enabled
    pub fn is_enabled(&self) -> bool {
        self.config.proxy.enabled && !self.proxies.is_empty()
    }

    /// Get total number of proxies
    pub fn count(&self) -> usize {
        self.proxies.len()
    }
}

#[derive(Debug, Clone)]
pub struct ProxyStats {
    pub name: String,
    pub region: String,
    pub success_count: u64,
    pub failure_count: u64,
    pub success_rate: f64,
    pub health_score: u8,
    pub last_success: Option<Instant>,
    pub last_failure: Option<Instant>,
}
