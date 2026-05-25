use std::fs;
use std::path::PathBuf;

pub struct CpuTemp {
    path: PathBuf,
}

impl CpuTemp {
    pub fn discover() -> Option<Self> {
        for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
            let Ok(name) = fs::read_to_string(entry.path().join("name")) else {
                continue;
            };
            let hwmon = entry.path();
            let name = name.trim();

            if name == "k10temp" {
                if let Some(path) = temp_input_with_label(&hwmon, "Tctl") {
                    return Some(Self { path });
                }
                if let Some(path) = first_existing_input(&hwmon, &["temp1_input"]) {
                    return Some(Self { path });
                }
            } else if name == "coretemp" {
                if let Some(path) = temp_input_with_label(&hwmon, "Package id 0") {
                    return Some(Self { path });
                }
                if let Some(path) = first_existing_input(&hwmon, &["temp1_input"]) {
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

fn temp_input_with_label(hwmon: &std::path::Path, wanted_label: &str) -> Option<PathBuf> {
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

fn first_existing_input(hwmon: &std::path::Path, inputs: &[&str]) -> Option<PathBuf> {
    inputs
        .iter()
        .map(|input| hwmon.join(input))
        .find(|path| path.exists())
}
