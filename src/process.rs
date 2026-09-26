//! User programs: loading an ELF executable into a fresh address space and
//! running it at EL0.
//!
//! There is no scheduler yet: `Process::run` runs the program until it
//! exits or faults, then returns to the kernel. A faulting program is
//! stopped and reported; the kernel carries on.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::addr::PAGE_SIZE;
use crate::addrspace::{AddressSpace, USER_END, USER_START};
use crate::cache;
use crate::elf::{Elf, ElfError};
use crate::exception::{KIND, Syndrome, TrapFrame};
use crate::mmu;
use crate::paging::{Access, MapError};
use crate::sync::SpinLock;

/// The top of every program's stack; the page below the stack is left
/// unmapped, so overflowing it faults.
pub const STACK_TOP: usize = 0x40_0000_0000;
pub const STACK_PAGES: usize = 16;

unsafe extern "C" {
    /// In boot.s: run user code from `entry` with its stack at `stack`,
    /// until `return_to_kernel`.
    fn run_user(entry: usize, stack: usize) -> u64;
    /// In boot.s: make the current `run_user` call return `value`.
    fn return_to_kernel(value: u64) -> !;
}

/// Set while a program runs, so `return_to_kernel` is only ever used with a
/// `run_user` to return to.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Details of the last user fault, for `Process::run` to report.
static LAST_FAULT: SpinLock<Option<Fault>> = SpinLock::new(None);

/// `return_to_kernel` values: an exit code, or this bit for a fault.
const FAULTED: u64 = 1 << 63;

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
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoadError::Elf(e) => write!(f, "not a valid program ({e:?})"),
            LoadError::Map(e) => write!(f, "could not set up its memory ({e:?})"),
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

/// A user program, loaded and ready to run.
pub struct Process {
    name: &'static str,
    space: AddressSpace,
    entry: usize,
}

impl Process {
    /// Load the ELF executable `image` into a new address space: each
    /// segment with the permissions its flags ask for, plus a stack.
    pub fn load(name: &'static str, image: &[u8]) -> Result<Process, LoadError> {
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
        Ok(Process {
            name,
            space,
            entry: elf.entry() as usize,
        })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Run the program until it exits or faults.
    pub fn run(&self) -> Exit {
        assert!(
            !RUNNING.swap(true, Ordering::Acquire),
            "a program is already running"
        );
        self.space.activate();
        // SAFETY: the program's address space is active with its code and
        // stack mapped, and RUNNING guards the matching return_to_kernel.
        let value = unsafe { run_user(self.entry, STACK_TOP) };
        RUNNING.store(false, Ordering::Release);
        mmu::set_user_tables(None);
        if value & FAULTED != 0 {
            Exit::Fault(LAST_FAULT.lock().take().expect("a fault was recorded"))
        } else {
            Exit::Code(value as u32 as i32)
        }
    }
}

/// The running program asked to exit (from the `exit` system call).
pub fn exit(code: i32) -> ! {
    finish(code as u32 as u64)
}

/// The running program caused an exception other than a system call:
/// record it and stop the program.
pub fn user_fault(frame: &TrapFrame, index: u64) -> ! {
    *LAST_FAULT.lock() = Some(Fault {
        index,
        esr: frame.esr,
        far: frame.far,
        pc: frame.elr,
    });
    finish(FAULTED)
}

fn finish(value: u64) -> ! {
    assert!(RUNNING.load(Ordering::Acquire), "no program is running");
    // SAFETY: a program is running (checked above), so `run_user` is on the
    // kernel stack to return to; this is only called from exception
    // handlers holding no locks or other state that needs dropping.
    unsafe { return_to_kernel(value) }
}

/// The user programs built into the kernel (until there is an initramfs).
pub mod programs {
    pub static HELLO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hello.elf"));
    pub static USERTEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/usertest.elf"));
    pub static CRASHTEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/crashtest.elf"));
}

/// Run the built-in test programs: `usertest` checks the system call
/// interface from user space and must exit 0; `crashtest` reads kernel
/// memory and must be stopped by a fault. Neither may leak memory.
pub fn self_test() -> Result<(), &'static str> {
    let free_before = crate::memory::frame_stats().free;
    let run = |name, image| {
        Process::load(name, image)
            .map(|p| p.run())
            .map_err(|_| "a test program failed to load")
    };
    let ticks_before = crate::timer::tick_count();
    match run("usertest", programs::USERTEST)? {
        Exit::Code(0) => {}
        _ => return Err("usertest failed"),
    }
    // usertest computes for long enough that timer interrupts arrive while
    // it runs at EL0; its result shows they didn't disturb it.
    if crate::timer::tick_count() == ticks_before {
        return Err("no timer interrupts arrived while usertest ran");
    }
    match run("crashtest", programs::CRASHTEST)? {
        Exit::Fault(fault)
            if Syndrome(fault.esr).is_data_abort() && fault.far == 0xFFFF_FF80_0008_0000 => {}
        _ => return Err("crashtest wasn't stopped when reading kernel memory"),
    }
    if crate::memory::frame_stats().free != free_before {
        return Err("user programs leaked memory");
    }
    Ok(())
}
