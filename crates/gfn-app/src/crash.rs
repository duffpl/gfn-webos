//! A launched TV app that dies on a signal leaves nothing behind. This writes the signal, the
//! faulting thread and its registers to stderr (the app log), plus the run-time address of a known
//! function so `pc`/`lr` can be mapped back to the binary with `addr2line` despite PIE relocation.

#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub fn install() {
    use std::io::Write;
    let mut anchor = Vec::new();
    let _ = writeln!(anchor, "crash handler: anchor install={:#x}", install as *const () as usize);
    let _ = std::io::stderr().write_all(&anchor);
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESETHAND | libc::SA_ONSTACK;
        for signal in [libc::SIGSEGV, libc::SIGILL, libc::SIGBUS, libc::SIGFPE, libc::SIGABRT] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
pub fn install() {}

/// Only async-signal-safe work: format into a stack buffer and `write(2)`.
#[cfg(all(target_os = "linux", target_arch = "arm"))]
extern "C" fn handler(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let mut buffer = [0u8; 512];
    let mut out = Cursor { buffer: &mut buffer, len: 0 };
    // `struct ucontext` on ARM Linux: flags, link, 12-byte stack_t, then `struct sigcontext`
    // (trap_no, error_code, oldmask, r0..r10, fp, ip, sp, lr, pc, cpsr, fault_address).
    let words = context as *const u32;
    let register = |index: usize| unsafe { *words.add(5 + index) };
    let mut name = [0u8; 16];
    unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr()) };
    out.text("crash: signal ");
    out.hex(signal as u32);
    out.text(" thread ");
    out.bytes(name.split(|&b| b == 0).next().unwrap_or(&[]));
    out.text(" addr ");
    out.hex(unsafe { (*info).si_addr() } as u32);
    out.text(" pc ");
    out.hex(register(18));
    out.text(" lr ");
    out.hex(register(17));
    out.text(" sp ");
    out.hex(register(16));
    out.text("\n");
    unsafe {
        libc::write(2, out.buffer.as_ptr().cast(), out.len);
        libc::raise(signal);
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
struct Cursor<'a> {
    buffer: &'a mut [u8],
    len: usize,
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
impl Cursor<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.len < self.buffer.len() {
                self.buffer[self.len] = b;
                self.len += 1;
            }
        }
    }

    fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    fn hex(&mut self, value: u32) {
        let mut digits = *b"0x00000000";
        for i in 0..8 {
            digits[9 - i] = b"0123456789abcdef"[((value >> (i * 4)) & 0xf) as usize];
        }
        self.bytes(&digits);
    }
}
