//! This process's identity, for naming temporary files: the operating system's process id on native targets, and in
//! a browser, which has none (the standard library's would panic), a number drawn once per page.

/// This process's id (see the module documentation).
pub fn id() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::process::id()
    }
    #[cfg(target_arch = "wasm32")]
    {
        static ID: crate::sync::OnceLock<u32> = crate::sync::OnceLock::new();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // From 0 to just under 2^32.
        *ID.get_or_init(|| (js_sys::Math::random() * f64::from(u32::MAX)) as u32)
    }
}
