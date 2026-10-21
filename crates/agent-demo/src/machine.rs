//! Readings of the machine the server runs on, sampled once a second (sysinfo).
//!
//! [`METRICS`] is the one list of scalar readings: the agent's `get_system_metrics` tool reports
//! them and WebAssembly widgets import each one as `(import "tools" "<name>" (func (result f64)))`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_demo_proto::agentdemo::v1 as pb;
use serde_json::{Value, json};
use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::watch;

pub const SAMPLE_EVERY: Duration = Duration::from_secs(1);
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = MB * 1024.0;

/// Processes kept per sample: the busiest by CPU plus the largest by memory.
const TOP_PROCESSES: usize = 30;

/// One sample: the metrics plus the busiest processes.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub metrics: pb::MachineMetrics,
    /// Busiest by CPU first.
    pub top: Vec<Process>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// A scalar reading: name, unit/description, value.
pub type Metric = (&'static str, &'static str, fn(&pb::MachineMetrics) -> f64);

#[allow(clippy::cast_precision_loss)] // byte counts far below 2^52
pub const METRICS: &[Metric] = &[
    ("cpu_percent", "CPU load of all cores, 0-100", |m| {
        f64::from(m.cpu_percent)
    }),
    ("core_count", "number of logical cores", |m| {
        m.cores.len() as f64
    }),
    ("memory_used_mb", "used memory, MiB", |m| {
        m.memory_used_bytes as f64 / MB
    }),
    ("memory_total_mb", "total memory, MiB", |m| {
        m.memory_total_bytes as f64 / MB
    }),
    ("memory_percent", "used memory, 0-100", |m| {
        percent(m.memory_used_bytes, m.memory_total_bytes)
    }),
    ("swap_used_mb", "used swap, MiB", |m| {
        m.swap_used_bytes as f64 / MB
    }),
    ("load_1", "load average over 1 minute", |m| m.load_1),
    ("load_5", "load average over 5 minutes", |m| m.load_5),
    ("load_15", "load average over 15 minutes", |m| m.load_15),
    ("uptime_s", "seconds since boot", |m| m.uptime_s as f64),
    ("process_count", "running processes", |m| {
        f64::from(m.process_count)
    }),
    ("disk_used_gb", "used disk space (all disks), GiB", |m| {
        m.disk_used_bytes as f64 / GB
    }),
    ("disk_total_gb", "disk space (all disks), GiB", |m| {
        m.disk_total_bytes as f64 / GB
    }),
    ("disk_percent", "used disk space, 0-100", |m| {
        percent(m.disk_used_bytes, m.disk_total_bytes)
    }),
    ("net_rx_kbps", "network receive rate, KiB/s", |m| {
        m.net_rx_bytes_per_s as f64 / 1024.0
    }),
    ("net_tx_kbps", "network transmit rate, KiB/s", |m| {
        m.net_tx_bytes_per_s as f64 / 1024.0
    }),
    ("time_ms", "sample time, Unix milliseconds", |m| {
        m.at_ms as f64
    }),
];

#[allow(clippy::cast_precision_loss)]
fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 * 100.0 / total as f64
    }
}

/// The latest sample, refreshed in the background.
#[derive(Debug, Clone)]
pub struct Machine {
    latest: watch::Receiver<Arc<Sample>>,
}

impl Machine {
    /// Starts sampling on a blocking thread (sysinfo calls block briefly).
    pub fn start() -> Self {
        let (tx, rx) = watch::channel(Arc::new(Sample::default()));
        std::thread::Builder::new()
            .name("machine-sampler".into())
            .spawn(move || sample_forever(&tx))
            .expect("spawning the sampler thread");
        Self { latest: rx }
    }

    /// A fixed sample, for tests.
    #[cfg(test)]
    pub fn fixed(sample: Sample) -> Self {
        let (tx, rx) = watch::channel(Arc::new(sample));
        std::mem::forget(tx);
        Self { latest: rx }
    }

    pub fn latest(&self) -> Arc<Sample> {
        Arc::clone(&self.latest.borrow())
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Sample>> {
        self.latest.clone()
    }

    /// The `get_system_metrics` tool's answer.
    pub fn report(&self) -> Value {
        let sample = self.latest();
        let m = &sample.metrics;
        let mut values = serde_json::Map::new();
        for (name, _, read) in METRICS {
            values.insert((*name).into(), json!(round(read(m), 2)));
        }
        json!({
            "host": m.host,
            "os": m.os,
            "metrics": values,
            "cores_percent": m.cores.iter().map(|c| round(f64::from(c.percent), 1)).collect::<Vec<_>>(),
        })
    }

    /// The `list_processes` tool's answer: the top `limit` by CPU or by memory.
    #[allow(clippy::cast_precision_loss)] // byte counts far below 2^52
    pub fn processes(&self, by_memory: bool, limit: usize) -> Value {
        let sample = self.latest();
        let mut procs: Vec<&Process> = sample.top.iter().collect();
        if by_memory {
            procs.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes));
        }
        let rows: Vec<_> = procs
            .into_iter()
            .take(limit.clamp(1, TOP_PROCESSES))
            .map(|p| {
                json!({
                    "pid": p.pid,
                    "name": p.name,
                    "cpu_percent": round(f64::from(p.cpu_percent), 1),
                    "memory_mb": round(p.memory_bytes as f64 / MB, 1),
                })
            })
            .collect();
        json!({ "sorted_by": if by_memory { "memory" } else { "cpu" }, "processes": rows })
    }
}

fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

fn sample_forever(tx: &watch::Sender<Arc<Sample>>) {
    let mut sys = System::new();
    let mut networks = Networks::new_with_refreshed_list();
    let mut disks = Disks::new_with_refreshed_list();
    // CPU usage is a difference between two refreshes: prime it.
    sys.refresh_cpu_usage();
    let mut tick: u64 = 0;
    loop {
        std::thread::sleep(SAMPLE_EVERY);
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        networks.refresh(true);
        // Disks change slowly.
        if tick.is_multiple_of(10) {
            disks.refresh(true);
        }
        tick += 1;
        if tx.send(Arc::new(read(&sys, &networks, &disks))).is_err() {
            return;
        }
    }
}

fn read(sys: &System, networks: &Networks, disks: &Disks) -> Sample {
    let load = System::load_average();
    let per_s = |bytes: u64| bytes * 1000 / u64::try_from(SAMPLE_EVERY.as_millis()).unwrap_or(1000);
    let (rx, tx) = networks.values().fold((0, 0), |(rx, tx), n| {
        (rx + n.received(), tx + n.transmitted())
    });
    let (disk_total, disk_free) = disks.list().iter().fold((0, 0), |(t, f), d| {
        (t + d.total_space(), f + d.available_space())
    });
    let metrics = pb::MachineMetrics {
        at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0)),
        host: System::host_name().unwrap_or_default(),
        os: System::long_os_version().unwrap_or_default(),
        cpu_percent: sys.global_cpu_usage(),
        cores: sys
            .cpus()
            .iter()
            .map(|c| pb::CoreLoad {
                percent: c.cpu_usage(),
                ..Default::default()
            })
            .collect(),
        memory_used_bytes: sys.used_memory(),
        memory_total_bytes: sys.total_memory(),
        swap_used_bytes: sys.used_swap(),
        swap_total_bytes: sys.total_swap(),
        load_1: load.one,
        load_5: load.five,
        load_15: load.fifteen,
        uptime_s: System::uptime(),
        process_count: u32::try_from(sys.processes().len()).unwrap_or(u32::MAX),
        disk_used_bytes: disk_total.saturating_sub(disk_free),
        disk_total_bytes: disk_total,
        net_rx_bytes_per_s: per_s(rx),
        net_tx_bytes_per_s: per_s(tx),
        ..Default::default()
    };
    Sample {
        metrics,
        top: top_processes(sys),
    }
}

/// The busiest processes by CPU, plus the largest by memory that are not among them.
fn top_processes(sys: &System) -> Vec<Process> {
    let mut all: Vec<_> = sys
        .processes()
        .iter()
        .map(|(pid, p)| (pid.as_u32(), p.cpu_usage(), p.memory()))
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut keep: Vec<_> = all.iter().take(TOP_PROCESSES).copied().collect();
    all.sort_by_key(|p| std::cmp::Reverse(p.2));
    for p in all.into_iter().take(TOP_PROCESSES) {
        if !keep.iter().any(|k| k.0 == p.0) {
            keep.push(p);
        }
    }
    keep.into_iter()
        .map(|(pid, cpu_percent, memory_bytes)| Process {
            pid,
            name: sys
                .process(sysinfo::Pid::from_u32(pid))
                .map(|p| p.name().to_string_lossy().into_owned())
                .unwrap_or_default(),
            cpu_percent,
            memory_bytes,
        })
        .collect()
}
