//! Test-only (the `testutil` feature, which only dev-dependencies enable): faults injected on purpose, so that a test
//! can make a GPU stack fail part way and check what the engine does then. The fault is the process's, for every
//! device: a test that uses it runs in a test binary of its own, where no other test stacks on the GPU.

use std::sync::atomic::{AtomicU64, Ordering};

/// The device checks to let pass before the fault, plus one; 0 when no fault is armed.
static COUNTDOWN: AtomicU64 = AtomicU64::new(0);
/// How many faults fired.
static FIRED: AtomicU64 = AtomicU64::new(0);

/// Makes every device fail once, at its check after the next `checks` ones (`Gpu::check`, which follows every wait
/// for the GPU and every readback), as a driver error would.
pub fn fail_after(checks: u64) {
    COUNTDOWN.store(checks.saturating_add(1), Ordering::SeqCst);
}

/// Disarms a fault that hasn't fired.
pub fn disarm() {
    COUNTDOWN.store(0, Ordering::SeqCst);
}

/// How many injected faults have fired.
pub fn fired() -> u64 {
    FIRED.load(Ordering::SeqCst)
}

/// Whether the fault fires at this check.
pub(crate) fn fault_now() -> bool {
    let fire = COUNTDOWN.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n > 0).then(|| n - 1)).is_ok_and(|before| before == 1);
    if fire {
        FIRED.fetch_add(1, Ordering::SeqCst);
    }
    fire
}
