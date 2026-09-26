//! Setting up a device on a root port: giving it an address, reading its
//! descriptors, and configuring it (USB 2.0 specification, chapter 9).

use crate::dma::Page;
use crate::xhci::{FULL_SPEED, HIGH_SPEED, LOW_SPEED, Result, Ring, Xhci};

/// Memory for one device, kept for its port and reused when something is
/// plugged in again.
pub struct DeviceMemory {
    input: Page,
    output: Page,
    ep0: Page,
    /// Descriptors are read into this.
    buffer: Page,
    /// The interrupt endpoint's ring and reports.
    pub interrupt: Page,
    pub reports: Page,
}

impl DeviceMemory {
    pub fn new(xhci: &mut Xhci) -> Result<DeviceMemory> {
        let mut page = || xhci.pool.page().ok_or("not enough DMA memory for a device");
        Ok(DeviceMemory {
            input: page()?,
            output: page()?,
            ep0: page()?,
            buffer: page()?,
            interrupt: page()?,
            reports: page()?,
        })
    }
}

/// A device with an address, and its control endpoint.
pub struct Device {
    pub slot: u8,
    pub port: u8,
    pub speed: u8,
    ep0: Ring,
}

// Standard requests.
const GET_DESCRIPTOR: u8 = 6;
const SET_CONFIGURATION: u8 = 9;
const DEVICE_DESCRIPTOR: u16 = 1;
const CONFIGURATION_DESCRIPTOR: u16 = 2;
const INTERFACE_DESCRIPTOR: u8 = 4;
const ENDPOINT_DESCRIPTOR: u8 = 5;

// Endpoint types, as the endpoint context numbers them.
const CONTROL: u32 = 4;
const INTERRUPT_IN: u32 = 7;

/// An interface a class driver might want, with its first interrupt IN
/// endpoint.
#[derive(Clone, Copy, Debug)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoint: Option<Endpoint>,
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    /// The endpoint number (without the direction bit).
    pub number: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    /// Its device context index: IN endpoints are odd.
    pub fn dci(&self) -> u8 {
        self.number * 2 + 1
    }
}

/// A setup packet.
fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    let [v0, v1] = value.to_le_bytes();
    let [i0, i1] = index.to_le_bytes();
    let [l0, l1] = length.to_le_bytes();
    [request_type, request, v0, v1, i0, i1, l0, l1]
}

impl Device {
    /// Give the device on `port` (at `speed`) a slot and an address.
    pub fn address(xhci: &mut Xhci, memory: &DeviceMemory, port: u8, speed: u8) -> Result<Device> {
        let slot = xhci.enable_slot()?;
        memory.output.zero();
        xhci.set_device_context(slot, &memory.output);
        let mut device = Device {
            slot,
            port,
            speed,
            ep0: Ring::new(memory.ep0),
        };
        // Until the device says, assume the smallest packets its speed
        // allows (full speed devices may use 8 to 64).
        let max_packet = match speed {
            LOW_SPEED | FULL_SPEED => 8,
            HIGH_SPEED => 64,
            _ => 512,
        };
        let cs = xhci.context_size;
        let input = &memory.input;
        input.zero();
        input.write32(4, 0b11); // add the slot and endpoint 0 contexts
        device.write_slot_context(input, cs, 1);
        let ep0 = 2 * cs;
        input.write32(ep0 + 4, 3 << 1 | CONTROL << 3 | max_packet << 16);
        input.write64(ep0 + 8, device.ep0.dequeue_pointer());
        input.write32(ep0 + 16, 8); // average TRB length
        if let Err(e) = xhci.address_device(slot, input) {
            xhci.disable_slot(slot);
            return Err(e);
        }

        // The real maximum packet size of endpoint 0 is in the first 8
        // bytes of the device descriptor.
        let result = device
            .get_descriptor(xhci, memory, DEVICE_DESCRIPTOR, 8)
            .and_then(|()| {
                let reported = memory.buffer.read8(7) as u32;
                let actual = if speed >= 4 { 1 << reported } else { reported };
                if actual != max_packet && actual != 0 {
                    input.zero();
                    input.write32(4, 0b10); // evaluate endpoint 0's context
                    input.write32(ep0 + 4, 3 << 1 | CONTROL << 3 | actual << 16);
                    xhci.evaluate_context(slot, input)
                } else {
                    Ok(())
                }
            });
        match result {
            Ok(()) => Ok(device),
            Err(e) => {
                xhci.disable_slot(slot);
                Err(e)
            }
        }
    }

    /// The slot context, in the input context, with `entries` endpoint
    /// contexts in use.
    fn write_slot_context(&self, input: &Page, cs: usize, entries: u32) {
        let slot = cs;
        input.write32(slot, (self.speed as u32) << 20 | entries << 27);
        input.write32(slot + 4, (self.port as u32) << 16);
    }

    fn get_descriptor(
        &mut self,
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        kind: u16,
        length: u16,
    ) -> Result<()> {
        let packet = setup(0x80, GET_DESCRIPTOR, kind << 8, 0, length);
        xhci.control(self.slot, &mut self.ep0, packet, &memory.buffer, length)
    }

    /// A request with no data stage.
    pub fn request(
        &mut self,
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
    ) -> Result<()> {
        let packet = setup(request_type, request, value, index, 0);
        xhci.control(self.slot, &mut self.ep0, packet, &memory.buffer, 0)
    }

    /// Read the device's (first) configuration, calling `each` for each of
    /// its interfaces, and return the configuration's value.
    pub fn configuration(
        &mut self,
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        mut each: impl FnMut(Interface),
    ) -> Result<u8> {
        self.get_descriptor(xhci, memory, CONFIGURATION_DESCRIPTOR, 9)?;
        let buffer = &memory.buffer;
        let total = (buffer.read8(2) as u16 | (buffer.read8(3) as u16) << 8).min(1024);
        let value = buffer.read8(5);
        self.get_descriptor(xhci, memory, CONFIGURATION_DESCRIPTOR, total)?;

        let mut at = 0usize;
        let mut current: Option<Interface> = None;
        while at + 2 <= total as usize {
            let length = buffer.read8(at) as usize;
            if length < 2 || at + length > total as usize {
                break;
            }
            match buffer.read8(at + 1) {
                INTERFACE_DESCRIPTOR if length >= 9 => {
                    if let Some(interface) = current.take() {
                        each(interface);
                    }
                    current = Some(Interface {
                        number: buffer.read8(at + 2),
                        class: buffer.read8(at + 5),
                        subclass: buffer.read8(at + 6),
                        protocol: buffer.read8(at + 7),
                        endpoint: None,
                    });
                }
                ENDPOINT_DESCRIPTOR if length >= 7 => {
                    let address = buffer.read8(at + 2);
                    let attributes = buffer.read8(at + 3);
                    if let Some(interface) = current.as_mut()
                        && interface.endpoint.is_none()
                        && address & 0x80 != 0
                        && attributes & 0x3 == 3
                    {
                        interface.endpoint = Some(Endpoint {
                            number: address & 0xF,
                            max_packet: (buffer.read8(at + 4) as u16
                                | (buffer.read8(at + 5) as u16) << 8)
                                & 0x7FF,
                            interval: buffer.read8(at + 6),
                        });
                    }
                }
                _ => {}
            }
            at += length;
        }
        if let Some(interface) = current.take() {
            each(interface);
        }
        Ok(value)
    }

    pub fn set_configuration(
        &mut self,
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        value: u8,
    ) -> Result<()> {
        self.request(xhci, memory, 0x00, SET_CONFIGURATION, value as u16, 0)
    }

    /// Set up interrupt IN `endpoint`, with `ring` as its transfer ring.
    pub fn configure_interrupt_in(
        &mut self,
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        endpoint: Endpoint,
        ring: &Ring,
    ) -> Result<()> {
        let cs = xhci.context_size;
        let input = &memory.input;
        let dci = endpoint.dci();
        input.zero();
        input.write32(4, 1 | 1 << dci); // add the slot context and the endpoint's
        self.write_slot_context(input, cs, dci as u32);
        // The interval, as a power of two of 125 us: high speed and faster
        // devices give that exponent (plus one); slower ones milliseconds.
        let interval = match self.speed {
            LOW_SPEED | FULL_SPEED => {
                let ms = endpoint.interval.max(1) as u32;
                (31 - (ms * 8).leading_zeros()).clamp(3, 10)
            }
            _ => (endpoint.interval.clamp(1, 16) - 1) as u32,
        };
        let context = (dci as usize + 1) * cs;
        let max_packet = endpoint.max_packet as u32;
        input.write32(context, interval << 16);
        input.write32(context + 4, 3 << 1 | INTERRUPT_IN << 3 | max_packet << 16);
        input.write64(context + 8, ring.dequeue_pointer());
        input.write32(context + 16, max_packet | max_packet << 16);
        xhci.configure_endpoint(self.slot, input)
    }
}
