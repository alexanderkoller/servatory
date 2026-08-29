#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use core::fmt::Write as _;

use embedded_graphics::{
    framebuffer::{Framebuffer, buffer_size},
    image::Image,
    mono_font::{MonoTextStyle, ascii::FONT_6X10, ascii::FONT_10X20},
    pixelcolor::{Rgb565, raw::BigEndian},
    prelude::*,
    primitives::{Arc, Line, PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    i2c::master::{Config as I2cConfig, Error as I2cError, I2c},
    main,
    spi::master::{Config as SpiConfig, Spi},
    time::{Duration, Instant, Rate},
    usb_serial_jtag::UsbSerialJtag,
};
use heapless::{Deque, String};
use mipidsi::{
    Builder,
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
};
use s3_display_protocol::{
    BackupJobStatus, ButtonAction, DeviceMessage, FilesystemUsage, FrameDecoder, GuestKind,
    GuestStatus, HealthSnapshot, HealthStatus, HostMessage, InternetStatus, MAX_FRAME_LEN,
    ShutdownFailure, ShutdownPhase, SmartDeviceSummary, SmartStatus, UpsStatus, decode_host,
    encode_device,
};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

const PANEL_WIDTH: u16 = 135;
const PANEL_HEIGHT: u16 = 240;
const DISPLAY_WIDTH: u16 = 240;
const DISPLAY_HEIGHT: u16 = 135;
type ScreenBuffer = Framebuffer<
    Rgb565,
    <Rgb565 as PixelColor>::Raw,
    BigEndian,
    240,
    135,
    { buffer_size::<Rgb565>(240, 135) },
>;
static SCREEN_BUFFER: StaticCell<ScreenBuffer> = StaticCell::new();
// M5Stack does not publish controller RAM offsets; these match this 135x240 panel family.
const DISPLAY_OFFSET_X: u16 = 52;
const DISPLAY_OFFSET_Y: u16 = 40;
const DEBOUNCE: Duration = Duration::from_millis(30);
const SHUTDOWN_ANIMATION_DELAY: Duration = Duration::from_millis(200);
const LONG_PRESS: Duration = Duration::from_millis(3_000);
const LINK_TIMEOUT: Duration = Duration::from_secs(15);
const USB_ACTIVITY_TIMEOUT: Duration = Duration::from_millis(250);
const SHUTDOWN_SPINNER_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_REPORT_TIMEOUT: Duration = Duration::from_secs(3);
const SHUTDOWN_USB_LOST_CONFIRM: Duration = Duration::from_secs(2);
const MAX_DEVICE_FRAME_LEN: usize = 64;
const PAGE_COUNT: u8 = 6;
const GUESTS_PER_PAGE: usize = 4;
const M5PM1_ADDRESS: u8 = 0x6e;
const M5PM1_GPIO2_MASK: u8 = 1 << 2;

#[derive(Clone, Copy, Eq, PartialEq)]
enum DaemonState {
    Waiting,
    Connected,
    Stale,
    PoweringOff,
}

#[derive(Clone, Copy)]
enum ShutdownDisplay {
    Accepted,
    Guests {
        total: u16,
        remaining: u16,
    },
    GuestsStopped,
    HostPoweroff {
        spinner: u8,
        remaining: u16,
    },
    ReportingLost {
        remaining: u16,
    },
    ConnectionLost {
        remaining: u16,
    },
    Failed {
        reason: ShutdownFailure,
        remaining: u16,
    },
}

#[derive(Clone, Copy)]
enum UiIcon {
    Cpu,
    Memory,
    Disk,
    Ups,
    Network,
    Guests,
    Qemu,
    Container,
    Io,
    Load,
}

struct UsbTx {
    queue: Deque<DeviceMessage, 8>,
    frame: [u8; MAX_DEVICE_FRAME_LEN],
    len: usize,
    cursor: usize,
}

impl UsbTx {
    const fn new() -> Self {
        Self {
            queue: Deque::new(),
            frame: [0; MAX_DEVICE_FRAME_LEN],
            len: 0,
            cursor: 0,
        }
    }

    fn enqueue(&mut self, message: DeviceMessage) {
        if self.queue.push_back(message).is_err()
            && message == DeviceMessage::Button(ButtonAction::ShutdownRequested)
        {
            // A shutdown request is more important than an old acknowledgement.
            self.queue.pop_front();
            let _ = self.queue.push_back(message);
        }
    }

    fn poll(&mut self, usb: &mut UsbSerialJtag<'_, esp_hal::Blocking>) {
        if self.cursor == self.len {
            let Some(message) = self.queue.pop_front() else {
                return;
            };
            let Ok(frame) = encode_device(message, &mut self.frame) else {
                return;
            };
            self.len = frame.len();
            self.cursor = 0;
        }

        while self.cursor < self.len {
            if usb.write_byte_nb(self.frame[self.cursor]).is_err() {
                return;
            }
            self.cursor += 1;
        }

        // Setting WR_DONE once hands this packet to the USB peripheral. Whether
        // it completes immediately or reports WouldBlock, the endpoint owns the
        // bytes from here. A later write_byte_nb call is the readiness check for
        // the next packet; repeatedly flushing here can re-trigger WR_DONE and
        // stall the endpoint.
        let _ = usb.flush_tx_nb();
        self.len = 0;
        self.cursor = 0;
    }
}

struct Button {
    raw_pressed: bool,
    raw_changed_at: Instant,
    stable_pressed: bool,
    pressed_at: Option<Instant>,
    long_press_sent: bool,
}

impl Button {
    const fn new(now: Instant) -> Self {
        Self {
            raw_pressed: false,
            raw_changed_at: now,
            stable_pressed: false,
            pressed_at: None,
            long_press_sent: false,
        }
    }

    fn update(&mut self, pressed: bool, now: Instant) -> Option<ButtonAction> {
        if pressed != self.raw_pressed {
            self.raw_pressed = pressed;
            self.raw_changed_at = now;
            return None;
        }

        if self.stable_pressed != self.raw_pressed && now - self.raw_changed_at >= DEBOUNCE {
            self.stable_pressed = self.raw_pressed;
            if self.stable_pressed {
                self.pressed_at = Some(now);
                self.long_press_sent = false;
            } else {
                self.pressed_at = None;
                if !self.long_press_sent {
                    return Some(ButtonAction::NextScreen);
                }
            }
        }

        if self.stable_pressed
            && !self.long_press_sent
            && self
                .pressed_at
                .is_some_and(|started| now - started >= LONG_PRESS)
        {
            self.long_press_sent = true;
            return Some(ButtonAction::ShutdownRequested);
        }
        None
    }

    fn held_for(&self, now: Instant) -> Option<Duration> {
        (self.stable_pressed && !self.long_press_sent)
            .then(|| self.pressed_at.map(|started| now - started))
            .flatten()
    }
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let mut delay = Delay::new();

    // The LCD is supplied by the L3B rail, controlled by M5PM1 GPIO2. The
    // PMIC remains powered across ESP32 resets, so establish the required
    // state explicitly instead of relying on whichever firmware ran before.
    let mut power_i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
        .expect("M5PM1 I2C configuration")
        .with_sda(peripherals.GPIO47)
        .with_scl(peripherals.GPIO48);
    enable_lcd_power(&mut power_i2c).expect("M5PM1 LCD power configuration");
    delay.delay_millis(100);

    // Official StickS3 pin map: SCK 40, MOSI 39, CS 41, DC 45, RST 21, BL 38.
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(40)),
    )
    .expect("SPI configuration")
    .with_sck(peripherals.GPIO40)
    .with_mosi(peripherals.GPIO39);
    let cs = Output::new(peripherals.GPIO41, Level::High, OutputConfig::default());
    let dc = Output::new(peripherals.GPIO45, Level::Low, OutputConfig::default());
    let reset = Output::new(peripherals.GPIO21, Level::High, OutputConfig::default());
    let mut backlight = Output::new(peripherals.GPIO38, Level::Low, OutputConfig::default());
    let spi_device = ExclusiveDevice::new_no_delay(spi, cs).expect("infallible SPI device");
    let mut display_buffer = [0_u8; 512];
    let interface = SpiInterface::new(spi_device, dc, &mut display_buffer);
    let mut display = Builder::new(ST7789, interface)
        .display_size(PANEL_WIDTH, PANEL_HEIGHT)
        .display_offset(DISPLAY_OFFSET_X, DISPLAY_OFFSET_Y)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .reset_pin(reset)
        .init(&mut delay)
        .expect("display initialization");
    let framebuffer = SCREEN_BUFFER.init(ScreenBuffer::new());
    backlight.set_high();

    // KEY1 is the front button used for navigation and shutdown.
    let button_pin = Input::new(
        peripherals.GPIO11,
        InputConfig::default().with_pull(Pull::Up),
    );
    let mut usb = UsbSerialJtag::new(peripherals.USB_DEVICE);
    let mut decoder = FrameDecoder::<MAX_FRAME_LEN>::new();
    let mut usb_tx = UsbTx::new();
    let mut button = Button::new(Instant::now());
    let mut daemon = DaemonState::Waiting;
    let mut usb_connected = false;
    let mut last_usb_activity = None;
    let mut last_update = None;
    let mut health_snapshot = None;
    let mut page = 0_u8;
    let mut shutdown_animation_active = false;
    let mut shutdown_animation_height = 0_u16;
    let mut shutdown_display = ShutdownDisplay::Accepted;
    let mut last_shutdown_spinner = Instant::now();
    let mut shutdown_usb_lost_since = None;

    // Render before attempting any USB traffic so offline operation always works.
    render(
        &mut display,
        framebuffer,
        usb_connected,
        daemon,
        page,
        health_snapshot,
        shutdown_display,
    );
    usb_tx.enqueue(DeviceMessage::Ready);

    loop {
        let now = Instant::now();
        let was_usb_connected = usb_connected;
        usb_connected = poll_usb_connection(now, &mut last_usb_activity);
        // USB SOF activity is advisory and can momentarily disappear while a
        // valid serial session is alive. Only decoded-update timeout changes an
        // established session to stale; physical activity redraws the waiting
        // screen only before a session has been established.
        if usb_connected != was_usb_connected
            && daemon == DaemonState::Waiting
            && !shutdown_animation_active
        {
            render(
                &mut display,
                framebuffer,
                usb_connected,
                daemon,
                page,
                health_snapshot,
                shutdown_display,
            );
        }

        while let Ok(byte) = usb.read_byte() {
            let Some(Ok(frame)) = decoder.push(byte) else {
                continue;
            };
            match decode_host(frame) {
                Ok(HostMessage::Update { sequence, .. }) => {
                    last_update = Some(now);
                    // ShutdownAccepted is terminal only for the old host session.
                    // A subsequent valid update proves that the host is running again.
                    daemon = DaemonState::Connected;
                    usb_tx.enqueue(DeviceMessage::Ack { sequence });
                    if !shutdown_animation_active {
                        render(
                            &mut display,
                            framebuffer,
                            usb_connected,
                            daemon,
                            page,
                            health_snapshot,
                            shutdown_display,
                        );
                    }
                }
                Ok(HostMessage::GuestSnapshot { sequence, .. }) => {
                    last_update = Some(now);
                    daemon = DaemonState::Connected;
                    usb_tx.enqueue(DeviceMessage::Ack { sequence });
                    if !shutdown_animation_active {
                        render(
                            &mut display,
                            framebuffer,
                            usb_connected,
                            daemon,
                            page,
                            health_snapshot,
                            shutdown_display,
                        );
                    }
                }
                Ok(HostMessage::HealthSnapshot {
                    sequence, snapshot, ..
                }) => {
                    last_update = Some(now);
                    health_snapshot = Some(snapshot);
                    daemon = DaemonState::Connected;
                    usb_tx.enqueue(DeviceMessage::Ack { sequence });
                    if !shutdown_animation_active {
                        render(
                            &mut display,
                            framebuffer,
                            usb_connected,
                            daemon,
                            page,
                            health_snapshot,
                            shutdown_display,
                        );
                    }
                }
                Ok(HostMessage::ShutdownAccepted) => {
                    last_update = Some(now);
                    shutdown_usb_lost_since = None;
                    daemon = DaemonState::PoweringOff;
                    shutdown_display = ShutdownDisplay::Accepted;
                    render(
                        &mut display,
                        framebuffer,
                        usb_connected,
                        daemon,
                        page,
                        health_snapshot,
                        shutdown_display,
                    );
                }
                Ok(HostMessage::ShutdownProgress {
                    phase,
                    guests_total,
                    guests_remaining,
                }) => {
                    last_update = Some(now);
                    shutdown_usb_lost_since = None;
                    daemon = DaemonState::PoweringOff;
                    shutdown_display = match phase {
                        ShutdownPhase::PreparingGuests => ShutdownDisplay::Accepted,
                        ShutdownPhase::StoppingGuests => ShutdownDisplay::Guests {
                            total: guests_total,
                            remaining: guests_remaining,
                        },
                        ShutdownPhase::GuestsStopped => ShutdownDisplay::GuestsStopped,
                        ShutdownPhase::PoweringOff => {
                            last_shutdown_spinner = now;
                            ShutdownDisplay::HostPoweroff {
                                spinner: 0,
                                remaining: guests_remaining,
                            }
                        }
                    };
                    render(
                        &mut display,
                        framebuffer,
                        usb_connected,
                        daemon,
                        page,
                        health_snapshot,
                        shutdown_display,
                    );
                }
                Ok(HostMessage::ShutdownFailed {
                    reason,
                    guests_remaining,
                }) => {
                    daemon = DaemonState::PoweringOff;
                    shutdown_display = ShutdownDisplay::Failed {
                        reason,
                        remaining: guests_remaining,
                    };
                    render(
                        &mut display,
                        framebuffer,
                        usb_connected,
                        daemon,
                        page,
                        health_snapshot,
                        shutdown_display,
                    );
                }
                Err(_) => {}
            }
        }

        if daemon == DaemonState::Connected
            && last_update.is_some_and(|updated| now - updated >= LINK_TIMEOUT)
        {
            daemon = DaemonState::Stale;
            if !shutdown_animation_active {
                render(
                    &mut display,
                    framebuffer,
                    usb_connected,
                    daemon,
                    page,
                    health_snapshot,
                    shutdown_display,
                );
            }
        }

        if let Some(action) = button.update(button_pin.is_low(), now) {
            let canceled_shutdown = action == ButtonAction::NextScreen && shutdown_animation_active;
            if action == ButtonAction::NextScreen {
                if !canceled_shutdown {
                    page = (page + 1) % PAGE_COUNT;
                }
                shutdown_animation_active = false;
                shutdown_animation_height = 0;
                render(
                    &mut display,
                    framebuffer,
                    usb_connected,
                    daemon,
                    page,
                    health_snapshot,
                    shutdown_display,
                );
            }
            // A recent decoded host update is the reliable session signal. The
            // USB SOF indicator is only advisory and may briefly read false even
            // while the serial connection is actively exchanging messages.
            if action == ButtonAction::ShutdownRequested {
                if daemon == DaemonState::Connected {
                    draw_shutdown_progress(
                        &mut display,
                        shutdown_animation_height,
                        DISPLAY_HEIGHT,
                        !shutdown_animation_active,
                    );
                } else if shutdown_animation_active {
                    render(
                        &mut display,
                        framebuffer,
                        usb_connected,
                        daemon,
                        page,
                        health_snapshot,
                        shutdown_display,
                    );
                }
                shutdown_animation_active = false;
                shutdown_animation_height = 0;
            }
            if daemon == DaemonState::Connected && !canceled_shutdown {
                usb_tx.enqueue(DeviceMessage::Button(action));
            }
        } else if daemon == DaemonState::Connected
            && let Some(held) = button.held_for(now)
            && held >= SHUTDOWN_ANIMATION_DELAY
        {
            let height = shutdown_progress_height(held);
            draw_shutdown_progress(
                &mut display,
                shutdown_animation_height,
                height,
                !shutdown_animation_active,
            );
            shutdown_animation_active = true;
            shutdown_animation_height = height;
        } else if shutdown_animation_active {
            shutdown_animation_active = false;
            shutdown_animation_height = 0;
            render(
                &mut display,
                framebuffer,
                usb_connected,
                daemon,
                page,
                health_snapshot,
                shutdown_display,
            );
        }

        if daemon == DaemonState::PoweringOff
            && !matches!(shutdown_display, ShutdownDisplay::Failed { .. })
            && !matches!(shutdown_display, ShutdownDisplay::ConnectionLost { .. })
        {
            if usb_connected {
                shutdown_usb_lost_since = None;
            } else {
                let lost_since = shutdown_usb_lost_since.get_or_insert(now);
                if now - *lost_since >= SHUTDOWN_USB_LOST_CONFIRM {
                    shutdown_display = ShutdownDisplay::ConnectionLost {
                        remaining: shutdown_remaining(shutdown_display),
                    };
                    render(
                        &mut display,
                        framebuffer,
                        usb_connected,
                        daemon,
                        page,
                        health_snapshot,
                        shutdown_display,
                    );
                }
            }
        } else if daemon != DaemonState::PoweringOff {
            shutdown_usb_lost_since = None;
        }

        if daemon == DaemonState::PoweringOff
            && !matches!(shutdown_display, ShutdownDisplay::ReportingLost { .. })
            && !matches!(shutdown_display, ShutdownDisplay::ConnectionLost { .. })
            && !matches!(shutdown_display, ShutdownDisplay::Failed { .. })
            && last_update.is_some_and(|updated| now - updated >= SHUTDOWN_REPORT_TIMEOUT)
        {
            shutdown_display = ShutdownDisplay::ReportingLost {
                remaining: shutdown_remaining(shutdown_display),
            };
            render(
                &mut display,
                framebuffer,
                usb_connected,
                daemon,
                page,
                health_snapshot,
                shutdown_display,
            );
        }

        if daemon == DaemonState::PoweringOff
            && now - last_shutdown_spinner >= SHUTDOWN_SPINNER_INTERVAL
            && let ShutdownDisplay::HostPoweroff { spinner, remaining } = shutdown_display
        {
            shutdown_display = ShutdownDisplay::HostPoweroff {
                spinner: (spinner + 1) % 4,
                remaining,
            };
            last_shutdown_spinner = now;
            render(
                &mut display,
                framebuffer,
                usb_connected,
                daemon,
                page,
                health_snapshot,
                shutdown_display,
            );
        }

        usb_tx.poll(&mut usb);
        delay.delay_millis(10);
    }
}

fn shutdown_progress_height(held: Duration) -> u16 {
    let elapsed = held
        .as_millis()
        .saturating_sub(SHUTDOWN_ANIMATION_DELAY.as_millis());
    let animation_duration = LONG_PRESS.as_millis() - SHUTDOWN_ANIMATION_DELAY.as_millis();
    let height = elapsed.saturating_mul(u64::from(DISPLAY_HEIGHT)) / animation_duration;
    u16::try_from(height.min(u64::from(DISPLAY_HEIGHT))).unwrap_or(DISPLAY_HEIGHT)
}

fn draw_shutdown_progress<D>(display: &mut D, previous_height: u16, height: u16, clear: bool)
where
    D: DrawTarget<Color = Rgb565>,
{
    if clear {
        display.clear(Rgb565::BLACK).ok();
    }
    if height > previous_height {
        Rectangle::new(
            Point::new(0, i32::from(DISPLAY_HEIGHT - height)),
            Size::new(
                u32::from(DISPLAY_WIDTH),
                u32::from(height - previous_height),
            ),
        )
        .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::RED).build())
        .draw(display)
        .ok();
    }

    draw_shutdown_symbol(display);
}

fn draw_shutdown_symbol<D>(display: &mut D)
where
    D: DrawTarget<Color = Rgb565>,
{
    let style = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::WHITE)
        .stroke_width(4)
        .build();
    Arc::new(Point::new(99, 46), 41, -45.0.deg(), 270.0.deg())
        .into_styled(style)
        .draw(display)
        .ok();
    Line::new(Point::new(119, 40), Point::new(119, 66))
        .into_styled(style)
        .draw(display)
        .ok();
}

fn enable_lcd_power(i2c: &mut I2c<'_, esp_hal::Blocking>) -> Result<(), I2cError> {
    // This is the initialization sequence used by M5Stack's M5GFX StickS3
    // driver: GPIO2 function, output mode, push-pull drive, output high.
    update_m5pm1_bits(i2c, 0x16, M5PM1_GPIO2_MASK, false)?;
    update_m5pm1_bits(i2c, 0x10, M5PM1_GPIO2_MASK, true)?;
    update_m5pm1_bits(i2c, 0x13, M5PM1_GPIO2_MASK, false)?;
    update_m5pm1_bits(i2c, 0x11, M5PM1_GPIO2_MASK, true)?;

    // Disable I2C idle sleep. M5PM1 is always powered, so this setting may
    // otherwise persist from software that ran before this firmware.
    i2c.write(M5PM1_ADDRESS, &[0x09, 0x00])
}

fn update_m5pm1_bits(
    i2c: &mut I2c<'_, esp_hal::Blocking>,
    register: u8,
    mask: u8,
    set: bool,
) -> Result<(), I2cError> {
    let mut value = [0_u8];
    i2c.write_read(M5PM1_ADDRESS, &[register], &mut value)?;
    value[0] = if set {
        value[0] | mask
    } else {
        value[0] & !mask
    };
    i2c.write(M5PM1_ADDRESS, &[register, value[0]])
}

fn poll_usb_connection(now: Instant, last_activity: &mut Option<Instant>) -> bool {
    let registers = esp_hal::peripherals::USB_DEVICE::regs();
    if registers.int_raw().read().sof().bit_is_set() {
        registers
            .int_clr()
            .write(|write| write.sof().clear_bit_by_one());
        *last_activity = Some(now);
    }
    last_activity.is_some_and(|activity| now - activity < USB_ACTIVITY_TIMEOUT)
}

fn render<D>(
    display: &mut D,
    framebuffer: &mut ScreenBuffer,
    usb_connected: bool,
    daemon: DaemonState,
    page: u8,
    health_snapshot: Option<HealthSnapshot>,
    shutdown_display: ShutdownDisplay,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let background = Rgb565::new(1, 2, 3);
    framebuffer.clear(background).ok();

    let accent = match daemon {
        DaemonState::PoweringOff => Rgb565::RED,
        DaemonState::Stale => Rgb565::YELLOW,
        DaemonState::Connected => health_snapshot.map_or(Rgb565::CYAN, |snapshot| {
            health_status_color(snapshot.health_status())
        }),
        DaemonState::Waiting => Rgb565::CYAN,
    };
    Rectangle::new(Point::new(0, 0), Size::new(u32::from(DISPLAY_WIDTH), 3))
        .into_styled(PrimitiveStyleBuilder::new().fill_color(accent).build())
        .draw(framebuffer)
        .ok();

    match (daemon, health_snapshot) {
        (DaemonState::PoweringOff, _) => {
            draw_shutdown_status(framebuffer, shutdown_display, accent);
        }
        (DaemonState::Stale, _) => {
            draw_message(framebuffer, "HOST LOST", "Updates stopped", accent);
        }
        (DaemonState::Connected, Some(snapshot)) => {
            match page % PAGE_COUNT {
                0 => draw_overview(framebuffer, &snapshot),
                1 => draw_resources(framebuffer, &snapshot),
                2 => draw_storage_smart(framebuffer, &snapshot),
                3 => draw_ups_network(framebuffer, &snapshot),
                guest_page => draw_guests(framebuffer, &snapshot, guest_page - 4),
            }
            draw_page_dots(framebuffer, page % PAGE_COUNT);
        }
        (DaemonState::Connected, None) => {
            draw_message(framebuffer, "WAITING", "No health snapshot", accent);
        }
        (DaemonState::Waiting, _) if usb_connected => {
            draw_message(framebuffer, "USB READY", "Start host daemon", accent);
        }
        (DaemonState::Waiting, _) => {
            draw_message(framebuffer, "OFFLINE", "Connect USB host", accent);
        }
    }

    Image::new(&framebuffer.as_image(), Point::zero())
        .draw(display)
        .ok();
}

fn draw_header<D>(display: &mut D, snapshot: &HealthSnapshot, title: &str, page: u8)
where
    D: DrawTarget<Color = Rgb565>,
{
    let body = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    let cyan = Rgb565::new(0, 48, 31);
    Text::new(
        &short_host_name(snapshot.host_name()),
        Point::new(4, 13),
        body,
    )
    .draw(display)
    .ok();
    Text::new(
        title,
        Point::new(88, 13),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    let mut page_text = String::<8>::new();
    let _ = write!(&mut page_text, "{}/{}", page + 1, PAGE_COUNT);
    Text::new(&page_text, Point::new(216, 13), body)
        .draw(display)
        .ok();
}

fn draw_overview<D>(display: &mut D, snapshot: &HealthSnapshot)
where
    D: DrawTarget<Color = Rgb565>,
{
    draw_header(display, snapshot, "OVERVIEW", 0);
    draw_card(display, Point::new(4, 19), Size::new(95, 103));
    let status = snapshot.health_status();
    let status_color = health_status_color(status);
    Text::new(
        status.label(),
        Point::new(12, 46),
        MonoTextStyle::new(&FONT_10X20, status_color),
    )
    .draw(display)
    .ok();
    let mut uptime = String::<24>::new();
    let days = snapshot.uptime_seconds / 86_400;
    let hours = snapshot.uptime_seconds % 86_400 / 3_600;
    let _ = write!(&mut uptime, "UP {days}d {hours}h");
    Text::new(
        &uptime,
        Point::new(14, 70),
        MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
    )
    .draw(display)
    .ok();
    if let Some(status_cause) = status.cause() {
        let mut cause = String::<16>::new();
        let _ = write!(&mut cause, "{status_cause}");
        Text::new(
            "CAUSE",
            Point::new(12, 91),
            MonoTextStyle::new(&FONT_6X10, Rgb565::new(0, 48, 31)),
        )
        .draw(display)
        .ok();
        Text::new(
            &cause,
            Point::new(12, 108),
            MonoTextStyle::new(&FONT_6X10, status_color),
        )
        .draw(display)
        .ok();
    }
    let (running, total) = guest_counts(snapshot);

    draw_card(display, Point::new(104, 19), Size::new(132, 31));
    draw_stacked_summary_item(
        display,
        Point::new(109, 23),
        UiIcon::Cpu,
        "HOST",
        &format_host_metrics(snapshot),
        resource_status_color(snapshot),
    );
    draw_card(display, Point::new(104, 55), Size::new(132, 30));
    let network_detail = format_network_summary(snapshot);
    draw_summary_item(
        display,
        Point::new(109, 58),
        UiIcon::Network,
        "NET",
        &format_link(snapshot),
        Some(&network_detail),
        network_color(snapshot),
    );
    draw_card(display, Point::new(104, 90), Size::new(132, 32));
    let mut guest_text = String::<24>::new();
    let _ = write!(&mut guest_text, "{running}/{total} RUN");
    draw_stacked_summary_item(
        display,
        Point::new(109, 94),
        UiIcon::Guests,
        "GUESTS",
        &guest_text,
        Rgb565::GREEN,
    );
}

fn draw_resources<D>(display: &mut D, snapshot: &HealthSnapshot)
where
    D: DrawTarget<Color = Rgb565>,
{
    draw_header(display, snapshot, "RESOURCES", 1);
    let memory = memory_percent(snapshot);
    let mut load = String::<16>::new();
    let _ = write!(
        &mut load,
        "{}.{:02}",
        snapshot.load_average_x100 / 100,
        snapshot.load_average_x100 % 100
    );
    draw_metric_card(
        display,
        Point::new(4, 19),
        UiIcon::Cpu,
        "CPU",
        snapshot.cpu_percent,
        "%",
    );
    draw_metric_card(
        display,
        Point::new(122, 19),
        UiIcon::Memory,
        "MEM",
        memory,
        "%",
    );
    let mut memory_detail = String::<24>::new();
    let _ = write!(
        &mut memory_detail,
        "{}/{}G",
        snapshot.memory_used_mib / 1_024,
        snapshot.memory_total_mib / 1_024
    );
    Text::new(
        &memory_detail,
        Point::new(
            230 - i32::try_from(memory_detail.len()).unwrap_or(0) * 6,
            34,
        ),
        MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
    )
    .draw(display)
    .ok();
    draw_metric_card(
        display,
        Point::new(4, 72),
        UiIcon::Io,
        "IO PRESS",
        snapshot.io_pressure_percent,
        "%",
    );
    draw_value_card(display, Point::new(122, 72), UiIcon::Load, "LOAD", &load);
}

fn draw_storage_smart<D>(display: &mut D, snapshot: &HealthSnapshot)
where
    D: DrawTarget<Color = Rgb565>,
{
    draw_header(display, snapshot, "STORAGE + SMART", 2);
    let cyan = Rgb565::new(0, 48, 31);
    draw_card(display, Point::new(4, 19), Size::new(114, 103));
    draw_icon(display, UiIcon::Disk, Point::new(10, 24), cyan);
    Text::new(
        "FILESYSTEMS",
        Point::new(30, 34),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    draw_storage_row(display, 51, "/", snapshot.root_storage);
    draw_storage_row(display, 69, "HDD", snapshot.hdd_storage);
    draw_storage_row(display, 87, "BACKUP", snapshot.backup_storage);
    let (backup_text, backup_color) = format_backup_job(snapshot);
    Text::new(
        &backup_text,
        Point::new(10, 112),
        MonoTextStyle::new(&FONT_6X10, backup_color),
    )
    .draw(display)
    .ok();

    draw_card(display, Point::new(122, 19), Size::new(114, 103));
    draw_icon(display, UiIcon::Disk, Point::new(128, 24), cyan);
    Text::new(
        "SMART",
        Point::new(148, 34),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    if snapshot.smart.devices().is_empty() {
        Text::new(
            "NOT CONFIGURED",
            Point::new(128, 58),
            MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
        )
        .draw(display)
        .ok();
    } else {
        for (row, device) in snapshot.smart.devices().iter().enumerate() {
            draw_smart_row(display, 51 + i32::try_from(row).unwrap_or(0) * 17, device);
        }
    }
}

fn draw_ups_network<D>(display: &mut D, snapshot: &HealthSnapshot)
where
    D: DrawTarget<Color = Rgb565>,
{
    draw_header(display, snapshot, "UPS + ETHERNET", 3);
    let cyan = Rgb565::new(0, 48, 31);
    let body = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    draw_card(display, Point::new(4, 19), Size::new(114, 103));
    draw_icon(display, UiIcon::Ups, Point::new(10, 24), cyan);
    Text::new(
        "UPS",
        Point::new(30, 34),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        ups_status_text(snapshot),
        Point::new(10, 57),
        MonoTextStyle::new(&FONT_10X20, ups_color(snapshot)),
    )
    .draw(display)
    .ok();
    Text::new(&format_ups_battery(snapshot), Point::new(10, 76), body)
        .draw(display)
        .ok();
    Text::new(&format_ups_load(snapshot), Point::new(10, 92), body)
        .draw(display)
        .ok();
    Text::new(&format_ups_runtime(snapshot), Point::new(10, 108), body)
        .draw(display)
        .ok();

    draw_card(display, Point::new(122, 19), Size::new(114, 103));
    draw_icon(display, UiIcon::Network, Point::new(128, 24), cyan);
    Text::new(
        "ETHERNET",
        Point::new(148, 34),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        if snapshot.network_interface().is_empty() {
            "NO INTERFACE"
        } else {
            snapshot.network_interface()
        },
        Point::new(128, 48),
        MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
    )
    .draw(display)
    .ok();
    let show_last_success = network_has_error(snapshot);
    let link_y = if show_last_success { 68 } else { 78 };
    let status_y = if show_last_success { 82 } else { 92 };
    let ip_y = if show_last_success { 96 } else { 108 };
    let link = format_link(snapshot);
    Text::new(
        &link,
        Point::new(128, link_y),
        MonoTextStyle::new(&FONT_10X20, network_color(snapshot)),
    )
    .draw(display)
    .ok();
    Text::new(
        network_status_text(snapshot),
        Point::new(128, status_y),
        MonoTextStyle::new(&FONT_6X10, network_color(snapshot)),
    )
    .draw(display)
    .ok();
    Text::new("IP", Point::new(128, ip_y), body)
        .draw(display)
        .ok();
    Text::new(
        &format_ipv4(snapshot.ipv4),
        Point::new(146, ip_y),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    if show_last_success {
        Text::new(
            &format_last_internet_success(snapshot.last_internet_success_age_seconds),
            Point::new(128, 112),
            MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
        )
        .draw(display)
        .ok();
    }
}

fn draw_smart_row<D>(display: &mut D, baseline: i32, device: &SmartDeviceSummary)
where
    D: DrawTarget<Color = Rgb565>,
{
    let color = smart_color(device.status);
    Text::new(
        device.label(),
        Point::new(128, baseline),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
    let detail = format_smart_detail(device);
    Text::new(
        &detail,
        Point::new(230 - i32::try_from(detail.len()).unwrap_or(0) * 6, baseline),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
}

fn format_smart_detail(device: &SmartDeviceSummary) -> String<12> {
    let mut text = String::new();
    let _ = text.push_str(match device.status {
        SmartStatus::Healthy => "OK",
        SmartStatus::Warning => "WARN",
        SmartStatus::Failed => "FAIL",
        SmartStatus::Sleeping => "SLEEP",
        SmartStatus::Unknown => "UNK",
    });
    if let Some(temperature) = device.temperature_celsius {
        let _ = write!(&mut text, " {temperature}C");
    } else if device.status == SmartStatus::Sleeping {
        let _ = text.push_str(" --");
    }
    text
}

const fn smart_color(status: SmartStatus) -> Rgb565 {
    match status {
        SmartStatus::Healthy => Rgb565::GREEN,
        SmartStatus::Sleeping => Rgb565::CYAN,
        SmartStatus::Warning | SmartStatus::Unknown => Rgb565::YELLOW,
        SmartStatus::Failed => Rgb565::RED,
    }
}

fn ups_status_text(snapshot: &HealthSnapshot) -> &'static str {
    if snapshot.ups.stale && snapshot.ups.status != UpsStatus::Unavailable {
        return "STALE";
    }
    match snapshot.ups.status {
        UpsStatus::NotConfigured => "NO UPS",
        UpsStatus::Unknown | UpsStatus::Unavailable => "NO DATA",
        UpsStatus::Online => "ONLINE",
        UpsStatus::OnBattery => "ON BATTERY",
        UpsStatus::LowBattery => "LOW BATTERY",
        UpsStatus::Charging => "CHARGING",
        UpsStatus::Bypass => "BYPASS",
        UpsStatus::OutputOff => "OUTPUT OFF",
        UpsStatus::ReplaceBattery => "REPLACE BAT",
    }
}

fn ups_color(snapshot: &HealthSnapshot) -> Rgb565 {
    match snapshot.ups.status {
        UpsStatus::Online | UpsStatus::Charging => Rgb565::GREEN,
        UpsStatus::NotConfigured => Rgb565::WHITE,
        UpsStatus::OnBattery | UpsStatus::Bypass | UpsStatus::Unknown | UpsStatus::Unavailable => {
            Rgb565::YELLOW
        }
        UpsStatus::LowBattery | UpsStatus::OutputOff | UpsStatus::ReplaceBattery => Rgb565::RED,
    }
}

fn format_ups_battery(snapshot: &HealthSnapshot) -> String<16> {
    let mut text = String::new();
    match snapshot.ups.battery_percent {
        Some(percent) => {
            let _ = write!(&mut text, "BAT       {percent}%");
        }
        None => {
            let _ = text.push_str("BAT        --");
        }
    }
    text
}

fn format_ups_load(snapshot: &HealthSnapshot) -> String<20> {
    let mut text = String::new();
    match (snapshot.ups.load_percent, snapshot.ups.estimated_watts) {
        (Some(load), Some(watts)) => {
            let _ = write!(&mut text, "LOAD {load}% ~{watts}W");
        }
        (Some(load), None) => {
            let _ = write!(&mut text, "LOAD      {load}%");
        }
        (None, _) => {
            let _ = text.push_str("LOAD       --");
        }
    }
    text
}

fn format_ups_runtime(snapshot: &HealthSnapshot) -> String<20> {
    let mut text = String::new();
    match snapshot.ups.runtime_seconds {
        Some(seconds) if seconds >= 3_600 => {
            let _ = write!(
                &mut text,
                "RUNTIME {}h{}m",
                seconds / 3_600,
                seconds % 3_600 / 60
            );
        }
        Some(seconds) => {
            let _ = write!(&mut text, "RUNTIME    {}m", seconds / 60);
        }
        None => {
            let _ = text.push_str("RUNTIME    --");
        }
    }
    text
}

fn draw_storage_row<D>(display: &mut D, baseline: i32, label: &str, usage: FilesystemUsage)
where
    D: DrawTarget<Color = Rgb565>,
{
    let color = storage_color(usage);
    Text::new(
        label,
        Point::new(10, baseline),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
    if !usage.mounted {
        Text::new(
            "MISSING",
            Point::new(70, baseline),
            MonoTextStyle::new(&FONT_6X10, Rgb565::RED),
        )
        .draw(display)
        .ok();
        return;
    }

    // FONT_6X10's uppercase glyphs span from baseline - 7 through the baseline.
    // A six-pixel bar at baseline - 6 shares the same visual center.
    draw_storage_bar(
        display,
        Point::new(50, baseline - 6),
        32,
        usage.used_percent,
        color,
    );
    let available = format_available_storage(usage.available_mib);
    Text::new(
        &available,
        Point::new(
            112 - i32::try_from(available.len()).unwrap_or(0) * 6,
            baseline,
        ),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
}

fn format_available_storage(available_mib: u32) -> String<8> {
    let mut available = String::new();
    if available_mib >= 1_048_576 {
        let tenths = u64::from(available_mib).saturating_mul(10) / 1_048_576;
        let _ = write!(&mut available, "{}.{}T", tenths / 10, tenths % 10);
    } else if available_mib >= 1_024 {
        let gib = (u64::from(available_mib) + 512) / 1_024;
        let _ = write!(&mut available, "{gib}G");
    } else {
        let _ = write!(&mut available, "{available_mib}M");
    }
    available
}

fn format_backup_job(snapshot: &HealthSnapshot) -> (String<24>, Rgb565) {
    let mut text = String::new();
    let color = match snapshot.backup_job_status {
        BackupJobStatus::Healthy => {
            let _ = text.push_str("BACKUP OK ");
            let _ = write_backup_age(
                &mut text,
                snapshot.last_successful_backup_age_seconds,
                " AGO",
            );
            Rgb565::GREEN
        }
        BackupJobStatus::Running => {
            let _ = text.push_str("BACKUP RUNNING");
            if snapshot
                .last_successful_backup_age_seconds
                .is_none_or(|age| age > 24 * 60 * 60)
            {
                Rgb565::YELLOW
            } else {
                Rgb565::CYAN
            }
        }
        BackupJobStatus::Failed => {
            let _ = text.push_str("BACKUP FAILED");
            Rgb565::YELLOW
        }
        BackupJobStatus::Stale => {
            let _ = text.push_str("BACKUP ");
            if snapshot.last_successful_backup_age_seconds.is_some() {
                let _ = write_backup_age(
                    &mut text,
                    snapshot.last_successful_backup_age_seconds,
                    " OLD",
                );
            } else {
                let _ = text.push_str("OVERDUE");
            }
            Rgb565::YELLOW
        }
        BackupJobStatus::NoJob => {
            let _ = text.push_str("BACKUP NO JOB");
            Rgb565::YELLOW
        }
        BackupJobStatus::Unknown => {
            let _ = text.push_str("BACKUP UNKNOWN");
            Rgb565::RED
        }
    };
    (text, color)
}

fn write_backup_age(text: &mut String<24>, age: Option<u32>, suffix: &str) -> core::fmt::Result {
    let seconds = age.unwrap_or(0);
    if seconds >= 2 * 86_400 {
        write!(text, "{}d{suffix}", seconds / 86_400)
    } else if seconds >= 3_600 {
        write!(text, "{}h{suffix}", seconds / 3_600)
    } else {
        write!(text, "{}m{suffix}", seconds / 60)
    }
}

fn storage_color(usage: FilesystemUsage) -> Rgb565 {
    if !usage.mounted || usage.used_percent > 90 {
        Rgb565::RED
    } else {
        Rgb565::GREEN
    }
}

fn draw_storage_bar<D>(display: &mut D, origin: Point, width: u32, percent: u8, color: Rgb565)
where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(origin, Size::new(width, 6))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Rgb565::new(12, 25, 24))
                .stroke_width(1)
                .build(),
        )
        .draw(display)
        .ok();
    let filled = width.saturating_sub(2) * u32::from(percent) / 100;
    if filled > 0 {
        Rectangle::new(origin + Point::new(1, 1), Size::new(filled, 4))
            .into_styled(PrimitiveStyleBuilder::new().fill_color(color).build())
            .draw(display)
            .ok();
    }
}

fn draw_guests<D>(display: &mut D, snapshot: &HealthSnapshot, guest_page: u8)
where
    D: DrawTarget<Color = Rgb565>,
{
    let page = usize::from(guest_page.min(1));
    draw_header(
        display,
        snapshot,
        if page == 0 {
            "GUESTS 1/2"
        } else {
            "GUESTS 2/2"
        },
        4 + u8::try_from(page).unwrap_or(0),
    );
    let cyan = Rgb565::new(0, 48, 31);
    let body = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    let (running, total) = guest_counts(snapshot);
    let mut count = String::<16>::new();
    let _ = write!(&mut count, "{running}/{total} RUN");
    draw_icon(display, UiIcon::Guests, Point::new(8, 20), cyan);
    Text::new(
        &count,
        Point::new(28, 31),
        MonoTextStyle::new(&FONT_6X10, Rgb565::GREEN),
    )
    .draw(display)
    .ok();
    Text::new(
        "STATUS",
        Point::new(143, 31),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        "CPU",
        Point::new(207, 31),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    for (row, guest) in snapshot
        .guests
        .guests()
        .iter()
        .skip(page * GUESTS_PER_PAGE)
        .take(GUESTS_PER_PAGE)
        .enumerate()
    {
        let y = 49 + i32::try_from(row).unwrap_or(0) * 22;
        let color = if guest.status == GuestStatus::Running {
            Rgb565::GREEN
        } else {
            Rgb565::RED
        };
        draw_icon(
            display,
            match guest.kind {
                GuestKind::VirtualMachine => UiIcon::Qemu,
                GuestKind::Container => UiIcon::Container,
            },
            Point::new(5, y - 12),
            cyan,
        );
        Text::new(&short_name(guest.name()), Point::new(25, y), body)
            .draw(display)
            .ok();
        Text::new(
            if guest.status == GuestStatus::Running {
                "RUN"
            } else {
                "STOP"
            },
            Point::new(143, y),
            MonoTextStyle::new(&FONT_6X10, color),
        )
        .draw(display)
        .ok();
        let mut cpu = String::<8>::new();
        let _ = write!(&mut cpu, "{}%", guest.cpu_percent);
        Text::new(&cpu, Point::new(204, y), body).draw(display).ok();
        if row < 3 {
            Line::new(Point::new(8, y + 7), Point::new(231, y + 7))
                .into_styled(
                    PrimitiveStyleBuilder::new()
                        .stroke_color(Rgb565::new(8, 18, 18))
                        .build(),
                )
                .draw(display)
                .ok();
        }
    }
}

fn draw_metric_card<D>(
    display: &mut D,
    origin: Point,
    icon: UiIcon,
    label: &str,
    value: u8,
    suffix: &str,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let mut text = String::<12>::new();
    let _ = write!(&mut text, "{value}{suffix}");
    draw_value_card(display, origin, icon, label, &text);
}

fn draw_value_card<D>(display: &mut D, origin: Point, icon: UiIcon, label: &str, value: &str)
where
    D: DrawTarget<Color = Rgb565>,
{
    draw_card(display, origin, Size::new(114, 50));
    let cyan = Rgb565::new(0, 48, 31);
    draw_icon(display, icon, origin + Point::new(7, 5), cyan);
    Text::new(
        label,
        origin + Point::new(27, 15),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        value,
        origin + Point::new(49, 39),
        MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN),
    )
    .draw(display)
    .ok();
}

fn draw_summary_item<D>(
    display: &mut D,
    origin: Point,
    icon: UiIcon,
    label: &str,
    value: &str,
    detail: Option<&str>,
    color: Rgb565,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let cyan = Rgb565::new(0, 48, 31);
    draw_icon(display, icon, origin, cyan);
    Text::new(
        label,
        origin + Point::new(20, 9),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        value,
        origin + Point::new(56, 9),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
    if let Some(detail) = detail {
        Text::new(
            detail,
            origin + Point::new(20, 21),
            MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
        )
        .draw(display)
        .ok();
    }
}

fn draw_stacked_summary_item<D>(
    display: &mut D,
    origin: Point,
    icon: UiIcon,
    label: &str,
    value: &str,
    color: Rgb565,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let cyan = Rgb565::new(0, 48, 31);
    draw_icon(display, icon, origin, cyan);
    Text::new(
        label,
        origin + Point::new(20, 9),
        MonoTextStyle::new(&FONT_6X10, cyan),
    )
    .draw(display)
    .ok();
    Text::new(
        value,
        origin + Point::new(20, 21),
        MonoTextStyle::new(&FONT_6X10, color),
    )
    .draw(display)
    .ok();
}

fn draw_icon<D>(display: &mut D, icon: UiIcon, origin: Point, color: Rgb565)
where
    D: DrawTarget<Color = Rgb565>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(color)
        .stroke_width(1)
        .build();
    let fill = PrimitiveStyleBuilder::new().fill_color(color).build();
    match icon {
        UiIcon::Cpu => {
            Rectangle::new(origin + Point::new(3, 3), Size::new(9, 9))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Rectangle::new(origin + Point::new(6, 6), Size::new(3, 3))
                .into_styled(fill)
                .draw(display)
                .ok();
            for offset in [4, 7, 10] {
                Line::new(
                    origin + Point::new(offset, 1),
                    origin + Point::new(offset, 3),
                )
                .into_styled(stroke)
                .draw(display)
                .ok();
                Line::new(
                    origin + Point::new(offset, 12),
                    origin + Point::new(offset, 14),
                )
                .into_styled(stroke)
                .draw(display)
                .ok();
                Line::new(
                    origin + Point::new(1, offset),
                    origin + Point::new(3, offset),
                )
                .into_styled(stroke)
                .draw(display)
                .ok();
                Line::new(
                    origin + Point::new(12, offset),
                    origin + Point::new(14, offset),
                )
                .into_styled(stroke)
                .draw(display)
                .ok();
            }
        }
        UiIcon::Memory => {
            Rectangle::new(origin + Point::new(1, 3), Size::new(14, 9))
                .into_styled(stroke)
                .draw(display)
                .ok();
            for x in [3, 7, 11] {
                Rectangle::new(origin + Point::new(x, 5), Size::new(3, 4))
                    .into_styled(fill)
                    .draw(display)
                    .ok();
            }
            for x in [3, 6, 9, 12] {
                Line::new(origin + Point::new(x, 12), origin + Point::new(x, 14))
                    .into_styled(stroke)
                    .draw(display)
                    .ok();
            }
        }
        UiIcon::Disk => {
            Rectangle::new(origin + Point::new(2, 1), Size::new(12, 14))
                .into_styled(stroke)
                .draw(display)
                .ok();
            for y in [5, 10] {
                Line::new(origin + Point::new(2, y), origin + Point::new(13, y))
                    .into_styled(stroke)
                    .draw(display)
                    .ok();
            }
            Rectangle::new(origin + Point::new(10, 12), Size::new(2, 2))
                .into_styled(fill)
                .draw(display)
                .ok();
        }
        UiIcon::Ups => {
            Rectangle::new(origin + Point::new(2, 3), Size::new(12, 11))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Rectangle::new(origin + Point::new(6, 1), Size::new(4, 2))
                .into_styled(fill)
                .draw(display)
                .ok();
            Rectangle::new(origin + Point::new(5, 6), Size::new(6, 5))
                .into_styled(fill)
                .draw(display)
                .ok();
        }
        UiIcon::Network => {
            Rectangle::new(origin + Point::new(5, 5), Size::new(6, 5))
                .into_styled(stroke)
                .draw(display)
                .ok();
            for (start, end) in [
                (Point::new(8, 5), Point::new(8, 2)),
                (Point::new(5, 8), Point::new(2, 8)),
                (Point::new(11, 8), Point::new(14, 8)),
                (Point::new(8, 10), Point::new(8, 13)),
            ] {
                Line::new(origin + start, origin + end)
                    .into_styled(stroke)
                    .draw(display)
                    .ok();
            }
            for point in [
                Point::new(6, 0),
                Point::new(0, 6),
                Point::new(13, 6),
                Point::new(6, 13),
            ] {
                Rectangle::new(origin + point, Size::new(4, 3))
                    .into_styled(fill)
                    .draw(display)
                    .ok();
            }
        }
        UiIcon::Guests => {
            Rectangle::new(origin + Point::new(1, 1), Size::new(14, 10))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(5, 13), origin + Point::new(11, 13))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(8, 11), origin + Point::new(8, 13))
                .into_styled(stroke)
                .draw(display)
                .ok();
        }
        UiIcon::Qemu => {
            Rectangle::new(origin + Point::new(1, 1), Size::new(14, 10))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Rectangle::new(origin + Point::new(4, 4), Size::new(2, 2))
                .into_styled(fill)
                .draw(display)
                .ok();
            Rectangle::new(origin + Point::new(8, 4), Size::new(2, 2))
                .into_styled(fill)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(5, 13), origin + Point::new(11, 13))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(8, 11), origin + Point::new(8, 13))
                .into_styled(stroke)
                .draw(display)
                .ok();
        }
        UiIcon::Container => {
            Rectangle::new(origin + Point::new(1, 2), Size::new(14, 12))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(1, 6), origin + Point::new(14, 6))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(5, 2), origin + Point::new(5, 14))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(10, 2), origin + Point::new(10, 14))
                .into_styled(stroke)
                .draw(display)
                .ok();
        }
        UiIcon::Io => {
            Line::new(origin + Point::new(4, 13), origin + Point::new(4, 2))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(4, 2), origin + Point::new(1, 5))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(4, 2), origin + Point::new(7, 5))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(11, 2), origin + Point::new(11, 13))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(11, 13), origin + Point::new(8, 10))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(11, 13), origin + Point::new(14, 10))
                .into_styled(stroke)
                .draw(display)
                .ok();
        }
        UiIcon::Load => {
            Line::new(origin + Point::new(1, 1), origin + Point::new(1, 14))
                .into_styled(stroke)
                .draw(display)
                .ok();
            Line::new(origin + Point::new(1, 14), origin + Point::new(15, 14))
                .into_styled(stroke)
                .draw(display)
                .ok();
            for (start, end) in [
                (Point::new(2, 11), Point::new(6, 7)),
                (Point::new(6, 7), Point::new(9, 9)),
                (Point::new(9, 9), Point::new(14, 3)),
            ] {
                Line::new(origin + start, origin + end)
                    .into_styled(stroke)
                    .draw(display)
                    .ok();
            }
        }
    }
}

fn draw_card<D>(display: &mut D, origin: Point, size: Size)
where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(origin, size)
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Rgb565::new(12, 25, 24))
                .stroke_width(1)
                .build(),
        )
        .draw(display)
        .ok();
}

fn draw_page_dots<D>(display: &mut D, page: u8)
where
    D: DrawTarget<Color = Rgb565>,
{
    for index in 0..i32::from(PAGE_COUNT) {
        Rectangle::new(Point::new(92 + index * 10, 129), Size::new(5, 3))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(if index == i32::from(page) {
                        Rgb565::GREEN
                    } else {
                        Rgb565::new(14, 22, 20)
                    })
                    .build(),
            )
            .draw(display)
            .ok();
    }
}

fn draw_message<D>(display: &mut D, title: &str, detail: &str, color: Rgb565)
where
    D: DrawTarget<Color = Rgb565>,
{
    Text::new(
        title,
        Point::new(20, 55),
        MonoTextStyle::new(&FONT_10X20, color),
    )
    .draw(display)
    .ok();
    Text::new(
        detail,
        Point::new(22, 79),
        MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE),
    )
    .draw(display)
    .ok();
}

fn draw_shutdown_status<D>(display: &mut D, status: ShutdownDisplay, color: Rgb565)
where
    D: DrawTarget<Color = Rgb565>,
{
    match status {
        ShutdownDisplay::Accepted => {
            draw_message(display, "SHUTTING DOWN", "Preparing guests...", color);
        }
        ShutdownDisplay::Guests { total, remaining } => {
            let mut detail = String::<32>::new();
            let _ = write!(&mut detail, "{remaining} of {total} remaining");
            draw_message(display, "STOPPING GUESTS", &detail, color);
            draw_guest_shutdown_bar(display, total, remaining);
        }
        ShutdownDisplay::GuestsStopped => {
            draw_message(
                display,
                "GUESTS STOPPED",
                "Host shutdown next",
                Rgb565::GREEN,
            );
        }
        ShutdownDisplay::HostPoweroff { spinner, remaining } => {
            let mut detail = String::<32>::new();
            if remaining == 0 {
                let _ = detail.push_str("Stopping services...");
            } else {
                let _ = write!(&mut detail, "{remaining} guests handed off");
            }
            draw_message(display, "HOST SHUTDOWN", &detail, color);
            draw_shutdown_spinner(display, spinner);
        }
        ShutdownDisplay::ReportingLost { remaining } => {
            let mut detail = String::<32>::new();
            let _ = write!(&mut detail, "USB active, last count {remaining}");
            draw_message(display, "REPORTING LOST", &detail, Rgb565::YELLOW);
        }
        ShutdownDisplay::ConnectionLost { remaining } => {
            let mut detail = String::<32>::new();
            let _ = write!(&mut detail, "USB link down, last {remaining}");
            draw_message(display, "CONNECTION LOST", &detail, Rgb565::YELLOW);
        }
        ShutdownDisplay::Failed { reason, remaining } => {
            let mut detail = String::<32>::new();
            match reason {
                ShutdownFailure::GuestQuery => {
                    let _ = detail.push_str("Could not query guests");
                }
                ShutdownFailure::GuestShutdown => {
                    let _ = write!(&mut detail, "{remaining} guests still running");
                }
                ShutdownFailure::HostPoweroff => {
                    let _ = detail.push_str("Could not power off host");
                }
            }
            draw_message(display, "SHUTDOWN FAILED", &detail, color);
        }
    }
}

fn shutdown_remaining(status: ShutdownDisplay) -> u16 {
    match status {
        ShutdownDisplay::Guests { remaining, .. }
        | ShutdownDisplay::HostPoweroff { remaining, .. }
        | ShutdownDisplay::ReportingLost { remaining }
        | ShutdownDisplay::ConnectionLost { remaining }
        | ShutdownDisplay::Failed { remaining, .. } => remaining,
        ShutdownDisplay::Accepted | ShutdownDisplay::GuestsStopped => 0,
    }
}

fn draw_guest_shutdown_bar<D>(display: &mut D, total: u16, remaining: u16)
where
    D: DrawTarget<Color = Rgb565>,
{
    let border = Rectangle::new(Point::new(20, 94), Size::new(200, 12));
    border
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Rgb565::WHITE)
                .stroke_width(1)
                .build(),
        )
        .draw(display)
        .ok();

    let stopped = total.saturating_sub(remaining.min(total));
    let width = if total == 0 {
        198
    } else {
        u32::from(stopped).saturating_mul(198) / u32::from(total)
    };
    if width > 0 {
        Rectangle::new(Point::new(21, 95), Size::new(width, 10))
            .into_styled(PrimitiveStyleBuilder::new().fill_color(Rgb565::RED).build())
            .draw(display)
            .ok();
    }
}

fn draw_shutdown_spinner<D>(display: &mut D, frame: u8)
where
    D: DrawTarget<Color = Rgb565>,
{
    for (index, point) in [
        Point::new(117, 91),
        Point::new(129, 103),
        Point::new(117, 115),
        Point::new(105, 103),
    ]
    .iter()
    .enumerate()
    {
        Rectangle::new(*point, Size::new(7, 7))
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(if index == usize::from(frame) {
                        Rgb565::RED
                    } else {
                        Rgb565::new(8, 14, 13)
                    })
                    .build(),
            )
            .draw(display)
            .ok();
    }
}

const fn health_status_color(status: HealthStatus) -> Rgb565 {
    match status {
        HealthStatus::Healthy => Rgb565::GREEN,
        HealthStatus::Warning(_) => Rgb565::YELLOW,
        HealthStatus::Critical(_) => Rgb565::RED,
    }
}

fn resource_status_color(snapshot: &HealthSnapshot) -> Rgb565 {
    usage_color(snapshot.cpu_percent.max(memory_percent(snapshot)))
}

fn usage_color(percent: u8) -> Rgb565 {
    if percent >= 95 {
        Rgb565::RED
    } else if percent >= 85 {
        Rgb565::YELLOW
    } else {
        Rgb565::GREEN
    }
}

fn memory_percent(snapshot: &HealthSnapshot) -> u8 {
    if snapshot.memory_total_mib == 0 {
        0
    } else {
        u8::try_from(
            u64::from(snapshot.memory_used_mib).saturating_mul(100)
                / u64::from(snapshot.memory_total_mib),
        )
        .unwrap_or(100)
    }
}

fn guest_counts(snapshot: &HealthSnapshot) -> (usize, usize) {
    let guests = snapshot.guests.guests();
    (
        guests
            .iter()
            .filter(|guest| guest.status == GuestStatus::Running)
            .count(),
        guests.len(),
    )
}

fn format_host_metrics(snapshot: &HealthSnapshot) -> String<48> {
    let mut text = String::new();
    let _ = write!(
        &mut text,
        "CPU {}% MEM {}%",
        snapshot.cpu_percent,
        memory_percent(snapshot)
    );
    text
}

fn format_link(snapshot: &HealthSnapshot) -> String<24> {
    let mut text = String::new();
    if !snapshot.network_up {
        let _ = text.push_str("DOWN");
    } else if snapshot.network_mbps >= 1_000 {
        let gigabits = snapshot.network_mbps / 1_000;
        let remainder = snapshot.network_mbps % 1_000;
        if remainder == 0 {
            let _ = write!(&mut text, "{gigabits}G UP");
        } else {
            let _ = write!(&mut text, "{gigabits}.{}G UP", remainder / 100);
        }
    } else if snapshot.network_mbps > 0 {
        let _ = write!(&mut text, "{}M UP", snapshot.network_mbps);
    } else {
        let _ = text.push_str("UP");
    }
    text
}

fn network_color(snapshot: &HealthSnapshot) -> Rgb565 {
    if !snapshot.network_up || snapshot.internet_status == InternetStatus::Failed {
        Rgb565::RED
    } else if snapshot.internet_status == InternetStatus::Missed {
        Rgb565::YELLOW
    } else {
        Rgb565::GREEN
    }
}

fn network_has_error(snapshot: &HealthSnapshot) -> bool {
    !snapshot.network_up
        || matches!(
            snapshot.internet_status,
            InternetStatus::Missed | InternetStatus::Failed
        )
}

fn network_status_text(snapshot: &HealthSnapshot) -> &'static str {
    if !snapshot.network_up {
        "LINK DOWN"
    } else {
        internet_status_text(snapshot.internet_status)
    }
}

const fn internet_status_text(status: InternetStatus) -> &'static str {
    match status {
        InternetStatus::Checking => "CHECKING",
        InternetStatus::Reachable => "ONLINE",
        InternetStatus::Missed => "PING MISSED",
        InternetStatus::Failed => "PING FAILED",
    }
}

fn format_network_summary(snapshot: &HealthSnapshot) -> String<16> {
    if !snapshot.network_up {
        let mut text = String::new();
        let _ = text.push_str("LINK DOWN");
        return text;
    }
    if snapshot.internet_status == InternetStatus::Reachable {
        return format_ipv4(snapshot.ipv4);
    }
    let mut text = String::new();
    let _ = text.push_str(internet_status_text(snapshot.internet_status));
    text
}

fn format_last_internet_success(age_seconds: Option<u32>) -> String<20> {
    let mut text = String::new();
    let Some(seconds) = age_seconds else {
        let _ = text.push_str("LAST OK NEVER");
        return text;
    };
    if seconds < 60 {
        let _ = write!(&mut text, "LAST OK {seconds}s AGO");
    } else if seconds < 3_600 {
        let _ = write!(&mut text, "LAST OK {}m AGO", seconds / 60);
    } else if seconds < 86_400 {
        let _ = write!(
            &mut text,
            "LAST OK {}h{}m AGO",
            seconds / 3_600,
            seconds % 3_600 / 60
        );
    } else {
        let _ = write!(&mut text, "LAST OK {}d AGO", seconds / 86_400);
    }
    text
}

fn format_ipv4(address: [u8; 4]) -> String<16> {
    let mut text = String::new();
    if address == [0; 4] {
        let _ = text.push_str("NO ADDRESS");
    } else {
        let _ = write!(
            &mut text,
            "{}.{}.{}.{}",
            address[0], address[1], address[2], address[3]
        );
    }
    text
}

fn short_name(name: &str) -> String<17> {
    name.chars().take(16).collect()
}

fn short_host_name(name: &str) -> String<11> {
    name.chars().take(10).collect()
}
