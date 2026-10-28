use nautik9_core::sms::{composer_opened_message, SmsMessage, SmsStatus};
use chrono::{Local, TimeZone};

/// Detects the active ADB connected device ID
pub async fn get_active_adb_device() -> Option<String> {
    let output = tokio::process::Command::new("adb")
        .args(&["devices"])
        .output()
        .await
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("\tdevice") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if !parts.is_empty() {
                return Some(parts[0].to_string());
            }
        }
    }
    None
}

/// Reads raw real SMS from the connected Android phone's content provider
pub async fn fetch_real_device_sms(limit: usize) -> Result<Vec<SmsMessage>, String> {
    let dev = get_active_adb_device().await
        .ok_or_else(|| "No ADB device connected (Pending pass / phone connection)".to_string())?;

    // Query real SMS content provider
    let cmd_str = format!(
        "content query --uri content://sms/inbox --projection _id,address,body,date --sort 'date DESC' | head -n {}",
        limit * 12
    );

    let output = tokio::process::Command::new("adb")
        .args(&["-s", &dev, "shell", &cmd_str])
        .output()
        .await
        .map_err(|e| format!("Failed to query content://sms/inbox: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ADB content query failed: {}", err));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_adb_sms_rows(&text, limit))
}

/// Parses the output of `content query --uri content://sms/inbox`
pub fn parse_adb_sms_rows(text: &str, limit: usize) -> Vec<SmsMessage> {
    let mut messages = Vec::new();
    
    // Rows typically begin with "Row: <number> "
    let chunks: Vec<&str> = text.split("\nRow: ").collect();
    for (i, raw_chunk) in chunks.into_iter().enumerate() {
        if messages.len() >= limit {
            break;
        }
        let chunk = if i == 0 && raw_chunk.starts_with("Row: ") {
            &raw_chunk[5..]
        } else {
            raw_chunk
        };

        if chunk.trim().is_empty() {
            continue;
        }

        // Extract _id
        let id_val = if let Some(idx) = chunk.find("_id=") {
            let rest = &chunk[idx + 4..];
            let end = rest.find(',').unwrap_or(rest.len());
            rest[..end].trim()
        } else {
            continue;
        };

        // Extract address
        let addr_val = if let Some(idx) = chunk.find("address=") {
            let rest = &chunk[idx + 8..];
            let end = rest.find(',').unwrap_or(rest.len());
            rest[..end].trim()
        } else {
            "Unknown"
        };

        // Extract body and date
        let (body_val, date_val) = match (chunk.find("body="), chunk.rfind(", date=")) {
            (Some(b_idx), Some(d_idx)) if b_idx < d_idx => {
                let b = &chunk[b_idx + 5..d_idx];
                let d_rest = &chunk[d_idx + 7..];
                let d_end = d_rest.find('\n').or_else(|| d_rest.find(',')).unwrap_or(d_rest.len());
                (b.trim(), d_rest[..d_end].trim())
            }
            _ => continue,
        };

        let timestamp_str = if let Ok(ms) = date_val.parse::<i64>() {
            let dt = Local.timestamp_millis_opt(ms).single();
            match dt {
                Some(d) => d.format("%I:%M %p").to_string(),
                None => "Recent".to_string(),
            }
        } else {
            "Recent".to_string()
        };

        messages.push(SmsMessage {
            id: format!("sms-{}", id_val),
            sender: addr_val.to_string(),
            body: body_val.to_string(),
            timestamp: timestamp_str,
            draft_reply: None,
            draft_error: None,
            status: SmsStatus::Pending,
            reply_model: None,
            latency_ms: None,
        });
    }

    messages
}

/// Opens the phone's SMS composer with the reply filled in (SENDTO intent).
/// Nothing is sent: the user still has to press send on the phone.
pub async fn dispatch_sms_to_phone(sender: &str, body: &str) -> Result<String, String> {
    let dev = get_active_adb_device().await
        .ok_or_else(|| "No ADB device connected to dispatch SMS".to_string())?;

    // Filter valid phone chars
    let clean_target: String = sender.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
    if clean_target.is_empty() {
        return Err(format!("Invalid recipient address: {}", sender));
    }

    let output = tokio::process::Command::new("adb")
        .args(&[
            "-s", &dev, "shell",
            "am", "start",
            "-a", "android.intent.action.SENDTO",
            "-d", &format!("sms:{}", clean_target),
            "--es", "sms_body", body,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to invoke Android SMS intent: {}", e))?;

    if output.status.success() {
        Ok(composer_opened_message(&clean_target))
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("Android intent dispatch failed: {}", err))
    }
}
