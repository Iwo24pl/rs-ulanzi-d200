use std::io::Write;
use std::time::Duration;

use async_hid::{AsyncHidWrite, HidBackend};
use futures_util::StreamExt;
use tokio::time::sleep;

const VENDOR_ID: u16 = 0x2207;
const PRODUCT_ID: u16 = 0x0019;
const USAGE_PAGE: u16 = 0x000c;

const PACKET_SIZE: usize = 1024;
const HEADER: [u8; 2] = [0x7c, 0x7c];
const OUT_SET_SMALL_WINDOW_DATA: u16 = 0x0006;

fn framed_packet(payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(PACKET_SIZE);
    packet.extend_from_slice(&HEADER);
    packet.extend_from_slice(&OUT_SET_SMALL_WINDOW_DATA.to_be_bytes());
    packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    packet.extend_from_slice(payload);
    packet.resize(PACKET_SIZE, 0);
    let mut out = Vec::with_capacity(PACKET_SIZE + 1);
    out.push(0x00);
    out.extend_from_slice(&packet);
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let backend = HidBackend::default();
    let devices: Vec<_> = backend.enumerate().await?.collect().await;
    let info = devices
        .into_iter()
        .find(|d| {
            d.vendor_id == VENDOR_ID && d.product_id == PRODUCT_ID && d.usage_page == USAGE_PAGE
        })
        .ok_or_else(|| anyhow::anyhow!("D200 not found"))?;
    let (_reader, mut writer) = info.open().await?;

    let now = chrono::Local::now();
    let hms = now.format("%H:%M:%S").to_string();
    let hm = now.format("%H:%M").to_string();

    // (label, payload)
    let variants: Vec<(&str, String)> = vec![
        ("V1 CURRENT  5-field HH:MM:SS", format!("1|0|0|{}|0", hms)),
        (
            "V2 OFFICIAL 7-field +24H HH:MM:SS",
            format!("1|0|0|{}|0|24H|", hms),
        ),
        ("V3 5-field HH:MM", format!("1|0|0|{}|0", hm)),
        ("V4 7-field +24H HH:MM", format!("1|0|0|{}|0|24H|", hm)),
        (
            "V5 7-field +24H +weekday suffix",
            format!("1|0|0|{}|0|24H|{}", hms, now.format("%a")),
        ),
    ];

    let seconds_each: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let log_path = std::env::temp_dir().join("swprobe.log");
    let mut logf = std::fs::File::create(&log_path)?;
    writeln!(
        logf,
        "== swprobe: {}s per variant -> {:?}",
        seconds_each, log_path
    )?;

    for (label, payload) in &variants {
        let line = format!("SEND [{}] payload={}", label, payload);
        writeln!(logf, "{}", line)?;
        logf.flush()?;
        println!("{}", line);
        let packet = framed_packet(payload.as_bytes());
        writer.write_output_report(&packet).await?;
        sleep(Duration::from_secs(seconds_each)).await;
    }

    writeln!(logf, "== done")?;
    println!("== done");
    Ok(())
}
