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

use pios_abi::call;
pub use pios_abi::{Error, ExitStatus, MESSAGE_WORDS, rights};

pub mod console;
mod ipc;
pub mod procman;
use ipc::close_raw;
pub use ipc::{Handle, Message, Received, Reply, endpoint, timer};

/// Make a system call with up to three arguments.
fn syscall(number: usize, a0: usize, a1: usize, a2: usize) -> Result<usize, Error> {
    syscall4(number, [a0, a1, a2, 0])
}

/// Make a system call with up to four arguments.
fn syscall4(number: usize, args: [usize; 4]) -> Result<usize, Error> {
    syscall6(number, [args[0], args[1], args[2], args[3], 0, 0])
}

/// Make a system call with up to six arguments.
fn syscall6(number: usize, args: [usize; 6]) -> Result<usize, Error> {
    let result: usize;
    // SAFETY: `svc` enters the kernel, which only changes x0, and checks
    // any pointers it is given. (It may write through them, which the
    // compiler allows for, as this isn't marked `nomem`.)
    unsafe {
        core::arch::asm!("svc #0", in("x8") number, inout("x0") args[0] => result,
                         in("x1") args[1], in("x2") args[2], in("x3") args[3],
                         in("x4") args[4], in("x5") args[5],
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

/// The handle this program was started with, if any (only the first call
/// gets it).
pub fn start_handle() -> Option<Handle> {
    match START_HANDLE.swap(0, Ordering::Relaxed) {
        0 => None,
        raw => Some(Handle::from_raw(raw)),
    }
}

/// The argument string this program was started with (by convention, its
/// command line's arguments). Empty if there was none, or it isn't UTF-8.
pub fn args() -> &'static str {
    let (addr, len) = (ARGS_ADDR.load(Ordering::Relaxed), ARGS_LEN.load(Ordering::Relaxed));
    if len == 0 {
        return "";
    }
    // SAFETY: the kernel copied the arguments to the top of our stack, above
    // where `_start` began, which nothing writes to; they stay for as long
    // as we run.
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    core::str::from_utf8(bytes).unwrap_or("")
}

static ARGUMENT: AtomicUsize = AtomicUsize::new(0);
static START_HANDLE: AtomicUsize = AtomicUsize::new(0);
static ARGS_ADDR: AtomicUsize = AtomicUsize::new(0);
static ARGS_LEN: AtomicUsize = AtomicUsize::new(0);

#[doc(hidden)]
pub fn _set_arguments(arg: usize, handle: usize, args_addr: usize, args_len: usize) {
    ARGUMENT.store(arg, Ordering::Relaxed);
    START_HANDLE.store(handle, Ordering::Relaxed);
    ARGS_ADDR.store(args_addr, Ordering::Relaxed);
    ARGS_LEN.store(args_len, Ordering::Relaxed);
}

/// A process this one started. Dropping it gives up the handle (the child
/// carries on); [`Child::wait`] waits for it to end.
pub struct Child {
    handle: Handle,
}

impl Child {
    /// The child's process handle (e.g. to pass it on).
    pub fn into_handle(self) -> Handle {
        self.handle
    }

    /// A child from a process handle someone passed us.
    pub fn from_handle(handle: Handle) -> Child {
        Child { handle }
    }

    /// Wait for the child to end, and say how.
    pub fn wait(self) -> Result<ExitStatus, Error> {
        // `wait` closes the handle itself.
        let raw = syscall(call::WAIT, self.handle.into_raw(), 0, 0)?;
        ExitStatus::from_raw(raw).ok_or(Error::InvalidArgument)
    }
}

/// Start a new process running the ELF executable `image`, with `arg` as
/// its argument, `args` as its argument string (see [`args`]) and giving it
/// `handle`, if any (which needs the `TRANSFER` right, and is gone either
/// way).
pub fn spawn(image: &[u8], arg: usize, handle: Option<Handle>, args: &str) -> Result<Child, Error> {
    let handle = handle.map_or(0, Handle::into_raw);
    let result = syscall6(
        call::SPAWN,
        [
            image.as_ptr() as usize,
            image.len(),
            arg,
            handle,
            args.as_ptr() as usize,
            args.len(),
        ],
    );
    if result.is_err() {
        close_raw(handle);
    }
    Ok(Child {
        handle: Handle::from_raw(result?),
    })
}

/// A static that the program can borrow mutably once, for data too big for
/// the stack. `take` hands out the only `&'static mut` there will ever be.
pub struct TakeOnce<T> {
    taken: core::sync::atomic::AtomicBool,
    value: core::cell::UnsafeCell<T>,
}

// SAFETY: at most one reference to the value is ever handed out.
unsafe impl<T: Send> Sync for TakeOnce<T> {}

impl<T> TakeOnce<T> {
    pub const fn new(value: T) -> TakeOnce<T> {
        TakeOnce {
            taken: core::sync::atomic::AtomicBool::new(false),
            value: core::cell::UnsafeCell::new(value),
        }
    }

    /// The value, the first time; `None` after that.
    // Handing out `&mut` from `&self` is the point, and sound: `taken`
    // makes sure it happens at most once.
    #[allow(clippy::mut_from_ref)]
    pub fn take(&'static self) -> Option<&'static mut T> {
        if self.taken.swap(true, Ordering::Acquire) {
            return None;
        }
        // SAFETY: `taken` makes this the only reference ever made.
        Some(unsafe { &mut *self.value.get() })
    }
}

/// Make a raw system call with six arguments, for testing the kernel's
/// argument checking.
pub fn raw_syscall6(number: usize, args: [usize; 6]) -> Result<usize, Error> {
    syscall6(number, args)
}

/// Make a raw system call, for testing the kernel's argument checking.
pub fn raw_syscall(number: usize, a0: usize, a1: usize, a2: usize) -> Result<usize, Error> {
    syscall(number, a0, a1, a2)
}

/// `core::fmt` output: to the console server once [`console::connect`] has
/// been called, else to the kernel's debug console.
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let result = if console::is_connected() {
            console::write(s.as_bytes())
        } else {
            debug_write(s.as_bytes()).map(|_| ())
        };
        result.map_err(|_| fmt::Error)
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

/// Define the program's entry point: `_start` records its arguments (for
/// [`argument`], [`start_handle`] and [`args`]), calls `$main` (a `fn() -> i32`) and exits with what it
/// returns.
#[macro_export]
macro_rules! pios_main {
    ($main:path) => {
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text._start")]
        pub extern "C" fn _start(arg: usize, handle: usize, args: usize, args_len: usize) -> ! {
            $crate::_set_arguments(arg, handle, args, args_len);
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
