use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub struct Config {
    pub openrgb_host: String,
    pub openrgb_port: u16,
    pub openrgb_device: Option<String>,
    pub aio_zone: Option<String>,
    pub aio_leds: Option<usize>,
    pub cpu_temp_zone: Option<String>,
    pub gpu_temp_zone: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        let file_values = load_config_file();

        Self {
            openrgb_host: string_value(
                &file_values,
                "openrgb_host",
                "SYSMON_OPENRGB_HOST",
                "127.0.0.1",
            ),
            openrgb_port: u16_value(&file_values, "openrgb_port", "SYSMON_OPENRGB_PORT", 6742),
            openrgb_device: optional_string_value(
                &file_values,
                "openrgb_device",
                "SYSMON_OPENRGB_DEVICE",
            ),
            aio_zone: optional_string_value(&file_values, "aio_zone", "SYSMON_AIO_ZONE"),
            aio_leds: optional_usize_value(&file_values, "aio_leds", "SYSMON_AIO_LEDS"),
            cpu_temp_zone: optional_string_value(
                &file_values,
                "cpu_temp_zone",
                "SYSMON_CPU_TEMP_ZONE",
            ),
            gpu_temp_zone: optional_string_value(
                &file_values,
                "gpu_temp_zone",
                "SYSMON_GPU_TEMP_ZONE",
            ),
        }
    }
}

fn load_config_file() -> HashMap<String, String> {
    let Some(path) = config_path() else {
        return HashMap::new();
    };
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };

    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim().to_ascii_lowercase();
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((key, value))
        })
        .collect()
}

fn config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SYSMON_CONFIG") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/sysmon/config.env"))
}

fn string_value(
    file_values: &HashMap<String, String>,
    file_key: &str,
    env_key: &str,
    default: &str,
) -> String {
    optional_string_value(file_values, file_key, env_key).unwrap_or_else(|| default.to_string())
}

fn optional_string_value(
    file_values: &HashMap<String, String>,
    file_key: &str,
    env_key: &str,
) -> Option<String> {
    std::env::var(env_key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| file_values.get(file_key).cloned())
}

fn u16_value(
    file_values: &HashMap<String, String>,
    file_key: &str,
    env_key: &str,
    default: u16,
) -> u16 {
    std::env::var(env_key)
        .ok()
        .or_else(|| file_values.get(file_key).cloned())
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn optional_usize_value(
    file_values: &HashMap<String, String>,
    file_key: &str,
    env_key: &str,
) -> Option<usize> {
    std::env::var(env_key)
        .ok()
        .or_else(|| file_values.get(file_key).cloned())
        .and_then(|value| value.parse().ok())
}
