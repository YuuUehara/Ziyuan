//! Standalone W1a/W1b/W1c/W1d native Windows probe binary.
//! Delegates to `ziyuan_sandbox_windows::run_probe_cli`.

fn main() {
    if let Err(error) = ziyuan_sandbox_windows::run_probe_cli() {
        eprintln!("W1 probe failed: {error}");
        std::process::exit(1);
    }
}
