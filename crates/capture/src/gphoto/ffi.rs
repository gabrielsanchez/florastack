//! The only unsafe module in the workspace. Declarations follow the public libgphoto2 API documentation:
//! <https://gphoto.sourceforge.io/doc/api/gphoto2-camera_8h.html> (and file/widget/list/context APIs).
//! Opaque objects are uniquely owned, used exclusively, freed with their matching API and never outlive the library.
//! `CameraFilePath` is the documented C layout, not a copy of an implementation. No driver/source code was consulted.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::ptr::NonNull;
use std::sync::Arc;

use florastack_core::Monitor;
use libloading::Library;

use crate::{Camera, Capabilities, Capture, Error};

type Ptr = *mut c_void;
#[repr(C)]
struct FilePath {
    name: [c_char; 128],
    folder: [c_char; 1024],
}

macro_rules! api {
    ($($name:ident: ($($arg:ty),*) -> $result:ty),* $(,)?) => {
        struct Api { _library: Library, $($name: unsafe extern "C" fn($($arg),*) -> $result),* }
        impl Api {
            fn from_library(library: Library) -> Result<Arc<Self>, Error> {
                $(// SAFETY: each symbol's exact public C signature is declared below; Library stays in Api until all objects are freed.
                let $name = *unsafe { library.get::<unsafe extern "C" fn($($arg),*) -> $result>(concat!(stringify!($name), "\0").as_bytes()) }
                    .map_err(|source| Error::Library { operation: format!("resolve {}", stringify!($name)), source })?;)*
                Ok(Arc::new(Self { _library: library, $($name),* }))
            }
        }
    }
}
api! {
    gp_context_new: () -> Ptr,
    gp_context_unref: (Ptr) -> (),
    gp_camera_new: (*mut Ptr) -> c_int,
    gp_camera_init: (Ptr, Ptr) -> c_int,
    gp_camera_exit: (Ptr, Ptr) -> c_int,
    gp_camera_free: (Ptr) -> c_int,
    gp_camera_autodetect: (Ptr, Ptr) -> c_int,
    gp_list_new: (*mut Ptr) -> c_int,
    gp_list_free: (Ptr) -> c_int,
    gp_list_count: (Ptr) -> c_int,
    gp_list_get_name: (Ptr, c_int, *mut *const c_char) -> c_int,
    gp_list_get_value: (Ptr, c_int, *mut *const c_char) -> c_int,
    gp_camera_capture: (Ptr, c_int, *mut FilePath, Ptr) -> c_int,
    gp_camera_capture_preview: (Ptr, Ptr, Ptr) -> c_int,
    gp_camera_file_get: (Ptr, *const c_char, *const c_char, c_int, Ptr, Ptr) -> c_int,
    gp_file_new: (*mut Ptr) -> c_int,
    gp_file_free: (Ptr) -> c_int,
    gp_file_get_data_and_size: (Ptr, *mut *const c_char, *mut c_ulong) -> c_int,
    gp_camera_get_single_config: (Ptr, *const c_char, *mut Ptr, Ptr) -> c_int,
    gp_camera_set_single_config: (Ptr, *const c_char, Ptr, Ptr) -> c_int,
    gp_widget_free: (Ptr) -> c_int,
    gp_widget_get_type: (Ptr, *mut c_int) -> c_int,
    gp_widget_get_readonly: (Ptr, *mut c_int) -> c_int,
    gp_widget_get_range: (Ptr, *mut f32, *mut f32, *mut f32) -> c_int,
    gp_widget_count_choices: (Ptr) -> c_int,
    gp_widget_get_choice: (Ptr, c_int, *mut *const c_char) -> c_int,
    gp_widget_set_value: (Ptr, *const c_void) -> c_int,
}

fn check(code: c_int) -> Result<(), Error> {
    if code < 0 {
        Err(Error::Camera(format!("libgphoto2 error {code}; check connection, camera mode and competing tether applications")))
    } else {
        Ok(())
    }
}

impl Api {
    fn load() -> Result<Arc<Self>, Error> {
        let names: Vec<std::ffi::OsString> = if let Some(path) = std::env::var_os("FLORASTACK_GPHOTO_LIBRARY") {
            if !std::path::Path::new(&path).is_absolute() {
                return Err(Error::Invalid("FLORASTACK_GPHOTO_LIBRARY must be an absolute shared-library path".into()));
            }
            vec![path]
        } else if cfg!(target_os = "macos") {
            vec!["/opt/homebrew/lib/libgphoto2.6.dylib".into(), "/usr/local/lib/libgphoto2.6.dylib".into(), "libgphoto2.6.dylib".into()]
        } else {
            vec!["libgphoto2.so.6".into()]
        };
        let mut failures = Vec::new();
        let mut last_error = None;
        for name in names {
            // SAFETY: loads an explicitly chosen system/user library. Its constructors obey the platform loader contract.
            // No symbols are called until every signature in Api is resolved; the user may replace this LGPL library.
            match unsafe { Library::new(&name) } {
                Ok(library) => return Self::from_library(library),
                Err(error) => {
                    failures.push(format!("{}: {error}", name.to_string_lossy()));
                    last_error = Some(error);
                }
            }
        }
        let operation = format!("install libgphoto2 or set FLORASTACK_GPHOTO_LIBRARY: {}", failures.join("; "));
        if let Some(source) = last_error { Err(Error::Library { operation, source }) } else { Err(Error::Camera(operation)) }
    }
}

struct Object {
    ptr: NonNull<c_void>,
    api: Arc<Api>,
    free: unsafe extern "C" fn(Ptr) -> c_int,
}
impl Object {
    fn new(api: &Arc<Api>, new: unsafe extern "C" fn(*mut Ptr) -> c_int, free: unsafe extern "C" fn(Ptr) -> c_int) -> Result<Self, Error> {
        let mut ptr = std::ptr::null_mut();
        // SAFETY: valid out-pointer; new/free are the matched pair from this Api. Ownership transfers only on success.
        check(unsafe { new(&raw mut ptr) })?;
        Ok(Self { ptr: NonNull::new(ptr).ok_or_else(|| Error::Camera("library returned a null object".into()))?, api: api.clone(), free })
    }
    fn raw(&self) -> Ptr {
        self.ptr.as_ptr()
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: exclusively owned live object; its matching destructor and library are still present.
        unsafe {
            (self.free)(self.raw());
        }
    }
}

struct Context {
    ptr: NonNull<c_void>,
    api: Arc<Api>,
}
impl Context {
    fn new(api: &Arc<Api>) -> Result<Self, Error> {
        // SAFETY: no arguments; returns a new owned GPContext or null on allocation failure.
        let ptr = unsafe { (api.gp_context_new)() };
        Ok(Self { ptr: NonNull::new(ptr).ok_or_else(|| Error::Camera("cannot allocate camera context".into()))?, api: api.clone() })
    }
    fn raw(&self) -> Ptr {
        self.ptr.as_ptr()
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: exactly one reference is owned; library outlives the unref.
        unsafe {
            (self.api.gp_context_unref)(self.raw());
        }
    }
}

pub fn discover() -> Result<Vec<String>, Error> {
    let api = Api::load()?;
    let context = Context::new(&api)?;
    let list = Object::new(&api, api.gp_list_new, api.gp_list_free)?;
    // SAFETY: all objects are live and exclusive, and the library holds list entries until list is freed.
    unsafe {
        check((api.gp_camera_autodetect)(list.raw(), context.raw()))?;
        let count = (api.gp_list_count)(list.raw());
        check(count)?;
        if count > 1024 {
            return Err(Error::Camera("too many discovered cameras".into()));
        }
        let mut cameras = Vec::new();
        for i in 0..count {
            let mut name = std::ptr::null();
            let mut port = std::ptr::null();
            check((api.gp_list_get_name)(list.raw(), i, &raw mut name))?;
            check((api.gp_list_get_value)(list.raw(), i, &raw mut port))?;
            if name.is_null() || port.is_null() {
                return Err(Error::Camera("null camera description".into()));
            }
            cameras.push(format!("{} ({})", CStr::from_ptr(name).to_string_lossy(), CStr::from_ptr(port).to_string_lossy()));
        }
        Ok(cameras)
    }
}

pub struct Connection {
    camera: Object,
    context: Context,
    capabilities: Capabilities,
}
// SAFETY: the connection, context and library-owned objects move together and have exclusive ownership.
// No callbacks or borrowed object pointers escape. The engine serializes all access; Connection is not Sync.
unsafe impl Send for Connection {}

impl Connection {
    /// Refuses ambiguous selection; connect just one camera before opening this driver.
    pub fn connect() -> Result<Self, Error> {
        let cameras = discover()?;
        if cameras.len() != 1 {
            return Err(Error::Unavailable("connect exactly one camera (discovery lists attached devices)".into()));
        }
        let api = Api::load()?;
        let context = Context::new(&api)?;
        let camera = Object::new(&api, api.gp_camera_new, api.gp_camera_free)?;
        // SAFETY: newly allocated exclusive camera/context, still backed by Api. Failed init is cleaned up by Object.
        check(unsafe { (api.gp_camera_init)(camera.raw(), context.raw()) })?;
        let mut connection = Self { camera, context, capabilities: Capabilities { capture: true, ..Capabilities::default() } };
        // Probe preview first, as many cameras expose focus widgets only after entering live view.
        connection.capabilities.live_view = connection.live_view(&florastack_core::Unmonitored).is_ok();
        connection.capabilities.focus = connection.focus_widget().is_ok();
        Ok(connection)
    }

    fn focus_widget(&self) -> Result<Object, Error> {
        let api = &self.camera.api;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: live camera/context and valid NUL-terminated widget name; successful call transfers widget ownership.
        check(unsafe { (api.gp_camera_get_single_config)(self.camera.raw(), c"manualfocusdrive".as_ptr(), &raw mut ptr, self.context.raw()) })?;
        let widget =
            Object { ptr: NonNull::new(ptr).ok_or_else(|| Error::Camera("null focus widget".into()))?, api: api.clone(), free: api.gp_widget_free };
        let mut readonly = 1;
        // SAFETY: live widget with correctly typed output value.
        check(unsafe { (api.gp_widget_get_readonly)(widget.raw(), &raw mut readonly) })?;
        if readonly != 0 {
            return Err(Error::Unavailable("camera's manualfocusdrive is read-only".into()));
        }
        Ok(widget)
    }

    fn bytes(file: &Object) -> Result<Vec<u8>, Error> {
        let mut ptr = std::ptr::null();
        let mut size = 0;
        // SAFETY: live CameraFile; output pointers are valid and its storage remains live through the copy.
        check(unsafe { (file.api.gp_file_get_data_and_size)(file.raw(), &raw mut ptr, &raw mut size) })?;
        let size = usize::try_from(size).map_err(|_| Error::Camera("camera file too large".into()))?;
        if size == 0 || size > 1024 * 1024 * 1024 || ptr.is_null() {
            return Err(Error::Camera("empty or oversized camera file".into()));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|_| Error::Camera("cannot allocate camera download".into()))?;
        // SAFETY: libgphoto2 guarantees size readable bytes in the live file's buffer, bounded above and below isize::MAX.
        bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) });
        Ok(bytes)
    }
}

impl Camera for Connection {
    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn capture(&mut self, monitor: &dyn Monitor) -> Result<Capture, Error> {
        monitor.checkpoint()?;
        let api = &self.camera.api;
        let mut path = FilePath { name: [0; 128], folder: [0; 1024] };
        let file = Object::new(api, api.gp_file_new, api.gp_file_free)?;
        // SAFETY: FilePath matches the public ABI and is writable. GP_CAPTURE_IMAGE=0; objects are live and exclusive.
        check(unsafe { (api.gp_camera_capture)(self.camera.raw(), 0, &raw mut path, self.context.raw()) })?;
        // Validate termination before letting the library read the returned path buffers.
        if !path.name.contains(&0) || !path.folder.contains(&0) {
            return Err(Error::Camera("unterminated capture path".into()));
        }
        // SAFETY: terminated path arrays, live file, camera/context; GP_FILE_TYPE_NORMAL=1 downloads original bytes.
        check(unsafe { (api.gp_camera_file_get)(self.camera.raw(), path.folder.as_ptr(), path.name.as_ptr(), 1, file.raw(), self.context.raw()) })?;
        // SAFETY: name has a terminator within its array, checked above, and remains live through this copy.
        let name = unsafe { CStr::from_ptr(path.name.as_ptr()) }.to_string_lossy().into_owned();
        // A completed exposure is retained even if cancelled during the blocking camera call. Host checks before the next shot.
        Ok(Capture { name, bytes: Self::bytes(&file)? })
    }

    fn live_view(&mut self, monitor: &dyn Monitor) -> Result<Capture, Error> {
        monitor.checkpoint()?;
        let api = &self.camera.api;
        let file = Object::new(api, api.gp_file_new, api.gp_file_free)?;
        // SAFETY: exclusive initialized camera, writable CameraFile and matching context.
        check(unsafe { (api.gp_camera_capture_preview)(self.camera.raw(), file.raw(), self.context.raw()) })?;
        Ok(Capture { name: "live-view.jpg".into(), bytes: Self::bytes(&file)? })
    }

    fn focus(&mut self, pulses: i32, monitor: &dyn Monitor) -> Result<(), Error> {
        if pulses.unsigned_abs() > 10000 {
            return Err(Error::Invalid("at most 10000 unit focus pulses per call".into()));
        }
        for _ in 0..pulses.unsigned_abs() {
            monitor.checkpoint()?;
            let widget = self.focus_widget()?;
            let api = &self.camera.api;
            let mut kind = 0;
            // SAFETY: writable integer output for a live widget.
            check(unsafe { (api.gp_widget_get_type)(widget.raw(), &raw mut kind) })?;
            // SAFETY: widget types are checked before passing their documented value type, and all storage stays live until set returns.
            unsafe {
                match kind {
                    3 => {
                        let (mut min, mut max, mut step) = (0.0, 0.0, 0.0);
                        check((api.gp_widget_get_range)(widget.raw(), &raw mut min, &raw mut max, &raw mut step))?;
                        let value = if pulses > 0 { 1.0_f32 } else { -1.0_f32 };
                        if min > value || max < value || !min.is_finite() || !max.is_finite() {
                            return Err(Error::Unavailable("focus range does not support signed unit pulses".into()));
                        }
                        check((api.gp_widget_set_value)(widget.raw(), (&raw const value).cast()))?;
                    }
                    5 | 6 => {
                        let wanted = if pulses > 0 { c"Far 1" } else { c"Near 1" };
                        let count = (api.gp_widget_count_choices)(widget.raw());
                        check(count)?;
                        if count > 1024 {
                            return Err(Error::Camera("too many focus choices".into()));
                        }
                        let mut found = false;
                        for i in 0..count {
                            let mut choice = std::ptr::null();
                            check((api.gp_widget_get_choice)(widget.raw(), i, &raw mut choice))?;
                            if !choice.is_null() && CStr::from_ptr(choice) == wanted {
                                found = true;
                            }
                        }
                        if !found {
                            return Err(Error::Unavailable("camera has no Near 1/Far 1 focus choices".into()));
                        }
                        let value = CString::from(wanted);
                        check((api.gp_widget_set_value)(widget.raw(), value.as_ptr().cast()))?;
                    }
                    _ => return Err(Error::Unavailable("unsupported focus widget type".into())),
                }
                check((api.gp_camera_set_single_config)(self.camera.raw(), c"manualfocusdrive".as_ptr(), widget.raw(), self.context.raw()))?;
            }
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: camera/context are still live here; field destructors then free camera before context and library.
        unsafe {
            (self.camera.api.gp_camera_exit)(self.camera.raw(), self.context.raw());
        }
    }
}
