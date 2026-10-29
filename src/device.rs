use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Result, anyhow};
use async_hid::{AsyncHidWrite, DeviceReader, DeviceWriter, HidBackend};
use data_url::DataUrl;
use futures_util::StreamExt;
use log::{debug, info, warn};
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;

use image::{DynamicImage, GenericImageView, RgbImage, RgbaImage};
use uuid::Uuid;
use zip::write::FileOptions;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const VENDOR_ID: u16 = 0x2207;
pub const PRODUCT_ID: u16 = 0x0019;
pub const DEVICE_NAMESPACE: &str = "e9";

const PACKET_SIZE: usize = 1024;
const HEADER: [u8; 2] = [0x7c, 0x7c];
const USAGE_PAGE: u16 = 0x000c;
pub const NUM_BUTTONS: usize = 14;

const MAX_COMMAND_PAYLOAD: usize = PACKET_SIZE - 8; // 1016

/// Index of the wide button in the 5x3 OpenDeck grid. It occupies the bottom
/// right two cells (`3_2` and `4_2`); the official app writes the same icon to
/// both so the firmware composes the wide tile correctly.
const WIDE_BUTTON: usize = 13;

// ---------------------------------------------------------------------------
// Command protocol
// ---------------------------------------------------------------------------

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommandProtocol {
    OutSetButtons = 0x0001,
    OutSetSmallWindowData = 0x0006,
    OutSetBrightness = 0x000a,
    OutSetLabelStyle = 0x000b,
    /// Incremental, per-button icon update (native protocol).
    OutSetButtonIcon = 0x000d,
    InButton = 0x0101,
    InButton2 = 0x0102,
}

// ---------------------------------------------------------------------------
// Button event
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ButtonEvent {
    pub index: usize,
    pub pressed: bool,
    pub state: u8,
}

// ---------------------------------------------------------------------------
// Button data structure
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ButtonImageData {
    pub image: Vec<u8>,
    pub uuid: Uuid,
}

// ---------------------------------------------------------------------------
// Device handle
// ---------------------------------------------------------------------------

pub struct UlanziDevice {
    writer: Arc<TokioMutex<DeviceWriter>>,
    reader: Option<DeviceReader>,
    id: String,
    /// Desired on-device state, keyed by physical button index.
    button_images: Mutex<HashMap<usize, ButtonImageData>>,
    /// Buttons changed since the last successful sync. Only used by the
    /// incremental path; a full layout clears it.
    dirty: Mutex<HashSet<usize>>,
    /// False until a full layout (`OutSetButtons`) was accepted, so a fresh
    /// device (or one cleared at startup) receives a complete manifest first.
    synced: AtomicBool,
}

/// Lock a `std::sync::Mutex` without letting a panic elsewhere poison image
/// staging for the rest of the process.
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Aspect-ratio-preserving resize helper
// ---------------------------------------------------------------------------

/// Resize an image to fit inside `size`x`size`, preserving aspect ratio.
/// The padding is transparent if the source has an alpha channel,
/// otherwise opaque black.
fn resize_square(img: &DynamicImage, size: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w == size && h == size {
        return img.clone();
    }

    // Scale factor to fit entirely inside the square.
    let scale = (size as f64 / w as f64).min(size as f64 / h as f64);
    // Clamp to >= 1 so extreme aspect ratios (e.g. 1x4000) still produce a
    // valid buffer instead of a zero-width image.
    let new_w = ((w as f64 * scale).round() as u32).clamp(1, size);
    let new_h = ((h as f64 * scale).round() as u32).clamp(1, size);

    let resized = img.resize(new_w, new_h, image::imageops::FilterType::Triangle);

    let has_alpha = matches!(
        img.color(),
        image::ColorType::Rgba8
            | image::ColorType::La8
            | image::ColorType::Rgba16
            | image::ColorType::La16
    );

    let x = ((size - new_w) / 2) as i64;
    let y = ((size - new_h) / 2) as i64;

    if has_alpha {
        let mut canvas = RgbaImage::from_pixel(size, size, image::Rgba([0, 0, 0, 0]));
        image::imageops::overlay(&mut canvas, &resized.to_rgba8(), x, y);
        DynamicImage::ImageRgba8(canvas)
    } else {
        let mut canvas = RgbImage::from_pixel(size, size, image::Rgb([0, 0, 0]));
        let rgb = resized.to_rgb8();
        image::imageops::overlay(&mut canvas, &rgb, x, y);
        DynamicImage::ImageRgb8(canvas)
    }
}

impl UlanziDevice {
    // -- Construction -------------------------------------------------------

    pub async fn connect() -> Result<Self> {
        let backend = HidBackend::default();
        let devices: Vec<_> = backend.enumerate().await?.collect().await;

        // Diagnostic dump: on Windows (win32 HID) the same physical device
        // can show up as multiple collections with different usage pages.
        // This makes "device not found" debuggable from OpenDeck logs.
        for d in &devices {
            debug!(
                "HID candidate: vid=0x{:04x} pid=0x{:04x} usage_page=0x{:04x} usage_id=0x{:04x} serial={:?} id={:?}",
                d.vendor_id, d.product_id, d.usage_page, d.usage_id, d.serial_number, d.id,
            );
        }

        let device_count = devices.len();
        let device_info = devices
            .into_iter()
            .find(|d| {
                d.vendor_id == VENDOR_ID && d.product_id == PRODUCT_ID && d.usage_page == USAGE_PAGE
            })
            .ok_or_else(|| {
                anyhow!(
                    "Ulanzi D200 device not found (vid=0x{:04x} pid=0x{:04x} usage_page=0x{:04x}, saw {} HID device(s); enable debug logs to list them)",
                    VENDOR_ID,
                    PRODUCT_ID,
                    USAGE_PAGE,
                    device_count
                )
            })?;

        let (reader, writer) = device_info.open().await?;

        let id = Self::generate_id(
            device_info.serial_number.as_deref(),
            &format!("{:?}", device_info.id),
        );

        info!("Connected to Ulanzi D200 (ID: {})", id);

        Ok(Self {
            writer: Arc::new(TokioMutex::new(writer)),
            reader: Some(reader),
            id,
            button_images: Mutex::new(HashMap::new()),
            dirty: Mutex::new(HashSet::new()),
            synced: AtomicBool::new(false),
        })
    }

    /// Build the OpenDeck-facing device id. Serials are sanitized so the id
    /// stays a stable token; without a serial the platform path is hashed
    /// instead of leaking `Debug` output (quotes, backslashes, `#`, ...).
    fn generate_id(serial: Option<&str>, fallback: &str) -> String {
        match serial {
            Some(s) if !s.is_empty() => {
                let cleaned = sanitize_id(s);
                if cleaned.is_empty() {
                    format!("{}-{:016x}", DEVICE_NAMESPACE, fnv1a(fallback.as_bytes()))
                } else {
                    format!("{}-{}", DEVICE_NAMESPACE, cleaned)
                }
            }
            _ => format!("{}-{:016x}", DEVICE_NAMESPACE, fnv1a(fallback.as_bytes())),
        }
    }

    // -- Accessors ----------------------------------------------------------

    pub fn get_id(&self) -> &str {
        &self.id
    }

    pub fn take_reader(&mut self) -> Option<DeviceReader> {
        self.reader.take()
    }

    // -- Report parsing -----------------------------------------------------

    pub fn parse_report(buf: &[u8]) -> Option<ButtonEvent> {
        if buf.len() < 12 || buf[0..2] != HEADER {
            return None;
        }

        let command = u16::from_be_bytes([buf[2], buf[3]]);
        if command != CommandProtocol::InButton as u16
            && command != CommandProtocol::InButton2 as u16
        {
            return None;
        }

        let index = buf[9] as usize;
        if index >= NUM_BUTTONS {
            warn!("Received button event with out-of-range index {}", index);
            return None;
        }

        Some(ButtonEvent {
            state: buf[8],
            index,
            pressed: buf[11] == 0x01,
        })
    }

    // -- High-level commands ------------------------------------------------

    pub async fn set_small_window_data(
        &self,
        mode: u8,
        cpu: u8,
        mem: u8,
        time_str: &str,
        gpu: u8,
    ) -> Result<()> {
        // Official Ulanzi payload is 7 fields:
        // mode|cpu|mem|time|gpu|24H|suffix.  The trailing "|24H|" format
        // marker is required for the firmware to lay out the time correctly
        // (without it the time renders slightly too high on the wide tile).
        let payload = format!("{}|{}|{}|{}|{}|24H|", mode, cpu, mem, time_str, gpu).into_bytes();
        crate::tlog!("TX-SW {}", String::from_utf8_lossy(&payload));
        self.send_command(CommandProtocol::OutSetSmallWindowData, &payload)
            .await
    }

    pub async fn set_brightness(&self, brightness: u8) -> Result<()> {
        let brightness = brightness.min(100);
        let payload = brightness.to_string().into_bytes();
        self.send_command(CommandProtocol::OutSetBrightness, &payload)
            .await?;
        debug!("Set brightness to {}%", brightness);
        Ok(())
    }

    pub async fn set_label_style(&self, style: &serde_json::Value) -> Result<()> {
        let payload = serde_json::to_vec(style)?;
        self.send_command(CommandProtocol::OutSetLabelStyle, &payload)
            .await?;
        debug!("Set label style");
        Ok(())
    }

    /// Stage a button image from a data URL (Base64) or a file path.
    /// Returns `Ok(true)` if the image was new/different, `Ok(false)` if unchanged.
    /// Call `flush()` to apply all staged images.
    pub async fn set_button_image(&self, index: usize, image_data: &str) -> Result<bool> {
        if index >= NUM_BUTTONS {
            return Err(anyhow!(
                "Button index {} out of range (0..{})",
                index,
                NUM_BUTTONS - 1
            ));
        }

        // Decoding, resizing and PNG encoding are CPU-bound: keep them off the
        // async runtime so the clock and key forwarding stay responsive.
        let source = image_data.to_owned();
        let png_data = tokio::task::spawn_blocking(move || Self::encode_button_png(&source))
            .await
            .map_err(|e| anyhow!("image processing task failed: {e}"))??;

        let changed = {
            let mut map = lock_unpoisoned(&self.button_images);
            let changed = match map.get(&index) {
                Some(existing) => existing.image != png_data,
                None => true,
            };
            if changed {
                map.insert(
                    index,
                    ButtonImageData {
                        image: png_data,
                        uuid: Uuid::now_v7(),
                    },
                );
            }
            changed
        };

        if changed {
            lock_unpoisoned(&self.dirty).insert(index);
        }

        Ok(changed)
    }

    fn encode_button_png(image_data: &str) -> Result<Vec<u8>> {
        let img = if image_data.starts_with("data:") {
            let url = DataUrl::process(image_data).map_err(|_| anyhow!("Invalid data URL"))?;
            let (body, _) = url
                .decode_to_vec()
                .map_err(|_| anyhow!("Failed to decode data URL"))?;
            image::load_from_memory(&body)?
        } else {
            let path = std::path::Path::new(image_data);
            if !path.exists() {
                return Err(anyhow!("Image file not found: {}", image_data));
            }
            image::open(path).map_err(|e| anyhow!("Failed to open image {}: {}", image_data, e))?
        };

        let resized = resize_square(&img, 196);
        let mut buf = Vec::new();
        resized.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)?;
        Ok(buf)
    }

    /// Remove a staged button image. Returns `true` when there was something
    /// to remove (and therefore something to send on the next `flush()`).
    pub fn clear_button_image(&self, index: usize) -> bool {
        if index >= NUM_BUTTONS {
            warn!("Attempt to clear out-of-range button index {}", index);
            return false;
        }
        let removed = lock_unpoisoned(&self.button_images)
            .remove(&index)
            .is_some();
        if removed {
            lock_unpoisoned(&self.dirty).insert(index);
        }
        removed
    }

    /// Remove **all** staged button images and send an empty full layout
    /// (clears all buttons on the device).
    pub async fn clear_all_images(&self) -> Result<bool> {
        lock_unpoisoned(&self.button_images).clear();
        lock_unpoisoned(&self.dirty).clear();
        self.synced.store(false, Ordering::Release);
        self.flush().await
    }

    /// Push pending changes to the device.
    ///
    /// The first sync after connecting sends the complete layout via
    /// `OutSetButtons` (`0x0001`). Later changes are sent as per-button
    /// incremental updates via `OutSetButtonIcon` (`0x000d`), exactly like the
    /// official application, so a page switch no longer re-uploads every icon.
    ///
    /// Returns `Ok(true)` when something was transferred, `Ok(false)` when the
    /// device was already up to date.
    pub async fn flush(&self) -> Result<bool> {
        let images = lock_unpoisoned(&self.button_images).clone();

        if !self.synced.load(Ordering::Acquire) {
            let entries = Self::all_entries(&images);
            let zip = tokio::task::spawn_blocking(move || Self::build_zip(&entries))
                .await
                .map_err(|e| anyhow!("ZIP build task failed: {e}"))??;

            self.send_file(CommandProtocol::OutSetButtons, &zip).await?;

            self.synced.store(true, Ordering::Release);
            lock_unpoisoned(&self.dirty).clear();
            info!(
                "Sent full button layout ({} buttons, {} bytes)",
                images.len(),
                zip.len()
            );
            return Ok(true);
        }

        let dirty = {
            let set = lock_unpoisoned(&self.dirty);
            let mut indices: Vec<usize> = set.iter().copied().collect();
            indices.sort_unstable();
            indices
        };

        if dirty.is_empty() {
            return Ok(false);
        }

        for index in dirty {
            let entry = (Self::index_to_keys(index), images.get(&index).cloned());
            let zip = tokio::task::spawn_blocking(move || Self::build_zip(&[entry]))
                .await
                .map_err(|e| anyhow!("ZIP build task failed: {e}"))??;

            self.send_file(CommandProtocol::OutSetButtonIcon, &zip)
                .await?;
            // Only clear once the transfer succeeded; a write error leaves the
            // button dirty so the daemon's retry picks it up again.
            lock_unpoisoned(&self.dirty).remove(&index);
            debug!(
                "Sent incremental icon update for button {} ({} bytes)",
                index,
                zip.len()
            );
        }

        Ok(true)
    }

    /// Grid keys a physical button maps to. The wide button covers the two
    /// bottom-right cells, so both must carry the same icon.
    fn index_to_keys(index: usize) -> Vec<String> {
        if index == WIDE_BUTTON {
            vec!["3_2".to_string(), "4_2".to_string()]
        } else {
            vec![format!("{}_{}", index % 5, index / 5)]
        }
    }

    fn all_entries(
        images: &HashMap<usize, ButtonImageData>,
    ) -> Vec<(Vec<String>, Option<ButtonImageData>)> {
        (0..NUM_BUTTONS)
            .map(|index| (Self::index_to_keys(index), images.get(&index).cloned()))
            .collect()
    }

    /// Build a clean configuration ZIP in the device's native format:
    /// an `Images/` directory, one `Images/<uuid>.png` per icon, and a
    /// `manifest.json` that maps grid keys to their view parameters.
    fn build_zip(entries: &[(Vec<String>, Option<ButtonImageData>)]) -> Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options =
                FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);

            zip.add_directory("Images", options)?;

            let mut manifest = serde_json::Map::new();
            for (keys, image) in entries {
                let view_param = if let Some(img) = image {
                    let path = format!("Images/{}.png", img.uuid);
                    zip.start_file(&path, options)?;
                    zip.write_all(&img.image)?;
                    json!({ "Text": "", "Icon": path })
                } else {
                    json!({ "Text": "", "Icon": "" })
                };

                let entry = json!({ "State": 0, "ViewParam": [view_param] });
                for key in keys {
                    manifest.insert(key.clone(), entry.clone());
                }
            }

            zip.start_file("manifest.json", options)?;
            zip.write_all(&serde_json::to_vec(&manifest)?)?;
            zip.finish()?;
        }
        Ok(cursor.into_inner())
    }

    // -- Low-level packet I/O -----------------------------------------------
    // Windows HID output reports include a leading Report-ID byte
    // (async-hid strips it on read but does not add it on write).
    // The D200 uses Report-ID 0, so every 1024B payload must go out as
    // [0x00 + 1024B] = 1025B on Windows. Linux hidraw needs no prefix.
    #[cfg(windows)]
    fn output_report(packet: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(packet.len() + 1);
        out.push(0x00);
        out.extend_from_slice(packet);
        out
    }

    #[cfg(not(windows))]
    fn output_report(packet: &[u8]) -> Vec<u8> {
        packet.to_vec()
    }

    async fn send_command(&self, command: CommandProtocol, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_COMMAND_PAYLOAD {
            return Err(anyhow!(
                "Command payload too large: {} bytes (max {})",
                payload.len(),
                MAX_COMMAND_PAYLOAD
            ));
        }
        let packet = Self::build_packet(command, payload, payload.len() as u32);
        let report = Self::output_report(&packet);
        self.writer
            .lock()
            .await
            .write_output_report(&report)
            .await?;
        Ok(())
    }

    /// Stream a payload as a framed multi-packet transfer.
    ///
    /// The first packet carries the 8-byte header (`7c 7c | cmd | total_len`)
    /// followed by up to `MAX_COMMAND_PAYLOAD` payload bytes; every subsequent
    /// packet is a raw 1024-byte continuation chunk of the remaining payload.
    /// The writer lock is held for the whole transfer so no other command can
    /// be interleaved between continuation packets.
    async fn send_file(&self, command: CommandProtocol, data: &[u8]) -> Result<()> {
        let packets = Self::build_file_packets(command, data);
        let mut writer = self.writer.lock().await;
        for packet in &packets {
            writer
                .write_output_report(&Self::output_report(packet))
                .await?;
        }
        Ok(())
    }

    /// Pure packetization (header/first chunk + zero-padded continuations),
    /// exposed separately so it can be tested without a HID device.
    fn build_file_packets(command: CommandProtocol, data: &[u8]) -> Vec<Vec<u8>> {
        let first_len = data.len().min(MAX_COMMAND_PAYLOAD);
        let mut packets = Vec::with_capacity(1 + data.len().div_ceil(PACKET_SIZE));
        packets.push(Self::build_packet(
            command,
            &data[..first_len],
            data.len() as u32,
        ));

        for chunk in data[first_len..].chunks(PACKET_SIZE) {
            let mut packet = vec![0u8; PACKET_SIZE];
            packet[..chunk.len()].copy_from_slice(chunk);
            packets.push(packet);
        }
        packets
    }

    fn build_packet(command: CommandProtocol, data: &[u8], total_length: u32) -> Vec<u8> {
        let mut packet = Vec::with_capacity(PACKET_SIZE);
        packet.extend_from_slice(&HEADER);
        packet.extend_from_slice(&(command as u16).to_be_bytes());
        // Device expects total_length as little-endian (working protocol).
        packet.extend_from_slice(&total_length.to_le_bytes());

        let data_len = data.len().min(PACKET_SIZE - 8);
        packet.extend_from_slice(&data[..data_len]);

        packet.resize(PACKET_SIZE, 0);
        packet
    }
}

/// Keep only characters that are safe in an OpenDeck device id.
fn sanitize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_separator = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_was_separator = false;
        } else if !last_was_separator {
            out.push('-');
            last_was_separator = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// FNV-1a, used to turn an opaque platform device path into a short, stable id.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_generate_id_with_serial() {
        let id = UlanziDevice::generate_id(Some("1234567890"), "fallback");
        assert_eq!(id, "e9-1234567890");
    }

    #[test]
    fn test_generate_id_sanitizes_serial() {
        let id = UlanziDevice::generate_id(Some("AB CD/EF"), "fallback");
        assert_eq!(id, "e9-AB-CD-EF");
    }

    #[test]
    fn test_generate_id_without_serial_hashes_path() {
        let path = r#"UncPath("\\\\?\\hid#vid_2207&pid_0019")"#;
        let id = UlanziDevice::generate_id(None, path);
        assert!(id.starts_with("e9-"), "unexpected id: {id}");
        assert_eq!(id.len(), 3 + 16);
        assert!(
            id[3..].chars().all(|c| c.is_ascii_hexdigit()),
            "unexpected id: {id}"
        );
        assert_ne!(id, UlanziDevice::generate_id(None, "other-path"));
    }

    #[test]
    fn test_index_to_keys() {
        assert_eq!(UlanziDevice::index_to_keys(0), vec!["0_0"]);
        assert_eq!(UlanziDevice::index_to_keys(4), vec!["4_0"]);
        assert_eq!(UlanziDevice::index_to_keys(5), vec!["0_1"]);
        assert_eq!(UlanziDevice::index_to_keys(12), vec!["2_2"]);
        // The wide button spans the bottom two right-hand cells.
        assert_eq!(UlanziDevice::index_to_keys(13), vec!["3_2", "4_2"]);
    }

    fn read_zip(bytes: &[u8]) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        zip::ZipArchive::new(Cursor::new(bytes.to_vec())).expect("valid zip")
    }

    fn manifest_of(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>) -> serde_json::Value {
        let mut text = String::new();
        archive
            .by_name("manifest.json")
            .expect("manifest.json present")
            .read_to_string(&mut text)
            .unwrap();
        serde_json::from_str(&text).expect("valid manifest")
    }

    fn test_image(byte: u8) -> ButtonImageData {
        ButtonImageData {
            image: vec![byte; 64],
            uuid: Uuid::now_v7(),
        }
    }

    #[test]
    fn test_full_layout_zip_has_all_grid_keys() {
        let mut images: HashMap<usize, ButtonImageData> = HashMap::new();
        images.insert(7, test_image(7));
        images.insert(13, test_image(13));

        let entries = UlanziDevice::all_entries(&images);
        let bytes = UlanziDevice::build_zip(&entries).unwrap();
        let mut archive = read_zip(&bytes);

        // 13 single-cell buttons + the wide button's two cells = 15 keys.
        let manifest = manifest_of(&mut archive);
        let keys = manifest.as_object().expect("manifest object");
        assert_eq!(keys.len(), 15);
        for key in ["0_0", "4_0", "0_1", "4_1", "0_2", "3_2", "4_2"] {
            assert!(keys.contains_key(key), "missing grid key {key}");
        }

        // The wide button references one icon from both of its cells.
        let wide_icon = &manifest["3_2"]["ViewParam"][0]["Icon"];
        assert_eq!(wide_icon, &manifest["4_2"]["ViewParam"][0]["Icon"]);
        assert!(wide_icon.as_str().unwrap().starts_with("Images/"));

        // Button 7 is row 1, column 2 and stores exactly one PNG.
        let icon_path = manifest["2_1"]["ViewParam"][0]["Icon"].as_str().unwrap();
        let mut png = Vec::new();
        archive
            .by_name(icon_path)
            .expect("icon present")
            .read_to_end(&mut png)
            .unwrap();
        assert_eq!(png, vec![7u8; 64]);

        // No workaround leftovers.
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        assert!(names.iter().any(|n| n == "Images/"));
        assert!(!names.iter().any(|n| n == "dummy.txt"));
        assert!(!names.iter().any(|n| n == "sentinel.txt"));
    }

    #[test]
    fn test_incremental_zip_clears_a_button() {
        let entries = vec![(UlanziDevice::index_to_keys(5), None)];
        let bytes = UlanziDevice::build_zip(&entries).unwrap();
        let mut archive = read_zip(&bytes);

        let manifest = manifest_of(&mut archive);
        assert_eq!(manifest.as_object().unwrap().len(), 1);
        assert_eq!(manifest["0_1"]["ViewParam"][0]["Icon"], "");
        assert_eq!(
            manifest["0_1"]["ViewParam"][0]["Text"],
            serde_json::json!("")
        );
    }

    #[test]
    fn test_build_file_packets_boundaries() {
        for len in [0usize, 1, 1015, 1016, 1017, 2040, 2041, 5000] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let packets =
                UlanziDevice::build_file_packets(CommandProtocol::OutSetButtonIcon, &data);

            assert_eq!(packets[0].len(), PACKET_SIZE);
            assert_eq!(&packets[0][0..2], &HEADER);
            assert_eq!(
                u16::from_be_bytes([packets[0][2], packets[0][3]]),
                CommandProtocol::OutSetButtonIcon as u16
            );
            assert_eq!(
                u32::from_le_bytes([packets[0][4], packets[0][5], packets[0][6], packets[0][7]]),
                len as u32
            );

            let first_len = len.min(MAX_COMMAND_PAYLOAD);
            assert_eq!(&packets[0][8..8 + first_len], &data[..first_len]);
            assert_eq!(
                packets.len(),
                1 + len
                    .saturating_sub(MAX_COMMAND_PAYLOAD)
                    .div_ceil(PACKET_SIZE)
            );

            // Continuations are raw and zero-padded; reassembly is lossless.
            let mut reassembled = packets[0][8..8 + first_len].to_vec();
            for packet in &packets[1..] {
                assert_eq!(packet.len(), PACKET_SIZE);
                reassembled.extend_from_slice(packet);
            }
            reassembled.truncate(len);
            assert_eq!(reassembled, data);
        }
    }

    fn report(command: u16, index: u8, pressed: u8) -> Vec<u8> {
        let mut buf = vec![0u8; 12];
        buf[0] = 0x7c;
        buf[1] = 0x7c;
        buf[2..4].copy_from_slice(&command.to_be_bytes());
        buf[8] = 1; // state
        buf[9] = index;
        buf[10] = 0xaa; // marker
        buf[11] = pressed;
        buf
    }

    #[test]
    fn test_parse_report() {
        let event = UlanziDevice::parse_report(&report(0x0101, 5, 1)).unwrap();
        assert_eq!(event.index, 5);
        assert!(event.pressed);

        let event = UlanziDevice::parse_report(&report(0x0102, 13, 0)).unwrap();
        assert_eq!(event.index, 13);
        assert!(!event.pressed);

        assert!(UlanziDevice::parse_report(&report(0x0101, 14, 1)).is_none());
        assert!(UlanziDevice::parse_report(&report(0x0001, 5, 1)).is_none());
        assert!(UlanziDevice::parse_report(&[0u8; 11]).is_none());
        assert!(UlanziDevice::parse_report(&[0x00; 12]).is_none());
    }

    #[test]
    fn test_resize_square_always_returns_square_canvas() {
        for (w, h) in [(196, 196), (400, 100), (100, 400), (1, 4000), (1, 1)] {
            let img = DynamicImage::new_rgb8(w, h);
            assert_eq!(resize_square(&img, 196).dimensions(), (196, 196));
        }
        let rgba = DynamicImage::new_rgba8(300, 50);
        assert_eq!(resize_square(&rgba, 196).dimensions(), (196, 196));
    }
}
