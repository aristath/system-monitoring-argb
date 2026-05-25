use std::ops::Range;
use std::thread;
use std::time::Duration;

mod config;
mod cpu;
mod gpu;
mod mem;
mod openrgb;
mod temp;

const DEFAULT_AIO_ACTIVE_LEDS: usize = 36;

const POLL_MS: u64 = 10; // LED update interval
const WINDOW_SIZE: usize = 200; // rolling average window (= 2000ms)

// Smoothing — RGB color (visual smoothness)
const COLOR_ALPHA: f64 = 0.03;

// Temperature range (°C)
const TEMP_MIN: f64 = 35.0;
const TEMP_MAX: f64 = 90.0;

// Color gradient: 11 stops from 0% to 100%
const GRADIENT: [(u8, u8, u8); 11] = [
    (0, 0, 255),   // 0%   blue
    (0, 128, 255), // 10%  blue-cyan
    (0, 255, 255), // 20%  cyan
    (0, 255, 128), // 30%  cyan-green
    (0, 255, 0),   // 40%  green
    (255, 255, 0), // 50%  yellow
    (255, 128, 0), // 60%  orange
    (255, 64, 0),  // 70%  orange-red
    (255, 0, 0),   // 80%  red
    (128, 0, 0),   // 90%  deep red
    (128, 0, 0),   // 100% deep red
];

fn gradient_color(pct: f64) -> (u8, u8, u8) {
    let p = pct.clamp(0.0, 100.0);
    let idx = p / 10.0;
    let lo = (idx as usize).min(9);
    let hi = lo + 1;
    let t = idx - lo as f64;
    let (r1, g1, b1) = GRADIENT[lo];
    let (r2, g2, b2) = GRADIENT[hi];
    (
        (r1 as f64 + (r2 as f64 - r1 as f64) * t) as u8,
        (g1 as f64 + (g2 as f64 - g1 as f64) * t) as u8,
        (b1 as f64 + (b2 as f64 - b1 as f64) * t) as u8,
    )
}

struct RollingAvg {
    buf: [f64; WINDOW_SIZE],
    pos: usize,
    count: usize,
    sum: f64,
}

impl RollingAvg {
    fn new() -> Self {
        Self {
            buf: [0.0; WINDOW_SIZE],
            pos: 0,
            count: 0,
            sum: 0.0,
        }
    }

    fn push(&mut self, val: f64) -> f64 {
        self.sum -= self.buf[self.pos];
        self.buf[self.pos] = val;
        self.sum += val;
        self.pos = (self.pos + 1) % WINDOW_SIZE;
        if self.count < WINDOW_SIZE {
            self.count += 1;
        }
        self.sum / self.count as f64
    }
}

struct SmoothColor {
    r: f64,
    g: f64,
    b: f64,
}

impl SmoothColor {
    fn new() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        }
    }

    fn update(&mut self, target: (u8, u8, u8)) -> [u8; 4] {
        self.r += COLOR_ALPHA * (target.0 as f64 - self.r);
        self.g += COLOR_ALPHA * (target.1 as f64 - self.g);
        self.b += COLOR_ALPHA * (target.2 as f64 - self.b);
        [
            self.r.round() as u8,
            self.g.round() as u8,
            self.b.round() as u8,
            0,
        ]
    }
}

fn apply_brightness(rgb: [u8; 4], pct: f64) -> [u8; 4] {
    let b = 0.03 + 0.97 * (pct / 100.0);
    [
        (rgb[0] as f64 * b).round() as u8,
        (rgb[1] as f64 * b).round() as u8,
        (rgb[2] as f64 * b).round() as u8,
        0,
    ]
}

fn temp_to_pct(temp: f64) -> f64 {
    ((temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN) * 100.0).clamp(0.0, 100.0)
}

struct LedSpan {
    label: String,
    start: usize,
    len: usize,
}

impl LedSpan {
    fn from_zone(zone: &openrgb::Zone) -> Self {
        Self {
            label: zone.name.clone(),
            start: zone.start,
            len: zone.count,
        }
    }

    fn range(&self) -> Range<usize> {
        self.start..self.start + self.len
    }
}

struct LedLayout {
    led_count: usize,
    cpu_usage: LedSpan,
    ram_usage: LedSpan,
    gpu_busy: LedSpan,
    gpu_vram: LedSpan,
    cpu_temp: Option<LedSpan>,
    gpu_temp: Option<LedSpan>,
}

impl LedLayout {
    fn discover(controller: &openrgb::Controller, config: &config::Config) -> Self {
        let aio_zone = choose_aio_zone(controller, config.aio_zone.as_deref())
            .unwrap_or_else(|| panic!("sysmon: no usable OpenRGB zone found for the main display"));
        let active_aio_leds = config
            .aio_leds
            .unwrap_or(DEFAULT_AIO_ACTIVE_LEDS)
            .min(aio_zone.count);

        let pump_len = active_aio_leds / 3;
        let gpu_busy_len = (active_aio_leds - pump_len) / 2;
        let gpu_vram_len = active_aio_leds - pump_len - gpu_busy_len;
        let cpu_len = pump_len / 2;
        let ram_len = pump_len - cpu_len;

        let mut start = aio_zone.start;
        let cpu_usage = span_from_parts("CPU usage", start, cpu_len);
        start += cpu_len;
        let ram_usage = span_from_parts("RAM usage", start, ram_len);
        start += ram_len;
        let gpu_busy = span_from_parts("GPU busy", start, gpu_busy_len);
        start += gpu_busy_len;
        let gpu_vram = span_from_parts("GPU VRAM", start, gpu_vram_len);

        let cpu_temp = choose_zone(
            controller,
            config.cpu_temp_zone.as_deref(),
            &["PCB", "IO Cover"],
        )
        .map(LedSpan::from_zone);
        let gpu_temp = choose_zone(
            controller,
            config.gpu_temp_zone.as_deref(),
            &["IO Cover", "PCB"],
        )
        .map(LedSpan::from_zone);

        eprintln!(
            "sysmon: LED layout: main='{}' active_leds={}, cpu_temp='{}', gpu_temp='{}'",
            aio_zone.name,
            active_aio_leds,
            cpu_temp
                .as_ref()
                .map(|span| span.label.as_str())
                .unwrap_or("none"),
            gpu_temp
                .as_ref()
                .map(|span| span.label.as_str())
                .unwrap_or("none")
        );

        Self {
            led_count: controller.led_count,
            cpu_usage,
            ram_usage,
            gpu_busy,
            gpu_vram,
            cpu_temp,
            gpu_temp,
        }
    }
}

fn span_from_parts(label: &str, start: usize, len: usize) -> LedSpan {
    LedSpan {
        label: label.to_string(),
        start,
        len,
    }
}

fn choose_aio_zone<'a>(
    controller: &'a openrgb::Controller,
    selector: Option<&str>,
) -> Option<&'a openrgb::Zone> {
    choose_zone(controller, selector, &["Addressable Header 3/Audio"])
        .or_else(|| {
            controller
                .zones
                .iter()
                .filter(|zone| zone.name.to_ascii_lowercase().contains("addressable"))
                .max_by_key(|zone| zone.count)
        })
        .or_else(|| controller.zones.iter().max_by_key(|zone| zone.count))
}

fn choose_zone<'a>(
    controller: &'a openrgb::Controller,
    selector: Option<&str>,
    defaults: &[&str],
) -> Option<&'a openrgb::Zone> {
    if let Some(selector) = selector {
        if let Some(zone) = matching_zone(controller, selector) {
            return Some(zone);
        }
        eprintln!("sysmon: configured OpenRGB zone '{selector}' was not found");
    }

    defaults
        .iter()
        .find_map(|default| matching_zone(controller, default))
}

fn matching_zone<'a>(
    controller: &'a openrgb::Controller,
    selector: &str,
) -> Option<&'a openrgb::Zone> {
    let selector = selector.to_ascii_lowercase();
    controller
        .zones
        .iter()
        .find(|zone| zone.name.to_ascii_lowercase() == selector)
        .or_else(|| {
            controller
                .zones
                .iter()
                .find(|zone| zone.name.to_ascii_lowercase().contains(&selector))
        })
}

fn displayed_gpu_count(gpu_count: usize, led_capacity: usize) -> usize {
    gpu_count.min(led_capacity)
}

fn metric_led_range(span: &LedSpan, metric_index: usize, displayed_metrics: usize) -> Range<usize> {
    let start = span.start + metric_index * span.len / displayed_metrics;
    let end = span.start + (metric_index + 1) * span.len / displayed_metrics;
    start..end
}

fn fill_leds(leds: &mut [[u8; 4]], range: Range<usize>, color: [u8; 4]) {
    for idx in range {
        if let Some(led) = leds.get_mut(idx) {
            *led = color;
        }
    }
}

fn write_metrics(cpu: f64, ram: f64, cpu_temp: f64, gpus: &[gpu::GpuMetrics]) {
    use std::io::Write as _;
    let mut s = String::from("{");
    s.push_str(&format!(
        "\"cpu\":{:.1},\"ram\":{:.1},\"cpu_temp\":{:.1}",
        cpu, ram, cpu_temp
    ));
    for (i, metrics) in gpus.iter().enumerate() {
        s.push_str(&format!(
            ",\"gpu{}_busy\":{:.1},\"gpu{}_vram\":{:.1},\"gpu{}_temp\":{:.1}",
            i, metrics.busy, i, metrics.vram, i, metrics.temp
        ));
    }
    s.push('}');
    let tmp = "/tmp/sysmon-metrics.json.tmp";
    let dst = "/tmp/sysmon-metrics.json";
    if let Ok(mut f) = std::fs::File::create(tmp) {
        let _ = f.write_all(s.as_bytes());
        let _ = std::fs::rename(tmp, dst);
    }
}

fn run_test(rgb: &mut openrgb::Client, layout: &LedLayout) {
    eprintln!("sysmon: running 10-second ramp test (0→100%)");
    let duration_ms = 10_000u64;
    let start = std::time::Instant::now();
    loop {
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= duration_ms {
            break;
        }
        let pct = elapsed as f64 / duration_ms as f64 * 100.0;
        let (r, g, b) = gradient_color(pct);
        let color = apply_brightness([g, r, b, 0], pct); // R/G swapped
        let mut leds = vec![[0u8; 4]; layout.led_count];
        fill_leds(&mut leds, layout.cpu_usage.range(), color);
        fill_leds(&mut leds, layout.ram_usage.range(), color);
        fill_leds(&mut leds, layout.gpu_busy.range(), color);
        fill_leds(&mut leds, layout.gpu_vram.range(), color);
        if let Some(span) = &layout.cpu_temp {
            fill_leds(&mut leds, span.range(), color);
        }
        if let Some(span) = &layout.gpu_temp {
            fill_leds(&mut leds, span.range(), color);
        }
        rgb.send_leds(&leds);
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!("sysmon: test done");
}

fn main() {
    let test_mode = std::env::args().any(|a| a == "--test");

    let config = config::Config::load();
    let mut rgb = openrgb::Client::connect(&config);
    let layout = LedLayout::discover(rgb.controller(), &config);

    if test_mode {
        run_test(&mut rgb, &layout);
        return;
    }

    // Discover hardware
    let mut cpu_reader = cpu::CpuReader::new();
    let mut gpus = gpu::discover();
    let cpu_temp_sensor = temp::CpuTemp::discover();
    let gpu_temp_sensors = gpus.iter().filter(|g| g.has_temp_sensor()).count();
    let displayed_gpus =
        displayed_gpu_count(gpus.len(), layout.gpu_busy.len.min(layout.gpu_vram.len));

    eprintln!(
        "sysmon: {} GPU(s) ({} displayed), cpu_temp={}, gpu_temp_sources={}",
        gpus.len(),
        displayed_gpus,
        if cpu_temp_sensor.is_some() {
            "yes"
        } else {
            "no"
        },
        gpu_temp_sensors
    );
    for (i, gpu) in gpus.iter().enumerate() {
        eprintln!("sysmon: gpu{i}: {}", gpu.description());
    }
    if gpus.len() > displayed_gpus {
        eprintln!("sysmon: only the first {displayed_gpus} GPUs fit on the configured LED zones");
    }

    // Rolling averages for each metric
    let mut r_cpu = RollingAvg::new();
    let mut r_ram = RollingAvg::new();
    let mut r_cpu_temp = RollingAvg::new();
    let mut r_gpu_temp: Vec<RollingAvg> = (0..gpus.len()).map(|_| RollingAvg::new()).collect();
    let mut r_gpu_busy: Vec<RollingAvg> = (0..gpus.len()).map(|_| RollingAvg::new()).collect();
    let mut r_gpu_vram: Vec<RollingAvg> = (0..gpus.len()).map(|_| RollingAvg::new()).collect();

    // Smoothed colors
    let mut c_cpu = SmoothColor::new();
    let mut c_ram = SmoothColor::new();
    let mut c_cpu_temp = SmoothColor::new();
    let mut c_gpu_temp = SmoothColor::new();
    let mut c_gpu_busy: Vec<SmoothColor> = (0..gpus.len()).map(|_| SmoothColor::new()).collect();
    let mut c_gpu_vram: Vec<SmoothColor> = (0..gpus.len()).map(|_| SmoothColor::new()).collect();

    let mut gpu_metrics = vec![gpu::GpuMetrics::default(); gpus.len()];
    let mut tick = 0u32;
    loop {
        // Read metrics every tick, smooth via rolling average
        let s_cpu = r_cpu.push(cpu_reader.read());
        let s_ram = r_ram.push(mem::read());
        let s_cpu_temp = r_cpu_temp.push(cpu_temp_sensor.as_ref().map(|s| s.read()).unwrap_or(0.0));

        let mut hottest_gpu_temp = 0.0f64;
        for (i, g) in gpus.iter_mut().enumerate() {
            let metrics = g.read_metrics();
            let s_busy = r_gpu_busy[i].push(metrics.busy);
            let s_vram = r_gpu_vram[i].push(metrics.vram);
            let s_temp = r_gpu_temp[i].push(metrics.temp);
            hottest_gpu_temp = hottest_gpu_temp.max(s_temp);
            gpu_metrics[i] = gpu::GpuMetrics {
                busy: s_busy,
                vram: s_vram,
                temp: s_temp,
            };
        }

        // Build LED buffer
        let mut leds = vec![[0u8; 4]; layout.led_count];

        // CPU temperature (R/G swapped)
        let cpu_temp_pct = temp_to_pct(s_cpu_temp);
        let cpu_temp_rgb = apply_brightness(
            c_cpu_temp.update(gradient_color(cpu_temp_pct)),
            cpu_temp_pct,
        );
        if let Some(span) = &layout.cpu_temp {
            fill_leds(
                &mut leds,
                span.range(),
                [cpu_temp_rgb[1], cpu_temp_rgb[0], cpu_temp_rgb[2], 0],
            );
        }

        // GPU temperature (R/G swapped)
        let gpu_temp_pct = temp_to_pct(hottest_gpu_temp);
        let gpu_temp_rgb = apply_brightness(
            c_gpu_temp.update(gradient_color(gpu_temp_pct)),
            gpu_temp_pct,
        );
        if let Some(span) = &layout.gpu_temp {
            fill_leds(
                &mut leds,
                span.range(),
                [gpu_temp_rgb[1], gpu_temp_rgb[0], gpu_temp_rgb[2], 0],
            );
        }

        // AIO pump: CPU + RAM (R/G swapped for AIO zone)
        let cpu_rgb = apply_brightness(c_cpu.update(gradient_color(s_cpu)), s_cpu);
        fill_leds(
            &mut leds,
            layout.cpu_usage.range(),
            [cpu_rgb[1], cpu_rgb[0], cpu_rgb[2], 0],
        );

        let ram_rgb = apply_brightness(c_ram.update(gradient_color(s_ram)), s_ram);
        fill_leds(
            &mut leds,
            layout.ram_usage.range(),
            [ram_rgb[1], ram_rgb[0], ram_rgb[2], 0],
        );

        // Bottom fan: GPU busy, split across however many GPUs fit.
        for (i, c) in c_gpu_busy.iter_mut().take(displayed_gpus).enumerate() {
            let s_busy = gpu_metrics[i].busy;
            let rgb = apply_brightness(c.update(gradient_color(s_busy)), s_busy);
            fill_leds(
                &mut leds,
                metric_led_range(&layout.gpu_busy, i, displayed_gpus),
                [rgb[1], rgb[0], rgb[2], 0],
            );
        }

        // Top fan: GPU VRAM, split across however many GPUs fit.
        for (i, c) in c_gpu_vram.iter_mut().take(displayed_gpus).enumerate() {
            let s_vram = gpu_metrics[i].vram;
            let rgb = apply_brightness(c.update(gradient_color(s_vram)), s_vram);
            fill_leds(
                &mut leds,
                metric_led_range(&layout.gpu_vram, i, displayed_gpus),
                [rgb[1], rgb[0], rgb[2], 0],
            );
        }

        rgb.send_leds(&leds);

        // Write metrics JSON every ~1 second
        if tick % 100 == 0 {
            write_metrics(s_cpu, s_ram, s_cpu_temp, &gpu_metrics);
        }

        tick = tick.wrapping_add(1);
        thread::sleep(Duration::from_millis(POLL_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ZONE_LEDS: usize = 12;

    fn test_span() -> LedSpan {
        LedSpan {
            label: "test".to_string(),
            start: 0,
            len: TEST_ZONE_LEDS,
        }
    }

    fn zone_spans(displayed_gpus: usize) -> Vec<Vec<usize>> {
        let span = test_span();
        (0..displayed_gpus)
            .map(|i| metric_led_range(&span, i, displayed_gpus).collect())
            .collect()
    }

    #[test]
    fn display_count_is_limited_by_physical_leds() {
        assert_eq!(displayed_gpu_count(0, TEST_ZONE_LEDS), 0);
        assert_eq!(displayed_gpu_count(5, TEST_ZONE_LEDS), 5);
        assert_eq!(displayed_gpu_count(20, TEST_ZONE_LEDS), TEST_ZONE_LEDS);
    }

    #[test]
    fn gpu_led_ranges_fill_zone_for_common_gpu_counts() {
        assert_eq!(
            zone_spans(1),
            vec![vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]]
        );
        assert_eq!(
            zone_spans(2),
            vec![vec![0, 1, 2, 3, 4, 5], vec![6, 7, 8, 9, 10, 11]]
        );
        assert_eq!(
            zone_spans(3),
            vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8, 9, 10, 11]]
        );
        assert_eq!(
            zone_spans(4),
            vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8], vec![9, 10, 11]]
        );
    }

    #[test]
    fn gpu_led_ranges_keep_every_displayed_gpu_visible() {
        for displayed_gpus in 1..=TEST_ZONE_LEDS {
            let spans = zone_spans(displayed_gpus);
            assert!(spans.iter().all(|span| !span.is_empty()));

            let covered: Vec<_> = spans.into_iter().flatten().collect();
            assert_eq!(covered, (0..TEST_ZONE_LEDS).collect::<Vec<_>>());
        }
    }
}
