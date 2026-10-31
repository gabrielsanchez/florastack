//! Reading images for display off the UI thread ([`Loader`]): a thread of its own decodes each request with the
//! engine's display loader (`florastack_engine::view`), so that a 24 MP RAW frame never stalls the window, and asks
//! for a repaint when it is done. A loader that keeps only the newest request ([`Loader::latest_only`]) skips the
//! ones a newer one replaced, as the viewer's do when the user moves through frames.

use std::path::PathBuf;
use std::sync::Arc;
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
        let spawned = std::thread::Builder::new().name(format!("florastack-{name}")).spawn(move || {
            while let Ok(mut request) = inbox.recv() {
                if latest {
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                }
                let image = view::load(&request.path, request.max_side).map(Arc::new).map_err(|e| e.to_string());
                if outbox.send(Done { request, image }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("no image loader thread ({e}): images won't show");
        }
        Loader { requests, done }
    }

    /// Asks for an image.
    pub fn request(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    /// The images read since last asked.
    pub fn done(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}
