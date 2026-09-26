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
starts `init`, and `init` the rest, synchronous IPC with handles, rights,
badges and one-shot reply handles, and handles for device memory,
interrupts and timers, with which a user-space console server drives the
serial ports and the screen, and a process manager and shell.

Next: see the [Roadmap](#roadmap).

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
- A plain *send* waits only until a receiver has taken the message.

As built (`src/ipc.rs`):

- **Messages** are 128 bytes in the caller's memory (`pios_abi::Message`): a
  label saying what the message means, 12 data words, and at most one
  handle, which moves to the receiver. On receipt the kernel adds the
  sender's badge and, for a call, a reply handle. Messages are copied
  through the kernel rather than passed in registers; registers could come
  later as a fast path if copying shows up as a cost. Bulk data will go
  through shared memory that one side maps for the other.
- **Rendezvous.** Whichever side arrives first waits in the endpoint's
  queue (senders and receivers each in arrival order). Messages pass
  between threads in kernel form; each thread copies to and from its own
  memory and installs handles in its own table.
- **When one side is gone.** An endpoint counts the handles that can send
  to it and receive from it. When the last of either kind closes, the other
  side is woken with `PeerGone`, and later attempts fail at once, as when
  one end of a Rust channel is dropped. A reply handle dropped unanswered,
  even by a server that exits, fails the call with `PeerGone` too.
- **Moving handles** needs the `TRANSFER` right. A handle in a message that
  isn't delivered is closed.

- **Notifications.** Interrupts and timers can't wait for a receiver, so
  they *notify* an endpoint instead: their badges are ORed into a pending
  word, which the next receive collects as one message (label `NOTIFY`),
  ahead of any waiting sender, as seL4's notifications do. A server waits
  for clients and hardware in the same `receive`, and tells them apart by
  badge. An interrupt is masked when it arrives until its owner
  acknowledges it, so a level-triggered device can't flood the system.

Known gaps, for later: no timeouts or non-blocking variants; a thread
waiting on IPC can't be interrupted (there is no way to stop a process
yet); and an endpoint can be kept alive by a cycle, such as a message
queued on an endpoint that carries a handle to that same endpoint's
receiving side.

## Access control: capabilities

Every process has a **handle table**, like Unix file descriptors. A handle
refers to a kernel object and carries **rights**; system calls name the
handle they act on, and holding the handle is the permission. The kernel
owns the table, so handles can't be forged. Handles can be passed in IPC
messages, and duplicated with the same or fewer rights, never more.

Handle numbers are never reused within a process, so a stale number can't
name a newer object, and closing a handle twice is harmless. A process may
hold up to 1024. The rights so far (`pios_abi::rights`):

| Right | Allows | Held by |
| --- | --- | --- |
| `SEND` | send and call | endpoint handles |
| `RECEIVE` | receive | endpoint handles |
| `DUPLICATE` | making copies (with the same or fewer rights) | endpoint handles |
| `TRANSFER` | passing the handle on, in a message or to `spawn` | every kind |
| `WAIT` | waiting for the process to end | process handles |
| `MAP` | mapping the memory; allocating DMA memory | memory and DMA handles |

`endpoint()` returns a handle with the first four. `spawn` can give the new
process one handle, which is how a parent sets up the first channel to a
child.

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
the hardware it can hand over, described by a `BootInfo` page. `init`
starts each server with only what it needs, and the shell will get handles
to the console and process manager endpoints and nothing else.

So far that hardware is the console: each UART's registers and interrupt,
the framebuffer, and the text the kernel left on the screen, which `init`
passes to the console server in set-up messages (through a handle with a
set-up badge, so clients can't send them). Later `init` should get handles
to all free memory and every device and interrupt, as planned.

The kernel keeps its temporary **debug print** system call for programs
without a console handle (the kernel's own test programs, and anything
that fails before connecting). It can go once those have a console too.

Kernel objects so far: endpoints, reply handles, processes, memory (device
registers, a framebuffer, or read-only kernel data), interrupts, timers,
and DMA handles.

A **DMA handle** lets a driver allocate memory for its device: physically
contiguous, zeroed, uncached, below the highest address the device can
reach, returned with the address the device sees it at (RP1, on the Pi 5,
sees RAM through PCIe at an offset). The memory belongs to the driver's
address space. There is no IOMMU on either board, so a device can be told
to read or write any memory, and a DMA handle is as powerful as the device
it goes with: only drivers get one. If a driver dies while its device is
still using its memory, the memory is freed and may be reused under the
device's feet; stopping the device first is the driver's job for now.

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
| 3 | `spawn(ptr, len, arg, handle, args_ptr, args_len) -> handle` |
| 4 | `wait(handle) -> exit status` |
| 5 | `close(handle)` |
| 6 | `endpoint() -> handle` |
| 7 | `duplicate(handle, rights, badge) -> handle` |
| 8 | `send(endpoint, message)` |
| 9 | `call(endpoint, message)`: the reply replaces the message |
| 10 | `receive(endpoint, message)` |
| 11 | `reply(reply, message)` |
| 12 | `map(memory, address)` |
| 13 | `interrupt_bind(interrupt, endpoint, badge)` |
| 14 | `interrupt_ack(interrupt)` |
| 15 | `timer(endpoint, badge, period_ms) -> handle` |
| 16 | `dma_alloc(dma, address, pages) -> bus address` |

The details of each are in the `pios-abi` crate.

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
restores them when switching threads. A program's entry point gets a
number in `x0`, a start handle in `x1`, and an argument string (its command
line's arguments, copied onto its stack by the kernel) in `x2`/`x3`; and
programs may read the system counter (`CNTVCT_EL0`) directly for the
time. They reach the board in the boot image (see
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

## The process manager and the shell

The **process manager** runs programs by name: `init` gives it the boot
image (as a read-only memory handle) and a console handle it can copy, and
it answers `RUN` (a command line) by starting the program with a console
handle and its arguments and handing back the process handle, and `LIST`
with the programs' names. Today it is the only thing that knows where
programs come from, so a file system can later slot in behind it without
the shell changing.

The **shell** gets a handle to the process manager (and its console through
it) and nothing else. It runs built-ins itself and everything else through
the process manager.

How a program finds the services it needs is still ad hoc: it gets one
start handle, and asks that service for others (the shell gets its console
from the process manager). A small naming or bootstrap service, handing
out handles to named services according to what a program is allowed, is
the obvious next step once there are more services.

## Roadmap

1. ~~Physical page allocator; map all RAM~~
2. ~~Higher-half kernel; per-process address spaces~~
3. ~~User mode, system call interface, debug print; a first user program~~
4. ~~Threads, context switching (including FP/SIMD state), preemptive scheduling~~
5. ~~Boot image; the kernel starts `init`, which starts the rest (was:
   initramfs; see [Decisions](#decisions))~~
6. ~~IPC and capabilities~~ (endpoints, replies and process handles so
   far; memory and interrupt handles come with step 7)
7. ~~User-space console server (device memory and interrupt handles)~~
8. ~~Process manager and the shell~~

Next, roughly in order:

9. Memory for programs: a system call to map fresh memory, so `libpios`
   can offer a heap (`alloc`), and shared memory between processes for
   bulk data
10. Stopping processes (Ctrl-C in the shell), and a process list (`ps`)
11. An SD card driver and a FAT file system server, so programs can come
    from the card (and the boot image shrinks; see [Decisions](#decisions))
12. The other cores
13. USB (behind PCIe on both boards) for a keyboard: done for the Pi 5
    (RP1's xHCIs, root ports only, polled); the Pi 4's VL805 needs the
    BCM2711's PCIe controller brought up first

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
