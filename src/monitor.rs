//! Linux system sampling: CPU, memory, NVIDIA/AMD GPU memory and the Ollama
//! server process. No external crates, so it also builds on a bare machine.

use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::ollama;

#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub cpu_pct: f32,
    pub ram_used: u64,
    pub ram_total: u64,
    pub vram_used: u64,
    pub vram_total: u64,
    pub gpu_util: Option<f32>,
    pub gpu_name: String,
    pub gpu_source: String,
    pub ollama_pid: Option<u32>,
    pub ollama_rss: u64,
    pub ollama_cpu: f32,
}

impl Sample {
    /// Memory on the card that nothing is using right now. Always worked out
    /// from the live driver numbers, never from a built-in size.
    pub fn vram_free(&self) -> u64 {
        self.vram_total.saturating_sub(self.vram_used)
    }
}

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub name: String,
    pub total: u64,
    pub used: u64,
    pub util: Option<f32>,
    pub source: &'static str,
}

/// Read one `nvidia-smi --query-gpu=name,memory.total,memory.used,utilization.gpu`
/// line per GPU. The numbers are whatever the driver reports in MiB; the card
/// with the most memory wins so a small integrated GPU does not hide the one
/// Ollama actually uses.
fn parse_nvidia(text: &str) -> Option<GpuInfo> {
    let mut best: Option<GpuInfo> = None;
    let mut count = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.rsplitn(4, ',').collect();
        if parts.len() < 4 {
            continue;
        }
        let util = parts[0].trim().parse::<f32>().ok();
        let used = parts[1].trim().parse::<u64>().unwrap_or(0);
        let total = parts[2].trim().parse::<u64>().unwrap_or(0);
        let name = parts[3].trim().to_string();
        if total == 0 {
            continue;
        }
        count += 1;
        let total_bytes = total * 1024 * 1024;
        if best.as_ref().map(|b| total_bytes > b.total).unwrap_or(true) {
            best = Some(GpuInfo {
                name,
                total: total_bytes,
                used: used * 1024 * 1024,
                util,
                source: "nvidia-smi",
            });
        }
    }
    if let Some(info) = best.as_mut() {
        if count > 1 {
            info.name = format!("{} +{}", info.name, count - 1);
        }
    }
    best
}

/// Fallback for drivers that expose the card through DRM sysfs (AMD, and some
/// NVIDIA/Intel setups). The sizes are in bytes there, unlike nvidia-smi.
fn sysfs_gpu() -> Option<GpuInfo> {
    for card in 0..4 {
        let base = format!("/sys/class/drm/card{card}/device");
        let read = |name: &str| fs::read_to_string(format!("{base}/{name}")).ok();
        let Some(total) = read("mem_info_vram_total")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
        else {
            continue;
        };
        let used = read("mem_info_vram_used")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let util = read("gpu_busy_percent").and_then(|v| v.trim().parse::<f32>().ok());
        let name = read("product_name")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                let vendor = read("vendor").unwrap_or_default();
                if vendor.trim().eq_ignore_ascii_case("0x10de") {
                    "NVIDIA GPU".to_string()
                } else if vendor.trim().eq_ignore_ascii_case("0x1002") {
                    "AMD GPU".to_string()
                } else {
                    "GPU".to_string()
                }
            });
        return Some(GpuInfo {
            name,
            total,
            used,
            util,
            source: "sysfs",
        });
    }
    None
}

#[derive(Clone, Debug)]
pub struct Tick {
    pub sample: Sample,
    pub loaded: Vec<ollama::LoadedModel>,
    pub online: bool,
    pub version: String,
}

fn cpu_times() -> (u64, u64) {
    let text = fs::read_to_string("/proc/stat").unwrap_or_default();
    if let Some(line) = text.lines().next() {
        let values: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        if values.len() >= 4 {
            let idle = values[3] + values.get(4).copied().unwrap_or(0);
            let total: u64 = values.iter().sum();
            return (total, idle);
        }
    }
    (0, 0)
}

fn memory() -> (u64, u64) {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut available = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => {
                total = parts
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
                    * 1024;
            }
            Some("MemAvailable:") => {
                available = parts
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
                    * 1024;
            }
            _ => {}
        }
    }
    (total.saturating_sub(available), total)
}

fn gpu() -> Option<GpuInfo> {
    if let Ok(output) = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(info) = parse_nvidia(&text) {
                return Some(info);
            }
        }
    }
    sysfs_gpu()
}

fn find_ollama_pid() -> Option<u32> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).to_string())
            .collect();
        if args.is_empty() {
            continue;
        }
        let exe = args[0].rsplit('/').next().unwrap_or(&args[0]).to_string();
        if exe == "ollama" && (args.len() == 1 || args.iter().any(|a| a == "serve")) {
            return Some(pid);
        }
    }
    None
}

fn process_stats(pid: u32) -> (u64, u64) {
    let mut rss = 0u64;
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
                    * 1024;
                break;
            }
        }
    }
    let mut ticks = 0u64;
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        if let Some(idx) = stat.rfind(')') {
            let rest = &stat[idx + 1..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() > 13 {
                let utime = fields[11].parse::<u64>().unwrap_or(0);
                let stime = fields[12].parse::<u64>().unwrap_or(0);
                ticks = utime + stime;
            }
        }
    }
    (rss, ticks)
}

struct ProcTracker {
    pid: Option<u32>,
    ticks: u64,
    at: Instant,
}

impl ProcTracker {
    fn new() -> Self {
        Self {
            pid: None,
            ticks: 0,
            at: Instant::now(),
        }
    }

    fn sample(&mut self) -> (Option<u32>, u64, f32) {
        let pid = find_ollama_pid();
        let Some(pid) = pid else {
            self.pid = None;
            self.ticks = 0;
            return (None, 0, 0.0);
        };
        let (rss, ticks) = process_stats(pid);
        let now = Instant::now();
        let cpu = if self.pid == Some(pid) {
            let dt = now.duration_since(self.at).as_secs_f64().max(0.001);
            let delta = ticks.saturating_sub(self.ticks) as f64 / 100.0;
            (delta / dt * 100.0) as f32
        } else {
            0.0
        };
        self.pid = Some(pid);
        self.ticks = ticks;
        self.at = now;
        (Some(pid), rss, cpu)
    }
}

fn sample(prev_cpu: (u64, u64), tracker: &mut ProcTracker) -> Sample {
    let (total, idle) = cpu_times();
    let cpu_pct = if prev_cpu.0 > 0 && total > prev_cpu.0 {
        let dt = (total - prev_cpu.0) as f32;
        let di = idle.saturating_sub(prev_cpu.1) as f32;
        ((dt - di) / dt * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let (ram_used, ram_total) = memory();
    let gpu = gpu();
    let (pid, rss, proc_cpu) = tracker.sample();
    let (gpu_name, gpu_source, vram_used, vram_total, gpu_util) = match gpu {
        Some(info) => (
            info.name,
            info.source.to_string(),
            info.used,
            info.total,
            info.util,
        ),
        None => (String::new(), String::new(), 0, 0, None),
    };
    Sample {
        cpu_pct,
        ram_used,
        ram_total,
        vram_used,
        vram_total,
        gpu_util,
        gpu_name,
        gpu_source,
        ollama_pid: pid,
        ollama_rss: rss,
        ollama_cpu: proc_cpu,
    }
}

pub fn spawn(tx: std::sync::mpsc::Sender<crate::Msg>) {
    thread::spawn(move || {
        let mut prev = cpu_times();
        let mut tracker = ProcTracker::new();
        let mut version = String::new();
        let mut tick: u64 = 0;
        let mut ever_online = false;
        loop {
            let started = Instant::now();
            let now = cpu_times();
            let sample = sample(prev, &mut tracker);
            prev = now;

            let ps = ollama::ps();
            let online = ps.is_ok();
            if online && (!ever_online || tick % 20 == 0) {
                version = ollama::version().unwrap_or_default();
                ever_online = true;
            } else if !online {
                version.clear();
                ever_online = false;
            }
            let _ = tx.send(crate::Msg::Tick(Tick {
                sample,
                loaded: ps.unwrap_or_default(),
                online,
                version: version.clone(),
            }));
            tick += 1;
            let spent = started.elapsed();
            thread::sleep(Duration::from_millis(1000).saturating_sub(spent));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size always comes from the driver's answer, so the same build
    /// follows an 8 GB card, a 16 GB card or anything else.
    #[test]
    fn reads_whatever_size_the_driver_reports() {
        let small = parse_nvidia("NVIDIA GeForce RTX 4060, 8188, 512, 3").unwrap();
        assert_eq!(small.total, 8188 * 1024 * 1024);
        assert_eq!(small.used, 512 * 1024 * 1024);
        assert_eq!(small.util, Some(3.0));

        let large = parse_nvidia("NVIDIA GeForce RTX 4060 Ti, 16380, 1767, 7").unwrap();
        assert_eq!(large.total, 16380 * 1024 * 1024);
        assert_eq!(large.used, 1767 * 1024 * 1024);

        let huge = parse_nvidia("NVIDIA GeForce RTX 5090, 32607, 0, 0").unwrap();
        assert_eq!(huge.total, 32607 * 1024 * 1024);
    }

    #[test]
    fn picks_the_card_with_the_most_memory() {
        let both = concat!(
            "AMD Radeon 780M, 512, 100, 2\n",
            "NVIDIA GeForce RTX 4060 Ti, 16380, 1767, 7\n"
        );
        let info = parse_nvidia(both).unwrap();
        assert_eq!(info.total, 16380 * 1024 * 1024);
        assert!(info.name.starts_with("NVIDIA"));
    }

    #[test]
    fn free_memory_is_total_minus_used() {
        let sample = Sample {
            vram_total: 16 * 1024 * 1024 * 1024,
            vram_used: 2 * 1024 * 1024 * 1024,
            ..Sample::default()
        };
        assert_eq!(sample.vram_free(), 14 * 1024 * 1024 * 1024);
    }
}
