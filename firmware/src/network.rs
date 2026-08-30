use alloc::{
    collections::VecDeque,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    cell::RefCell,
    fmt::{Debug, Display, Write as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use critical_section::Mutex as CriticalMutex;
use edge_http::{Method as HttpMethod, io::server::Connection as HttpConnection};
use edge_mdns::{HostAnswersMdnsHandler, host::Host};
use edge_nal::{TcpBind, UdpSplit, WithTimeout};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
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
use embedded_io_async::{Read, Write};
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
    BackupJobStatus, DisplayLabel, DisplayPage, GuestKind, GuestStatus, HealthLevel,
    HealthSnapshot, Incident, IncidentId, InternetStatus, NetworkConfig, NotificationPriority,
    PROTOCOL_VERSION, SmartStatus, SoftwareVersion, StickIncident, UpsStatus,
};
use static_cell::StaticCell;

use crate::{
    memory::{INTERNAL_HEAP_BYTES, PsramBox, zeroed_psram},
    provisioning::{Provisioning, Store, StoredSettings},
};

const PROVISIONING_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const HOST_STALE_AFTER: Duration = Duration::from_secs(15);
const MAX_PENDING_NOTIFICATIONS: usize = 8;
const HTTP_WORKERS: usize = 2;
const HTTP_IO_TIMEOUT_MS: u32 = 2_000;
const HTTP_BUFFER_SIZE: usize = 1_536;
const HTTP_SOCKET_BUFFER_SIZE: usize = 1_024;
const MDNS_BUFFER_SIZE: usize = 768;
const NOTIFICATION_TLS_RX_SIZE: usize = 16_384;
const NOTIFICATION_TLS_TX_SIZE: usize = 4_096;
const NOTIFICATION_RESPONSE_SIZE: usize = 1_024;
const DASHBOARD_SCRIPT: &str = include_str!("dashboard.js");
const DASHBOARD_STYLE: &str = include_str!("dashboard.css");
const DASHBOARD_BODY_CAPACITY: usize = 16 * 1024;
const DASHBOARD_DYNAMIC_RESPONSES: usize = 1;
const MIN_NON_DASHBOARD_HEAP: usize = 80 * 1024;
const _: () = assert!(
    DASHBOARD_BODY_CAPACITY * DASHBOARD_DYNAMIC_RESPONSES + MIN_NON_DASHBOARD_HEAP
        <= INTERNAL_HEAP_BYTES
);
const ISRG_ROOT_X1: &str = concat!(
    "MIIFazCCA1OgAwIBAgIRAIIQz7DSQONZRGPgu2OCiwAwDQYJKoZIhvcNAQELBQAw",
    "TzELMAkGA1UEBhMCVVMxKTAnBgNVBAoTIEludGVybmV0IFNlY3VyaXR5IFJlc2Vh",
    "cmNoIEdyb3VwMRUwEwYDVQQDEwxJU1JHIFJvb3QgWDEwHhcNMTUwNjA0MTEwNDM4",
    "WhcNMzUwNjA0MTEwNDM4WjBPMQswCQYDVQQGEwJVUzEpMCcGA1UEChMgSW50ZXJu",
    "ZXQgU2VjdXJpdHkgUmVzZWFyY2ggR3JvdXAxFTATBgNVBAMTDElTUkcgUm9vdCBY",
    "MTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBAK3oJHP0FDfzm54rVygc",
    "h77ct984kIxuPOZXoHj3dcKi/vVqbvYATyjb3miGbESTtrFj/RQSa78f0uoxmyF+",
    "0TM8ukj13Xnfs7j/EvEhmkvBioZxaUpmZmyPfjxwv60pIgbz5MDmgK7iS4+3mX6U",
    "A5/TR5d8mUgjU+g4rk8Kb4Mu0UlXjIB0ttov0DiNewNwIRt18jA8+o+u3dpjq+sW",
    "T8KOEUt+zwvo/7V3LvSye0rgTBIlDHCNAymg4VMk7BPZ7hm/ELNKjD+Jo2FR3qyH",
    "B5T0Y3HsLuJvW5iB4YlcNHlsdu87kGJ55tukmi8mxdAQ4Q7e2RCOFvu396j3x+UC",
    "B5iPNgiV5+I3lg02dZ77DnKxHZu8A/lJBdiB3QW0KtZB6awBdpUKD9jf1b0SHzUv",
    "KBds0pjBqAlkd25HN7rOrFleaJ1/ctaJxQZBKT5ZPt0m9STJEadao0xAH0ahmbWn",
    "OlFuhjuefXKnEgV4We0+UXgVCwOPjdAvBbI+e0ocS3MFEvzG6uBQE3xDk3SzynTn",
    "jh8BCNAw1FtxNrQHusEwMFxIt4I7mKZ9YIqioymCzLq9gwQbooMDQaHWBfEbwrbw",
    "qHyGO0aoSCqI3Haadr8faqU9GY/rOPNk3sgrDQoo//fb4hVC1CLQJ13hef4Y53CI",
    "rU7m2Ys6xt0nUW7/vGT1M0NPAgMBAAGjQjBAMA4GA1UdDwEB/wQEAwIBBjAPBgNV",
    "HRMBAf8EBTADAQH/MB0GA1UdDgQWBBR5tFnme7bl5AFzgAiIyBpY9umbbjANBgkq",
    "hkiG9w0BAQsFAAOCAgEAVR9YqbyyqFDQDLHYGmkgJykIrGF1XIpu+ILlaS/V9lZL",
    "ubhzEFnTIZd+50xx+7LSYK05qAvqFyFWhfFQDlnrzuBZ6brJFe+GnY+EgPbk6ZGQ",
    "3BebYhtF8GaV0nxvwuo77x/Py9auJ/GpsMiu/X1+mvoiBOv/2X/qkSsisRcOj/KK",
    "NFtY2PwByVS5uCbMiogziUwthDyC3+6WVwW6LLv3xLfHTjuCvjHIInNzktHCgKQ5",
    "ORAzI4JMPJ+GslWYHb4phowim57iaztXOoJwTdwJx4nLCgdNbOhdjsnvzqvHu7Ur",
    "TkXWStAmzOVyyghqpZXjFaH3pO3JLF+l+/+sKAIuvtd7u+Nxe5AW0wdeRlN8NwdC",
    "jNPElpzVmbUq4JUagEiuTDkHzsxHpFKVK7q4+63SM1N95R1NbdWhscdCb+ZAJzVc",
    "oyi3B43njTOQ5yOf+1CceWxG1bQVs5ZufpsMljq4Ui0/1lvh+wjChP4kqKOJ2qxq",
    "4RgqsahDYVvTH9w7jXbyLeiNdd8XM2w9U/t7y0Ff/9yi0GE44Za4rF2LN9d11TPA",
    "mRGunUHBcnWEvgJBQl9nJEiU0Zsnvgc/ubhPgXRR4Xq37Z0j4r7g1SgEEzwxA57d",
    "emyPxgcYxn/eR44/KJ4EBs+lVDR3veyJm+kXQ99b21/+jh5Xos1AnX5iItreGCc=",
);

#[derive(Clone)]
struct SharedState {
    snapshot: Option<Arc<HealthSnapshot>>,
    manifest: Option<Arc<NetworkConfig>>,
    filesystem_labels: Vec<DisplayLabel>,
    last_update: Option<Instant>,
    station_ipv4: Option<Ipv4Addr>,
    ntfy_topic: Option<String>,
    daemon_version: Option<SoftwareVersion>,
}

impl SharedState {
    const fn new() -> Self {
        Self {
            snapshot: None,
            manifest: None,
            filesystem_labels: Vec::new(),
            last_update: None,
            station_ipv4: None,
            ntfy_topic: None,
            daemon_version: None,
        }
    }
}

static STATE: Mutex<CriticalSectionRawMutex, SharedState> = Mutex::new(SharedState::new());
static DASHBOARD_RESPONSE: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());
static MANIFESTS: Channel<CriticalSectionRawMutex, NetworkConfig, 1> = Channel::new();
static TOPIC_UPDATES: Channel<CriticalSectionRawMutex, String, 1> = Channel::new();
static TEST_NOTIFICATIONS: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static STATION_IPV4: CriticalMutex<RefCell<Option<Ipv4Addr>>> =
    CriticalMutex::new(RefCell::new(None));
static WEB_SERVER_PORT: CriticalMutex<RefCell<Option<u16>>> =
    CriticalMutex::new(RefCell::new(None));
static STATION_STACK_RESOURCES: StaticCell<StackResources<10>> = StaticCell::new();
static PROVISIONING_STACK_RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();
static HTTP_TCP_BUFFERS: StaticCell<
    edge_nal_embassy::TcpBuffers<HTTP_WORKERS, HTTP_SOCKET_BUFFER_SIZE, HTTP_SOCKET_BUFFER_SIZE>,
> = StaticCell::new();
static HTTP_SERVER: StaticCell<edge_http::io::server::Server<HTTP_WORKERS, HTTP_BUFFER_SIZE, 16>> =
    StaticCell::new();
static MDNS_UDP_BUFFERS: StaticCell<
    edge_nal_embassy::UdpBuffers<1, MDNS_BUFFER_SIZE, MDNS_BUFFER_SIZE, 2>,
> = StaticCell::new();
static MDNS_RECV_BUFFER: StaticCell<
    edge_mdns::buf::VecBufAccess<CriticalSectionRawMutex, MDNS_BUFFER_SIZE>,
> = StaticCell::new();
static MDNS_SEND_BUFFER: StaticCell<
    edge_mdns::buf::VecBufAccess<CriticalSectionRawMutex, MDNS_BUFFER_SIZE>,
> = StaticCell::new();
static MDNS_BROADCAST_SIGNAL: StaticCell<
    embassy_sync::signal::Signal<CriticalSectionRawMutex, ()>,
> = StaticCell::new();

#[derive(Clone)]
pub struct ProvisioningDisplay {
    pub ssid: heapless::String<32>,
}

static PROVISIONING_DISPLAY: CriticalMutex<RefCell<Option<ProvisioningDisplay>>> =
    CriticalMutex::new(RefCell::new(None));

struct DashboardBuffer {
    inner: String,
    overflowed: bool,
}

impl DashboardBuffer {
    fn new() -> Self {
        Self {
            inner: String::with_capacity(DASHBOARD_BODY_CAPACITY),
            overflowed: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        if self
            .inner
            .len()
            .checked_add(value.len())
            .is_some_and(|length| length <= DASHBOARD_BODY_CAPACITY)
        {
            self.inner.push_str(value);
        } else {
            self.overflowed = true;
        }
    }

    fn push(&mut self, value: char) {
        let mut bytes = [0_u8; 4];
        self.push_str(value.encode_utf8(&mut bytes));
    }

    fn finish(self, document: bool) -> String {
        if !self.overflowed {
            return self.inner;
        }
        drop(self.inner);
        if document {
            "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'><title>Servatory</title><p>Dashboard data exceeds the firmware response budget.</p>".to_string()
        } else {
            "<div class=shell><p>Dashboard data exceeds the firmware response budget.</p></div>"
                .to_string()
        }
    }
}

impl core::fmt::Write for DashboardBuffer {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.push_str(value);
        if self.overflowed {
            Err(core::fmt::Error)
        } else {
            Ok(())
        }
    }
}

pub fn provisioning_display() -> Option<ProvisioningDisplay> {
    critical_section::with(|section| PROVISIONING_DISPLAY.borrow(section).borrow().clone())
}

pub fn station_ipv4() -> Option<Ipv4Addr> {
    critical_section::with(|section| *STATION_IPV4.borrow(section).borrow())
}

pub fn web_server_port() -> Option<u16> {
    critical_section::with(|section| *WEB_SERVER_PORT.borrow(section).borrow())
}

fn set_web_server(enabled: bool, port: u16) {
    critical_section::with(|section| {
        *WEB_SERVER_PORT.borrow(section).borrow_mut() = enabled.then_some(port);
    });
}

pub async fn update_daemon_version(version: SoftwareVersion) {
    STATE.lock().await.daemon_version = Some(version);
}

pub async fn update_snapshot(snapshot: Arc<HealthSnapshot>) {
    let mut state = STATE.lock().await;
    state.snapshot = Some(snapshot);
    state.last_update = Some(Instant::now());
}

pub async fn update_filesystem_labels(labels: Vec<DisplayLabel>) {
    STATE.lock().await.filesystem_labels = labels;
}

pub async fn update_manifest(manifest: NetworkConfig) {
    set_web_server(manifest.http.enabled, manifest.http.port);
    let mut state = STATE.lock().await;
    if state.manifest.as_deref() == Some(&manifest) {
        return;
    }
    state.manifest = Some(Arc::new(manifest.clone()));
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
        if let Some(manifest) = settings.network.as_ref() {
            set_web_server(manifest.http.enabled, manifest.http.port);
        }
        {
            let mut state = STATE.lock().await;
            state.manifest = settings.network.clone().map(Arc::new);
            state.ntfy_topic = Some(settings.provisioning.ntfy_topic.clone());
        }
        start_station(spawner, wifi, store, settings, seed);
    } else {
        let ntfy_topic = random_ntfy_topic();
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
    let stored_network = settings.network;
    let provisioning = settings.provisioning;
    let mut dhcp_hostname = heapless08::String::new();
    dhcp_hostname
        .push_str(&provisioning.hostname)
        .expect("validated hostname fits DHCP option 12");
    let mut dhcp = embassy_net::DhcpConfig::default();
    dhcp.hostname = Some(dhcp_hostname);
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
        embassy_net::Config::dhcpv4(dhcp),
        STATION_STACK_RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(station_connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(address_tracker(stack).unwrap());
    spawner.spawn(http_server(stack).unwrap());
    spawner.spawn(mdns_responder(stack, provisioning.hostname.clone()).unwrap());
    spawner.spawn(notification_worker(stack, seed).unwrap());
    spawner.spawn(manifest_store(store, provisioning, stored_network).unwrap());
}

fn random_ntfy_topic() -> String {
    let rng = Rng::new();
    format!(
        "servatory-{:08x}{:08x}{:08x}{:08x}",
        rng.random(),
        rng.random(),
        rng.random(),
        rng.random()
    )
}

#[embassy_executor::task]
async fn address_tracker(stack: Stack<'static>) {
    loop {
        stack.wait_config_up().await;
        let address = stack.config_v4().map(|config| config.address.address());
        STATE.lock().await.station_ipv4 = address;
        critical_section::with(|section| {
            *STATION_IPV4.borrow(section).borrow_mut() = address;
        });
        stack.wait_config_down().await;
        STATE.lock().await.station_ipv4 = None;
        critical_section::with(|section| {
            *STATION_IPV4.borrow(section).borrow_mut() = None;
        });
    }
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
        PROVISIONING_STACK_RESOURCES.init(StackResources::new()),
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
async fn manifest_store(
    mut store: Store<'static>,
    mut provisioning: Provisioning,
    mut network: Option<NetworkConfig>,
) {
    loop {
        match select(MANIFESTS.receive(), TOPIC_UPDATES.receive()).await {
            Either::First(manifest) => network = Some(manifest),
            Either::Second(topic) => provisioning.ntfy_topic = topic,
        }
        if let Some(manifest) = network.clone() {
            let _ = store.save_network(&provisioning, manifest);
        } else {
            let _ = store.save_provisioning(provisioning.clone());
        }
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
         <label>Device hostname<input class=box name=hostname value=servatory maxlength=32 required></label>\
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
    let tcp_buffers = HTTP_TCP_BUFFERS.init(edge_nal_embassy::TcpBuffers::new());
    let tcp = edge_nal_embassy::Tcp::new(stack, tcp_buffers);
    let server = HTTP_SERVER.init(edge_http::io::server::Server::new());
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
        let Ok(acceptor) = tcp
            .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
            .await
        else {
            Timer::after_secs(1).await;
            continue;
        };
        let _ = server
            .run(
                Some(HTTP_IO_TIMEOUT_MS),
                WithTimeout::new(HTTP_IO_TIMEOUT_MS, acceptor),
                HttpHandler,
            )
            .await;
        Timer::after_secs(1).await;
    }
}

struct HttpHandler;

impl edge_http::io::server::Handler for HttpHandler {
    type Error<E>
        = edge_http::io::Error<E>
    where
        E: Debug;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl Display + Copy,
        connection: &mut HttpConnection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + edge_nal::TcpSplit,
    {
        let (method, path, regenerate) = {
            let headers = connection.headers()?;
            (
                headers.method,
                headers.path,
                headers
                    .headers
                    .get("X-Servatory-Action")
                    .is_some_and(|value| value.eq_ignore_ascii_case("regenerate")),
            )
        };

        if method == HttpMethod::Post
            && path == "/api/v1/notifications/topic/regenerate"
            && regenerate
        {
            let topic = random_ntfy_topic();
            STATE.lock().await.ntfy_topic = Some(topic.clone());
            if let Err(embassy_sync::channel::TrySendError::Full(topic)) =
                TOPIC_UPDATES.try_send(topic)
            {
                let _ = TOPIC_UPDATES.try_receive();
                let _ = TOPIC_UPDATES.try_send(topic);
            }
            return write_http_response(
                connection,
                200,
                "OK",
                "text/html; charset=utf-8",
                "<!doctype html><meta http-equiv=refresh content='0;url=/'><p>New ntfy topic generated and saved.</p>",
            )
            .await;
        }

        if method == HttpMethod::Post && path == "/api/v1/notifications/test" {
            let _ = TEST_NOTIFICATIONS.try_send(());
            return write_http_response(
                connection,
                202,
                "Accepted",
                "text/plain; charset=utf-8",
                "Test notification queued.\n",
            )
            .await;
        }

        if method != HttpMethod::Get {
            return write_http_response(
                connection,
                405,
                "Method Not Allowed",
                "text/plain; charset=utf-8",
                "Method not allowed.\n",
            )
            .await;
        }

        match path {
            "/api/v1/health" => {
                let body = {
                    let state = STATE.lock().await;
                    health_json(&state)
                };
                write_http_response(connection, 200, "OK", "application/json", &body).await?;
            }
            "/api/v1/device" => {
                let body = {
                    let state = STATE.lock().await;
                    let age = snapshot_age(&state)
                        .map(|age| age.as_secs())
                        .unwrap_or(u64::MAX);
                    let address = state
                        .station_ipv4
                        .map_or_else(|| "0.0.0.0".to_string(), |address| address.to_string());
                    let daemon = state
                        .daemon_version
                        .as_ref()
                        .map_or("unknown", SoftwareVersion::as_str);
                    format!(
                        "{{\"firmware\":\"{}\",\"daemon\":\"{daemon}\",\"protocol\":{PROTOCOL_VERSION},\"wifi\":\"connected\",\"ipv4\":\"{address}\",\"snapshot_age_seconds\":{age}}}",
                        env!("SERVATORY_BUILD_VERSION"),
                    )
                };
                write_http_response(connection, 200, "OK", "application/json", &body).await?;
            }
            "/api/v1/dashboard-fragment" => {
                let _response = DASHBOARD_RESPONSE.lock().await;
                let state = STATE.lock().await.clone();
                let body = dashboard_fragment(&state);
                write_http_response(connection, 200, "OK", "text/html; charset=utf-8", &body)
                    .await?;
            }
            "/dashboard.css" => {
                write_http_response(
                    connection,
                    200,
                    "OK",
                    "text/css; charset=utf-8",
                    DASHBOARD_STYLE,
                )
                .await?;
            }
            "/dashboard.js" => {
                write_http_response(
                    connection,
                    200,
                    "OK",
                    "text/javascript; charset=utf-8",
                    DASHBOARD_SCRIPT,
                )
                .await?;
            }
            "/healthz" => {
                write_http_response(connection, 200, "OK", "text/plain", "ok\n").await?;
            }
            _ => {
                let _response = DASHBOARD_RESPONSE.lock().await;
                let state = STATE.lock().await.clone();
                let body = dashboard_html(&state);
                write_http_response(connection, 200, "OK", "text/html; charset=utf-8", &body)
                    .await?;
            }
        }
        Ok(())
    }
}

async fn write_http_response<T, const N: usize>(
    connection: &mut HttpConnection<'_, T, N>,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
) -> Result<(), edge_http::io::Error<T::Error>>
where
    T: Read + Write,
{
    connection
        .initiate_response(
            status,
            Some(reason),
            &[
                ("Content-Type", content_type),
                ("Cache-Control", "no-store"),
                ("Connection", "Close"),
            ],
        )
        .await?;
    connection.write_all(body.as_bytes()).await?;
    Ok(())
}

#[embassy_executor::task]
async fn mdns_responder(stack: Stack<'static>, hostname: String) {
    use edge_mdns::{buf::VecBufAccess, domain::base::Ttl, io};
    use edge_nal_embassy::{Udp, UdpBuffers};
    use embassy_sync::signal::Signal;

    let udp_buffers = MDNS_UDP_BUFFERS.init(UdpBuffers::new());
    let recv_buf = MDNS_RECV_BUFFER.init(VecBufAccess::new());
    let send_buf = MDNS_SEND_BUFFER.init(VecBufAccess::new());
    let broadcast_signal = MDNS_BROADCAST_SIGNAL.init(Signal::new());
    let udp = Udp::new(stack, udp_buffers);

    loop {
        stack.wait_config_up().await;
        let Some(ipv4) = stack.config_v4().map(|config| config.address.address()) else {
            continue;
        };
        let Ok(mut socket) = io::bind(&udp, io::IPV4_DEFAULT_SOCKET, Some(ipv4), None).await else {
            Timer::after_secs(1).await;
            continue;
        };
        let (recv, send) = socket.split();
        let host = Host {
            hostname: &hostname,
            ipv4,
            ipv6: Ipv6Addr::UNSPECIFIED,
            ttl: Ttl::from_secs(120),
        };
        let mdns = io::Mdns::new(
            Some(ipv4),
            None,
            recv,
            send,
            &*recv_buf,
            &*send_buf,
            Rng::new(),
            &*broadcast_signal,
        );
        match select(
            mdns.run(HostAnswersMdnsHandler::new(&host)),
            stack.wait_config_down(),
        )
        .await
        {
            Either::First(_) => Timer::after_secs(1).await,
            Either::Second(_) => {}
        }
    }
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

fn notifiable_incidents(state: &SharedState) -> Vec<Incident> {
    active_incidents(state)
        .into_iter()
        .filter(|incident| {
            state.snapshot.is_some() || incident.id != IncidentId::Stick(StickIncident::HostOffline)
        })
        .collect()
}

fn dashboard_html(state: &SharedState) -> String {
    dashboard_markup(state, true)
}

fn dashboard_fragment(state: &SharedState) -> String {
    dashboard_markup(state, false)
}

fn dashboard_markup(state: &SharedState, document: bool) -> String {
    let incidents = active_incidents(state);
    let age = snapshot_age(state).map(|age| age.as_secs());
    let stale = age.is_none_or(|age| age >= HOST_STALE_AFTER.as_secs());
    // Freshness is represented separately by the LIVE/STALE badge. Keep the
    // headline tied to the last actual host health result instead of rewriting
    // a healthy snapshot as critical merely because it is cached.
    let level = state
        .snapshot
        .as_ref()
        .map_or(HealthLevel::Critical, |snapshot| snapshot.health.level);
    let mut html = DashboardBuffer::new();
    if document {
        html.push_str("<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><meta name=theme-color content='#f2f6f8'><title>Servatory status</title><link rel=stylesheet href=/dashboard.css><script defer src=/dashboard.js></script>");
        let _ = write!(html, "</head><body class={}>", level_class(level));
    }
    let age_text = age.map_or_else(
        || "No host snapshot received".to_string(),
        |seconds| match seconds {
            0 => "Updated just now".to_string(),
            1 => "Updated 1 second ago".to_string(),
            _ => format!("Updated {seconds} seconds ago"),
        },
    );
    let age_value = age.map_or(-1_i64, |seconds| seconds as i64);
    let _ = write!(
        html,
        "<div class=shell data-body-class={} data-snapshot-age={age_value} data-stale-after={}><header class=hero><div class=hero-top><div class=brand><span class=brand-mark>{}</span><span>SERVATORY</span></div><div class=live>{}</div></div>\
         <div class=hero-status><div class=eyebrow>System health</div><h1>{}</h1><p class=snapshot-age aria-live=polite>{}</p></div></header>",
        level_class(level),
        HOST_STALE_AFTER.as_secs(),
        dashboard_icon(0),
        if stale { "STALE" } else { "LIVE" },
        level_text(level),
        age_text,
    );
    if !incidents.is_empty() {
        html.push_str(
            "<section class='card wide'><div class=card-head><span class='icon icon-alert'>",
        );
        html.push_str(dashboard_icon(7));
        html.push_str("</span><h2>Active incidents</h2></div><ul class=incident-list>");
        for incident in &incidents {
            let _ = write!(html, "<li class={}>", level_class(incident.level));
            push_html(&mut html, incident.message());
            html.push_str("</li>");
        }
        html.push_str("</ul></section>");
    }
    html.push_str("<main class=grid>");
    let Some(snapshot) = state.snapshot.as_ref() else {
        render_device_panels(&mut html, state);
        html.push_str("</main></div>");
        if document {
            html.push_str("</body></html>");
        }
        return html.finish(document);
    };
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
        let _ = write!(
            html,
            "<section class='card page-card {}'><div class=card-head><span class=icon>",
            page_class(kind)
        );
        html.push_str(dashboard_icon(kind));
        html.push_str("</span><h2>");
        push_html(&mut html, view.title.as_str());
        html.push_str("</h2></div>");
        render_page(&mut html, &view.page, snapshot, &state.filesystem_labels);
        html.push_str("</section>");
    }
    render_device_panels(&mut html, state);
    html.push_str("</main></div>");
    if document {
        html.push_str("</body></html>");
    }
    html.finish(document)
}

fn render_device_panels(html: &mut DashboardBuffer, state: &SharedState) {
    let address = state
        .station_ipv4
        .map_or_else(|| "WAITING".to_string(), |address| address.to_string());
    let daemon = state
        .daemon_version
        .as_ref()
        .map_or("WAITING", SoftwareVersion::as_str);
    let hostname = state
        .manifest
        .as_ref()
        .map_or("servatory", |manifest| manifest.http.hostname());
    let protocol = format!("{PROTOCOL_VERSION}");
    html.push_str("<section class='card notifications'><div class=card-head><span class=icon>");
    html.push_str(dashboard_icon(5));
    html.push_str("</span><h2>Notifications</h2></div>");
    if let Some(topic) = state.ntfy_topic.as_deref() {
        html.push_str(
            "<span class=label>ntfy topic</span><div class=topic-wrap><code id=topic class=topic>",
        );
        push_html(html, topic);
        html.push_str("</code><button class=copy-button type=button onclick='copyNtfyTopic(this)'>Copy</button></div>");
    }
    let (enabled, server) = state.manifest.as_ref().map_or((false, "—"), |manifest| {
        (manifest.ntfy.enabled, manifest.ntfy.server())
    });
    let _ = write!(
        html,
        "<div class=facts><div class=fact><span class=label>Status</span><strong class={}>{}</strong></div>",
        if enabled { "ok" } else { "warn" },
        if enabled { "ENABLED" } else { "DISABLED" }
    );
    html.push_str("<div class=fact><span class=label>Server</span><strong>");
    push_html(html, server);
    html.push_str("</strong></div></div><div class=button-row><button data-client-state type=button onclick='sendTestNotification(this)'>Send test notification</button><span data-client-state id=test-feedback class=test-feedback aria-live=polite></span><button class=button-secondary type=button onclick=\"if(confirm('Generate a new topic? Existing ntfy subscriptions will stop receiving Servatory alerts.'))fetch('/api/v1/notifications/topic/regenerate',{method:'POST',headers:{'X-Servatory-Action':'regenerate'}}).then(()=>location.reload())\">Generate new topic</button></div></section>");

    html.push_str("<section class='card about'><div class=card-head><span class=icon>");
    html.push_str(dashboard_icon(6));
    html.push_str("</span><h2>About</h2></div><div class=facts>");
    for (label, value) in [
        ("Firmware", env!("SERVATORY_BUILD_VERSION")),
        ("Daemon", daemon),
        ("Protocol", protocol.as_str()),
        ("IP address", address.as_str()),
        ("Hostname", hostname),
    ] {
        html.push_str("<div class=fact><span class=label>");
        push_html(html, label);
        html.push_str("</span><strong>");
        push_html(html, value);
        html.push_str("</strong></div>");
    }
    html.push_str(
        "</div><p class=about-note>Servatory firmware and host connection details.</p></section>",
    );
}

const fn page_class(kind: usize) -> &'static str {
    match kind {
        0 => "page-overview",
        1 => "page-resources",
        2 => "page-storage",
        3 => "page-power",
        4 => "page-guests",
        _ => "",
    }
}

// Lucide icons stay crisp at any density and keep this embedded page self-contained.
const fn dashboard_icon(kind: usize) -> &'static str {
    match kind {
        0 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2'/></svg>"
        }
        1 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='m12 14 4-4'/><path d='M3.34 19a10 10 0 1 1 17.32 0'/></svg>"
        }
        2 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='M10 16h.01'/><path d='M2.212 11.577a2 2 0 0 0-.212.896V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.527a2 2 0 0 0-.212-.896L18.55 5.11A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z'/><path d='M21.946 12.013H2.054'/><path d='M6 16h.01'/></svg>"
        }
        3 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><rect x=16 y=16 width=6 height=6 rx=1/><rect x=2 y=16 width=6 height=6 rx=1/><rect x=9 y=2 width=6 height=6 rx=1/><path d='M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3'/><path d='M12 12V8'/></svg>"
        }
        4 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='M2.97 12.92A2 2 0 0 0 2 14.63v3.24a2 2 0 0 0 .97 1.71l3 1.8a2 2 0 0 0 2.06 0L12 19v-5.5l-5-3-4.03 2.42Z'/><path d='m7 16.5-4.74-2.85'/><path d='m7 16.5 5-3'/><path d='M7 16.5v5.17'/><path d='M12 13.5V19l3.97 2.38a2 2 0 0 0 2.06 0l3-1.8a2 2 0 0 0 .97-1.71v-3.24a2 2 0 0 0-.97-1.71L17 10.5l-5 3Z'/><path d='m17 16.5-5-3'/><path d='m17 16.5 4.74-2.85'/><path d='M17 16.5v5.17'/><path d='M7.97 4.42A2 2 0 0 0 7 6.13v4.37l5 3 5-3V6.13a2 2 0 0 0-.97-1.71l-3-1.8a2 2 0 0 0-2.06 0l-3 1.8Z'/><path d='M12 8 7.26 5.15'/><path d='m12 8 4.74-2.85'/><path d='M12 13.5V8'/></svg>"
        }
        5 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='M10.268 21a2 2 0 0 0 3.464 0'/><path d='M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326'/></svg>"
        }
        6 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><circle cx=12 cy=12 r=10/><path d='M12 16v-4'/><path d='M12 8h.01'/></svg>"
        }
        7 => {
            "<svg aria-hidden=true viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round><path d='m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3'/><path d='M12 9v4'/><path d='M12 17h.01'/></svg>"
        }
        _ => "",
    }
}

fn render_page(
    html: &mut DashboardBuffer,
    page: &DisplayPage,
    snapshot: &HealthSnapshot,
    filesystem_labels: &[DisplayLabel],
) {
    match page {
        DisplayPage::Overview => {
            let running = snapshot
                .guests
                .guests()
                .iter()
                .filter(|guest| guest.status == GuestStatus::Running)
                .count();
            let days = snapshot.uptime_seconds / 86_400;
            let hours = snapshot.uptime_seconds % 86_400 / 3_600;
            html.push_str("<div class=overview-lead><span class=pulse></span><div><span class=label>Current state</span><strong class=");
            html.push_str(level_class(snapshot.health.level));
            html.push('>');
            push_html(html, snapshot.health.message());
            html.push_str("</strong></div></div><div class=overview-grid><div class=datum><span class=label>Host</span><strong>");
            push_html(html, snapshot.host_name());
            let _ = write!(
                html,
                "</strong></div><div class=datum><span class=label>Uptime</span><strong>{days}d {hours}h</strong></div><div class=datum><span class=label>Network</span><strong>"
            );
            if snapshot.network_interface().is_empty() {
                html.push_str("No interface");
            } else {
                push_html(html, snapshot.network_interface());
            }
            let _ = write!(
                html,
                "</strong><small>{} Mbps</small></div><div class=datum><span class=label>Guests</span><strong>{running} / {}</strong><small>running / total</small></div></div>",
                snapshot.network_mbps,
                snapshot.guests.guests().len(),
            );
        }
        DisplayPage::Resources => {
            let memory = if snapshot.memory_total_mib == 0 {
                0
            } else {
                u64::from(snapshot.memory_used_mib) * 100 / u64::from(snapshot.memory_total_mib)
            };
            render_metric(html, "CPU", snapshot.cpu_percent, "processor usage");
            render_metric(
                html,
                "Memory",
                u8::try_from(memory).unwrap_or(100),
                &format!(
                    "{} / {} MiB",
                    snapshot.memory_used_mib, snapshot.memory_total_mib
                ),
            );
            render_metric(
                html,
                "I/O pressure",
                snapshot.io_pressure_percent,
                "recent pressure",
            );
            let _ = write!(
                html,
                "<div class=inline-stat><span class=label>Load average</span><strong>{:.2}</strong></div>",
                f32::from(snapshot.load_average_x100) / 100.0
            );
        }
        DisplayPage::Storage { .. } => {
            html.push_str("<div class=storage-grid><section class=subpanel><h3 class=section-title>Filesystems</h3>");
            for (index, usage) in snapshot.filesystems.iter().enumerate() {
                let label = filesystem_labels
                    .get(index)
                    .map_or("Filesystem", DisplayLabel::as_str);
                let detail = if usage.mounted {
                    format!("{} available", format_capacity_mib(usage.available_mib))
                } else {
                    "MISSING".to_string()
                };
                render_metric(html, label, usage.used_percent, &detail);
            }
            html.push_str("</section><section class=subpanel><h3 class=section-title>SMART devices</h3><div class=table-list>");
            if snapshot.smart.devices().is_empty() {
                html.push_str("<p class=empty>Not configured</p>");
            }
            for device in snapshot.smart.devices() {
                html.push_str("<div class=table-row><strong>");
                push_html(html, device.label());
                let _ = write!(
                    html,
                    "</strong><span class={}>{}{}",
                    smart_status_class(device.status),
                    smart_status_text(device.status),
                    device
                        .temperature_celsius
                        .map_or_else(String::new, |temperature| format!(" · {temperature}°C")),
                );
                html.push_str("</span></div>");
            }
            html.push_str("</div></section></div><div class=backup-strip><span class=label>Proxmox backup</span><strong class=");
            html.push_str(backup_status_class(snapshot.backup_job_status));
            html.push('>');
            push_html(html, &backup_status_text(snapshot));
            html.push_str("</strong></div>");
        }
        DisplayPage::PowerNetwork { .. } => {
            html.push_str("<div class=power-grid><section class=subpanel><h3 class=section-title>UPS</h3><div class=table-list><div class=table-row><strong>Status</strong><span class=");
            html.push_str(ups_status_class(snapshot.ups.status));
            html.push('>');
            push_html(
                html,
                ups_status_text(snapshot.ups.status, snapshot.ups.stale),
            );
            html.push_str("</span></div><div class=table-row><strong>Battery</strong><span>");
            if let Some(battery) = snapshot.ups.battery_percent {
                let _ = write!(html, "{battery}%");
            } else {
                html.push('—');
            }
            html.push_str("</span></div><div class=table-row><strong>Load</strong><span>");
            match (snapshot.ups.load_percent, snapshot.ups.estimated_watts) {
                (Some(load), Some(watts)) => {
                    let _ = write!(html, "{load}% · ~{watts} W");
                }
                (Some(load), None) => {
                    let _ = write!(html, "{load}%");
                }
                (None, _) => html.push('—'),
            }
            html.push_str("</span></div><div class=table-row><strong>Runtime</strong><span>");
            if let Some(seconds) = snapshot.ups.runtime_seconds {
                push_html(html, &format_duration_compact(seconds));
            } else {
                html.push('—');
            }
            html.push_str("</span></div></div></section><section class=subpanel><h3 class=section-title>Ethernet</h3><div class=table-list><div class=table-row><strong>Interface</strong><span>");
            if snapshot.network_interface().is_empty() {
                html.push_str("Not found");
            } else {
                push_html(html, snapshot.network_interface());
            }
            let _ = write!(
                html,
                "</span></div><div class=table-row><strong>Link</strong><span class={}>{} · {} Mbps</span></div><div class=table-row><strong>Internet</strong><span class={}>{}</span></div><div class=table-row><strong>Host IPv4</strong><span>{}.{}.{}.{}</span></div>",
                if snapshot.network_up { "ok" } else { "crit" },
                if snapshot.network_up { "UP" } else { "DOWN" },
                snapshot.network_mbps,
                internet_status_class(snapshot.internet_status),
                internet_status_text(snapshot.internet_status),
                snapshot.ipv4[0],
                snapshot.ipv4[1],
                snapshot.ipv4[2],
                snapshot.ipv4[3],
            );
            if snapshot.internet_status != InternetStatus::Reachable {
                html.push_str("<div class=table-row><strong>Last reachable</strong><span>");
                if let Some(seconds) = snapshot.last_internet_success_age_seconds {
                    let _ = write!(html, "{} ago", format_duration_compact(seconds));
                } else {
                    html.push_str("Unknown");
                }
                html.push_str("</span></div>");
            }
            html.push_str("</div></section></div>");
        }
        DisplayPage::Guests { .. } => {
            html.push_str("<div class=guest-list>");
            if snapshot.guests.guests().is_empty() {
                html.push_str("<p class=empty>No guests reported</p>");
            }
            for guest in snapshot.guests.guests() {
                let memory_percent = if guest.memory_total_mib == 0 {
                    0
                } else {
                    (u64::from(guest.memory_used_mib) * 100 / u64::from(guest.memory_total_mib))
                        .min(100) as u8
                };
                let _ = write!(
                    html,
                    "<div class=guest-row><span class=guest-id>#{}</span><div class=guest-name><strong>",
                    guest.vmid
                );
                push_html(html, guest.name());
                html.push_str("</strong><small>");
                html.push_str(match guest.kind {
                    GuestKind::VirtualMachine => "Virtual machine",
                    GuestKind::Container => "Container",
                });
                let _ = write!(
                    html,
                    "</small></div><span class='guest-state {}'>{}</span><div class=guest-usage><span><b>CPU</b><b>{}%</b></span><div class=bar><i style='width:{}%'></i></div><span><b>Memory</b><b>{} / {} MiB</b></span><div class=bar><i style='width:{}%'></i></div></div></div>",
                    if guest.status == GuestStatus::Running {
                        "ok"
                    } else {
                        "crit"
                    },
                    if guest.status == GuestStatus::Running {
                        "RUNNING"
                    } else {
                        "STOPPED"
                    },
                    guest.cpu_percent,
                    guest.cpu_percent.min(100),
                    guest.memory_used_mib,
                    guest.memory_total_mib,
                    memory_percent,
                );
            }
            html.push_str("</div>");
        }
    }
}

fn format_capacity_mib(mebibytes: u32) -> String {
    if mebibytes >= 1_048_576 {
        let tenths = u64::from(mebibytes) * 10 / 1_048_576;
        format!("{}.{} TiB", tenths / 10, tenths % 10)
    } else if mebibytes >= 1_024 {
        let tenths = u64::from(mebibytes) * 10 / 1_024;
        format!("{}.{} GiB", tenths / 10, tenths % 10)
    } else {
        format!("{mebibytes} MiB")
    }
}

fn format_duration_compact(seconds: u32) -> String {
    if seconds >= 86_400 {
        format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3_600)
    } else if seconds >= 3_600 {
        format!("{}h {}m", seconds / 3_600, seconds % 3_600 / 60)
    } else {
        format!("{}m", seconds / 60)
    }
}

fn backup_status_text(snapshot: &HealthSnapshot) -> String {
    match snapshot.backup_job_status {
        BackupJobStatus::Healthy => snapshot.last_successful_backup_age_seconds.map_or_else(
            || "Healthy".to_string(),
            |seconds| format!("Healthy · {} ago", format_duration_compact(seconds)),
        ),
        BackupJobStatus::Running => "Running".to_string(),
        BackupJobStatus::Failed => "Failed".to_string(),
        BackupJobStatus::Stale => snapshot.last_successful_backup_age_seconds.map_or_else(
            || "Overdue".to_string(),
            |seconds| format!("{} old", format_duration_compact(seconds)),
        ),
        BackupJobStatus::NoJob => "No job".to_string(),
        BackupJobStatus::Unknown => "Unknown".to_string(),
    }
}

const fn backup_status_class(status: BackupJobStatus) -> &'static str {
    match status {
        BackupJobStatus::Healthy => "ok",
        BackupJobStatus::Running => "ok",
        BackupJobStatus::Failed | BackupJobStatus::Stale | BackupJobStatus::NoJob => "warn",
        BackupJobStatus::Unknown => "crit",
    }
}

const fn smart_status_text(status: SmartStatus) -> &'static str {
    match status {
        SmartStatus::Healthy => "HEALTHY",
        SmartStatus::Warning => "WARNING",
        SmartStatus::Failed => "FAILED",
        SmartStatus::Sleeping => "SLEEPING",
        SmartStatus::Unknown => "UNKNOWN",
    }
}

const fn smart_status_class(status: SmartStatus) -> &'static str {
    match status {
        SmartStatus::Healthy => "ok",
        SmartStatus::Sleeping => "",
        SmartStatus::Warning | SmartStatus::Unknown => "warn",
        SmartStatus::Failed => "crit",
    }
}

const fn ups_status_text(status: UpsStatus, stale: bool) -> &'static str {
    if stale && !matches!(status, UpsStatus::Unavailable) {
        return "STALE";
    }
    match status {
        UpsStatus::NotConfigured => "NOT CONFIGURED",
        UpsStatus::Unknown | UpsStatus::Unavailable => "NO DATA",
        UpsStatus::Online => "ONLINE",
        UpsStatus::OnBattery => "ON BATTERY",
        UpsStatus::LowBattery => "LOW BATTERY",
        UpsStatus::Charging => "CHARGING",
        UpsStatus::Bypass => "BYPASS",
        UpsStatus::OutputOff => "OUTPUT OFF",
        UpsStatus::ReplaceBattery => "REPLACE BATTERY",
    }
}

const fn ups_status_class(status: UpsStatus) -> &'static str {
    match status {
        UpsStatus::Online | UpsStatus::Charging => "ok",
        UpsStatus::NotConfigured => "",
        UpsStatus::OnBattery | UpsStatus::Bypass | UpsStatus::Unknown | UpsStatus::Unavailable => {
            "warn"
        }
        UpsStatus::LowBattery | UpsStatus::OutputOff | UpsStatus::ReplaceBattery => "crit",
    }
}

const fn internet_status_text(status: InternetStatus) -> &'static str {
    match status {
        InternetStatus::Reachable => "REACHABLE",
        InternetStatus::Checking => "CHECKING",
        InternetStatus::Missed => "RETRYING",
        InternetStatus::Failed => "FAILED",
    }
}

const fn internet_status_class(status: InternetStatus) -> &'static str {
    match status {
        InternetStatus::Reachable => "ok",
        InternetStatus::Checking | InternetStatus::Missed => "warn",
        InternetStatus::Failed => "crit",
    }
}

fn render_metric(html: &mut DashboardBuffer, label: &str, percent: u8, detail: &str) {
    html.push_str("<div class=metric><div class=metric-row><strong>");
    push_html(html, label);
    let _ = write!(html, "</strong><span>{percent}% · ");
    push_html(html, detail);
    let _ = write!(
        html,
        "</span></div><div class=bar><i style='width:{}%'></i></div></div>",
        percent.min(100)
    );
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

fn push_html(output: &mut DashboardBuffer, value: &str) {
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

struct PendingNotification {
    message: String,
    priority: NotificationPriority,
    recovery: bool,
}

struct NotificationWorkspace {
    tls_rx: PsramBox<[u8]>,
    tls_tx: PsramBox<[u8]>,
    response: PsramBox<[u8]>,
    ca: Vec<u8>,
}

impl NotificationWorkspace {
    fn new() -> Self {
        Self {
            tls_rx: zeroed_psram(NOTIFICATION_TLS_RX_SIZE),
            tls_tx: zeroed_psram(NOTIFICATION_TLS_TX_SIZE),
            response: zeroed_psram(NOTIFICATION_RESPONSE_SIZE),
            ca: STANDARD.decode(ISRG_ROOT_X1).unwrap_or_default(),
        }
    }
}

#[embassy_executor::task]
async fn notification_worker(stack: Stack<'static>, mut seed: u64) {
    stack.wait_config_up().await;
    let mut workspace = NotificationWorkspace::new();
    let tcp_state = make_static(TcpClientState::<1, 4096, 4096>::new());
    let tcp = TcpClient::new(stack, tcp_state);
    let dns = DnsSocket::new(stack);
    let mut previous: Vec<Incident> = Vec::new();
    let mut pending = VecDeque::new();
    let mut next_critical_repeat = None;
    loop {
        let state = STATE.lock().await.clone();
        let current = notifiable_incidents(&state);
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
            if let (Some(notification), Some(topic)) =
                (pending.front(), state.ntfy_topic.as_deref())
            {
                seed = seed.wrapping_add(1);
                if publish_notification(
                    &tcp,
                    &dns,
                    config,
                    topic,
                    notification,
                    seed,
                    &mut workspace,
                )
                .await
                {
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
    workspace: &mut NotificationWorkspace,
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
    if workspace.ca.is_empty() {
        return false;
    }
    let tls = TlsConfig::new(
        seed,
        workspace.tls_rx.as_mut(),
        workspace.tls_tx.as_mut(),
        TlsVerify::Certificate {
            ca: &workspace.ca,
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
    request
        .send(workspace.response.as_mut())
        .await
        .is_ok_and(|response| response.status.is_successful())
}
