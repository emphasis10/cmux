use gtk::glib::translate::from_glib_full;
use gtk::prelude::*;
use serde::Serialize;
use std::env;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhosttyStatus {
    pub available: bool,
    pub library_available: bool,
    pub renderer_available: bool,
    pub library_path: Option<String>,
    pub version: Option<String>,
    pub abi_version: Option<u32>,
    pub reason: Option<String>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyGtkEmbedInfo {
    abi_version: u32,
    version: *const c_char,
    version_len: usize,
    capabilities: u64,
}

#[repr(C)]
struct GhosttyGtkSurfaceConfig {
    command: *const c_char,
    working_directory: *const c_char,
    title: *const c_char,
}

#[repr(C)]
struct GhosttyGtkString {
    ptr: *const u8,
    len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhosttyGtkSurfaceHealth {
    pub alive: bool,
    pub child_exited: bool,
    pub rows: u32,
    pub cols: u32,
}

type GhosttyGtkEmbedInit = unsafe extern "C" fn(usize, *const *const c_char) -> c_int;
type GhosttyGtkEmbedInfoFn = unsafe extern "C" fn() -> GhosttyGtkEmbedInfo;
type GhosttyGtkSurfaceNew =
    unsafe extern "C" fn(*const GhosttyGtkSurfaceConfig) -> *mut gtk::ffi::GtkWidget;
type GhosttyGtkSurfaceFree = unsafe extern "C" fn(*mut gtk::ffi::GtkWidget);
type GhosttyGtkSurfaceFocus = unsafe extern "C" fn(*mut gtk::ffi::GtkWidget, bool);
type GhosttyGtkSurfaceResize =
    unsafe extern "C" fn(*mut gtk::ffi::GtkWidget, i32, i32, f64, f64);
type GhosttyGtkSurfaceSendText =
    unsafe extern "C" fn(*mut gtk::ffi::GtkWidget, *const u8, usize) -> bool;
type GhosttyGtkSurfaceSendKey =
    unsafe extern "C" fn(*mut gtk::ffi::GtkWidget, *const c_char, usize) -> bool;
type GhosttyGtkSurfaceReadText =
    unsafe extern "C" fn(*mut gtk::ffi::GtkWidget, *mut GhosttyGtkString) -> bool;
type GhosttyGtkStringFree = unsafe extern "C" fn(GhosttyGtkString);
type GhosttyGtkSurfaceRefresh = unsafe extern "C" fn(*mut gtk::ffi::GtkWidget);
type GhosttyGtkSurfaceHealthFn =
    unsafe extern "C" fn(*mut gtk::ffi::GtkWidget) -> GhosttyGtkSurfaceHealth;

#[derive(Clone, Copy)]
struct GhosttyApi {
    init: GhosttyGtkEmbedInit,
    info: GhosttyGtkEmbedInfoFn,
    surface_new: GhosttyGtkSurfaceNew,
    surface_free: GhosttyGtkSurfaceFree,
    surface_focus: GhosttyGtkSurfaceFocus,
    #[allow(dead_code)]
    surface_resize: GhosttyGtkSurfaceResize,
    surface_send_text: GhosttyGtkSurfaceSendText,
    #[allow(dead_code)]
    surface_send_key: GhosttyGtkSurfaceSendKey,
    surface_read_text: GhosttyGtkSurfaceReadText,
    string_free: GhosttyGtkStringFree,
    #[allow(dead_code)]
    surface_refresh: GhosttyGtkSurfaceRefresh,
    surface_health: GhosttyGtkSurfaceHealthFn,
}

unsafe impl Send for GhosttyApi {}
unsafe impl Sync for GhosttyApi {}

pub struct GhosttySurfaceHandle {
    widget: usize,
    api: &'static GhosttyApi,
}

unsafe impl Send for GhosttySurfaceHandle {}
unsafe impl Sync for GhosttySurfaceHandle {}

impl GhosttySurfaceHandle {
    pub fn send_text(&self, text: &str) -> bool {
        self.send_bytes(text.as_bytes())
    }

    pub fn send_bytes(&self, bytes: &[u8]) -> bool {
        let api = self.api;
        let widget = self.widget;
        let bytes = bytes.to_vec();
        on_main(move || unsafe {
            (api.surface_send_text)(
                widget as *mut gtk::ffi::GtkWidget,
                bytes.as_ptr(),
                bytes.len(),
            )
        })
    }

    #[allow(dead_code)]
    pub fn send_key(&self, key: &str) -> bool {
        let api = self.api;
        let widget = self.widget;
        let key = key.as_bytes().to_vec();
        on_main(move || unsafe {
            (api.surface_send_key)(
                widget as *mut gtk::ffi::GtkWidget,
                key.as_ptr().cast(),
                key.len(),
            )
        })
    }

    pub fn read_text(&self) -> Option<String> {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe {
            let mut out = GhosttyGtkString {
                ptr: std::ptr::null(),
                len: 0,
            };
            if !(api.surface_read_text)(widget as *mut gtk::ffi::GtkWidget, &mut out) {
                return None;
            }
            let text = if out.ptr.is_null() || out.len == 0 {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(out.ptr, out.len)).to_string()
            };
            (api.string_free)(out);
            Some(text)
        })
    }

    pub fn focus(&self, focused: bool) {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe {
            (api.surface_focus)(widget as *mut gtk::ffi::GtkWidget, focused);
        });
    }

    #[allow(dead_code)]
    pub fn resize(&self, width_px: i32, height_px: i32, scale_x: f64, scale_y: f64) {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe {
            (api.surface_resize)(
                widget as *mut gtk::ffi::GtkWidget,
                width_px,
                height_px,
                scale_x,
                scale_y,
            );
        });
    }

    #[allow(dead_code)]
    pub fn refresh(&self) {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe {
            (api.surface_refresh)(widget as *mut gtk::ffi::GtkWidget);
        });
    }

    pub fn health(&self) -> GhosttyGtkSurfaceHealth {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe { (api.surface_health)(widget as *mut gtk::ffi::GtkWidget) })
    }
}

impl Drop for GhosttySurfaceHandle {
    fn drop(&mut self) {
        let api = self.api;
        let widget = self.widget;
        on_main(move || unsafe {
            (api.surface_free)(widget as *mut gtk::ffi::GtkWidget);
        });
    }
}

fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let context = gtk::glib::MainContext::default();
    if context.is_owner() {
        return f();
    }

    let (sender, receiver) = mpsc::sync_channel(1);
    context.invoke(move || {
        let _ = sender.send(f());
    });
    receiver
        .recv()
        .expect("Ghostty GTK main-context call was dropped")
}

pub fn status() -> &'static GhosttyStatus {
    static STATUS: OnceLock<GhosttyStatus> = OnceLock::new();
    STATUS.get_or_init(|| match loader() {
        Some(loader) => loader.status.clone(),
        None => GhosttyStatus {
            available: false,
            library_available: false,
            renderer_available: false,
            library_path: None,
            version: None,
            abi_version: None,
            reason: Some("libghostty-gtk-embed was not found; using the Linux PTY fallback".to_string()),
        },
    })
}

pub fn renderer_available() -> bool {
    loader()
        .map(|loader| loader.status.renderer_available)
        .unwrap_or(false)
}

pub fn create_surface(
    title: &str,
    initial_command: Option<&str>,
    working_directory: Option<&str>,
) -> Option<(gtk::Widget, GhosttySurfaceHandle)> {
    let loader = loader()?;
    if !loader.status.renderer_available {
        return None;
    }

    let title = CString::new(title).ok()?;
    let command = initial_command.and_then(|value| CString::new(value).ok());
    let working_directory = working_directory.and_then(|value| CString::new(value).ok());
    let config = GhosttyGtkSurfaceConfig {
        command: command
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
        working_directory: working_directory
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
        title: title.as_ptr(),
    };

    let raw = unsafe { (loader.api.surface_new)(&config) };
    if raw.is_null() {
        return None;
    }

    let widget: gtk::Widget = unsafe { from_glib_full(raw) };
    widget.set_focusable(true);
    widget.set_hexpand(true);
    widget.set_vexpand(true);
    widget.add_css_class("cmux-ghostty-surface");

    let handle = GhosttySurfaceHandle {
        widget: raw as usize,
        api: &loader.api,
    };

    Some((widget, handle))
}

struct GhosttyLoader {
    api: GhosttyApi,
    status: GhosttyStatus,
}

unsafe impl Send for GhosttyLoader {}
unsafe impl Sync for GhosttyLoader {}

fn loader() -> Option<&'static GhosttyLoader> {
    static LOADER: OnceLock<Option<GhosttyLoader>> = OnceLock::new();
    LOADER.get_or_init(load).as_ref()
}

fn load() -> Option<GhosttyLoader> {
    let mut last_reason = None;
    for candidate in candidates() {
        match unsafe { load_candidate(&candidate) } {
            Ok(loader) => return Some(loader),
            Err(reason) => last_reason = Some(reason),
        }
    }

    Some(GhosttyLoader {
        api: missing_api(),
        status: GhosttyStatus {
            available: false,
            library_available: false,
            renderer_available: false,
            library_path: None,
            version: None,
            abi_version: None,
            reason: last_reason.or_else(|| {
                Some("libghostty-gtk-embed was not found; using the Linux PTY fallback".to_string())
            }),
        },
    })
}

unsafe fn load_candidate(candidate: &Candidate) -> Result<GhosttyLoader, String> {
    let (name, display) = match candidate {
        Candidate::Path(path) => {
            if !path.exists() {
                return Err(format!("{} does not exist", path.display()));
            }
            let name = CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| format!("{} contains an interior NUL byte", path.display()))?;
            (name, path.display().to_string())
        }
        Candidate::Library(name) => (
            CString::new(name.as_str()).map_err(|_| "invalid library name".to_string())?,
            name.clone(),
        ),
    };

    let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
    if handle.is_null() {
        return Err(dlerror().unwrap_or_else(|| format!("failed to open {display}")));
    }

    let api = GhosttyApi {
        init: load_symbol(handle, b"ghostty_gtk_embed_init\0")?,
        info: load_symbol(handle, b"ghostty_gtk_embed_info\0")?,
        surface_new: load_symbol(handle, b"ghostty_gtk_surface_new\0")?,
        surface_free: load_symbol(handle, b"ghostty_gtk_surface_free\0")?,
        surface_focus: load_symbol(handle, b"ghostty_gtk_surface_focus\0")?,
        surface_resize: load_symbol(handle, b"ghostty_gtk_surface_resize\0")?,
        surface_send_text: load_symbol(handle, b"ghostty_gtk_surface_send_text\0")?,
        surface_send_key: load_symbol(handle, b"ghostty_gtk_surface_send_key\0")?,
        surface_read_text: load_symbol(handle, b"ghostty_gtk_surface_read_text\0")?,
        string_free: load_symbol(handle, b"ghostty_gtk_string_free\0")?,
        surface_refresh: load_symbol(handle, b"ghostty_gtk_surface_refresh\0")?,
        surface_health: load_symbol(handle, b"ghostty_gtk_surface_health\0")?,
    };

    let probe = on_main({
        let display = display.clone();
        move || unsafe {
            let argv0 = CString::new("cmux-gtk").unwrap();
            let argv = [argv0.as_ptr()];
            if (api.init)(argv.len(), argv.as_ptr()) != 0 {
                return Err(GhosttyStatus {
                    available: false,
                    library_available: true,
                    renderer_available: false,
                    library_path: Some(display),
                    version: None,
                    abi_version: None,
                    reason: Some("libghostty-gtk-embed initialized with an error".to_string()),
                });
            }

            let info = (api.info)();
            let version = if info.version.is_null() || info.version_len == 0 {
                None
            } else {
                Some(
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        info.version.cast::<u8>(),
                        info.version_len,
                    ))
                    .to_string(),
                )
            };

            let title = CString::new("cmux-probe").unwrap();
            let config = GhosttyGtkSurfaceConfig {
                command: std::ptr::null(),
                working_directory: std::ptr::null(),
                title: title.as_ptr(),
            };
            let probe = (api.surface_new)(&config);
            if probe.is_null() {
                return Err(GhosttyStatus {
                    available: false,
                    library_available: true,
                    renderer_available: false,
                    library_path: Some(display),
                    version,
                    abi_version: Some(info.abi_version),
                    reason: Some(
                        "libghostty-gtk-embed could not create a probe surface".to_string(),
                    ),
                });
            }
            (api.surface_free)(probe);
            Ok((version, info.abi_version))
        }
    });

    let (version, abi_version) = match probe {
        Ok(values) => values,
        Err(status) => return Ok(GhosttyLoader { api, status }),
    };

    Ok(GhosttyLoader {
        api,
        status: GhosttyStatus {
            available: true,
            library_available: true,
            renderer_available: true,
            library_path: Some(display),
            version,
            abi_version: Some(abi_version),
            reason: None,
        },
    })
}

unsafe fn load_symbol<T: Copy>(handle: *mut c_void, name: &[u8]) -> Result<T, String> {
    let name = CStr::from_bytes_with_nul(name).map_err(|_| "invalid symbol name".to_string())?;
    let symbol = libc::dlsym(handle, name.as_ptr());
    if symbol.is_null() {
        return Err(format!("missing symbol {}", name.to_string_lossy()));
    }
    Ok(std::mem::transmute_copy(&symbol))
}

fn dlerror() -> Option<String> {
    unsafe {
        let error = libc::dlerror();
        if error.is_null() {
            None
        } else {
            Some(CStr::from_ptr(error).to_string_lossy().to_string())
        }
    }
}

enum Candidate {
    Path(PathBuf),
    Library(String),
}

fn candidates() -> Vec<Candidate> {
    let mut candidates = Vec::new();
    if let Some(path) =
        env::var_os("CMUX_LIBGHOSTTY_GTK_EMBED_PATH").filter(|value| !value.is_empty())
    {
        candidates.push(Candidate::Path(PathBuf::from(path)));
    }
    candidates.extend([
        Candidate::Path(PathBuf::from("/usr/lib/cmux/libghostty-gtk-embed.so")),
        Candidate::Path(PathBuf::from("/usr/local/lib/libghostty-gtk-embed.so")),
        Candidate::Library("libghostty-gtk-embed.so".to_string()),
    ]);
    candidates
}

fn missing_api() -> GhosttyApi {
    unsafe extern "C" fn init(_: usize, _: *const *const c_char) -> c_int {
        1
    }
    unsafe extern "C" fn info() -> GhosttyGtkEmbedInfo {
        GhosttyGtkEmbedInfo {
            abi_version: 0,
            version: std::ptr::null(),
            version_len: 0,
            capabilities: 0,
        }
    }
    unsafe extern "C" fn new(_: *const GhosttyGtkSurfaceConfig) -> *mut gtk::ffi::GtkWidget {
        std::ptr::null_mut()
    }
    unsafe extern "C" fn free(_: *mut gtk::ffi::GtkWidget) {}
    unsafe extern "C" fn focus(_: *mut gtk::ffi::GtkWidget, _: bool) {}
    unsafe extern "C" fn resize(_: *mut gtk::ffi::GtkWidget, _: i32, _: i32, _: f64, _: f64) {}
    unsafe extern "C" fn send_text(_: *mut gtk::ffi::GtkWidget, _: *const u8, _: usize) -> bool {
        false
    }
    unsafe extern "C" fn send_key(_: *mut gtk::ffi::GtkWidget, _: *const c_char, _: usize) -> bool {
        false
    }
    unsafe extern "C" fn read_text(
        _: *mut gtk::ffi::GtkWidget,
        _: *mut GhosttyGtkString,
    ) -> bool {
        false
    }
    unsafe extern "C" fn string_free(_: GhosttyGtkString) {}
    unsafe extern "C" fn refresh(_: *mut gtk::ffi::GtkWidget) {}
    unsafe extern "C" fn health(_: *mut gtk::ffi::GtkWidget) -> GhosttyGtkSurfaceHealth {
        GhosttyGtkSurfaceHealth {
            alive: false,
            child_exited: true,
            rows: 0,
            cols: 0,
        }
    }
    GhosttyApi {
        init,
        info,
        surface_new: new,
        surface_free: free,
        surface_focus: focus,
        surface_resize: resize,
        surface_send_text: send_text,
        surface_send_key: send_key,
        surface_read_text: read_text,
        string_free,
        surface_refresh: refresh,
        surface_health: health,
    }
}
