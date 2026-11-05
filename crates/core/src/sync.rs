//! Locks that a browser's main thread may take. The main thread of a page may not block: a wait there traps, which
//! ends the program. In the browser build these spin on the main thread instead (`wasm_sync`'s mutex, which tries the
//! standard library's until it gets it), and wait everywhere else; on native targets they are the standard library's.
//! Use them for what the UI thread shares with jobs and workers; locks that only workers take can be the standard
//! library's.

pub use std::sync::MutexGuard;
#[cfg(not(target_arch = "wasm32"))]
pub use std::sync::{Mutex, OnceLock};
#[cfg(target_arch = "wasm32")]
pub use wasm_sync::Mutex;
#[cfg(target_arch = "wasm32")]
pub use web::OnceLock;

/// Whether this thread may block: always on native targets; in a browser, everywhere but the page's main thread.
pub fn can_block() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        thread_local! {
            static CAN_BLOCK: bool = web_sys::window().is_none();
        }
        CAN_BLOCK.with(|b| *b)
    }
    #[cfg(not(target_arch = "wasm32"))]
    true
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::sync::PoisonError;

    /// The standard library's `OnceLock`, initialized under a lock that spins on the main thread, so that two
    /// threads never initialize it at once (the standard library's would make the second one wait). Not
    /// `wasm_sync`'s own, which reads the standard library's private state and spins forever with newer ones.
    #[derive(Debug, Default)]
    pub struct OnceLock<T> {
        cell: std::sync::OnceLock<T>,
        init: wasm_sync::Mutex<()>,
    }

    impl<T> OnceLock<T> {
        pub const fn new() -> OnceLock<T> {
            OnceLock { cell: std::sync::OnceLock::new(), init: wasm_sync::Mutex::new(()) }
        }

        pub fn get(&self) -> Option<&T> {
            self.cell.get()
        }

        pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
            if let Some(value) = self.cell.get() {
                return value;
            }
            let _initializing = self.init.lock().unwrap_or_else(PoisonError::into_inner);
            self.cell.get_or_init(f)
        }

        pub fn set(&self, value: T) -> Result<(), T> {
            let _initializing = self.init.lock().unwrap_or_else(PoisonError::into_inner);
            self.cell.set(value)
        }
    }
}
