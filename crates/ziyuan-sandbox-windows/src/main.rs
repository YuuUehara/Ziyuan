//! W1a self-reexecution probe, not an arbitrary-command launcher.
//! No runtime/MCP dependency may use this as a production sandbox.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

fn main() {
    #[cfg(windows)]
    if let Err(error) = windows::run() {
        eprintln!("W1a probe failed: {error}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("W1a requires native Windows; no isolation was performed");
        std::process::exit(1);
    }
}
