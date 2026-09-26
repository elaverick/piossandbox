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
preemptive round-robin scheduling, a boot image from which the kernel
starts `init`, and `init` the rest, with the first handles (to child
processes).

Next: IPC, capabilities, user-space drivers, the shell.

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
| 3 | `spawn(ptr, len, arg) -> handle` |
| 4 | `wait(handle) -> exit status` |
| 5 | `close(handle)` |

IPC calls (send, receive, call, reply) and more handle management come next.

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
directly for the time. They reach the board in the boot image (see
[Decisions](#decisions)).

## Start-up and the boot image

The kernel carries a **boot image**: a small read-only archive of the
programs needed to start the system. At boot, the kernel:

1. finds `init` in the boot image, and nothing else (apart from the test
   programs its own self-test runs);
2. loads `init` and maps the whole boot image read-only into its address
   space, passing its address as `init`'s argument (until there are
   handles for memory, when it will be a read-only memory handle);
3. waits for `init`.

`init` reads the boot image itself and starts the other programs with the
`spawn` system call, which loads an ELF executable from the caller's memory
into a new process and returns a handle to it. `wait` on that handle waits
for the process to end and returns how it ended; `close` gives the handle
up. These are the first handles: each process has a handle table, which
IPC and capabilities will extend with more kinds of object and with rights.
Until then, any process may call `spawn`.

The format (`bootfs/`, used by the build to write it and by the kernel and
`init` to read it; all little-endian):

| Part | Contents |
| --- | --- |
| header, 32 bytes | magic `piosboot`, format version (1), number of files, total size |
| directory, 64 bytes per file | offset and size of the file, and its name (up to 48 bytes of UTF-8) |
| files | each starting on a 4 KiB page boundary, in directory order |

The image's total size is a whole number of pages too. Page alignment lets
the image be mapped straight into a process, and a program's read-only
segments be mapped from it rather than copied (the ELF loader copies them
for now).

## Roadmap

1. ~~Physical page allocator; map all RAM~~
2. ~~Higher-half kernel; per-process address spaces~~
3. ~~User mode, system call interface, debug print; a first user program~~
4. ~~Threads, context switching (including FP/SIMD state), preemptive scheduling~~
5. ~~Boot image; the kernel starts `init`, which starts the rest (was:
   initramfs; see [Decisions](#decisions))~~
6. IPC and capabilities
7. User-space console server (device memory and interrupt handles)
8. Process manager and the shell

## Decisions

Decisions worth revisiting, with what was weighed at the time.

### Boot image instead of an initramfs

*Decided 2026-09-26. Revisit once there is a file system.*

**Question.** How do the first programs reach memory at boot, in what
format, and who unpacks them? Linux's answer (an initramfs: a cpio archive
the bootloader loads, which the kernel unpacks into a RAM file system)
suits a kernel that has a file system to unpack into; a microkernel
doesn't.

**What other systems do.**

| System | Boot-time programs | Who unpacks them |
| --- | --- | --- |
| Linux | cpio archive loaded next to the kernel | the kernel, into a RAM file system |
| seL4 | linked into one boot image with the kernel | the kernel starts one root task with every capability; it starts the rest |
| Fuchsia | "bootfs" in the boot image: a directory, then page-aligned files | the kernel starts `userboot`, which reads bootfs |
| QNX | image file system built by `mkifs`, with a boot script | a startup program and the process manager |
| GNU Hurd, L4Re, NOVA | boot modules: separate files the bootloader loads | the kernel hands them to its first task |
| Redox, Plan 9 | files built into the kernel image | early user space, or the kernel |

**Decision.** Three separate choices:

- *Who unpacks:* user space, as in seL4 and Fuchsia. The kernel only finds
  `init` and hands it the whole image; `init` starts everything else. This
  fits the plan for `init` to start with every capability, keeps archive
  parsing out of the kernel, and means the kernel's ELF loader serves one
  program at boot.
- *Format:* our own, page-aligned, like Fuchsia's bootfs (see
  [Start-up and the boot image](#start-up-and-the-boot-image)). One small
  crate both writes it (at build time) and reads it (kernel and `init`),
  with host tests. Page alignment allows mapping files without copying.
  Rejected: `tar`, which every system can create and inspect but whose
  512-byte blocks force copying and whose headers are awkward to parse;
  `cpio`, with the same problems and no advantage beyond Linux habit.
- *How it gets into memory:* linked into the kernel image, as in seL4 and
  Redox. One file on the SD card, so kernel and programs can't get out of
  step, and it works unchanged in every QEMU set-up we test with. The cost
  is relinking the kernel when a program changes, which the build does
  anyway. Rejected for now: a separate file loaded by the firmware
  (`initramfs <file> followkernel` in `config.txt`, which despite the name
  loads any file and records where in the device tree's `/chosen` node;
  QEMU's `-initrd` does the same). That needs the device tree, which some of
  our QEMU runs don't have, and support in the Pi 5 test model's firmware
  stub. Since the kernel only passes a memory range to `init`, switching
  later only changes how the kernel finds that range.

**Revisit when** there is a user-space SD card driver and file system
server. Then the boot image need only carry what it takes to reach the
file system (`init`, the console and process manager, the SD and file
system servers), and everything else, the shell included, can load from
the card, as QNX and Fuchsia do. At that point, reconsider:

- whether the boot image should move out of the kernel into a separate,
  firmware-loaded file (for updating programs without rebuilding the
  kernel);
- whether a standard format (`tar`) would be worth its costs, for
  inspecting and editing images with ordinary tools;
- whether `init` should map read-only segments from the image instead of
  copying them.
