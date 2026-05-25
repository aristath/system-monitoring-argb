# sysmon

A lightweight system monitoring tool that drives motherboard ARGB LEDs to display real-time hardware metrics. Written in Rust with zero dependencies.

## What it does

Sysmon reads CPU, RAM, GPU utilization, and temperature data from the Linux kernel and maps them to ARGB LED colors via [OpenRGB](https://openrgb.org/). At a glance, you can see how hard your system is working by the color of the LEDs — blue means idle, red means hot.

## Hardware

Sysmon discovers most hardware at startup:

- **OpenRGB controller**: selected by `SYSMON_OPENRGB_DEVICE` when set, otherwise the controller with the best addressable-LED match
- **LED count and zone ranges**: read from the OpenRGB SDK controller data
- **GPUs**: Discrete AMD GPUs from sysfs plus NVIDIA GPUs from `nvidia-smi`, discovered at startup
- **CPU temp sensor**: AMD k10temp Tctl or Intel coretemp package sensor
- **GPU temp sensors**: per-card amdgpu hwmon (junction preferred, edge fallback) or `nvidia-smi`

The only layout value OpenRGB cannot know is how many LEDs are physically attached to an addressable header. Use `SYSMON_AIO_LEDS` for that when the default is not right.

## LED Layout

The default layout is name-based, not offset-based:

| Role | Default OpenRGB zone |
|------|----------------------|
| Main CPU/RAM/GPU display | `Addressable Header 3/Audio`, then largest addressable zone |
| CPU temperature | `PCB`, then `IO Cover` |
| GPU temperature | `IO Cover`, then `PCB` |

The main display zone is divided into three sections: pump metrics, GPU busy, and GPU VRAM. The pump section is split between CPU and RAM. The GPU busy and VRAM sections are divided from the discovered GPU count at startup, down to one LED per displayed GPU.

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

Two-layer smoothing keeps transitions calm:
1. **Metric smoothing**: a 200-sample rolling average, currently 2 seconds at a 10ms poll interval
2. **RGB color smoothing**: alpha=0.03 blends LED colors across gradient stop boundaries

## How it works

1. Connects to the OpenRGB server via TCP (localhost:6742)
2. Sets the device to Direct mode for flash-free updates
3. Every 10ms:
   - Reads CPU, RAM, AMD GPU busy/VRAM/temperature, and per-GPU temperatures from `/proc` and `/sys`
   - Reads NVIDIA GPU busy/VRAM/temperature from `nvidia-smi` on a 1-second cache
   - Smooths the values with a rolling average and RGB color blending
   - Maps percentages to gradient colors with additional RGB smoothing
   - Sends the controller-reported number of LED colors atomically via OpenRGB's `UpdateLEDs` packet (1050)
4. Writes a JSON metrics snapshot to `/tmp/sysmon-metrics.json` every ~1 second

## Requirements

- Linux (reads from `/proc/stat`, `/proc/meminfo`, `/sys/class/drm`, `/sys/class/hwmon`)
- [OpenRGB](https://openrgb.org/) server running on localhost:6742
- `nvidia-smi` available in PATH if NVIDIA GPUs should be included
- Rust 2024 edition

## Configuration

Configuration is optional. Values can come from environment variables or from `~/.config/sysmon/config.env`; environment variables win. Use `SYSMON_CONFIG=/path/to/file` to load a different config file.

```env
openrgb_host=127.0.0.1
openrgb_port=6742
openrgb_device=ASRock
aio_zone=Addressable Header 3/Audio
aio_leds=36
cpu_temp_zone=PCB
gpu_temp_zone=IO Cover
```

Supported environment variables:

- `SYSMON_OPENRGB_HOST`
- `SYSMON_OPENRGB_PORT`
- `SYSMON_OPENRGB_DEVICE`
- `SYSMON_AIO_ZONE`
- `SYSMON_AIO_LEDS`
- `SYSMON_CPU_TEMP_ZONE`
- `SYSMON_GPU_TEMP_ZONE`

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
{"cpu":12.3,"ram":45.6,"cpu_temp":52.1,"gpu0_busy":30.0,"gpu0_vram":75.2,"gpu0_temp":47.0,"gpu1_busy":25.0,"gpu1_vram":80.1,"gpu1_temp":48.0,"gpu2_busy":10.0,"gpu2_vram":44.0,"gpu2_temp":42.0,"gpu3_busy":60.0,"gpu3_vram":66.0,"gpu3_temp":57.0,"gpu4_busy":5.0,"gpu4_vram":0.1,"gpu4_temp":49.0}
```

This can be consumed by other tools (e.g., a GNOME Shell extension) for on-screen display.

## Customization

All tuning values are `const` at the top of `src/main.rs`:

- `POLL_MS` — polling interval (default: 10ms)
- `WINDOW_SIZE` — metric smoothing window
- `COLOR_ALPHA` — RGB color transition speed
- `TEMP_MIN` / `TEMP_MAX` — temperature-to-percentage mapping range
- `GRADIENT` — the 11-stop color table

GPU count, OpenRGB controller index, total LED count, and zone offsets are not tuning constants. The service discovers them on startup.
