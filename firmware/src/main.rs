#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10, ascii::FONT_10X20},
    pixelcolor::Rgb565,
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
use mipidsi::{Builder, interface::SpiInterface, models::ST7789, options::ColorInversion};
use s3_display_protocol::{
    ButtonAction, DeviceMessage, FrameDecoder, GuestKind, GuestSnapshot, GuestStatus, HostMessage,
    MAX_FRAME_LEN, Sequence, decode_host, encode_device,
};

esp_bootloader_esp_idf::esp_app_desc!();

const DISPLAY_WIDTH: u16 = 135;
const DISPLAY_HEIGHT: u16 = 240;
// M5Stack does not publish controller RAM offsets; these match this 135x240 panel family.
const DISPLAY_OFFSET_X: u16 = 52;
const DISPLAY_OFFSET_Y: u16 = 40;
const DEBOUNCE: Duration = Duration::from_millis(30);
const SHUTDOWN_ANIMATION_DELAY: Duration = Duration::from_millis(200);
const LONG_PRESS: Duration = Duration::from_millis(3_000);
const LINK_TIMEOUT: Duration = Duration::from_secs(15);
const USB_ACTIVITY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_DEVICE_FRAME_LEN: usize = 64;
const M5PM1_ADDRESS: u8 = 0x6e;
const M5PM1_GPIO2_MASK: u8 = 1 << 2;

#[derive(Clone, Copy, Eq, PartialEq)]
enum DaemonState {
    Waiting,
    Connected,
    Stale,
    PoweringOff,
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
        .display_size(DISPLAY_WIDTH, DISPLAY_HEIGHT)
        .display_offset(DISPLAY_OFFSET_X, DISPLAY_OFFSET_Y)
        .invert_colors(ColorInversion::Inverted)
        .reset_pin(reset)
        .init(&mut delay)
        .expect("display initialization");
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
    let mut last_sequence = None;
    let mut guest_snapshot = None;
    let mut page = 0_u8;
    let mut shutdown_animation_active = false;
    let mut shutdown_animation_height = 0_u16;

    // Render before attempting any USB traffic so offline operation always works.
    render(
        &mut display,
        usb_connected,
        daemon,
        page,
        last_sequence,
        guest_snapshot,
    );
    usb_tx.enqueue(DeviceMessage::Ready);

    loop {
        let now = Instant::now();
        let was_usb_connected = usb_connected;
        usb_connected = poll_usb_connection(now, &mut last_usb_activity);
        if !usb_connected && daemon == DaemonState::Connected {
            daemon = DaemonState::Stale;
        }
        if usb_connected != was_usb_connected && !shutdown_animation_active {
            render(
                &mut display,
                usb_connected,
                daemon,
                page,
                last_sequence,
                guest_snapshot,
            );
        }

        while let Ok(byte) = usb.read_byte() {
            let Some(Ok(frame)) = decoder.push(byte) else {
                continue;
            };
            match decode_host(frame) {
                Ok(HostMessage::Update { sequence, .. }) => {
                    last_update = Some(now);
                    last_sequence = Some(sequence);
                    if daemon != DaemonState::PoweringOff {
                        daemon = DaemonState::Connected;
                    }
                    usb_tx.enqueue(DeviceMessage::Ack { sequence });
                    if !shutdown_animation_active {
                        render(
                            &mut display,
                            usb_connected,
                            daemon,
                            page,
                            last_sequence,
                            guest_snapshot,
                        );
                    }
                }
                Ok(HostMessage::GuestSnapshot {
                    sequence, snapshot, ..
                }) => {
                    last_update = Some(now);
                    last_sequence = Some(sequence);
                    guest_snapshot = Some(snapshot);
                    if daemon != DaemonState::PoweringOff {
                        daemon = DaemonState::Connected;
                    }
                    usb_tx.enqueue(DeviceMessage::Ack { sequence });
                    if !shutdown_animation_active {
                        render(
                            &mut display,
                            usb_connected,
                            daemon,
                            page,
                            last_sequence,
                            guest_snapshot,
                        );
                    }
                }
                Ok(HostMessage::ShutdownAccepted) => {
                    daemon = DaemonState::PoweringOff;
                    render(
                        &mut display,
                        usb_connected,
                        daemon,
                        page,
                        last_sequence,
                        guest_snapshot,
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
                    usb_connected,
                    daemon,
                    page,
                    last_sequence,
                    guest_snapshot,
                );
            }
        }

        if let Some(action) = button.update(button_pin.is_low(), now) {
            let canceled_shutdown = action == ButtonAction::NextScreen && shutdown_animation_active;
            if action == ButtonAction::NextScreen {
                if !canceled_shutdown {
                    page = (page + 1) % 2;
                }
                shutdown_animation_active = false;
                shutdown_animation_height = 0;
                render(
                    &mut display,
                    usb_connected,
                    daemon,
                    page,
                    last_sequence,
                    guest_snapshot,
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
                        usb_connected,
                        daemon,
                        page,
                        last_sequence,
                        guest_snapshot,
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
                usb_connected,
                daemon,
                page,
                last_sequence,
                guest_snapshot,
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
    Arc::new(Point::new(47, 100), 41, -45.0.deg(), 270.0.deg())
        .into_styled(style)
        .draw(display)
        .ok();
    Line::new(Point::new(67, 94), Point::new(67, 120))
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
    usb_connected: bool,
    daemon: DaemonState,
    page: u8,
    sequence: Option<Sequence>,
    guest_snapshot: Option<GuestSnapshot>,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let background = Rgb565::new(1, 2, 3);
    display.clear(background).ok();

    let accent = match daemon {
        DaemonState::Connected if usb_connected => Rgb565::GREEN,
        DaemonState::PoweringOff => Rgb565::RED,
        DaemonState::Stale => Rgb565::YELLOW,
        DaemonState::Connected | DaemonState::Waiting => Rgb565::CYAN,
    };
    Rectangle::new(Point::new(0, 0), Size::new(u32::from(DISPLAY_WIDTH), 6))
        .into_styled(PrimitiveStyleBuilder::new().fill_color(accent).build())
        .draw(display)
        .ok();

    let heading = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let body = MonoTextStyle::new(&FONT_6X10, Rgb565::new(20, 45, 28));
    Text::new("S3 DISPLAY", Point::new(8, 34), heading)
        .draw(display)
        .ok();
    let mut page_text = String::<8>::new();
    let _ = write!(&mut page_text, "{}/2", page + 1);
    Text::new(&page_text, Point::new(108, 31), body)
        .draw(display)
        .ok();

    draw_indicator(
        display,
        54,
        if usb_connected {
            Rgb565::GREEN
        } else {
            Rgb565::RED
        },
        if usb_connected {
            "USB connected"
        } else {
            "USB disconnected"
        },
        body,
    );
    let (daemon_color, daemon_text) = match daemon {
        DaemonState::Connected => (Rgb565::GREEN, "Daemon connected"),
        DaemonState::Stale => (Rgb565::YELLOW, "Daemon stale"),
        DaemonState::PoweringOff => (Rgb565::RED, "Shutdown accepted"),
        DaemonState::Waiting => (Rgb565::RED, "Daemon disconnected"),
    };
    draw_indicator(display, 74, daemon_color, daemon_text, body);

    let (title, detail) = match daemon {
        DaemonState::PoweringOff => ("POWERING OFF", "Clean shutdown\nwas accepted."),
        DaemonState::Connected if usb_connected && guest_snapshot.is_some() => ("GUESTS", ""),
        DaemonState::Connected if usb_connected => {
            if page == 0 {
                ("HEALTH PAGE 1", "Health data layout\nwill be added next.")
            } else {
                ("HEALTH PAGE 2", "Health data layout\nwill be added next.")
            }
        }
        DaemonState::Stale => ("HOST LOST", "Updates stopped.\nCheck the daemon."),
        DaemonState::Connected | DaemonState::Waiting if usb_connected => {
            ("USB READY", "Start the Proxmox\nhost daemon.")
        }
        DaemonState::Connected | DaemonState::Waiting => {
            ("OFFLINE", "Connect a USB host\nto receive data.")
        }
    };
    Text::new(title, Point::new(8, 112), body)
        .draw(display)
        .ok();
    for (index, line) in detail.split('\n').enumerate() {
        Text::new(
            line,
            Point::new(8, 132 + i32::try_from(index).unwrap_or(0) * 14),
            body,
        )
        .draw(display)
        .ok();
    }

    if daemon == DaemonState::Connected
        && usb_connected
        && let Some(snapshot) = guest_snapshot
    {
        draw_guests(display, &snapshot, page, body);
    }

    if daemon == DaemonState::Connected && usb_connected && guest_snapshot.is_some() {
        Text::new("Press: next  Hold: off", Point::new(8, 224), body)
            .draw(display)
            .ok();
    } else {
        let mut sequence_text = String::<32>::new();
        match sequence {
            Some(value) => write!(&mut sequence_text, "Update #{value}").ok(),
            None => sequence_text.push_str("No host data yet").ok(),
        };
        Text::new(&sequence_text, Point::new(8, 174), body)
            .draw(display)
            .ok();

        let copy = if usb_connected && daemon == DaemonState::Connected {
            "Press: next screen\nHold 2s: shutdown"
        } else {
            "Press: next screen\nShutdown needs host"
        };
        for (index, line) in copy.split('\n').enumerate() {
            Text::new(
                line,
                Point::new(8, 205 + i32::try_from(index).unwrap_or(0) * 14),
                body,
            )
            .draw(display)
            .ok();
        }
    }
}

fn draw_guests<D>(
    display: &mut D,
    snapshot: &GuestSnapshot,
    page: u8,
    text_style: MonoTextStyle<'_, Rgb565>,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let start = usize::from(page) * 3;
    for (row, guest) in snapshot.guests().iter().skip(start).take(3).enumerate() {
        let mut heading = String::<32>::new();
        let kind = match guest.kind {
            GuestKind::VirtualMachine => "VM",
            GuestKind::Container => "CT",
        };
        let _ = write!(&mut heading, "{} {kind} {}", guest.vmid, guest.name());
        Text::new(
            &heading,
            Point::new(8, 126 + i32::try_from(row).unwrap_or(0) * 26),
            text_style,
        )
        .draw(display)
        .ok();

        let mut metrics = String::<32>::new();
        match guest.status {
            GuestStatus::Running => {
                let _ = write!(
                    &mut metrics,
                    " RUN {}% {}/{}M",
                    guest.cpu_percent, guest.memory_used_mib, guest.memory_total_mib
                );
            }
            GuestStatus::Stopped => {
                let _ = metrics.push_str(" STOPPED");
            }
        }
        Text::new(
            &metrics,
            Point::new(8, 138 + i32::try_from(row).unwrap_or(0) * 26),
            text_style,
        )
        .draw(display)
        .ok();
    }
}

fn draw_indicator<D>(
    display: &mut D,
    y: i32,
    color: Rgb565,
    label: &str,
    text_style: MonoTextStyle<'_, Rgb565>,
) where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(Point::new(8, y - 7), Size::new(8, 8))
        .into_styled(PrimitiveStyleBuilder::new().fill_color(color).build())
        .draw(display)
        .ok();
    Text::new(label, Point::new(22, y), text_style)
        .draw(display)
        .ok();
}
