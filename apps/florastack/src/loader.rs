//! Reading images for display off the UI thread ([`Loader`]): a thread of its own decodes each request with the
//! engine's display loader (`florastack_engine::view`), so that a 24 MP RAW frame never stalls the window, and asks
//! for a repaint when it is done. A loader that keeps only the newest request ([`Loader::latest_only`]) skips the
//! ones a newer one replaced, as the viewer's do when the user moves through frames.
//!
//! In a browser the thread is a Web Worker, which asks for no repaint (egui's locks are the page's main thread's):
//! the app repaints while [`Loader::pending`] says requests are out.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use florastack_engine::view::{self, DisplayImage};

/// A request: a file, and the longest side wanted (none for full size).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub path: PathBuf,
    pub max_side: Option<usize>,
}

/// A request done.
pub struct Done {
    pub request: Request,
    pub image: Result<Arc<DisplayImage>, String>,
}

/// See the module documentation.
pub struct Loader {
    requests: Sender<Request>,
    done: Receiver<Done>,
    /// Requests sent and not yet answered (skipped ones count as answered).
    pending: Arc<AtomicUsize>,
}

impl Loader {
    /// A loader that reads every request, in order (thumbnails).
    pub fn every(name: &str, ctx: &egui::Context) -> Loader {
        Loader::start(name, ctx, false)
    }

    /// A loader that reads only the newest request waiting (the viewer's image).
    pub fn latest_only(name: &str, ctx: &egui::Context) -> Loader {
        Loader::start(name, ctx, true)
    }

    fn start(name: &str, ctx: &egui::Context, latest: bool) -> Loader {
        let (requests, inbox) = channel::<Request>();
        let (outbox, done) = channel::<Done>();
        let ctx = ctx.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let left = Arc::clone(&pending);
        let spawned = florastack_engine::thread::spawn(format!("florastack-{name}"), move || {
            while let Ok(mut request) = inbox.recv() {
                if latest {
                    while let Ok(newer) = inbox.try_recv() {
                        left.fetch_sub(1, Ordering::Relaxed);
                        request = newer;
                    }
                }
                let image = view::load(&request.path, request.max_side).map(Arc::new).map_err(|e| e.to_string());
                if outbox.send(Done { request, image }).is_err() {
                    break;
                }
                left.fetch_sub(1, Ordering::Relaxed);
                if !cfg!(target_arch = "wasm32") {
                    ctx.request_repaint();
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("no image loader thread ({e}): images won't show");
        }
        Loader { requests, done, pending }
    }

    /// Asks for an image.
    pub fn request(&self, request: Request) {
        // Counted before it is sent, so that the thread never answers it first.
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.requests.send(request).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// How many requests are out.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// The images read since last asked.
    pub fn done(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}
