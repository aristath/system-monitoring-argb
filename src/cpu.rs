use std::fs;

pub struct CpuReader {
    prev_idle: u64,
    prev_total: u64,
}

impl CpuReader {
    pub fn new() -> Self {
        let (idle, total) = Self::read_stat();
        Self { prev_idle: idle, prev_total: total }
    }

    pub fn read(&mut self) -> f64 {
        let (idle, total) = Self::read_stat();
        let d_idle = idle.wrapping_sub(self.prev_idle);
        let d_total = total.wrapping_sub(self.prev_total);
        self.prev_idle = idle;
        self.prev_total = total;
        if d_total == 0 {
            return 0.0;
        }
        (1.0 - d_idle as f64 / d_total as f64) * 100.0
    }

    fn read_stat() -> (u64, u64) {
        let content = fs::read_to_string("/proc/stat").unwrap_or_default();
        let line = content.lines().next().unwrap_or("");
        let vals: Vec<u64> = line
            .split_whitespace()
            .skip(1) // skip "cpu"
            .filter_map(|s| s.parse().ok())
            .collect();
        if vals.len() < 5 {
            return (0, 0);
        }
        // idle = idle + iowait
        let idle = vals[3] + vals[4];
        let total: u64 = vals.iter().sum();
        (idle, total)
    }
}
