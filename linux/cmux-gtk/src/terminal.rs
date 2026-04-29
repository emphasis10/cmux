use crate::settings::{self, TerminalBackendPreference};
use gtk::gdk;
use gtk::glib;
use gtk::glib::translate::from_glib_none;
use gtk::prelude::*;
use serde::Serialize;
use std::env;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const BACKSPACE_BYTES: &[u8] = b"\x7f";
const MAX_CAPTURE_BYTES: usize = 256 * 1024;

pub fn new_terminal_panel(
    surface_id: &str,
    initial_command: Option<&str>,
    working_directory: Option<&str>,
) -> gtk::Widget {
    match settings::get().terminal_backend {
        TerminalBackendPreference::Pty => {
            new_text_pty_terminal_panel(surface_id, initial_command, working_directory)
        }
        TerminalBackendPreference::Vte => new_vte_terminal_panel().unwrap_or_else(|| {
            new_text_pty_terminal_panel(surface_id, initial_command, working_directory)
        }),
        TerminalBackendPreference::Ghostty => new_ghostty_terminal_panel(
            surface_id,
            initial_command,
            working_directory,
        )
            .unwrap_or_else(|| {
                new_text_pty_terminal_panel(surface_id, initial_command, working_directory)
            }),
        TerminalBackendPreference::Auto => new_ghostty_terminal_panel(
            surface_id,
            initial_command,
            working_directory,
        )
            .unwrap_or_else(|| {
                new_text_pty_terminal_panel(surface_id, initial_command, working_directory)
            }),
    }
}

fn new_ghostty_terminal_panel(
    surface_id: &str,
    initial_command: Option<&str>,
    working_directory: Option<&str>,
) -> Option<gtk::Widget> {
    let (widget, handle) =
        crate::ghostty_backend::create_surface(surface_id, initial_command, working_directory)?;
    register_ghostty_terminal(surface_id, handle);
    {
        let widget = widget.clone();
        glib::idle_add_local_once(move || {
            widget.grab_focus();
        });
    }
    Some(widget)
}

fn new_vte_terminal_panel() -> Option<gtk::Widget> {
    let api = VteApi::load()?;
    let widget: gtk::Widget = unsafe {
        let raw = (api.terminal_new)();
        if raw.is_null() {
            return None;
        }
        (api.set_scrollback_lines)(raw.cast(), 10_000);
        (api.set_cursor_blink_mode)(raw.cast(), VTE_CURSOR_BLINK_SYSTEM);
        (api.set_mouse_autohide)(raw.cast(), 1);
        spawn_vte_shell(api, raw.cast());
        from_glib_none(raw)
    };
    widget.set_focusable(true);
    widget.set_hexpand(true);
    widget.set_vexpand(true);
    {
        let widget = widget.clone();
        glib::idle_add_local_once(move || {
            widget.grab_focus();
        });
    }
    Some(widget.upcast())
}

fn spawn_vte_shell(api: &VteApi, terminal: *mut c_void) {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let shell_c = CString::new(shell.clone()).unwrap_or_else(|_| CString::new("/bin/bash").unwrap());
    let working_directory = env::current_dir()
        .ok()
        .and_then(|path| CString::new(path.to_string_lossy().as_bytes()).ok());
    let mut argv = vec![
        shell_c.as_ptr() as *mut c_char,
        std::ptr::null_mut::<c_char>(),
    ];

    unsafe {
        (api.spawn_async)(
            terminal,
            VTE_PTY_DEFAULT,
            working_directory
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
            argv.as_mut_ptr(),
            std::ptr::null_mut(),
            G_SPAWN_DEFAULT,
            None,
            std::ptr::null_mut(),
            None,
            -1,
            std::ptr::null_mut(),
            None,
            std::ptr::null_mut(),
        );
    }

    drop(shell_c);
    drop(working_directory);
}

fn new_text_pty_terminal_panel(
    surface_id: &str,
    initial_command: Option<&str>,
    working_directory: Option<&str>,
) -> gtk::Widget {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    let writer = match PtySession::spawn(
        sender,
        initial_command.map(ToString::to_string),
        working_directory.map(PathBuf::from),
    ) {
        Ok(writer) => Some(Arc::new(writer)),
        Err(error) => {
            let label = gtk::Label::new(Some(&format!("Unable to start shell: {error}")));
            label.set_margin_top(24);
            label.set_margin_bottom(24);
            label.set_margin_start(24);
            label.set_margin_end(24);
            return label.upcast();
        }
    };

    let surface_id = surface_id.to_string();
    if let Some(writer) = writer.as_ref() {
        register_terminal(&surface_id, Arc::clone(writer));
    }

    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    buffer.set_text("");

    let text_view = gtk::TextView::with_buffer(&buffer);
    text_view.add_css_class("monospace");
    text_view.set_editable(false);
    text_view.set_cursor_visible(true);
    text_view.set_focusable(true);
    text_view.set_hexpand(true);
    text_view.set_vexpand(true);
    text_view.set_wrap_mode(gtk::WrapMode::Char);
    {
        let text_view = text_view.clone();
        glib::idle_add_local_once(move || {
            text_view.grab_focus();
        });
    }

    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&text_view)
        .build();

    install_output_pump(&surface_id, &text_view, &buffer, receiver);
    if let Some(writer) = writer {
        install_keyboard_handler(&text_view, writer);
    }

    scroll.upcast()
}

const VTE_PTY_DEFAULT: i32 = 0;
const G_SPAWN_DEFAULT: i32 = 0;
const VTE_CURSOR_BLINK_SYSTEM: i32 = 0;

type VteTerminalNew = unsafe extern "C" fn() -> *mut gtk::ffi::GtkWidget;
type VteTerminalSetScrollbackLines = unsafe extern "C" fn(*mut c_void, i64);
type VteTerminalSetCursorBlinkMode = unsafe extern "C" fn(*mut c_void, i32);
type VteTerminalSetMouseAutohide = unsafe extern "C" fn(*mut c_void, i32);
type VteTerminalSpawnAsync = unsafe extern "C" fn(
    terminal: *mut c_void,
    pty_flags: i32,
    working_directory: *const c_char,
    argv: *mut *mut c_char,
    envv: *mut *mut c_char,
    spawn_flags: i32,
    child_setup: Option<unsafe extern "C" fn(*mut c_void)>,
    child_setup_data: *mut c_void,
    child_setup_data_destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    timeout: i32,
    cancellable: *mut c_void,
    callback: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
    user_data: *mut c_void,
);

#[derive(Clone, Copy)]
struct VteApi {
    terminal_new: VteTerminalNew,
    set_scrollback_lines: VteTerminalSetScrollbackLines,
    set_cursor_blink_mode: VteTerminalSetCursorBlinkMode,
    set_mouse_autohide: VteTerminalSetMouseAutohide,
    spawn_async: VteTerminalSpawnAsync,
}

impl VteApi {
    fn load() -> Option<&'static Self> {
        static API: OnceLock<Option<VteApi>> = OnceLock::new();
        API.get_or_init(|| unsafe { VteApi::load_dynamic() })
            .as_ref()
    }

    unsafe fn load_dynamic() -> Option<Self> {
        let library = CStr::from_bytes_with_nul(b"libvte-2.91-gtk4.so.0\0").ok()?;
        let handle = libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            return None;
        }
        Some(Self {
            terminal_new: load_symbol(handle, b"vte_terminal_new\0")?,
            set_scrollback_lines: load_symbol(handle, b"vte_terminal_set_scrollback_lines\0")?,
            set_cursor_blink_mode: load_symbol(handle, b"vte_terminal_set_cursor_blink_mode\0")?,
            set_mouse_autohide: load_symbol(handle, b"vte_terminal_set_mouse_autohide\0")?,
            spawn_async: load_symbol(handle, b"vte_terminal_spawn_async\0")?,
        })
    }
}

unsafe fn load_symbol<T: Copy>(handle: *mut c_void, name: &[u8]) -> Option<T> {
    let name = CStr::from_bytes_with_nul(name).ok()?;
    let symbol = libc::dlsym(handle, name.as_ptr());
    if symbol.is_null() {
        return None;
    }
    Some(std::mem::transmute_copy(&symbol))
}

fn install_output_pump(
    surface_id: &str,
    text_view: &gtk::TextView,
    buffer: &gtk::TextBuffer,
    receiver: mpsc::Receiver<Vec<u8>>,
) {
    let surface_id = surface_id.to_string();
    let text_view = text_view.clone();
    let buffer = buffer.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let mut appended = false;
        for chunk in receiver.try_iter().take(64) {
            let text = terminal_text_from_bytes(&chunk);
            if !text.is_empty() {
                append_capture(&surface_id, &text);
                appended |= apply_terminal_text(&buffer, &text);
            }
        }
        if appended {
            let mut end = buffer.end_iter();
            buffer.place_cursor(&end);
            text_view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
        }
        glib::ControlFlow::Continue
    });
}

enum TerminalIo {
    Pty(Arc<PtyWriter>),
    Ghostty(crate::ghostty_backend::GhosttySurfaceHandle),
}

struct TerminalRegistration {
    io: TerminalIo,
    capture: String,
    backend: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct TerminalStatus {
    pub backend: &'static str,
    pub live: bool,
    pub pid: i32,
    pub exited: bool,
    pub captured_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ghostty: Option<crate::ghostty_backend::GhosttyGtkSurfaceHealth>,
}

fn registry() -> &'static Mutex<std::collections::BTreeMap<String, TerminalRegistration>> {
    static REGISTRY: OnceLock<Mutex<std::collections::BTreeMap<String, TerminalRegistration>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn register_terminal(surface_id: &str, writer: Arc<PtyWriter>) {
    let mut registry = registry().lock().expect("terminal registry lock poisoned");
    registry.insert(
        surface_id.to_string(),
        TerminalRegistration {
            io: TerminalIo::Pty(writer),
            capture: String::new(),
            backend: "pty",
        },
    );
}

fn register_ghostty_terminal(
    surface_id: &str,
    handle: crate::ghostty_backend::GhosttySurfaceHandle,
) {
    let mut registry = registry().lock().expect("terminal registry lock poisoned");
    registry.insert(
        surface_id.to_string(),
        TerminalRegistration {
            io: TerminalIo::Ghostty(handle),
            capture: String::new(),
            backend: "ghostty",
        },
    );
}

pub fn unregister(surface_id: &str) -> bool {
    let mut registry = registry().lock().expect("terminal registry lock poisoned");
    registry.remove(surface_id).is_some()
}

pub fn send_text(surface_id: &str, text: &str) -> bool {
    let registry = registry().lock().expect("terminal registry lock poisoned");
    let Some(registration) = registry.get(surface_id) else {
        return false;
    };
    match &registration.io {
        TerminalIo::Pty(writer) => {
            writer.write(text.as_bytes());
            true
        }
        TerminalIo::Ghostty(handle) => handle.send_text(text),
    }
}

pub fn send_key(surface_id: &str, key: &str) -> bool {
    let Some(bytes) = bytes_for_socket_key(key) else {
        return false;
    };
    let registry = registry().lock().expect("terminal registry lock poisoned");
    let Some(registration) = registry.get(surface_id) else {
        return false;
    };
    match &registration.io {
        TerminalIo::Pty(writer) => {
            writer.write(&bytes);
            true
        }
        TerminalIo::Ghostty(handle) => handle.send_bytes(&bytes),
    }
}

pub fn read_text(surface_id: &str) -> Option<String> {
    let registry = registry().lock().expect("terminal registry lock poisoned");
    registry.get(surface_id).map(|registration| match &registration.io {
        TerminalIo::Pty(_) => registration.capture.clone(),
        TerminalIo::Ghostty(handle) => handle.read_text().unwrap_or_default(),
    })
}

pub fn clear_text(surface_id: &str) -> bool {
    let mut registry = registry().lock().expect("terminal registry lock poisoned");
    let Some(registration) = registry.get_mut(surface_id) else {
        return false;
    };
    registration.capture.clear();
    true
}

pub fn focus(surface_id: &str, focused: bool) -> bool {
    let registry = registry().lock().expect("terminal registry lock poisoned");
    let Some(registration) = registry.get(surface_id) else {
        return false;
    };
    match &registration.io {
        TerminalIo::Pty(_) => true,
        TerminalIo::Ghostty(handle) => {
            handle.focus(focused);
            true
        }
    }
}

pub fn status(surface_id: &str) -> Option<TerminalStatus> {
    let registry = registry().lock().expect("terminal registry lock poisoned");
    let registration = registry.get(surface_id)?;
    let (live, pid, exited, ghostty) = match &registration.io {
        TerminalIo::Pty(writer) => {
            let exited = writer.exited.load(Ordering::Relaxed);
            (!exited, writer.pid, exited, None)
        }
        TerminalIo::Ghostty(handle) => {
            let health = handle.health();
            (health.alive && !health.child_exited, 0, health.child_exited, Some(health))
        }
    };
    Some(TerminalStatus {
        backend: registration.backend,
        live,
        pid,
        exited,
        captured_bytes: registration.capture.len(),
        ghostty,
    })
}

fn append_capture(surface_id: &str, text: &str) {
    let mut registry = registry().lock().expect("terminal registry lock poisoned");
    let Some(registration) = registry.get_mut(surface_id) else {
        return;
    };
    registration.capture.push_str(text);
    if registration.capture.len() > MAX_CAPTURE_BYTES {
        let excess = registration.capture.len() - MAX_CAPTURE_BYTES;
        registration.capture.drain(..excess);
    }
}

fn install_keyboard_handler(text_view: &gtk::TextView, writer: Arc<PtyWriter>) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, state| {
        if let Some(bytes) = bytes_for_key(key, state) {
            writer.write(&bytes);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    text_view.add_controller(controller);
}

struct PtySession;

impl PtySession {
    fn spawn(
        sender: mpsc::Sender<Vec<u8>>,
        initial_command: Option<String>,
        working_directory: Option<PathBuf>,
    ) -> io::Result<PtyWriter> {
        let mut master: libc::c_int = -1;
        let winsize = libc::winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null(),
                &winsize,
            )
        };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            child_exec_shell(initial_command, working_directory);
        }

        let exited = Arc::new(AtomicBool::new(false));
        std::thread::Builder::new()
            .name("cmux-linux-pty-reader".to_string())
            .spawn({
                let exited = Arc::clone(&exited);
                move || read_pty_loop(master, sender, exited)
            })
            .map_err(io::Error::other)?;

        Ok(PtyWriter {
            fd: master,
            pid,
            exited,
        })
    }
}

struct PtyWriter {
    fd: RawFd,
    pid: i32,
    exited: Arc<AtomicBool>,
}

impl PtyWriter {
    fn write(&self, bytes: &[u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            let result = unsafe {
                libc::write(
                    self.fd,
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                )
            };
            if result <= 0 {
                break;
            }
            offset += result as usize;
        }
    }
}

unsafe impl Send for PtyWriter {}
unsafe impl Sync for PtyWriter {}

impl Drop for PtyWriter {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Relaxed);
        unsafe {
            let _ = libc::kill(self.pid, libc::SIGHUP);
            let _ = libc::close(self.fd);
        }
    }
}

fn child_exec_shell(initial_command: Option<String>, working_directory: Option<PathBuf>) -> ! {
    configure_child_terminal();
    if let Some(working_directory) = working_directory {
        if let Ok(cwd) = CString::new(working_directory.to_string_lossy().as_bytes()) {
            unsafe {
                let _ = libc::chdir(cwd.as_ptr());
            }
        }
    }

    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let shell_c = CString::new(shell.clone()).unwrap_or_else(|_| CString::new("/bin/bash").unwrap());
    let arg0_name = shell
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("sh");
    let arg0 = CString::new(format!("-{arg0_name}")).unwrap();
    let term_key = CString::new("TERM").unwrap();
    let term_value = CString::new("xterm-256color").unwrap();
    let color_key = CString::new("COLORTERM").unwrap();
    let color_value = CString::new("truecolor").unwrap();
    let term_program_key = CString::new("TERM_PROGRAM").unwrap();
    let term_program_value = CString::new("cmux").unwrap();
    let cmux_present_key = CString::new("CMUX").unwrap();
    let cmux_present_value = CString::new("1").unwrap();
    let cmux_key = CString::new("CMUX_LINUX_PTY").unwrap();
    let cmux_value = CString::new("1").unwrap();

    unsafe {
        libc::setenv(term_key.as_ptr(), term_value.as_ptr(), 1);
        libc::setenv(color_key.as_ptr(), color_value.as_ptr(), 1);
        libc::setenv(term_program_key.as_ptr(), term_program_value.as_ptr(), 1);
        libc::setenv(cmux_present_key.as_ptr(), cmux_present_value.as_ptr(), 1);
        libc::setenv(cmux_key.as_ptr(), cmux_value.as_ptr(), 1);
        if let Some(initial_command) = initial_command {
            let login_flag = CString::new("-lc").unwrap();
            if let Ok(command) = CString::new(initial_command) {
                libc::execlp(
                    shell_c.as_ptr(),
                    arg0.as_ptr(),
                    login_flag.as_ptr(),
                    command.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
            }
        } else {
            libc::execlp(
                shell_c.as_ptr(),
                arg0.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
        }
        libc::_exit(127);
    }
}

fn configure_child_terminal() {
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut termios) == 0 {
            termios.c_cc[libc::VERASE] = 0x7f;
            let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &termios);
        }
    }
}

fn read_pty_loop(master: RawFd, sender: mpsc::Sender<Vec<u8>>, exited: Arc<AtomicBool>) {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count <= 0 {
            exited.store(true, Ordering::Relaxed);
            let _ = sender.send(b"\n[process exited]\n".to_vec());
            break;
        }
        if sender.send(buffer[..count as usize].to_vec()).is_err() {
            break;
        }
    }
}

fn apply_terminal_text(buffer: &gtk::TextBuffer, text: &str) -> bool {
    if text.is_empty() {
        return false;
    }

    let mut changed = false;
    let mut pending = String::new();
    for ch in text.chars() {
        match ch {
            '\u{8}' | '\u{7f}' => {
                flush_pending(buffer, &mut pending);
                delete_previous_char(buffer);
                changed = true;
            }
            '\r' => {
                flush_pending(buffer, &mut pending);
            }
            _ => {
                pending.push(ch);
                changed = true;
            }
        }
    }
    flush_pending(buffer, &mut pending);
    changed
}

fn flush_pending(buffer: &gtk::TextBuffer, pending: &mut String) {
    if pending.is_empty() {
        return;
    }
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, pending);
    pending.clear();
}

fn delete_previous_char(buffer: &gtk::TextBuffer) {
    let mut start = buffer.end_iter();
    if !start.backward_char() {
        return;
    }
    let mut end = buffer.end_iter();
    buffer.delete(&mut start, &mut end);
}

fn terminal_text_from_bytes(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    strip_ansi_controls(&decoded)
        .chars()
        .filter(|ch| {
            *ch == '\n'
                || *ch == '\t'
                || *ch == '\r'
                || *ch == '\u{8}'
                || *ch == '\u{7f}'
                || !ch.is_control()
        })
        .collect()
}

fn strip_ansi_controls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek().copied() == Some('\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    output
}

fn bytes_for_key(key: gdk::Key, state: gdk::ModifierType) -> Option<Vec<u8>> {
    let name = key.name().map(|name| name.to_string()).unwrap_or_default();
    match name.as_str() {
        "Return" | "KP_Enter" => return Some(b"\r".to_vec()),
        "BackSpace" | "Backspace" => return Some(BACKSPACE_BYTES.to_vec()),
        "Tab" => return Some(b"\t".to_vec()),
        "Escape" => return Some(vec![0x1b]),
        "Left" => return Some(b"\x1b[D".to_vec()),
        "Right" => return Some(b"\x1b[C".to_vec()),
        "Up" => return Some(b"\x1b[A".to_vec()),
        "Down" => return Some(b"\x1b[B".to_vec()),
        "Home" => return Some(b"\x1b[H".to_vec()),
        "End" => return Some(b"\x1b[F".to_vec()),
        "Delete" => return Some(b"\x1b[3~".to_vec()),
        "Page_Up" => return Some(b"\x1b[5~".to_vec()),
        "Page_Down" => return Some(b"\x1b[6~".to_vec()),
        _ => {}
    }

    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        if let Some(ch) = key.to_unicode().map(|ch| ch.to_ascii_lowercase()) {
            if ch.is_ascii_lowercase() {
                return Some(vec![(ch as u8) - b'a' + 1]);
            }
        }
        return None;
    }

    key.to_unicode().map(|ch| ch.to_string().into_bytes())
}

fn bytes_for_socket_key(key: &str) -> Option<Vec<u8>> {
    let normalized = key.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "enter" | "return" => Some(b"\r".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "escape" | "esc" => Some(vec![0x1b]),
        "backspace" => Some(BACKSPACE_BYTES.to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" | "page-up" => Some(b"\x1b[5~".to_vec()),
        "pagedown" | "page-down" => Some(b"\x1b[6~".to_vec()),
        value if value.starts_with("ctrl-") && value.len() == 6 => {
            let ch = value.as_bytes()[5];
            ch.is_ascii_lowercase().then_some(vec![ch - b'a' + 1])
        }
        value if value.len() == 1 => Some(value.as_bytes().to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_escape_sequences() {
        assert_eq!(strip_ansi_controls("a\u{1b}[31mb\u{1b}[0mc"), "abc");
    }
}
