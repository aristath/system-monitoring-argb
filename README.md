# sysmon

A lightweight system monitoring tool that drives motherboard ARGB LEDs to display real-time hardware metrics. Written in Rust with zero dependencies.

## What it does

Sysmon reads CPU, RAM, GPU utilization, and temperature data from the Linux kernel and maps them to ARGB LED colors via [OpenRGB](https://openrgb.org/). At a glance, you can see how hard your system is working by the color of the LEDs — blue means idle, red means hot.

## Hardware

Built for the following setup, but adaptable to other configurations:

- **Motherboard**: ASRock X870E Taichi (OpenRGB device 0, 265 LEDs)
- **AIO Cooler**: Arctic Liquid Freezer III Pro 280 A-RGB (36 LEDs: 12 pump + 12 per fan)
- **GPUs**: Up to 3 discrete AMD GPUs (detected via sysfs, >4GB VRAM filter)
- **CPU temp sensor**: AMD k10temp (Tctl)
- **GPU temp sensors**: amdgpu hwmon (junction preferred, edge fallback)

## LED Layout

| Zone | LEDs | Metric |
|------|------|--------|
| AIO pump head | 0–5 | CPU usage |
| AIO pump head | 6–11 | RAM usage |
| AIO bottom fan | 12–23 | GPU busy (4 LEDs per GPU) |
| AIO top fan | 24–35 | GPU VRAM (4 LEDs per GPU) |
| IO Cover | 176–184 | CPU temperature |
| PCB strip | 161–175 | GPU temperature (hottest) |

## Color Gradient

Colors transition smoothly from cool to critical:

| Load | Color |
|------|-------|
| 0% | Blue |
| 20% | Cyan |
| 40% | Green |
| 50% | Yellow |
| 60% | Orange |
| 80% | Red |
| 90–100% | Deep red |

Two-layer smoothing ensures silky transitions:
1. **Metric smoothing** (EMA alpha=0.3, delta cap=3%/tick) — stabilizes noisy sensor readings
2. **RGB color smoothing** (alpha=0.08) — blends LED colors across gradient stop boundaries

## How it works

1. Connects to the OpenRGB server via TCP (localhost:6742)
2. Sets the device to Direct mode for flash-free updates
3. Every 100ms:
   - Reads CPU, RAM, GPU busy, GPU VRAM, and temperatures from `/proc` and `/sys`
   - Smooths the values with EMA + delta capping
   - Maps percentages to gradient colors with additional RGB smoothing
   - Sends all 265 LED colors atomically via OpenRGB's `UpdateLEDs` packet (1050)
4. Writes a JSON metrics snapshot to `/tmp/sysmon-metrics.json` every ~1 second

## Requirements

- Linux (reads from `/proc/stat`, `/proc/meminfo`, `/sys/class/drm`, `/sys/class/hwmon`)
- [OpenRGB](https://openrgb.org/) server running on localhost:6742
- `openrgb` CLI available in PATH (used once at startup to set Direct mode)
- Rust 2024 edition

## Build & Run

```bash
cargo build --release
./target/release/sysmon
```

## systemd Service

To run automatically at login, create `~/.config/systemd/user/sysmon.service`:

```ini
[Unit]
Description=System Monitor LEDs
After=graphical-session.target openrgb-server.service

[Service]
ExecStart=/path/to/sysmon/target/release/sysmon
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
```

Then enable and start:

```bash
systemctl --user enable --now sysmon
```

## Metrics JSON

The JSON written to `/tmp/sysmon-metrics.json` looks like:

```json
{"cpu":12.3,"ram":45.6,"cpu_temp":52.1,"gpu0_busy":30.0,"gpu0_vram":75.2,"gpu0_temp":47.0,"gpu1_busy":25.0,"gpu1_vram":80.1,"gpu1_temp":48.0}
```

This can be consumed by other tools (e.g., a GNOME Shell extension) for on-screen display.

## Customization

All tuning values are `const` at the top of `src/main.rs`:

- `POLL_MS` — polling interval (default: 100ms)
- `EMA_ALPHA` / `DELTA_CAP` — metric smoothing aggressiveness
- `COLOR_ALPHA` — RGB color transition speed
- `TEMP_MIN` / `TEMP_MAX` — temperature-to-percentage mapping range
- `GRADIENT` — the 11-stop color table
- LED index constants — remap to match your hardware layout
