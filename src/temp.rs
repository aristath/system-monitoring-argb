use std::fs;
use std::path::PathBuf;

pub struct CpuTemp {
    path: PathBuf,
}

pub struct GpuTemp {
    paths: Vec<PathBuf>,
}

impl CpuTemp {
    pub fn discover() -> Option<Self> {
        // Find k10temp hwmon and use Tctl (temp1_input)
        for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
            let name = fs::read_to_string(entry.path().join("name")).ok()?;
            if name.trim() == "k10temp" {
                let path = entry.path().join("temp1_input");
                if path.exists() {
                    return Some(Self { path });
                }
            }
        }
        None
    }

    pub fn read(&self) -> f64 {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|mc| mc / 1000.0)
            .unwrap_or(0.0)
    }
}

impl GpuTemp {
    pub fn discover() -> Self {
        let mut paths = Vec::new();
        let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
            return Self { paths };
        };
        for entry in entries.flatten() {
            let Ok(name) = fs::read_to_string(entry.path().join("name")) else {
                continue;
            };
            if name.trim() != "amdgpu" {
                continue;
            }
            // Prefer junction temp (temp2), fall back to edge (temp1)
            let junction = entry.path().join("temp2_input");
            let edge = entry.path().join("temp1_input");
            if junction.exists() {
                paths.push(junction);
            } else if edge.exists() {
                paths.push(edge);
            }
        }
        Self { paths }
    }

    pub fn sensor_count(&self) -> usize {
        self.paths.len()
    }

    /// Returns the hottest GPU temperature in °C
    pub fn read_hottest(&self) -> f64 {
        self.paths.iter()
            .filter_map(|p| {
                fs::read_to_string(p).ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|mc| mc / 1000.0)
            })
            .fold(0.0f64, f64::max)
    }
}
