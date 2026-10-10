use std::io::Write;
use std::time::Instant;

use async_hid::{AsyncHidRead, HidBackend};
use futures_util::StreamExt;

const VENDOR_ID: u16 = 0x2207;
const PRODUCT_ID: u16 = 0x0019;
const USAGE_PAGE: u16 = 0x000c;

fn log_line(buf: &mut impl Write, line: &str) {
    let _ = writeln!(buf, "{}", line);
    let _ = buf.flush();
    println!("{}", line);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let log_path = std::env::temp_dir().join("hidcap.log");
    let mut file = std::fs::File::create(&log_path)?;

    log_line(
        &mut file,
        &format!("== hidcap: capturing {}s -> {}", seconds, log_path.display()),
    );

    let backend = HidBackend::default();
    let devices: Vec<_> = backend.enumerate().await?.collect().await;
    for d in &devices {
        log_line(
            &mut file,
            &format!(
                "candidate vid=0x{:04x} pid=0x{:04x} usage_page=0x{:04x} usage=0x{:04x} serial={:?} id={:?}",
                d.vendor_id, d.product_id, d.usage_page, d.usage_id, d.serial_number, d.id
            ),
        );
    }

    let info = devices
        .into_iter()
        .find(|d| {
            d.vendor_id == VENDOR_ID
                && d.product_id == PRODUCT_ID
                && d.usage_page == USAGE_PAGE
        })
        .ok_or_else(|| anyhow::anyhow!("D200 not found"))?;

    let (mut reader, _writer) = info.open().await?;
    log_line(&mut file, "opened device; waiting for input reports...");

    let start = Instant::now();
    let mut report = [0u8; 2048];
    let mut count = 0usize;

    loop {
        let remaining = seconds.saturating_sub(start.elapsed().as_secs());
        if remaining == 0 {
            break;
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(remaining),
            reader.read_input_report(&mut report),
        )
        .await
        {
            Ok(Ok(len)) => {
                count += 1;
                let bytes = &report[..len];
                let hex: String = bytes
                    .iter()
                    .take(24)
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                let parsed = if len >= 12 && bytes[0..2] == [0x7c, 0x7c] {
                    let cmd = u16::from_be_bytes([bytes[2], bytes[3]]);
                    let total = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                    match cmd {
                        0x0101 | 0x0102 => format!(
                            "cmd=0x{:04x} total={} state={} index={} marker={} value={} pressed={}",
                            cmd,
                            total,
                            bytes[8],
                            bytes[9],
                            bytes[10],
                            bytes[11],
                            bytes[11] == 1
                        ),
                        _ => format!("cmd=0x{:04x} total={}", cmd, total),
                    }
                } else {
                    "NOT A FRAME".to_string()
                };
                log_line(
                    &mut file,
                    &format!("[{:>8.3}s] len={} {} | {}", start.elapsed().as_secs_f64(), len, hex, parsed),
                );
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => {
                log_line(&mut file, &format!("read error: {}", e));
                break;
            }
            Err(_) => break,
        }
    }

    log_line(&mut file, &format!("== done: {} report(s)", count));
    Ok(())
}
