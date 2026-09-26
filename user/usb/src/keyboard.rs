//! USB keyboards, in the HID boot protocol (HID 1.11, appendix B): every
//! keyboard supports it, and its reports are fixed: a byte of modifier
//! keys, a reserved byte, and up to six keys held down.
//!
//! Keys are translated with a US layout. Keys held down repeat after half
//! a second, 30 times a second (the boot protocol leaves that to us).

use crate::device::{Device, DeviceMemory, Endpoint};
use crate::xhci::{IOC, ISP, NORMAL, Result, Ring, SHORT_PACKET, SUCCESS, Trb, Xhci};

const HID_CLASS: u8 = 3;
const BOOT_SUBCLASS: u8 = 1;
const KEYBOARD_PROTOCOL: u8 = 1;
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const REPORT_SIZE: u32 = 8;

const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_EVERY_MS: u64 = 33;

// Modifier bits.
const CTRL: u8 = 0x01 | 0x10;
const SHIFT: u8 = 0x02 | 0x20;

const CAPS_LOCK: u8 = 0x39;

/// Bytes typed, waiting to go to the console.
pub struct Typed {
    pub bytes: [u8; 64],
    pub len: usize,
}

impl Typed {
    fn push(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }
}

pub struct Keyboard {
    pub device: Device,
    endpoint: Endpoint,
    ring: Ring,
    report: crate::dma::Page,
    previous: [u8; 8],
    caps_lock: bool,
    /// The key to repeat, and when.
    repeat: Option<(u8, u64)>,
}

impl Keyboard {
    /// Set up `device` as a keyboard, if it has a boot keyboard interface;
    /// else return what class of device it is.
    pub fn start(
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        mut device: Device,
    ) -> core::result::Result<Keyboard, (Device, &'static str)> {
        let mut found = None;
        let mut first_class = 0;
        let configuration = device.configuration(xhci, memory, |interface| {
            if first_class == 0 {
                first_class = interface.class;
            }
            let keyboard = interface.class == HID_CLASS
                && interface.subclass == BOOT_SUBCLASS
                && interface.protocol == KEYBOARD_PROTOCOL;
            if keyboard && found.is_none() {
                found = interface
                    .endpoint
                    .map(|endpoint| (interface.number, endpoint));
            }
        });
        let configuration = match configuration {
            Ok(value) => value,
            Err(e) => return Err((device, e)),
        };
        let Some((interface, endpoint)) = found else {
            return Err((device, class_name(first_class)));
        };
        let result = Self::configure(
            xhci,
            memory,
            &mut device,
            configuration,
            interface,
            endpoint,
        );
        match result {
            Ok(ring) => {
                let mut keyboard = Keyboard {
                    device,
                    endpoint,
                    ring,
                    report: memory.reports,
                    previous: [0; 8],
                    caps_lock: false,
                    repeat: None,
                };
                keyboard.queue(xhci);
                Ok(keyboard)
            }
            Err(e) => Err((device, e)),
        }
    }

    fn configure(
        xhci: &mut Xhci,
        memory: &DeviceMemory,
        device: &mut Device,
        configuration: u8,
        interface: u8,
        endpoint: Endpoint,
    ) -> Result<Ring> {
        device.set_configuration(xhci, memory, configuration)?;
        // Class requests to the interface: reports in the boot format, and
        // only when something changes.
        device.request(xhci, memory, 0x21, SET_PROTOCOL, 0, interface as u16)?;
        let _ = device.request(xhci, memory, 0x21, SET_IDLE, 0, interface as u16);
        let ring = Ring::new(memory.interrupt);
        device.configure_interrupt_in(xhci, memory, endpoint, &ring)?;
        Ok(ring)
    }

    /// Ask for the next report.
    fn queue(&mut self, xhci: &mut Xhci) {
        self.ring.push(Trb {
            param: self.report.bus,
            status: REPORT_SIZE,
            control: NORMAL << 10 | IOC | ISP,
        });
        xhci.ring_doorbell(self.device.slot, self.endpoint.dci());
    }

    /// Whether `event` is about this keyboard's reports.
    pub fn owns(&self, event: &Trb) -> bool {
        event.slot() == self.device.slot && event.endpoint() == self.endpoint.dci()
    }

    /// A report arrived (or failed): turn newly pressed keys into bytes,
    /// and ask for the next. Returns false if the keyboard stopped
    /// working.
    pub fn report(&mut self, xhci: &mut Xhci, event: &Trb, now_ms: u64, typed: &mut Typed) -> bool {
        if !matches!(event.code(), SUCCESS | SHORT_PACKET) {
            return false;
        }
        let mut report = [0u8; 8];
        for (i, byte) in report.iter_mut().enumerate() {
            *byte = self.report.read8(i);
        }
        let modifiers = report[0];
        for &key in &report[2..] {
            if key < 4 || self.previous[2..].contains(&key) {
                continue; // no key, an error code, or still held
            }
            if key == CAPS_LOCK {
                self.caps_lock = !self.caps_lock;
            } else if let Some(byte) = translate(key, modifiers, self.caps_lock) {
                typed.push(byte);
                self.repeat = Some((key, now_ms + REPEAT_DELAY_MS));
            }
        }
        if let Some((key, _)) = self.repeat
            && !report[2..].contains(&key)
        {
            self.repeat = None;
        }
        self.previous = report;
        self.queue(xhci);
        true
    }

    /// Repeat a held key, if it is time.
    pub fn tick(&mut self, now_ms: u64, typed: &mut Typed) {
        if let Some((key, when)) = self.repeat
            && now_ms >= when
        {
            if let Some(byte) = translate(key, self.previous[0], self.caps_lock) {
                typed.push(byte);
            }
            self.repeat = Some((key, now_ms.max(when) + REPEAT_EVERY_MS));
        }
    }
}

/// A key, as ASCII, in a US layout: letters, digits and punctuation, with
/// Shift and Caps Lock; Ctrl with a letter gives a control character;
/// Enter, Backspace, Tab and Escape. `None` for keys that type nothing
/// (arrows, function keys and so on, for now).
fn translate(key: u8, modifiers: u8, caps_lock: bool) -> Option<u8> {
    const PLAIN: &[u8; 0x39 - 0x1E] = b"1234567890\r\x1b\x7f\t -=[]\\#;'`,./";
    const SHIFTED: &[u8; 0x39 - 0x1E] = b"!@#$%^&*()\r\x1b\x7f\t _+{}|~:\"~<>?";
    let shift = modifiers & SHIFT != 0;
    match key {
        0x04..=0x1D => {
            let letter = b'a' + (key - 0x04);
            if modifiers & CTRL != 0 {
                Some(letter & 0x1F)
            } else if shift != caps_lock {
                Some(letter.to_ascii_uppercase())
            } else {
                Some(letter)
            }
        }
        0x1E..=0x38 => {
            let table = if shift { SHIFTED } else { PLAIN };
            Some(table[(key - 0x1E) as usize])
        }
        _ => None,
    }
}

fn class_name(class: u8) -> &'static str {
    match class {
        0x01 => "an audio device",
        0x02 | 0x0A => "a communications device",
        0x03 => "a HID device that isn't a keyboard",
        0x07 => "a printer",
        0x08 => "a storage device",
        0x09 => "a hub (not supported yet)",
        0x0E => "a video device",
        0xE0 => "a wireless controller",
        _ => "a device",
    }
}
