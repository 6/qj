//! Running qj's work on a stack big enough for it, whatever `ulimit -s` says.
//!
//! The main thread's stack is as big as `RLIMIT_STACK` says. With 8 MB or more
//! (the default on macOS and Linux) that is far more than qj's own frames ever
//! need — its deepest recursions are bounded by jq's own limits or turn into
//! loops, and need about 100 KB in an optimized build — so qj runs there, as
//! it always did. Below that, [`run`] first moves to a stack of its own:
//! [`Stack::new`] maps one (address space only: pages are committed as they
//! are touched), and [`run_on`] moves the stack pointer there, calls a
//! function, and moves it back. Either way qj's own frames never run out
//! where jq's wouldn't, and only compat mode's models of jq's stack, which
//! read the limit, decide whether a run dies of it (`src/compat.rs`).
//!
//! It is the main thread still, not a thread of qj's making: its thread-local
//! state, its signal mask and its identity stay what they were. Starting a
//! thread for the work instead would cost about 65 µs of qj's 1.65 ms start
//! on an M5 Max (the thread, and its allocator heap), and the switch about
//! 30 µs (the mapping, and its first pages), which is why it only happens
//! when it is needed.
//!
//! Only aarch64 and x86-64 have the switch ([`switch`]); elsewhere [`run_on`]
//! calls the function where it is.

use std::io;
use std::panic::{self, AssertUnwindSafe};

/// A stack for [`run_on`]: `size` bytes of address space, with an inaccessible
/// page below them that turns an overflow into `SIGSEGV` (Linux) or `SIGBUS`
/// (macOS), as a thread's guard page does. Never unmapped: qj exits from it.
pub struct Stack {
    /// The lowest address of the mapping, where the guard page is.
    base: *mut u8,
    /// The whole mapping, guard included.
    len: usize,
}

impl Stack {
    /// Maps a stack of at least `size` bytes.
    pub fn new(size: usize) -> io::Result<Stack> {
        // SAFETY: sysconf has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(4096);
        let len = size.div_ceil(page) * page + page;
        #[cfg(target_os = "linux")]
        // MAP_NORESERVE: address space, not commit charge (where the kernel
        // overcommits). MAP_STACK: no transparent huge pages (Linux 6.7+).
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_STACK;
        #[cfg(not(target_os = "linux"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        // SAFETY: an anonymous mapping of `len` bytes; nothing else refers to
        // the address range it returns.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the first page of the mapping just made.
        if unsafe { libc::mprotect(base, page, libc::PROT_NONE) } != 0 {
            let e = io::Error::last_os_error();
            // SAFETY: unmapping what was just mapped, unused.
            unsafe { libc::munmap(base, len) };
            return Err(e);
        }
        Ok(Stack {
            base: base.cast(),
            len,
        })
    }

    /// The address a stack pointer starts at: the top of the mapping, which a
    /// page boundary keeps 16-byte aligned, as both ABIs want.
    fn top(&self) -> *mut u8 {
        // SAFETY: one past the end of the mapping.
        unsafe { self.base.add(self.len) }
    }
}

/// The smallest `RLIMIT_STACK` qj runs on as it is: macOS's default (8,176
/// KB; Linux's is 8,192), and many times what qj's own frames need.
pub const MAIN_STACK_FLOOR: u64 = 8176 << 10;

/// Runs `f` on the main thread's stack if `RLIMIT_STACK` gives it at least
/// [`MAIN_STACK_FLOOR`], and otherwise on a [`Stack`] of `size` bytes (or, if
/// none can be mapped, where it is). A panic comes back as an error.
pub fn run<F: FnOnce() -> R, R>(size: usize, f: F) -> std::thread::Result<R> {
    // SAFETY: getrlimit writes an rlimit into a valid out-pointer.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    let roomy = unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut lim) } != 0
        || lim.rlim_cur == libc::RLIM_INFINITY
        || lim.rlim_cur >= MAIN_STACK_FLOOR;
    if !roomy && let Ok(stack) = Stack::new(size) {
        return run_on(&stack, f);
    }
    panic::catch_unwind(AssertUnwindSafe(f))
}

/// Runs `f` on `stack`, on this thread, and returns what it returns — or, if
/// it panicked, the panic, caught on `stack` (unwinding doesn't cross the
/// switch).
pub fn run_on<F: FnOnce() -> R, R>(stack: &Stack, f: F) -> std::thread::Result<R> {
    struct Call<F, R> {
        f: Option<F>,
        out: Option<std::thread::Result<R>>,
    }
    unsafe extern "C" fn trampoline<F: FnOnce() -> R, R>(call: *mut u8) {
        // SAFETY: `run_on` passes a live, exclusive `Call<F, R>`.
        let call = unsafe { &mut *call.cast::<Call<F, R>>() };
        let f = call.f.take().expect("called once");
        call.out = Some(panic::catch_unwind(AssertUnwindSafe(f)));
    }
    let mut call = Call {
        f: Some(f),
        out: None,
    };
    let data = (&raw mut call).cast::<u8>();
    // SAFETY: `stack` is a mapping no one else uses, and `trampoline` doesn't
    // unwind (it catches every panic).
    unsafe { switch(stack.top(), data, trampoline::<F, R>) };
    call.out.expect("the trampoline ran")
}

/// Calls `f(data)` with the stack pointer at `top`, and puts it back.
///
/// # Safety
///
/// `top` must be the 16-byte aligned top of a stack nothing else uses, big
/// enough for `f`, and `f` must not unwind.
#[cfg(target_arch = "aarch64")]
unsafe fn switch(top: *mut u8, data: *mut u8, f: unsafe extern "C" fn(*mut u8)) {
    // x20 is callee-saved, so it survives the call; the compiler saves it for
    // this function. The frame pointer is zeroed for the call, so that a walk
    // of frame records (a debugger's, a profiler's) stops at the switch.
    // SAFETY: the caller's contract; the stack is 16-byte aligned.
    unsafe {
        std::arch::asm!(
            "mov x20, sp",
            "mov x21, x29",
            "mov sp, {top}",
            "mov x29, xzr",
            "blr {f}",
            "mov sp, x20",
            "mov x29, x21",
            top = in(reg) top,
            f = in(reg) f,
            in("x0") data,
            out("x20") _,
            out("x21") _,
            clobber_abi("C"),
        );
    }
}

/// Calls `f(data)` with the stack pointer at `top`, and puts it back.
///
/// # Safety
///
/// `top` must be the 16-byte aligned top of a stack nothing else uses, big
/// enough for `f`, and `f` must not unwind.
#[cfg(target_arch = "x86_64")]
unsafe fn switch(top: *mut u8, data: *mut u8, f: unsafe extern "C" fn(*mut u8)) {
    // r12 is callee-saved, so it survives the call; the compiler saves it for
    // this function. `call` pushes the return address, leaving the stack
    // pointer 8 below a 16-byte boundary at `f`'s entry, as the ABI wants. The
    // frame pointer is zeroed for the call, so that a walk of frame records
    // stops at the switch.
    // SAFETY: the caller's contract; the stack is 16-byte aligned.
    unsafe {
        std::arch::asm!(
            "mov r12, rsp",
            "mov r13, rbp",
            "mov rsp, {top}",
            "xor ebp, ebp",
            "call {f}",
            "mov rsp, r12",
            "mov rbp, r13",
            top = in(reg) top,
            f = in(reg) f,
            in("rdi") data,
            out("r12") _,
            out("r13") _,
            clobber_abi("C"),
        );
    }
}

/// Elsewhere: no switch, `f` runs on the current stack.
///
/// # Safety
///
/// `f` must not unwind.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
unsafe fn switch(_top: *mut u8, data: *mut u8, f: unsafe extern "C" fn(*mut u8)) {
    // SAFETY: the caller's contract.
    unsafe { f(data) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recursion that needs about `n` KB of stack.
    fn deep(n: u32) -> u32 {
        let buf = std::hint::black_box([n as u8; 1024]);
        if n == 0 {
            u32::from(buf[0])
        } else {
            deep(n - 1) + u32::from(buf[1] & 0)
        }
    }

    #[test]
    fn runs_on_the_new_stack_and_returns() {
        let stack = Stack::new(64 << 20).expect("a stack");
        let here = 0u8;
        let (inside, out) = run_on(&stack, || {
            let there = 0u8;
            (std::ptr::addr_of!(there) as usize, deep(20_000))
        })
        .expect("no panic");
        assert_eq!(out, 0);
        // 20 MB of frames, far more than a test thread has.
        let (lo, hi) = (stack.base as usize, stack.top() as usize);
        assert!(lo < inside && inside < hi, "ran on the new stack");
        assert!(!(lo..hi).contains(&(std::ptr::addr_of!(here) as usize)));
    }

    #[test]
    fn a_panic_comes_back_as_an_error() {
        let stack = Stack::new(1 << 20).expect("a stack");
        let r = run_on(&stack, || -> u32 { panic!("inside") });
        assert!(r.is_err());
        // And the thread goes on as before.
        assert_eq!(run_on(&stack, || 7).expect("no panic"), 7);
    }

    #[test]
    fn thread_locals_are_the_callers() {
        thread_local! {
            static CELL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        CELL.with(|c| c.set(41));
        let stack = Stack::new(1 << 20).expect("a stack");
        let seen = run_on(&stack, || CELL.with(|c| c.get() + 1)).expect("no panic");
        assert_eq!(seen, 42);
    }
}
