/*!
 * Stats Exporter - Writes collector metrics to JSON for dashboard monitoring
 *
 * Overhead: <0.01% CPU, ~2KB memory
 */

use log::{debug, error};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::fs;
use tokio::time::interval;

use crate::rest::{ProxyUsageRecord, RestClient};
use crate::websocket::MessageHandler;
use crate::websocket::WebSocketManager;

#[derive(Debug, Serialize, Deserialize)]
pub struct CollectorStats {
    pub collector: String,
    pub timestamp: String,
    pub uptime_seconds: u64,
    pub messages_total: u64,
    pub connection_status: String,
    pub active_connections: usize,
    pub total_reconnects: u32,
    pub trades_written: u64,
    pub ws_latency_ms: Option<f64>,
    pub messages_per_sec: Option<f64>,
    pub proxy_usage: Option<Vec<ProxyUsageRecord>>,
    // Resource footprint, self-reported from /proc so it is correct wherever the
    // collector runs (bare host, VM, or container) rather than wherever the
    // dashboard runs. Absent on non-Linux. `sys_*` is the collector's host.
    pub rss_mb: Option<f64>,
    pub cpu_pct: Option<f64>,
    pub fd_count: Option<u64>,
    pub sys_mem_used_mb: Option<f64>,
    pub sys_mem_total_mb: Option<f64>,
    pub sys_cpu_cores: Option<u64>, // host core count, for the "of N cores" context on collector CPU
    // Listings detected by periodic discovery since this process started,
    // the dashboard alerts on these; capture begins at the next restart.
    // serde(default) keeps stats.json files from older builds deserializable.
    #[serde(default)]
    pub pending_new_listings: Vec<NewListingStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewListingStat {
    pub symbol: String,
    pub detected_at: String, // RFC 3339, UTC
}

pub struct StatsExporter {
    collector_name: String,
    stats_dir: PathBuf,
    stats_file: PathBuf,
    start_time: SystemTime,
    ws_manager: Arc<WebSocketManager>,
    message_handler: Arc<MessageHandler>,
    rest_client: Arc<RestClient>,
    // Listings detected mid-run by discovery (symbol -> detection epoch ms).
    new_listings: Arc<dashmap::DashMap<String, i64>>,
    last_message_count: std::sync::atomic::AtomicU64,
    last_check_time: parking_lot::RwLock<std::time::Instant>,
    // Previous process CPU-time (jiffies) for the interval-rate calculation.
    last_cpu_ticks: std::sync::atomic::AtomicU64,
}

impl StatsExporter {
    /// `stats_dir` is derived from the configured output directory rather than
    /// assumed. Hardcoding "./data/stats" silently split the heartbeat from the
    /// capture whenever a user chose any other data directory: the dashboard found
    /// stats.json and reported "live" while Coverage, Validation and Replay saw an
    /// empty tree.
    pub fn new(
        collector_name: String,
        ws_manager: Arc<WebSocketManager>,
        message_handler: Arc<MessageHandler>,
        rest_client: Arc<RestClient>,
        new_listings: Arc<dashmap::DashMap<String, i64>>,
        stats_dir: PathBuf,
    ) -> Self {
        let stats_file = stats_dir.join("stats.json");

        Self {
            collector_name,
            stats_dir,
            stats_file,
            start_time: SystemTime::now(),
            ws_manager,
            message_handler,
            rest_client,
            new_listings,
            last_message_count: std::sync::atomic::AtomicU64::new(0),
            last_check_time: parking_lot::RwLock::new(std::time::Instant::now()),
            last_cpu_ticks: std::sync::atomic::AtomicU64::new(0),
        }
    }

    async fn get_stats(&self) -> CollectorStats {
        let uptime = self
            .start_time
            .elapsed()
            .unwrap_or(Duration::from_secs(0))
            .as_secs();

        let ws_stats = self.ws_manager.get_stats().await;
        let active_connections = ws_stats.len();
        let total_reconnects: u32 = ws_stats.values().map(|s| s.reconnect_attempts).sum();

        let msg_stats = self.message_handler.get_stats();
        let messages_total =
            msg_stats.trades + msg_stats.depth_updates + msg_stats.klines + msg_stats.liquidations;

        // Futures collector writes trades directly, not via a separate trade processor
        let trades_written = msg_stats.trades;

        // Connection status: "connected" if we have active connections
        let connection_status = if active_connections > 0 {
            "connected".to_string()
        } else {
            "disconnected".to_string()
        };

        let now = std::time::Instant::now();
        let last_count = self
            .last_message_count
            .load(std::sync::atomic::Ordering::Relaxed);
        let last_time = *self.last_check_time.read();
        let elapsed = now.duration_since(last_time).as_secs_f64();

        let messages_per_sec = if last_count > 0 && elapsed > 0.0 {
            let msg_delta = messages_total.saturating_sub(last_count);
            Some(msg_delta as f64 / elapsed)
        } else {
            None
        };

        // Resource footprint from /proc (no-op off Linux). CPU is a rate, so it
        // needs the previous tick count and the same interval as messages/sec.
        let res = read_resource_stats();
        // Collector CPU as percent-of-one-core = process CPU-time delta / wall-clock
        // delta. Source: the process's own scheduler-accounted time (utime+stime from
        // /proc/self/stat) over a monotonic clock. We deliberately do NOT derive a
        // whole-machine figure from /proc/stat: on a virtualised host its tick-sampled
        // per-CPU counters under-count so badly that the machine can appear to use
        // less CPU than one of its own processes: impossible, and the source of a
        // nonsensical "collector > system" reading. The per-process clock is the
        // trustworthy one, so we report the collector's own footprint and nothing we
        // can't stand behind.
        let cpu_pct = res.cpu_ticks.and_then(|ticks| {
            let prev = self
                .last_cpu_ticks
                .swap(ticks, std::sync::atomic::Ordering::Relaxed);
            (prev > 0 && elapsed > 0.0).then(|| {
                let cpu_secs = ticks.saturating_sub(prev) as f64 / clk_tck();
                cpu_secs / elapsed * 100.0
            })
        });

        self.last_message_count
            .store(messages_total, std::sync::atomic::Ordering::Relaxed);
        *self.last_check_time.write() = now;

        let ws_latency_ms = if msg_stats.total_latency_samples > 0 {
            Some(msg_stats.ws_latency_ms)
        } else {
            None
        };

        let proxy_usage = Some(self.rest_client.get_proxy_usage_stats());

        let timestamp = chrono::Utc::now().to_rfc3339();

        // Listings discovery has flagged since start, oldest first; the
        // dashboard alerts on these until a restart picks them up.
        let mut pending_new_listings: Vec<NewListingStat> = self
            .new_listings
            .iter()
            .map(|e| NewListingStat {
                symbol: e.key().clone(),
                detected_at: chrono::DateTime::from_timestamp_millis(*e.value())
                    .unwrap_or_default()
                    .to_rfc3339(),
            })
            .collect();
        pending_new_listings.sort_by(|a, b| a.detected_at.cmp(&b.detected_at));

        CollectorStats {
            collector: self.collector_name.clone(),
            timestamp,
            uptime_seconds: uptime,
            messages_total,
            connection_status,
            active_connections,
            total_reconnects,
            trades_written,
            ws_latency_ms,
            messages_per_sec,
            proxy_usage,
            rss_mb: res.rss_kb.map(|k| k as f64 / 1024.0),
            cpu_pct,
            fd_count: res.fd_count,
            sys_mem_used_mb: res.sys_mem_used_kb.map(|k| k as f64 / 1024.0),
            sys_mem_total_mb: res.sys_mem_total_kb.map(|k| k as f64 / 1024.0),
            sys_cpu_cores: res.n_cpus,
            pending_new_listings,
        }
    }

    /// Write stats to JSON file atomically
    async fn write_stats(&self) {
        let stats = self.get_stats().await;
        let json = match serde_json::to_string_pretty(&stats) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize stats: {}", e);
                return;
            }
        };

        // Write to a temp file first, then rename atomically.
        let temp_file = self.stats_file.with_extension("tmp");
        if let Err(e) = fs::write(&temp_file, json).await {
            error!("Failed to write stats to temp file: {}", e);
            return;
        }

        if let Err(e) = fs::rename(&temp_file, &self.stats_file).await {
            error!("Failed to rename stats file: {}", e);
            return;
        }

        debug!("Stats written to {}", self.stats_file.display());
    }

    /// Start background task that writes stats every 5 seconds
    pub async fn start(self: Arc<Self>) {
        let stats_dir = self.stats_dir.clone();
        if let Err(e) = fs::create_dir_all(&stats_dir).await {
            error!("Failed to create stats directory: {}", e);
            return;
        }

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                self.write_stats().await;
            }
        });
    }
}

// --- resource footprint (self-reported from /proc; no-op off Linux) ---------

#[derive(Default)]
struct ResourceStats {
    rss_kb: Option<u64>,
    cpu_ticks: Option<u64>, // utime + stime, jiffies
    fd_count: Option<u64>,
    sys_mem_used_kb: Option<u64>,
    sys_mem_total_kb: Option<u64>,
    n_cpus: Option<u64>, // number of cores (per-core `cpuN` lines in /proc/stat)
}

/// Clock ticks per second (USER_HZ): the unit of process CPU time in /proc.
#[cfg(target_os = "linux")]
fn clk_tck() -> f64 {
    // sysconf is always safe to call; it returns -1 on error, which we guard.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as f64
    } else {
        100.0
    }
}

#[cfg(not(target_os = "linux"))]
fn clk_tck() -> f64 {
    100.0
}

#[cfg(any(target_os = "linux", test))]
fn parse_vm_kb(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|l| {
        let rest = l.strip_prefix(key)?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// utime (field 14) + stime (field 15) from /proc/self/stat, robust to a `comm`
/// that contains spaces or parentheses by scanning past the final ')'.
#[cfg(any(target_os = "linux", test))]
fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let after = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = after.split_whitespace().collect();
    // After ')', index 0 is `state` (field 3); utime=14 -> index 11, stime -> 12.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// (total_kb, available_kb) from /proc/meminfo.
#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(meminfo: &str) -> Option<(u64, u64)> {
    Some((
        parse_vm_kb(meminfo, "MemTotal:")?,
        parse_vm_kb(meminfo, "MemAvailable:")?,
    ))
}

/// Number of cores = count of per-core `cpuN` lines in /proc/stat.
#[cfg(any(target_os = "linux", test))]
fn parse_n_cpus(procstat: &str) -> Option<u64> {
    let n = procstat
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .count() as u64;
    (n > 0).then_some(n)
}

#[cfg(target_os = "linux")]
fn read_resource_stats() -> ResourceStats {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let procstat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let (total, avail) = match parse_meminfo(&meminfo) {
        Some((t, a)) => (Some(t), Some(a)),
        None => (None, None),
    };
    ResourceStats {
        rss_kb: parse_vm_kb(&status, "VmRSS:"),
        cpu_ticks: parse_cpu_ticks(&stat),
        // read_dir holds one fd on /proc/self/fd while counting, so it lists
        // itself, so subtract that one to report the real open-descriptor count.
        fd_count: std::fs::read_dir("/proc/self/fd")
            .ok()
            .map(|d| (d.count() as u64).saturating_sub(1)),
        sys_mem_used_kb: total.zip(avail).map(|(t, a)| t.saturating_sub(a)),
        sys_mem_total_kb: total,
        n_cpus: parse_n_cpus(&procstat),
    }
}

#[cfg(not(target_os = "linux"))]
fn read_resource_stats() -> ResourceStats {
    ResourceStats::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vmrss_and_ignores_other_keys() {
        let s = "Name:\tcollector\nVmHWM:\t  862052 kB\nVmRSS:\t  706556 kB\n";
        assert_eq!(parse_vm_kb(s, "VmRSS:"), Some(706556));
        assert_eq!(parse_vm_kb(s, "VmHWM:"), Some(862052));
        assert_eq!(parse_vm_kb(s, "VmNope:"), None);
    }

    #[test]
    fn cpu_ticks_survive_parens_in_comm() {
        // A comm containing spaces and a ')' is the classic /proc/self/stat trap.
        let stat = "1234 (weird )name) S 1 1234 1234 0 -1 4194304 100 0 0 0 111 222 0 0 20 0 8";
        assert_eq!(parse_cpu_ticks(stat), Some(333)); // utime 111 + stime 222
    }

    #[test]
    fn meminfo_used_is_total_minus_available() {
        let m = "MemTotal: 8000000 kB\nMemFree: 100000 kB\nMemAvailable: 3000000 kB\n";
        assert_eq!(parse_meminfo(m), Some((8_000_000, 3_000_000)));
    }

    #[test]
    fn n_cpus_counts_per_core_lines_only() {
        let p = "cpu  100 0 50 800 40 0 10 0 0 0\ncpu0 1 2 3 4\ncpu1 5 6 7 8\n";
        assert_eq!(parse_n_cpus(p), Some(2)); // the aggregate `cpu ` line isn't a core
        assert_eq!(parse_n_cpus("cpu 1 2 3 4\n"), None);
    }
}
