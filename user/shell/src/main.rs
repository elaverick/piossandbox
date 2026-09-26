//! The shell: reads command lines from the console and runs them.
//!
//! `init` starts it with a handle to the process manager, from which it gets
//! its console. A command is a built-in (see `help`) or the name of a
//! program in the boot image followed by its arguments, which the process
//! manager starts; the shell waits for it and says if it failed.
//!
//! Line editing: printable characters are added to the line, Backspace or
//! Delete removes the last one, Ctrl-C abandons the line and Enter runs it.

#![no_std]
#![no_main]

use libpios::console::{self, MAX_BYTES};
use libpios::procman::{self, RunError};
use libpios::{ExitStatus, Handle, print, println};

libpios::pios_main!(main);

const PROMPT: &str = "pios> ";

/// The longest command line (what fits in a message to the process
/// manager).
const MAX_LINE: usize = MAX_BYTES;

const BUILTINS: [(&str, &str); 4] = [
    ("help", "list the commands"),
    ("echo", "print its arguments"),
    ("uptime", "how long since the system started"),
    ("exit", "leave the shell (init starts a new one)"),
];

/// Output collected while handling a batch of input, so typing a burst
/// doesn't take a message per character echoed.
struct Output {
    bytes: [u8; 256],
    len: usize,
}

impl Output {
    fn push(&mut self, bytes: &[u8]) {
        if self.len + bytes.len() > self.bytes.len() {
            self.flush();
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn flush(&mut self) {
        let _ = console::write(&self.bytes[..self.len]);
        self.len = 0;
    }
}

struct Shell {
    procman: Handle,
    line: [u8; MAX_LINE],
    len: usize,
    output: Output,
}

/// What to do after a line.
enum Next {
    Continue,
    Exit,
}

impl Shell {
    /// Handle one byte of input.
    fn input(&mut self, byte: u8) -> Next {
        match byte {
            b'\r' | b'\n' => {
                self.output.push(b"\n");
                self.output.flush();
                let len = core::mem::take(&mut self.len);
                let mut line = [0u8; MAX_LINE];
                line[..len].copy_from_slice(&self.line[..len]);
                // Only printable ASCII gets into the line, so it is UTF-8.
                let line = core::str::from_utf8(&line[..len]).unwrap_or("");
                if let Next::Exit = self.run(line) {
                    return Next::Exit;
                }
                self.output.push(PROMPT.as_bytes());
            }
            0x08 | 0x7F if self.len > 0 => {
                self.len -= 1;
                self.output.push(b"\x08 \x08");
            }
            0x03 => {
                self.len = 0;
                self.output.push(b"^C\n");
                self.output.push(PROMPT.as_bytes());
            }
            0x20..=0x7E if self.len < MAX_LINE => {
                self.line[self.len] = byte;
                self.len += 1;
                self.output.push(&[byte]);
            }
            _ => {}
        }
        Next::Continue
    }

    /// Run one command line.
    fn run(&mut self, line: &str) -> Next {
        let line = line.trim();
        let (command, args) = line.split_once(' ').unwrap_or((line, ""));
        let args = args.trim_start();
        match command {
            "" => {}
            "help" => self.help(),
            "echo" => println!("{}", args),
            "uptime" => {
                let ticks = libpios::counter();
                let hz = libpios::counter_frequency().max(1);
                println!("up {}.{:02} seconds", ticks / hz, ticks % hz * 100 / hz);
            }
            "exit" => return Next::Exit,
            _ => self.start(command, line),
        }
        Next::Continue
    }

    fn help(&self) {
        println!("Built-in commands:");
        for (name, what) in BUILTINS {
            println!("  {:<8} {}", name, what);
        }
        print!("Programs:");
        let mut name = [0u8; MAX_BYTES];
        let mut index = 0;
        while let Some(len) = procman::program(&self.procman, index, &mut name) {
            print!(" {}", core::str::from_utf8(&name[..len]).unwrap_or("?"));
            index += 1;
        }
        println!();
    }

    /// Run a program and wait for it.
    fn start(&self, name: &str, line: &str) {
        let status = match procman::run(&self.procman, line) {
            Ok(child) => child.wait(),
            Err(RunError::NotFound) => {
                println!("shell: {}: command not found", name);
                return;
            }
            Err(RunError::TooLong) => {
                println!("shell: command line too long");
                return;
            }
            Err(RunError::Failed(error)) => {
                println!("shell: {}: couldn't start it ({:?})", name, error);
                return;
            }
        };
        match status {
            Ok(ExitStatus::Code(0)) => {}
            Ok(ExitStatus::Code(code)) => println!("[{} exited with code {}]", name, code),
            Ok(ExitStatus::Fault { esr }) => {
                println!("[{} was stopped by a fault: {}]", name, describe(esr))
            }
            Err(error) => println!("shell: {}: lost track of it ({:?})", name, error),
        }
    }
}

/// A fault's exception syndrome, in words.
fn describe(esr: u32) -> impl core::fmt::Display {
    struct Syndrome(u32);
    impl core::fmt::Display for Syndrome {
        fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            let what = match self.0 >> 26 {
                0x00 => "undefined instruction",
                0x20 | 0x21 => "bad instruction fetch",
                0x22 => "misaligned PC",
                0x24 | 0x25 => "bad memory access",
                0x26 => "misaligned stack",
                0x3C => "breakpoint",
                _ => "exception",
            };
            write!(f, "{} (syndrome {:#x})", what, self.0)
        }
    }
    Syndrome(esr)
}

fn main() -> i32 {
    let Some(procman) = libpios::start_handle() else {
        return 1;
    };
    match procman::console(&procman) {
        Ok(handle) => console::connect(handle),
        Err(_) => return 1,
    }
    let mut shell = Shell {
        procman,
        line: [0; MAX_LINE],
        len: 0,
        output: Output {
            bytes: [0; 256],
            len: 0,
        },
    };
    println!();
    println!("Welcome to the pios shell. Type 'help' for the commands.");
    print!("{}", PROMPT);

    let mut input = [0u8; MAX_BYTES];
    loop {
        let Ok(count) = console::read(&mut input) else {
            return 1;
        };
        for &byte in &input[..count] {
            if let Next::Exit = shell.input(byte) {
                return 0;
            }
        }
        shell.output.flush();
    }
}
