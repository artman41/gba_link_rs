#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::{gpio, i2c};
use embassy_time::{Timer, Delay};
use gpio::{Level, Output};
use i2c::I2c;
use hd44780_driver::{HD44780, DisplayMode, Display, Cursor, CursorBlink};
use panic_halt as _;

// Format a number as a string (simple implementation for no_std)
fn format_number(mut num: u32, buffer: &mut [u8; 16]) -> &str {
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

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut times: u32 = 0;
    let mut buffer = [0u8; 16];
    
    // Set up I2C for LCD (using pins GP0 (SDA) and GP1 (SCL))
    let sda = p.PIN_0;
    let scl = p.PIN_1;
    let i2c = I2c::new_blocking(p.I2C0, scl, sda, i2c::Config::default());
    
    // Set up LCD (trying address 0x27 first, most common)
    let mut lcd = HD44780::new_i2c(i2c, 0x27, &mut Delay).unwrap();
    
    // Initialize LCD
    lcd.reset(&mut Delay).unwrap();
    lcd.clear(&mut Delay).unwrap();
    
    // Turn on display without cursor
    lcd.set_display_mode(
        DisplayMode {
            display: Display::On,
            cursor_visibility: Cursor::Invisible,
            cursor_blink: CursorBlink::Off,
        },
        &mut Delay,
    ).unwrap();
    
    // Display initial message
    lcd.write_str("LED Count:", &mut Delay).unwrap();
    
    // Set up LED
    let mut led = Output::new(p.PIN_28, Level::Low);
    
    loop {
        // Turn LED on and increment counter
        led.set_high();
        times += 1;
        
        // Update LCD with new count
        lcd.set_cursor_pos(40, &mut Delay).unwrap(); // Second line
        lcd.write_str("        ", &mut Delay).unwrap(); // Clear line
        lcd.set_cursor_pos(40, &mut Delay).unwrap(); // Back to second line start
        
        let count_str = format_number(times, &mut buffer);
        lcd.write_str(count_str, &mut Delay).unwrap();
        
        Timer::after_secs(1).await;
        
        // Turn LED off
        led.set_low();
        Timer::after_secs(1).await;
    }
}