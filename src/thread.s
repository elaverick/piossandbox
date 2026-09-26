// Context switching (see thread.rs).
//
// A thread that isn't running is stopped inside `switch_context`, with its
// callee-saved registers, stack pointer and return address saved in its
// `Context`. Everything else it was doing is on its own kernel stack: the
// Rust frames that called `switch_context`, and below them, for a thread
// that was running user code, the trap frame holding its user registers.
//
// The context also holds the thread's user state that no trap frame
// covers: TPIDR_EL0 and the FP/SIMD registers. The kernel itself never
// uses FP/SIMD (it is built for soft float), so whatever is in them belongs
// to the thread being switched away from.

// Offsets into `Context`; checked in thread.rs.
.equ CTX_SP, 0
.equ CTX_X19, 8
.equ CTX_LR, 96
.equ CTX_FPCR, 112
.equ CTX_STACK_LIMIT, 128
.equ CTX_Q, 144

.arch_extension fp
.arch_extension simd

.section ".text", "ax"

// switch_context(prev: *mut Context, next: *const Context)
//
// Save the running thread's state in `prev` and resume the thread whose
// state is in `next`. Returns when something switches back to `prev`.
// Called with IRQs masked.
.global switch_context
switch_context:
    mov     x9, sp
    stp     x9, x19, [x0, #CTX_SP]
    stp     x20, x21, [x0, #CTX_X19 + 8]
    stp     x22, x23, [x0, #CTX_X19 + 24]
    stp     x24, x25, [x0, #CTX_X19 + 40]
    stp     x26, x27, [x0, #CTX_X19 + 56]
    stp     x28, x29, [x0, #CTX_X19 + 72]
    mrs     x9, tpidr_el0
    stp     x30, x9, [x0, #CTX_LR]
    mrs     x9, fpcr
    mrs     x10, fpsr
    stp     x9, x10, [x0, #CTX_FPCR]
    add     x9, x0, #CTX_Q
    stp     q0, q1, [x9, #0]
    stp     q2, q3, [x9, #32]
    stp     q4, q5, [x9, #64]
    stp     q6, q7, [x9, #96]
    stp     q8, q9, [x9, #128]
    stp     q10, q11, [x9, #160]
    stp     q12, q13, [x9, #192]
    stp     q14, q15, [x9, #224]
    stp     q16, q17, [x9, #256]
    stp     q18, q19, [x9, #288]
    stp     q20, q21, [x9, #320]
    stp     q22, q23, [x9, #352]
    stp     q24, q25, [x9, #384]
    stp     q26, q27, [x9, #416]
    stp     q28, q29, [x9, #448]
    stp     q30, q31, [x9, #480]

    add     x9, x1, #CTX_Q
    ldp     q0, q1, [x9, #0]
    ldp     q2, q3, [x9, #32]
    ldp     q4, q5, [x9, #64]
    ldp     q6, q7, [x9, #96]
    ldp     q8, q9, [x9, #128]
    ldp     q10, q11, [x9, #160]
    ldp     q12, q13, [x9, #192]
    ldp     q14, q15, [x9, #224]
    ldp     q16, q17, [x9, #256]
    ldp     q18, q19, [x9, #288]
    ldp     q20, q21, [x9, #320]
    ldp     q22, q23, [x9, #352]
    ldp     q24, q25, [x9, #384]
    ldp     q26, q27, [x9, #416]
    ldp     q28, q29, [x9, #448]
    ldp     q30, q31, [x9, #480]
    ldp     x9, x10, [x1, #CTX_FPCR]
    msr     fpcr, x9
    msr     fpsr, x10
    ldp     x30, x9, [x1, #CTX_LR]
    msr     tpidr_el0, x9
    ldp     x28, x29, [x1, #CTX_X19 + 72]
    ldp     x26, x27, [x1, #CTX_X19 + 56]
    ldp     x24, x25, [x1, #CTX_X19 + 40]
    ldp     x22, x23, [x1, #CTX_X19 + 24]
    ldp     x20, x21, [x1, #CTX_X19 + 8]
    ldp     x9, x19, [x1, #CTX_SP]
    // Change stacks and tell the exception entry code about the new one
    // together: nothing can interrupt in between.
    mov     sp, x9
    ldr     x9, [x1, #CTX_STACK_LIMIT]
    adrp    x10, current_stack_limit
    str     x9, [x10, :lo12:current_stack_limit]
    ret

// Where a new kernel thread starts, "returning" from its first switch:
// x19 is its closure, for `kernel_thread_main`.
.global kernel_thread_start
kernel_thread_start:
    bl      thread_started
    mov     x0, x19
    bl      kernel_thread_main          // never returns

// Where a new user thread starts: sp points at a trap frame that enters
// user mode.
.global user_thread_start
user_thread_start:
    bl      thread_started
    b       exception_return
