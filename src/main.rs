use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: rocksvision [PATH]\n       rocksvision --dump <PATH> [--limit N] [--rows N]");
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            Ok(false) => std::process::exit(1),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1)
            }
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
