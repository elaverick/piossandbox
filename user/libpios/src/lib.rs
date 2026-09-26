//! The runtime for pios user programs: the entry point, system call
//! wrappers, `print!`/`println!` and a panic handler.
//!
//! A program looks like:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! libpios::pios_main!(main);
//!
//! fn main() -> i32 {
//!     libpios::println!("hello");
//!     0
//! }
//! ```

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use pios_abi::Error;
use pios_abi::call;

/// Make a system call with up to three arguments.
fn syscall(number: usize, a0: usize, a1: usize, a2: usize) -> Result<usize, Error> {
    let result: usize;
    // SAFETY: `svc` enters the kernel, which only changes x0 and checks
    // any pointers it is given.
    unsafe {
        core::arch::asm!("svc #0", in("x8") number, inout("x0") a0 => result, in("x1") a1, in("x2") a2,
                         options(nostack));
    }
    Error::from_result(result)
}

/// Write bytes to the kernel console (temporary, until there is a console
/// server). Returns how many were written.
pub fn debug_write(bytes: &[u8]) -> Result<usize, Error> {
    let mut written = 0;
    for chunk in bytes.chunks(pios_abi::DEBUG_WRITE_MAX) {
        written += syscall(call::DEBUG_WRITE, chunk.as_ptr() as usize, chunk.len(), 0)?;
    }
    Ok(written)
}

/// End this process with `code`.
pub fn exit(code: i32) -> ! {
    let _ = syscall(call::EXIT, code as usize, 0, 0);
    unreachable!("exit returned")
}

/// Let other threads run for the rest of this time slice.
pub fn yield_now() {
    let _ = syscall(call::YIELD, 0, 0, 0);
}

/// The current time, in ticks of the system counter (see
/// [`counter_frequency`]), from some point before the program started.
pub fn counter() -> u64 {
    let ticks: u64;
    // SAFETY: the kernel lets user programs read the virtual counter. The
    // `isb` stops the read being done early.
    unsafe {
        core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) ticks, options(nomem, nostack))
    };
    ticks
}

/// Ticks per second of [`counter`].
pub fn counter_frequency() -> u64 {
    let hz: u64;
    // SAFETY: readable whenever the counter is.
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) hz, options(nomem, nostack)) };
    hz
}

/// The argument this program was started with.
pub fn argument() -> usize {
    ARGUMENT.load(Ordering::Relaxed)
}

static ARGUMENT: AtomicUsize = AtomicUsize::new(0);

#[doc(hidden)]
pub fn _set_argument(arg: usize) {
    ARGUMENT.store(arg, Ordering::Relaxed);
}

/// Make a raw system call, for testing the kernel's argument checking.
pub fn raw_syscall(number: usize, a0: usize, a1: usize, a2: usize) -> Result<usize, Error> {
    syscall(number, a0, a1, a2)
}

/// `core::fmt` output to the debug console.
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        debug_write(s.as_bytes()).map(|_| ()).map_err(|_| fmt::Error)
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let _ = fmt::Write::write_fmt(&mut Console, args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Define the program's entry point: `_start` records its argument (for
/// [`argument`]), calls `$main` (a `fn() -> i32`) and exits with what it
/// returns.
#[macro_export]
macro_rules! pios_main {
    ($main:path) => {
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text._start")]
        pub extern "C" fn _start(arg: usize) -> ! {
            $crate::_set_argument(arg);
            let main: fn() -> i32 = $main;
            $crate::exit(main())
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", info.message());
    if let Some(location) = info.location() {
        println!("    at {}", location);
    }
    exit(101)
}
