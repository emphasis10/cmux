pub fn tr(key: &str) -> &'static str {
    match key {
        "app.title" => "cmux",
        "sidebar.workspaces" => "Workspaces",
        "sidebar.newWorkspace" => "New workspace",
        "sidebar.socket" => "Socket",
        "sidebar.notifications.none" => "No notifications",
        "sidebar.notifications.one" => "1 notification",
        "sidebar.notifications.clear" => "Clear notifications",
        "workspace.empty" => "No workspace",
        "pane.empty" => "Empty pane",
        "surface.markdown" => "Markdown",
        _ => "cmux",
    }
}
