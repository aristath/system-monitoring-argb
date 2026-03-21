use std::fs;

pub fn read() -> f64 {
    let content = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut available = 0u64;
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            total = parse_kb(line);
        } else if line.starts_with("MemAvailable:") {
            available = parse_kb(line);
        }
    }
    if total == 0 {
        return 0.0;
    }
    (total - available) as f64 / total as f64 * 100.0
}

fn parse_kb(line: &str) -> u64 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
