use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub struct Gpu {
    bus_id: String,
    name: String,
    source: GpuSource,
}

enum GpuSource {
    AmdSysfs {
        path: PathBuf,
        temp_path: Option<PathBuf>,
    },
    NvidiaSmi {
        index: u32,
        cached: GpuMetrics,
        last_refresh: Option<Instant>,
    },
}

#[derive(Clone, Copy, Default)]
pub struct GpuMetrics {
    pub busy: f64,
    pub vram: f64,
    pub temp: f64,
}

pub fn discover() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    let mut seen_bus_ids = HashSet::new();

    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return discover_nvidia_gpus(gpus, seen_bus_ids);
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Only match "cardN", not "card0-DP-1" etc.
        let Some(card_num) = name.strip_prefix("card") else {
            continue;
        };
        if card_num.is_empty() || !card_num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let device = entry.path().join("device");
        // Filter for discrete GPUs (>4GB VRAM)
        if let Some(vram) = read_u64(&device.join("mem_info_vram_total")) {
            if vram > 4_000_000_000 {
                let bus_id = pci_bus_id(&device).unwrap_or(name);
                let temp_path = discover_temp_path(&device);
                seen_bus_ids.insert(bus_id.clone());
                gpus.push(Gpu {
                    name: "AMD GPU".to_string(),
                    bus_id,
                    source: GpuSource::AmdSysfs {
                        path: device,
                        temp_path,
                    },
                });
            }
        }
    }

    let mut gpus = discover_nvidia_gpus(gpus, seen_bus_ids);
    gpus.sort_by(|a, b| a.bus_id.cmp(&b.bus_id));
    gpus
}

impl Gpu {
    pub fn read_metrics(&mut self) -> GpuMetrics {
        match &mut self.source {
            GpuSource::AmdSysfs { path, temp_path } => GpuMetrics {
                busy: read_u64(&path.join("gpu_busy_percent")).unwrap_or(0) as f64,
                vram: read_vram_percent(path),
                temp: temp_path
                    .as_deref()
                    .and_then(read_millidegrees_c)
                    .unwrap_or(0.0),
            },
            GpuSource::NvidiaSmi {
                index,
                cached,
                last_refresh,
            } => {
                if last_refresh
                    .map(|last| last.elapsed() >= Duration::from_secs(1))
                    .unwrap_or(true)
                {
                    if let Some(metrics) = read_nvidia_metrics(*index) {
                        *cached = metrics;
                    }
                    *last_refresh = Some(Instant::now());
                }
                *cached
            }
        }
    }

    pub fn has_temp_sensor(&self) -> bool {
        match &self.source {
            GpuSource::AmdSysfs { temp_path, .. } => temp_path.is_some(),
            GpuSource::NvidiaSmi { .. } => true,
        }
    }

    pub fn description(&self) -> String {
        match &self.source {
            GpuSource::AmdSysfs { .. } => format!("{} at {} via sysfs", self.name, self.bus_id),
            GpuSource::NvidiaSmi { .. } => {
                format!("{} at {} via nvidia-smi", self.name, self.bus_id)
            }
        }
    }
}

fn discover_nvidia_gpus(mut gpus: Vec<Gpu>, mut seen_bus_ids: HashSet<String>) -> Vec<Gpu> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,pci.bus_id,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output();

    let Ok(output) = output else {
        return gpus;
    };
    if !output.status.success() {
        return gpus;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let Some(discovered) = parse_nvidia_discovery_line(line) else {
            continue;
        };
        if discovered.total_mib <= 4096.0 {
            continue;
        }

        let bus_id = normalize_pci_bus_id(&discovered.bus_id);
        if !seen_bus_ids.insert(bus_id.clone()) {
            continue;
        }

        gpus.push(Gpu {
            bus_id,
            name: discovered.name,
            source: GpuSource::NvidiaSmi {
                index: discovered.index,
                cached: discovered.metrics,
                last_refresh: Some(Instant::now()),
            },
        });
    }

    gpus
}

struct NvidiaDiscovery {
    index: u32,
    bus_id: String,
    name: String,
    total_mib: f64,
    metrics: GpuMetrics,
}

fn parse_nvidia_discovery_line(line: &str) -> Option<NvidiaDiscovery> {
    let fields: Vec<_> = line.split(',').map(str::trim).collect();
    if fields.len() < 7 {
        return None;
    }

    let index = fields[0].parse().ok()?;
    let used_mib = parse_f64(fields[4])?;
    let total_mib = parse_f64(fields[5])?;
    let vram = if total_mib > 0.0 {
        used_mib / total_mib * 100.0
    } else {
        0.0
    };

    Some(NvidiaDiscovery {
        index,
        bus_id: fields[1].to_string(),
        name: fields[2].to_string(),
        total_mib,
        metrics: GpuMetrics {
            busy: parse_f64(fields[3])?,
            vram,
            temp: parse_f64(fields[6])?,
        },
    })
}

fn read_nvidia_metrics(index: u32) -> Option<GpuMetrics> {
    let output = Command::new("nvidia-smi")
        .arg("-i")
        .arg(index.to_string())
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_nvidia_metrics_line(stdout.lines().next()?)
}

fn parse_nvidia_metrics_line(line: &str) -> Option<GpuMetrics> {
    let fields: Vec<_> = line.split(',').map(str::trim).collect();
    if fields.len() < 4 {
        return None;
    }

    let used_mib = parse_f64(fields[1])?;
    let total_mib = parse_f64(fields[2])?;
    let vram = if total_mib > 0.0 {
        used_mib / total_mib * 100.0
    } else {
        0.0
    };

    Some(GpuMetrics {
        busy: parse_f64(fields[0])?,
        vram,
        temp: parse_f64(fields[3])?,
    })
}

fn discover_temp_path(device: &Path) -> Option<PathBuf> {
    let hwmon_dir = device.join("hwmon");
    for entry in fs::read_dir(hwmon_dir).ok()?.flatten() {
        let hwmon = entry.path();
        let Ok(name) = fs::read_to_string(hwmon.join("name")) else {
            continue;
        };
        if name.trim() != "amdgpu" {
            continue;
        }

        if let Some(path) = temp_input_with_label(&hwmon, "junction") {
            return Some(path);
        }
        if let Some(path) = temp_input_with_label(&hwmon, "edge") {
            return Some(path);
        }
        for input in ["temp2_input", "temp1_input"] {
            let path = hwmon.join(input);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

fn pci_bus_id(device: &Path) -> Option<String> {
    fs::canonicalize(device)
        .ok()?
        .file_name()?
        .to_str()
        .map(normalize_pci_bus_id)
}

fn normalize_pci_bus_id(bus_id: &str) -> String {
    let bus_id = bus_id.trim().to_ascii_lowercase();
    let mut parts: Vec<&str> = bus_id.split(':').collect();
    if parts.len() == 3 && parts[0].len() > 4 {
        parts[0] = &parts[0][parts[0].len() - 4..];
        return parts.join(":");
    }
    bus_id
}

fn read_vram_percent(path: &Path) -> f64 {
    let used = read_u64(&path.join("mem_info_vram_used")).unwrap_or(0);
    let total = read_u64(&path.join("mem_info_vram_total")).unwrap_or(1);
    used as f64 / total as f64 * 100.0
}

fn temp_input_with_label(hwmon: &Path, wanted_label: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(hwmon).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(index) = name
            .strip_prefix("temp")
            .and_then(|s| s.strip_suffix("_label"))
        else {
            continue;
        };

        let Ok(label) = fs::read_to_string(&path) else {
            continue;
        };
        if label.trim() == wanted_label {
            let input = hwmon.join(format!("temp{index}_input"));
            if input.exists() {
                return Some(input);
            }
        }
    }
    None
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_millidegrees_c(path: &Path) -> Option<f64> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|mc| mc / 1000.0)
}

fn parse_f64(value: &str) -> Option<f64> {
    value.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_nvidia_bus_ids_to_linux_pci_shape() {
        assert_eq!(normalize_pci_bus_id("00000000:1C:00.0"), "0000:1c:00.0");
        assert_eq!(normalize_pci_bus_id("0000:03:00.0"), "0000:03:00.0");
    }

    #[test]
    fn parses_nvidia_discovery_rows() {
        let gpu = parse_nvidia_discovery_line(
            "0, 00000000:1C:00.0, NVIDIA GeForce RTX 3090, 7, 12288, 24576, 54",
        )
        .unwrap();

        assert_eq!(gpu.index, 0);
        assert_eq!(gpu.bus_id, "00000000:1C:00.0");
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 3090");
        assert_eq!(gpu.metrics.busy, 7.0);
        assert_eq!(gpu.metrics.vram, 50.0);
        assert_eq!(gpu.metrics.temp, 54.0);
    }

    #[test]
    fn parses_nvidia_metric_rows() {
        let metrics = parse_nvidia_metrics_line("12, 6144, 24576, 49").unwrap();

        assert_eq!(metrics.busy, 12.0);
        assert_eq!(metrics.vram, 25.0);
        assert_eq!(metrics.temp, 49.0);
    }
}
