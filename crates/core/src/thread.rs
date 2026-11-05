//! Threads that work in a browser too.
//!
//! On native targets, [`spawn`] is `std::thread`'s. In the browser build (`wasm32-unknown-unknown` with atomics and
//! shared memory, which `cargo xtask web` builds), each thread is a Web Worker that instantiates the same module on the
//! same memory: the page serves a worker script beside its module (`worker.js`, [`set_worker_script`] to name another),
//! which receives the module, the memory and a number, initializes the module and calls its `florastackThread` export
//! with the number, which runs the closure [`spawn`] registered under it.
//!
//! A worker starts only once the thread that made it has returned to the browser's event loop, and the page's main
//! thread may not block at all, so nothing may wait on the main thread for a thread it has just spawned. That is why
//! [`start_pool`], which starts rayon's threads in a browser, is asynchronous: it waits for its workers to be running
//! before it builds rayon's global pool, whose construction waits for them. Code that uses rayon on the main thread
//! spins until the pool's workers are done (rayon's `web_spin_lock` feature), so long work belongs in jobs.

use std::io;

/// Starts a thread named `name` that runs `f`.
pub fn spawn<F>(name: impl Into<String>, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new().name(name.into()).spawn(f).map(drop)
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::spawn(&name.into(), web::Work::Blocking(Box::new(f)))
    }
}

/// How many threads can run at once: the cores the system reports, or the browser's `navigator.hardwareConcurrency`;
/// 1 when neither says.
pub fn available_parallelism() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::hardware_concurrency()
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::{set_worker_script, spawn_async, start_pool, threads_supported};

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use wasm_bindgen::JsValue;
    use wasm_bindgen::prelude::wasm_bindgen;

    use crate::sync::Mutex;

    /// What a new thread runs.
    pub(super) enum Work {
        /// A closure; the worker ends when it returns.
        Blocking(Box<dyn FnOnce() + Send>),
        /// A closure that makes a future, which the worker's event loop runs; the worker stays.
        Async(Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()>>> + Send>),
    }

    /// The work of the threads spawned and not yet started, by number.
    static WORK: Mutex<BTreeMap<u32, Work>> = Mutex::new(BTreeMap::new());
    static NEXT: AtomicU32 = AtomicU32::new(1);
    /// The worker script's URL; `None` for `./worker.js`.
    static SCRIPT: Mutex<Option<String>> = Mutex::new(None);

    thread_local! {
        /// The workers this thread made, kept so that they live as long as the page.
        static WORKERS: RefCell<Vec<web_sys::Worker>> = const { RefCell::new(Vec::new()) };
    }

    fn work() -> std::sync::MutexGuard<'static, BTreeMap<u32, Work>> {
        WORK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn js_error(what: &str, e: &JsValue) -> io::Error {
        io::Error::other(format!("{what}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))))
    }

    /// Whether this build can start threads: one built with atomics, on a page whose memory is shared (which takes
    /// cross-origin isolation, the COOP and COEP headers).
    pub fn threads_supported() -> bool {
        cfg!(target_feature = "atomics") && js_sys::Reflect::get(&js_sys::global(), &"crossOriginIsolated".into()).is_ok_and(|v| v.is_truthy())
    }

    /// Names the worker script other than `./worker.js`, relative to the page (or to the worker that spawns).
    pub fn set_worker_script(url: impl Into<String>) {
        *SCRIPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(url.into());
    }

    pub(super) fn spawn(name: &str, f: Work) -> io::Result<()> {
        if !threads_supported() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "threads need a build with atomics on a cross-origin isolated page"));
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        work().insert(id, f);
        let started = start(id, name);
        if started.is_err() {
            work().remove(&id);
        }
        started
    }

    fn start(id: u32, name: &str) -> io::Result<()> {
        let script = SCRIPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().unwrap_or_else(|| "./worker.js".to_owned());
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        options.set_name(name);
        let worker = web_sys::Worker::new_with_options(&script, &options).map_err(|e| js_error(&format!("starting a worker from {script}"), &e))?;
        let message = js_sys::Array::of3(&wasm_bindgen::module(), &wasm_bindgen::memory(), &JsValue::from(id));
        worker.post_message(&message).map_err(|e| js_error("starting a worker", &e))?;
        WORKERS.with(|w| w.borrow_mut().push(worker));
        Ok(())
    }

    /// Starts a thread whose event loop runs the future `f` makes, for work that awaits the browser (its file system,
    /// the GPU) rather than blocks. The thread stays when the future ends.
    pub fn spawn_async<F, Fut>(name: impl Into<String>, f: F) -> io::Result<()>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        spawn(&name.into(), Work::Async(Box::new(move || Box::pin(f()))))
    }

    /// The entry point of every worker (see the module documentation): runs thread `id`'s work, and says whether the
    /// worker must stay for the futures it left running.
    #[wasm_bindgen(js_name = florastackThread)]
    pub fn run(id: u32) -> bool {
        let taken = work().remove(&id);
        match taken {
            Some(Work::Blocking(f)) => {
                f();
                false
            }
            Some(Work::Async(f)) => {
                wasm_bindgen_futures::spawn_local(f());
                true
            }
            None => false,
        }
    }

    pub(super) fn hardware_concurrency() -> usize {
        let navigator = js_sys::Reflect::get(&js_sys::global(), &"navigator".into()).unwrap_or(JsValue::UNDEFINED);
        let cores = js_sys::Reflect::get(&navigator, &"hardwareConcurrency".into()).ok().and_then(|v| v.as_f64()).unwrap_or(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to 1 to 1024 first.
        let cores = cores.clamp(1.0, 1024.0) as usize;
        cores
    }

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(handler: &js_sys::Function, timeout: i32) -> JsValue;
    }

    /// Waits `ms` milliseconds without blocking: the event loop runs meanwhile.
    pub(crate) async fn sleep(ms: i32) {
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            set_timeout(&resolve, ms);
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }

    /// How long [`start_pool`] waits for its workers to start.
    const START_TIMEOUT_MS: i32 = 30_000;

    /// Starts rayon's global pool on `threads` workers (see the module documentation). Call it once, from the page's
    /// main thread, before anything uses rayon; it fails when the workers can't start (no `worker.js`, a page that
    /// isn't cross-origin isolated) or don't within 30 seconds.
    pub async fn start_pool(threads: usize) -> io::Result<()> {
        let threads = threads.max(1);
        let running = Arc::new(AtomicUsize::new(0));
        let mut senders = Vec::with_capacity(threads);
        for i in 0..threads {
            let (sender, receiver) = std::sync::mpsc::channel::<rayon::ThreadBuilder>();
            let running = Arc::clone(&running);
            spawn(
                &format!("florastack-rayon-{i}"),
                Work::Blocking(Box::new(move || {
                    running.fetch_add(1, Ordering::SeqCst);
                    if let Ok(thread) = receiver.recv() {
                        thread.run();
                    }
                })),
            )?;
            senders.push(sender);
        }
        let mut waited = 0;
        while running.load(Ordering::SeqCst) < threads {
            if waited >= START_TIMEOUT_MS {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} of {threads} worker threads started", running.load(Ordering::SeqCst)),
                ));
            }
            sleep(10).await;
            waited += 10;
        }
        let mut senders = senders.into_iter();
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("florastack-rayon-{i}"))
            .spawn_handler(move |thread| {
                let sender = senders.next().ok_or_else(|| io::Error::other("rayon asked for more threads than were started"))?;
                sender.send(thread).map_err(|_| io::Error::other("a worker thread ended before rayon started"))
            })
            .build_global()
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_thread_runs() {
        let (sender, receiver) = std::sync::mpsc::channel();
        spawn("florastack-test", move || sender.send(7).unwrap()).unwrap();
        assert_eq!(receiver.recv().unwrap(), 7);
        assert!(available_parallelism() >= 1);
    }
}
