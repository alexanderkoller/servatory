use std::{
    fs,
    net::Ipv4Addr,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use servatory_protocol::{
    BackupJobStatus, FilesystemUsage, GuestKind, GuestSnapshot, GuestStatus, GuestSummary,
    HealthSnapshot, InternetStatus, SmartDeviceSummary, SmartSnapshot, SmartStatus, UpsSnapshot,
    UpsStatus,
};

use crate::config::{InternetConfig, SmartDeviceConfig};

#[derive(Clone, Copy)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

pub struct HealthCollector {
    previous_cpu: Option<CpuTimes>,
    connectivity: ConnectivityProbe,
    ups: UpsCollector,
    smart_devices: Vec<SmartDeviceConfig>,
    filesystems: Vec<String>,
    backup_task_history_limit: u16,
}

impl Default for HealthCollector {
    fn default() -> Self {
        Self::new(
            None,
            2,
            Vec::new(),
            vec!["/".into(), "/mnt/pve/hdd".into(), "/mnt/pve/backup".into()],
            None,
            50,
        )
    }
}

impl HealthCollector {
    #[must_use]
    pub fn new(
        ups_target: Option<String>,
        ups_failures_before_unavailable: u8,
        smart_devices: Vec<SmartDeviceConfig>,
        filesystems: Vec<String>,
        internet: Option<&InternetConfig>,
        backup_task_history_limit: u16,
    ) -> Self {
        Self {
            previous_cpu: None,
            connectivity: ConnectivityProbe::new(internet),
            ups: UpsCollector::new(ups_target, ups_failures_before_unavailable),
            smart_devices,
            filesystems,
            backup_task_history_limit,
        }
    }
}

impl HealthCollector {
    #[must_use]
    pub fn collect(&mut self) -> HealthSnapshot {
        let current_cpu = read_cpu_times();
        let cpu_percent = cpu_percent(self.previous_cpu, current_cpu);
        self.previous_cpu = current_cpu;

        let host_name = read_host_name();
        let (memory_used_mib, memory_total_mib) = read_memory();
        let network = read_network();
        let (internet_status, last_internet_success_age_seconds) =
            self.connectivity.snapshot(network.up);
        let filesystems = self
            .filesystems
            .iter()
            .map(|path| read_filesystem_usage(path))
            .collect();
        let (backup_job_status, last_successful_backup_age_seconds) = read_backup_job_status(
            &host_name,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs()),
            self.backup_task_history_limit,
        );
        let ups = self.ups.collect();
        let smart = read_smart(&self.smart_devices);
        HealthSnapshot::new(
            &host_name,
            read_uptime(),
            cpu_percent,
            memory_used_mib,
            memory_total_mib,
            read_io_pressure(),
            read_load_average(),
            filesystems,
            backup_job_status,
            last_successful_backup_age_seconds,
            network.up,
            network.mbps,
            &network.interface,
            internet_status,
            last_internet_success_age_seconds,
            network.ipv4,
            read_guests(&host_name),
            ups,
            smart,
        )
    }
}

struct UpsCollector {
    target: Option<String>,
    last_good: Option<UpsSnapshot>,
    consecutive_failures: u8,
    failures_before_unavailable: u8,
}

impl UpsCollector {
    fn new(target: Option<String>, failures_before_unavailable: u8) -> Self {
        Self {
            target,
            last_good: None,
            consecutive_failures: 0,
            failures_before_unavailable,
        }
    }

    fn collect(&mut self) -> UpsSnapshot {
        let Some(target) = self.target.as_deref() else {
            return UpsSnapshot::NOT_CONFIGURED;
        };
        let snapshot = ["/usr/bin/upsc", "/usr/sbin/upsc"]
            .iter()
            .find_map(|executable| {
                let output = Command::new(executable).arg(target).output().ok()?;
                output
                    .status
                    .success()
                    .then(|| parse_upsc(&String::from_utf8_lossy(&output.stdout)))?
            });
        if let Some(snapshot) = snapshot {
            self.consecutive_failures = 0;
            self.last_good = Some(snapshot);
            return snapshot;
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let mut stale = self.last_good.unwrap_or(UpsSnapshot {
            status: UpsStatus::Unknown,
            battery_percent: None,
            load_percent: None,
            runtime_seconds: None,
            estimated_watts: None,
            stale: true,
        });
        stale.stale = true;
        if self.consecutive_failures >= self.failures_before_unavailable {
            stale.status = UpsStatus::Unavailable;
        }
        stale
    }
}

fn parse_upsc(output: &str) -> Option<UpsSnapshot> {
    let value = |key: &str| {
        output.lines().find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            (candidate.trim() == key).then(|| value.trim())
        })
    };
    let flags = value("ups.status")?;
    let has = |flag: &str| flags.split_whitespace().any(|candidate| candidate == flag);
    let status = if has("LB") {
        UpsStatus::LowBattery
    } else if has("RB") {
        UpsStatus::ReplaceBattery
    } else if has("OFF") {
        UpsStatus::OutputOff
    } else if has("OB") {
        UpsStatus::OnBattery
    } else if has("BYPASS") {
        UpsStatus::Bypass
    } else if has("CHRG") {
        UpsStatus::Charging
    } else if has("OL") {
        UpsStatus::Online
    } else {
        UpsStatus::Unknown
    };
    Some(UpsSnapshot {
        status,
        battery_percent: value("battery.charge").and_then(parse_percent_u8),
        load_percent: value("ups.load").and_then(parse_percent_u8),
        runtime_seconds: value("battery.runtime").and_then(|value| value.parse().ok()),
        estimated_watts: value("ups.realpower")
            .and_then(|value| value.parse::<f64>().ok())
            .map(bounded_u16),
        stale: false,
    })
}

fn parse_percent_u8(value: &str) -> Option<u8> {
    value.parse::<f64>().ok().map(percent)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn bounded_u16(value: f64) -> u16 {
    value.clamp(0.0, f64::from(u16::MAX)).round() as u16
}

fn read_smart(devices: &[SmartDeviceConfig]) -> SmartSnapshot {
    let summaries: Vec<_> = devices
        .iter()
        .map(|device| {
            let (status, temperature) = read_smart_device(&device.path);
            SmartDeviceSummary::new(&device.label, status, temperature)
        })
        .collect();
    SmartSnapshot::new(summaries)
}

fn read_smart_device(path: &str) -> (SmartStatus, Option<i8>) {
    let output = ["/usr/sbin/smartctl", "/usr/bin/smartctl"]
        .iter()
        .find_map(|executable| {
            Command::new(executable)
                .args(["-j", "-n", "standby", "-H", "-A", "-d", "sat", path])
                .output()
                .ok()
        });
    let Some(output) = output else {
        return (SmartStatus::Unknown, None);
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.code() == Some(2) && combined.to_ascii_uppercase().contains("STANDBY") {
        return (SmartStatus::Sleeping, None);
    }

    let json: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let temperature = smart_temperature(&json);
    let exit = output.status.code().unwrap_or(1);
    let passed = json
        .pointer("/smart_status/passed")
        .and_then(Value::as_bool);
    let status = if passed == Some(false) || exit & (1 << 3) != 0 {
        SmartStatus::Failed
    } else if exit & ((1 << 4) | (1 << 5)) != 0
        || temperature.is_some_and(|temperature| temperature >= 55)
    {
        SmartStatus::Warning
    } else if passed == Some(true) || output.status.success() {
        SmartStatus::Healthy
    } else {
        SmartStatus::Unknown
    };
    (status, temperature)
}

fn smart_temperature(json: &Value) -> Option<i8> {
    json.pointer("/temperature/current")
        .and_then(Value::as_i64)
        .or_else(|| {
            json.pointer("/ata_smart_attributes/table")?
                .as_array()?
                .iter()
                .find(|attribute| {
                    matches!(
                        attribute.get("name").and_then(Value::as_str),
                        Some("Temperature_Celsius" | "Airflow_Temperature_Cel")
                    )
                })?
                .pointer("/raw/value")
                .and_then(Value::as_i64)
        })
        .and_then(|temperature| i8::try_from(temperature).ok())
}

fn read_host_name() -> String {
    let name = fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "proxmox".into());
    name.trim().to_owned()
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

fn read_filesystem_usage(path: &str) -> FilesystemUsage {
    let Ok(output) = Command::new("/usr/bin/df")
        .args(["-P", "-k", path])
        .output()
    else {
        return FilesystemUsage::MISSING;
    };
    if !output.status.success() {
        return FilesystemUsage::MISSING;
    }
    parse_filesystem_usage(&String::from_utf8_lossy(&output.stdout), path)
        .unwrap_or(FilesystemUsage::MISSING)
}

fn parse_filesystem_usage(output: &str, expected_mount: &str) -> Option<FilesystemUsage> {
    let fields: Vec<_> = output.lines().last()?.split_whitespace().collect();
    if fields.len() < 6 || fields.last().copied() != Some(expected_mount) {
        return None;
    }
    let available_kib = fields.get(3)?.parse::<u64>().ok()?;
    let used_percent = fields.get(4)?.trim_end_matches('%').parse::<u8>().ok()?;
    Some(FilesystemUsage::new(
        used_percent,
        u32::try_from(available_kib / 1_024).unwrap_or(u32::MAX),
    ))
}

fn read_backup_job_status(
    host_name: &str,
    now_seconds: Option<u64>,
    task_history_limit: u16,
) -> (BackupJobStatus, Option<u32>) {
    let Some(now_seconds) = now_seconds else {
        return (BackupJobStatus::Unknown, None);
    };
    let Ok(jobs) = Command::new("/usr/bin/pvesh")
        .args(["get", "/cluster/backup", "--output-format", "json"])
        .output()
    else {
        return (BackupJobStatus::Unknown, None);
    };
    let task_path = format!("/nodes/{host_name}/tasks");
    let task_history_limit = task_history_limit.to_string();
    let Ok(tasks) = Command::new("/usr/bin/pvesh")
        .args([
            "get",
            &task_path,
            "--typefilter",
            "vzdump",
            "--source",
            "all",
            "--limit",
            &task_history_limit,
            "--output-format",
            "json",
        ])
        .output()
    else {
        return (BackupJobStatus::Unknown, None);
    };
    if !jobs.status.success() || !tasks.status.success() {
        return (BackupJobStatus::Unknown, None);
    }
    parse_backup_job_status(&jobs.stdout, &tasks.stdout, host_name, now_seconds)
}

fn parse_backup_job_status(
    jobs_bytes: &[u8],
    tasks_bytes: &[u8],
    host_name: &str,
    now_seconds: u64,
) -> (BackupJobStatus, Option<u32>) {
    let Ok(Value::Array(jobs)) = serde_json::from_slice(jobs_bytes) else {
        return (BackupJobStatus::Unknown, None);
    };
    let enabled_jobs = jobs
        .iter()
        .filter(|job| backup_job_enabled(job))
        .filter(|job| {
            job.get("node")
                .and_then(Value::as_str)
                .is_none_or(|node| node.is_empty() || node == "all" || node == host_name)
        })
        .count();
    if enabled_jobs == 0 {
        return (BackupJobStatus::NoJob, None);
    }

    let Ok(Value::Array(tasks)) = serde_json::from_slice(tasks_bytes) else {
        return (BackupJobStatus::Unknown, None);
    };
    let mut local_tasks: Vec<_> = tasks
        .iter()
        .filter(|task| {
            task.get("node")
                .and_then(Value::as_str)
                .is_none_or(|node| node == host_name)
        })
        .collect();
    local_tasks.sort_by_key(|task| task.get("starttime").and_then(Value::as_u64).unwrap_or(0));

    let last_success_end = local_tasks
        .iter()
        .filter(|task| task.get("status").and_then(Value::as_str) == Some("OK"))
        .filter_map(|task| task.get("endtime").and_then(Value::as_u64))
        .max();
    let success_age = last_success_end
        .map(|end| u32::try_from(now_seconds.saturating_sub(end)).unwrap_or(u32::MAX));

    let Some(latest) = local_tasks.last() else {
        return (BackupJobStatus::Stale, None);
    };
    if latest.get("endtime").and_then(Value::as_u64).is_none() {
        return (BackupJobStatus::Running, success_age);
    }
    if latest.get("status").and_then(Value::as_str) != Some("OK") {
        return (BackupJobStatus::Failed, success_age);
    }
    (BackupJobStatus::Healthy, success_age)
}

fn backup_job_enabled(job: &Value) -> bool {
    match job.get("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(Value::Number(enabled)) => enabled.as_u64() != Some(0),
        Some(Value::String(enabled)) => enabled != "0",
        Some(_) => false,
    }
}

const CONNECTIVITY_PROBE_INTERVAL: Duration = Duration::from_secs(30);
const CONNECTIVITY_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const LINK_UP_SETTLE_DELAY: Duration = Duration::from_secs(3);
const LINK_UP_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct NetworkSnapshot {
    interface: String,
    up: bool,
    mbps: u16,
    ipv4: [u8; 4],
}

struct ProbeState {
    status: InternetStatus,
    consecutive_failures: u8,
    last_success: Option<Instant>,
    next_probe: Instant,
    in_flight: bool,
    last_link_up: Option<bool>,
    generation: u32,
    link_up_grace_retry: bool,
    interval: Duration,
    settle_delay: Duration,
    retry_delay: Duration,
    failures_before_failed: u8,
}

impl ProbeState {
    #[cfg(test)]
    fn new(now: Instant) -> Self {
        Self::new_configured(
            now,
            CONNECTIVITY_PROBE_INTERVAL,
            LINK_UP_SETTLE_DELAY,
            LINK_UP_RETRY_DELAY,
            2,
        )
    }

    fn new_configured(
        now: Instant,
        interval: Duration,
        settle_delay: Duration,
        retry_delay: Duration,
        failures_before_failed: u8,
    ) -> Self {
        Self {
            status: InternetStatus::Checking,
            consecutive_failures: 0,
            last_success: None,
            next_probe: now,
            in_flight: false,
            last_link_up: None,
            generation: 0,
            link_up_grace_retry: false,
            interval,
            settle_delay,
            retry_delay,
            failures_before_failed,
        }
    }

    fn prepare_probe(&mut self, now: Instant, link_up: bool) -> Option<u32> {
        if self.last_link_up != Some(link_up) {
            self.last_link_up = Some(link_up);
            self.next_probe = if link_up {
                now + self.settle_delay
            } else {
                now
            };
            self.status = InternetStatus::Checking;
            self.consecutive_failures = 0;
            self.generation = self.generation.wrapping_add(1);
            self.link_up_grace_retry = link_up;
        }
        if self.in_flight || now < self.next_probe {
            return None;
        }
        self.in_flight = true;
        self.next_probe = now + self.interval;
        Some(self.generation)
    }

    fn record_result(&mut self, generation: u32, reachable: bool, now: Instant) {
        self.in_flight = false;
        if self.generation != generation {
            return;
        }
        if reachable {
            self.status = InternetStatus::Reachable;
            self.consecutive_failures = 0;
            self.last_success = Some(now);
            self.link_up_grace_retry = false;
        } else if self.link_up_grace_retry {
            self.status = InternetStatus::Checking;
            self.link_up_grace_retry = false;
            self.next_probe = now + self.retry_delay;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            self.status = if self.consecutive_failures < self.failures_before_failed {
                InternetStatus::Missed
            } else {
                InternetStatus::Failed
            };
        }
    }
}

struct ConnectivityProbe {
    state: Arc<Mutex<ProbeState>>,
    target: String,
    timeout: Duration,
}

impl ConnectivityProbe {
    fn new(config: Option<&InternetConfig>) -> Self {
        let target =
            config.map_or_else(|| "www.google.de".to_owned(), |value| value.target.clone());
        let timeout = config.map_or(CONNECTIVITY_PROBE_TIMEOUT, |value| value.timeout);
        let interval = config.map_or(CONNECTIVITY_PROBE_INTERVAL, |value| value.interval);
        let settle = config.map_or(LINK_UP_SETTLE_DELAY, |value| value.link_up_settle_delay);
        let retry = config.map_or(LINK_UP_RETRY_DELAY, |value| value.first_failure_retry_delay);
        let failures = config.map_or(2, |value| value.failures_before_failed);
        Self {
            state: Arc::new(Mutex::new(ProbeState::new_configured(
                Instant::now(),
                interval,
                settle,
                retry,
                failures,
            ))),
            target,
            timeout,
        }
    }

    fn snapshot(&self, link_up: bool) -> (InternetStatus, Option<u32>) {
        let now = Instant::now();
        let probe_generation = self
            .state
            .lock()
            .map_or(None, |mut state| state.prepare_probe(now, link_up));
        if let Some(probe_generation) = probe_generation {
            let shared = Arc::clone(&self.state);
            let target = self.target.clone();
            let timeout = self.timeout;
            if thread::Builder::new()
                .name("internet-probe".into())
                .spawn(move || {
                    let reachable = ping_target(&target, timeout);
                    if let Ok(mut state) = shared.lock() {
                        state.record_result(probe_generation, reachable, Instant::now());
                    }
                })
                .is_err()
                && let Ok(mut state) = self.state.lock()
            {
                state.record_result(probe_generation, false, Instant::now());
            }
        }

        self.state
            .lock()
            .map_or((InternetStatus::Checking, None), |state| {
                let age = state
                    .last_success
                    .map(|success| u32::try_from(success.elapsed().as_secs()).unwrap_or(u32::MAX));
                (state.status, age)
            })
    }
}

fn ping_target(target: &str, timeout: Duration) -> bool {
    let Ok(mut child) = Command::new("/usr/bin/ping")
        .args(["-4", "-n", "-c", "1", "-W", "2", target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn read_network() -> NetworkSnapshot {
    let Some((routed_interface, ipv4)) = read_route() else {
        return NetworkSnapshot::default();
    };
    let sysfs = Path::new("/sys/class/net");
    let physical_interface = resolve_physical_interface(sysfs, &routed_interface)
        .unwrap_or_else(|| routed_interface.clone());
    let (up, mbps) = read_link(sysfs, &physical_interface);
    NetworkSnapshot {
        interface: physical_interface,
        up,
        mbps,
        ipv4,
    }
}

fn read_route() -> Option<(String, [u8; 4])> {
    for executable in ["/usr/sbin/ip", "/usr/bin/ip"] {
        let Ok(output) = Command::new(executable)
            .args(["-4", "route", "get", "8.8.8.8"])
            .output()
        else {
            continue;
        };
        if output.status.success()
            && let Some(route) = parse_ip_route(&String::from_utf8_lossy(&output.stdout))
        {
            return Some(route);
        }
    }

    let interface = parse_default_route(&fs::read_to_string("/proc/net/route").ok()?)?;
    let ipv4 = read_interface_ipv4(&interface);
    Some((interface, ipv4))
}

fn parse_ip_route(output: &str) -> Option<(String, [u8; 4])> {
    let words: Vec<_> = output.split_whitespace().collect();
    let interface = words
        .windows(2)
        .find(|pair| pair[0] == "dev")?
        .get(1)?
        .to_string();
    let ipv4 = words
        .windows(2)
        .find(|pair| pair[0] == "src")
        .and_then(|pair| pair.get(1))
        .and_then(|address| address.parse::<Ipv4Addr>().ok())
        .map_or([0; 4], |address| address.octets());
    Some((interface, ipv4))
}

fn parse_default_route(routes: &str) -> Option<String> {
    routes
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 8 || fields[1] != "00000000" || fields[7] != "00000000" {
                return None;
            }
            let flags = u16::from_str_radix(fields[3], 16).ok()?;
            if flags & 1 == 0 {
                return None;
            }
            let metric = fields[6].parse::<u32>().ok()?;
            Some((metric, fields[0].to_owned()))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, interface)| interface)
}

fn read_interface_ipv4(interface: &str) -> [u8; 4] {
    for executable in ["/usr/sbin/ip", "/usr/bin/ip"] {
        let Ok(output) = Command::new(executable)
            .args([
                "-4", "-o", "address", "show", "dev", interface, "scope", "global",
            ])
            .output()
        else {
            continue;
        };
        if output.status.success()
            && let Some(address) = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .find_map(|word| word.split('/').next()?.parse::<Ipv4Addr>().ok())
        {
            return address.octets();
        }
    }
    [0; 4]
}

fn resolve_physical_interface(sysfs: &Path, routed: &str) -> Option<String> {
    resolve_interface(sysfs, routed, &mut Vec::new())
}

fn resolve_interface(sysfs: &Path, interface: &str, visited: &mut Vec<String>) -> Option<String> {
    if visited.iter().any(|seen| seen == interface) {
        return None;
    }
    visited.push(interface.to_owned());
    let path = sysfs.join(interface);
    if !path.exists() {
        return None;
    }

    if let Ok(active_slave) = fs::read_to_string(path.join("bonding/active_slave")) {
        let active_slave = active_slave.trim();
        if !active_slave.is_empty()
            && let Some(resolved) = resolve_interface(sysfs, active_slave, visited)
        {
            return Some(resolved);
        }
    }

    if let Ok(entries) = fs::read_dir(path.join("brif")) {
        let mut candidates: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let mut branch_visited = visited.clone();
                resolve_interface(
                    sysfs,
                    &entry.file_name().to_string_lossy(),
                    &mut branch_visited,
                )
            })
            .collect();
        candidates.sort();
        return candidates.into_iter().max_by_key(|candidate| {
            let candidate_path = sysfs.join(candidate);
            let (up, speed) = read_link(sysfs, candidate);
            (candidate_path.join("device").exists(), up, speed)
        });
    }

    if let Ok(entries) = fs::read_dir(&path)
        && let Some(lower) = entries
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with("lower_"))
            .and_then(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .strip_prefix("lower_")
                    .map(str::to_owned)
            })
        && let Some(resolved) = resolve_interface(sysfs, &lower, visited)
    {
        return Some(resolved);
    }

    Some(interface.to_owned())
}

fn read_link(sysfs: &Path, interface: &str) -> (bool, u16) {
    let path = sysfs.join(interface);
    let up = fs::read_to_string(path.join("carrier")).map_or_else(
        |_| fs::read_to_string(path.join("operstate")).is_ok_and(|state| state.trim() == "up"),
        |carrier| carrier.trim() == "1",
    );
    let speed = fs::read_to_string(path.join("speed"))
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0);
    (up, speed)
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
        return GuestSnapshot::default();
    };
    if !output.status.success() {
        return GuestSnapshot::default();
    }
    parse_guests(&output.stdout, host_name)
}

fn parse_guests(bytes: &[u8], host_name: &str) -> GuestSnapshot {
    let Ok(Value::Array(values)) = serde_json::from_slice(bytes) else {
        return GuestSnapshot::default();
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
    GuestSnapshot::new(guests)
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
    Some(GuestSummary::new(
        vmid,
        name,
        kind,
        status,
        cpu,
        memory_used_mib,
        memory_total_mib,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TempSysfs(std::path::PathBuf);

    impl TempSysfs {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "servatory-sysfs-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_df_usage_and_available_space() {
        let usage = parse_filesystem_usage(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sdc1 9767546464 5476083304 3758096384 60% /mnt/pve/backup\n",
            "/mnt/pve/backup",
        )
        .unwrap();
        assert!(usage.mounted);
        assert_eq!(usage.used_percent, 60);
        assert_eq!(usage.available_mib, 3_670_016);
        assert!(parse_filesystem_usage(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/mapper/pve-root 98559220 5000000 87000000 6% /\n",
            "/mnt/pve/backup",
        )
        .is_none());
    }

    #[test]
    fn parses_read_only_nut_status_and_estimated_power() {
        let snapshot = parse_upsc(
            "battery.charge: 100\nbattery.runtime: 2160\nups.load: 15\nups.realpower: 102\nups.status: OL\n",
        )
        .unwrap();
        assert_eq!(snapshot.status, UpsStatus::Online);
        assert_eq!(snapshot.battery_percent, Some(100));
        assert_eq!(snapshot.load_percent, Some(15));
        assert_eq!(snapshot.runtime_seconds, Some(2_160));
        assert_eq!(snapshot.estimated_watts, Some(102));
        assert!(!snapshot.stale);
    }

    #[test]
    fn nut_status_uses_the_most_urgent_flag() {
        assert_eq!(
            parse_upsc("ups.status: OB LB DISCHRG\n").unwrap().status,
            UpsStatus::LowBattery
        );
        assert_eq!(
            parse_upsc("ups.status: OL RB\n").unwrap().status,
            UpsStatus::ReplaceBattery
        );
    }

    #[test]
    fn parses_smart_temperature_from_json_variants() {
        let direct: Value = serde_json::from_str(r#"{"temperature":{"current":31}}"#).unwrap();
        assert_eq!(smart_temperature(&direct), Some(31));
        let attribute: Value = serde_json::from_str(
            r#"{"ata_smart_attributes":{"table":[{"name":"Temperature_Celsius","raw":{"value":38}}]}}"#,
        )
        .unwrap();
        assert_eq!(smart_temperature(&attribute), Some(38));
    }

    #[test]
    fn backup_status_uses_latest_successful_vzdump_age() {
        let now = 1_800_000_000;
        let (status, age) = parse_backup_job_status(
            br#"[{"id":"nightly","enabled":1,"node":"pve-01"}]"#,
            br#"[{"node":"pve-01","starttime":1799978000,"endtime":1799978400,"status":"OK"}]"#,
            "pve-01",
            now,
        );
        assert_eq!(status, BackupJobStatus::Healthy);
        assert_eq!(age, Some(21_600));
    }

    #[test]
    fn newer_failed_backup_overrides_a_recent_success() {
        let (status, age) = parse_backup_job_status(
            br#"[{"id":"nightly","enabled":true}]"#,
            br#"[{"starttime":100,"endtime":110,"status":"OK"},{"starttime":120,"endtime":130,"status":"ERROR: disk full"}]"#,
            "pve-01",
            200,
        );
        assert_eq!(status, BackupJobStatus::Failed);
        assert_eq!(age, Some(90));
    }

    #[test]
    fn backup_age_is_reported_without_applying_health_policy() {
        let (status, age) = parse_backup_job_status(
            br#"[{"id":"nightly"}]"#,
            br#"[{"starttime":1,"endtime":100,"status":"OK"}]"#,
            "pve-01",
            86_501,
        );
        assert_eq!(status, BackupJobStatus::Healthy);
        assert_eq!(age, Some(86_401));
    }

    #[test]
    fn disabled_or_remote_jobs_do_not_count() {
        let (status, age) = parse_backup_job_status(
            br#"[{"id":"disabled","enabled":0},{"id":"remote","node":"pve-02"}]"#,
            br"[]",
            "pve-01",
            100,
        );
        assert_eq!(status, BackupJobStatus::NoJob);
        assert_eq!(age, None);
    }

    #[test]
    fn parses_routed_interface_and_source_address() {
        assert_eq!(
            parse_ip_route("8.8.8.8 via 192.168.1.1 dev vmbr0 src 192.168.1.50 uid 0\n"),
            Some(("vmbr0".to_owned(), [192, 168, 1, 50]))
        );
    }

    #[test]
    fn fallback_route_uses_lowest_metric_default() {
        let routes = "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
                      eno1 00000000 0101A8C0 0003 0 0 200 00000000 0 0 0\n\
                      vmbr0 00000000 0101A8C0 0003 0 0 100 00000000 0 0 0\n";
        assert_eq!(parse_default_route(routes).as_deref(), Some("vmbr0"));
    }

    #[test]
    fn bridge_resolution_prefers_physical_port_over_faster_tap() {
        let sysfs = TempSysfs::new();
        for interface in ["vmbr0", "enp3s0", "tap100i0"] {
            fs::create_dir_all(sysfs.0.join(interface)).unwrap();
        }
        fs::create_dir_all(sysfs.0.join("vmbr0/brif/enp3s0")).unwrap();
        fs::create_dir_all(sysfs.0.join("vmbr0/brif/tap100i0")).unwrap();
        fs::create_dir_all(sysfs.0.join("enp3s0/device")).unwrap();
        fs::write(sysfs.0.join("enp3s0/carrier"), "1\n").unwrap();
        fs::write(sysfs.0.join("enp3s0/speed"), "2500\n").unwrap();
        fs::write(sysfs.0.join("tap100i0/carrier"), "1\n").unwrap();
        fs::write(sysfs.0.join("tap100i0/speed"), "10000\n").unwrap();

        assert_eq!(
            resolve_physical_interface(&sysfs.0, "vmbr0").as_deref(),
            Some("enp3s0")
        );
        assert_eq!(read_link(&sysfs.0, "enp3s0"), (true, 2_500));
    }

    #[test]
    fn link_up_waits_then_retries_before_counting_a_failure() {
        let now = Instant::now();
        let mut state = ProbeState::new(now);
        assert!(state.prepare_probe(now, true).is_none());
        assert!(
            state
                .prepare_probe(now + Duration::from_secs(2), true)
                .is_none()
        );
        let initial_generation = state
            .prepare_probe(now + LINK_UP_SETTLE_DELAY, true)
            .unwrap();
        state.record_result(initial_generation, false, now + LINK_UP_SETTLE_DELAY);
        assert_eq!(state.status, InternetStatus::Checking);
        assert_eq!(state.consecutive_failures, 0);

        assert!(
            state
                .prepare_probe(now + Duration::from_secs(7), true)
                .is_none()
        );
        let retry_at = now + LINK_UP_SETTLE_DELAY + LINK_UP_RETRY_DELAY;
        let retry_generation = state.prepare_probe(retry_at, true).unwrap();
        state.record_result(retry_generation, false, retry_at);
        assert_eq!(state.status, InternetStatus::Missed);
        assert_eq!(state.consecutive_failures, 1);
    }

    #[test]
    fn link_down_schedules_an_immediate_fresh_probe() {
        let now = Instant::now();
        let mut state = ProbeState::new(now);
        assert!(state.prepare_probe(now, true).is_none());
        let initial_generation = state
            .prepare_probe(now + LINK_UP_SETTLE_DELAY, true)
            .unwrap();
        let succeeded_at = now + LINK_UP_SETTLE_DELAY;
        state.record_result(initial_generation, true, succeeded_at);

        let changed_generation = state
            .prepare_probe(succeeded_at + Duration::from_secs(1), false)
            .unwrap();
        assert_ne!(changed_generation, initial_generation);
        assert_eq!(state.status, InternetStatus::Checking);
        assert_eq!(state.last_success, Some(succeeded_at));
    }

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
}
