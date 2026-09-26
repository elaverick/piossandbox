//! User programs: loading an ELF executable into a fresh address space and
//! starting a thread to run it at EL0.
//!
//! A process is an address space, a handle table and, for now, exactly one
//! thread. When the thread exits or faults, the process ends, and its memory
//! and handles are freed with the thread.
//!
//! The kernel starts one process itself, `init`, from the boot image; `init`
//! starts the rest with the `spawn` system call.

use alloc::sync::Arc;
use core::fmt;

use crate::addr::PAGE_SIZE;
use crate::addrspace::{AddressSpace, USER_END, USER_START};
use crate::bootimage;
use crate::cache;
use crate::elf::{Elf, ElfError};
use crate::exception::{KIND, Syndrome, TrapFrame};
use crate::handle::{Handle, HandleTable};
use crate::paging::{Access, MapError};
use crate::sync::SpinLock;
use crate::thread::{self, JoinHandle};

/// The top of every program's stack; the page below the stack is left
/// unmapped, so overflowing it faults.
pub const STACK_TOP: usize = 0x40_0000_0000;
pub const STACK_PAGES: usize = 16;

/// Where `init` finds the boot image (read-only): the argument to its entry
/// point.
pub const BOOT_IMAGE_ADDR: usize = 0x10_0000_0000;

/// Why a program stopped.
#[derive(Clone, Copy, Debug)]
pub enum Exit {
    /// It called `exit`.
    Code(i32),
    /// It faulted.
    Fault(Fault),
}

/// A fault that stopped a program.
#[derive(Clone, Copy, Debug)]
pub struct Fault {
    /// Exception vector index (kind and source).
    pub index: u64,
    pub esr: u64,
    /// The faulting address, for aborts.
    pub far: u64,
    /// The program counter.
    pub pc: u64,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let syndrome = Syndrome(self.esr);
        write!(
            f,
            "{} exception: {}",
            KIND[(self.index & 3) as usize],
            syndrome
        )?;
        if syndrome.is_data_abort() || self.esr >> 26 == 0x20 {
            write!(f, " at {:#x}", self.far)?;
        }
        write!(f, ", pc {:#x}", self.pc)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LoadError {
    Elf(ElfError),
    Map(MapError),
    /// No memory for its thread.
    NoThread,
    /// Not in the boot image.
    Missing,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoadError::Elf(e) => write!(f, "not a valid program ({e:?})"),
            LoadError::Map(e) => write!(f, "could not set up its memory ({e:?})"),
            LoadError::NoThread => write!(f, "no memory for its thread"),
            LoadError::Missing => write!(f, "not in the boot image"),
        }
    }
}

impl From<ElfError> for LoadError {
    fn from(e: ElfError) -> Self {
        LoadError::Elf(e)
    }
}

impl From<MapError> for LoadError {
    fn from(e: MapError) -> Self {
        LoadError::Map(e)
    }
}

/// A user program's process: its address space, shared by its thread(s).
pub struct Process {
    space: AddressSpace,
    handles: SpinLock<HandleTable>,
}

impl Process {
    /// Load the ELF executable `image` into a new address space: each
    /// segment with the permissions its flags ask for, plus a stack.
    /// Returns the process and its entry point.
    fn load(image: &[u8]) -> Result<(Process, usize), LoadError> {
        let elf = Elf::new(image, USER_START as u64, USER_END as u64)?;
        let mut space = AddressSpace::new()?;
        for segment in elf.segments() {
            let (start, end) = segment.pages();
            let access = if segment.is_executable() {
                Access::USER_READ_EXECUTE
            } else if segment.is_writable() {
                Access::USER_READ_WRITE
            } else {
                Access::USER_READ
            };
            let (start, pages) = (start as usize, (end - start) as usize / PAGE_SIZE);
            space.allocate(start, pages, access)?;
            space.write(segment.vaddr as usize, segment.data)?;
        }
        // The code was written through the data cache; make sure
        // instruction fetches see it.
        cache::invalidate_instruction_cache();
        space.allocate(
            STACK_TOP - STACK_PAGES * PAGE_SIZE,
            STACK_PAGES,
            Access::USER_READ_WRITE,
        )?;
        let process = Process {
            space,
            handles: SpinLock::new(HandleTable::new()),
        };
        Ok((process, elf.entry() as usize))
    }

    pub fn space(&self) -> &AddressSpace {
        &self.space
    }

    pub fn handles(&self) -> &SpinLock<HandleTable> {
        &self.handles
    }
}

/// Load the ELF executable `image` as a new process and start running it,
/// with `arg` as the first argument to its entry point. If there is a
/// `handle`, the new process gets it, and its number there as the second
/// argument. Join the returned handle to wait for it to end.
pub fn spawn(
    name: &'static str,
    image: &[u8],
    arg: usize,
    handle: Option<Handle>,
) -> Result<JoinHandle, LoadError> {
    let (process, entry) = Process::load(image)?;
    start(name, process, entry, arg, handle)
}

/// Start the program `name` from the boot image, with the whole boot image
/// mapped read-only at `BOOT_IMAGE_ADDR`, which is its argument. That is how
/// the kernel starts `init` (and its IPC self-test).
pub fn spawn_from_boot_image(name: &'static str) -> Result<JoinHandle, LoadError> {
    let image = bootimage::image();
    let program = image.find(name).ok_or(LoadError::Missing)?;
    let (mut process, entry) = Process::load(program.data)?;
    process
        .space
        .map_static(BOOT_IMAGE_ADDR, image.as_bytes())?;
    start(name, process, entry, BOOT_IMAGE_ADDR, None)
}

fn start(
    name: &'static str,
    process: Process,
    entry: usize,
    arg: usize,
    handle: Option<Handle>,
) -> Result<JoinHandle, LoadError> {
    let handle = match handle {
        Some(handle) => {
            let number = process.handles.lock().insert(handle);
            number.unwrap_or_else(|_| unreachable!("a new handle table has room"))
        }
        None => 0,
    };
    thread::spawn_user(name, Arc::new(process), entry, STACK_TOP, [arg, handle])
        .map_err(|_| LoadError::NoThread)
}

/// The running program asked to exit (from the `exit` system call).
pub fn exit(code: i32) -> ! {
    thread::exit(Exit::Code(code))
}

/// The running program caused an exception other than a system call: stop
/// it.
pub fn user_fault(frame: &TrapFrame, index: u64) -> ! {
    thread::exit(Exit::Fault(Fault {
        index,
        esr: frame.esr,
        far: frame.far,
        pc: frame.elr,
    }))
}

/// Run the test programs from the boot image, all at once so they take turns:
///
/// - `usertest` checks the system call interface from user space;
/// - two `fptest`s each fill the FP/SIMD registers with their own values
///   and check them while being switched in and out;
/// - `crashtest` reads kernel memory and must be stopped by a fault;
/// - `ipctest` checks IPC and handles, as two processes.
///
/// None may leak memory, threads or endpoints.
pub fn self_test() -> Result<(), &'static str> {
    use crate::memory::frame_stats;
    use crate::timer::tick_count;

    let free_before = frame_stats().free;
    let endpoints_before = crate::ipc::live_endpoints();
    let before = thread::stats();
    let ticks_before = tick_count();
    let start = |name, arg| {
        let image = bootimage::program(name).ok_or("a test program is missing")?;
        spawn(name, image, arg, None).map_err(|_| "a test program failed to load")
    };
    let usertest = start("usertest", 0)?;
    let fptests = [start("fptest", 1)?, start("fptest", 2)?];
    let crashtest = start("crashtest", 0)?;
    let ipctest = spawn_from_boot_image("ipctest").map_err(|_| "ipctest failed to load")?;

    if !matches!(usertest.join(), Exit::Code(0)) {
        return Err("usertest failed");
    }
    for fptest in fptests {
        match fptest.join() {
            Exit::Code(0) => {}
            Exit::Code(_) => return Err("a thread's FP/SIMD registers were disturbed"),
            Exit::Fault(_) => return Err("fptest faulted"),
        }
    }
    match crashtest.join() {
        Exit::Fault(fault)
            if Syndrome(fault.esr).is_data_abort() && fault.far == 0xFFFF_FF80_0008_0000 => {}
        _ => return Err("crashtest wasn't stopped when reading kernel memory"),
    }
    if !matches!(ipctest.join(), Exit::Code(0)) {
        return Err("ipctest failed");
    }

    // The programs compute for long enough that the timer tick preempts
    // them, and the results show that didn't disturb them.
    let after = thread::stats();
    if tick_count() == ticks_before {
        return Err("no timer interrupts arrived while the test programs ran");
    }
    if after.preemptions < before.preemptions + MIN_PREEMPTIONS {
        return Err("the test programs weren't preempted");
    }
    if after.threads != before.threads {
        return Err("finished threads weren't freed");
    }
    if frame_stats().free != free_before {
        return Err("user programs leaked memory");
    }
    if crate::ipc::live_endpoints() != endpoints_before {
        return Err("endpoints leaked");
    }
    Ok(())
}

/// How many times, at least, the test programs must be preempted.
const MIN_PREEMPTIONS: u64 = 4;
