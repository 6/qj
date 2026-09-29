use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() {
    // Restore default SIGPIPE behavior so piping to `head` etc. exits cleanly
    // instead of producing BrokenPipe errors. Rust's runtime sets SIG_IGN by default.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // qj is the jq 1.8.1 port: jq's main.c (src/cli/run.rs) on the ported
    // core (src/jq), reading input through src/io.
    qj::cli::run::main();
}
