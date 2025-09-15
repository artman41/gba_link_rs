#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::{bind_interrupts, gpio::{Level, Output}, i2c::{self, I2c}, multicore::{spawn_core1, Stack}, peripherals::USB, usb::{Driver, InterruptHandler}, Peri};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Delay, Timer};
use embassy_usb::{class::hid::{HidReaderWriter, ReportId, RequestHandler, State}, control::OutResponse, Builder, Config, Handler};
use hd44780_driver::{Cursor, CursorBlink, Display, DisplayMode, HD44780};
use portable_atomic::AtomicU32;
use static_cell::StaticCell;
use heapless::String;
use usbd_hid::descriptor::generator_prelude::*;
// use panic_halt as _;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

// Type alias for log messages - using fixed-size array for embedded systems
const LOG_MESSAGE_SIZE: usize = 64;

#[derive(Copy, Clone)]
#[allow(dead_code)]
enum LogSeverity {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 4,
    Critical = 8,
}

struct LogMessage {
    #[allow(dead_code)]
    severity: LogSeverity,
    message: [u8; LOG_MESSAGE_SIZE],
}

impl LogMessage {
    fn new(severity: LogSeverity, message: [u8; LOG_MESSAGE_SIZE]) -> Self {
        Self {
            severity,
            message
        }
    }
}

// Static pointer to the log channel - initialized at startup
const MAX_LOG_MESSAGES: usize = 128;
static mut LOG_CHANNEL_PTR: *const Channel<CriticalSectionRawMutex, LogMessage, MAX_LOG_MESSAGES> = core::ptr::null();

// Single macro for logging that chunks messages into [u8;MAX_LOG_MESSAGES] pieces
macro_rules! log {
    ($msg:expr) => {
        {
            let maybe_sender = unsafe {
                if LOG_CHANNEL_PTR.is_null() {
                    None
                } else {
                    let channel = &*LOG_CHANNEL_PTR;
                    Some(channel.sender())
                }
            };
            if let Some(sender) = maybe_sender {
                let mut msg_bytes = [0u8; $msg.len()+1];
                msg_bytes[..$msg.len()].copy_from_slice($msg.as_bytes());
                let mut offset = 0;
                while offset < msg_bytes.len() {
                    let mut log_msg = [0u8; LOG_MESSAGE_SIZE];
                    let chunk_len = (msg_bytes.len() - offset).min(LOG_MESSAGE_SIZE);
                    log_msg[..chunk_len].copy_from_slice(&msg_bytes[offset..offset + chunk_len]);
                    // Use try_send to avoid blocking
                    let _ = sender.try_send(LogMessage::new(LogSeverity::Info, log_msg));
                    offset += chunk_len;
                }
            }
        }
    };
    ($log_severity:expr, $fmt:expr, $($args:expr),*) => {
        {
            let maybe_sender = unsafe {
                if LOG_CHANNEL_PTR.is_null() {
                    None
                } else {
                    let channel = &*LOG_CHANNEL_PTR;
                    Some(channel.sender())
                }
            };
            if let Some(sender) = maybe_sender {
                let mut formatted: String<256> = String::new();
                if let Ok(_) = core::fmt::write(&mut formatted, format_args!($fmt, $($args),*)) {
                    let msg_bytes = formatted.as_bytes();
                    let mut offset = 0;
                    while offset < msg_bytes.len() {
                        let mut log_msg = [0u8; LOG_MESSAGE_SIZE];
                        let chunk_len = (msg_bytes.len() - offset).min(LOG_MESSAGE_SIZE);
                        log_msg[..chunk_len].copy_from_slice(&msg_bytes[offset..offset + chunk_len]);
                        // Use try_send to avoid blocking
                        let _ = sender.try_send(LogMessage::new($log_severity, log_msg));
                        offset += chunk_len;
                    }
                }
            }
        }
    };
}

// Remove or comment out this line:
// use panic_halt as _;

// Add this custom panic handler instead:
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Try to log the panic info if logging is available
    if let Some(location) = info.location() {
        if let Some(message) = info.message().as_str() {
            log!(LogSeverity::Critical, "PANIC at {}:{} - {}", location.file(), location.line(), message);
        } else {
            log!(LogSeverity::Critical, "PANIC at {}:{}", location.file(), location.line());
        }
    } else {
        log!(LogSeverity::Critical, "{}", "PANIC occurred at unknown location");
    }
    
    // Send multiple copies to increase chances of delivery
    for _ in 0..3 {
        log!(LogSeverity::Critical, "{}", "SYSTEM PANIC - HALTING");
    }
    
    // Give much more time for the log to be sent over USB
    // Use multiple shorter delays to allow the async USB system to process logs
    for _ in 0..50 {
        cortex_m::asm::delay(100_000); // Multiple shorter delays
        // Allow interrupts to continue processing USB
        cortex_m::asm::nop();
    }
    
    // Final halt
    loop {
        cortex_m::asm::wfi(); // Wait for interrupt (low power halt)
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_rp::init(Default::default());

    // Initialize the log channel and set up the static pointer
    static LOG_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, LogMessage, MAX_LOG_MESSAGES>> = StaticCell::new();
    let channel = LOG_CHANNEL.init(Channel::new());
    unsafe {
        LOG_CHANNEL_PTR = channel as *const Channel<CriticalSectionRawMutex, LogMessage, MAX_LOG_MESSAGES>;
    }

    static TIMES: AtomicU32 = AtomicU32::new(0);
    
    // Move LED/LCD logic to CORE1 to avoid executor contention
    static mut CORE1_STACK: Stack<1024> = Stack::new();
    static EXECUTOR: StaticCell<embassy_executor::Executor> = StaticCell::new();
    
    // Extract peripherals for CORE1
    let pin_0 = peripherals.PIN_0;
    let pin_1 = peripherals.PIN_1;
    let i2c0 = peripherals.I2C0;
    let pin_28 = peripherals.PIN_28;
    let usb = peripherals.USB;
    
    // Since TIMES is static, both cores can access it safely
    spawn_core1(peripherals.CORE1, unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) }, move || {
        let executor = EXECUTOR.init(embassy_executor::Executor::new());
        executor.run(|spawner| {
            spawner.spawn(loop_onboard(pin_0, pin_1, i2c0, pin_28, &TIMES)).unwrap();
        })
    });
    
    // Keep USB on main core for better enumeration
    spawner.spawn(handle_usb(usb, &TIMES)).unwrap();
    
    // Keep main alive
    loop {
        Timer::after_secs(10).await;
    }
}

// #[embassy_executor::task]
#[embassy_executor::task]
async fn loop_onboard(
    pin_0: Peri<'static, embassy_rp::peripherals::PIN_0>,
    pin_1: Peri<'static, embassy_rp::peripherals::PIN_1>, 
    i2c0: Peri<'static, embassy_rp::peripherals::I2C0>,
    pin_28: Peri<'static, embassy_rp::peripherals::PIN_28>,
    times: &'static AtomicU32
) {
    log!("Starting up...");
    let mut buffer = [0u8; 16];
    
    // Set up I2C for LCD (using pins GP0 (SDA) and GP1 (SCL))
    let sda = pin_0;
    let scl = pin_1;
    let i2c = I2c::new_blocking(i2c0, scl, sda, i2c::Config::default());
    
    // Set up LCD (trying address 0x27 first, most common)
    let mut lcd = HD44780::new_i2c(i2c, 0x27, &mut Delay).unwrap();
    
    // Initialize LCD
    lcd.reset(&mut Delay).unwrap();
    lcd.clear(&mut Delay).unwrap();
    log!("Initialised LCD");
    
    // Turn on display without cursor
    lcd.set_display_mode(
        DisplayMode {
            display: Display::On,
            cursor_visibility: Cursor::Invisible,
            cursor_blink: CursorBlink::Off,
        },
        &mut Delay,
    ).unwrap();
    log!("Configured LCD");
    
    // Display initial message
    lcd.write_str("LED Count:", &mut Delay).unwrap();
    
    // Set up LED
    let mut led = Output::new(pin_28, Level::Low);
    log!("Powered LCD");
    
    loop {
        let mut current = times.load(Ordering::Relaxed);
        log!("Powering LED");
        // Turn LED on and increment counter
        led.set_high();
        current = current.wrapping_add(1);
        times.store(current, Ordering::Relaxed);

        log!("Updating LCD");
        // Update LCD with new count - add error handling
        if let Err(_) = lcd.set_cursor_pos(40, &mut Delay) {
            log!("LCD cursor pos error");
        }
        if let Err(_) = lcd.write_str("        ", &mut Delay) {
            log!("LCD clear error");
        }
        if let Err(_) = lcd.set_cursor_pos(40, &mut Delay) {
            log!("LCD cursor pos2 error");
        }
        
        let count_str = format_number(current as u64, &mut buffer);
        if let Err(_) = lcd.write_str(count_str, &mut Delay) {
            log!("LCD write error");
        }
        
        log!("LED timer wait");
        Timer::after_secs(1).await;
        
        log!("Unpowering LED");
        // Turn LED off
        led.set_low();
        log!("LED off timer wait");
        Timer::after_secs(1).await;
    }
}

const VENDOR_ID: u16 = 0x5f56;
const PRODUCT_ID: u16 = 0xc0d8;

#[allow(dead_code)]
struct USBRequestHandler {}
impl RequestHandler for USBRequestHandler {
    fn get_report(&mut self, _id: ReportId, _buf: &mut [u8]) -> Option<usize> {
        None
    }

    fn set_report(&mut self, _id: ReportId, _data: &[u8]) -> OutResponse {
        OutResponse::Accepted
    }

    fn set_idle_ms(&mut self, _id: Option<ReportId>, _dur: u32) {
    }

    fn get_idle_ms(&mut self, _id: Option<ReportId>) -> Option<u32> {
        None
    }
}

struct USBDeviceHandler {
    configured: AtomicBool
}
impl USBDeviceHandler {
    fn new() -> Self {
        Self {
            configured: AtomicBool::new(false)
        }
    }
}

impl Handler for USBDeviceHandler {
    fn reset(&mut self) {
        self.configured.store(false, Ordering::Relaxed);
    }

    fn enabled(&mut self, _enabled: bool) {
        self.configured.store(false, Ordering::Relaxed)
    }

    fn addressed(&mut self, _addr: u8) {
        self.configured.store(false, Ordering::Relaxed)
    }

    fn configured(&mut self, configured: bool) {
        self.configured.store(configured, Ordering::Relaxed)
    }   
}

#[repr(u8)]
#[allow(dead_code)]
enum PayloadType {
    Log = 1,
    Counter = 2,
    Heartbeat = 3,
}

#[gen_hid_descriptor(
    (collection = APPLICATION, usage_page = 0xFF00, usage = 0x01) = {
        (usage = 0x01,) = {
            #[item_settings data,variable,absolute] payload_type=input;
        };
        (usage = 0x02,) = {
            #[item_settings data,variable,absolute] correlation_id=input;
        };
        (usage = 0x03,) = {
            #[item_settings data,variable,absolute] severity=input;
        };
        (usage = 0x04,) = {
            #[item_settings data,variable,absolute] payload_low=input;
        };
        (usage = 0x05,) = {
            #[item_settings data,variable,absolute] payload_high=input;
        };
        (usage = 0x06,) = {
            #[item_settings data,variable,absolute] cmd_type=output;
        };
        (usage = 0x07,) = {
            #[item_settings data,variable,absolute] cmd_low=output;
        };
        (usage = 0x08,) = {
            #[item_settings data,variable,absolute] cmd_high=output;
        };
    }
)]
#[allow(dead_code)]
pub struct USBHIDReport {
    // device to host
    pub payload_type: u8,
    pub correlation_id: u8,
    pub severity: u8,
    pub payload_low: u16,
    pub payload_high: u16,
    // host to device
    pub cmd_type: u8,
    pub cmd_low: u16,
    pub cmd_high: u16,
}

impl USBHIDReport {
    pub const fn default() -> Self {
        Self {
            payload_type: PayloadType::Heartbeat as u8,
            correlation_id: 0,
            severity: LogSeverity::Info as u8,
            payload_low: 0,
            payload_high: 0,
            cmd_type: 0,
            cmd_low: 0,
            cmd_high: 0,
        }
    }
}

#[embassy_executor::task]
async fn handle_usb(
    usb: Peri<'static, USB>, 
    times: &'static AtomicU32
) {
    // Create the driver, from the HAL.
    let driver = Driver::new(usb, Irqs);

    // Create embassy-usb Config
    let mut config = Config::new(VENDOR_ID, PRODUCT_ID);
    config.manufacturer = Some("tylerhughes");
    config.product = Some("USB Counter");
    config.serial_number = Some("12345678");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    // You can also add a Microsoft OS descriptor.
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; 64];
    let mut device_handler = USBDeviceHandler::new();

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );
    builder.handler(&mut device_handler);

    let config = embassy_usb::class::hid::Config {
        report_descriptor: USBHIDReport::desc(),
        request_handler: None,
        poll_ms: 10,
        max_packet_size: 64,
    };
    let hid = HidReaderWriter::<_, 5, 8>::new(&mut builder, &mut state, config);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();
    
    let (reader, writer) = hid.split();

    // Channel for coordinating counter requests between input and output tasks
    static COUNTER_REQUEST_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, bool, 8>> = StaticCell::new();
    let counter_request_channel = COUNTER_REQUEST_CHANNEL.init(Channel::new());

    let in_fut = async {
        let mut writer = writer;
        let counter_receiver = counter_request_channel.receiver();
        let mut next_correlation_id = 1u8;

        let channel = 
            unsafe {
                if LOG_CHANNEL_PTR.is_null() {
                    // Channel not initialized yet, skip logging
                    panic!("Log channel not initialized");
                }
                &*LOG_CHANNEL_PTR
            };
        let log_receiver = channel.receiver();
        
        loop {
            let mut report = USBHIDReport::default();
            
            // Check if host requested counter value
            if let Ok(_) = counter_receiver.try_receive() {
                // Send counter value
                report.payload_type = PayloadType::Counter as u8;
                report.correlation_id = next_correlation_id;
                next_correlation_id = next_correlation_id.wrapping_add(1);
                let current = times.load(Ordering::Relaxed);
                report.payload_low = (current & 0xFFFF) as u16;
                report.payload_high = ((current >> 16) & 0xFFFF) as u16;
                let _ = writer.write_serialize(&report).await;
                
            } else {
                let mut sent_log = false;
                loop {
                    if let Ok(log_msg) = log_receiver.try_receive() {
                        sent_log = true;
                        let mut last_msg = false;

                        // Find the actual length of the message (up to first null byte)
                        let mut msg_len = 0;
                        for &byte in log_msg.message.iter() {
                            msg_len += 1;
                            if byte == 0 {
                                last_msg = true;
                                break;
                            }
                        }
                        
                        // Send message in 4-byte chunks
                        let mut offset = 0;
                        while offset < msg_len {
                            let mut chunk_report = USBHIDReport::default();
                            chunk_report.payload_type = PayloadType::Log as u8;
                            chunk_report.correlation_id = next_correlation_id;
                            chunk_report.severity = log_msg.severity as u8;
                            
                            // Pack 4 bytes into this chunk
                            let mut chunk_bytes = [0u8; 4];
                            for i in 0..4 {
                                if offset + i < msg_len {
                                    chunk_bytes[i] = log_msg.message[offset + i];
                                }
                            }
                            
                            chunk_report.payload_low = u16::from_le_bytes([chunk_bytes[0], chunk_bytes[1]]);
                            chunk_report.payload_high = u16::from_le_bytes([chunk_bytes[2], chunk_bytes[3]]);
                            
                            let _ = writer.write_serialize(&chunk_report).await;
                            Timer::after_millis(10).await; // Small delay between chunks
                            
                            offset += 4;
                        }
                        if last_msg {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                if sent_log {
                    next_correlation_id = next_correlation_id.wrapping_add(1);
                } else {
                    // No log messages available, send heartbeat
                    report.payload_type = PayloadType::Heartbeat as u8;
                    report.correlation_id = 0; // Heartbeats don't need correlation IDs
                    report.payload_low = 0;
                    report.payload_high = 0;
                    let _ = writer.write_serialize(&report).await;
                }
            }
            
            Timer::after_millis(50).await; // Send updates every 50ms
        }
    };

    let out_fut = async {
        let mut reader = reader;
        let sender = counter_request_channel.sender();
        let mut buf = [0u8; 5];
        
        loop {
            match reader.read(&mut buf).await {
                Ok(len) if len >= 5 => {
                    log!(LogSeverity::Info, "Received data from host of size {} {:?}", len, &buf);
                    // Process received data - manually parse the report
                    // New structure: payload_type(1) + correlation_id(1) + severity(1) + payload_low(2) + payload_high(2) + cmd_type(1) + cmd_low(2) + cmd_high(2)
                    let cmd_type = buf[0]; // cmd_type field offset (moved due to correlation_id and severity)
                    let cmd_low = u16::from_le_bytes([buf[1], buf[2]]);
                    let cmd_high = u16::from_le_bytes([buf[3], buf[4]]);
                    let cmd_value = ((cmd_high as u32) << 16) | (cmd_low as u32);
                    
                    match cmd_type {
                        1 => { // Log command - request current counter
                            log!("Host requested counter value");
                            let _ = sender.try_send(true);
                        },
                        2 => { // Counter set command
                            times.store(cmd_value, Ordering::Relaxed);
                            log!("Counter set to new value");
                            // Also send updated counter value
                            let _ = sender.try_send(true);
                        },
                        _ => {
                            log!("Unknown command type");
                        }
                    }
                },
                Ok(len) => {
                    log!(LogSeverity::Warn, "Received incomplete data from host of size {}", len);
                    // Incomplete read, continue
                    Timer::after_millis(10).await;
                },
                Err(err) => {
                    log!(LogSeverity::Error, "Error reading data from host: {:?}", err);
                    // Read error, wait and retry
                    Timer::after_millis(50).await;
                }
            }
        }
    };

    // Run everything concurrently.
    join(usb_fut, join(in_fut, out_fut)).await;
}

fn format_number(mut num: u64, buffer: &mut [u8]) -> &str {
    if num == 0 {
        buffer[0] = b'0';
        return core::str::from_utf8(&buffer[0..1]).unwrap();
    }
    
    let mut pos = 0;
    let mut temp = num;
    
    // Count digits
    while temp > 0 {
        temp /= 10;
        pos += 1;
    }
    
    // Fill buffer backwards
    let end_pos = pos;
    while num > 0 {
        pos -= 1;
        buffer[pos] = b'0' + (num % 10) as u8;
        num /= 10;
    }
    
    core::str::from_utf8(&buffer[0..end_pos]).unwrap()
}