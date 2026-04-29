use gtk::glib::translate::{from_glib_none, ToGlibPtr};
use gtk::prelude::*;
use serde::Serialize;
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[link(name = "webkitgtk-6.0")]
extern "C" {
    fn webkit_web_view_new() -> *mut gtk::ffi::GtkWidget;
    fn webkit_web_view_load_uri(web_view: *mut gtk::ffi::GtkWidget, uri: *const c_char);
    fn webkit_web_view_reload(web_view: *mut gtk::ffi::GtkWidget);
    fn webkit_web_view_go_back(web_view: *mut gtk::ffi::GtkWidget);
    fn webkit_web_view_go_forward(web_view: *mut gtk::ffi::GtkWidget);
    fn webkit_web_view_get_network_session(web_view: *mut gtk::ffi::GtkWidget) -> *mut c_void;
    fn webkit_web_view_get_snapshot(
        web_view: *mut gtk::ffi::GtkWidget,
        region: i32,
        options: i32,
        cancellable: *mut c_void,
        callback: GAsyncReadyCallback,
        user_data: *mut c_void,
    );
    fn webkit_web_view_get_snapshot_finish(
        web_view: *mut gtk::ffi::GtkWidget,
        result: *mut c_void,
        error: *mut *mut GError,
    ) -> *mut c_void;
    fn webkit_web_view_evaluate_javascript(
        web_view: *mut gtk::ffi::GtkWidget,
        script: *const c_char,
        length: isize,
        world_name: *const c_char,
        source_uri: *const c_char,
        cancellable: *mut c_void,
        callback: GAsyncReadyCallback,
        user_data: *mut c_void,
    );
    fn webkit_web_view_evaluate_javascript_finish(
        web_view: *mut gtk::ffi::GtkWidget,
        result: *mut c_void,
        error: *mut *mut GError,
    ) -> *mut c_void;
    fn webkit_script_dialog_ref(dialog: *mut c_void) -> *mut c_void;
    fn webkit_script_dialog_unref(dialog: *mut c_void);
    fn webkit_script_dialog_close(dialog: *mut c_void);
    fn webkit_script_dialog_get_dialog_type(dialog: *mut c_void) -> i32;
    fn webkit_script_dialog_get_message(dialog: *mut c_void) -> *const c_char;
    fn webkit_script_dialog_prompt_get_default_text(dialog: *mut c_void) -> *const c_char;
    fn webkit_script_dialog_confirm_set_confirmed(dialog: *mut c_void, confirmed: i32);
    fn webkit_script_dialog_prompt_set_text(dialog: *mut c_void, text: *const c_char);
    fn webkit_download_get_request(download: *mut c_void) -> *mut c_void;
    fn webkit_download_get_destination(download: *mut c_void) -> *const c_char;
    fn webkit_download_set_destination(download: *mut c_void, destination: *const c_char);
    fn webkit_download_get_received_data_length(download: *mut c_void) -> u64;
    fn webkit_uri_request_get_uri(request: *mut c_void) -> *const c_char;
}

#[link(name = "javascriptcoregtk-6.0")]
extern "C" {
    fn jsc_value_to_string(value: *mut c_void) -> *mut c_char;
}

#[link(name = "glib-2.0")]
extern "C" {
    fn g_free(mem: *mut c_void);
    fn g_error_free(error: *mut GError);
    fn g_bytes_get_data(bytes: *mut c_void, size: *mut usize) -> *const u8;
    fn g_bytes_unref(bytes: *mut c_void);
}

#[link(name = "gobject-2.0")]
extern "C" {
    fn g_signal_connect_data(
        instance: *mut c_void,
        detailed_signal: *const c_char,
        c_handler: *mut c_void,
        data: *mut c_void,
        destroy_data: GClosureNotify,
        connect_flags: i32,
    ) -> u64;
    fn g_object_unref(object: *mut c_void);
}

#[link(name = "gtk-4")]
extern "C" {
    fn gdk_texture_save_to_png_bytes(texture: *mut c_void) -> *mut c_void;
    fn gdk_texture_get_width(texture: *mut c_void) -> i32;
    fn gdk_texture_get_height(texture: *mut c_void) -> i32;
}

type GAsyncReadyCallback =
    Option<unsafe extern "C" fn(source_object: *mut c_void, result: *mut c_void, user_data: *mut c_void)>;
type GClosureNotify = Option<unsafe extern "C" fn(data: *mut c_void, closure: *mut c_void)>;

#[repr(C)]
struct GError {
    domain: u32,
    code: i32,
    message: *mut c_char,
}

const WEBKIT_SNAPSHOT_REGION_VISIBLE: i32 = 0;
const WEBKIT_SNAPSHOT_REGION_FULL_DOCUMENT: i32 = 1;
const MAX_RUNTIME_EVENTS: usize = 200;

#[derive(Clone, Debug, Serialize)]
pub struct ScreenshotResult {
    pub png_base64: String,
    pub width: i32,
    pub height: i32,
    pub full_page: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowserEvent {
    pub level: String,
    pub text: String,
    pub source: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DialogInfo {
    pub id: String,
    pub kind: String,
    pub message: String,
    pub default_text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadInfo {
    pub id: String,
    pub url: String,
    pub path: String,
    pub status: String,
    pub received_bytes: u64,
    pub error: Option<String>,
}

#[derive(Clone)]
struct RuntimeRecord {
    web_view: usize,
    active_frame_selector: Option<String>,
    console: Vec<BrowserEvent>,
    errors: Vec<BrowserEvent>,
    dialogs: Vec<DialogRecord>,
    downloads: Vec<DownloadRecord>,
}

#[derive(Clone)]
struct DialogRecord {
    info: DialogInfo,
    dialog: usize,
}

#[derive(Clone)]
struct DownloadRecord {
    info: DownloadInfo,
}

pub fn new_web_view(surface_id: &str, uri: &str) -> gtk::Widget {
    let sanitized_uri = CString::new(uri).unwrap_or_else(|_| CString::new("about:blank").unwrap());
    unsafe {
        let widget: gtk::Widget = from_glib_none(webkit_web_view_new());
        let web_view = widget.to_glib_none().0;
        webkit_web_view_load_uri(web_view, sanitized_uri.as_ptr());
        register(surface_id, web_view as usize);
        install_signal_handlers(surface_id, web_view as *mut c_void);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        install_observers_later(surface_id);
        widget
    }
}

fn registry() -> &'static Mutex<std::collections::BTreeMap<String, RuntimeRecord>> {
    static REGISTRY: OnceLock<Mutex<std::collections::BTreeMap<String, RuntimeRecord>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn register(surface_id: &str, web_view: usize) {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    registry.insert(
        surface_id.to_string(),
        RuntimeRecord {
            web_view,
            active_frame_selector: None,
            console: Vec::new(),
            errors: Vec::new(),
            dialogs: Vec::new(),
            downloads: Vec::new(),
        },
    );
}

pub fn navigate(surface_id: &str, uri: &str) -> bool {
    let web_view = web_view_for(surface_id);
    let Some(web_view) = web_view else {
        return false;
    };
    let uri = uri.to_string();
    let surface_id = surface_id.to_string();
    gtk::glib::MainContext::default().invoke(move || {
        let sanitized_uri =
            CString::new(uri).unwrap_or_else(|_| CString::new("about:blank").unwrap());
        unsafe {
            webkit_web_view_load_uri(web_view as *mut gtk::ffi::GtkWidget, sanitized_uri.as_ptr());
        }
        install_observers_later(&surface_id);
    });
    true
}

pub fn reload(surface_id: &str) -> bool {
    with_web_view(surface_id, |web_view| unsafe {
        webkit_web_view_reload(web_view);
    })
}

pub fn go_back(surface_id: &str) -> bool {
    with_web_view(surface_id, |web_view| unsafe {
        webkit_web_view_go_back(web_view);
    })
}

pub fn go_forward(surface_id: &str) -> bool {
    with_web_view(surface_id, |web_view| unsafe {
        webkit_web_view_go_forward(web_view);
    })
}

pub fn focus(surface_id: &str) -> bool {
    with_web_view(surface_id, |web_view| unsafe {
        let widget: gtk::Widget = from_glib_none(web_view);
        widget.grab_focus();
    })
}

pub fn screenshot(
    surface_id: &str,
    full_page: bool,
    timeout: Duration,
) -> Option<Result<ScreenshotResult, String>> {
    let web_view = web_view_for(surface_id)?;
    let (sender, receiver) = mpsc::channel::<Result<ScreenshotResult, String>>();
    gtk::glib::MainContext::default().invoke(move || {
        let sender = Box::new(SnapshotRequest { sender, full_page });
        unsafe {
            webkit_web_view_get_snapshot(
                web_view as *mut gtk::ffi::GtkWidget,
                if full_page {
                    WEBKIT_SNAPSHOT_REGION_FULL_DOCUMENT
                } else {
                    WEBKIT_SNAPSHOT_REGION_VISIBLE
                },
                0,
                std::ptr::null_mut(),
                Some(snapshot_finished),
                Box::into_raw(sender).cast(),
            );
        }
    });
    match receiver.recv_timeout(timeout) {
        Ok(result) => Some(result),
        Err(mpsc::RecvTimeoutError::Timeout) => Some(Err("screenshot timed out".to_string())),
        Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err("screenshot was cancelled".to_string())),
    }
}

pub fn evaluate_javascript(
    surface_id: &str,
    script: &str,
    timeout: Duration,
) -> Option<Result<String, String>> {
    let web_view = web_view_for(surface_id)?;
    let script = script.to_string();
    let (sender, receiver) = mpsc::channel::<Result<String, String>>();
    gtk::glib::MainContext::default().invoke(move || {
        let Ok(script) = CString::new(script) else {
            let _ = sender.send(Err("script contains an interior NUL byte".to_string()));
            return;
        };
        let sender = Box::new(sender);
        unsafe {
            webkit_web_view_evaluate_javascript(
                web_view as *mut gtk::ffi::GtkWidget,
                script.as_ptr(),
                -1,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                Some(evaluate_javascript_finished),
                Box::into_raw(sender).cast(),
            );
        }
    });
    match receiver.recv_timeout(timeout) {
        Ok(result) => Some(result),
        Err(mpsc::RecvTimeoutError::Timeout) => Some(Err("javascript evaluation timed out".to_string())),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Some(Err("javascript evaluation was cancelled".to_string()))
        }
    }
}

pub fn evaluate_expression_json(
    surface_id: &str,
    script: &str,
    timeout: Duration,
) -> Option<Result<String, String>> {
    let wrapped = wrap_script(surface_id, script, ScriptMode::Expression)?;
    evaluate_javascript(surface_id, &wrapped, timeout)
}

pub fn run_statement_json(
    surface_id: &str,
    script: &str,
    timeout: Duration,
) -> Option<Result<String, String>> {
    let wrapped = wrap_script(surface_id, script, ScriptMode::Statement)?;
    evaluate_javascript(surface_id, &wrapped, timeout)
}

enum ScriptMode {
    Expression,
    Statement,
}

fn wrap_script(surface_id: &str, script: &str, mode: ScriptMode) -> Option<String> {
    let frame = {
        let registry = registry().lock().expect("browser registry lock poisoned");
        registry
            .get(surface_id)
            .map(|record| record.active_frame_selector.clone())?
    };
    let frame_prefix = if let Some(selector) = frame {
        let selector = serde_json::to_string(&selector).ok()?;
        format!(
            "const __cmuxFrame = document.querySelector({selector}); if (!__cmuxFrame || !__cmuxFrame.contentDocument) throw new Error('frame is not accessible'); const document = __cmuxFrame.contentDocument; const window = __cmuxFrame.contentWindow;"
        )
    } else {
        String::new()
    };
    let body = match mode {
        ScriptMode::Expression => format!(
            "{frame_prefix} const __cmuxValue = ({script}); return typeof __cmuxValue === 'function' ? __cmuxValue() : __cmuxValue;"
        ),
        ScriptMode::Statement => format!("{frame_prefix} {script}"),
    };
    Some(format!("JSON.stringify((() => {{ {body} }})())"))
}

unsafe extern "C" fn evaluate_javascript_finished(
    source_object: *mut c_void,
    result: *mut c_void,
    user_data: *mut c_void,
) {
    let sender = Box::from_raw(user_data.cast::<mpsc::Sender<Result<String, String>>>());
    let mut error: *mut GError = std::ptr::null_mut();
    let value = webkit_web_view_evaluate_javascript_finish(
        source_object.cast::<gtk::ffi::GtkWidget>(),
        result,
        &mut error,
    );
    if !error.is_null() {
        let message = if (*error).message.is_null() {
            "javascript evaluation failed".to_string()
        } else {
            CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned()
        };
        g_error_free(error);
        let _ = sender.send(Err(message));
        return;
    }
    if value.is_null() {
        let _ = sender.send(Ok(String::new()));
        return;
    }
    let text = jsc_value_to_string(value);
    let output = if text.is_null() {
        String::new()
    } else {
        let output = CStr::from_ptr(text).to_string_lossy().into_owned();
        g_free(text.cast());
        output
    };
    g_object_unref(value);
    let _ = sender.send(Ok(output));
}

struct SnapshotRequest {
    sender: mpsc::Sender<Result<ScreenshotResult, String>>,
    full_page: bool,
}

unsafe extern "C" fn snapshot_finished(
    source_object: *mut c_void,
    result: *mut c_void,
    user_data: *mut c_void,
) {
    let request = Box::from_raw(user_data.cast::<SnapshotRequest>());
    let mut error: *mut GError = std::ptr::null_mut();
    let texture =
        webkit_web_view_get_snapshot_finish(source_object.cast::<gtk::ffi::GtkWidget>(), result, &mut error);
    if !error.is_null() {
        let message = g_error_message(error, "screenshot failed");
        g_error_free(error);
        let _ = request.sender.send(Err(message));
        return;
    }
    if texture.is_null() {
        let _ = request.sender.send(Err("screenshot returned no texture".to_string()));
        return;
    }
    let bytes = gdk_texture_save_to_png_bytes(texture);
    let width = gdk_texture_get_width(texture);
    let height = gdk_texture_get_height(texture);
    g_object_unref(texture);
    if bytes.is_null() {
        let _ = request.sender.send(Err("failed to encode screenshot PNG".to_string()));
        return;
    }
    let mut len = 0_usize;
    let data = g_bytes_get_data(bytes, &mut len);
    let png_base64 = if data.is_null() || len == 0 {
        String::new()
    } else {
        let slice = std::slice::from_raw_parts(data, len);
        encode_base64(slice)
    };
    g_bytes_unref(bytes);
    let _ = request.sender.send(Ok(ScreenshotResult {
        png_base64,
        width,
        height,
        full_page: request.full_page,
    }));
}

fn with_web_view<F>(surface_id: &str, action: F) -> bool
where
    F: FnOnce(*mut gtk::ffi::GtkWidget) + Send + 'static,
{
    let web_view = web_view_for(surface_id);
    let Some(web_view) = web_view else {
        return false;
    };
    gtk::glib::MainContext::default().invoke(move || {
        action(web_view as *mut gtk::ffi::GtkWidget);
    });
    true
}

pub fn is_registered(surface_id: &str) -> bool {
    let registry = registry().lock().expect("browser registry lock poisoned");
    registry.contains_key(surface_id)
}

pub fn unregister(surface_id: &str) -> bool {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    registry.remove(surface_id).is_some()
}

pub fn console_messages(surface_id: &str) -> Vec<BrowserEvent> {
    install_observers(surface_id);
    refresh_script_events(surface_id);
    let registry = registry().lock().expect("browser registry lock poisoned");
    registry
        .get(surface_id)
        .map(|record| record.console.clone())
        .unwrap_or_default()
}

pub fn error_messages(surface_id: &str) -> Vec<BrowserEvent> {
    install_observers(surface_id);
    refresh_script_events(surface_id);
    let registry = registry().lock().expect("browser registry lock poisoned");
    registry
        .get(surface_id)
        .map(|record| record.errors.clone())
        .unwrap_or_default()
}

pub fn respond_to_dialog(
    surface_id: &str,
    accept: bool,
    prompt_text: Option<String>,
) -> Result<DialogInfo, String> {
    let record = {
        let mut registry = registry().lock().expect("browser registry lock poisoned");
        let runtime = registry
            .get_mut(surface_id)
            .ok_or_else(|| "surface does not have a live WebView".to_string())?;
        if runtime.dialogs.is_empty() {
            return Err("no pending dialog".to_string());
        }
        runtime.dialogs.remove(0)
    };
    let info = record.info.clone();
    let response_info = info.clone();
    let text = prompt_text.unwrap_or_default();
    gtk::glib::MainContext::default().invoke(move || unsafe {
        match info.kind.as_str() {
            "confirm" | "before_unload_confirm" => {
                webkit_script_dialog_confirm_set_confirmed(record.dialog as *mut c_void, i32::from(accept));
            }
            "prompt" => {
                if accept {
                    let text = CString::new(text).unwrap_or_else(|_| CString::new("").unwrap());
                    webkit_script_dialog_prompt_set_text(record.dialog as *mut c_void, text.as_ptr());
                }
            }
            _ => {}
        }
        webkit_script_dialog_close(record.dialog as *mut c_void);
        webkit_script_dialog_unref(record.dialog as *mut c_void);
    });
    Ok(response_info)
}

pub fn wait_for_download(surface_id: &str, timeout: Duration) -> Result<DownloadInfo, String> {
    let start = std::time::Instant::now();
    loop {
        {
            let registry = registry().lock().expect("browser registry lock poisoned");
            let runtime = registry
                .get(surface_id)
                .ok_or_else(|| "surface does not have a live WebView".to_string())?;
            if let Some(download) = runtime
                .downloads
                .iter()
                .rev()
                .find(|download| download.info.status == "finished" || download.info.status == "failed")
            {
                return Ok(download.info.clone());
            }
        }
        if start.elapsed() >= timeout {
            return Err("download timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn select_frame(surface_id: &str, selector: &str) -> Result<(), String> {
    let selector_js = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    let script = format!(
        "(() => {{ const frame = document.querySelector({selector_js}); return !!(frame && frame.contentDocument); }})()"
    );
    match evaluate_expression_json(surface_id, &script, Duration::from_secs(2)) {
        Some(Ok(text)) if matches!(serde_json::from_str::<Value>(&text), Ok(Value::Bool(true))) => {
            let mut registry = registry().lock().expect("browser registry lock poisoned");
            if let Some(runtime) = registry.get_mut(surface_id) {
                runtime.active_frame_selector = Some(selector.to_string());
            }
            Ok(())
        }
        Some(Ok(_)) => Err("frame is not accessible".to_string()),
        Some(Err(error)) => Err(error),
        None => Err("surface does not have a live WebView".to_string()),
    }
}

pub fn select_main_frame(surface_id: &str) -> bool {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    let Some(runtime) = registry.get_mut(surface_id) else {
        return false;
    };
    runtime.active_frame_selector = None;
    true
}

pub fn active_frame(surface_id: &str) -> Option<String> {
    let registry = registry().lock().expect("browser registry lock poisoned");
    registry
        .get(surface_id)
        .and_then(|runtime| runtime.active_frame_selector.clone())
}

fn web_view_for(surface_id: &str) -> Option<usize> {
    let registry = registry().lock().expect("browser registry lock poisoned");
    registry.get(surface_id).map(|record| record.web_view)
}

fn install_signal_handlers(surface_id: &str, web_view: *mut c_void) {
    connect_signal(
        web_view,
        "script-dialog",
        script_dialog_cb as *const () as *mut c_void,
        surface_id.to_string(),
    );
    connect_signal(
        web_view,
        "load-failed",
        load_failed_cb as *const () as *mut c_void,
        surface_id.to_string(),
    );
    connect_signal(
        web_view,
        "web-process-terminated",
        web_process_terminated_cb as *const () as *mut c_void,
        surface_id.to_string(),
    );
    unsafe {
        let session = webkit_web_view_get_network_session(web_view.cast::<gtk::ffi::GtkWidget>());
        if !session.is_null() {
            connect_signal(
                session,
                "download-started",
                download_started_cb as *const () as *mut c_void,
                surface_id.to_string(),
            );
        }
    }
}

fn connect_signal(
    instance: *mut c_void,
    signal: &str,
    callback: *mut c_void,
    surface_id: String,
) {
    let signal = CString::new(signal).expect("static signal name");
    let data = Box::into_raw(Box::new(surface_id)).cast();
    unsafe {
        g_signal_connect_data(instance, signal.as_ptr(), callback, data, Some(drop_string_box), 0);
    }
}

unsafe extern "C" fn drop_string_box(data: *mut c_void, _closure: *mut c_void) {
    drop(Box::from_raw(data.cast::<String>()));
}

unsafe extern "C" fn script_dialog_cb(
    _web_view: *mut c_void,
    dialog: *mut c_void,
    user_data: *mut c_void,
) -> i32 {
    let surface_id = &*(user_data.cast::<String>());
    let dialog = webkit_script_dialog_ref(dialog);
    let kind = dialog_kind(webkit_script_dialog_get_dialog_type(dialog));
    let message = cstr_to_string(webkit_script_dialog_get_message(dialog));
    let default_text = if kind == "prompt" {
        cstr_to_string(webkit_script_dialog_prompt_get_default_text(dialog))
    } else {
        String::new()
    };
    let id = next_dialog_id();
    let info = DialogInfo {
        id,
        kind,
        message,
        default_text,
    };
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    if let Some(runtime) = registry.get_mut(surface_id) {
        runtime.dialogs.push(DialogRecord {
            info,
            dialog: dialog as usize,
        });
        trim_vec(&mut runtime.dialogs, 20);
    }
    1
}

unsafe extern "C" fn load_failed_cb(
    _web_view: *mut c_void,
    _load_event: i32,
    failing_uri: *const c_char,
    error: *mut GError,
    user_data: *mut c_void,
) -> i32 {
    let surface_id = &*(user_data.cast::<String>());
    push_error(
        surface_id,
        BrowserEvent {
            level: "error".to_string(),
            text: format!(
                "{}: {}",
                cstr_to_string(failing_uri),
                g_error_message(error, "load failed")
            ),
            source: "load".to_string(),
            line: None,
        },
    );
    0
}

unsafe extern "C" fn web_process_terminated_cb(
    _web_view: *mut c_void,
    reason: i32,
    user_data: *mut c_void,
) {
    let surface_id = &*(user_data.cast::<String>());
    push_error(
        surface_id,
        BrowserEvent {
            level: "error".to_string(),
            text: format!("web process terminated: {reason}"),
            source: "web-process".to_string(),
            line: None,
        },
    );
}

unsafe extern "C" fn download_started_cb(
    _session: *mut c_void,
    download: *mut c_void,
    user_data: *mut c_void,
) {
    let surface_id = (&*(user_data.cast::<String>())).clone();
    let id = next_download_id();
    let url = {
        let request = webkit_download_get_request(download);
        if request.is_null() {
            String::new()
        } else {
            cstr_to_string(webkit_uri_request_get_uri(request))
        }
    };
    let path = download_path(&url, &id);
    let destination_uri = CString::new(format!("file://{}", path.display()))
        .unwrap_or_else(|_| CString::new("file:///tmp/cmux-download").unwrap());
    webkit_download_set_destination(download, destination_uri.as_ptr());
    {
        let mut registry = registry().lock().expect("browser registry lock poisoned");
        if let Some(runtime) = registry.get_mut(&surface_id) {
            runtime.downloads.push(DownloadRecord {
                info: DownloadInfo {
                    id: id.clone(),
                    url,
                    path: path.display().to_string(),
                    status: "started".to_string(),
                    received_bytes: 0,
                    error: None,
                },
            });
            trim_vec(&mut runtime.downloads, 50);
        }
    }
    connect_download_signal(
        download,
        "created-destination",
        download_created_destination_cb as *const () as *mut c_void,
        surface_id.clone(),
        id.clone(),
    );
    connect_download_signal(
        download,
        "received-data",
        download_received_data_cb as *const () as *mut c_void,
        surface_id.clone(),
        id.clone(),
    );
    connect_download_signal(
        download,
        "failed",
        download_failed_cb as *const () as *mut c_void,
        surface_id.clone(),
        id.clone(),
    );
    connect_download_signal(
        download,
        "finished",
        download_finished_cb as *const () as *mut c_void,
        surface_id,
        id,
    );
}

fn connect_download_signal(
    instance: *mut c_void,
    signal: &str,
    callback: *mut c_void,
    surface_id: String,
    download_id: String,
) {
    let signal = CString::new(signal).expect("static signal name");
    let data = Box::into_raw(Box::new(DownloadSignalData {
        surface_id,
        download_id,
    }))
    .cast();
    unsafe {
        g_signal_connect_data(instance, signal.as_ptr(), callback, data, Some(drop_download_signal_data), 0);
    }
}

struct DownloadSignalData {
    surface_id: String,
    download_id: String,
}

unsafe extern "C" fn drop_download_signal_data(data: *mut c_void, _closure: *mut c_void) {
    drop(Box::from_raw(data.cast::<DownloadSignalData>()));
}

unsafe extern "C" fn download_created_destination_cb(
    download: *mut c_void,
    destination: *const c_char,
    user_data: *mut c_void,
) {
    let data = &*(user_data.cast::<DownloadSignalData>());
    update_download(&data.surface_id, &data.download_id, |record| {
        record.info.path = cstr_to_string(destination).trim_start_matches("file://").to_string();
        record.info.status = "created".to_string();
        record.info.received_bytes = webkit_download_get_received_data_length(download);
    });
}

unsafe extern "C" fn download_received_data_cb(
    download: *mut c_void,
    _length: u64,
    user_data: *mut c_void,
) {
    let data = &*(user_data.cast::<DownloadSignalData>());
    update_download(&data.surface_id, &data.download_id, |record| {
        record.info.status = "receiving".to_string();
        record.info.received_bytes = webkit_download_get_received_data_length(download);
    });
}

unsafe extern "C" fn download_failed_cb(
    download: *mut c_void,
    error: *mut GError,
    user_data: *mut c_void,
) {
    let data = &*(user_data.cast::<DownloadSignalData>());
    update_download(&data.surface_id, &data.download_id, |record| {
        record.info.status = "failed".to_string();
        record.info.received_bytes = webkit_download_get_received_data_length(download);
        record.info.error = Some(g_error_message(error, "download failed"));
    });
}

unsafe extern "C" fn download_finished_cb(download: *mut c_void, user_data: *mut c_void) {
    let data = &*(user_data.cast::<DownloadSignalData>());
    update_download(&data.surface_id, &data.download_id, |record| {
        if record.info.status != "failed" {
            record.info.status = "finished".to_string();
        }
        record.info.received_bytes = webkit_download_get_received_data_length(download);
        let destination = cstr_to_string(webkit_download_get_destination(download));
        if !destination.is_empty() {
            record.info.path = destination.trim_start_matches("file://").to_string();
        }
    });
}

fn update_download(
    surface_id: &str,
    download_id: &str,
    update: impl FnOnce(&mut DownloadRecord),
) {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    if let Some(download) = registry
        .get_mut(surface_id)
        .and_then(|runtime| runtime.downloads.iter_mut().find(|download| download.info.id == download_id))
    {
        update(download);
    }
}

fn push_error(surface_id: &str, event: BrowserEvent) {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    if let Some(runtime) = registry.get_mut(surface_id) {
        runtime.errors.push(event);
        trim_vec(&mut runtime.errors, MAX_RUNTIME_EVENTS);
    }
}

fn push_console(surface_id: &str, event: BrowserEvent) {
    let mut registry = registry().lock().expect("browser registry lock poisoned");
    if let Some(runtime) = registry.get_mut(surface_id) {
        runtime.console.push(event);
        trim_vec(&mut runtime.console, MAX_RUNTIME_EVENTS);
    }
}

fn trim_vec<T>(items: &mut Vec<T>, max: usize) {
    if items.len() > max {
        let excess = items.len() - max;
        items.drain(..excess);
    }
}

fn install_observers_later(surface_id: &str) {
    let surface_id = surface_id.to_string();
    gtk::glib::timeout_add_local_once(Duration::from_millis(250), move || {
        install_observers(&surface_id);
    });
}

fn install_observers(surface_id: &str) {
    let _ = run_statement_json(surface_id, OBSERVER_SCRIPT, Duration::from_secs(2));
}

fn refresh_script_events(surface_id: &str) {
    let Some(Ok(text)) = evaluate_expression_json(
        surface_id,
        "window.__cmuxGetEvents ? window.__cmuxGetEvents() : { console: [], errors: [] }",
        Duration::from_secs(2),
    ) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    for event in value
        .get("console")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_console(surface_id, event_from_value(event, "console"));
    }
    for event in value
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_error(surface_id, event_from_value(event, "javascript"));
    }
}

fn event_from_value(value: &Value, source: &str) -> BrowserEvent {
    BrowserEvent {
        level: value
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_string(),
        text: value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or(source)
            .to_string(),
        line: value.get("line").and_then(Value::as_u64).map(|line| line as u32),
    }
}

const OBSERVER_SCRIPT: &str = r#"
if (!window.__cmuxObserversInstalled) {
  window.__cmuxObserversInstalled = true;
  window.__cmuxConsoleEvents = [];
  window.__cmuxErrorEvents = [];
  const trim = (items) => { if (items.length > 200) items.splice(0, items.length - 200); };
  const stringify = (value) => {
    try {
      if (typeof value === 'string') return value;
      if (value instanceof Error) return value.stack || value.message || String(value);
      return JSON.stringify(value);
    } catch (_) {
      return String(value);
    }
  };
  ['log', 'info', 'warn', 'error', 'debug'].forEach((level) => {
    const original = console[level]?.bind(console);
    console[level] = (...args) => {
      window.__cmuxConsoleEvents.push({ level, text: args.map(stringify).join(' '), source: 'console' });
      trim(window.__cmuxConsoleEvents);
      if (original) original(...args);
    };
  });
  window.addEventListener('error', (event) => {
    window.__cmuxErrorEvents.push({
      level: 'error',
      text: event.message || String(event.error || ''),
      source: event.filename || 'javascript',
      line: event.lineno || null
    });
    trim(window.__cmuxErrorEvents);
  });
  window.addEventListener('unhandledrejection', (event) => {
    window.__cmuxErrorEvents.push({
      level: 'error',
      text: stringify(event.reason),
      source: 'unhandledrejection',
      line: null
    });
    trim(window.__cmuxErrorEvents);
  });
  window.__cmuxGetEvents = () => {
    const payload = { console: window.__cmuxConsoleEvents || [], errors: window.__cmuxErrorEvents || [] };
    window.__cmuxConsoleEvents = [];
    window.__cmuxErrorEvents = [];
    return payload;
  };
}
return true;
"#;

fn next_dialog_id() -> String {
    static NEXT: OnceLock<Mutex<u64>> = OnceLock::new();
    let mut next = NEXT.get_or_init(|| Mutex::new(1)).lock().expect("dialog id lock poisoned");
    let id = *next;
    *next += 1;
    format!("dialog-{id}")
}

fn next_download_id() -> String {
    static NEXT: OnceLock<Mutex<u64>> = OnceLock::new();
    let mut next = NEXT.get_or_init(|| Mutex::new(1)).lock().expect("download id lock poisoned");
    let id = *next;
    *next += 1;
    format!("download-{id}")
}

fn dialog_kind(kind: i32) -> String {
    match kind {
        0 => "alert",
        1 => "confirm",
        2 => "prompt",
        3 => "before_unload_confirm",
        _ => "unknown",
    }
    .to_string()
}

fn download_path(url: &str, id: &str) -> PathBuf {
    let dir = std::env::var_os("CMUX_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(|path| PathBuf::from(path).join("cmux").join("downloads"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|path| PathBuf::from(path).join(".cache").join("cmux").join("downloads"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp").join("cmux-downloads"));
    let _ = std::fs::create_dir_all(&dir);
    let name = url
        .rsplit('/')
        .next()
        .map(|value| value.split('?').next().unwrap_or(value))
        .filter(|value| !value.is_empty())
        .unwrap_or("download.bin");
    let safe_name: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{id}-{safe_name}"))
}

unsafe fn cstr_to_string(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        CStr::from_ptr(value).to_string_lossy().into_owned()
    }
}

unsafe fn g_error_message(error: *mut GError, fallback: &str) -> String {
    if error.is_null() || (*error).message.is_null() {
        fallback.to_string()
    } else {
        CStr::from_ptr((*error).message).to_string_lossy().into_owned()
    }
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(b0 >> 2) as usize] as char);
        output.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}
