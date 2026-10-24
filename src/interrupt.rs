//! Catching Ctrl-C, for commands that finish up instead of dying: stats shows
//! what it read so far, and run stops its commands cleanly.

use std::sync::atomic::{AtomicUsize, Ordering};

static PRESSES: AtomicUsize = AtomicUsize::new(0);

/// Count Ctrl-C presses instead of dying. With `stop_signals`, a request to
/// stop (SIGTERM, or SIGHUP when the terminal closes) counts as one too.
pub fn catch(stop_signals: bool) {
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        let signals: &[i32] = if stop_signals {
            &[SIGINT, SIGTERM, SIGHUP]
        } else {
            &[SIGINT]
        };
        for &signal in signals {
            // SAFETY: the handler only adds to an atomic, which is signal-safe.
            unsafe {
                let _ = signal_hook::low_level::register(signal, || {
                    PRESSES.fetch_add(1, Ordering::SeqCst);
                });
            }
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
        };
        unsafe extern "system" fn handler(event: u32) -> windows_sys::core::BOOL {
            if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT || event == CTRL_CLOSE_EVENT {
                PRESSES.fetch_add(1, Ordering::SeqCst);
                1
            } else {
                0
            }
        }
        let _ = stop_signals;
        // SAFETY: the handler only adds to an atomic.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

/// How many times Ctrl-C was pressed since `catch`.
pub fn presses() -> usize {
    PRESSES.load(Ordering::SeqCst)
}
