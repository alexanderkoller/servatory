use std::{fs, net::Ipv4Addr, process::Command};

use s3_display_protocol::{
    GuestKind, GuestSnapshot, GuestStatus, GuestSummary, HealthSnapshot, MAX_GUEST_NAME_LEN,
    MAX_HOST_NAME_LEN,
};
use serde_json::Value;

#[derive(Clone, Copy)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

#[derive(Default)]
pub struct HealthCollector {
    previous_cpu: Option<CpuTimes>,
}

impl HealthCollector {
    #[must_use]
    pub fn collect(&mut self) -> HealthSnapshot {
        let current_cpu = read_cpu_times();
        let cpu_percent = cpu_percent(self.previous_cpu, current_cpu);
        self.previous_cpu = current_cpu;

        let host_name = read_host_name();
        let (memory_used_mib, memory_total_mib) = read_memory();
        let (network_up, network_mbps) = read_network_link();
        HealthSnapshot::new(
            &host_name,
            read_uptime(),
            cpu_percent,
            memory_used_mib,
            memory_total_mib,
            read_io_pressure(),
            read_load_average(),
            read_root_usage(),
            read_backup_connected(),
            network_up,
            network_mbps,
            read_ipv4(),
            read_guests(&host_name),
        )
        .expect("collector truncates host names to the protocol limit")
    }
}

fn read_host_name() -> String {
    let name = fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "proxmox".into());
    truncate_utf8(name.trim(), MAX_HOST_NAME_LEN).to_owned()
}

fn read_uptime() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .next()?
                .split('.')
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn read_cpu_times() -> Option<CpuTimes> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    Some(CpuTimes {
        total: values.iter().sum(),
        idle: values[3].saturating_add(values.get(4).copied().unwrap_or(0)),
    })
}

fn cpu_percent(previous: Option<CpuTimes>, current: Option<CpuTimes>) -> u8 {
    let (Some(previous), Some(current)) = (previous, current) else {
        return 0;
    };
    let total = current.total.saturating_sub(previous.total);
    let idle = current.idle.saturating_sub(previous.idle);
    total
        .saturating_sub(idle)
        .saturating_mul(100)
        .checked_div(total)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(0)
}

fn read_memory() -> (u32, u32) {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let value = |key: &str| {
        text.lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let total_kib = value("MemTotal:");
    let available_kib = value("MemAvailable:");
    (mib(total_kib.saturating_sub(available_kib)), mib(total_kib))
}

fn mib(kib: u64) -> u32 {
    u32::try_from(kib / 1_024).unwrap_or(u32::MAX)
}

fn read_io_pressure() -> u8 {
    let text = fs::read_to_string("/proc/pressure/io").unwrap_or_default();
    text.lines()
        .find(|line| line.starts_with("some "))
        .and_then(|line| {
            line.split_whitespace()
                .find(|part| part.starts_with("avg10="))
        })
        .and_then(|part| part.strip_prefix("avg10="))
        .and_then(|value| value.parse::<f64>().ok())
        .map_or(0, percent)
}

fn read_load_average() -> u16 {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| parse_hundredths(text.split_whitespace().next()?))
        .unwrap_or(0)
}

fn read_root_usage() -> u8 {
    let Ok(output) = Command::new("/usr/bin/df").args(["-P", "/"]).output() else {
        return 0;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
        .and_then(|value| value.trim_end_matches('%').parse().ok())
        .unwrap_or(0)
}

fn read_backup_connected() -> bool {
    fs::read_to_string("/proc/mounts").is_ok_and(|mounts| {
        mounts.lines().any(|line| {
            line.split_whitespace()
                .nth(1)
                .is_some_and(|path| path.starts_with("/mnt/pve/"))
        })
    })
}

fn read_network_link() -> (bool, u16) {
    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return (false, 0);
    };
    let mut up = false;
    let mut fastest = 0_u16;
    for entry in entries.flatten() {
        if entry.file_name() == "lo" {
            continue;
        }
        let path = entry.path();
        if fs::read_to_string(path.join("operstate")).is_ok_and(|state| state.trim() == "up") {
            up = true;
            let speed = fs::read_to_string(path.join("speed"))
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or(0);
            fastest = fastest.max(speed);
        }
    }
    (up, fastest)
}

fn read_ipv4() -> [u8; 4] {
    let Ok(output) = Command::new("/usr/bin/hostname").arg("-I").output() else {
        return [0; 4];
    };
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|value| value.parse::<Ipv4Addr>().ok())
        .map_or([0; 4], |address| address.octets())
}

fn read_guests(host_name: &str) -> GuestSnapshot {
    let Ok(output) = Command::new("/usr/bin/pvesh")
        .args([
            "get",
            "/cluster/resources",
            "--type",
            "vm",
            "--output-format",
            "json",
        ])
        .output()
    else {
        return GuestSnapshot::EMPTY;
    };
    if !output.status.success() {
        return GuestSnapshot::EMPTY;
    }
    parse_guests(&output.stdout, host_name)
}

fn parse_guests(bytes: &[u8], host_name: &str) -> GuestSnapshot {
    let Ok(Value::Array(values)) = serde_json::from_slice(bytes) else {
        return GuestSnapshot::EMPTY;
    };
    let guests: Vec<_> = values
        .iter()
        .filter(|value| {
            value
                .get("node")
                .and_then(Value::as_str)
                .is_none_or(|node| node == host_name)
        })
        .filter_map(parse_guest)
        .collect();
    GuestSnapshot::from_slice(&guests)
}

fn parse_guest(value: &Value) -> Option<GuestSummary> {
    let vmid = u32::try_from(value.get("vmid")?.as_u64()?).ok()?;
    let kind = if value.get("type").and_then(Value::as_str) == Some("lxc") {
        GuestKind::Container
    } else {
        GuestKind::VirtualMachine
    };
    let status = if value.get("status").and_then(Value::as_str) == Some("running") {
        GuestStatus::Running
    } else {
        GuestStatus::Stopped
    };
    let cpu = value
        .get("cpu")
        .and_then(Value::as_f64)
        .map_or(0, |usage| percent(usage * 100.0));
    let memory_used_mib = bytes_to_mib(value.get("mem").and_then(Value::as_u64).unwrap_or(0));
    let memory_total_mib = bytes_to_mib(value.get("maxmem").and_then(Value::as_u64).unwrap_or(0));
    let fallback = vmid.to_string();
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&fallback);
    GuestSummary::new(
        vmid,
        truncate_utf8(name, MAX_GUEST_NAME_LEN),
        kind,
        status,
        cpu,
        memory_used_mib,
        memory_total_mib,
    )
    .ok()
}

fn bytes_to_mib(bytes: u64) -> u32 {
    u32::try_from(bytes / 1_048_576).unwrap_or(u32::MAX)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent(value: f64) -> u8 {
    value.clamp(0.0, 100.0).round() as u8
}

fn parse_hundredths(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u32>().ok()?;
    let mut digits = fraction
        .bytes()
        .take(2)
        .map(|digit| digit.checked_sub(b'0'));
    let tenths = u32::from(digits.next().flatten().unwrap_or(0));
    let hundredths = u32::from(digits.next().flatten().unwrap_or(0));
    u16::try_from(
        whole
            .saturating_mul(100)
            .saturating_add(tenths * 10)
            .saturating_add(hundredths),
    )
    .ok()
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxmox_guest_json() {
        let snapshot = parse_guests(
            br#"[{"vmid":100,"node":"pve-01","name":"atlas","type":"qemu","status":"running","cpu":0.23,"mem":3254779904,"maxmem":8589934592},{"vmid":102,"node":"pve-01","name":"paperless","type":"lxc","status":"stopped"},{"vmid":200,"node":"pve-02","name":"remote","type":"qemu","status":"running"}]"#,
            "pve-01",
        );
        assert_eq!(snapshot.guests().len(), 2);
        assert_eq!(snapshot.guests()[0].name(), "atlas");
        assert_eq!(snapshot.guests()[0].cpu_percent, 23);
        assert_eq!(snapshot.guests()[0].memory_total_mib, 8_192);
        assert_eq!(snapshot.guests()[1].kind, GuestKind::Container);
        assert_eq!(snapshot.guests()[1].status, GuestStatus::Stopped);
    }

    #[test]
    fn truncates_on_utf8_boundaries() {
        let long = "123456789012345678é";
        let truncated = truncate_utf8(long, MAX_GUEST_NAME_LEN);
        assert!(truncated.len() <= MAX_GUEST_NAME_LEN);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
