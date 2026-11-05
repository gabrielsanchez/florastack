//! The browser build's front end (`cargo xtask web`): what the page does that a window's operating system does for the
//! desktop app.
//!
//! - [`start`], which the page calls once: the log to the browser's console, rayon's threads on Web Workers and the
//!   storage worker for spilled tiles (florastack-core's `thread` and `TempFileStore`), then the app on the page's
//!   canvas through eframe's web runner, drawn with WebGPU, or WebGL where the browser has no WebGPU.
//! - Files in: a hidden file input (`#florastack-files`), which **Add frames…** opens, and files dropped on the page,
//!   whose bytes egui hands over. Both are written to the in-memory file system's `/files` (florastack-core's `fs`),
//!   where the engine reads them like any file. Before reading, the files' size is checked against the memory the page
//!   may still take ([`MEMORY_LIMIT`]): files that wouldn't fit are refused with a message that says what to do.
//! - Files out: whatever is written under `/downloads` (the folder the Export tab's **Export…** gets here, see
//!   `dialogs.rs`) is downloaded once no job is running, and removed.
//! - `window.florastack.dispatch(name, params)`, from the page's script: an engine command, answered with its result,
//!   a job's once it ends. It is how the headless browser test stacks, and what scripts can use as the desktop app's
//!   control channel.
//!
//! Everything here runs on the page's main thread, which owns the page's JavaScript objects.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use florastack_engine::{Dispatched, JobId, JobState, Value, fs};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::app::FloraStackApp;
use crate::i18n::tr_args;

/// The most a page's memory can grow to: 4 GiB, WebAssembly's 32-bit limit (`cargo xtask web` links for it).
pub const MEMORY_LIMIT: u64 = 4 << 30;
/// What the files given to the page may take of it: the rest is for decoding, stacking and the app.
pub const FILES_LIMIT: u64 = MEMORY_LIMIT / 2;
/// Where the page's files go, and where downloads come from.
const FILES: &str = "/files";
pub(crate) const DOWNLOADS: &str = "/downloads";
const INPUT_ID: &str = "florastack-files";
const STATUS_ID: &str = "florastack-status";

/// What files picked are for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Frames to add.
    Frames,
    /// Files to split into stacks (**Import stacks**).
    Stacks,
    /// A preset to apply.
    Preset,
}

/// A request of the page's script, waiting for the app.
struct Request {
    name: String,
    params: Value,
    resolve: js_sys::Function,
    reject: js_sys::Function,
}

#[derive(Default)]
struct State {
    ctx: Option<egui::Context>,
    /// What the file input was opened for.
    picking: Option<Purpose>,
    /// Files read into the in-memory file system, waiting for the app.
    arrived: Vec<(Purpose, Vec<PathBuf>)>,
    /// Messages for the app to show.
    notices: Vec<(String, bool)>,
    requests: Vec<Request>,
    /// Jobs whose request waits for their end.
    waiting: Vec<(JobId, js_sys::Function, js_sys::Function)>,
    /// Batches of files read, for unique folder names.
    batches: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn repaint() {
    if let Some(ctx) = with(|s| s.ctx.clone()) {
        ctx.request_repaint();
    }
}

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

fn js_text(e: &JsValue) -> String {
    if let Some(error) = e.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

/// Shows `text` in the page's status line, outside the canvas (before the app runs, or when it can't).
fn status(text: &str) {
    if let Some(element) = document().and_then(|d| d.get_element_by_id(STATUS_ID)) {
        element.set_text_content(Some(text));
    }
}

/// Starts the app on the canvas `canvas_id` (see the module documentation). The page shows what failed in its status
/// line: a browser without threads (a page served without cross-origin isolation), or one whose workers can't start.
#[wasm_bindgen]
pub async fn start(canvas_id: String) -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(|info| {
        let text = format!("{} stopped: {info}", florastack_engine::branding::DISPLAY_NAME);
        web_sys::console::error_1(&text.as_str().into());
        status(&text);
    }));
    let query = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    let params = web_sys::UrlSearchParams::new_with_str(&query).ok();
    let param = |name: &str| params.as_ref().and_then(|p| p.get(name));
    let level = param("log").and_then(|l| l.parse().ok()).unwrap_or(tracing_subscriber::filter::LevelFilter::WARN);
    tracing_subscriber::fmt().with_writer(Console).without_time().with_ansi(false).with_max_level(level).init();

    if !florastack_engine::thread::threads_supported() {
        let text = format!(
            "{} needs a page served with cross-origin isolation (the Cross-Origin-Opener-Policy and Cross-Origin-Embedder-Policy headers), \
             which its threads need: see the hosting notes.",
            florastack_engine::branding::DISPLAY_NAME
        );
        status(&text);
        return Err(text.into());
    }
    let cores = florastack_engine::thread::available_parallelism();
    let threads = param("threads").and_then(|t| t.parse::<usize>().ok()).unwrap_or(cores).clamp(1, 32);
    status("Starting threads…");
    if let Err(e) = florastack_engine::thread::start_pool(threads).await {
        let text = format!("{}'s threads didn't start: {e}", florastack_engine::branding::DISPLAY_NAME);
        status(&text);
        return Err(text.into());
    }
    florastack_engine::start_storage();
    install_input()?;

    let canvas = document()
        .and_then(|d| d.get_element_by_id(&canvas_id))
        .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .ok_or_else(|| JsValue::from_str(&format!("no canvas #{canvas_id}")))?;
    status("");
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(|cc| {
                with(|s| s.ctx = Some(cc.egui_ctx.clone()));
                Ok(Box::new(FloraStackApp::new(cc, crate::app::Options::web())))
            }),
        )
        .await
}

/// The log, to the browser's console: one message per event.
struct Console;

struct Line(Vec<u8>);

impl std::io::Write for Line {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.0);
        let text = text.trim_end();
        if !text.is_empty() {
            web_sys::console::log_1(&text.into());
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Console {
    type Writer = Line;
    fn make_writer(&'a self) -> Line {
        Line(Vec::new())
    }
}

/// The hidden file input, made once.
fn install_input() -> Result<(), JsValue> {
    let document = document().ok_or("no document")?;
    if document.get_element_by_id(INPUT_ID).is_some() {
        return Ok(());
    }
    let input: web_sys::HtmlInputElement = document.create_element("input")?.dyn_into()?;
    input.set_type("file");
    input.set_multiple(true);
    input.set_id(INPUT_ID);
    input.set_hidden(true);
    let changed = Closure::<dyn FnMut()>::new(move || {
        let Some(input) = document_input() else { return };
        let Some(list) = input.files() else { return };
        let files: Vec<web_sys::File> = (0..list.length()).filter_map(|i| list.get(i)).collect();
        // The same files can be picked again.
        input.set_value("");
        let purpose = with(|s| s.picking.take()).unwrap_or(Purpose::Frames);
        wasm_bindgen_futures::spawn_local(read_files(files, purpose));
    });
    input.add_event_listener_with_callback("change", changed.as_ref().unchecked_ref())?;
    changed.forget();
    document.body().ok_or("no body")?.append_child(&input)?;
    Ok(())
}

fn document_input() -> Option<web_sys::HtmlInputElement> {
    document()?.get_element_by_id(INPUT_ID)?.dyn_into().ok()
}

/// Opens the browser's file picker for `purpose`; the files come to the app once they are read.
pub fn pick(purpose: Purpose) {
    let Some(input) = document_input() else { return };
    let accept = match purpose {
        Purpose::Frames | Purpose::Stacks => {
            "image/*,.dng,.cr2,.cr3,.nef,.nrw,.arw,.srf,.sr2,.raf,.rw2,.rwl,.pef,.orf,.srw,.3fr,.iiq,.erf,.mef,.mos,.exr,.tif,.tiff"
        }
        Purpose::Preset => ".toml",
    };
    input.set_accept(accept);
    with(|s| s.picking = Some(purpose));
    input.click();
}

/// A new folder in `parent` for a batch of files.
fn batch_folder(parent: &str) -> PathBuf {
    let n = with(|s| {
        s.batches += 1;
        s.batches
    });
    PathBuf::from(format!("{parent}/{n}"))
}

/// A message for files of `size` bytes that wouldn't fit beside the `held` bytes the page has.
fn too_large(size: u64, held: u64) -> String {
    let gb = |b: u64| format!("{:.1}", b as f64 / f64::from(1 << 30));
    tr_args("web-files-too-large", &[("size", gb(size).into()), ("held", gb(held).into()), ("limit", gb(FILES_LIMIT).into())])
}

/// `name` as a file name: the last component of what the browser gave, never a path or `..`.
fn file_name(name: &str) -> Option<String> {
    Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty() && n != "..")
}

/// Writes the files of one batch to a folder of their own, and hands them to the app for `purpose`.
fn arrive(purpose: Purpose, files: Vec<(String, Vec<u8>)>) {
    let folder = batch_folder(FILES);
    if let Err(e) = fs::create_dir_all(&folder) {
        with(|s| s.notices.push((e.to_string(), true)));
        return;
    }
    let mut paths = Vec::with_capacity(files.len());
    for (name, bytes) in files {
        let path = folder.join(&name);
        match fs::write_shared(&path, std::sync::Arc::new(bytes)) {
            Ok(()) => paths.push(path),
            Err(e) => with(|s| s.notices.push((format!("{name}: {e}"), true))),
        }
    }
    paths.sort();
    with(|s| s.arrived.push((purpose, paths)));
    repaint();
}

/// Reads `files` into the in-memory file system for `purpose`, unless they wouldn't fit in the page's memory.
async fn read_files(files: Vec<web_sys::File>, purpose: Purpose) {
    let size: f64 = files.iter().map(|f| f.size()).sum();
    let held = fs::bytes_in_memory().unwrap_or(0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A size in bytes, far below 2^53.
    let size = size.max(0.0) as u64;
    if held.saturating_add(size) > FILES_LIMIT {
        with(|s| s.notices.push((too_large(size, held), true)));
        repaint();
        return;
    }
    let mut read = Vec::with_capacity(files.len());
    for file in files {
        let Some(name) = file_name(&file.name()) else { continue };
        match JsFuture::from(file.array_buffer()).await {
            Ok(buffer) => read.push((name, js_sys::Uint8Array::new(&buffer).to_vec())),
            Err(e) => with(|s| s.notices.push((format!("{name}: {}", js_text(&e)), true))),
        }
    }
    arrive(purpose, read);
}

/// Reads files dropped on the page into the in-memory file system as frames, as many as fit in the page's memory.
pub fn dropped(files: &[egui::DroppedFileHandle]) {
    if files.is_empty() {
        return;
    }
    let files = files.to_vec();
    wasm_bindgen_futures::spawn_local(async move {
        let held = fs::bytes_in_memory().unwrap_or(0);
        let mut size = 0_u64;
        let mut read = Vec::with_capacity(files.len());
        for file in &files {
            let Some(name) = file_name(&file.path().to_string_lossy()) else { continue };
            match file.bytes_async().await {
                Ok(bytes) => {
                    size = size.saturating_add(bytes.len() as u64);
                    if held.saturating_add(size) > FILES_LIMIT {
                        with(|s| s.notices.push((too_large(size, held), true)));
                        repaint();
                        return;
                    }
                    read.push((name, bytes));
                }
                Err(e) => with(|s| s.notices.push((format!("{name}: {e}"), true))),
            }
        }
        arrive(Purpose::Frames, read);
    });
}

/// A new folder under `/downloads`, whose files are downloaded once written.
pub fn download_folder() -> Option<PathBuf> {
    let folder = batch_folder(DOWNLOADS);
    fs::create_dir_all(&folder).ok()?;
    Some(folder)
}

/// Offers the file at `path` as a download named as it is.
fn download(path: &Path) -> Result<(), String> {
    let bytes = fs::read_shared(path).map_err(|e| e.to_string())?;
    let name = path.file_name().map_or_else(|| "download".to_owned(), |n| n.to_string_lossy().into_owned());
    let length = u32::try_from(bytes.len()).map_err(|_| "a file over 4 GiB".to_owned())?;
    // A copy of the page's own: the module's memory is shared, which a Blob can't hold.
    let array = js_sys::Uint8Array::new_with_length(length);
    array.copy_from(&bytes);
    let blob = web_sys::Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&array)).map_err(|e| js_text(&e))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|e| js_text(&e))?;
    let anchor: web_sys::HtmlAnchorElement =
        document().ok_or("no document")?.create_element("a").map_err(|e| js_text(&e))?.dyn_into().map_err(|_| "not an anchor".to_owned())?;
    anchor.set_href(&url);
    anchor.set_download(&name);
    anchor.click();
    // The download has its own reference to the data once it starts; the URL can go a little later.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 60_000);
    }
    Ok(())
}

/// Every file under `dir`, in name order.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                folders.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// Called each frame: hands the app the files read and the messages, runs the page script's requests and answers
/// those whose job ended, and downloads what was written to `/downloads` once no job runs.
pub fn tick(app: &mut FloraStackApp) {
    let (arrived, notices, requests) = with(|s| (std::mem::take(&mut s.arrived), std::mem::take(&mut s.notices), std::mem::take(&mut s.requests)));
    for (text, error) in notices {
        app.shell.notify(text, error);
    }
    for (purpose, paths) in arrived {
        if paths.is_empty() {
            continue;
        }
        match purpose {
            Purpose::Frames => app.add_frames(&paths),
            Purpose::Stacks => {
                app.shell.run("project.findStacks", florastack_engine::json!({ "paths": paths }));
            }
            Purpose::Preset => {
                if let Some(path) = paths.into_iter().next() {
                    app.offer_preset(crate::presets::Source::File(path));
                }
            }
        }
    }
    for request in requests {
        let params = if request.params.is_null() { florastack_engine::json!({}) } else { request.params };
        match app.shell.dispatch(&request.name, params) {
            Ok(Dispatched::Done(value)) => answer(&request.resolve, &value),
            Ok(Dispatched::Started(job)) => with(|s| s.waiting.push((job, request.resolve, request.reject))),
            Err(e) => {
                let _ = request.reject.call1(&JsValue::NULL, &JsValue::from_str(&e.info().message));
            }
        }
    }
    let waiting = with(|s| std::mem::take(&mut s.waiting));
    let mut still = Vec::new();
    for (job, resolve, reject) in waiting {
        match app.shell.engine().job(job) {
            Some(info) if info.state == JobState::Done => answer(&resolve, &info.result.unwrap_or(Value::Null)),
            Some(info) if info.state.is_finished() => {
                let message = info.error.map_or_else(|| format!("{:?}", info.state).to_lowercase(), |e| e.message);
                let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&message));
            }
            Some(_) => still.push((job, resolve, reject)),
            None => {
                let _ = reject.call1(&JsValue::NULL, &JsValue::from("the job is gone"));
            }
        }
    }
    let pending = !still.is_empty();
    with(|s| s.waiting.extend(still));
    if !app.shell.busy() && fs::path_is_dir(Path::new(DOWNLOADS)) {
        for path in files_under(Path::new(DOWNLOADS)) {
            if let Err(e) = download(&path) {
                app.shell.notify(format!("{}: {e}", path.display()), true);
            }
        }
        let _ = fs::remove_dir_all(DOWNLOADS);
    }
    if pending && let Some(ctx) = with(|s| s.ctx.clone()) {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn answer(resolve: &js_sys::Function, value: &Value) {
    let json = js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL);
    let _ = resolve.call1(&JsValue::NULL, &json);
}

/// `window.florastack.dispatch(name, params)` (see the module documentation): a promise of the command's result, or
/// of a job's once it ends.
#[wasm_bindgen]
pub fn dispatch(name: &str, params: &JsValue) -> js_sys::Promise {
    let params = if params.is_undefined() || params.is_null() {
        Value::Null
    } else {
        js_sys::JSON::stringify(params).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
    };
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        with(|s| s.requests.push(Request { name: name.to_owned(), params: params.clone(), resolve, reject }));
    });
    repaint();
    promise
}

/// Whether the app is running, for the page's script to wait for.
#[wasm_bindgen]
pub fn ready() -> bool {
    with(|s| s.ctx.is_some())
}
