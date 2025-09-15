# gba_link_rs

A Project designed to use the Pi Pico to provide an interface to the GBA's Link port to allow for direct communication from PC <-> GBA

## Testing

Currently, in way of testing, I have been looking in to how exactly the Pi Pico works.

* `counter.rs` is a simple incrementing application that will shine an LED every second, counting the amount of times the LED has turned on and displaying it on an LCD.

* `usb_counter.rs` takes this a bit further and provides a USB HID device to the system which is primarily used for logging but can be used to request the current counter value

* The next goal is to connect a button to the Pi Pico and have an application which polls for user input.

After this, the task will be to put that knowledge together and attempt the GBA Link