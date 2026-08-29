use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
use serde_yaml::Value;
use servatory_protocol::{
    BackupJobStatus, DisplayConfig, DisplayLabel, DisplayPage, DisplayView, HealthLevel,
    HealthReport, HealthSnapshot, HostMessage, HttpConfig, Incident, IncidentId, MAX_FRAME_LEN,
    NetworkConfig as DeviceNetworkConfig, NotificationPriority, NotificationSeverities, NtfyConfig,
    RuleId, SmartStatus, SoftwareVersion, UpsStatus, encode_host,
};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/servatory/config.yaml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u8,
    pub host: HostConfig,
    pub connection: ConnectionConfig,
    pub actions: ActionsConfig,
    pub sources: SourcesConfig,
    pub health: HealthConfig,
    pub views: BTreeMap<String, ViewConfig>,
    pub outputs: OutputsConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    #[serde(deserialize_with = "duration")]
    pub update_interval: Duration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub usb_serial: UsbSerialConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbSerialConfig {
    pub device: PathBuf,
    #[serde(deserialize_with = "duration")]
    pub reconnect_interval: Duration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {
    pub shutdown: ShutdownConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "duration")]
    pub hold_time: Duration,
    #[serde(deserialize_with = "duration")]
    pub animation_delay: Duration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcesConfig {
    pub system: SystemConfig,
    pub filesystems: Vec<FilesystemConfig>,
    pub smart: SmartConfig,
    pub ups: UpsConfig,
    pub network: NetworkConfig,
    pub internet: InternetConfig,
    pub proxmox: ProxmoxConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub provider: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemConfig {
    pub id: String,
    pub path: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmartConfig {
    pub devices: Vec<SmartDeviceConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmartDeviceConfig {
    pub id: String,
    pub path: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsConfig {
    pub endpoint: Option<String>,
    #[serde(default = "two")]
    pub failures_before_unavailable: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub interface: String,
    pub resolve_physical_interface: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxmoxConfig {
    pub node: String,
    pub backup: BackupConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    pub task_history_limit: u16,
}

const fn two() -> u8 {
    2
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternetConfig {
    pub target: String,
    #[serde(deserialize_with = "duration")]
    pub interval: Duration,
    #[serde(deserialize_with = "duration")]
    pub timeout: Duration,
    #[serde(deserialize_with = "duration")]
    pub link_up_settle_delay: Duration,
    #[serde(deserialize_with = "duration")]
    pub first_failure_retry_delay: Duration,
    pub failures_before_failed: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    pub default: HealthDefault,
    pub rules: Vec<HealthRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthDefault {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthRule {
    pub id: String,
    pub severity: Severity,
    pub when: Condition,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Healthy,
    Warning,
    Critical,
}

impl From<Severity> for HealthLevel {
    fn from(value: Severity) -> Self {
        match value {
            Severity::Healthy => Self::Healthy,
            Severity::Warning => Self::Warning,
            Severity::Critical => Self::Critical,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    All { all: Vec<Condition> },
    Any { any: Vec<Condition> },
    Predicate(Predicate),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub measurement: String,
    pub equals: Option<Value>,
    pub r#in: Option<Vec<Value>>,
    pub at_least: Option<f64>,
    pub greater_than: Option<f64>,
    #[serde(default, deserialize_with = "optional_duration")]
    pub missing_or_greater_than: Option<Duration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewConfig {
    pub title: String,
    pub layout: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputsConfig {
    pub stick: StickOutput,
    pub http: Option<HttpOutput>,
    pub ntfy: Option<NtfyOutput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StickOutput {
    pub lcd: LcdOutput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcdOutput {
    pub views: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpOutput {
    pub enabled: bool,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default = "default_http_port")]
    pub port: u16,
    pub views: Vec<String>,
}

fn default_hostname() -> String {
    "servatory".to_owned()
}

const fn default_http_port() -> u16 {
    80
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NtfyOutput {
    pub enabled: bool,
    pub server: String,
    pub severities: Vec<Severity>,
    pub priorities: NtfyPriorities,
    pub notify_recovery: bool,
    #[serde(default, deserialize_with = "optional_duration")]
    pub repeat_critical: Option<Duration>,
    pub click_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NtfyPriorities {
    pub warning: Priority,
    pub critical: Priority,
    pub recovery: Priority,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Default,
    High,
    Urgent,
}

impl From<Priority> for NotificationPriority {
    fn from(value: Priority) -> Self {
        match value {
            Priority::Default => Self::Default,
            Priority::High => Self::High,
            Priority::Urgent => Self::Urgent,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        let config: Self = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported configuration version {}", self.version);
        }
        if self.host.update_interval.is_zero() {
            bail!("host.update_interval must be positive");
        }
        if self.connection.usb_serial.reconnect_interval.is_zero() {
            bail!("connection.usb_serial.reconnect_interval must be positive");
        }
        if self.actions.shutdown.hold_time.is_zero() {
            bail!("actions.shutdown.hold_time must be positive");
        }
        if self.actions.shutdown.animation_delay >= self.actions.shutdown.hold_time {
            bail!("actions.shutdown.animation_delay must be shorter than hold_time");
        }
        if self.sources.filesystems.is_empty() {
            bail!("at least one filesystem must be configured");
        }
        validate_ids(
            self.sources.filesystems.iter().map(|item| item.id.as_str()),
            "filesystem",
        )?;
        validate_ids(
            self.sources
                .smart
                .devices
                .iter()
                .map(|item| item.id.as_str()),
            "SMART device",
        )?;
        for fs in &self.sources.filesystems {
            if fs.label.is_empty() {
                bail!("filesystem {} label must not be empty", fs.id);
            }
            if !fs.path.starts_with('/') {
                bail!("filesystem {} path must be absolute", fs.id);
            }
        }
        for disk in &self.sources.smart.devices {
            if disk.label.is_empty() {
                bail!("SMART device {} label must not be empty", disk.id);
            }
            if !disk.path.starts_with("/dev/") {
                bail!("SMART device {} path must start with /dev/", disk.id);
            }
        }
        if self.sources.ups.failures_before_unavailable == 0 {
            bail!("UPS failures_before_unavailable must be positive");
        }
        if self.sources.system.provider != "procfs" {
            bail!("only the procfs system provider is currently supported");
        }
        if self.sources.network.interface != "default_ipv4_route"
            || !self.sources.network.resolve_physical_interface
        {
            bail!("the current firmware requires the resolved default IPv4 route interface");
        }
        if self.sources.proxmox.node != "local_hostname"
            || self.sources.proxmox.backup.task_history_limit == 0
        {
            bail!("invalid Proxmox node or backup history limit");
        }
        if self.sources.internet.target.is_empty()
            || self.sources.internet.interval.is_zero()
            || self.sources.internet.timeout.is_zero()
            || self.sources.internet.failures_before_failed == 0
        {
            bail!("internet probe target, durations, and failure count must be positive");
        }
        validate_message(&self.health.default.message, "health.default.message")?;
        validate_ids(
            self.health.rules.iter().map(|rule| rule.id.as_str()),
            "health rule",
        )?;
        for rule in &self.health.rules {
            validate_condition(&rule.when, self)
                .with_context(|| format!("health rule {:?}", rule.id))?;
            validate_message(&rule.message, &format!("health rule {} message", rule.id))?;
        }
        self.validate_outputs()?;
        self.validate_manifest_sizes()
    }

    fn validate_outputs(&self) -> Result<()> {
        if let Some(http) = &self.outputs.http {
            if http.enabled && http.views.is_empty() {
                bail!("outputs.http.views must not be empty when HTTP output is enabled");
            }
            for id in &http.views {
                if !self.views.contains_key(id) {
                    bail!("unknown HTTP view {id:?}");
                }
            }
            if http.hostname.is_empty()
                || !http
                    .hostname
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || http.port == 0
            {
                bail!("outputs.http hostname and port are invalid");
            }
        }
        if let Some(ntfy) = &self.outputs.ntfy {
            if ntfy.enabled
                && !ntfy.server.starts_with("https://")
                && !ntfy.server.starts_with("http://")
            {
                bail!("outputs.ntfy.server must be an HTTP or HTTPS URL");
            }
            if ntfy.enabled && ntfy.severities.is_empty() {
                bail!("outputs.ntfy.severities must not be empty when ntfy is enabled");
            }
            if ntfy.severities.contains(&Severity::Healthy) {
                bail!("outputs.ntfy.severities cannot contain healthy");
            }
            if ntfy.enabled
                && ntfy
                    .repeat_critical
                    .is_some_and(|duration| duration.is_zero())
            {
                bail!("outputs.ntfy.repeat-critical must be greater than zero");
            }
        }
        Ok(())
    }

    fn validate_manifest_sizes(&self) -> Result<()> {
        let display = self.display_config(0)?;
        let mut frame = vec![0_u8; MAX_FRAME_LEN];
        encode_host(HostMessage::DisplayConfig(display), &mut frame).map_err(|_| {
            anyhow::anyhow!(
                "configured display manifest does not fit the {MAX_FRAME_LEN}-byte device frame budget"
            )
        })?;
        encode_host(
            HostMessage::NetworkConfig(self.network_config(0)?),
            &mut frame,
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "configured network manifest does not fit the {MAX_FRAME_LEN}-byte device frame budget"
            )
        })?;
        Ok(())
    }

    pub fn display_config(&self, guest_count: usize) -> Result<DisplayConfig> {
        let labels: Vec<_> = self
            .sources
            .filesystems
            .iter()
            .map(|fs| DisplayLabel::new(&fs.label))
            .collect();
        let hold = u16::try_from(self.actions.shutdown.hold_time.as_millis())
            .context("shutdown hold_time exceeds device limit")?;
        let delay = u16::try_from(self.actions.shutdown.animation_delay.as_millis())
            .context("shutdown animation_delay exceeds device limit")?;
        let mut display = DisplayConfig::new(hold, delay, labels, self.compile_pages(guest_count)?)
            .map_err(|error| anyhow::anyhow!("invalid display configuration: {error}"))?;
        display.daemon_version = SoftwareVersion::new(env!("SERVATORY_BUILD_VERSION"));
        Ok(display)
    }

    fn compile_pages(&self, guest_count: usize) -> Result<Vec<DisplayView>> {
        self.compile_pages_for(&self.outputs.stick.lcd.views, guest_count, "LCD")
    }

    fn compile_pages_for(
        &self,
        view_ids: &[String],
        guest_count: usize,
        output_name: &str,
    ) -> Result<Vec<DisplayView>> {
        if view_ids.is_empty() {
            bail!("{output_name} views must not be empty");
        }
        let mut pages = Vec::new();
        for id in view_ids {
            let view = self
                .views
                .get(id)
                .with_context(|| format!("unknown LCD view {id:?}"))?;
            let title = DisplayLabel::new(&view.title);
            if contains(&view.layout, "guest_list")
                && find_bool(&view.layout, "paginate") == Some(true)
            {
                let count = guest_count.max(1).div_ceil(4);
                for index in 0..count {
                    pages.push(DisplayView::new(
                        title.clone(),
                        DisplayPage::Guests {
                            offset: u32::try_from(index * 4)?,
                            limit: 4,
                        },
                    ));
                }
                continue;
            }
            if contains(&view.layout, "filesystems") && contains(&view.layout, "smart") {
                let filesystem_ids = find_string_list(&view.layout, "filesystems")
                    .context("storage view requires a filesystem ID list")?;
                let smart_ids = find_string_list(&view.layout, "smart")
                    .context("storage view requires a SMART device ID list")?;
                let filesystem_ids_available: Vec<_> = self
                    .sources
                    .filesystems
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect();
                let smart_ids_available: Vec<_> = self
                    .sources
                    .smart
                    .devices
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect();
                let filesystem_indices =
                    resolve_indices(&filesystem_ids, &filesystem_ids_available, "filesystem")?;
                let smart_indices =
                    resolve_indices(&smart_ids, &smart_ids_available, "SMART device")?;
                let page_count = filesystem_indices
                    .len()
                    .div_ceil(3)
                    .max(smart_indices.len().div_ceil(5))
                    .max(1);
                let filesystems_left = column_contains(&view.layout, "left", "filesystems");
                for index in 0..page_count {
                    pages.push(DisplayView::new(
                        title.clone(),
                        DisplayPage::Storage {
                            filesystems_left,
                            filesystem_indices: filesystem_indices
                                .iter()
                                .skip(index * 3)
                                .take(3)
                                .copied()
                                .collect(),
                            smart_indices: smart_indices
                                .iter()
                                .skip(index * 5)
                                .take(5)
                                .copied()
                                .collect(),
                        },
                    ));
                }
                continue;
            }
            let page = page_from_layout(&view.layout).with_context(|| format!("view {id:?}"))?;
            pages.push(DisplayView::new(title, page));
        }
        Ok(pages)
    }

    pub fn network_config(&self, guest_count: usize) -> Result<DeviceNetworkConfig> {
        let http = if let Some(http) = &self.outputs.http {
            HttpConfig::new(
                http.enabled,
                &http.hostname,
                http.port,
                self.compile_pages_for(&http.views, guest_count, "HTTP")?,
            )
        } else {
            HttpConfig::new(false, "servatory", 80, Vec::new())
        };
        let ntfy = if let Some(ntfy) = &self.outputs.ntfy {
            NtfyConfig::new(
                ntfy.enabled,
                &ntfy.server,
                NotificationSeverities {
                    warning: ntfy.severities.contains(&Severity::Warning),
                    critical: ntfy.severities.contains(&Severity::Critical),
                },
                ntfy.notify_recovery,
                ntfy.priorities.warning.into(),
                ntfy.priorities.critical.into(),
                ntfy.priorities.recovery.into(),
                ntfy.repeat_critical
                    .map(|duration| u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)),
                ntfy.click_url.as_deref(),
            )
        } else {
            NtfyConfig::new(
                false,
                "https://ntfy.sh",
                NotificationSeverities {
                    warning: false,
                    critical: false,
                },
                false,
                NotificationPriority::High,
                NotificationPriority::Urgent,
                NotificationPriority::Default,
                None,
                None,
            )
        };
        Ok(DeviceNetworkConfig { http, ntfy })
    }

    pub fn evaluate_health(&self, snapshot: &HealthSnapshot) -> (HealthReport, Vec<Incident>) {
        let mut incidents = Vec::new();
        for rule in &self.health.rules {
            if matches_condition(&rule.when, snapshot, self) {
                let message = render_message(&rule.message, snapshot, self);
                incidents.push(Incident::new(
                    IncidentId::Rule(RuleId::new(&rule.id).expect("validated health rule ID")),
                    rule.severity.into(),
                    &message,
                ));
            }
        }
        let primary = incidents
            .iter()
            .fold(None, |primary: Option<&Incident>, incident| {
                if primary
                    .is_none_or(|current| incident.level.priority() > current.level.priority())
                {
                    Some(incident)
                } else {
                    primary
                }
            })
            .map_or_else(
                || {
                    HealthReport::new(
                        self.health.default.severity.into(),
                        &self.health.default.message,
                    )
                },
                |incident| HealthReport::new(incident.level, incident.message()),
            );
        (primary, incidents)
    }
}

fn validate_ids<'a>(ids: impl Iterator<Item = &'a str>, kind: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.is_empty()
            || id.len() > servatory_protocol::MAX_RULE_ID_LEN
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!("invalid {kind} id {id:?}");
        }
        if !seen.insert(id) {
            bail!("duplicate {kind} id {id:?}");
        }
    }
    Ok(())
}

fn validate_message(message: &str, field: &str) -> Result<()> {
    if message.is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn validate_condition(condition: &Condition, config: &Config) -> Result<()> {
    match condition {
        Condition::All { all } => {
            if all.is_empty() {
                bail!("all condition must not be empty");
            }
            for child in all {
                validate_condition(child, config)?;
            }
        }
        Condition::Any { any } => {
            if any.is_empty() {
                bail!("any condition must not be empty");
            }
            for child in any {
                validate_condition(child, config)?;
            }
        }
        Condition::Predicate(predicate) => {
            let operators = usize::from(predicate.equals.is_some())
                + usize::from(predicate.r#in.is_some())
                + usize::from(predicate.at_least.is_some())
                + usize::from(predicate.greater_than.is_some())
                + usize::from(predicate.missing_or_greater_than.is_some());
            if operators != 1 {
                bail!("predicate must specify exactly one comparison");
            }
            let known = matches!(
                predicate.measurement.as_str(),
                "ups.main.status"
                    | "ups.main.stale"
                    | "smart.*.status"
                    | "network.uplink.up"
                    | "network.internet.status"
                    | "proxmox.backup.status"
                    | "proxmox.backup.last_success_age"
                    | "system.cpu.percent"
                    | "system.memory.used_percent"
                    | "system.io_pressure.percent"
            ) || valid_filesystem_measurement(&predicate.measurement, config);
            if !known {
                bail!("unknown measurement {:?}", predicate.measurement);
            }
        }
    }
    Ok(())
}

fn valid_filesystem_measurement(path: &str, config: &Config) -> bool {
    let parts: Vec<_> = path.split('.').collect();
    parts.len() == 3
        && parts[0] == "filesystem"
        && matches!(parts[2], "mounted" | "used_percent")
        && config
            .sources
            .filesystems
            .iter()
            .any(|filesystem| filesystem.id == parts[1])
}

fn page_from_layout(layout: &Value) -> Result<DisplayPage> {
    if contains(layout, "guest_list") {
        let offset = find_number(layout, "offset").unwrap_or(0);
        let limit = find_number(layout, "limit").unwrap_or(4);
        return Ok(DisplayPage::Guests {
            offset: u32::try_from(offset)?,
            limit: u32::try_from(limit)?,
        });
    }
    if column_contains(layout, "left", "health") && column_contains(layout, "right", "host_summary")
    {
        Ok(DisplayPage::Overview)
    } else if contains(layout, "cpu") && contains(layout, "memory") {
        Ok(DisplayPage::Resources)
    } else if column_contains(layout, "left", "ups") && column_contains(layout, "right", "network")
    {
        Ok(DisplayPage::PowerNetwork { ups_left: true })
    } else if column_contains(layout, "right", "ups") && column_contains(layout, "left", "network")
    {
        Ok(DisplayPage::PowerNetwork { ups_left: false })
    } else {
        bail!("layout cannot be rendered by the current firmware")
    }
}

fn resolve_indices(ids: &[String], available: &[&str], kind: &str) -> Result<Vec<u32>> {
    ids.iter()
        .map(|id| {
            available
                .iter()
                .position(|candidate| *candidate == id)
                .with_context(|| format!("unknown {kind} ID {id:?}"))
                .and_then(|index| {
                    u32::try_from(index).context("resource index exceeds protocol range")
                })
        })
        .collect()
}

fn find_string_list(value: &Value, needle: &str) -> Option<Vec<String>> {
    match value {
        Value::Mapping(values) => values.iter().find_map(|(key, value)| {
            if key.as_str() == Some(needle) {
                return value.as_sequence().map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                });
            }
            find_string_list(value, needle)
        }),
        Value::Sequence(values) => values
            .iter()
            .find_map(|value| find_string_list(value, needle)),
        _ => None,
    }
}

fn find_bool(value: &Value, needle: &str) -> Option<bool> {
    match value {
        Value::Mapping(values) => values.iter().find_map(|(key, value)| {
            (key.as_str() == Some(needle))
                .then(|| value.as_bool())
                .flatten()
                .or_else(|| find_bool(value, needle))
        }),
        Value::Sequence(values) => values.iter().find_map(|value| find_bool(value, needle)),
        _ => None,
    }
}

fn column_contains(layout: &Value, side: &str, needle: &str) -> bool {
    layout
        .as_mapping()
        .and_then(|layout| layout.get(Value::String("columns".into())))
        .and_then(Value::as_mapping)
        .and_then(|columns| columns.get(Value::String(side.into())))
        .is_some_and(|value| contains(value, needle))
}

fn contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(value) => value == needle,
        Value::Sequence(values) => values.iter().any(|value| contains(value, needle)),
        Value::Mapping(values) => values
            .iter()
            .any(|(key, value)| contains(key, needle) || contains(value, needle)),
        _ => false,
    }
}

fn find_number(value: &Value, needle: &str) -> Option<u64> {
    match value {
        Value::Mapping(values) => values.iter().find_map(|(key, value)| {
            (key.as_str() == Some(needle))
                .then(|| value.as_u64())
                .flatten()
                .or_else(|| find_number(value, needle))
        }),
        Value::Sequence(values) => values.iter().find_map(|value| find_number(value, needle)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
enum Metric {
    Bool(bool),
    Number(f64),
    Text(&'static str),
    Missing,
}

fn matches_condition(condition: &Condition, snapshot: &HealthSnapshot, config: &Config) -> bool {
    match condition {
        Condition::All { all } => all
            .iter()
            .all(|item| matches_condition(item, snapshot, config)),
        Condition::Any { any } => any
            .iter()
            .any(|item| matches_condition(item, snapshot, config)),
        Condition::Predicate(predicate) => metrics(&predicate.measurement, snapshot, config)
            .iter()
            .any(|value| matches_predicate(value, predicate)),
    }
}

fn matches_predicate(value: &Metric, predicate: &Predicate) -> bool {
    if let Some(expected) = &predicate.equals {
        return metric_equals(value, expected);
    }
    if let Some(expected) = &predicate.r#in {
        return expected.iter().any(|item| metric_equals(value, item));
    }
    if let Some(limit) = predicate.at_least {
        return matches!(value, Metric::Number(number) if *number >= limit);
    }
    if let Some(limit) = predicate.greater_than {
        return matches!(value, Metric::Number(number) if *number > limit);
    }
    if let Some(limit) = predicate.missing_or_greater_than {
        return matches!(value, Metric::Missing)
            || matches!(value, Metric::Number(number) if *number > limit.as_secs_f64());
    }
    false
}

fn metric_equals(metric: &Metric, expected: &Value) -> bool {
    match metric {
        Metric::Bool(value) => expected.as_bool() == Some(*value),
        Metric::Number(value) => expected.as_f64() == Some(*value),
        Metric::Text(value) => expected.as_str() == Some(*value),
        Metric::Missing => expected.is_null(),
    }
}

fn metrics(path: &str, snapshot: &HealthSnapshot, config: &Config) -> Vec<Metric> {
    match path {
        "ups.main.status" => vec![Metric::Text(ups_status(snapshot.ups.status))],
        "ups.main.stale" => vec![Metric::Bool(snapshot.ups.stale)],
        "smart.*.status" => snapshot
            .smart
            .devices()
            .iter()
            .map(|device| Metric::Text(smart_status(device.status)))
            .collect(),
        "network.uplink.up" => vec![Metric::Bool(snapshot.network_up)],
        "network.internet.status" => vec![Metric::Text(internet_status(snapshot.internet_status))],
        "proxmox.backup.status" => vec![Metric::Text(backup_status(snapshot.backup_job_status))],
        "proxmox.backup.last_success_age" => vec![
            snapshot
                .last_successful_backup_age_seconds
                .map_or(Metric::Missing, |value| Metric::Number(f64::from(value))),
        ],
        "system.cpu.percent" => vec![Metric::Number(f64::from(snapshot.cpu_percent))],
        "system.memory.used_percent" => vec![Metric::Number(f64::from(memory_percent(snapshot)))],
        "system.io_pressure.percent" => {
            vec![Metric::Number(f64::from(snapshot.io_pressure_percent))]
        }
        _ if path.starts_with("filesystem.") => filesystem_metric(path, snapshot, config),
        _ => vec![Metric::Missing],
    }
}

fn filesystem_metric(path: &str, snapshot: &HealthSnapshot, config: &Config) -> Vec<Metric> {
    let mut parts = path.split('.');
    let _ = parts.next();
    let Some(id) = parts.next() else {
        return vec![Metric::Missing];
    };
    let Some(field) = parts.next() else {
        return vec![Metric::Missing];
    };
    let Some(index) = config.sources.filesystems.iter().position(|fs| fs.id == id) else {
        return vec![Metric::Missing];
    };
    let Some(usage) = snapshot.filesystems.get(index).copied() else {
        return vec![Metric::Missing];
    };
    match field {
        "mounted" => vec![Metric::Bool(usage.mounted)],
        "used_percent" => vec![Metric::Number(f64::from(usage.used_percent))],
        _ => vec![Metric::Missing],
    }
}

fn render_message(template: &str, snapshot: &HealthSnapshot, config: &Config) -> String {
    let mut message = template.to_owned();
    if let Some(start) = template.find("filesystem.") {
        let rest = &template[start + 11..];
        if let Some(end) = rest.find(".label}") {
            let id = &rest[..end];
            if let Some(fs) = config.sources.filesystems.iter().find(|fs| fs.id == id) {
                message = message.replace(&format!("{{filesystem.{id}.label}}"), &fs.label);
            }
        }
    }
    if message.contains("{value}") {
        let value = config
            .health
            .rules
            .iter()
            .find(|rule| rule.message == template)
            .and_then(|rule| metrics_for_value(&rule.when, snapshot, config))
            .unwrap_or(0.0);
        message = message.replace("{value}", &format!("{value:.0}"));
    }
    message
}

fn metrics_for_value(
    condition: &Condition,
    snapshot: &HealthSnapshot,
    config: &Config,
) -> Option<f64> {
    match condition {
        Condition::Predicate(predicate) => metrics(&predicate.measurement, snapshot, config)
            .into_iter()
            .find_map(|value| {
                if let Metric::Number(value) = value {
                    Some(value)
                } else {
                    None
                }
            }),
        Condition::All { all } => all
            .iter()
            .find_map(|condition| metrics_for_value(condition, snapshot, config)),
        Condition::Any { any } => any
            .iter()
            .find_map(|condition| metrics_for_value(condition, snapshot, config)),
    }
}

fn memory_percent(snapshot: &HealthSnapshot) -> u8 {
    if snapshot.memory_total_mib == 0 {
        0
    } else {
        u8::try_from(
            u64::from(snapshot.memory_used_mib) * 100 / u64::from(snapshot.memory_total_mib),
        )
        .unwrap_or(100)
    }
}
fn ups_status(value: UpsStatus) -> &'static str {
    match value {
        UpsStatus::NotConfigured => "not_configured",
        UpsStatus::Unknown => "unknown",
        UpsStatus::Online => "online",
        UpsStatus::OnBattery => "on_battery",
        UpsStatus::LowBattery => "low_battery",
        UpsStatus::Charging => "charging",
        UpsStatus::Bypass => "bypass",
        UpsStatus::OutputOff => "output_off",
        UpsStatus::ReplaceBattery => "replace_battery",
        UpsStatus::Unavailable => "unavailable",
    }
}
fn smart_status(value: SmartStatus) -> &'static str {
    match value {
        SmartStatus::Healthy => "healthy",
        SmartStatus::Warning => "warning",
        SmartStatus::Failed => "failed",
        SmartStatus::Sleeping => "sleeping",
        SmartStatus::Unknown => "unknown",
    }
}
fn internet_status(value: servatory_protocol::InternetStatus) -> &'static str {
    match value {
        servatory_protocol::InternetStatus::Checking => "checking",
        servatory_protocol::InternetStatus::Reachable => "reachable",
        servatory_protocol::InternetStatus::Missed => "missed",
        servatory_protocol::InternetStatus::Failed => "failed",
    }
}
fn backup_status(value: BackupJobStatus) -> &'static str {
    match value {
        BackupJobStatus::Unknown => "unknown",
        BackupJobStatus::NoJob => "no_job",
        BackupJobStatus::Healthy => "healthy",
        BackupJobStatus::Running => "running",
        BackupJobStatus::Failed => "failed",
        BackupJobStatus::Stale => "stale",
    }
}

fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let value = String::deserialize(deserializer)?;
    parse_duration(&value).map_err(serde::de::Error::custom)
}
fn optional_duration<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error> {
    Option::<String>::deserialize(deserializer)?
        .map(|value| parse_duration(&value).map_err(serde::de::Error::custom))
        .transpose()
}
fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let (number, unit) = value
        .find(|c: char| !c.is_ascii_digit())
        .map_or((value, "ms"), |index| value.split_at(index));
    let number: u64 = number
        .parse()
        .map_err(|_| format!("invalid duration {value:?}"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(number)),
        "s" => Ok(Duration::from_secs(number)),
        "m" => Ok(Duration::from_secs(number * 60)),
        "h" => Ok(Duration::from_secs(number * 3_600)),
        _ => Err(format!("invalid duration unit in {value:?}")),
    }
}

#[cfg(test)]
mod tests {
    use servatory_protocol::{
        BackupJobStatus, FilesystemUsage, GuestSnapshot, InternetStatus, SmartSnapshot, UpsSnapshot,
    };

    use super::*;

    fn current_config() -> Config {
        let config: Config = serde_yaml::from_str(include_str!("../../deploy/servatory.yaml"))
            .expect("deployed configuration parses");
        config.validate().expect("deployed configuration validates");
        config
    }

    fn healthy_snapshot() -> HealthSnapshot {
        HealthSnapshot::new(
            "pve-01",
            86_400,
            23,
            16_384,
            32_768,
            4,
            82,
            vec![
                FilesystemUsage::new(20, 80 * 1_024),
                FilesystemUsage::new(30, 7_000 * 1_024),
                FilesystemUsage::new(40, 4_000 * 1_024),
            ],
            BackupJobStatus::Healthy,
            Some(21_600),
            true,
            2_500,
            "enp3s0",
            InternetStatus::Reachable,
            Some(0),
            [10, 0, 0, 12],
            GuestSnapshot::default(),
            UpsSnapshot::NOT_CONFIGURED,
            SmartSnapshot::default(),
        )
    }

    #[test]
    fn deployed_configuration_compiles_to_six_pages() {
        let config = current_config();
        let display = config.display_config(5).unwrap();
        assert_eq!(display.pages().len(), 6);
        assert_eq!(display.pages()[2].title.as_str(), "STORAGE + SMART");
    }

    #[test]
    fn animation_delay_may_be_below_two_hundred_milliseconds() {
        let yaml = include_str!("../../deploy/servatory.yaml")
            .replace("animation_delay: 200ms", "animation_delay: 1ms");
        let config: Config = serde_yaml::from_str(&yaml).unwrap();
        config.validate().unwrap();
        assert_eq!(
            config
                .display_config(0)
                .unwrap()
                .shutdown_animation_delay_ms,
            1
        );
    }

    #[test]
    fn yaml_validation_rejects_a_manifest_beyond_the_device_budget() {
        let mut config = current_config();
        config.views.get_mut("overview").unwrap().title = "X".repeat(MAX_FRAME_LEN);
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("device frame budget"));
    }

    #[test]
    fn two_column_panels_can_be_swapped() {
        let yaml = include_str!("../../deploy/servatory.yaml").replace(
            "columns: { left: ups, right: network }",
            "columns: { left: network, right: ups }",
        );
        let config: Config = serde_yaml::from_str(&yaml).unwrap();
        config.validate().unwrap();
        assert!(matches!(
            config.display_config(0).unwrap().pages()[3].page,
            DisplayPage::PowerNetwork { ups_left: false }
        ));
    }

    #[test]
    fn ordered_yaml_policy_supplies_the_health_message() {
        let config = current_config();
        let mut snapshot = healthy_snapshot();
        assert_eq!(
            config.evaluate_health(&snapshot).0.level,
            HealthLevel::Healthy
        );
        snapshot.network_up = false;
        let (report, incidents) = config.evaluate_health(&snapshot);
        assert_eq!(report.level, HealthLevel::Critical);
        assert_eq!(report.message(), "LINK DOWN");
        assert_eq!(incidents.len(), 1);
        assert!(matches!(incidents[0].id, IncidentId::Rule(_)));
    }

    #[test]
    fn backup_age_threshold_lives_in_health_policy() {
        let config = current_config();
        let mut snapshot = healthy_snapshot();
        snapshot.last_successful_backup_age_seconds = Some(24 * 60 * 60 + 1);
        let (report, incidents) = config.evaluate_health(&snapshot);
        assert_eq!(report.level, HealthLevel::Warning);
        assert_eq!(report.message(), "BACKUP OVERDUE");
        assert_eq!(incidents.len(), 1);
    }

    #[test]
    fn all_matching_incidents_are_preserved_and_primary_uses_severity_then_order() {
        let config = current_config();
        let mut snapshot = healthy_snapshot();
        snapshot.network_up = false;
        snapshot.cpu_percent = 99;
        let (primary, incidents) = config.evaluate_health(&snapshot);
        assert_eq!(primary.level, HealthLevel::Critical);
        assert_eq!(primary.message(), "LINK DOWN");
        assert_eq!(incidents.len(), 2);
        assert_eq!(incidents[0].message(), "LINK DOWN");
        assert_eq!(incidents[1].message(), "CPU 99%");
    }
}
