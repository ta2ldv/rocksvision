// Windows: GUI subsystem, so launching from Explorer opens no console window. CLI use from a
// terminal re-attaches to the parent console in `attach_parent_console` (below).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use std::sync::atomic::{AtomicBool, Ordering};

/// Set when this process allocated its own console (Windows CLI mode without a parent console).
static OWN_CONSOLE: AtomicBool = AtomicBool::new(false);

/// Windows only: attach to the launching terminal's console, if any, so `--dump`, `--help`, usage
/// errors, `ui error` and panic messages are visible. Must run before anything is printed (it is
/// the first statement of `main`). Rust's std does not cache the console handle: every
/// stdout/stderr write calls GetStdHandle (library/std/src/sys/pal/windows/stdio.rs `get_handle`),
/// so println!/eprintln! use the attached console afterwards; handles that were already
/// redirected (`> file`, pipes) are inherited and left untouched by AttachConsole.
/// Returns false when there is no parent console. Known limitations: the shell does not wait for
/// a GUI-subsystem process, so its prompt may print before (or between) our output; closing that
/// terminal also ends an attached GUI process (as with a console-subsystem exe).
/// NOT compiler- or runtime-verified on Windows (developed on macOS).
#[cfg(windows)]
fn attach_parent_console() -> bool {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain Win32 call with a constant argument; failure (no parent console, or one is
    // already attached) is reported via the return value only.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}

/// Windows only, CLI modes (`--dump`, `--help`, their usage errors) without a parent console
/// (e.g. started from Explorer or a shortcut with arguments): open a console of our own so the
/// output is not discarded, unless stdout or stderr is already redirected (a pipe or file
/// keeps receiving the output; AllocConsole would replace the standard handles). The GUI
/// mode (no args, or a path to open) never gets a console.
#[cfg(windows)]
fn alloc_console_for_cli() {
    use windows_sys::Win32::System::Console::{AllocConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    // SAFETY: plain Win32 calls with constant arguments; results are checked by value only.
    unsafe {
        let absent = |h: *mut core::ffi::c_void| h.is_null() || h as isize == -1;
        if absent(GetStdHandle(STD_OUTPUT_HANDLE)) && absent(GetStdHandle(STD_ERROR_HANDLE)) && AllocConsole() != 0 {
            OWN_CONSOLE.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() -> bool {
    true
}

#[cfg(not(windows))]
fn alloc_console_for_cli() {}

/// Exits with `code`; a console this process allocated would close at once, so it first waits
/// for Enter there (Windows CLI started without a terminal). Unix: plain exit.
fn exit(code: i32) -> ! {
    if OWN_CONSOLE.load(Ordering::Relaxed) {
        use std::io::Write;
        eprint!("\nPress Enter to close this window.");
        let _ = std::io::stderr().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(code)
}

fn usage() -> ! {
    eprintln!("usage: rocksvision [PATH]\n       rocksvision --dump <PATH> [--limit N] [--rows N]");
    exit(2)
}

fn main() {
    // First statement, before any argument parsing or output (see attach_parent_console).
    let attached = attach_parent_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = args.first().map(|s| s.as_str()) == Some("--dump") || args.iter().any(|a| a == "-h" || a == "--help");
    if cli && !attached {
        alloc_console_for_cli();
    }
    if args.first().map(|s| s.as_str()) == Some("--dump") {
        let Some(path) = args.get(1) else { usage() };
        let mut o = rocksvision::dump::DumpOpts::default();
        let mut i = 2;
        while i < args.len() {
            let v = args.get(i + 1).and_then(|x| x.parse::<usize>().ok()).unwrap_or_else(|| usage());
            match args[i].as_str() {
                "--limit" => o.limit = v,
                "--rows" => o.rows = v,
                _ => usage(),
            }
            i += 2;
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        match rocksvision::dump::dump(std::path::Path::new(path), &o, &mut lock) {
            Ok(true) => {}
            Ok(false) => exit(1),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                eprintln!("error: {e}");
                exit(1)
            }
        }
        drop(lock);
        if OWN_CONSOLE.load(Ordering::Relaxed) {
            exit(0);
        }
        return;
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }
    // Finder may pass -psn_* arguments; ignore anything that is not an existing path.
    let initial = args.iter().map(PathBuf::from).find(|p| p.exists());
    if let Err(e) = rocksvision::ui::run(initial) {
        eprintln!("ui error: {e}");
        std::process::exit(1);
    }
}
