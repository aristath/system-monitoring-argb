use std::fs;
use std::path::PathBuf;

pub struct Gpu {
    path: PathBuf,
}

pub fn discover() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return gpus;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Only match "cardN", not "card0-DP-1" etc.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        // Filter for discrete GPUs (>4GB VRAM)
        if let Some(vram) = read_u64(&device.join("mem_info_vram_total")) {
            if vram > 4_000_000_000 {
                gpus.push(Gpu { path: device });
            }
        }
    }
    gpus.sort_by(|a, b| a.path.cmp(&b.path));
    gpus
}

impl Gpu {
    pub fn read_busy(&self) -> f64 {
        read_u64(&self.path.join("gpu_busy_percent")).unwrap_or(0) as f64
    }

    pub fn read_vram_percent(&self) -> f64 {
        let used = read_u64(&self.path.join("mem_info_vram_used")).unwrap_or(0);
        let total = read_u64(&self.path.join("mem_info_vram_total")).unwrap_or(1);
        used as f64 / total as f64 * 100.0
    }
}

fn read_u64(path: &PathBuf) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}
