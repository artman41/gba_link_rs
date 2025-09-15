import hid
import struct
import time
import signal
from datetime import datetime, timezone

# Globals are kept in a single variable 
# That trick enables accessing them from 
# various routines...

class glbs:
    pass
glb = glbs()

glb.runflag = True

# Find our HID device
print("Looking for HID device...")
devices = hid.enumerate(0x5F56, 0xC0D8)

if not devices:
    print("Available HID devices:")
    for device in hid.enumerate():
        print(f"VID: 0x{device['vendor_id']:04X}, PID: 0x{device['product_id']:04X} - {device['product_string']}")
    raise ValueError('HID Device not found')

print(f"Found {len(devices)} matching device(s)")
for i, device in enumerate(devices):
    print(f"Device {i}: {device['product_string']} - Path: {device['path']}")

# Open the first matching device
dev = hid.device()
try:
    dev.open(0x5F56, 0xC0D8)
    print("Successfully opened HID device")
    
    # Get device info
    print(f"Manufacturer: {dev.get_manufacturer_string()}")
    print(f"Product: {dev.get_product_string()}")
    print(f"Serial: {dev.get_serial_number_string()}")
    
except Exception as e:
    print(f"Error opening device: {e}")
    exit(1)

def on_sig_int(sig, frame):
    glb.runflag = False
    print("\nShutting down...")

signal.signal(signal.SIGINT, on_sig_int)

print("Starting to read data... Press Ctrl+C to stop")
print("Press Enter to request counter value...")

import threading
import sys

def input_thread():
    """Thread to handle user input for requesting counter values"""
    while glb.runflag:
        try:
            input()  # Wait for Enter key
            if glb.runflag:
                # Try different sizes to see what works
                request_data = [1] * 5
                request_data[0] = 1
                dev.write(request_data)
        except:
            break

# Start input thread
input_handler = threading.Thread(target=input_thread, daemon=True)
input_handler.start()

log_messages = {}

while glb.runflag:
    try:
        # Read HID report (timeout in milliseconds)
        data = dev.read(64, timeout_ms=1000)
        
        if data:
            
            # Handle 5-byte HID input report: payload_type(1) + payload_low(2) + payload_high(2)
            if len(data) >= 5:
                payload_type = data[0]
                payload_corrid = data[1]
                payload_severity = data[2]
                payload_low = struct.unpack('<H', bytes(data[3:5]))[0]
                payload_high = struct.unpack('<H', bytes(data[5:7]))[0]
                
                if payload_type == 1:  # Log message fragment
                    # Extract 4 bytes from payload_low and payload_high
                    byte1 = payload_low & 0xFF
                    byte2 = (payload_low >> 8) & 0xFF
                    byte3 = payload_high & 0xFF
                    byte4 = (payload_high >> 8) & 0xFF
                    
                    # Convert to characters, showing both text and hex
                    chars = []
                    hex_vals = []

                    # print(f"Log fragment (corrid={payload_corrid}): {[f'{b:02x}' for b in [byte1, byte2, byte3, byte4]]}")
                    for b in [byte1, byte2, byte3, byte4]:
                        hex_vals.append(f"{b:02x}")
                        if b == 0 or b == 1:
                            break
                        elif 32 <= b <= 126:  # Printable ASCII
                            chars.append(chr(b))
                        else:
                            chars.append(f'\\x{b:02x}')
                    
                    char_str = ''.join(chars)
                    msg = log_messages.get(payload_corrid, {"severity": 0, "message": ''})
                    msg["severity"] |= payload_severity
                    msg["message"] += char_str
                    log_messages[payload_corrid] = msg
                    if byte4 == 0:  # Null terminator indicates end of message
                        msg = log_messages.pop(payload_corrid, '')
                        if msg["severity"] & 8:
                            sev = "Critical"
                        elif msg["severity"] & 4:
                            sev = "Error"
                        elif msg["severity"] & 2:
                            sev = "Warn"
                        elif msg["severity"] & 1:
                            sev = "Info"
                        elif msg["severity"] & 0:
                            sev = "Debug"
                        print(f"<LOG {payload_corrid}> [{sev}] '{msg["message"]}'")
                elif payload_type == 2:  # Counter value
                    counter_value = (payload_high << 16) | payload_low
                    print(f"<COUNTER> Current value: {counter_value}")
                elif payload_type == 3:
                    pass # Don't log heartbeats
                else:
                    counter_value = (payload_high << 16) | payload_low
                    print(f"[UNKNOWN] Type: {payload_type}, Value: {counter_value}")
            else:
                print(f"Received very short packet: {len(data)} bytes: {[f'{b:02x}' for b in data]}")
        else:
            print(".", end="", flush=True)  # Show we're still alive
            
    except Exception as e:
        print(f"Error reading data: {e}")
        time.sleep(0.1)

# Clean up
dev.close()
print("Device closed")
