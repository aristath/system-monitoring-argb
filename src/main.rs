use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

mod cpu;
mod gpu;
mod mem;
mod temp;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 6742;
const DEVICE: u32 = 0;
const DEVICE_TOTAL_LEDS: u16 = 265;

// Zone offsets in device LED array
const AIO_OFFSET: usize = 185;
const IO_COVER_OFFSET: usize = 176;  // 9 LEDs (176-184) — CPU temp
const PCB_OFFSET: usize = 161;       // 15 LEDs (161-175) — GPU temp

const IO_COVER_LEDS: usize = 9;
const PCB_LEDS: usize = 15;

// AIO LED layout (relative to zone 5, add AIO_OFFSET for device index)
// Pump head: 0-11 (CPU + RAM)
const CPU_LEDS: [usize; 6] = [0, 1, 2, 3, 4, 5];
const RAM_LEDS: [usize; 6] = [6, 7, 8, 9, 10, 11];
// Bottom fan: 12-23 (GPU busy, 4 LEDs per GPU)
const GPU_BUSY_START: usize = 12;
// Top fan: 24-35 (GPU VRAM, 4 LEDs per GPU)
const GPU_VRAM_START: usize = 24;
const LEDS_PER_GPU: usize = 4;

// Smoothing — metric values
const POLL_MS: u64 = 100;
const EMA_ALPHA: f64 = 0.3;
const DELTA_CAP: f64 = 3.0; // max % change per tick

// Smoothing — RGB color (visual smoothness)
const COLOR_ALPHA: f64 = 0.08;

// Temperature range (°C)
const TEMP_MIN: f64 = 35.0;
const TEMP_MAX: f64 = 90.0;

// Color gradient: 11 stops from 0% to 100%
const GRADIENT: [(u8, u8, u8); 11] = [
    (0, 0, 255),     // 0%   blue
    (0, 128, 255),   // 10%  blue-cyan
    (0, 255, 255),   // 20%  cyan
    (0, 255, 128),   // 30%  cyan-green
    (0, 255, 0),     // 40%  green
    (255, 255, 0),   // 50%  yellow
    (255, 128, 0),   // 60%  orange
    (255, 64, 0),    // 70%  orange-red
    (255, 0, 0),     // 80%  red
    (128, 0, 0),     // 90%  deep red
    (128, 0, 0),     // 100% deep red
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

fn smooth(current: f64, target: f64) -> f64 {
    let ema = current + EMA_ALPHA * (target - current);
    let delta = (ema - current).clamp(-DELTA_CAP, DELTA_CAP);
    current + delta
}

struct SmoothColor {
    r: f64,
    g: f64,
    b: f64,
}

impl SmoothColor {
    fn new() -> Self {
        Self { r: 0.0, g: 0.0, b: 0.0 }
    }

    fn update(&mut self, target: (u8, u8, u8)) -> [u8; 4] {
        self.r += COLOR_ALPHA * (target.0 as f64 - self.r);
        self.g += COLOR_ALPHA * (target.1 as f64 - self.g);
        self.b += COLOR_ALPHA * (target.2 as f64 - self.b);
        [self.r.round() as u8, self.g.round() as u8, self.b.round() as u8, 0]
    }
}

fn temp_to_pct(temp: f64) -> f64 {
    ((temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN) * 100.0).clamp(0.0, 100.0)
}

fn send_packet(stream: &mut TcpStream, dev: u32, id: u32, data: &[u8]) {
    let mut pkt = Vec::with_capacity(16 + data.len());
    pkt.extend_from_slice(b"ORGB");
    pkt.extend_from_slice(&dev.to_le_bytes());
    pkt.extend_from_slice(&id.to_le_bytes());
    pkt.extend_from_slice(&(data.len() as u32).to_le_bytes());
    pkt.extend_from_slice(data);
    let _ = stream.write_all(&pkt);
}

fn read_response(stream: &mut TcpStream) -> Vec<u8> {
    let mut hdr = [0u8; 16];
    stream.read_exact(&mut hdr).unwrap();
    let size = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
    let mut data = vec![0u8; size];
    if size > 0 { stream.read_exact(&mut data).unwrap(); }
    data
}

fn write_metrics(cpu: f64, ram: f64, cpu_temp: f64, gpus: &[(f64, f64, f64)]) {
    use std::io::Write as _;
    let mut s = String::from("{");
    s.push_str(&format!("\"cpu\":{:.1},\"ram\":{:.1},\"cpu_temp\":{:.1}", cpu, ram, cpu_temp));
    for (i, (busy, vram, temp)) in gpus.iter().enumerate() {
        s.push_str(&format!(",\"gpu{}_busy\":{:.1},\"gpu{}_vram\":{:.1},\"gpu{}_temp\":{:.1}",
            i, busy, i, vram, i, temp));
    }
    s.push('}');
    let tmp = "/tmp/sysmon-metrics.json.tmp";
    let dst = "/tmp/sysmon-metrics.json";
    if let Ok(mut f) = std::fs::File::create(tmp) {
        let _ = f.write_all(s.as_bytes());
        let _ = std::fs::rename(tmp, dst);
    }
}

fn main() {
    // Wait for OpenRGB server to be ready
    let mut stream = loop {
        match TcpStream::connect(format!("{HOST}:{PORT}")) {
            Ok(s) => break s,
            Err(_) => {
                eprintln!("sysmon: waiting for OpenRGB server...");
                thread::sleep(Duration::from_secs(2));
            }
        }
    };

    // Set direct mode (server is up, but give it time to detect hardware)
    thread::sleep(Duration::from_secs(3));
    std::process::Command::new("openrgb")
        .args(["--device", "0", "--mode", "direct"])
        .output()
        .unwrap();
    thread::sleep(Duration::from_secs(1));
    send_packet(&mut stream, 0, 40, &4u32.to_le_bytes());
    read_response(&mut stream);
    send_packet(&mut stream, 0, 50, b"sysmon\0");
    send_packet(&mut stream, 0, 0, &[]);
    read_response(&mut stream);
    send_packet(&mut stream, DEVICE, 1, &[]);
    read_response(&mut stream);

    // Discover hardware
    let mut cpu_reader = cpu::CpuReader::new();
    let gpus = gpu::discover();
    let cpu_temp_sensor = temp::CpuTemp::discover();
    let gpu_temp_sensor = temp::GpuTemp::discover();

    eprintln!("sysmon: {} GPU(s), cpu_temp={}, gpu_temp_sensors={}",
        gpus.len(),
        if cpu_temp_sensor.is_some() { "yes" } else { "no" },
        gpu_temp_sensor.sensor_count());

    // Smoothed values
    let mut s_cpu = 0.0f64;
    let mut s_ram = 0.0f64;
    let mut s_gpu_busy = vec![0.0f64; gpus.len()];
    let mut s_gpu_vram = vec![0.0f64; gpus.len()];
    let mut s_cpu_temp = 0.0f64;
    let mut s_gpu_temp = 0.0f64;

    // Smoothed colors
    let mut c_cpu = SmoothColor::new();
    let mut c_ram = SmoothColor::new();
    let mut c_cpu_temp = SmoothColor::new();
    let mut c_gpu_temp = SmoothColor::new();
    let mut c_gpu_busy: Vec<SmoothColor> = (0..gpus.len()).map(|_| SmoothColor::new()).collect();
    let mut c_gpu_vram: Vec<SmoothColor> = (0..gpus.len()).map(|_| SmoothColor::new()).collect();

    // LED buffer: 265 LEDs × 4 bytes (R,G,B,0) + header (4 + 2)
    let n = DEVICE_TOTAL_LEDS;
    let data_size = 4 + 2 + (n as u32) * 4;

    let mut tick = 0u32;
    loop {
        // Read raw metrics
        let raw_cpu = cpu_reader.read();
        let raw_ram = mem::read();
        let raw_cpu_temp = cpu_temp_sensor.as_ref().map(|s| s.read()).unwrap_or(0.0);
        let raw_gpu_temp = gpu_temp_sensor.read_hottest();

        // Smooth
        s_cpu = smooth(s_cpu, raw_cpu);
        s_ram = smooth(s_ram, raw_ram);
        s_cpu_temp = smooth(s_cpu_temp, raw_cpu_temp);
        s_gpu_temp = smooth(s_gpu_temp, raw_gpu_temp);

        let mut gpu_metrics = Vec::with_capacity(gpus.len());
        for (i, g) in gpus.iter().enumerate() {
            let raw_busy = g.read_busy();
            let raw_vram = g.read_vram_percent();
            s_gpu_busy[i] = smooth(s_gpu_busy[i], raw_busy);
            s_gpu_vram[i] = smooth(s_gpu_vram[i], raw_vram);
            gpu_metrics.push((s_gpu_busy[i], s_gpu_vram[i], raw_gpu_temp));
        }

        // Build LED buffer
        let mut buf = Vec::with_capacity(data_size as usize);
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend_from_slice(&n.to_le_bytes());

        // Start with all LEDs off
        let mut leds = [[0u8; 4]; 265];

        // IO Cover (176-184): CPU temperature (R/G swapped)
        let cpu_temp_rgb = c_cpu_temp.update(gradient_color(temp_to_pct(s_cpu_temp)));
        for i in 0..IO_COVER_LEDS {
            leds[IO_COVER_OFFSET + i] = [cpu_temp_rgb[1], cpu_temp_rgb[0], cpu_temp_rgb[2], 0];
        }

        // PCB (161-175): GPU temperature (R/G swapped)
        let gpu_temp_rgb = c_gpu_temp.update(gradient_color(temp_to_pct(s_gpu_temp)));
        for i in 0..PCB_LEDS {
            leds[PCB_OFFSET + i] = [gpu_temp_rgb[1], gpu_temp_rgb[0], gpu_temp_rgb[2], 0];
        }

        // AIO pump: CPU + RAM (R/G swapped for AIO zone)
        let cpu_rgb = c_cpu.update(gradient_color(s_cpu));
        for &idx in &CPU_LEDS {
            leds[AIO_OFFSET + idx] = [cpu_rgb[1], cpu_rgb[0], cpu_rgb[2], 0];
        }

        let ram_rgb = c_ram.update(gradient_color(s_ram));
        for &idx in &RAM_LEDS {
            leds[AIO_OFFSET + idx] = [ram_rgb[1], ram_rgb[0], ram_rgb[2], 0];
        }

        // Bottom fan: GPU busy (4 LEDs per GPU, R/G swapped)
        for (i, c) in c_gpu_busy.iter_mut().enumerate() {
            let rgb = c.update(gradient_color(s_gpu_busy[i]));
            let start = GPU_BUSY_START + i * LEDS_PER_GPU;
            for j in 0..LEDS_PER_GPU {
                leds[AIO_OFFSET + start + j] = [rgb[1], rgb[0], rgb[2], 0];
            }
        }

        // Top fan: GPU VRAM (4 LEDs per GPU, R/G swapped)
        for (i, c) in c_gpu_vram.iter_mut().enumerate() {
            let rgb = c.update(gradient_color(s_gpu_vram[i]));
            let start = GPU_VRAM_START + i * LEDS_PER_GPU;
            for j in 0..LEDS_PER_GPU {
                leds[AIO_OFFSET + start + j] = [rgb[1], rgb[0], rgb[2], 0];
            }
        }

        // Serialize LED data
        for led in &leds {
            buf.extend_from_slice(led);
        }

        send_packet(&mut stream, DEVICE, 1050, &buf);

        // Write metrics JSON every ~1 second
        if tick % 10 == 0 {
            write_metrics(s_cpu, s_ram, s_cpu_temp, &gpu_metrics);
        }

        tick = tick.wrapping_add(1);
        thread::sleep(Duration::from_millis(POLL_MS));
    }
}
