//! A flush hook for process exits that bypass `main`: the SIGINT handler
//! exits with 130 from inside core, so the binary registers how to flush its
//! buffered output and the handler calls it first.

use std::sync::OnceLock;

static HOOK: OnceLock<fn()> = OnceLock::new();

/// Register the flush to run before an out-of-band exit. First call wins.
pub fn set_flush_hook(hook: fn()) {
    let _ = HOOK.set(hook);
}

/// Run the registered flush, if any. The hook must itself be bounded.
pub fn flush() {
    if let Some(hook) = HOOK.get() {
        hook();
    }
}
