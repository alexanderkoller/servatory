use alloc::{
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::RefCell, fmt::Write as _, net::Ipv4Addr};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use critical_section::Mutex as CriticalMutex;
use embassy_executor::Spawner;
use embassy_net::{
    IpListenEndpoint, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
    dns::DnsSocket,
    tcp::{
        TcpSocket,
        client::{TcpClient, TcpClientState},
    },
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AuthenticationMethod, Config as WifiConfig, ControllerConfig, Interface, WifiController,
    ap::AccessPointConfig, sta::StationConfig,
};
use reqwless::{
    client::{HttpClient, TlsConfig, TlsVerify},
    request::{Method, RequestBuilder},
};
use servatory_protocol::{
    DisplayPage, HealthLevel, HealthSnapshot, Incident, IncidentId, NetworkConfig,
    NotificationPriority, StickIncident,
};

use crate::provisioning::{Provisioning, Store, StoredSettings};

const PROVISIONING_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const HOST_STALE_AFTER: Duration = Duration::from_secs(15);
const MAX_PENDING_NOTIFICATIONS: usize = 8;
const ISRG_ROOT_X1: &str = concat!(
    "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",
);

#[derive(Clone)]
struct SharedState {
    snapshot: Option<HealthSnapshot>,
    manifest: Option<NetworkConfig>,
    last_update: Option<Instant>,
}

impl SharedState {
    const fn new() -> Self {
        Self {
            snapshot: None,
            manifest: None,
            last_update: None,
        }
    }
}

static STATE: Mutex<CriticalSectionRawMutex, SharedState> = Mutex::new(SharedState::new());
static MANIFESTS: Channel<CriticalSectionRawMutex, NetworkConfig, 1> = Channel::new();
static TEST_NOTIFICATIONS: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();

#[derive(Clone)]
pub struct ProvisioningDisplay {
    pub ssid: heapless::String<32>,
}

static PROVISIONING_DISPLAY: CriticalMutex<RefCell<Option<ProvisioningDisplay>>> =
    CriticalMutex::new(RefCell::new(None));

pub fn provisioning_display() -> Option<ProvisioningDisplay> {
    critical_section::with(|section| PROVISIONING_DISPLAY.borrow(section).borrow().clone())
}

pub async fn update_snapshot(snapshot: HealthSnapshot) {
    let mut state = STATE.lock().await;
    state.snapshot = Some(snapshot);
    state.last_update = Some(Instant::now());
}

pub async fn update_manifest(manifest: NetworkConfig) {
    let mut state = STATE.lock().await;
    if state.manifest.as_ref() == Some(&manifest) {
        return;
    }
    state.manifest = Some(manifest.clone());
    drop(state);
    if let Err(embassy_sync::channel::TrySendError::Full(manifest)) = MANIFESTS.try_send(manifest) {
        let _ = MANIFESTS.try_receive();
        let _ = MANIFESTS.try_send(manifest);
    }
}

pub async fn start(
    spawner: Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    flash: esp_hal::peripherals::FLASH<'static>,
    force_provisioning: bool,
) {
    let mut store = Store::new(flash);
    let saved = (!force_provisioning).then(|| store.load()).flatten();
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | u64::from(rng.random());
    if let Some(settings) = saved {
        if let Some(manifest) = settings.network.clone() {
            STATE.lock().await.manifest = Some(manifest);
        }
        start_station(spawner, wifi, store, settings, seed);
    } else {
        let ntfy_topic = format!(
            "servatory-{:08x}{:08x}{:08x}{:08x}",
            rng.random(),
            rng.random(),
            rng.random(),
            rng.random()
        );
        start_provisioning(spawner, wifi, store, seed, ntfy_topic);
    }
}

fn start_station(
    spawner: Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    store: Store<'static>,
    settings: StoredSettings,
    seed: u64,
) {
    let provisioning = settings.provisioning;
    let station = StationConfig::default()
        .with_ssid(provisioning.ssid.as_str())
        .with_password(provisioning.password.as_str().into());
    let station = if provisioning.password.is_empty() {
        station.with_auth_method(AuthenticationMethod::None)
    } else {
        station
    };
    let wifi_config = WifiConfig::Station(station);
    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(wifi_config),
    )
    .expect("Wi-Fi station initialization");
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(Default::default()),
        make_static(StackResources::<8>::new()),
        seed,
    );
    spawner.spawn(station_connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(http_server(stack).unwrap());
    spawner.spawn(mdns_responder(stack, provisioning.hostname.clone()).unwrap());
    spawner.spawn(notification_worker(stack, provisioning.ntfy_topic.clone(), seed).unwrap());
    spawner.spawn(manifest_store(store, provisioning).unwrap());
}

fn start_provisioning(
    spawner: Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    store: Store<'static>,
    seed: u64,
    ntfy_topic: String,
) {
    let suffix = seed as u16;
    let ssid = format!("Servatory-{suffix:04X}");
    let wifi_config = WifiConfig::AccessPoint(
        AccessPointConfig::default()
            .with_ssid(ssid.as_str())
            .with_auth_method(AuthenticationMethod::None),
    );
    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(wifi_config),
    )
    .expect("Wi-Fi provisioning initialization");
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(PROVISIONING_IP, 24),
        gateway: Some(PROVISIONING_IP),
        dns_servers: Default::default(),
    });
    let (stack, runner) = embassy_net::new(
        interfaces.access_point,
        config,
        make_static(StackResources::<6>::new()),
        seed,
    );
    let mut display_ssid = heapless::String::new();
    display_ssid.push_str(&ssid).ok();
    critical_section::with(|section| {
        *PROVISIONING_DISPLAY.borrow(section).borrow_mut() =
            Some(ProvisioningDisplay { ssid: display_ssid });
    });
    spawner.spawn(access_point_connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(dhcp_server(stack).unwrap());
    spawner.spawn(provisioning_server(stack, store, ntfy_topic).unwrap());
}

fn make_static<T: 'static>(value: T) -> &'static mut T {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(value))
}

#[embassy_executor::task(pool_size = 2)]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await;
}

#[embassy_executor::task]
async fn station_connection(mut controller: WifiController<'static>) {
    loop {
        if controller.connect_async().await.is_ok() {
            let _ = controller.wait_for_disconnect_async().await;
        }
        Timer::after_secs(5).await;
    }
}

#[embassy_executor::task]
async fn access_point_connection(controller: WifiController<'static>) {
    loop {
        let _ = controller
            .wait_for_access_point_connected_event_async()
            .await;
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn dhcp_server(stack: Stack<'static>) {
    use core::net::SocketAddrV4;
    use edge_dhcp::{
        io::{self, DEFAULT_SERVER_PORT},
        server::{Server, ServerOptions},
    };
    use edge_nal::UdpBind;
    use edge_nal_embassy::{Udp, UdpBuffers};

    let mut packet = [0_u8; 1500];
    let mut gateways = [Ipv4Addr::UNSPECIFIED];
    let buffers = UdpBuffers::<3, 1024, 1024, 10>::new();
    let udp = Udp::new(stack, &buffers);
    let mut socket = udp
        .bind(core::net::SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DEFAULT_SERVER_PORT,
        )))
        .await
        .expect("DHCP socket");
    loop {
        let _ = io::server::run(
            &mut Server::<_, 64>::new_with_et(PROVISIONING_IP),
            &ServerOptions::new(PROVISIONING_IP, Some(&mut gateways)),
            &mut socket,
            &mut packet,
        )
        .await;
        Timer::after_millis(500).await;
    }
}

#[embassy_executor::task]
async fn provisioning_server(stack: Stack<'static>, mut store: Store<'static>, ntfy_topic: String) {
    stack.wait_config_up().await;
    let mut rx = [0_u8; 3072];
    let mut tx = [0_u8; 1536];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(15)));
    loop {
        if socket
            .accept(IpListenEndpoint {
                addr: None,
                port: 80,
            })
            .await
            .is_err()
        {
            continue;
        }
        let mut request = [0_u8; 3072];
        let len = read_request(&mut socket, &mut request).await;
        let request = core::str::from_utf8(&request[..len]).unwrap_or("");
        if request.starts_with("POST /configure ") {
            let body = request
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .unwrap_or("");
            if let Some(value) = parse_provisioning(body)
                && store.save_provisioning(value).is_ok()
            {
                write_response(
                    &mut socket,
                    "200 OK",
                    "text/html; charset=utf-8",
                    "<h1>Saved</h1><p>Servatory is restarting and will join your Wi-Fi network.</p>",
                )
                .await;
                Timer::after_secs(1).await;
                esp_hal::system::software_reset();
            }
            write_response(
                &mut socket,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                "Invalid Wi-Fi, hostname, or ntfy topic settings.",
            )
            .await;
        } else {
            let page = provisioning_page(&ntfy_topic);
            write_response(&mut socket, "200 OK", "text/html; charset=utf-8", &page).await;
        }
        close_socket(&mut socket).await;
    }
}

#[embassy_executor::task]
async fn manifest_store(mut store: Store<'static>, provisioning: Provisioning) {
    loop {
        let manifest = MANIFESTS.receive().await;
        let _ = store.save_network(&provisioning, manifest);
    }
}

fn provisioning_page(ntfy_topic: &str) -> String {
    format!(
        "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'>\
         <style>body{{font:16px system-ui;max-width:32rem;margin:2rem auto;padding:0 1rem;color:#172033}}\
         label{{display:block;margin:1rem 0}}.box{{width:100%;box-sizing:border-box;padding:.7rem}}\
         button{{padding:.8rem 1rem;background:#1457d9;color:white;border:0;border-radius:.4rem}}\
         .copy{{margin-bottom:1rem;background:#526174}}</style>\
         <h1>Servatory Wi-Fi setup</h1><form method=post action=/configure>\
         <label>Wi-Fi network<input class=box name=ssid maxlength=32 required></label>\
         <label>Wi-Fi password<input class=box name=password type=password maxlength=63></label>\
         <label>Device hostname<input class=box name=hostname value=servatory maxlength=63 required></label>\
         <label>ntfy topic<input id=topic class=box name=ntfy_topic value='{ntfy_topic}' maxlength=128 required></label>\
         <button class=copy type=button onclick=\"let t=document.getElementById('topic');t.select();document.execCommand('copy')\">Copy topic</button>\
         <p>This random topic is private. Copy it into the ntfy app, or replace it here.</p>\
         <button>Save and restart</button></form>"
    )
}

fn parse_provisioning(body: &str) -> Option<Provisioning> {
    let value = |name: &str| {
        body.split('&').find_map(|field| {
            let (key, value) = field.split_once('=')?;
            (key == name).then(|| form_decode(value))
        })
    };
    let provisioning = Provisioning {
        ssid: value("ssid")?,
        password: value("password").unwrap_or_default(),
        hostname: value("hostname")?,
        ntfy_topic: value("ntfy_topic")?,
    };
    provisioning.is_valid().then_some(provisioning)
}

fn form_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    decoded.push(high << 4 | low);
                    index += 2;
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).unwrap_or_default()
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[embassy_executor::task]
async fn http_server(stack: Stack<'static>) {
    stack.wait_config_up().await;
    let mut rx = [0_u8; 2048];
    let mut tx = [0_u8; 1536];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(15)));
    loop {
        let (enabled, port) = {
            let state = STATE.lock().await;
            state
                .manifest
                .as_ref()
                .map(|manifest| (manifest.http.enabled, manifest.http.port))
                .unwrap_or((false, 80))
        };
        if !enabled {
            Timer::after_secs(1).await;
            continue;
        }
        if socket
            .accept(IpListenEndpoint { addr: None, port })
            .await
            .is_err()
        {
            continue;
        }
        let mut request = [0_u8; 1024];
        let len = read_request(&mut socket, &mut request).await;
        let request = core::str::from_utf8(&request[..len]).unwrap_or("");
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        let state = STATE.lock().await.clone();
        match path {
            "/api/v1/notifications/test" if request.starts_with("POST ") => {
                let _ = TEST_NOTIFICATIONS.try_send(());
                write_response(
                    &mut socket,
                    "202 Accepted",
                    "text/plain; charset=utf-8",
                    "Test notification queued.\n",
                )
                .await;
            }
            "/api/v1/health" => {
                let body = health_json(&state);
                write_response(&mut socket, "200 OK", "application/json", &body).await;
            }
            "/api/v1/device" => {
                let age = snapshot_age(&state)
                    .map(|age| age.as_secs())
                    .unwrap_or(u64::MAX);
                let body = format!(
                    "{{\"firmware\":\"{}\",\"wifi\":\"connected\",\"snapshot_age_seconds\":{age}}}",
                    env!("SERVATORY_BUILD_VERSION")
                );
                write_response(&mut socket, "200 OK", "application/json", &body).await;
            }
            "/healthz" => {
                write_response(&mut socket, "200 OK", "text/plain", "ok\n").await;
            }
            _ => {
                let body = dashboard_html(&state);
                write_response(&mut socket, "200 OK", "text/html; charset=utf-8", &body).await;
            }
        }
        close_socket(&mut socket).await;
    }
}

#[embassy_executor::task]
async fn mdns_responder(stack: Stack<'static>, hostname: String) {
    use core::net::{IpAddr, SocketAddr};
    use embassy_net::udp::{PacketMetadata, UdpSocket};

    const MDNS: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
    stack.wait_config_up().await;
    let _ = stack.join_multicast_group(MDNS);
    let rx_meta = make_static([PacketMetadata::EMPTY; 2]);
    let tx_meta = make_static([PacketMetadata::EMPTY; 2]);
    let rx_buffer = make_static([0_u8; 768]);
    let tx_buffer = make_static([0_u8; 768]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer);
    if socket.bind(5353).is_err() {
        return;
    }
    let name = dns_name(&format!("{}.local", hostname));
    let mut request = [0_u8; 768];
    loop {
        let Ok((len, _)) = socket.recv_from(&mut request).await else {
            continue;
        };
        if !request[..len]
            .windows(name.len())
            .any(|window| window == name)
        {
            continue;
        }
        let Some(address) = stack.config_v4().map(|config| config.address.address()) else {
            continue;
        };
        let mut response = Vec::with_capacity(64 + name.len());
        response.extend_from_slice(&[0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        response.extend_from_slice(&name);
        response.extend_from_slice(&[0, 1, 0, 1]);
        response.extend_from_slice(&120_u32.to_be_bytes());
        response.extend_from_slice(&[0, 4]);
        response.extend_from_slice(&address.octets());
        let _ = socket
            .send_to(&response, SocketAddr::new(IpAddr::V4(MDNS), 5353))
            .await;
    }
}

fn dns_name(value: &str) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len() + 2);
    for label in value.split('.') {
        encoded.push(u8::try_from(label.len()).unwrap_or(0));
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    encoded
}

async fn read_request(socket: &mut TcpSocket<'_>, buffer: &mut [u8]) -> usize {
    let mut len = 0;
    while len < buffer.len() {
        match socket.read(&mut buffer[len..]).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                len += read;
                if let Ok(text) = core::str::from_utf8(&buffer[..len])
                    && let Some(header_end) = text.find("\r\n\r\n")
                {
                    let content_length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if len >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
        }
    }
    len
}

async fn write_response(socket: &mut TcpSocket<'_>, status: &str, content_type: &str, body: &str) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(body.as_bytes()).await;
    let _ = socket.flush().await;
}

async fn close_socket(socket: &mut TcpSocket<'_>) {
    socket.close();
    Timer::after_millis(100).await;
    socket.abort();
}

fn snapshot_age(state: &SharedState) -> Option<Duration> {
    state.last_update.map(|updated| Instant::now() - updated)
}

fn active_incidents(state: &SharedState) -> Vec<Incident> {
    let mut incidents = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_incidents().to_vec())
        .unwrap_or_default();
    if snapshot_age(state).is_none_or(|age| age >= HOST_STALE_AFTER) {
        incidents.insert(
            0,
            Incident::new(
                IncidentId::Stick(StickIncident::HostOffline),
                HealthLevel::Critical,
                "HOST CONNECTION LOST",
            ),
        );
    }
    incidents
}

fn dashboard_html(state: &SharedState) -> String {
    let mut html = String::from(
        "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'>\
         <meta http-equiv=refresh content=5><style>:root{color-scheme:light dark}\
         body{font:16px system-ui;margin:auto;max-width:70rem;padding:1rem;background:#10141d;color:#edf2ff}\
         header,.card{background:#1b2230;border-radius:.65rem;padding:1rem;margin:.75rem 0}\
         .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(18rem,1fr));gap:.75rem}\
         .grid .card{margin:0}.ok{color:#63d69f}.warn{color:#ffd166}.crit{color:#ff6b6b}\
         table{width:100%;border-collapse:collapse}td{padding:.3rem;border-bottom:1px solid #394257}\
         button{padding:.55rem .8rem;border:0;border-radius:.4rem}small{color:#aab4c8}@media(prefers-color-scheme:light){body{background:#eef2f8;color:#172033}\
         header,.card{background:white}small{color:#586174}td{border-color:#d8deea}}</style>",
    );
    let incidents = active_incidents(state);
    let age = snapshot_age(state).map(|age| age.as_secs());
    let stale = age.is_none_or(|age| age >= HOST_STALE_AFTER.as_secs());
    let level = incidents
        .iter()
        .map(|incident| incident.level)
        .max_by_key(|level| level.priority())
        .unwrap_or(HealthLevel::Healthy);
    let _ = write!(
        html,
        "<header><small>SERVATORY · {}</small><h1 class={}>{}</h1><p>{}</p>\
         <form method=post action=/api/v1/notifications/test><button>Send test notification</button></form></header>",
        if stale { "STALE" } else { "LIVE" },
        level_class(level),
        level_text(level),
        age.map_or_else(
            || "No host snapshot received".to_string(),
            |age| format!("Updated {age} seconds ago")
        )
    );
    if !incidents.is_empty() {
        html.push_str("<section class=card><h2>Active incidents</h2><ul>");
        for incident in &incidents {
            let _ = write!(html, "<li class={}>", level_class(incident.level));
            push_html(&mut html, incident.message());
            html.push_str("</li>");
        }
        html.push_str("</ul></section>");
    }
    let Some(snapshot) = state.snapshot.as_ref() else {
        return html;
    };
    html.push_str("<main class=grid>");
    let pages = state
        .manifest
        .as_ref()
        .map(|manifest| manifest.http.pages())
        .unwrap_or(&[]);
    let mut shown = [false; 5];
    for view in pages {
        let kind = match view.page {
            DisplayPage::Overview => 0,
            DisplayPage::Resources => 1,
            DisplayPage::Storage { .. } => 2,
            DisplayPage::PowerNetwork { .. } => 3,
            DisplayPage::Guests { .. } => 4,
        };
        if shown[kind] {
            continue;
        }
        shown[kind] = true;
        html.push_str("<section class=card><h2>");
        push_html(&mut html, view.title.as_str());
        html.push_str("</h2>");
        render_page(&mut html, &view.page, snapshot, pages);
        html.push_str("</section>");
    }
    html.push_str("</main>");
    html
}

fn render_page(
    html: &mut String,
    page: &DisplayPage,
    snapshot: &HealthSnapshot,
    pages: &[servatory_protocol::DisplayView],
) {
    match page {
        DisplayPage::Overview => {
            let _ = write!(html, "<p><b>{}</b><br>Host: ", snapshot.health.message());
            push_html(html, snapshot.host_name());
            let running = snapshot
                .guests
                .guests()
                .iter()
                .filter(|guest| matches!(guest.status, servatory_protocol::GuestStatus::Running))
                .count();
            let _ = write!(
                html,
                "<br>Uptime: {} days<br>Network: ",
                snapshot.uptime_seconds / 86_400
            );
            push_html(html, snapshot.network_interface());
            let _ = write!(
                html,
                " · {} Mbps<br>Guests: {running} running · {} total</p>",
                snapshot.network_mbps,
                snapshot.guests.guests().len()
            );
        }
        DisplayPage::Resources => {
            let memory = if snapshot.memory_total_mib == 0 {
                0
            } else {
                u64::from(snapshot.memory_used_mib) * 100 / u64::from(snapshot.memory_total_mib)
            };
            let _ = write!(
                html,
                "<table><tr><td>CPU<td>{}%<tr><td>Memory<td>{} / {} MiB ({memory}%)<tr><td>I/O pressure<td>{}%<tr><td>Load average<td>{:.2}</table>",
                snapshot.cpu_percent,
                snapshot.memory_used_mib,
                snapshot.memory_total_mib,
                snapshot.io_pressure_percent,
                f32::from(snapshot.load_average_x100) / 100.0
            );
        }
        DisplayPage::Storage { .. } => {
            html.push_str("<table>");
            for (index, usage) in snapshot.filesystems.iter().enumerate() {
                let _ = write!(
                    html,
                    "<tr><td>Filesystem {}<td>{}% · {} MiB available{}",
                    index + 1,
                    usage.used_percent,
                    usage.available_mib,
                    if usage.mounted { "" } else { " · MISSING" }
                );
            }
            for device in snapshot.smart.devices() {
                html.push_str("<tr><td>");
                push_html(html, device.label());
                let _ = write!(
                    html,
                    " SMART<td>{:?}{}",
                    device.status,
                    device
                        .temperature_celsius
                        .map_or_else(String::new, |temperature| format!(" · {temperature}°C"))
                );
            }
            let _ = write!(
                html,
                "<tr><td>Backup<td>{:?}</table>",
                snapshot.backup_job_status
            );
        }
        DisplayPage::PowerNetwork { .. } => {
            let _ = write!(
                html,
                "<table><tr><td>UPS<td>{:?}<tr><td>Battery<td>{}<tr><td>Load<td>{}<tr><td>Ethernet<td>{} · {} Mbps<tr><td>Internet<td>{:?}<tr><td>IPv4<td>{}.{}.{}.{}</table>",
                snapshot.ups.status,
                optional_percent(snapshot.ups.battery_percent),
                optional_percent(snapshot.ups.load_percent),
                if snapshot.network_up { "up" } else { "down" },
                snapshot.network_mbps,
                snapshot.internet_status,
                snapshot.ipv4[0],
                snapshot.ipv4[1],
                snapshot.ipv4[2],
                snapshot.ipv4[3]
            );
        }
        DisplayPage::Guests { .. } => {
            html.push_str("<table>");
            for guest in snapshot.guests.guests() {
                let _ = write!(html, "<tr><td>{}<td>", guest.vmid);
                push_html(html, guest.name());
                let _ = write!(
                    html,
                    "<td>{:?}<td>{}% CPU · {} / {} MiB",
                    guest.status, guest.cpu_percent, guest.memory_used_mib, guest.memory_total_mib
                );
            }
            html.push_str("</table>");
        }
    }
    let _ = pages;
}

fn optional_percent(value: Option<u8>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{value}%"))
}

fn level_class(level: HealthLevel) -> &'static str {
    match level {
        HealthLevel::Healthy => "ok",
        HealthLevel::Warning => "warn",
        HealthLevel::Critical => "crit",
    }
}

fn level_text(level: HealthLevel) -> &'static str {
    match level {
        HealthLevel::Healthy => "HEALTHY",
        HealthLevel::Warning => "WARNING",
        HealthLevel::Critical => "CRITICAL",
    }
}

fn push_html(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(character),
        }
    }
}

fn health_json(state: &SharedState) -> String {
    let stale = snapshot_age(state).is_none_or(|age| age >= HOST_STALE_AFTER);
    let incidents = active_incidents(state);
    let mut json = format!("{{\"stale\":{stale},\"incidents\":[");
    for (index, incident) in incidents.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(
            json,
            "{{\"level\":\"{}\",\"message\":\"",
            level_text(incident.level).to_ascii_lowercase()
        );
        push_json(&mut json, incident.message());
        json.push_str("\"}");
    }
    json.push(']');
    if let Some(snapshot) = &state.snapshot {
        json.push_str(",\"host\":\"");
        push_json(&mut json, snapshot.host_name());
        let _ = write!(
            json,
            "\",\"cpu_percent\":{},\"memory_used_mib\":{},\"memory_total_mib\":{},\"io_pressure_percent\":{},\"load_average_x100\":{},\"network_up\":{},\"network_mbps\":{},\"ipv4\":[{},{},{},{}],\"guest_count\":{}",
            snapshot.cpu_percent,
            snapshot.memory_used_mib,
            snapshot.memory_total_mib,
            snapshot.io_pressure_percent,
            snapshot.load_average_x100,
            snapshot.network_up,
            snapshot.network_mbps,
            snapshot.ipv4[0],
            snapshot.ipv4[1],
            snapshot.ipv4[2],
            snapshot.ipv4[3],
            snapshot.guests.guests().len()
        );
    }
    json.push('}');
    json
}

fn push_json(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {}
            _ => output.push(character),
        }
    }
}

#[derive(Clone)]
struct PendingNotification {
    message: String,
    priority: NotificationPriority,
    recovery: bool,
}

#[embassy_executor::task]
async fn notification_worker(stack: Stack<'static>, topic: String, mut seed: u64) {
    stack.wait_config_up().await;
    let tcp_state = make_static(TcpClientState::<1, 4096, 4096>::new());
    let tcp = TcpClient::new(stack, tcp_state);
    let dns = DnsSocket::new(stack);
    let mut previous: Vec<Incident> = Vec::new();
    let mut pending = VecDeque::new();
    let mut next_critical_repeat = None;
    loop {
        let state = STATE.lock().await.clone();
        let current = active_incidents(&state);
        if let Some(config) = state.manifest.as_ref().map(|manifest| &manifest.ntfy)
            && config.enabled
        {
            if TEST_NOTIFICATIONS.try_receive().is_ok() {
                push_pending(
                    &mut pending,
                    PendingNotification {
                        message: "Servatory test notification".to_string(),
                        priority: NotificationPriority::Default,
                        recovery: false,
                    },
                );
            }
            for incident in &current {
                if previous
                    .iter()
                    .find(|old| old.id == incident.id)
                    .is_none_or(|old| old.level.priority() < incident.level.priority())
                    && ((incident.level == HealthLevel::Warning && config.severities.warning)
                        || (incident.level == HealthLevel::Critical && config.severities.critical))
                {
                    push_pending(
                        &mut pending,
                        PendingNotification {
                            message: incident.message().to_string(),
                            priority: if incident.level == HealthLevel::Critical {
                                config.critical_priority
                            } else {
                                config.warning_priority
                            },
                            recovery: false,
                        },
                    );
                }
            }
            let criticals: Vec<_> = current
                .iter()
                .filter(|incident| incident.level == HealthLevel::Critical)
                .collect();
            if let Some(seconds) = config.repeat_critical_seconds
                && !criticals.is_empty()
            {
                let due = next_critical_repeat.get_or_insert_with(|| {
                    Instant::now() + Duration::from_secs(u64::from(seconds))
                });
                if Instant::now() >= *due {
                    for incident in criticals {
                        push_pending(
                            &mut pending,
                            PendingNotification {
                                message: format!("STILL ACTIVE: {}", incident.message()),
                                priority: config.critical_priority,
                                recovery: false,
                            },
                        );
                    }
                    *due = Instant::now() + Duration::from_secs(u64::from(seconds));
                }
            } else {
                next_critical_repeat = None;
            }
            if config.notify_recovery {
                for incident in &previous {
                    if !current.iter().any(|new| new.id == incident.id) {
                        push_pending(
                            &mut pending,
                            PendingNotification {
                                message: format!("RECOVERED: {}", incident.message()),
                                priority: config.recovery_priority,
                                recovery: true,
                            },
                        );
                    }
                }
            }
            if let Some(notification) = pending.front().cloned() {
                seed = seed.wrapping_add(1);
                if publish_notification(&tcp, &dns, config, &topic, &notification, seed).await {
                    pending.pop_front();
                }
            }
        }
        previous = current;
        Timer::after_secs(5).await;
    }
}

fn push_pending(queue: &mut VecDeque<PendingNotification>, value: PendingNotification) {
    if queue.len() == MAX_PENDING_NOTIFICATIONS {
        queue.pop_front();
    }
    queue.push_back(value);
}

async fn publish_notification(
    tcp: &TcpClient<'_, 1, 4096, 4096>,
    dns: &DnsSocket<'_>,
    config: &servatory_protocol::NtfyConfig,
    topic: &str,
    notification: &PendingNotification,
    seed: u64,
) -> bool {
    let url = format!("{}/{}", config.server().trim_end_matches('/'), topic);
    let priority = match notification.priority {
        NotificationPriority::Default => "default",
        NotificationPriority::High => "high",
        NotificationPriority::Urgent => "urgent",
    };
    let title = if notification.recovery {
        "Servatory recovery"
    } else {
        "Servatory incident"
    };
    let mut tls_rx = [0_u8; 16_384];
    let mut tls_tx = [0_u8; 4_096];
    let ca = match STANDARD.decode(ISRG_ROOT_X1) {
        Ok(ca) => ca,
        Err(_) => return false,
    };
    let tls = TlsConfig::new(
        seed,
        &mut tls_rx,
        &mut tls_tx,
        TlsVerify::Certificate {
            ca: &ca,
            cert: None,
            key: None,
        },
    );
    let mut client = HttpClient::new_with_tls(tcp, dns, tls);
    let headers = [
        ("Title", title),
        ("Priority", priority),
        (
            "Tags",
            if notification.recovery {
                "white_check_mark"
            } else {
                "warning"
            },
        ),
        ("Click", config.click_url().unwrap_or("")),
    ];
    let Ok(request) = client.request(Method::POST, &url).await else {
        return false;
    };
    let mut request = request
        .headers(&headers)
        .body(notification.message.as_bytes());
    let mut response = [0_u8; 1024];
    request
        .send(&mut response)
        .await
        .is_ok_and(|response| response.status.is_successful())
}
