use crate::config::Config;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

const CLIENT_PROTOCOL_VERSION: u32 = 5;

const REQUEST_CONTROLLER_COUNT: u32 = 0;
const REQUEST_CONTROLLER_DATA: u32 = 1;
const REQUEST_PROTOCOL_VERSION: u32 = 40;
const SET_CLIENT_NAME: u32 = 50;
const SET_CUSTOM_MODE: u32 = 1100;
const UPDATE_MODE: u32 = 1101;
const UPDATE_LEDS: u32 = 1050;

pub struct Client {
    stream: TcpStream,
    protocol_version: u32,
    controller: Controller,
}

#[derive(Clone)]
pub struct Controller {
    pub id: u32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub location: String,
    pub active_mode: i32,
    pub modes: Vec<Mode>,
    pub led_count: usize,
    pub zones: Vec<Zone>,
}

#[derive(Clone, Debug)]
pub struct Mode {
    pub index: usize,
    pub name: String,
    pub value: i32,
    data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub index: usize,
    pub name: String,
    pub start: usize,
    pub count: usize,
}

impl Client {
    pub fn connect(config: &Config) -> Self {
        let addr = format!("{}:{}", config.openrgb_host, config.openrgb_port);
        let mut stream = loop {
            match TcpStream::connect(&addr) {
                Ok(stream) => break stream,
                Err(err) => {
                    eprintln!("sysmon: waiting for OpenRGB SDK at {addr}: {err}");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        };

        let protocol_version = negotiate_protocol(&mut stream).unwrap_or(0);
        send_packet(&mut stream, 0, SET_CLIENT_NAME, b"sysmon\0").ok();

        let controllers = read_controllers(&mut stream, protocol_version).unwrap_or_else(|err| {
            eprintln!("sysmon: failed to read OpenRGB controller list: {err}");
            Vec::new()
        });
        let controller = select_controller(controllers, config.openrgb_device.as_deref())
            .unwrap_or_else(|| panic!("sysmon: no usable OpenRGB controllers found"));

        set_direct_mode(&mut stream, &controller);

        eprintln!(
            "sysmon: OpenRGB controller {}: {} ({} LEDs, {} zones, active_mode={})",
            controller.id,
            controller.name,
            controller.led_count,
            controller.zones.len(),
            controller.active_mode
        );

        Self {
            stream,
            protocol_version,
            controller,
        }
    }

    pub fn controller(&self) -> &Controller {
        &self.controller
    }

    pub fn send_leds(&mut self, leds: &[[u8; 4]]) {
        let n = leds.len() as u16;
        let data_size = 4 + 2 + (n as u32) * 4;
        let mut buf = Vec::with_capacity(data_size as usize);
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend_from_slice(&n.to_le_bytes());
        for led in leds {
            buf.extend_from_slice(led);
        }
        send_packet(&mut self.stream, self.controller.id, UPDATE_LEDS, &buf).ok();
    }

    #[allow(dead_code)]
    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
}

fn set_direct_mode(stream: &mut TcpStream, controller: &Controller) {
    if let Err(err) = send_packet(stream, controller.id, SET_CUSTOM_MODE, &[]) {
        eprintln!("sysmon: failed to request OpenRGB custom mode: {err}");
    }

    let Some(mode) = controller.direct_mode() else {
        eprintln!(
            "sysmon: OpenRGB controller {} has no Direct mode; LED updates may flicker",
            controller.id
        );
        return;
    };

    let data_size = 4 + 4 + mode.data.len() as u32;
    let mut data = Vec::with_capacity(data_size as usize);
    data.extend_from_slice(&data_size.to_le_bytes());
    data.extend_from_slice(&(mode.index as i32).to_le_bytes());
    data.extend_from_slice(&mode.data);

    if let Err(err) = send_packet(stream, controller.id, UPDATE_MODE, &data) {
        eprintln!(
            "sysmon: failed to apply OpenRGB Direct mode '{}' on controller {}: {err}",
            mode.name, controller.id
        );
    } else {
        eprintln!(
            "sysmon: requested OpenRGB Direct mode '{}' (index {}, value {}) on controller {}",
            mode.name, mode.index, mode.value, controller.id
        );
    }
}

impl Controller {
    fn direct_mode(&self) -> Option<&Mode> {
        self.modes
            .iter()
            .find(|mode| mode.name.eq_ignore_ascii_case("direct"))
            .or_else(|| {
                self.modes
                    .iter()
                    .find(|mode| mode.name.to_ascii_lowercase().contains("direct"))
            })
    }
}

fn negotiate_protocol(stream: &mut TcpStream) -> io::Result<u32> {
    send_packet(
        stream,
        0,
        REQUEST_PROTOCOL_VERSION,
        &CLIENT_PROTOCOL_VERSION.to_le_bytes(),
    )?;
    let data = read_response(stream)?;
    let Some(version) = data.get(..4) else {
        return Ok(0);
    };
    let version = u32::from_le_bytes(version.try_into().unwrap());
    Ok(version.min(CLIENT_PROTOCOL_VERSION))
}

fn read_controllers(stream: &mut TcpStream, protocol_version: u32) -> io::Result<Vec<Controller>> {
    send_packet(stream, 0, REQUEST_CONTROLLER_COUNT, &[])?;
    let data = read_response(stream)?;
    let count = data
        .get(..4)
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()))
        .unwrap_or(0);

    let mut controllers = Vec::new();
    for id in 0..count {
        let request_data = if protocol_version == 0 {
            Vec::new()
        } else {
            protocol_version.to_le_bytes().to_vec()
        };
        send_packet(stream, id, REQUEST_CONTROLLER_DATA, &request_data)?;
        let data = read_response(stream)?;
        match parse_controller(id, protocol_version, &data) {
            Ok(controller) => controllers.push(controller),
            Err(err) => eprintln!("sysmon: skipped OpenRGB controller {id}: {err}"),
        }
    }

    Ok(controllers)
}

fn select_controller(controllers: Vec<Controller>, selector: Option<&str>) -> Option<Controller> {
    if let Some(selector) = selector {
        if let Ok(id) = selector.parse::<u32>() {
            if let Some(controller) = controllers.iter().find(|controller| controller.id == id) {
                return Some(controller.clone());
            }
        }

        let selector = selector.to_ascii_lowercase();
        if let Some(controller) = controllers.iter().find(|controller| {
            controller.name.to_ascii_lowercase().contains(&selector)
                || controller.vendor.to_ascii_lowercase().contains(&selector)
                || controller
                    .description
                    .to_ascii_lowercase()
                    .contains(&selector)
                || controller.location.to_ascii_lowercase().contains(&selector)
        }) {
            return Some(controller.clone());
        }

        eprintln!("sysmon: configured OpenRGB device '{selector}' was not found");
    }

    controllers.into_iter().max_by_key(|controller| {
        let addressable_zones = controller
            .zones
            .iter()
            .filter(|zone| zone.name.to_ascii_lowercase().contains("addressable"))
            .count();
        (addressable_zones, controller.led_count)
    })
}

fn send_packet(stream: &mut TcpStream, dev: u32, id: u32, data: &[u8]) -> io::Result<()> {
    let mut pkt = Vec::with_capacity(16 + data.len());
    pkt.extend_from_slice(b"ORGB");
    pkt.extend_from_slice(&dev.to_le_bytes());
    pkt.extend_from_slice(&id.to_le_bytes());
    pkt.extend_from_slice(&(data.len() as u32).to_le_bytes());
    pkt.extend_from_slice(data);
    stream.write_all(&pkt)
}

fn read_response(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut hdr = [0u8; 16];
    stream.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"ORGB" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid OpenRGB packet magic",
        ));
    }

    let size = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
    let mut data = vec![0u8; size];
    if size > 0 {
        stream.read_exact(&mut data)?;
    }
    Ok(data)
}

fn parse_controller(id: u32, protocol_version: u32, data: &[u8]) -> Result<Controller, String> {
    let mut cursor = Cursor::new(data);
    let _data_size = cursor.u32()?;
    let _device_type = cursor.i32()?;
    let name = cursor.string()?;
    let vendor = if protocol_version >= 1 {
        cursor.string()?
    } else {
        String::new()
    };
    let description = cursor.string()?;
    let _version = cursor.string()?;
    let _serial = cursor.string()?;
    let location = cursor.string()?;

    let mode_count = cursor.u16()? as usize;
    let active_mode = cursor.i32()?;
    let mut modes = Vec::with_capacity(mode_count);
    for index in 0..mode_count {
        modes.push(parse_mode(&mut cursor, protocol_version, index)?);
    }

    let zone_count = cursor.u16()? as usize;
    let mut zones = Vec::with_capacity(zone_count);
    for index in 0..zone_count {
        zones.push(parse_zone(&mut cursor, protocol_version, index)?);
    }

    let led_count = cursor.u16()? as usize;
    let mut first_led_by_zone = vec![None; zones.len()];
    let mut led_count_by_zone = vec![0usize; zones.len()];
    for led_index in 0..led_count {
        let _led_name = cursor.string()?;
        let zone_index = cursor.u32()? as usize;
        if zone_index < zones.len() {
            first_led_by_zone[zone_index].get_or_insert(led_index);
            led_count_by_zone[zone_index] += 1;
        }
    }

    let mut fallback_start = 0usize;
    for zone in &mut zones {
        if let Some(start) = first_led_by_zone[zone.index] {
            zone.start = start;
            zone.count = led_count_by_zone[zone.index];
        } else {
            zone.start = fallback_start;
        }
        fallback_start += zone.count;
    }

    Ok(Controller {
        id,
        name,
        vendor,
        description,
        location,
        active_mode,
        modes,
        led_count,
        zones,
    })
}

fn parse_mode(
    cursor: &mut Cursor<'_>,
    protocol_version: u32,
    index: usize,
) -> Result<Mode, String> {
    let start = cursor.offset;
    let name = cursor.string()?;
    let value = cursor.i32()?;
    let _flags = cursor.u32()?;
    let _speed_min = cursor.u32()?;
    let _speed_max = cursor.u32()?;
    if protocol_version >= 3 {
        let _brightness_min = cursor.u32()?;
        let _brightness_max = cursor.u32()?;
    }
    let _colors_min = cursor.u32()?;
    let _colors_max = cursor.u32()?;
    let _speed = cursor.u32()?;
    if protocol_version >= 3 {
        let _brightness = cursor.u32()?;
    }
    let _direction = cursor.u32()?;
    let _color_mode = cursor.i32()?;
    let color_count = cursor.u16()? as usize;
    cursor.skip(color_count * 4)?;

    Ok(Mode {
        index,
        name,
        value,
        data: cursor.data[start..cursor.offset].to_vec(),
    })
}

fn parse_zone(
    cursor: &mut Cursor<'_>,
    protocol_version: u32,
    index: usize,
) -> Result<Zone, String> {
    let name = cursor.string()?;
    let _zone_type = cursor.i32()?;
    let _leds_min = cursor.u32()?;
    let _leds_max = cursor.u32()?;
    let leds_count = cursor.u32()? as usize;
    let matrix_len = cursor.u16()? as usize;
    if matrix_len > 0 {
        let _height = cursor.u32()?;
        let _width = cursor.u32()?;
        cursor.skip(matrix_len.saturating_sub(8))?;
    }

    if protocol_version >= 4 {
        let segment_count = cursor.u16()? as usize;
        for _ in 0..segment_count {
            let _name = cursor.string()?;
            let _segment_type = cursor.i32()?;
            let _start = cursor.u32()?;
            let _count = cursor.u32()?;
        }
    }

    if protocol_version >= 5 {
        let _zone_flags = cursor.u32()?;
    }

    Ok(Zone {
        index,
        name,
        start: 0,
        count: leds_count,
    })
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.u16()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let bytes = self.bytes(len)?;
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
    }

    fn skip(&mut self, count: usize) -> Result<(), String> {
        self.bytes(count).map(|_| ())
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| "OpenRGB controller data offset overflow".to_string())?;
        let Some(bytes) = self.data.get(self.offset..end) else {
            return Err("OpenRGB controller data ended unexpectedly".to_string());
        };
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_matches_name_vendor_description_location_or_id() {
        let controllers = vec![
            Controller {
                id: 0,
                name: "Small GPU".to_string(),
                vendor: "Vendor".to_string(),
                description: "Graphics card".to_string(),
                location: "PCI".to_string(),
                active_mode: 0,
                modes: Vec::new(),
                led_count: 32,
                zones: vec![Zone {
                    index: 0,
                    name: "GPU".to_string(),
                    start: 0,
                    count: 32,
                }],
            },
            Controller {
                id: 1,
                name: "Big Board".to_string(),
                vendor: "BoardCo".to_string(),
                description: "Motherboard".to_string(),
                location: "HID".to_string(),
                active_mode: 0,
                modes: Vec::new(),
                led_count: 265,
                zones: vec![Zone {
                    index: 0,
                    name: "Addressable Header".to_string(),
                    start: 0,
                    count: 80,
                }],
            },
        ];

        assert_eq!(
            select_controller(controllers.clone(), Some("1"))
                .unwrap()
                .name,
            "Big Board"
        );
        assert_eq!(
            select_controller(controllers.clone(), Some("boardco"))
                .unwrap()
                .name,
            "Big Board"
        );
        assert_eq!(
            select_controller(controllers, None).unwrap().name,
            "Big Board"
        );
    }
}
