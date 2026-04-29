mod browser;
mod i18n;
mod ghostty_backend;
mod settings;
mod socket;
mod terminal;

use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

const APP_ID: &str = "com.cmuxterm.cmux";
const DEFAULT_SIDEBAR_WIDTH: i32 = 220;
const MIN_SIDEBAR_WIDTH: i32 = 160;
const MAX_SIDEBAR_WIDTH: i32 = 420;

fn main() -> glib::ExitCode {
    let application_id = application_id();
    let app = adw::Application::builder()
        .application_id(&application_id)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|_| {
        adw::init().expect("failed to initialize libadwaita");
    });
    app.connect_activate(build_window);
    app.connect_command_line(|app, _command_line| {
        app.activate();
        glib::ExitCode::SUCCESS
    });

    app.run()
}

fn application_id() -> String {
    let Some(raw_suffix) = std::env::var("CMUX_APP_ID_SUFFIX")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return APP_ID.to_string();
    };
    let suffix: String = raw_suffix
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{APP_ID}.{suffix}")
}

fn workspace_sidebar_width() -> i32 {
    std::env::var("CMUX_WORKSPACE_SIDEBAR_WIDTH")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or_else(|| {
            let width = settings::get().sidebar_width;
            if width > 0 {
                width
            } else {
                DEFAULT_SIDEBAR_WIDTH
            }
        })
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

fn build_window(app: &adw::Application) {
    let socket_path = socket::default_socket_path();
    let state = socket::shared_state();
    if let Err(error) = socket::start_background_server(socket_path.clone(), state.clone()) {
        eprintln!("cmux linux socket disabled: {error}");
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(i18n::tr("app.title"))
        .default_width(1280)
        .default_height(820)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(i18n::tr("app.title")))));
    toolbar.add_top_bar(&header);

    let root = gtk::Paned::new(gtk::Orientation::Horizontal);
    root.set_position(workspace_sidebar_width());
    root.set_resize_start_child(true);
    root.set_shrink_start_child(false);
    root.set_resize_end_child(true);
    root.set_shrink_end_child(false);
    root.set_start_child(Some(&build_sidebar(&socket_path, &state)));
    root.set_end_child(Some(&build_workspace_host(&state)));
    toolbar.set_content(Some(&root));

    window.set_content(Some(&toolbar));
    install_desktop_notification_pump(app, &state);
    window.present();
}

fn build_sidebar(socket_path: &Path, state: &socket::AppState) -> gtk::Widget {
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_width_request(MIN_SIDEBAR_WIDTH);
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_margin_top(16);
    sidebar.set_margin_bottom(16);
    sidebar.set_margin_start(16);
    sidebar.set_margin_end(16);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(i18n::tr("sidebar.workspaces")));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let add_workspace = gtk::Button::with_label("+");
    add_workspace.set_tooltip_text(Some(i18n::tr("sidebar.newWorkspace")));
    header.append(&add_workspace);
    sidebar.append(&header);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
    rows.set_vexpand(true);
    sidebar.append(&rows);

    let selected_workspace = Rc::new(RefCell::new(
        state
            .workspace_summaries()
            .first()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_default(),
    ));
    refresh_workspace_rows(&rows, state, &selected_workspace);

    let add_state = state.clone();
    let add_rows = rows.clone();
    let add_selected = Rc::clone(&selected_workspace);
    add_workspace.connect_clicked(move |_| {
        let count = add_state.workspace_summaries().len() + 1;
        let id = add_state.create_workspace_with_title(format!("Workspace {count}"));
        *add_selected.borrow_mut() = id;
        refresh_workspace_rows(&add_rows, &add_state, &add_selected);
    });

    let refresh_state = state.clone();
    let refresh_rows = rows.clone();
    let refresh_selected = Rc::clone(&selected_workspace);
    let last_revision = Rc::new(Cell::new(refresh_state.revision()));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let revision = refresh_state.revision();
        if revision != last_revision.get() {
            last_revision.set(revision);
            refresh_workspace_rows(&refresh_rows, &refresh_state, &refresh_selected);
        }
        glib::ControlFlow::Continue
    });

    let status = gtk::Label::new(Some(&format!(
        "{}\n{}",
        i18n::tr("sidebar.socket"),
        socket_path.display()
    )));
    status.add_css_class("dim-label");
    status.set_wrap(true);
    status.set_xalign(0.0);
    sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    sidebar.append(&status);

    let notifications = gtk::Label::new(Some(&notification_status(state.notification_count())));
    notifications.add_css_class("caption");
    notifications.set_xalign(0.0);
    sidebar.append(&notifications);
    sidebar.append(&build_notification_panel(state));
    let notify_state = state.clone();
    let notify_label = notifications.clone();
    let notify_last_revision = Rc::new(Cell::new(notify_state.revision()));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let revision = notify_state.revision();
        if revision != notify_last_revision.get() {
            notify_last_revision.set(revision);
            notify_label.set_text(&notification_status(notify_state.notification_count()));
        }
        glib::ControlFlow::Continue
    });

    sidebar.upcast()
}

fn notification_status(count: usize) -> String {
    match count {
        0 => i18n::tr("sidebar.notifications.none").to_string(),
        1 => i18n::tr("sidebar.notifications.one").to_string(),
        count => format!("{count} notifications"),
    }
}

fn build_notification_panel(state: &socket::AppState) -> gtk::Widget {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    panel.set_vexpand(false);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    refresh_notification_rows(&rows, state);
    panel.append(&rows);

    let clear = gtk::Button::with_label(i18n::tr("sidebar.notifications.clear"));
    let clear_state = state.clone();
    let clear_rows = rows.clone();
    clear.connect_clicked(move |_| {
        clear_state.clear_notifications_for_ui();
        refresh_notification_rows(&clear_rows, &clear_state);
    });
    panel.append(&clear);

    let refresh_state = state.clone();
    let refresh_rows = rows.clone();
    let last_revision = Rc::new(Cell::new(refresh_state.revision()));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let revision = refresh_state.revision();
        if revision != last_revision.get() {
            last_revision.set(revision);
            refresh_notification_rows(&refresh_rows, &refresh_state);
        }
        glib::ControlFlow::Continue
    });

    panel.upcast()
}

fn refresh_notification_rows(rows: &gtk::Box, state: &socket::AppState) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for notification in state.notification_summaries().into_iter().rev().take(5) {
        let row = gtk::Button::new();
        row.set_halign(gtk::Align::Fill);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(8);
        content.set_margin_end(8);

        let title = gtk::Label::new(Some(&notification.title));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&title);

        if !notification.body.is_empty() {
            let body = gtk::Label::new(Some(&notification.body));
            body.add_css_class("caption");
            body.set_xalign(0.0);
            body.set_wrap(true);
            content.append(&body);
        }
        row.set_child(Some(&content));

        let focus_state = state.clone();
        row.connect_clicked(move |_| {
            if let Some(workspace_id) = notification.workspace_id.as_deref() {
                let _ = focus_state.select_workspace_by_id(workspace_id);
            }
            if let Some(surface_id) = notification.surface_id.as_deref() {
                let _ = focus_state.focus_surface_by_id(surface_id);
            }
        });
        rows.append(&row);
    }
}

fn install_desktop_notification_pump(app: &adw::Application, state: &socket::AppState) {
    if !settings::get().desktop_notifications {
        return;
    }
    let app = app.clone();
    let state = state.clone();
    let last_seen = Rc::new(RefCell::new(String::new()));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        if let Some(notification) = state.latest_notification() {
            if *last_seen.borrow() != notification.id {
                *last_seen.borrow_mut() = notification.id.clone();
                let gio_notification = gio::Notification::new(&notification.title);
                if !notification.body.is_empty() {
                    gio_notification.set_body(Some(&notification.body));
                }
                app.send_notification(Some(&notification.id), &gio_notification);
            }
        }
        glib::ControlFlow::Continue
    });
}

fn refresh_workspace_rows(
    rows: &gtk::Box,
    state: &socket::AppState,
    selected_workspace: &Rc<RefCell<String>>,
) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }

    let summaries = state.workspace_summaries();
    if selected_workspace.borrow().is_empty() {
        if let Some(workspace) = summaries.first() {
            *selected_workspace.borrow_mut() = workspace.id.clone();
        }
    }

    let can_close_workspaces = summaries.len() > 1;
    for workspace in summaries {
        let row_container = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row_container.set_halign(gtk::Align::Fill);

        let row = gtk::Button::new();
        row.set_halign(gtk::Align::Fill);
        row.set_hexpand(true);
        if workspace.selected || *selected_workspace.borrow() == workspace.id {
            row.add_css_class("suggested-action");
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(10);
        content.set_margin_end(10);

        let title = gtk::Label::new(Some(&workspace.title));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&title);

        let detail = gtk::Label::new(Some(&format!(
            "{} panes, {} panels",
            workspace.pane_count, workspace.surface_count
        )));
        detail.add_css_class("caption");
        detail.set_xalign(0.0);
        content.append(&detail);

        row.set_child(Some(&content));

        let row_rows = rows.clone();
        let row_state = state.clone();
        let row_selected = Rc::clone(selected_workspace);
        let row_workspace_id = workspace.id.clone();
        row.connect_clicked(move |_| {
            if let Err(error) = row_state.select_workspace_by_id(&row_workspace_id) {
                eprintln!("cmux linux workspace select failed: {error}");
            }
            *row_selected.borrow_mut() = row_workspace_id.clone();
            refresh_workspace_rows(&row_rows, &row_state, &row_selected);
        });

        row_container.append(&row);

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.set_tooltip_text(Some(if can_close_workspaces {
            i18n::tr("sidebar.closeWorkspace")
        } else {
            i18n::tr("sidebar.closeWorkspaceLast")
        }));
        close.set_valign(gtk::Align::Center);
        close.set_sensitive(can_close_workspaces);
        let close_rows = rows.clone();
        let close_state = state.clone();
        let close_selected = Rc::clone(selected_workspace);
        let close_workspace_id = workspace.id;
        close.connect_clicked(move |_| {
            if let Err(error) = close_state.close_workspace_by_id(&close_workspace_id) {
                eprintln!("cmux linux workspace close failed: {error}");
            }
            let summaries = close_state.workspace_summaries();
            let selected_still_exists = summaries
                .iter()
                .any(|workspace| workspace.id == *close_selected.borrow());
            if !selected_still_exists {
                *close_selected.borrow_mut() = summaries
                    .iter()
                    .find(|workspace| workspace.selected)
                    .or_else(|| summaries.first())
                    .map(|workspace| workspace.id.clone())
                    .unwrap_or_default();
            }
            refresh_workspace_rows(&close_rows, &close_state, &close_selected);
        });
        row_container.append(&close);

        rows.append(&row_container);
    }
}

fn build_workspace_host(state: &socket::AppState) -> gtk::Widget {
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.set_hexpand(true);
    host.set_vexpand(true);
    refresh_workspace_host(&host, state);

    let refresh_state = state.clone();
    let refresh_host = host.clone();
    let last_revision = Rc::new(Cell::new(refresh_state.revision()));
    glib::timeout_add_local(Duration::from_millis(250), move || {
        let revision = refresh_state.revision();
        if revision != last_revision.get() {
            last_revision.set(revision);
            refresh_workspace_host(&refresh_host, &refresh_state);
        }
        glib::ControlFlow::Continue
    });

    host.upcast()
}

fn refresh_workspace_host(host: &gtk::Box, state: &socket::AppState) {
    while let Some(child) = host.first_child() {
        host.remove(&child);
    }
    host.append(&build_workspace(state));
}

fn build_workspace(state: &socket::AppState) -> gtk::Widget {
    let Some(workspace) = state.active_workspace_detail() else {
        let label = gtk::Label::new(Some(i18n::tr("workspace.empty")));
        label.set_hexpand(true);
        label.set_vexpand(true);
        return label.upcast();
    };

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let title = gtk::Label::new(Some(&workspace.title));
    title.add_css_class("title-1");
    title.set_xalign(0.0);
    title.set_tooltip_text(Some(&workspace.id));
    content.append(&title);

    content.append(&build_pane_layout(&workspace.layout, state));

    content.upcast()
}

fn build_pane_layout(layout: &socket::PaneLayoutDetail, state: &socket::AppState) -> gtk::Widget {
    match layout {
        socket::PaneLayoutDetail::Leaf(pane) => build_pane(pane, state),
        socket::PaneLayoutDetail::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let orientation = if direction == "vertical" {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            };
            let paned = gtk::Paned::new(orientation);
            paned.set_hexpand(true);
            paned.set_vexpand(true);
            paned.set_resize_start_child(true);
            paned.set_resize_end_child(true);
            paned.set_shrink_start_child(false);
            paned.set_shrink_end_child(false);
            paned.set_start_child(Some(&build_pane_layout(first, state)));
            paned.set_end_child(Some(&build_pane_layout(second, state)));
            let position = (1000.0 * ratio.clamp(0.1, 0.9)).round() as i32;
            paned.set_position(position);
            paned.upcast()
        }
    }
}

fn build_pane(pane: &socket::PaneDetail, state: &socket::AppState) -> gtk::Widget {
    let frame = gtk::Frame::new(None);
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_tooltip_text(Some(&pane.id));

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tabs.add_css_class("toolbar");
    tabs.set_margin_top(6);
    tabs.set_margin_bottom(6);
    tabs.set_margin_start(6);
    tabs.set_margin_end(6);

    for surface in &pane.surfaces {
        let label = gtk::Label::new(Some(&surface.title));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let button = gtk::Button::new();
        if surface.focused || pane.selected_surface_id.as_deref() == Some(surface.id.as_str()) {
            button.add_css_class("suggested-action");
        }
        let tab_state = state.clone();
        let surface_id = surface.id.clone();
        button.connect_clicked(move |_| {
            if let Err(error) = tab_state.focus_surface_by_id(&surface_id) {
                eprintln!("cmux linux surface focus failed: {error}");
            }
        });
        button.set_child(Some(&label));
        tabs.append(&button);
    }
    box_.append(&tabs);

    let active_surface = pane
        .selected_surface_id
        .as_ref()
        .and_then(|id| pane.surfaces.iter().find(|surface| &surface.id == id))
        .or_else(|| pane.surfaces.first());

    if let Some(surface) = active_surface {
        box_.append(&build_surface(surface));
    } else {
        let label = gtk::Label::new(Some(i18n::tr("pane.empty")));
        label.add_css_class("dim-label");
        label.set_hexpand(true);
        label.set_vexpand(true);
        box_.append(&label);
    }

    frame.set_child(Some(&box_));
    frame.upcast()
}

fn build_surface(surface: &socket::SurfaceDetail) -> gtk::Widget {
    match surface.kind.as_str() {
        "browser" => browser::new_web_view(&surface.id, surface.url.as_deref().unwrap_or("about:blank")),
        "markdown" => {
            let label = gtk::Label::new(Some(&surface.title));
            label.set_hexpand(true);
            label.set_vexpand(true);
            label.upcast()
        }
        _ => terminal::new_terminal_panel(
            &surface.id,
            surface.initial_command.as_deref(),
            surface.working_directory.as_deref(),
        ),
    }
}
