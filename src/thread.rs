//! Threads and the scheduler.
//!
//! Every thread has its own kernel stack. Kernel threads run a Rust closure
//! at EL1; user threads run a program at EL0 in their process's address
//! space, entering the kernel on their own stack for system calls and
//! interrupts.
//!
//! Scheduling is round-robin and preemptive: each timer tick ends the
//! running thread's time slice, and the next ready thread runs. A thread can
//! also give up the rest of its slice (`yield_now`), wait (`park`, until
//! some other thread or interrupt handler calls `unpark`), or finish
//! (`exit`). When no thread is ready, the idle thread waits for an
//! interrupt.
//!
//! Switching threads saves everything the next thread could disturb: the
//! kernel registers the switch itself doesn't preserve are saved by Rust's
//! calling convention, user registers are in the trap frame on the thread's
//! kernel stack, and the context holds the rest, including the FP/SIMD
//! registers and which address space was active.
//!
//! Only one core runs threads for now. The scheduler's state is behind a
//! lock that masks interrupts, and a switch happens with interrupts masked
//! throughout, which on one core is enough to make it atomic.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use core::fmt;
use core::mem::offset_of;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::addr::{PhysAddr, VirtAddr};
use crate::exception::TrapFrame;
use crate::irq;
use crate::mmu;
use crate::process::{Exit, Process};
use crate::stack::KernelStack;
use crate::sync::SpinLock;

core::arch::global_asm!(include_str!("thread.s"));

unsafe extern "C" {
    /// In thread.s: save the running thread's context in `prev`, and
    /// resume the one in `next`.
    fn switch_context(prev: *mut Context, next: *const Context);
    /// In thread.s: where new threads start.
    fn kernel_thread_start();
    fn user_thread_start();
    /// In linker.ld: the bottom of the boot stack.
    static __stack_bottom: u8;
}

/// A thread's identity. Never reused.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ThreadId(u64);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What `switch_context` saves. Must match the offsets in thread.s.
#[repr(C, align(16))]
struct Context {
    sp: u64,
    /// x19-x29.
    callee_saved: [u64; 11],
    /// x30: where `switch_context` returns to.
    lr: u64,
    tpidr_el0: u64,
    fpcr: u64,
    fpsr: u64,
    /// The lowest address the thread's kernel stack may use.
    stack_limit: u64,
    _padding: u64,
    /// The FP/SIMD registers.
    q: [u128; 32],
}

const _: () = {
    assert!(offset_of!(Context, sp) == 0);
    assert!(offset_of!(Context, callee_saved) == 8);
    assert!(offset_of!(Context, lr) == 96);
    assert!(offset_of!(Context, fpcr) == 112);
    assert!(offset_of!(Context, stack_limit) == 128);
    assert!(offset_of!(Context, q) == 144);
};

impl Context {
    /// A thread that will start at `start`, with `x19` for it, on a stack
    /// whose pointer is `sp`. The user-visible state (TPIDR_EL0, FP/SIMD) is
    /// all zero, so nothing leaks from other threads.
    fn new(start: unsafe extern "C" fn(), x19: u64, sp: usize, stack_limit: usize) -> Context {
        let mut callee_saved = [0; 11];
        callee_saved[0] = x19;
        Context {
            sp: sp as u64,
            callee_saved,
            lr: start as usize as u64,
            tpidr_el0: 0,
            fpcr: 0,
            fpsr: 0,
            stack_limit: stack_limit as u64,
            _padding: 0,
            q: [0; 32],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Waiting in the ready queue (or, for the idle thread, not running).
    Ready,
    Running,
    /// Waiting for `unpark`.
    Parked,
    /// Finished; freed by the next thread to run.
    Dead,
}

/// How a thread ended, and who is waiting to hear.
struct Completion {
    exit: Option<Exit>,
    waiter: Option<ThreadId>,
}

type SharedCompletion = Arc<SpinLock<Completion>>;

struct Thread {
    id: ThreadId,
    name: &'static str,
    context: Context,
    state: State,
    /// Set by `unpark` when the thread wasn't parked, so its next `park`
    /// returns at once instead of missing the wakeup.
    unparked: bool,
    /// The lower half's page tables while this thread runs: its process's,
    /// or for kernel threads whatever they last used.
    user_tables: PhysAddr,
    /// `None` for the boot thread, which keeps the boot stack.
    _stack: Option<KernelStack>,
    /// For user threads, the process they belong to.
    _process: Option<Arc<Process>>,
    completion: SharedCompletion,
}

struct Scheduler {
    /// Every thread, boxed so its context stays put while it is switched
    /// out.
    threads: BTreeMap<ThreadId, Box<Thread>>,
    /// Threads waiting to run, in order.
    ready: VecDeque<ThreadId>,
    current: ThreadId,
    /// Runs when nothing else can; never in `ready`.
    idle: ThreadId,
    /// A thread that has finished, still to be freed: it was running on its
    /// own stack when it finished.
    dead: Option<ThreadId>,
    next_id: u64,
}

static SCHEDULER: SpinLock<Option<Scheduler>> = SpinLock::new(None);

/// Set by the timer tick: the running thread's time slice is over.
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
/// The running thread's ID, readable without the lock (for diagnostics).
static RUNNING: AtomicU64 = AtomicU64::new(0);
static SWITCHES: AtomicU64 = AtomicU64::new(0);
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);

fn with_scheduler<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    f(SCHEDULER.lock().as_mut().expect("the scheduler is started"))
}

impl Scheduler {
    fn thread(&mut self, id: ThreadId) -> &mut Thread {
        self.threads.get_mut(&id).expect("thread exists")
    }

    fn new_id(&mut self) -> ThreadId {
        self.next_id += 1;
        ThreadId(self.next_id - 1)
    }

    fn add(&mut self, thread: Thread) {
        let id = thread.id;
        self.threads.insert(id, Box::new(thread));
        self.ready.push_back(id);
    }

    /// Stop running the current thread, which becomes `state`, and pick
    /// the next one. Returns the contexts to switch between, or `None` to
    /// carry on with the current thread.
    fn prepare_switch(&mut self, state: State) -> Option<(*mut Context, *const Context)> {
        let (current, idle) = (self.current, self.idle);
        let thread = self.thread(current);
        if state == State::Parked && thread.unparked {
            thread.unparked = false;
            return None;
        }
        assert!(
            current != idle || state == State::Ready,
            "the idle thread must not stop"
        );
        thread.state = state;
        match state {
            State::Ready if current != idle => self.ready.push_back(current),
            State::Dead => {
                assert!(self.dead.is_none(), "the last dead thread was freed");
                self.dead = Some(current);
            }
            _ => {}
        }
        let next = self.ready.pop_front().unwrap_or(idle);
        if next == current {
            self.thread(current).state = State::Running;
            return None;
        }

        let prev = self.thread(current);
        prev.user_tables = mmu::user_tables();
        let prev_context = &raw mut prev.context;
        let thread = self.thread(next);
        thread.state = State::Running;
        if thread.user_tables != mmu::user_tables() {
            mmu::set_user_tables(Some(thread.user_tables));
        }
        let next_context = &raw const thread.context;
        self.current = next;
        RUNNING.store(next.0, Ordering::Relaxed);
        SWITCHES.fetch_add(1, Ordering::Relaxed);
        Some((prev_context, next_context))
    }
}

/// Stop the current thread, which becomes `state`, and run the next one.
/// Returns (when this thread next runs) whether it switched at all.
fn switch(state: State) -> bool {
    let daif = irq::save_and_disable();
    let contexts = with_scheduler(|s| s.prepare_switch(state));
    if let Some((prev, next)) = contexts {
        // SAFETY: both contexts are in boxed threads, which stay put: the
        // current one isn't freed while it runs, and `next` is a live,
        // switched-out thread (or a new one) that is now current. IRQs are
        // masked until the switch is complete.
        unsafe { switch_context(prev, next) };
        finish_switch();
    }
    irq::restore(daif);
    contexts.is_some()
}

/// Run on the new thread after every switch: free the thread that just
/// finished, if that's what switched here.
fn finish_switch() {
    let dead = with_scheduler(|s| {
        s.dead
            .take()
            .map(|id| s.threads.remove(&id).expect("dead thread exists"))
    });
    // Freeing its stack and perhaps its process takes other locks, so do
    // it without the scheduler's.
    drop(dead);
}

/// Called by thread.s when a new thread first runs.
#[unsafe(no_mangle)]
extern "C" fn thread_started() {
    finish_switch();
}

type KernelThreadFn = Box<dyn FnOnce() + Send>;

/// Called by thread.s to run a new kernel thread's closure.
#[unsafe(no_mangle)]
extern "C" fn kernel_thread_main(closure: *mut KernelThreadFn) -> ! {
    // SAFETY: `spawn` leaked this box for us, and passes it only once.
    let closure = unsafe { Box::from_raw(closure) };
    irq::enable();
    closure();
    exit(Exit::Code(0))
}

/// Start the scheduler: the code running now becomes the first thread
/// (named `name`), and the idle thread is created. Call once, with the
/// heap working and interrupts set up.
pub fn init(name: &'static str) {
    let boot = Thread {
        id: ThreadId(0),
        name,
        // Filled in when it first switches out.
        context: Context::new(
            kernel_thread_start,
            0,
            0,
            VirtAddr::from_ptr(&raw const __stack_bottom).as_usize(),
        ),
        state: State::Running,
        unparked: false,
        user_tables: mmu::user_tables(),
        _stack: None,
        _process: None,
        completion: Arc::new(SpinLock::new(Completion {
            exit: None,
            waiter: None,
        })),
    };
    let mut scheduler = Scheduler {
        threads: BTreeMap::new(),
        ready: VecDeque::new(),
        current: boot.id,
        idle: boot.id,
        dead: None,
        next_id: 1,
    };
    scheduler.threads.insert(boot.id, Box::new(boot));
    *SCHEDULER.lock() = Some(scheduler);

    // Nobody joins the idle thread; it never ends.
    let _ = spawn("idle", || {
        loop {
            // SAFETY: `wfi` just waits for an interrupt, which (if it makes
            // a thread ready) switches away from here.
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
        }
    })
    .expect("there is memory for the idle thread");
    // Take it off the ready queue: it only runs when nothing else can.
    with_scheduler(|s| s.idle = s.ready.pop_back().expect("the idle thread is queued"));
    STARTED.store(true, Ordering::Release);
}

/// Why a thread couldn't be created.
#[derive(Clone, Copy, Debug)]
pub struct OutOfMemory;

fn new_thread(
    s: &mut Scheduler,
    name: &'static str,
    context: Context,
    stack: KernelStack,
    process: Option<Arc<Process>>,
    user_tables: PhysAddr,
) -> JoinHandle {
    let completion = Arc::new(SpinLock::new(Completion {
        exit: None,
        waiter: None,
    }));
    let id = s.new_id();
    s.add(Thread {
        id,
        name,
        context,
        state: State::Ready,
        unparked: false,
        user_tables,
        _stack: Some(stack),
        _process: process,
        completion: completion.clone(),
    });
    JoinHandle { completion }
}

/// Start a kernel thread running `f`.
pub fn spawn(
    name: &'static str,
    f: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle, OutOfMemory> {
    let stack = KernelStack::new().ok_or(OutOfMemory)?;
    let closure: Box<KernelThreadFn> = Box::new(Box::new(f));
    let context = Context::new(
        kernel_thread_start,
        Box::into_raw(closure) as u64,
        stack.top(),
        stack.bottom(),
    );
    Ok(with_scheduler(|s| {
        let tables = mmu::user_tables_when_idle();
        new_thread(s, name, context, stack, None, tables)
    }))
}

/// Start a user thread in `process`, entering user mode at `entry` with its
/// stack pointer at `stack` and `arg` in x0.
pub fn spawn_user(
    name: &'static str,
    process: Arc<Process>,
    entry: usize,
    stack: usize,
    arg: usize,
) -> Result<JoinHandle, OutOfMemory> {
    let kernel_stack = KernelStack::new().ok_or(OutOfMemory)?;
    let frame = (kernel_stack.top() - size_of::<TrapFrame>()) as *mut TrapFrame;
    // SAFETY: the frame fits at the top of the new stack, which is mapped,
    // aligned (the stack top is page-aligned and the frame's size a
    // multiple of 16) and ours alone.
    unsafe { frame.write(TrapFrame::new_user(entry as u64, stack as u64, arg as u64)) };
    let context = Context::new(user_thread_start, 0, frame as usize, kernel_stack.bottom());
    let tables = process.space().root();
    Ok(with_scheduler(|s| {
        new_thread(s, name, context, kernel_stack, Some(process), tables)
    }))
}

/// A thread's exit status, once it has one.
pub struct JoinHandle {
    completion: SharedCompletion,
}

impl JoinHandle {
    /// Wait for the thread to finish, and say how it ended. By the time
    /// this returns, the thread and everything it owned are freed.
    pub fn join(self) -> Exit {
        loop {
            {
                let mut completion = self.completion.lock();
                if let Some(exit) = completion.exit {
                    return exit;
                }
                completion.waiter = Some(current());
            }
            park();
        }
    }
}

/// The running thread.
pub fn current() -> ThreadId {
    with_scheduler(|s| s.current)
}

/// The running thread, described for a crash report: without waiting for
/// the scheduler's lock, in case the crash happened while holding it.
pub struct Running;

impl fmt::Display for Running {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let id = RUNNING.load(Ordering::Relaxed);
        match SCHEDULER.try_lock() {
            Some(s) => match s.as_ref().and_then(|s| s.threads.get(&ThreadId(id))) {
                Some(thread) => write!(f, "{} ({})", id, thread.name),
                None => write!(f, "{}", id),
            },
            None => write!(f, "{}", id),
        }
    }
}

/// Give the rest of this time slice to the next ready thread, if any.
pub fn yield_now() {
    switch(State::Ready);
}

/// Wait until some thread or interrupt handler calls `unpark` for this
/// thread; at once if one did since the last `park`. It may also return
/// early, so check what you were waiting for and park again if need be.
pub fn park() {
    switch(State::Parked);
}

/// Wake `id` if it is parked, or else make its next `park` return at once.
/// Callable from interrupt handlers. Does nothing if `id` has finished.
pub fn unpark(id: ThreadId) {
    with_scheduler(|s| {
        let idle = s.idle;
        let current = s.current;
        let Some(thread) = s.threads.get_mut(&id) else {
            return;
        };
        match thread.state {
            State::Parked => {
                thread.state = State::Ready;
                s.ready.push_back(id);
                // Don't make it wait for the idle thread's time slice to
                // run out.
                if current == idle {
                    NEED_RESCHED.store(true, Ordering::Relaxed);
                }
            }
            State::Dead => {}
            State::Ready | State::Running => thread.unparked = true,
        }
    });
}

/// End the current thread with `exit`, waking whoever is joining it.
pub fn exit(exit: Exit) -> ! {
    // Nothing may switch away between recording the exit and dying.
    irq::disable();
    let completion = with_scheduler(|s| {
        let current = s.current;
        s.thread(current).completion.clone()
    });
    let waiter = {
        let mut completion = completion.lock();
        completion.exit = Some(exit);
        completion.waiter.take()
    };
    // This stack is abandoned once we switch away, so drop everything we
    // own first.
    drop(completion);
    if let Some(waiter) = waiter {
        unpark(waiter);
    }
    switch(State::Dead);
    unreachable!("a dead thread was resumed")
}

/// Called on every timer tick: end the running thread's time slice.
pub fn tick() {
    if STARTED.load(Ordering::Acquire) {
        NEED_RESCHED.store(true, Ordering::Relaxed);
    }
}

/// Called at the end of an interrupt: if the time slice is over, switch to
/// the next ready thread.
pub fn preempt_if_needed() {
    if NEED_RESCHED.swap(false, Ordering::Relaxed) && switch(State::Ready) {
        PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct Stats {
    pub threads: usize,
    /// Thread switches so far.
    pub switches: u64,
    /// Of those, how many took the processor from a thread that could have
    /// carried on.
    pub preemptions: u64,
}

pub fn stats() -> Stats {
    Stats {
        threads: with_scheduler(|s| s.threads.len()),
        switches: SWITCHES.load(Ordering::Relaxed),
        preemptions: PREEMPTIONS.load(Ordering::Relaxed),
    }
}
