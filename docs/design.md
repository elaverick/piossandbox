# pios design

pios is a microkernel: the kernel provides address spaces, threads,
scheduling, inter-process communication (IPC) and access control, and
everything else (device drivers, the console, the shell, file systems) runs
as ordinary user programs. This document records the decisions made so far
and the plan for getting there.

## Status

Done: boot on the Raspberry Pi 4 and 5, serial and HDMI console, exceptions,
GIC interrupts and a timer tick, MMU and caches, kernel heap, physical page
allocator, higher-half kernel with W^X mappings, per-process address spaces,
user mode with an ELF loader and the first system calls, threads with
preemptive round-robin scheduling (user programs are still built into the
kernel image).

Next: loading programs from an initramfs, IPC, capabilities, user-space
drivers, the shell.

## Memory layout

Virtual addresses are 39 bits wide in each half of the address space
(4 KiB pages, three levels of page table).

| Range | Contents | Page table |
| --- | --- | --- |
| `0x0000_0000_0000_0000` - `0x0000_007F_FFFF_FFFF` | the running process (user space) | TTBR0, one per process |
| `0xFFFF_FF80_0000_0000` - `0xFFFF_FFBF_FFFF_FFFF` | the kernel: linear map | TTBR1, shared |
| `0xFFFF_FFC0_0000_0000` - `0xFFFF_FFFF_FFFF_FFFF` | the kernel: thread stacks | TTBR1, shared |

The kernel half is a *linear map*: physical address `p` appears at
`0xFFFF_FF80_0000_0000 + p`, so the kernel can reach any RAM or device by
adding a constant. The kernel image itself is linked at its place in that
map (`0xFFFF_FF80_0008_0000`, as the firmware loads it at physical
`0x80000`). Within the kernel half:

- RAM (from the device tree's `/memory` node) is read-write and never
  executable, except the kernel's own image, which is mapped per section:
  code read-execute, constants read-only, data read-write.
- Peripherals are Device memory; the framebuffer is Normal non-cacheable.
- Each thread's kernel stack is mapped above the linear map, in a slot with
  an unmapped guard below it.
- Nothing else is mapped: stray accesses fault.

User address spaces map only their own memory, never the kernel's, and
page 0 is never mapped so null pointers fault.

In Rust, physical and virtual addresses are different types (`PhysAddr`,
`VirtAddr`), and page permissions can only be read-only, read-write or
read-execute, so a writable and executable page can't be requested.

## Physical memory

A frame allocator hands out 4 KiB physical pages from the RAM the device
tree describes, minus the kernel, the device tree, the GPU's memory and any
reserved ranges. Frames are owned values (`OwnedFrame`): dropping one
returns it, so address spaces and page tables free their memory when they
go away. The kernel heap is carved from it at boot.

## IPC

IPC is **synchronous send / receive / reply**, as in L4, seL4 and QNX:

- A client *calls* an endpoint: it sends a message and blocks until the
  reply arrives.
- A server *receives* on an endpoint, blocking until a message arrives,
  then *replies*.
- Short messages travel in registers; bulk data goes through shared memory
  that one side maps for the other.

## Access control: capabilities

Every process has a **handle table**, like Unix file descriptors. A handle
refers to a kernel object and carries **rights**; system calls name the
handle they act on, and holding the handle is the permission. The kernel
owns the table, so handles can't be forged. Handles can be passed in IPC
messages, and duplicated with the same or fewer rights, never more.

Kernel objects:

| Object | What a handle to it allows |
| --- | --- |
| Endpoint | send and call (clients), receive (the server) |
| Reply | reply once to the caller waiting on it |
| Memory | map a range of physical memory (RAM, or specific device registers) |
| Interrupt | wait for and acknowledge one interrupt |
| Process, thread | start, stop, inspect |
| Address space | map memory into a process |

Two ideas from seL4:

- **Badged endpoints.** A server can hand each client a copy of its
  endpoint handle stamped with a number (a badge). Messages sent through
  that copy arrive with the badge, so the server knows which client is
  talking without trusting any global ID.
- **One-shot reply capabilities.** A call gives the server a Reply object
  that can be used exactly once, to reply to exactly that caller.

Kernel objects are reference counted and destroyed when the last handle to
them closes; that is also the only form of revocation for now. Kernel memory
for objects comes from the kernel heap, with per-process limits. There are
no global names: finding a service is a user-space job.

**Start-up.** The kernel starts one user program, `init`, holding handles to
all free memory, every device and every interrupt. `init` starts each
server with only what it needs (for example the console server gets the
UART's registers, its interrupt and a fresh endpoint), and the shell gets
handles to the console and process manager endpoints and nothing else.

Until the console server exists, the kernel keeps a temporary **debug print**
system call so early user programs can show they are alive. It will be
removed once the console server works.

## System calls

`svc #0`, with the call number in `x8` and up to six arguments in `x0`-`x5`.
The result comes back in `x0`: non-negative for success, or a negative error
code. The numbers and codes live in the `pios-abi` crate, which the kernel
and `libpios` both use.

| Number | Call |
| --- | --- |
| 0 | `debug_write(ptr, len)`: write to the kernel console (temporary) |
| 1 | `exit(code)` |
| 2 | `yield()` |

IPC calls (send, receive, call, reply) and handle management come next.

Every pointer argument is checked with the MMU, from the calling program's
point of view, before the kernel touches the memory.

## Threads and scheduling

A thread is the unit the scheduler runs: a kernel stack, a saved context
and, for user threads, the process (address space) it runs in. Each process
has one thread for now; a system call to start more will come with the
process manager.

- Scheduling is round-robin and preemptive: the 100 Hz timer tick ends each
  time slice. When nothing is ready, an idle thread waits for interrupts.
  Priorities can come later, if the servers need them.
- Threads block with `park` and are woken with `unpark`, which remembers a
  wakeup that arrives early (as Rust's `std::thread::park` does), so a
  thread can check a condition, register itself and park without losing a
  wakeup in between. IPC will block and wake threads the same way.
- The context switch saves the callee-saved registers, stack pointer,
  TPIDR_EL0, FPCR/FPSR and all FP/SIMD registers. Saving FP/SIMD state on
  every switch is simple and cheap next to the TLB flush; switching it
  lazily (trapping a thread's first FP use) can come later if it matters.
- A thread that finishes is freed by the next thread to run, since its own
  stack is in use until the switch.
- Only one core runs for now. The scheduler's lock masks interrupts, and a
  switch runs with interrupts masked, which on one core makes it atomic.
  Using the other cores needs per-core run queues and current-thread state.
- Without address space IDs, switching between processes flushes the TLB.

## User programs

Written in Rust, `no_std`, on a small runtime crate (`libpios`: entry point,
system call wrappers, `print!`, panic handler; a heap will follow), and
built as statically linked ELF executables linked at `0x40_0000`, with
floating point. The kernel itself never uses the FP/SIMD registers, so a
program's are preserved across system calls, and the scheduler saves and
restores them when switching threads. A program's entry point gets one
argument in `x0`, and programs may read the system counter (`CNTVCT_EL0`)
directly for the time. They reach the board in an initramfs: a
cpio archive the firmware loads after the kernel (`initramfs ... followkernel`
in `config.txt`) and describes in the device tree.

## Roadmap

1. ~~Physical page allocator; map all RAM~~
2. ~~Higher-half kernel; per-process address spaces~~
3. ~~User mode, system call interface, debug print; a first user program~~
4. ~~Threads, context switching (including FP/SIMD state), preemptive scheduling~~
5. initramfs (cpio) and an ELF loader; the kernel starts `/init`
6. IPC and capabilities
7. User-space console server (device memory and interrupt handles)
8. Process manager and the shell
