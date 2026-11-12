//! KV v1 and v2 paths and reply shapes. Field values are returned to the
//! caller and never placed in an error string.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use telepatch::{SecretError, SecretValue};

#[derive(Clone)]
pub struct Mount {
    pub prefix: String,
    pub version: u8,
}

pub fn parse_mount(body: &str, secret_path: &str) -> Result<Mount, SecretError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| SecretError::new(format!("no mount info for {secret_path}")))?;
    let data = v
        .get("data")
        .ok_or_else(|| SecretError::new(format!("no mount info for {secret_path}")))?;
    let prefix = data
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .trim_matches('/')
        .to_string();
    if prefix.is_empty() || !covers(&prefix, secret_path) {
        return Err(SecretError::new(format!("no mount for {secret_path}")));
    }
    let version = data
        .get("options")
        .and_then(|o| o.get("version"))
        .map(version_num)
        .unwrap_or(Some(1))
        .ok_or_else(|| SecretError::new(format!("unsupported kv version at {secret_path}")))?;
    if version != 1 && version != 2 {
        return Err(SecretError::new(format!(
            "unsupported kv version {version} at {secret_path}"
        )));
    }
    Ok(Mount { prefix, version })
}

fn version_num(v: &Value) -> Option<u8> {
    v.as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

pub fn covers(prefix: &str, secret_path: &str) -> bool {
    let path = secret_path.trim_matches('/');
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// `/v1/<mount>/data/<rest>` for v2, `/v1/<mount>/<rest>` for v1.
pub fn data_url(mount: &Mount, secret_path: &str) -> String {
    let path = secret_path.trim_matches('/');
    let rest = path
        .strip_prefix(&mount.prefix)
        .unwrap_or(path)
        .trim_start_matches('/');
    if mount.version == 2 {
        format!("/v1/{}/data/{rest}", mount.prefix)
    } else {
        format!("/v1/{}/{rest}", mount.prefix)
    }
}

pub fn take_field(
    mount: &Mount,
    doc: &Value,
    field: &str,
    secret_path: &str,
) -> Result<SecretValue, SecretError> {
    let obj = if mount.version == 2 {
        doc.get("data").and_then(|d| d.get("data"))
    } else {
        doc.get("data")
    };
    let Some(map) = obj.and_then(|v| v.as_object()) else {
        return Err(SecretError::new(format!("no secret at {secret_path}")));
    };
    match map.get(field) {
        Some(Value::String(s)) => Ok(SecretValue::new(s.clone())),
        Some(Value::Number(n)) => Ok(SecretValue::new(n.to_string())),
        Some(Value::Bool(b)) => Ok(SecretValue::new(b.to_string())),
        Some(_) => Err(SecretError::new(format!(
            "{secret_path} field {field} is not a string"
        ))),
        None => {
            let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
            names.sort_unstable();
            Err(SecretError::new(format!(
                "{secret_path} has no field {field}; it has: {}",
                names.join(", ")
            )))
        }
    }
}

/// `Some(day)` when `expire_time` is a time already past. `None` when the
/// token does not expire or the stamp cannot be read.
pub fn expiry_day(body: &str) -> Result<Option<String>, SecretError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| SecretError::new("auth/token/lookup-self was not json"))?;
    let Some(text) = v
        .get("data")
        .and_then(|d| d.get("expire_time"))
        .and_then(|e| e.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let Some(secs) = unix_seconds(text) else {
        return Ok(None);
    };
    if secs < now_secs() {
        let day = if text.len() >= 10 && &text[4..5] == "-" && &text[7..8] == "-" {
            text[..10].to_string()
        } else {
            text.to_string()
        };
        Ok(Some(day))
    } else {
        Ok(None)
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn unix_seconds(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i64 = std::str::from_utf8(&b[0..4]).ok()?.parse().ok()?;
    let month: u32 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    let day: u32 = std::str::from_utf8(&b[8..10]).ok()?.parse().ok()?;
    let mut hour = 0i64;
    let mut min = 0i64;
    let mut sec = 0i64;
    let mut rest = &s[10..];
    if let Some(time) = rest.strip_prefix('T') {
        let t = time.as_bytes();
        if t.len() < 8 || t[2] != b':' || t[5] != b':' {
            return None;
        }
        hour = std::str::from_utf8(&t[0..2]).ok()?.parse().ok()?;
        min = std::str::from_utf8(&t[3..5]).ok()?.parse().ok()?;
        sec = std::str::from_utf8(&t[6..8]).ok()?.parse().ok()?;
        rest = &time[8..];
        if let Some(frac) = rest.strip_prefix('.') {
            let n = frac
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(frac.len());
            rest = &frac[n..];
        }
    }
    let offset = if rest.is_empty() || rest == "Z" {
        0
    } else if rest.starts_with('+') || rest.starts_with('-') {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let o = &rest.as_bytes()[1..];
        if o.len() < 5 || o[2] != b':' {
            return None;
        }
        let oh: i64 = std::str::from_utf8(&o[0..2]).ok()?.parse().ok()?;
        let om: i64 = std::str::from_utf8(&o[3..5]).ok()?.parse().ok()?;
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    let days = days_from_civil(year, month, day)?;
    Some(days * 86400 + hour * 3600 + min * 60 + sec - offset)
}

/// Howard Hinnant's civil-from-days, inverted. `1970-01-01` is 0.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe as i64 - 719468)
}
