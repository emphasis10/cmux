use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::browser;
use crate::terminal;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

static NEXT_WORKSPACE_ORDINAL: AtomicU64 = AtomicU64::new(1);
static NEXT_SURFACE_ORDINAL: AtomicU64 = AtomicU64::new(1);
static NEXT_PANE_ORDINAL: AtomicU64 = AtomicU64::new(1);
static NEXT_NOTIFICATION_ORDINAL: AtomicU64 = AtomicU64::new(1);
static NEXT_UUID_ORDINAL: AtomicU64 = AtomicU64::new(1);
static SERVER_STARTED: AtomicBool = AtomicBool::new(false);
static SHARED_STATE: OnceLock<AppState> = OnceLock::new();
static WINDOW_ID: OnceLock<String> = OnceLock::new();
const STATIC_SCREENSHOT_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAWklEQVR4nO3PQQ0AIBDAMMC/5+ONAvZoFSzZnZmdA3i6A0gHkA4gHUA6gHQA6QDSAaQDSAeQDiAdQDqAdADpANIBpANIB5AOIB1AOoB0AOkA0gGkA0gHkA4gHUC6AX0mAkGx9HjRAAAAAElFTkSuQmCCAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[allow(dead_code)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    ok: bool,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Clone, Debug)]
pub struct AppState {
    inner: Arc<Mutex<State>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct State {
    workspaces: Vec<WorkspaceRecord>,
    revision: u64,
    active_workspace_id: Option<String>,
    active_pane_id: Option<String>,
    active_surface_id: Option<String>,
    notifications: Vec<NotificationRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRecord {
    id: String,
    title: String,
    panes: Vec<PaneRecord>,
    surfaces: Vec<SurfaceRecord>,
    layout: PaneNode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaneRecord {
    id: String,
    workspace_id: String,
    surface_ids: Vec<String>,
    active_surface_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SurfaceRecord {
    id: String,
    workspace_id: String,
    pane_id: String,
    title: String,
    kind: SurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    history: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_index: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    input_log: Vec<String>,
    #[serde(default, skip_serializing_if = "BrowserState::is_empty")]
    browser_state: BrowserState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationRecord {
    id: String,
    title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    subtitle: String,
    body: String,
    workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    surface_id: Option<String>,
    #[serde(default)]
    created_at_ms: u64,
    #[serde(default)]
    read: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum SurfaceKind {
    Terminal,
    Browser,
    Markdown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserState {
    html: String,
    title: String,
    active_element_id: Option<String>,
    page_scroll_y: i64,
    hover_count: u64,
    dbl_count: u64,
    key_down_count: u64,
    key_up_count: u64,
    key_press_count: u64,
    elements: BTreeMap<String, BrowserElement>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElement {
    id: String,
    tag: String,
    text: String,
    html: String,
    value: String,
    checked: bool,
    disabled: bool,
    visible: bool,
    scroll_top: i64,
    attrs: BTreeMap<String, String>,
}

impl BrowserState {
    fn is_empty(&self) -> bool {
        self.html.is_empty() && self.elements.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum PaneNode {
    Leaf {
        pane_id: String,
    },
    Split {
        direction: SplitDirection,
        ratio: f64,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

#[derive(Clone, Debug)]
pub struct WorkspaceSummary {
    pub id: String,
    pub title: String,
    pub pane_count: usize,
    pub surface_count: usize,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct WorkspaceDetail {
    pub id: String,
    pub title: String,
    pub layout: PaneLayoutDetail,
}

#[derive(Clone, Debug)]
pub struct PaneDetail {
    pub id: String,
    pub selected_surface_id: Option<String>,
    pub surfaces: Vec<SurfaceDetail>,
}

#[derive(Clone, Debug)]
pub struct SurfaceDetail {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub url: Option<String>,
    pub initial_command: Option<String>,
    pub working_directory: Option<String>,
    pub focused: bool,
}

#[derive(Clone, Debug)]
pub enum PaneLayoutDetail {
    Leaf(PaneDetail),
    Split {
        direction: String,
        ratio: f64,
        first: Box<PaneLayoutDetail>,
        second: Box<PaneLayoutDetail>,
    },
}

#[derive(Clone, Debug)]
pub struct NotificationSummary {
    pub id: String,
    pub title: String,
    pub body: String,
    pub workspace_id: Option<String>,
    pub surface_id: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(default_state())),
        }
    }
}

impl AppState {
    fn load_or_default() -> Self {
        let state = load_persisted_state().unwrap_or_else(default_state);
        Self {
            inner: Arc::new(Mutex::new(repair_state(state))),
        }
    }

    pub fn revision(&self) -> u64 {
        let state = self.inner.lock().expect("app state lock poisoned");
        state.revision
    }

    pub fn workspace_summaries(&self) -> Vec<WorkspaceSummary> {
        let state = self.inner.lock().expect("app state lock poisoned");
        state
            .workspaces
            .iter()
            .map(|workspace| WorkspaceSummary {
                id: workspace.id.clone(),
                title: workspace.title.clone(),
                pane_count: workspace.panes.len(),
                surface_count: workspace.surfaces.len(),
                selected: state.active_workspace_id.as_deref() == Some(workspace.id.as_str()),
            })
            .collect()
    }

    pub fn notification_count(&self) -> usize {
        let state = self.inner.lock().expect("app state lock poisoned");
        state.notifications.len()
    }

    pub fn notification_summaries(&self) -> Vec<NotificationSummary> {
        let state = self.inner.lock().expect("app state lock poisoned");
        state
            .notifications
            .iter()
            .map(|notification| NotificationSummary {
                id: notification.id.clone(),
                title: notification.title.clone(),
                body: notification.body.clone(),
                workspace_id: notification.workspace_id.clone(),
                surface_id: notification.surface_id.clone(),
            })
            .collect()
    }

    pub fn latest_notification(&self) -> Option<NotificationSummary> {
        self.notification_summaries().into_iter().last()
    }

    pub fn clear_notifications_for_ui(&self) {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        state.notifications.clear();
        mark_state_changed(&mut state);
    }

    pub fn active_workspace_detail(&self) -> Option<WorkspaceDetail> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let workspace = current_workspace_record(&state)?;
        Some(workspace_detail(&state, workspace))
    }

    pub fn select_workspace_by_id(&self, workspace_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let index = find_workspace_index(&state, workspace_id)
            .ok_or_else(|| format!("unknown workspace: {workspace_id}"))?;
        let workspace = state.workspaces[index].clone();
        state.active_workspace_id = Some(workspace.id.clone());
        state.active_pane_id = workspace.panes.first().map(|pane| pane.id.clone());
        state.active_surface_id = workspace
            .panes
            .first()
            .and_then(|pane| pane.active_surface_id.clone())
            .or_else(|| workspace.surfaces.first().map(|surface| surface.id.clone()));
        mark_state_changed(&mut state);
        Ok(())
    }

    pub fn focus_surface_by_id(&self, surface_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = resolve_surface_identifier(&state, surface_id).map_err(|error| error.message)?;
        let Some((workspace_index, surface_index)) = surface_location(&state, &surface_id) else {
            return Err(format!("unknown surface: {surface_id}"));
        };
        let surface = state.workspaces[workspace_index].surfaces[surface_index].clone();
        if let Some(pane) = state.workspaces[workspace_index]
            .panes
            .iter_mut()
            .find(|pane| pane.id == surface.pane_id)
        {
            pane.active_surface_id = Some(surface.id.clone());
        }
        state.active_workspace_id = Some(surface.workspace_id);
        state.active_pane_id = Some(surface.pane_id);
        state.active_surface_id = Some(surface.id);
        mark_state_changed(&mut state);
        Ok(())
    }

    pub fn create_workspace_with_title(&self, title: impl Into<String>) -> String {
        self.create_workspace_with_terminal(title.into(), None, None)
    }

    fn list_workspaces(&self) -> Value {
        let state = self.inner.lock().expect("app state lock poisoned");
        let workspaces: Vec<Value> = state
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| workspace_summary_value(&state, index, workspace))
            .collect();
        json!({ "window_id": window_id(), "workspaces": workspaces })
    }

    fn create_workspace(&self, params: Option<Value>) -> Value {
        let title = params
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Workspace")
            .to_string();

        let initial_command = optional_string_any(
            params.as_ref(),
            &["command", "initial_command", "initialCommand"],
        );
        let working_directory = optional_string_any(
            params.as_ref(),
            &["working_directory", "workingDirectory", "cwd"],
        );

        let id = self.create_workspace_with_terminal(title, initial_command, working_directory);
        let state = self.inner.lock().expect("app state lock poisoned");
        let index = state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == id)
            .expect("workspace just created");
        workspace_result_value(&state, index, &state.workspaces[index])
    }

    fn create_workspace_with_terminal(
        &self,
        title: String,
        initial_command: Option<String>,
        working_directory: Option<String>,
    ) -> String {
        let workspace_id = next_workspace_id();
        let pane_id = next_pane_id();
        let surface_id = next_surface_id();
        let workspace = WorkspaceRecord {
            id: workspace_id.clone(),
            title,
            layout: PaneNode::Leaf {
                pane_id: pane_id.clone(),
            },
            panes: vec![PaneRecord {
                id: pane_id.clone(),
                workspace_id: workspace_id.clone(),
                surface_ids: vec![surface_id.clone()],
                active_surface_id: Some(surface_id.clone()),
            }],
            surfaces: vec![SurfaceRecord {
                id: surface_id.clone(),
                workspace_id: workspace_id.clone(),
                pane_id: pane_id.clone(),
                title: "Terminal".to_string(),
                kind: SurfaceKind::Terminal,
                initial_command,
                working_directory,
                url: None,
                history: Vec::new(),
                history_index: None,
                input_log: Vec::new(),
                browser_state: BrowserState::default(),
            }],
        };
        let id = workspace.id.clone();
        let mut state = self.inner.lock().expect("app state lock poisoned");
        state.workspaces.push(workspace);
        state.active_workspace_id = Some(id.clone());
        state.active_pane_id = Some(pane_id);
        state.active_surface_id = Some(surface_id);
        mark_state_changed(&mut state);
        id
    }

    fn list_surfaces(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = optional_string_any(
            params.as_ref(),
            &["workspace", "workspaceId", "workspace_id"],
        )
        .map(|value| resolve_workspace_identifier(&state, &value))
        .transpose()?;
        let mut surfaces = Vec::new();
        for workspace in state
            .workspaces
            .iter()
            .filter(|workspace| workspace_id.as_deref().map_or(true, |id| workspace.id == id))
        {
            for (index, surface) in workspace.surfaces.iter().enumerate() {
                surfaces.push(surface_summary_value(&state, workspace, index, surface));
            }
        }

        Ok(json!({ "surfaces": surfaces }))
    }

    fn create_surface(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id =
            optional_string_any(params.as_ref(), &["workspace", "workspaceId", "workspace_id"]);
        let pane_id = optional_string_any(params.as_ref(), &["pane", "paneId", "pane_id"]);
        let kind = params
            .as_ref()
            .and_then(|value| value.get("type").or_else(|| value.get("kind")))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| invalid_params("surface type must be terminal, browser, or markdown"))?
            .unwrap_or(SurfaceKind::Terminal);
        let url = params
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let initial_command = optional_string_any(
            params.as_ref(),
            &["command", "initial_command", "initialCommand"],
        );
        let working_directory = optional_string_any(
            params.as_ref(),
            &["working_directory", "workingDirectory", "cwd"],
        );
        let default_title = default_surface_title(&kind, url.as_deref());
        let title = params
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or(default_title);

        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = workspace_id
            .map(|value| resolve_workspace_identifier(&state, &value))
            .transpose()?;
        let pane_id = pane_id
            .map(|value| resolve_pane_identifier(&state, &value))
            .transpose()?;
        let active_pane_id = state.active_pane_id.clone();
        let workspace_index = if let Some(workspace_id) = workspace_id {
            state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == workspace_id)
                .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?
        } else if let Some(pane_id) = pane_id.as_ref() {
            state
                .workspaces
                .iter()
                .position(|workspace| workspace.panes.iter().any(|pane| pane.id == *pane_id))
                .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?
        } else {
            state
                .workspaces
                .iter()
                .position(|_| true)
                .ok_or_else(|| invalid_params("no workspace exists"))?
        };

        let (pane_id, surface) = {
            let workspace = &mut state.workspaces[workspace_index];
            let pane_index = if let Some(pane_id) = pane_id {
                workspace
                    .panes
                    .iter()
                    .position(|pane| pane.id == pane_id)
                    .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?
            } else if let Some(active_pane_id) = active_pane_id.as_ref() {
                workspace
                    .panes
                    .iter()
                    .position(|pane| pane.id == *active_pane_id)
                    .unwrap_or_else(|| ensure_default_pane(workspace))
            } else {
                ensure_default_pane(workspace)
            };
            let pane_id = workspace.panes[pane_index].id.clone();
            let mut surface = SurfaceRecord {
                id: next_surface_id(),
                workspace_id: workspace.id.clone(),
                pane_id: pane_id.clone(),
                title,
                kind,
                initial_command,
                working_directory,
                history: url.iter().cloned().collect(),
                history_index: url.as_ref().map(|_| 0),
                url,
                input_log: Vec::new(),
                browser_state: BrowserState::default(),
            };
            initialize_browser_state(&mut surface);
            workspace.surfaces.push(surface.clone());
            workspace.panes[pane_index].surface_ids.push(surface.id.clone());
            workspace.panes[pane_index].active_surface_id = Some(surface.id.clone());
            (pane_id, surface)
        };
        state.active_workspace_id = Some(surface.workspace_id.clone());
        state.active_pane_id = Some(pane_id);
        state.active_surface_id = Some(surface.id.clone());
        mark_state_changed(&mut state);
        let surface_index = state.workspaces[workspace_index]
            .surfaces
            .iter()
            .position(|candidate| candidate.id == surface.id)
            .unwrap_or(0);
        Ok(surface_result_value(
            &state,
            workspace_index,
            surface_index,
            &surface,
        ))
    }

    fn current_workspace(&self) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let workspace = current_workspace_record(&state).ok_or_else(|| invalid_params("no workspace exists"))?;
        let index = state
            .workspaces
            .iter()
            .position(|candidate| candidate.id == workspace.id)
            .unwrap_or(0);
        Ok(workspace_result_value(&state, index, workspace))
    }

    fn select_workspace(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id = required_workspace_param(params.as_ref())?;
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = resolve_workspace_identifier(&state, &workspace_id)?;
        let workspace = state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .cloned()
            .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?;
        state.active_workspace_id = Some(workspace.id.clone());
        state.active_pane_id = workspace.panes.first().map(|pane| pane.id.clone());
        state.active_surface_id = workspace.surfaces.first().map(|surface| surface.id.clone());
        mark_state_changed(&mut state);
        let index = state
            .workspaces
            .iter()
            .position(|candidate| candidate.id == workspace.id)
            .unwrap_or(0);
        Ok(workspace_result_value(&state, index, &workspace))
    }

    fn close_workspace(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id = required_workspace_param(params.as_ref())?;
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = resolve_workspace_identifier(&state, &workspace_id)?;
        let index = state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?;
        if state.workspaces.len() == 1 {
            return Err(invalid_params("cannot close the last workspace"));
        }
        let workspace = state.workspaces.remove(index);
        if state.active_workspace_id.as_deref() == Some(workspace.id.as_str()) {
            let active_workspace_id = state.workspaces.first().map(|workspace| workspace.id.clone());
            let active_surface_id = state
                .workspaces
                .first()
                .and_then(|workspace| workspace.surfaces.first())
                .map(|surface| surface.id.clone());
            let active_pane_id = state
                .workspaces
                .first()
                .and_then(|workspace| workspace.panes.first())
                .map(|pane| pane.id.clone());
            state.active_workspace_id = active_workspace_id;
            state.active_pane_id = active_pane_id;
            state.active_surface_id = active_surface_id;
        }
        mark_state_changed(&mut state);
        Ok(json!({
            "window_id": window_id(),
            "workspace_id": workspace.id,
            "closed": true,
            "workspace": workspace,
        }))
    }

    fn focus_surface(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let surface_id = required_surface_param(params.as_ref())?;
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = resolve_surface_identifier(&state, &surface_id)?;
        let surface = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.surfaces.iter())
            .find(|surface| surface.id == surface_id)
            .cloned()
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        state.active_workspace_id = Some(surface.workspace_id.clone());
        state.active_pane_id = Some(surface.pane_id.clone());
        state.active_surface_id = Some(surface.id.clone());
        if let Some((workspace_index, pane_index)) = pane_location(&state, &surface.pane_id) {
            state.workspaces[workspace_index].panes[pane_index].active_surface_id =
                Some(surface.id.clone());
        }
        if matches!(surface.kind, SurfaceKind::Terminal) {
            let _ = terminal::focus(&surface.id, true);
        }
        mark_state_changed(&mut state);
        let (workspace_index, surface_index) = surface_location(&state, &surface.id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {}", surface.id)))?;
        Ok(surface_result_value(&state, workspace_index, surface_index, &surface))
    }

    fn close_surface(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = match required_surface_param(params.as_ref()) {
            Ok(surface_id) => resolve_surface_identifier(&state, &surface_id)?,
            Err(_) => state
                .active_surface_id
                .clone()
                .ok_or_else(|| invalid_params("no active surface"))?,
        };
        let was_active = state.active_surface_id.as_deref() == Some(surface_id.as_str());
        let mut removed: Option<(SurfaceRecord, Option<String>, Option<String>)> = None;
        for workspace in &mut state.workspaces {
            if let Some(index) = workspace
                .surfaces
                .iter()
                .position(|surface| surface.id == surface_id)
            {
                let surface = workspace.surfaces.remove(index);
                for pane in &mut workspace.panes {
                    pane.surface_ids.retain(|id| id != &surface.id);
                    if pane.active_surface_id.as_deref() == Some(surface.id.as_str()) {
                        pane.active_surface_id = pane.surface_ids.first().cloned();
                    }
                }
                let workspace_has_surfaces = !workspace.surfaces.is_empty();
                workspace
                    .panes
                    .retain(|pane| !pane.surface_ids.is_empty() || !workspace_has_surfaces);
                let pane_ids: Vec<String> = workspace.panes.iter().map(|pane| pane.id.clone()).collect();
                if let Some(layout) = prune_layout(workspace.layout.clone(), &pane_ids) {
                    workspace.layout = layout;
                }
                let active_ids = if was_active {
                    let active_pane = workspace.panes.first();
                    (
                        active_pane.map(|pane| pane.id.clone()),
                        active_pane.and_then(|pane| pane.active_surface_id.clone()),
                    )
                } else {
                    (state.active_pane_id.clone(), state.active_surface_id.clone())
                };
                removed = Some((surface, active_ids.0, active_ids.1));
                break;
            }
        }
        if let Some((surface, active_pane_id, active_surface_id)) = removed {
            match &surface.kind {
                SurfaceKind::Terminal => {
                    let _ = terminal::unregister(&surface.id);
                }
                SurfaceKind::Browser => {
                    let _ = browser::unregister(&surface.id);
                }
                SurfaceKind::Markdown => {}
            }
            state.active_pane_id = active_pane_id;
            state.active_surface_id = active_surface_id;
            mark_state_changed(&mut state);
            return Ok(json!({
                "workspace_id": surface.workspace_id,
                "pane_id": surface.pane_id,
                "surface_id": surface.id,
                "closed": true,
                "surface": surface,
            }));
        }
        Err(invalid_params(format!("unknown surface: {surface_id}")))
    }

    fn list_panes(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = optional_string_any(
            params.as_ref(),
            &["workspace", "workspaceId", "workspace_id"],
        )
        .map(|value| resolve_workspace_identifier(&state, &value))
        .transpose()?;
        let mut panes = Vec::new();
        for workspace in state
            .workspaces
            .iter()
            .filter(|workspace| workspace_id.as_deref().map_or(true, |id| workspace.id == id))
        {
            for (index, pane) in workspace.panes.iter().enumerate() {
                panes.push(pane_summary_value(&state, workspace, index, pane));
            }
        }
        Ok(json!({ "panes": panes }))
    }

    fn pane_surfaces(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let pane_id = optional_string_any(params.as_ref(), &["pane", "paneId", "pane_id"]);
        let state = self.inner.lock().expect("app state lock poisoned");
        let Some(pane_id) = pane_id
            .map(|value| resolve_pane_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_pane_id.clone())
        else {
            return Ok(json!({ "surfaces": [] }));
        };
        let workspace = state
            .workspaces
            .iter()
            .find(|workspace| workspace.panes.iter().any(|pane| pane.id == pane_id))
            .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?;
        let pane = workspace
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .expect("pane already located");
        let surfaces: Vec<Value> = pane
            .surface_ids
            .iter()
            .enumerate()
            .filter_map(|(index, surface_id)| {
                workspace.surfaces.iter().find(|surface| surface.id == *surface_id).map(
                    |surface| surface_summary_value(&state, workspace, index, surface),
                )
            })
            .collect();
        let workspace_index = state
            .workspaces
            .iter()
            .position(|candidate| candidate.id == workspace.id)
            .unwrap_or(0);
        let pane_index = workspace
            .panes
            .iter()
            .position(|candidate| candidate.id == pane.id)
            .unwrap_or(0);
        Ok(json!({
            "workspace_id": workspace.id,
            "pane_id": pane.id,
            "pane": pane_summary_value(&state, workspace, pane_index, pane),
            "workspace": workspace_summary_value(&state, workspace_index, workspace),
            "surfaces": surfaces
        }))
    }

    fn create_pane(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id =
            optional_string_any(params.as_ref(), &["workspace", "workspaceId", "workspace_id"]);
        let pane_id = optional_string_any(params.as_ref(), &["pane", "paneId", "pane_id"]);
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"]);
        let split_direction = split_direction_from_params(params.as_ref())?;
        let insert_before = split_insert_before(params.as_ref());
        let kind = params
            .as_ref()
            .and_then(|value| value.get("type").or_else(|| value.get("kind")))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| invalid_params("pane type must be terminal, browser, or markdown"))?
            .unwrap_or(SurfaceKind::Terminal);
        let url = params
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let initial_command = optional_string_any(
            params.as_ref(),
            &["command", "initial_command", "initialCommand"],
        );
        let working_directory = optional_string_any(
            params.as_ref(),
            &["working_directory", "workingDirectory", "cwd"],
        );
        let title = default_surface_title(&kind, url.as_deref());

        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = workspace_id
            .map(|value| resolve_workspace_identifier(&state, &value))
            .transpose()?;
        let pane_id = pane_id
            .map(|value| resolve_pane_identifier(&state, &value))
            .transpose()?;
        let surface_id = surface_id
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?;
        let target_pane_id = if let Some(surface_id) = surface_id.as_ref() {
            surface_location(&state, surface_id).map(|(workspace_index, surface_index)| {
                state.workspaces[workspace_index].surfaces[surface_index]
                    .pane_id
                    .clone()
            })
        } else {
            pane_id.clone().or_else(|| state.active_pane_id.clone())
        };
        let workspace_index = if let Some(workspace_id) = workspace_id {
            state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == workspace_id)
                .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?
        } else if let Some(target_pane_id) = target_pane_id.as_ref() {
            state
                .workspaces
                .iter()
                .position(|workspace| workspace.panes.iter().any(|pane| pane.id == *target_pane_id))
                .ok_or_else(|| invalid_params(format!("unknown pane: {target_pane_id}")))?
        } else {
            let active_workspace_id = state.active_workspace_id.clone();
            active_workspace_id
                .as_ref()
                .and_then(|id| state.workspaces.iter().position(|workspace| workspace.id == *id))
                .or_else(|| state.workspaces.first().map(|_| 0))
                .ok_or_else(|| invalid_params("no workspace exists"))?
        };
        let pane_id = next_pane_id();
        let surface_id = next_surface_id();
        let (workspace_id, pane, surface) = {
            let workspace = &mut state.workspaces[workspace_index];
            let pane = PaneRecord {
                id: pane_id.clone(),
                workspace_id: workspace.id.clone(),
                surface_ids: vec![surface_id.clone()],
                active_surface_id: Some(surface_id.clone()),
            };
            let mut surface = SurfaceRecord {
                id: surface_id,
                workspace_id: workspace.id.clone(),
                pane_id: pane_id.clone(),
                title,
                kind,
                initial_command,
                working_directory,
                history: url.iter().cloned().collect(),
                history_index: url.as_ref().map(|_| 0),
                url,
                input_log: Vec::new(),
                browser_state: BrowserState::default(),
            };
            initialize_browser_state(&mut surface);
            workspace.panes.push(pane.clone());
            workspace.surfaces.push(surface.clone());
            let target_pane_id = target_pane_id
                .as_deref()
                .filter(|target| workspace.panes.iter().any(|pane| pane.id == *target));
            insert_pane_in_layout(
                &mut workspace.layout,
                target_pane_id,
                pane.id.clone(),
                split_direction,
                insert_before,
            );
            (workspace.id.clone(), pane, surface)
        };
        state.active_workspace_id = Some(workspace_id.clone());
        state.active_pane_id = Some(pane.id.clone());
        state.active_surface_id = Some(surface.id.clone());
        mark_state_changed(&mut state);
        let workspace = &state.workspaces[workspace_index];
        let pane_index = workspace
            .panes
            .iter()
            .position(|candidate| candidate.id == pane.id)
            .unwrap_or(0);
        let surface_index = workspace
            .surfaces
            .iter()
            .position(|candidate| candidate.id == surface.id)
            .unwrap_or(0);
        Ok(json!({
            "window_id": window_id(),
            "workspace_id": workspace_id,
            "pane_id": pane.id,
            "surface_id": surface.id,
            "pane": pane_summary_value(&state, workspace, pane_index, &pane),
            "surface": surface_summary_value(&state, workspace, surface_index, &surface),
        }))
    }

    fn browser_url(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        let surface = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.surfaces.iter())
            .find(|surface| surface.id == surface_id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        ensure_browser_surface(surface)?;
        Ok(json!({
            "surface_id": surface.id,
            "url": surface.url.clone().unwrap_or_default(),
            "title": surface.title,
            "loading": false,
            "surface": surface,
        }))
    }

    fn navigate_browser(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let url = params
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("url is required"))?
            .to_string();
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        for workspace in &mut state.workspaces {
            if let Some(surface) = workspace
                .surfaces
                .iter_mut()
                .find(|surface| surface.id == surface_id)
            {
                ensure_browser_surface(surface)?;
                surface.url = Some(url.clone());
                surface.title = url.clone();
                if let Some(index) = surface.history_index {
                    surface.history.truncate(index.saturating_add(1));
                }
                surface.history.push(url.clone());
                surface.history_index = Some(surface.history.len() - 1);
                surface.browser_state = browser_state_for_url(&url);
                if !surface.browser_state.title.is_empty() {
                    surface.title = surface.browser_state.title.clone();
                } else {
                    surface.title = url.clone();
                }
                let live_webview = browser::navigate(&surface.id, &url);
                let surface = surface.clone();
                state.active_workspace_id = Some(surface.workspace_id.clone());
                state.active_surface_id = Some(surface.id.clone());
                mark_state_changed(&mut state);
                return Ok(json!({
                    "workspace_id": surface.workspace_id,
                    "pane_id": surface.pane_id,
                    "surface_id": surface.id,
                    "url": surface.url.clone().unwrap_or_default(),
                    "live_webview": live_webview,
                    "surface": surface,
                }));
            }
        }
        Err(invalid_params(format!("unknown surface: {surface_id}")))
    }

    fn browser_history_step(&self, params: Option<Value>, delta: isize) -> Result<Value, RpcError> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        for workspace in &mut state.workspaces {
            if let Some(surface) = workspace
                .surfaces
                .iter_mut()
                .find(|surface| surface.id == surface_id)
            {
                ensure_browser_surface(surface)?;
                if surface.history.is_empty() {
                    return Ok(json!({ "surface_id": surface.id, "surface": surface.clone(), "url": surface.url.clone().unwrap_or_default() }));
                }
                let current = surface.history_index.unwrap_or(surface.history.len() - 1);
                let next = current
                    .saturating_add_signed(delta)
                    .min(surface.history.len() - 1);
                surface.history_index = Some(next);
                surface.url = surface.history.get(next).cloned();
                if let Some(url) = surface.url.clone() {
                    surface.browser_state = browser_state_for_url(&url);
                    surface.title = if surface.browser_state.title.is_empty() {
                        url
                    } else {
                        surface.browser_state.title.clone()
                    };
                }
                let surface = surface.clone();
                let live_webview = match delta.cmp(&0) {
                    std::cmp::Ordering::Less => browser::go_back(&surface.id),
                    std::cmp::Ordering::Greater => browser::go_forward(&surface.id),
                    std::cmp::Ordering::Equal => false,
                };
                let live_webview = live_webview
                    || surface
                        .url
                        .as_ref()
                        .is_some_and(|url| browser::navigate(&surface.id, url));
                state.active_workspace_id = Some(surface.workspace_id.clone());
                state.active_surface_id = Some(surface.id.clone());
                mark_state_changed(&mut state);
                return Ok(json!({
                    "surface_id": surface.id,
                    "surface": surface,
                    "url": surface.url.clone().unwrap_or_default(),
                    "live_webview": live_webview,
                }));
            }
        }
        Err(invalid_params(format!("unknown surface: {surface_id}")))
    }

    fn browser_reload(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let result = self.browser_url(params)?;
        let surface_id = result
            .get("surface_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_params("surface_id missing from browser URL result"))?;
        let live_webview = browser::reload(surface_id)
            || result
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .is_some_and(|url| browser::navigate(surface_id, url));
        Ok(json!({
            "surface_id": surface_id,
            "url": result.get("url").cloned().unwrap_or(Value::String(String::new())),
            "live_webview": live_webview,
        }))
    }

    fn create_notification(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let title = params
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("cmux")
            .to_string();
        let body = params
            .as_ref()
            .and_then(|value| value.get("body"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let subtitle = params
            .as_ref()
            .and_then(|value| value.get("subtitle"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let workspace_id =
            optional_string_any(params.as_ref(), &["workspace", "workspaceId", "workspace_id"]);
        let surface_id =
            optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"]);
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = workspace_id
            .map(|value| resolve_workspace_identifier(&state, &value))
            .transpose()?;
        let surface_id = surface_id
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?;
        let notification = NotificationRecord {
            id: next_notification_id(),
            title,
            subtitle,
            body,
            workspace_id,
            surface_id,
            created_at_ms: now_ms(),
            read: false,
        };
        state.notifications.push(notification.clone());
        mark_state_changed(&mut state);
        Ok(json!({ "notification": notification }))
    }

    fn send_to_surface(&self, params: Option<Value>, key: &str) -> Result<Value, RpcError> {
        let payload = params
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        for workspace in &mut state.workspaces {
            if let Some(surface) = workspace
                .surfaces
                .iter_mut()
                .find(|surface| surface.id == surface_id)
            {
                surface.input_log.push(payload.clone());
                let live_io = if matches!(surface.kind, SurfaceKind::Terminal) {
                    match key {
                        "text" => terminal::send_text(&surface.id, &payload),
                        "key" => terminal::send_key(&surface.id, &payload),
                        _ => false,
                    }
                } else {
                    false
                };
                let surface = surface.clone();
                state.active_workspace_id = Some(surface.workspace_id.clone());
                state.active_pane_id = Some(surface.pane_id.clone());
                state.active_surface_id = Some(surface.id.clone());
                mark_state_changed(&mut state);
                return Ok(json!({
                    "workspace_id": surface.workspace_id,
                    "pane_id": surface.pane_id,
                    "surface_id": surface.id,
                    "surface": surface,
                    "accepted": true,
                    "live_io": live_io
                }));
            }
        }
        Err(invalid_params(format!("unknown surface: {surface_id}")))
    }

    fn read_surface_text(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        let surface = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.surfaces.iter())
            .find(|surface| surface.id == surface_id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        let text = match surface.kind {
            SurfaceKind::Browser => browser_inner_text(&surface.browser_state),
            SurfaceKind::Terminal => terminal::read_text(&surface.id)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| surface.input_log.join("")),
            SurfaceKind::Markdown => surface.input_log.join(""),
        };
        let terminal_status = terminal::status(&surface.id);
        let backend = terminal_status
            .as_ref()
            .map(|status| status.backend)
            .unwrap_or_else(|| {
                if matches!(surface.kind, SurfaceKind::Terminal) {
                    "linux-state"
                } else {
                    surface_kind_name(&surface.kind)
                }
            });
        Ok(json!({
            "surface_id": surface.id,
            "text": text,
            "backend": backend,
            "live_io": terminal_status.as_ref().map_or(false, |status| status.live),
            "terminal": terminal_status,
        }))
    }

    fn clear_surface_history(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        let mut cleared = false;
        for workspace in &mut state.workspaces {
            if let Some(surface) = workspace.surfaces.iter_mut().find(|surface| surface.id == surface_id) {
                surface.input_log.clear();
                if matches!(surface.kind, SurfaceKind::Terminal) {
                    let _ = terminal::clear_text(&surface.id);
                }
                if matches!(surface.kind, SurfaceKind::Browser) {
                    surface.browser_state = BrowserState::default();
                }
                cleared = true;
                break;
            }
        }
        if !cleared {
            return Err(invalid_params(format!("unknown surface: {surface_id}")));
        }
        mark_state_changed(&mut state);
        Ok(json!({ "surface_id": surface_id, "cleared": true }))
    }

    fn surface_health(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        let surface = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.surfaces.iter())
            .find(|surface| surface.id == surface_id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        let terminal_status = terminal::status(&surface.id);
        let backend = terminal_status
            .as_ref()
            .map(|status| status.backend)
            .unwrap_or_else(|| {
                if matches!(surface.kind, SurfaceKind::Terminal) {
                    "not-mounted"
                } else {
                    surface_kind_name(&surface.kind)
                }
            });
        Ok(json!({
            "surface_id": surface.id,
            "ok": !matches!(surface.kind, SurfaceKind::Terminal) || terminal_status.as_ref().map_or(false, |status| status.live),
            "kind": surface_kind_name(&surface.kind),
            "backend": backend,
            "live_io": terminal_status.as_ref().map_or(false, |status| status.live),
            "terminal": terminal_status,
            "browser_live": matches!(surface.kind, SurfaceKind::Browser) && browser::is_registered(&surface.id),
        }))
    }

    fn identify(&self) -> Value {
        let state = self.inner.lock().expect("app state lock poisoned");
        let focused = json!({
            "window_id": window_id(),
            "workspace_id": state.active_workspace_id,
            "pane_id": state.active_pane_id,
            "surface_id": state.active_surface_id,
        });
        json!({
            "platform": "linux",
            "window_id": window_id(),
            "focused": focused,
        })
    }

    fn rename_workspace(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id = optional_string_any(
            params.as_ref(),
            &["workspace", "workspaceId", "workspace_id"],
        );
        let title = params
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("title is required"))?
            .to_string();
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = workspace_id
            .map(|value| resolve_workspace_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_workspace_id.clone())
            .ok_or_else(|| invalid_params("no workspace selected"))?;
        let index = find_workspace_index(&state, &workspace_id)
            .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?;
        state.workspaces[index].title = title;
        mark_state_changed(&mut state);
        Ok(workspace_result_value(&state, index, &state.workspaces[index]))
    }

    fn focus_pane(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let pane_id = required_pane_param(params.as_ref())?;
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let pane_id = resolve_pane_identifier(&state, &pane_id)?;
        let Some((workspace_index, pane_index)) = pane_location(&state, &pane_id) else {
            return Err(invalid_params(format!("unknown pane: {pane_id}")));
        };
        let pane = state.workspaces[workspace_index].panes[pane_index].clone();
        state.active_workspace_id = Some(pane.workspace_id.clone());
        state.active_pane_id = Some(pane.id.clone());
        state.active_surface_id = pane.active_surface_id.clone();
        mark_state_changed(&mut state);
        Ok(json!({
            "workspace_id": pane.workspace_id,
            "pane_id": pane.id,
            "pane": pane_summary_value(&state, &state.workspaces[workspace_index], pane_index, &pane),
        }))
    }

    fn list_notifications(&self) -> Value {
        let state = self.inner.lock().expect("app state lock poisoned");
        json!({ "notifications": state.notifications })
    }

    fn clear_notifications(&self) -> Value {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let count = state.notifications.len();
        state.notifications.clear();
        mark_state_changed(&mut state);
        json!({ "cleared": count })
    }

    fn browser_focus(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let focused = self.focus_surface(params)?;
        if let Some(surface_id) = focused.get("surface_id").and_then(Value::as_str) {
            let _ = browser::focus(surface_id);
        }
        Ok(json!({
            "surface_id": focused.get("surface_id").cloned().unwrap_or(Value::Null),
            "focused": true,
        }))
    }

    fn browser_focused(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        Ok(json!({
            "surface_id": surface_id,
            "focused": state.active_surface_id.as_deref() == Some(surface_id.as_str()),
            "live_webview": browser::is_registered(&surface_id),
        }))
    }

    fn browser_automation(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = optional_string_any(params.as_ref(), &["surface", "surfaceId", "surface_id"])
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_surface_id.clone())
            .ok_or_else(|| invalid_params("no surface specified"))?;
        let (workspace_index, surface_index) = surface_location(&state, &surface_id)
            .ok_or_else(|| browser_not_found(format!("unknown surface: {surface_id}")))?;
        let surface = &mut state.workspaces[workspace_index].surfaces[surface_index];
        ensure_browser_surface(surface)?;

        let result = match method {
            "browser.snapshot" => browser_snapshot(surface),
            "browser.eval" => {
                let script = required_string_any(params.as_ref(), &["script", "expression"])?;
                if let Some(value) = eval_live_browser_script(&surface.id, &script) {
                    json!({ "surface_id": surface.id, "value": value, "live_webview": true })
                } else {
                    json!({ "surface_id": surface.id, "value": eval_browser_script(&surface.browser_state, &script), "live_webview": false })
                }
            }
            "browser.wait" => browser_wait(surface, params.as_ref())?,
            "browser.click" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.click(); return true;"),
                );
                if !live_webview {
                    ensure_browser_element(&surface.browser_state, &selector)?;
                }
                if !live_webview && selector == "#btn" {
                    let value = surface
                        .browser_state
                        .elements
                        .get("name")
                        .map(|element| element.value.clone())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "empty".to_string());
                    if let Some(status) = surface.browser_state.elements.get_mut("status") {
                        status.text = value.clone();
                    }
                    if let Some(out) = surface.browser_state.elements.get_mut("out") {
                        out.text = value;
                    }
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.dblclick" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.dispatchEvent(new MouseEvent('dblclick', {{ bubbles: true, cancelable: true }})); return true;"),
                );
                if !live_webview {
                    ensure_browser_element(&surface.browser_state, &selector)?;
                }
                if !live_webview && selector == "#dbl" {
                    surface.browser_state.dbl_count = surface.browser_state.dbl_count.saturating_add(1);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.hover" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true, cancelable: true }})); el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: true, cancelable: true }})); return true;"),
                );
                if !live_webview {
                    ensure_browser_element(&surface.browser_state, &selector)?;
                }
                if !live_webview && selector == "#hover" {
                    surface.browser_state.hover_count = surface.browser_state.hover_count.saturating_add(1);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.focus" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.focus(); return true;"),
                );
                if !live_webview {
                    let element_id = browser_element_id(&surface.browser_state, &selector)?;
                    surface.browser_state.active_element_id = Some(element_id);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.fill" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let text = required_string_any(params.as_ref(), &["text", "value"])?;
                let selector_js = js_string_literal(&selector);
                let text_js = js_string_literal(&text);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.value = {text_js}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true;"),
                );
                if !live_webview {
                    let element = browser_element_mut(&mut surface.browser_state, &selector)?;
                    element.value = text;
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.type" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let text = required_string_any(params.as_ref(), &["text", "value"])?;
                let selector_js = js_string_literal(&selector);
                let text_js = js_string_literal(&text);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.focus(); el.value = String(el.value ?? '') + {text_js}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); return true;"),
                );
                if !live_webview {
                    let element = browser_element_mut(&mut surface.browser_state, &selector)?;
                    element.value.push_str(&text);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.press" => {
                let live_webview = optional_string_any(params.as_ref(), &["key"])
                    .map(|key| dispatch_live_keyboard_event(&surface.id, &key, "press"))
                    .unwrap_or(false);
                if !live_webview {
                    surface.browser_state.key_down_count =
                        surface.browser_state.key_down_count.saturating_add(1);
                    surface.browser_state.key_press_count =
                        surface.browser_state.key_press_count.saturating_add(1);
                    surface.browser_state.key_up_count =
                        surface.browser_state.key_up_count.saturating_add(1);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.keydown" => {
                let live_webview = optional_string_any(params.as_ref(), &["key"])
                    .map(|key| dispatch_live_keyboard_event(&surface.id, &key, "keydown"))
                    .unwrap_or(false);
                if !live_webview {
                    surface.browser_state.key_down_count =
                        surface.browser_state.key_down_count.saturating_add(1);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.keyup" => {
                let live_webview = optional_string_any(params.as_ref(), &["key"])
                    .map(|key| dispatch_live_keyboard_event(&surface.id, &key, "keyup"))
                    .unwrap_or(false);
                if !live_webview {
                    surface.browser_state.key_up_count =
                        surface.browser_state.key_up_count.saturating_add(1);
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.check" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.checked = true; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true;"),
                );
                if !live_webview {
                    browser_element_mut(&mut surface.browser_state, &selector)?.checked = true;
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.uncheck" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.checked = false; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true;"),
                );
                if !live_webview {
                    browser_element_mut(&mut surface.browser_state, &selector)?.checked = false;
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.select" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let value = required_string_any(params.as_ref(), &["value"])?;
                let selector_js = js_string_literal(&selector);
                let value_js = js_string_literal(&value);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.value = {value_js}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true;"),
                );
                if !live_webview {
                    browser_element_mut(&mut surface.browser_state, &selector)?.value = value;
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.scroll" => {
                let dy = params
                    .as_ref()
                    .and_then(|value| value.get("dy"))
                    .and_then(number_from_value)
                    .unwrap_or(0.0)
                    .round() as i64;
                if let Some(selector) = optional_string_any(params.as_ref(), &["selector"]) {
                    let selector_js = js_string_literal(&selector);
                    let live_webview = run_live_browser_script(
                        &surface.id,
                        &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.scrollTop += {dy}; return true;"),
                    );
                    if !live_webview {
                        let element = browser_element_mut(&mut surface.browser_state, &selector)?;
                        element.scroll_top = element.scroll_top.saturating_add(dy);
                    }
                    browser_action_result_with_live(surface, params.as_ref(), live_webview)
                } else {
                    let live_webview =
                        run_live_browser_script(&surface.id, &format!("window.scrollBy(0, {dy}); return true;"));
                    if !live_webview {
                        surface.browser_state.page_scroll_y =
                            surface.browser_state.page_scroll_y.saturating_add(dy);
                    }
                    browser_action_result_with_live(surface, params.as_ref(), live_webview)
                }
            }
            "browser.scroll_into_view" => {
                let selector = required_string_any(params.as_ref(), &["selector"])?;
                let selector_js = js_string_literal(&selector);
                let live_webview = run_live_browser_script(
                    &surface.id,
                    &format!("const el = document.querySelector({selector_js}); if (!el) return false; el.scrollIntoView({{ block: 'center', inline: 'nearest' }}); return true;"),
                );
                if !live_webview {
                    ensure_browser_element(&surface.browser_state, &selector)?;
                }
                browser_action_result_with_live(surface, params.as_ref(), live_webview)
            }
            "browser.screenshot" => browser_screenshot(surface, params.as_ref())?,
            method if method.starts_with("browser.get.") => browser_get(surface, method, params.as_ref())?,
            method if method.starts_with("browser.is.") => browser_is(surface, method, params.as_ref())?,
            method if method.starts_with("browser.find.") => browser_find(surface, method, params.as_ref())?,
            "browser.frame.main" => {
                let live_webview = browser::select_main_frame(&surface.id);
                json!({ "surface_id": surface.id, "frame": browser::active_frame(&surface.id).unwrap_or_else(|| "main".to_string()), "live_webview": live_webview })
            }
            "browser.frame.select" => {
                let selector = required_string_any(params.as_ref(), &["selector", "frame"])?;
                browser::select_frame(&surface.id, &selector).map_err(not_supported)?;
                json!({ "surface_id": surface.id, "frame": selector, "live_webview": true })
            }
            "browser.console.list" => {
                json!({ "surface_id": surface.id, "messages": browser::console_messages(&surface.id), "live_webview": browser::is_registered(&surface.id) })
            }
            "browser.errors.list" => {
                json!({ "surface_id": surface.id, "errors": browser::error_messages(&surface.id), "live_webview": browser::is_registered(&surface.id) })
            }
            "browser.highlight" => browser_action_result(surface, params.as_ref()),
            "browser.state.save" => json!({ "surface_id": surface.id, "state": surface.browser_state }),
            "browser.state.load" => {
                if let Some(state_value) = params.as_ref().and_then(|value| value.get("state")) {
                    surface.browser_state = serde_json::from_value(state_value.clone())
                        .map_err(|_| invalid_params("state must be a browser state object"))?;
                }
                browser_action_result(surface, params.as_ref())
            }
            "browser.dialog.respond" => browser_dialog_respond(surface, params.as_ref())?,
            "browser.download.wait" => browser_download_wait(surface, params.as_ref())?,
            _ => return Err(not_supported(format!("{method} is not available on Linux yet"))),
        };
        mark_state_changed(&mut state);
        Ok(result)
    }

    fn pane_last(&self) -> Result<Value, RpcError> {
        let state = self.inner.lock().expect("app state lock poisoned");
        let pane_id = state
            .active_pane_id
            .clone()
            .ok_or_else(|| invalid_params("no pane selected"))?;
        let (workspace_index, pane_index) = pane_location(&state, &pane_id)
            .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?;
        let workspace = &state.workspaces[workspace_index];
        let pane = &workspace.panes[pane_index];
        Ok(json!({
            "workspace_id": workspace.id,
            "pane_id": pane.id,
            "pane": pane_summary_value(&state, workspace, pane_index, pane),
        }))
    }

    fn pane_swap(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let pane_id = required_pane_param(params.as_ref())?;
        let target_pane_id = optional_string_any(
            params.as_ref(),
            &["target_pane", "targetPane", "target_pane_id", "targetPaneId"],
        )
        .ok_or_else(|| invalid_params("target_pane_id is required"))?;
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let pane_id = resolve_pane_identifier(&state, &pane_id)?;
        let target_pane_id = resolve_pane_identifier(&state, &target_pane_id)?;
        let (workspace_index, _) = pane_location(&state, &pane_id)
            .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?;
        let (target_workspace_index, _) = pane_location(&state, &target_pane_id)
            .ok_or_else(|| invalid_params(format!("unknown pane: {target_pane_id}")))?;
        if workspace_index != target_workspace_index {
            return Err(invalid_params("pane.swap is limited to one workspace on Linux"));
        }
        swap_panes_in_layout(&mut state.workspaces[workspace_index].layout, &pane_id, &target_pane_id);
        if params
            .as_ref()
            .and_then(|value| value.get("focus"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            state.active_workspace_id = Some(state.workspaces[workspace_index].id.clone());
            state.active_pane_id = Some(target_pane_id.clone());
            state.active_surface_id = state.workspaces[workspace_index]
                .panes
                .iter()
                .find(|pane| pane.id == target_pane_id)
                .and_then(|pane| pane.active_surface_id.clone());
        }
        mark_state_changed(&mut state);
        Ok(json!({
            "workspace_id": state.workspaces[workspace_index].id,
            "pane_id": pane_id,
            "target_pane_id": target_pane_id,
        }))
    }

    fn pane_resize(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let pane_id = optional_string_any(params.as_ref(), &["pane", "paneId", "pane_id"]);
        let ratio = params
            .as_ref()
            .and_then(|value| value.get("ratio").or_else(|| value.get("percent")))
            .and_then(number_from_value)
            .unwrap_or(0.5)
            .clamp(0.1, 0.9);
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let pane_id = pane_id
            .map(|value| resolve_pane_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_pane_id.clone())
            .ok_or_else(|| invalid_params("no pane selected"))?;
        let (workspace_index, _) = pane_location(&state, &pane_id)
            .ok_or_else(|| invalid_params(format!("unknown pane: {pane_id}")))?;
        if !resize_split_for_pane(&mut state.workspaces[workspace_index].layout, &pane_id, ratio) {
            return Err(invalid_params("pane has no resizable split"));
        }
        mark_state_changed(&mut state);
        Ok(json!({
            "workspace_id": state.workspaces[workspace_index].id,
            "pane_id": pane_id,
            "ratio": ratio,
        }))
    }

    fn workspace_equalize_splits(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let workspace_id = optional_string_any(
            params.as_ref(),
            &["workspace", "workspaceId", "workspace_id"],
        );
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let workspace_id = workspace_id
            .map(|value| resolve_workspace_identifier(&state, &value))
            .transpose()?
            .or_else(|| state.active_workspace_id.clone())
            .ok_or_else(|| invalid_params("no workspace selected"))?;
        let workspace_index = find_workspace_index(&state, &workspace_id)
            .ok_or_else(|| invalid_params(format!("unknown workspace: {workspace_id}")))?;
        equalize_layout(&mut state.workspaces[workspace_index].layout);
        mark_state_changed(&mut state);
        Ok(workspace_result_value(
            &state,
            workspace_index,
            &state.workspaces[workspace_index],
        ))
    }

    fn move_surface(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let surface_id = required_surface_param(params.as_ref())?;
        let target_pane_id = optional_string_any(
            params.as_ref(),
            &["pane", "paneId", "pane_id", "target_pane", "targetPane", "target_pane_id"],
        );
        let before_surface_id = optional_string_any(
            params.as_ref(),
            &["before", "beforeSurface", "before_surface_id", "beforeSurfaceId"],
        );
        let after_surface_id = optional_string_any(
            params.as_ref(),
            &["after", "afterSurface", "after_surface_id", "afterSurfaceId"],
        );
        let index = params
            .as_ref()
            .and_then(|value| value.get("index"))
            .and_then(usize_from_value)
            .map(|value| value as usize);
        let mut state = self.inner.lock().expect("app state lock poisoned");
        let surface_id = resolve_surface_identifier(&state, &surface_id)?;
        let before_surface_id = before_surface_id
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?;
        let after_surface_id = after_surface_id
            .map(|value| resolve_surface_identifier(&state, &value))
            .transpose()?;
        let (workspace_index, surface_index) = surface_location(&state, &surface_id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        let target_pane_id = target_pane_id
            .map(|value| resolve_pane_identifier(&state, &value))
            .transpose()?
            .or_else(|| {
                before_surface_id
                    .as_ref()
                    .or(after_surface_id.as_ref())
                    .and_then(|id| surface_location(&state, id))
                    .map(|(workspace_index, surface_index)| {
                        state.workspaces[workspace_index].surfaces[surface_index]
                            .pane_id
                            .clone()
                    })
            })
            .unwrap_or_else(|| state.workspaces[workspace_index].surfaces[surface_index].pane_id.clone());
        let (target_workspace_index, target_pane_index) = pane_location(&state, &target_pane_id)
            .ok_or_else(|| invalid_params(format!("unknown pane: {target_pane_id}")))?;
        let surface = state.workspaces[workspace_index].surfaces[surface_index].clone();

        for workspace in &mut state.workspaces {
            for pane in &mut workspace.panes {
                pane.surface_ids.retain(|candidate| candidate != &surface_id);
            }
        }
        let insert_index = surface_insert_index(
            &state.workspaces[target_workspace_index].panes[target_pane_index].surface_ids,
            index,
            before_surface_id.as_deref(),
            after_surface_id.as_deref(),
        );
        state.workspaces[target_workspace_index].panes[target_pane_index]
            .surface_ids
            .insert(insert_index, surface_id.clone());
        state.workspaces[target_workspace_index].panes[target_pane_index].active_surface_id =
            Some(surface_id.clone());
        if workspace_index != target_workspace_index {
            state.workspaces[workspace_index].surfaces.remove(surface_index);
            let mut moved = surface.clone();
            moved.workspace_id = state.workspaces[target_workspace_index].id.clone();
            moved.pane_id = target_pane_id.clone();
            state.workspaces[target_workspace_index].surfaces.push(moved);
        } else if let Some(surface) = state.workspaces[workspace_index]
            .surfaces
            .iter_mut()
            .find(|surface| surface.id == surface_id)
        {
            surface.pane_id = target_pane_id.clone();
        }
        state.active_workspace_id = Some(state.workspaces[target_workspace_index].id.clone());
        state.active_pane_id = Some(target_pane_id);
        state.active_surface_id = Some(surface_id.clone());
        mark_state_changed(&mut state);
        let (workspace_index, surface_index) = surface_location(&state, &surface_id)
            .ok_or_else(|| invalid_params(format!("unknown surface: {surface_id}")))?;
        Ok(surface_result_value(
            &state,
            workspace_index,
            surface_index,
            &state.workspaces[workspace_index].surfaces[surface_index],
        ))
    }
}

pub fn default_socket_path() -> PathBuf {
    if let Some(path) = env::var_os("CMUX_SOCKET_PATH").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(runtime_dir).join("cmux").join("cmux.sock");
    }
    PathBuf::from("/tmp/cmux.sock")
}

pub fn shared_state() -> AppState {
    SHARED_STATE.get_or_init(AppState::load_or_default).clone()
}

pub fn start_background_server(socket_path: PathBuf, state: AppState) -> std::io::Result<()> {
    if SERVER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }

    prepare_socket_path(&socket_path)?;
    let listener = UnixListener::bind(&socket_path).inspect_err(|_| {
        SERVER_STARTED.store(false, Ordering::Release);
    })?;
    if let Err(error) = publish_socket_addr(&socket_path) {
        eprintln!("cmux linux socket address file disabled: {error}");
    }
    let spawn_result = thread::Builder::new()
        .name("cmux-linux-rpc".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let state = state.clone();
                        thread::spawn(move || handle_client(stream, state));
                    }
                    Err(error) => eprintln!("cmux linux socket accept failed: {error}"),
                }
            }
            SERVER_STARTED.store(false, Ordering::Release);
        });
    if spawn_result.is_err() {
        SERVER_STARTED.store(false, Ordering::Release);
    }
    spawn_result.map(|_| ())
}

fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_socket() {
            fs::remove_file(path)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("refusing to replace non-socket path {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn publish_socket_addr(path: &Path) -> std::io::Result<()> {
    let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let cmux_dir = PathBuf::from(home).join(".cmux");
    fs::create_dir_all(&cmux_dir)?;
    fs::write(cmux_dir.join("socket_addr"), path.display().to_string())
}

fn handle_client(mut stream: UnixStream, state: AppState) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            let _ = writeln!(stream, "{}", encode_error(None, -32603, &error.to_string()));
            return;
        }
    };
    let reader = BufReader::new(reader_stream);
    for line in reader.lines() {
        let response = match line {
            Ok(line) => dispatch_line(&line, &state),
            Err(error) => encode_error(None, -32603, &error.to_string()),
        };
        if writeln!(stream, "{response}").is_err() {
            break;
        }
    }
}

fn dispatch_line(line: &str, state: &AppState) -> String {
    let request: RpcRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return dispatch_v1_line(line, state),
    };

    let RpcRequest {
        jsonrpc,
        id,
        method,
        params,
    } = request;

    if jsonrpc.as_deref().unwrap_or("2.0") != "2.0" {
        return encode_error(id, -32600, "unsupported jsonrpc version");
    }

    let result = match method.as_str() {
        "ping" | "app.ping" | "system.ping" => json!({ "pong": true }),
        "app.status" => {
            let settings = crate::settings::get();
            json!({
                "platform": "linux",
                "socket": {
                    "defaultPath": default_socket_path().display().to_string()
                },
                "settings_path": crate::settings::path().map(|path| path.display().to_string()),
                "terminal_backend": format!("{:?}", settings.terminal_backend).to_ascii_lowercase(),
                "terminal_runtime": {
                    "defaultBackend": if crate::ghostty_backend::renderer_available() { "ghostty" } else { "pty" },
                    "socketControllable": true
                },
                "ghostty": crate::ghostty_backend::status(),
            })
        },
        "system.capabilities" => {
            let settings = crate::settings::get();
            let ghostty = crate::ghostty_backend::status();
            json!({
                "protocol": "cmux-socket",
                "version": 2,
                "platform": "linux",
                "app": "cmux-gtk",
                "socket_path": default_socket_path().display().to_string(),
                "settings_path": crate::settings::path().map(|path| path.display().to_string()),
                "terminal_backend": format!("{:?}", settings.terminal_backend).to_ascii_lowercase(),
                "ghostty": ghostty,
                "terminal_runtime": {
                    "defaultBackend": "pty",
                    "socketControllable": true
                },
                "access_mode": "local",
                "window_id": window_id(),
                "methods": supported_methods(),
                "socketProtocol": "jsonrpc",
                "features": {
                    "terminal": true,
                    "browser": true,
                    "ghosttyRenderer": ghostty.renderer_available,
                    "ghosttyLibrary": ghostty.library_available,
                    "notifications": true,
                    "sshRelay": true,
                    "terminalLivePty": true,
                    "browserStateAutomation": settings.browser_state_automation,
                    "browserNativeScreenshots": true,
                    "browserConsole": true,
                    "browserDialogs": true,
                    "browserDownloads": true,
                    "browserFrames": true
                }
            })
        },
        "system.identify" => state.identify(),
        "workspace.list" | "list_workspaces" => state.list_workspaces(),
        "workspace.create" | "new_workspace" => state.create_workspace(params),
        "workspace.current" => match state.current_workspace() {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "workspace.select" => match state.select_workspace(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "workspace.close" => match state.close_workspace(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "workspace.rename" => match state.rename_workspace(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "workspace.equalize_splits" => match state.workspace_equalize_splits(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.list" | "list_surfaces" | "list_panels" => {
            match state.list_surfaces(params) {
                Ok(result) => result,
                Err(error) => return encode_response(error_response(id, error)),
            }
        }
        "surface.create" | "new_surface" => match state.create_surface(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.focus" => match state.focus_surface(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.close" => match state.close_surface(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.move" | "surface.reorder" => match state.move_surface(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.drag_to_split" => match state.create_pane(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.list" => match state.list_panes(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.surfaces" => match state.pane_surfaces(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.create" | "surface.split" => match state.create_pane(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.focus" => match state.focus_pane(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.swap" => match state.pane_swap(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.resize" => match state.pane_resize(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "pane.last" => match state.pane_last() {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.open_split" => {
            let params = with_default_surface_kind(params, SurfaceKind::Browser);
            match state.create_pane(params) {
                Ok(result) => result,
                Err(error) => return encode_response(error_response(id, error)),
            }
        }
        "browser.navigate" => match state.navigate_browser(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.url.get" => match state.browser_url(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.back" => match state.browser_history_step(params, -1) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.forward" => match state.browser_history_step(params, 1) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.reload" => match state.browser_reload(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.focus_webview" => match state.browser_focus(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "browser.is_webview_focused" => match state.browser_focused(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        method if is_browser_automation_method(method) => {
            match state.browser_automation(method, params) {
                Ok(result) => result,
                Err(error) => return encode_response(error_response(id, error)),
            }
        }
        "surface.send_text" => match state.send_to_surface(params, "text") {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.send_key" => match state.send_to_surface(params, "key") {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.read_text" => match state.read_surface_text(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.clear_history" => match state.clear_surface_history(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "surface.health" => match state.surface_health(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "notification.create" => match state.create_notification(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "notification.create_for_surface" => match state.create_notification(params) {
            Ok(result) => result,
            Err(error) => return encode_response(error_response(id, error)),
        },
        "notification.list" => state.list_notifications(),
        "notification.clear" => state.clear_notifications(),
        "surface.refresh" => json!({}),
        method => {
            return encode_response(RpcResponse {
                jsonrpc: "2.0",
                ok: false,
                id,
                result: None,
                error: Some(RpcError {
                    code: -32601,
                    message: format!("method not implemented on Linux yet: {method}"),
                }),
            })
        }
    };

    encode_response(RpcResponse {
        jsonrpc: "2.0",
        ok: true,
        id,
        result: Some(result),
        error: None,
    })
}

fn dispatch_v1_line(line: &str, state: &AppState) -> String {
    let mut parts = line.split_whitespace();
    let Some(command) = parts.next() else {
        return "ERR empty command".to_string();
    };

    match command {
        "ping" => "pong".to_string(),
        "new_window" => {
            let id = state.create_workspace_with_title("Workspace");
            format!("OK window_id={id}")
        }
        "current_window" => match state.current_workspace() {
            Ok(result) => result["workspace"]["id"]
                .as_str()
                .map_or_else(|| "ERR no current window".to_string(), ToString::to_string),
            Err(error) => format!("ERR {}", error.message),
        },
        "list_windows" => {
            let summaries = state.workspace_summaries();
            if summaries.is_empty() {
                "OK".to_string()
            } else {
                summaries
                    .into_iter()
                    .map(|workspace| format!("window:{} {}", workspace.id, workspace.title))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "close_window" => {
            let workspace_id = parts.next().unwrap_or_default();
            let params = json!({ "workspace_id": workspace_id });
            match state.close_workspace(Some(params)) {
                Ok(_) => "OK".to_string(),
                Err(error) => format!("ERR {}", error.message),
            }
        }
        "focus_window" => {
            let workspace_id = parts.next().unwrap_or_default();
            let params = json!({ "workspace_id": workspace_id });
            match state.select_workspace(Some(params)) {
                Ok(_) => "OK".to_string(),
                Err(error) => format!("ERR {}", error.message),
            }
        }
        command => format!("ERR method not implemented on Linux yet: {command}"),
    }
}

fn encode_error(id: Option<Value>, code: i64, message: &str) -> String {
    encode_response(RpcResponse {
        jsonrpc: "2.0",
        ok: false,
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.to_string(),
        }),
    })
}

fn encode_response(response: RpcResponse) -> String {
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialization failed"}}"#
            .to_string()
    })
}

fn error_response(id: Option<Value>, error: RpcError) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        ok: false,
        id,
        result: None,
        error: Some(error),
    }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError {
        code: -32602,
        message: format!("invalid_params: {}", message.into()),
    }
}

fn default_state() -> State {
    let workspace_id = next_workspace_id();
    let pane_id = next_pane_id();
    let surface_id = next_surface_id();
    State {
        revision: 1,
        active_workspace_id: Some(workspace_id.clone()),
        active_pane_id: Some(pane_id.clone()),
        active_surface_id: Some(surface_id.clone()),
        notifications: Vec::new(),
        workspaces: vec![WorkspaceRecord {
            id: workspace_id.clone(),
            title: "Default".to_string(),
            layout: PaneNode::Leaf {
                pane_id: pane_id.clone(),
            },
            panes: vec![PaneRecord {
                id: pane_id.clone(),
                workspace_id: workspace_id.clone(),
                surface_ids: vec![surface_id.clone()],
                active_surface_id: Some(surface_id.clone()),
            }],
            surfaces: vec![SurfaceRecord {
                id: surface_id,
                workspace_id,
                pane_id,
                title: "Terminal".to_string(),
                kind: SurfaceKind::Terminal,
                initial_command: None,
                working_directory: None,
                url: None,
                history: Vec::new(),
                history_index: None,
                input_log: Vec::new(),
                browser_state: BrowserState::default(),
            }],
        }],
    }
}

fn repair_state(mut state: State) -> State {
    if state.workspaces.is_empty() {
        return default_state();
    }
    for workspace in &mut state.workspaces {
        if workspace.panes.is_empty() {
            let pane_id = next_pane_id();
            workspace.layout = PaneNode::Leaf {
                pane_id: pane_id.clone(),
            };
            workspace.panes.push(PaneRecord {
                id: pane_id,
                workspace_id: workspace.id.clone(),
                surface_ids: Vec::new(),
                active_surface_id: None,
            });
        }
        let pane_ids: Vec<String> = workspace.panes.iter().map(|pane| pane.id.clone()).collect();
        if let Some(layout) = prune_layout(workspace.layout.clone(), &pane_ids) {
            workspace.layout = layout;
        } else if let Some(pane) = workspace.panes.first() {
            workspace.layout = PaneNode::Leaf {
                pane_id: pane.id.clone(),
            };
        }
    }
    if state.active_workspace_id.as_ref().map_or(true, |id| {
        !state.workspaces.iter().any(|workspace| workspace.id == *id)
    }) {
        state.active_workspace_id = state.workspaces.first().map(|workspace| workspace.id.clone());
    }
    if state
        .active_pane_id
        .as_ref()
        .map_or(true, |id| pane_location(&state, id).is_none())
    {
        state.active_pane_id = current_workspace_record(&state)
            .and_then(|workspace| workspace.panes.first())
            .map(|pane| pane.id.clone());
    }
    if state
        .active_surface_id
        .as_ref()
        .map_or(true, |id| surface_location(&state, id).is_none())
    {
        state.active_surface_id = state
            .active_pane_id
            .as_ref()
            .and_then(|pane_id| pane_location(&state, pane_id))
            .and_then(|(workspace_index, pane_index)| {
                state.workspaces[workspace_index].panes[pane_index]
                    .active_surface_id
                    .clone()
            })
            .or_else(|| {
                current_workspace_record(&state)
                    .and_then(|workspace| workspace.surfaces.first())
                    .map(|surface| surface.id.clone())
            });
    }
    state.revision = state.revision.saturating_add(1);
    state
}

fn mark_state_changed(state: &mut State) {
    state.revision = state.revision.saturating_add(1);
    persist_state(state);
}

fn load_persisted_state() -> Option<State> {
    let path = session_state_path()?;
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn persist_state(state: &State) {
    let Some(path) = session_state_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(data) = serde_json::to_string_pretty(state) else {
        return;
    };
    let _ = fs::write(path, data);
}

fn session_state_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("CMUX_SESSION_STATE_PATH").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(state_home) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(state_home).join("cmux").join("session-linux.json"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("cmux")
                .join("session-linux.json")
        })
}

fn not_supported(message: impl Into<String>) -> RpcError {
    RpcError {
        code: -32004,
        message: message.into(),
    }
}

fn browser_not_found(message: impl Into<String>) -> RpcError {
    RpcError {
        code: -32004,
        message: format!(
            "not_found: {}; snapshot available; hint: verify selector and target surface",
            message.into()
        ),
    }
}

fn browser_timeout(message: impl Into<String>) -> RpcError {
    RpcError {
        code: -32002,
        message: format!("timeout: {}", message.into()),
    }
}

fn window_id() -> String {
    WINDOW_ID.get_or_init(|| next_uuid("window")).clone()
}

fn current_workspace_record(state: &State) -> Option<&WorkspaceRecord> {
    state
        .active_workspace_id
        .as_ref()
        .and_then(|id| state.workspaces.iter().find(|workspace| &workspace.id == id))
        .or_else(|| state.workspaces.first())
}

fn workspace_detail(state: &State, workspace: &WorkspaceRecord) -> WorkspaceDetail {
    WorkspaceDetail {
        id: workspace.id.clone(),
        title: workspace.title.clone(),
        layout: pane_layout_detail(state, workspace, &workspace.layout)
            .unwrap_or_else(|| fallback_pane_layout_detail(state, workspace)),
    }
}

fn fallback_pane_layout_detail(state: &State, workspace: &WorkspaceRecord) -> PaneLayoutDetail {
    let mut panes = workspace
        .panes
        .iter()
        .filter_map(|pane| pane_detail(state, workspace, pane))
        .map(PaneLayoutDetail::Leaf);
    let Some(first) = panes.next() else {
        return PaneLayoutDetail::Leaf(PaneDetail {
            id: String::new(),
            selected_surface_id: None,
            surfaces: Vec::new(),
        });
    };
    panes.fold(first, |acc, next| PaneLayoutDetail::Split {
        direction: "horizontal".to_string(),
        ratio: 0.5,
        first: Box::new(acc),
        second: Box::new(next),
    })
}

fn pane_layout_detail(
    state: &State,
    workspace: &WorkspaceRecord,
    node: &PaneNode,
) -> Option<PaneLayoutDetail> {
    match node {
        PaneNode::Leaf { pane_id } => workspace
            .panes
            .iter()
            .find(|pane| pane.id == *pane_id)
            .and_then(|pane| pane_detail(state, workspace, pane))
            .map(PaneLayoutDetail::Leaf),
        PaneNode::Split {
            direction,
            ratio,
            first,
            second,
        } => Some(PaneLayoutDetail::Split {
            direction: split_direction_name(*direction).to_string(),
            ratio: *ratio,
            first: Box::new(pane_layout_detail(state, workspace, first)?),
            second: Box::new(pane_layout_detail(state, workspace, second)?),
        }),
    }
}

fn pane_detail(
    state: &State,
    workspace: &WorkspaceRecord,
    pane: &PaneRecord,
) -> Option<PaneDetail> {
    let surfaces = pane
        .surface_ids
        .iter()
        .filter_map(|surface_id| workspace.surfaces.iter().find(|surface| &surface.id == surface_id))
        .map(|surface| SurfaceDetail {
            id: surface.id.clone(),
            title: surface.title.clone(),
            kind: surface_kind_name(&surface.kind).to_string(),
            url: surface.url.clone(),
            initial_command: surface.initial_command.clone(),
            working_directory: surface.working_directory.clone(),
            focused: state.active_surface_id.as_deref() == Some(surface.id.as_str()),
        })
        .collect();
    Some(PaneDetail {
        id: pane.id.clone(),
        selected_surface_id: pane.active_surface_id.clone(),
        surfaces,
    })
}

fn workspace_summary_value(state: &State, index: usize, workspace: &WorkspaceRecord) -> Value {
    json!({
        "id": workspace.id,
        "workspace_id": workspace.id,
        "window_id": window_id(),
        "ref": format!("workspace:{}", index + 1),
        "index": index + 1,
        "title": workspace.title,
        "selected": state.active_workspace_id.as_deref() == Some(workspace.id.as_str()),
        "pane_count": workspace.panes.len(),
        "surface_count": workspace.surfaces.len(),
    })
}

fn workspace_result_value(state: &State, index: usize, workspace: &WorkspaceRecord) -> Value {
    json!({
        "workspace_id": workspace.id,
        "workspace_ref": format!("workspace:{}", index + 1),
        "window_id": window_id(),
        "workspace": workspace_summary_value(state, index, workspace),
    })
}

fn pane_summary_value(
    state: &State,
    workspace: &WorkspaceRecord,
    index: usize,
    pane: &PaneRecord,
) -> Value {
    json!({
        "id": pane.id,
        "pane_id": pane.id,
        "workspace_id": workspace.id,
        "window_id": window_id(),
        "ref": format!("pane:{}", index + 1),
        "index": index + 1,
        "surface_count": pane.surface_ids.len(),
        "focused": state.active_pane_id.as_deref() == Some(pane.id.as_str()),
        "selected_surface_id": pane.active_surface_id,
    })
}

fn surface_summary_value(
    state: &State,
    workspace: &WorkspaceRecord,
    index: usize,
    surface: &SurfaceRecord,
) -> Value {
    json!({
        "id": surface.id,
        "surface_id": surface.id,
        "panel_id": surface.id,
        "pane_id": surface.pane_id,
        "workspace_id": workspace.id,
        "window_id": window_id(),
        "ref": format!("surface:{}", index + 1),
        "index": index + 1,
        "title": surface.title,
        "kind": surface.kind,
        "type": surface_kind_name(&surface.kind),
        "url": surface.url,
        "initial_command": surface.initial_command,
        "working_directory": surface.working_directory,
        "focused": state.active_surface_id.as_deref() == Some(surface.id.as_str()),
        "selected": workspace
            .panes
            .iter()
            .find(|pane| pane.id == surface.pane_id)
            .and_then(|pane| pane.active_surface_id.as_deref())
            == Some(surface.id.as_str()),
    })
}

fn surface_result_value(
    state: &State,
    workspace_index: usize,
    surface_index: usize,
    surface: &SurfaceRecord,
) -> Value {
    let workspace = &state.workspaces[workspace_index];
    json!({
        "workspace_id": surface.workspace_id,
        "pane_id": surface.pane_id,
        "surface_id": surface.id,
        "panel_id": surface.id,
        "surface_ref": format!("surface:{}", surface_index + 1),
        "pane_ref": state
            .workspaces
            .get(workspace_index)
            .and_then(|workspace| workspace.panes.iter().position(|pane| pane.id == surface.pane_id))
            .map(|index| format!("pane:{}", index + 1))
            .unwrap_or_else(|| "pane:1".to_string()),
        "window_id": window_id(),
        "workspace": workspace_summary_value(state, workspace_index, workspace),
        "surface": surface_summary_value(state, workspace, surface_index, surface),
    })
}

fn find_workspace_index(state: &State, identifier: &str) -> Option<usize> {
    if let Some(index) = ref_index(identifier, "workspace") {
        return state.workspaces.get(index).map(|_| index);
    }
    state
        .workspaces
        .iter()
        .position(|workspace| workspace.id == identifier)
}

fn pane_location(state: &State, identifier: &str) -> Option<(usize, usize)> {
    if let Some(index) = ref_index(identifier, "pane") {
        if let Some(workspace) = current_workspace_record(state) {
            let workspace_index = state
                .workspaces
                .iter()
                .position(|candidate| candidate.id == workspace.id)?;
            if workspace.panes.get(index).is_some() {
                return Some((workspace_index, index));
            }
        }
    }
    state.workspaces.iter().enumerate().find_map(|(workspace_index, workspace)| {
        workspace
            .panes
            .iter()
            .position(|pane| pane.id == identifier)
            .map(|pane_index| (workspace_index, pane_index))
    })
}

fn surface_location(state: &State, identifier: &str) -> Option<(usize, usize)> {
    if let Some(index) = ref_index(identifier, "surface") {
        if let Some(workspace) = current_workspace_record(state) {
            let workspace_index = state
                .workspaces
                .iter()
                .position(|candidate| candidate.id == workspace.id)?;
            if workspace.surfaces.get(index).is_some() {
                return Some((workspace_index, index));
            }
        }
    }
    state.workspaces.iter().enumerate().find_map(|(workspace_index, workspace)| {
        workspace
            .surfaces
            .iter()
            .position(|surface| surface.id == identifier)
            .map(|surface_index| (workspace_index, surface_index))
    })
}

fn resolve_workspace_identifier(state: &State, identifier: &str) -> Result<String, RpcError> {
    find_workspace_index(state, identifier)
        .map(|index| state.workspaces[index].id.clone())
        .ok_or_else(|| invalid_params(format!("unknown workspace: {identifier}")))
}

fn resolve_pane_identifier(state: &State, identifier: &str) -> Result<String, RpcError> {
    pane_location(state, identifier)
        .map(|(workspace_index, pane_index)| state.workspaces[workspace_index].panes[pane_index].id.clone())
        .ok_or_else(|| invalid_params(format!("unknown pane: {identifier}")))
}

fn resolve_surface_identifier(state: &State, identifier: &str) -> Result<String, RpcError> {
    surface_location(state, identifier)
        .map(|(workspace_index, surface_index)| state.workspaces[workspace_index].surfaces[surface_index].id.clone())
        .ok_or_else(|| invalid_params(format!("unknown surface: {identifier}")))
}

fn ref_index(identifier: &str, prefix: &str) -> Option<usize> {
    let value = identifier.strip_prefix(prefix)?.strip_prefix(':')?;
    value.parse::<usize>().ok()?.checked_sub(1)
}

fn ensure_browser_surface(surface: &SurfaceRecord) -> Result<(), RpcError> {
    if matches!(surface.kind, SurfaceKind::Browser) {
        Ok(())
    } else {
        Err(browser_not_found(format!(
            "surface {} is not a browser surface",
            surface.id
        )))
    }
}

fn surface_kind_name(kind: &SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Terminal => "terminal",
        SurfaceKind::Browser => "browser",
        SurfaceKind::Markdown => "markdown",
    }
}

fn split_direction_name(direction: SplitDirection) -> &'static str {
    match direction {
        SplitDirection::Horizontal => "horizontal",
        SplitDirection::Vertical => "vertical",
    }
}

fn split_direction_from_params(params: Option<&Value>) -> Result<SplitDirection, RpcError> {
    let raw = params
        .and_then(|value| value.get("direction"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("right");
    match raw {
        "left" | "right" | "horizontal" => Ok(SplitDirection::Horizontal),
        "up" | "down" | "top" | "bottom" | "vertical" => Ok(SplitDirection::Vertical),
        _ => Err(invalid_params(format!("unknown split direction: {raw}"))),
    }
}

fn split_insert_before(params: Option<&Value>) -> bool {
    matches!(
        params
            .and_then(|value| value.get("direction"))
            .and_then(Value::as_str)
            .map(str::trim),
        Some("left" | "up" | "top")
    )
}

fn insert_pane_in_layout(
    layout: &mut PaneNode,
    target_pane_id: Option<&str>,
    new_pane_id: String,
    direction: SplitDirection,
    insert_before: bool,
) {
    if let Some(target_pane_id) = target_pane_id {
        if split_layout_at_pane(layout, target_pane_id, new_pane_id.clone(), direction, insert_before)
        {
            return;
        }
    }
    let previous = std::mem::replace(
        layout,
        PaneNode::Leaf {
            pane_id: new_pane_id.clone(),
        },
    );
    *layout = PaneNode::Split {
        direction,
        ratio: 0.5,
        first: Box::new(previous),
        second: Box::new(PaneNode::Leaf { pane_id: new_pane_id }),
    };
}

fn split_layout_at_pane(
    node: &mut PaneNode,
    target_pane_id: &str,
    new_pane_id: String,
    direction: SplitDirection,
    insert_before: bool,
) -> bool {
    match node {
        PaneNode::Leaf { pane_id } if pane_id == target_pane_id => {
            let existing = std::mem::replace(
                node,
                PaneNode::Leaf {
                    pane_id: new_pane_id.clone(),
                },
            );
            let new_leaf = PaneNode::Leaf { pane_id: new_pane_id };
            let (first, second) = if insert_before {
                (new_leaf, existing)
            } else {
                (existing, new_leaf)
            };
            *node = PaneNode::Split {
                direction,
                ratio: 0.5,
                first: Box::new(first),
                second: Box::new(second),
            };
            true
        }
        PaneNode::Leaf { .. } => false,
        PaneNode::Split { first, second, .. } => {
            split_layout_at_pane(first, target_pane_id, new_pane_id.clone(), direction, insert_before)
                || split_layout_at_pane(second, target_pane_id, new_pane_id, direction, insert_before)
        }
    }
}

fn prune_layout(node: PaneNode, pane_ids: &[String]) -> Option<PaneNode> {
    match node {
        PaneNode::Leaf { pane_id } => pane_ids
            .iter()
            .any(|candidate| candidate == &pane_id)
            .then_some(PaneNode::Leaf { pane_id }),
        PaneNode::Split {
            direction,
            ratio,
            first,
            second,
        } => match (prune_layout(*first, pane_ids), prune_layout(*second, pane_ids)) {
            (Some(first), Some(second)) => Some(PaneNode::Split {
                direction,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

fn swap_panes_in_layout(node: &mut PaneNode, first_id: &str, second_id: &str) {
    match node {
        PaneNode::Leaf { pane_id } if pane_id == first_id => {
            *pane_id = second_id.to_string();
        }
        PaneNode::Leaf { pane_id } if pane_id == second_id => {
            *pane_id = first_id.to_string();
        }
        PaneNode::Leaf { .. } => {}
        PaneNode::Split { first, second, .. } => {
            swap_panes_in_layout(first, first_id, second_id);
            swap_panes_in_layout(second, first_id, second_id);
        }
    }
}

fn resize_split_for_pane(node: &mut PaneNode, pane_id: &str, ratio: f64) -> bool {
    match node {
        PaneNode::Leaf { .. } => false,
        PaneNode::Split {
            ratio: split_ratio,
            first,
            second,
            ..
        } if layout_contains_pane(first, pane_id) || layout_contains_pane(second, pane_id) => {
            *split_ratio = ratio;
            true
        }
        PaneNode::Split { first, second, .. } => {
            resize_split_for_pane(first, pane_id, ratio)
                || resize_split_for_pane(second, pane_id, ratio)
        }
    }
}

fn layout_contains_pane(node: &PaneNode, pane_id: &str) -> bool {
    match node {
        PaneNode::Leaf { pane_id: candidate } => candidate == pane_id,
        PaneNode::Split { first, second, .. } => {
            layout_contains_pane(first, pane_id) || layout_contains_pane(second, pane_id)
        }
    }
}

fn equalize_layout(node: &mut PaneNode) {
    match node {
        PaneNode::Leaf { .. } => {}
        PaneNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            *ratio = 0.5;
            equalize_layout(first);
            equalize_layout(second);
        }
    }
}

fn surface_insert_index(
    surface_ids: &[String],
    explicit_index: Option<usize>,
    before_surface_id: Option<&str>,
    after_surface_id: Option<&str>,
) -> usize {
    if let Some(index) = explicit_index {
        return index.saturating_sub(1).min(surface_ids.len());
    }
    if let Some(before_surface_id) = before_surface_id {
        return surface_ids
            .iter()
            .position(|candidate| candidate == before_surface_id)
            .unwrap_or(surface_ids.len());
    }
    if let Some(after_surface_id) = after_surface_id {
        return surface_ids
            .iter()
            .position(|candidate| candidate == after_surface_id)
            .map(|index| index + 1)
            .unwrap_or(surface_ids.len());
    }
    surface_ids.len()
}

fn supported_methods() -> Vec<&'static str> {
    let mut methods = vec![
        "system.ping",
        "system.capabilities",
        "system.identify",
        "app.status",
        "workspace.list",
        "workspace.create",
        "workspace.current",
        "workspace.select",
        "workspace.close",
        "workspace.rename",
        "workspace.equalize_splits",
        "surface.list",
        "surface.create",
        "surface.focus",
        "surface.close",
        "surface.split",
        "surface.move",
        "surface.reorder",
        "surface.drag_to_split",
        "surface.send_text",
        "surface.send_key",
        "surface.read_text",
        "surface.clear_history",
        "surface.health",
        "surface.refresh",
        "pane.list",
        "pane.surfaces",
        "pane.create",
        "pane.focus",
        "pane.swap",
        "pane.resize",
        "pane.last",
        "browser.open_split",
        "browser.navigate",
        "browser.back",
        "browser.forward",
        "browser.reload",
        "browser.url.get",
        "browser.focus_webview",
        "browser.is_webview_focused",
        "notification.create",
        "notification.create_for_surface",
        "notification.list",
        "notification.clear",
    ];
    methods.extend(BROWSER_AUTOMATION_METHODS);
    methods.sort_unstable();
    methods
}

const BROWSER_AUTOMATION_METHODS: &[&str] = &[
    "browser.snapshot",
    "browser.eval",
    "browser.wait",
    "browser.click",
    "browser.dblclick",
    "browser.type",
    "browser.fill",
    "browser.press",
    "browser.keydown",
    "browser.keyup",
    "browser.hover",
    "browser.focus",
    "browser.check",
    "browser.uncheck",
    "browser.select",
    "browser.scroll",
    "browser.scroll_into_view",
    "browser.screenshot",
    "browser.get.title",
    "browser.get.text",
    "browser.get.html",
    "browser.get.value",
    "browser.get.attr",
    "browser.get.count",
    "browser.get.box",
    "browser.get.styles",
    "browser.is.visible",
    "browser.is.enabled",
    "browser.is.checked",
    "browser.find.role",
    "browser.find.text",
    "browser.find.label",
    "browser.find.placeholder",
    "browser.find.alt",
    "browser.find.title",
    "browser.find.testid",
    "browser.find.nth",
    "browser.find.first",
    "browser.find.last",
    "browser.frame.select",
    "browser.frame.main",
    "browser.dialog.respond",
    "browser.download.wait",
    "browser.console.list",
    "browser.errors.list",
    "browser.highlight",
    "browser.state.save",
    "browser.state.load",
];

fn is_browser_automation_method(method: &str) -> bool {
    BROWSER_AUTOMATION_METHODS.contains(&method)
}

fn optional_string_param(params: Option<&Value>, key: &str) -> Option<String> {
    params?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn optional_string_any(params: Option<&Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| optional_string_param(params, key))
}

fn required_string_any(params: Option<&Value>, keys: &[&str]) -> Result<String, RpcError> {
    optional_string_any(params, keys)
        .ok_or_else(|| invalid_params(format!("{} is required", keys.first().copied().unwrap_or("value"))))
}

fn number_from_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

fn usize_from_value(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .map(|value| value as usize)
        .or_else(|| value.as_str()?.trim().parse::<usize>().ok())
}

fn bool_from_value(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        match value.as_str()?.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

fn default_surface_title(kind: &SurfaceKind, url: Option<&str>) -> String {
    match (kind, url) {
        (SurfaceKind::Browser, Some(url)) => url.to_string(),
        (SurfaceKind::Browser, None) => "Browser".to_string(),
        (SurfaceKind::Markdown, _) => "Markdown".to_string(),
        (SurfaceKind::Terminal, _) => "Terminal".to_string(),
    }
}

fn initialize_browser_state(surface: &mut SurfaceRecord) {
    if matches!(surface.kind, SurfaceKind::Browser) {
        if let Some(url) = surface.url.as_deref() {
            surface.browser_state = browser_state_for_url(url);
            if !surface.browser_state.title.is_empty() {
                surface.title = surface.browser_state.title.clone();
            }
        }
    }
}

fn browser_state_for_url(url: &str) -> BrowserState {
    let html = html_from_url(url).unwrap_or_default();
    let mut state = BrowserState {
        html: html.clone(),
        title: extract_between(&html, "<title", "</title>")
            .and_then(|chunk| chunk.split_once('>').map(|(_, value)| value.to_string()))
            .unwrap_or_default(),
        ..BrowserState::default()
    };
    parse_browser_elements(&mut state);
    state
}

fn html_from_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("data:text/html")?;
    let (_, encoded) = rest.split_once(',')?;
    Some(percent_decode(encoded.replace('+', " ").as_str()))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                output.push(value);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).to_string()
}

fn parse_browser_elements(state: &mut BrowserState) {
    let html = state.html.clone();
    let mut cursor = 0;
    while let Some(open_offset) = html[cursor..].find('<') {
        let open = cursor + open_offset;
        let Some(close_offset) = html[open..].find('>') else {
            break;
        };
        let close = open + close_offset;
        let tag_content = &html[open + 1..close];
        cursor = close + 1;
        if tag_content.starts_with('/') || tag_content.starts_with('!') || tag_content.starts_with("script") || tag_content.starts_with("style") {
            continue;
        }
        let tag = tag_content
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches('/')
            .to_ascii_lowercase();
        if tag.is_empty() {
            continue;
        }
        let attrs = parse_attrs(tag_content);
        let Some(id) = attrs.get("id").cloned() else {
            continue;
        };
        let text = element_inner_text(&html, cursor, &tag).unwrap_or_default();
        let element_html = element_outer_html(&html, open, cursor, &tag).unwrap_or_else(|| html[open..=close].to_string());
        let value = attrs.get("value").cloned().unwrap_or_default();
        let style = attrs.get("style").cloned().unwrap_or_default();
        let visible = !style.contains("display: none")
            && !html.contains(&format!("#{id} {{ display: none"))
            && id != "hidden";
        state.elements.insert(
            id.clone(),
            BrowserElement {
                id,
                tag,
                text,
                html: element_html,
                value,
                checked: attrs.contains_key("checked"),
                disabled: attrs.contains_key("disabled"),
                visible,
                scroll_top: 0,
                attrs,
            },
        );
    }
}

fn parse_attrs(tag_content: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    for quote in ['"', '\''] {
        let mut rest = tag_content;
        while let Some(eq) = rest.find('=') {
            let key = rest[..eq]
                .split_whitespace()
                .last()
                .unwrap_or_default()
                .trim_matches('/')
                .to_ascii_lowercase();
            let after_eq = &rest[eq + 1..];
            if !after_eq.starts_with(quote) {
                rest = after_eq;
                continue;
            }
            let Some(end) = after_eq[1..].find(quote) else {
                break;
            };
            attrs.insert(key, after_eq[1..1 + end].to_string());
            rest = &after_eq[2 + end..];
        }
    }
    for bare in ["disabled", "checked"] {
        if tag_content.contains(bare) {
            attrs.entry(bare.to_string()).or_default();
        }
    }
    attrs
}

fn element_inner_text(html: &str, cursor: usize, tag: &str) -> Option<String> {
    let close_tag = format!("</{tag}>");
    let end = html[cursor..].find(&close_tag)?;
    Some(strip_tags(&html[cursor..cursor + end]).trim().to_string())
}

fn element_outer_html(html: &str, open: usize, cursor: usize, tag: &str) -> Option<String> {
    let close_tag = format!("</{tag}>");
    let end = html[cursor..].find(&close_tag)?;
    Some(html[open..cursor + end + close_tag.len()].to_string())
}

fn strip_tags(input: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn extract_between(input: &str, start: &str, end: &str) -> Option<String> {
    let start_index = input.find(start)?;
    let end_index = input[start_index..].find(end)?;
    Some(input[start_index..start_index + end_index].to_string())
}

fn browser_element_id(state: &BrowserState, selector: &str) -> Result<String, RpcError> {
    if let Some(id) = selector.strip_prefix('#') {
        if state.elements.contains_key(id) {
            return Ok(id.to_string());
        }
    }
    state
        .elements
        .values()
        .find(|element| element.tag == selector)
        .map(|element| element.id.clone())
        .ok_or_else(|| browser_not_found(format!("selector not found: {selector}")))
}

fn ensure_browser_element(state: &BrowserState, selector: &str) -> Result<(), RpcError> {
    browser_element_id(state, selector).map(|_| ())
}

fn browser_element_mut<'a>(
    state: &'a mut BrowserState,
    selector: &str,
) -> Result<&'a mut BrowserElement, RpcError> {
    let id = browser_element_id(state, selector)?;
    state
        .elements
        .get_mut(&id)
        .ok_or_else(|| browser_not_found(format!("selector not found: {selector}")))
}

fn browser_element<'a>(
    state: &'a BrowserState,
    selector: &str,
) -> Result<&'a BrowserElement, RpcError> {
    let id = browser_element_id(state, selector)?;
    state
        .elements
        .get(&id)
        .ok_or_else(|| browser_not_found(format!("selector not found: {selector}")))
}

fn browser_action_result(surface: &SurfaceRecord, params: Option<&Value>) -> Value {
    browser_action_result_with_live(surface, params, false)
}

fn browser_action_result_with_live(
    surface: &SurfaceRecord,
    params: Option<&Value>,
    live_webview: bool,
) -> Value {
    let mut result = json!({ "surface_id": surface.id, "ok": true });
    if let Some(object) = result.as_object_mut() {
        object.insert("live_webview".to_string(), Value::Bool(live_webview));
    }
    if params
        .and_then(|value| value.get("snapshot_after"))
        .and_then(bool_from_value)
        .unwrap_or(false)
    {
        if let Some(object) = result.as_object_mut() {
            object.insert("post_action_snapshot".to_string(), browser_snapshot(surface));
        }
    }
    result
}

fn browser_snapshot(surface: &SurfaceRecord) -> Value {
    if let Some(snapshot) = live_browser_snapshot(surface) {
        return snapshot;
    }

    let mut refs = serde_json::Map::new();
    let mut lines = Vec::new();
    if !surface.browser_state.title.is_empty() {
        lines.push(surface.browser_state.title.clone());
    }
    for (index, element) in surface.browser_state.elements.values().enumerate() {
        let reference = format!("e{}", index + 1);
        refs.insert(
            reference.clone(),
            json!({
                "selector": format!("#{}", element.id),
                "tag": element.tag,
                "text": element.text,
            }),
        );
        let label = if element.text.is_empty() {
            element.value.as_str()
        } else {
            element.text.as_str()
        };
        lines.push(format!("{reference} <{} id=\"{}\"> {}", element.tag, element.id, label));
    }
    json!({
        "surface_id": surface.id,
        "snapshot": lines.join("\n"),
        "refs": refs,
        "live_webview": false,
    })
}

fn browser_screenshot(
    surface: &SurfaceRecord,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    let full_page = params
        .and_then(|value| value.get("full_page").or_else(|| value.get("fullPage")))
        .and_then(bool_from_value)
        .unwrap_or(false);
    let timeout = browser_timeout_duration(params, 5_000);
    if let Some(result) = browser::screenshot(&surface.id, full_page, timeout) {
        match result {
            Ok(screenshot) => {
                return Ok(json!({
                    "surface_id": surface.id,
                    "png_base64": screenshot.png_base64,
                    "width": screenshot.width,
                    "height": screenshot.height,
                    "full_page": screenshot.full_page,
                    "live_webview": true,
                }));
            }
            Err(error) => return Err(browser_timeout(error)),
        }
    }
    Ok(json!({
        "surface_id": surface.id,
        "png_base64": STATIC_SCREENSHOT_PNG_BASE64,
        "live_webview": false,
    }))
}

fn browser_dialog_respond(
    surface: &SurfaceRecord,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    let action = optional_string_any(params, &["action", "response"])
        .unwrap_or_else(|| "accept".to_string())
        .to_ascii_lowercase();
    let accept = match action.as_str() {
        "accept" | "ok" | "confirm" | "yes" => true,
        "dismiss" | "cancel" | "deny" | "no" => false,
        _ => return Err(invalid_params("action must be accept or dismiss")),
    };
    let prompt_text = optional_string_any(params, &["prompt_text", "promptText", "text", "value"]);
    match browser::respond_to_dialog(&surface.id, accept, prompt_text) {
        Ok(dialog) => Ok(json!({
            "surface_id": surface.id,
            "ok": true,
            "dialog": dialog,
            "accepted": accept,
            "live_webview": true,
        })),
        Err(error) if error.contains("no pending") => Err(browser_not_found(error)),
        Err(error) => Err(not_supported(error)),
    }
}

fn browser_download_wait(
    surface: &SurfaceRecord,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    let timeout = browser_timeout_duration(params, 30_000);
    match browser::wait_for_download(&surface.id, timeout) {
        Ok(download) => Ok(json!({
            "surface_id": surface.id,
            "download": download,
            "live_webview": true,
        })),
        Err(error) if error.contains("timed out") => Err(browser_timeout(error)),
        Err(error) => Err(not_supported(error)),
    }
}

fn browser_timeout_duration(params: Option<&Value>, default_ms: u64) -> std::time::Duration {
    let timeout_ms = params
        .and_then(|value| {
            value
                .get("timeout_ms")
                .or_else(|| value.get("timeoutMs"))
                .or_else(|| value.get("timeout"))
        })
        .and_then(number_from_value)
        .map(|value| value.max(0.0) as u64)
        .unwrap_or(default_ms);
    std::time::Duration::from_millis(timeout_ms)
}

fn live_browser_snapshot(surface: &SurfaceRecord) -> Option<Value> {
    let selector_fn = live_selector_function();
    let script = format!(
        "(() => {{ {selector_fn} const refs = {{}}; const lines = []; if (document.title) lines.push(document.title); const nodes = Array.from(document.querySelectorAll('a,button,input,textarea,select,[role],h1,h2,h3,p,label')).filter((el) => {{ const style = getComputedStyle(el); const rect = el.getBoundingClientRect(); return style.display !== 'none' && style.visibility !== 'hidden' && rect.width >= 0 && rect.height >= 0; }}).slice(0, 100); nodes.forEach((el, index) => {{ const ref = `e${{index + 1}}`; const tag = el.tagName.toLowerCase(); const label = (el.innerText || el.textContent || el.value || el.getAttribute('aria-label') || el.getAttribute('placeholder') || '').trim().replace(/\\s+/g, ' '); refs[ref] = {{ selector: cmuxSelector(el), tag, text: label }}; lines.push(`${{ref}} <${{tag}}> ${{label}}`.trim()); }}); return {{ snapshot: lines.join('\\n'), refs }}; }})()"
    );
    let mut value = eval_live_browser_script(&surface.id, &script)?;
    let object = value.as_object_mut()?;
    object.insert("surface_id".to_string(), Value::String(surface.id.clone()));
    object.insert("live_webview".to_string(), Value::Bool(true));
    Some(value)
}

fn browser_wait(surface: &SurfaceRecord, params: Option<&Value>) -> Result<Value, RpcError> {
    let timeout = browser_timeout_duration(params, 5_000);
    let deadline = std::time::Instant::now() + timeout;
    let last_error = loop {
        match browser_wait_once(surface, params) {
            Ok(live_webview) => {
                return Ok(json!({
                    "surface_id": surface.id,
                    "ok": true,
                    "live_webview": live_webview
                }));
            }
            Err(error) => {
                if std::time::Instant::now() >= deadline {
                    break error;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    Err(browser_timeout(last_error))
}

fn browser_wait_once(surface: &SurfaceRecord, params: Option<&Value>) -> Result<bool, String> {
    let mut checked_any = false;
    let mut live_webview = false;
    if let Some(selector) = optional_string_any(params, &["selector"]) {
        checked_any = true;
        let selector_js = js_string_literal(&selector);
        if let Some(Value::Bool(true)) =
            eval_live_browser_script(&surface.id, &format!("document.querySelector({selector_js}) !== null"))
        {
            live_webview = true;
        } else {
            ensure_browser_element(&surface.browser_state, &selector)
                .map_err(|_| format!("selector not found: {selector}"))?;
        }
    }
    if let Some(text) = optional_string_any(params, &["text_contains", "textContains", "text"]) {
        checked_any = true;
        let text_js = js_string_literal(&text);
        if let Some(Value::Bool(true)) = eval_live_browser_script(
            &surface.id,
            &format!("document.body && document.body.innerText.includes({text_js})"),
        ) {
            live_webview = true;
        } else if !browser_inner_text(&surface.browser_state).contains(&text) {
            return Err(format!("text not found: {text}"));
        }
    }
    if let Some(script) = optional_string_any(params, &["function", "script"]) {
        checked_any = true;
        if let Some(Value::Bool(true)) = eval_live_browser_script(&surface.id, &script) {
            live_webview = true;
        } else if !eval_browser_script(&surface.browser_state, &script).as_bool().unwrap_or(false) {
            return Err("function did not become true".to_string());
        }
    }
    if let Some(load_state) = optional_string_any(params, &["load_state", "loadState"]) {
        checked_any = true;
        if load_state != "complete" && load_state != "domcontentloaded" && load_state != "load" {
            return Err(format!("unsupported load state: {load_state}"));
        }
        let ready_script = match load_state.as_str() {
            "complete" | "load" => "document.readyState === 'complete'",
            "domcontentloaded" => {
                "document.readyState === 'interactive' || document.readyState === 'complete'"
            }
            _ => unreachable!("load_state was validated above"),
        };
        if let Some(Value::Bool(true)) = eval_live_browser_script(&surface.id, ready_script) {
            live_webview = true;
        }
    }
    if let Some(url_contains) = optional_string_any(params, &["url_contains", "urlContains"]) {
        checked_any = true;
        let url_js = js_string_literal(&url_contains);
        if let Some(Value::Bool(true)) =
            eval_live_browser_script(&surface.id, &format!("location.href.includes({url_js})"))
        {
            live_webview = true;
        } else if !surface.url.as_deref().unwrap_or_default().contains(&url_contains) {
            return Err(format!("url does not contain: {url_contains}"));
        }
    }
    if checked_any {
        Ok(live_webview)
    } else {
        Ok(browser::is_registered(&surface.id))
    }
}

fn browser_get(
    surface: &SurfaceRecord,
    method: &str,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    let selector = optional_string_any(params, &["selector"]);
    let state = &surface.browser_state;
    match method {
        "browser.get.title" => {
            if let Some(value) = eval_live_browser_script(&surface.id, "document.title") {
                return Ok(json!({ "surface_id": surface.id, "title": value, "value": value, "live_webview": true }));
            }
            Ok(json!({ "surface_id": surface.id, "title": state.title, "value": state.title, "live_webview": false }))
        }
        "browser.get.text" => {
            let live_script = selector.as_ref().map_or_else(
                || "document.body ? document.body.innerText : ''".to_string(),
                |selector| {
                    let selector = js_string_literal(selector);
                    format!("document.querySelector({selector})?.innerText ?? ''")
                },
            );
            if let Some(value) = eval_live_browser_script(&surface.id, &live_script) {
                return Ok(json!({ "surface_id": surface.id, "text": value, "value": value, "live_webview": true }));
            }
            let value = if let Some(selector) = selector {
                browser_element(state, &selector)?.text.clone()
            } else {
                browser_inner_text(state)
            };
            Ok(json!({ "surface_id": surface.id, "text": value, "value": value, "live_webview": false }))
        }
        "browser.get.html" => {
            let live_script = selector.as_ref().map_or_else(
                || "document.documentElement ? document.documentElement.outerHTML : ''".to_string(),
                |selector| {
                    let selector = js_string_literal(selector);
                    format!("document.querySelector({selector})?.outerHTML ?? ''")
                },
            );
            if let Some(value) = eval_live_browser_script(&surface.id, &live_script) {
                return Ok(json!({ "surface_id": surface.id, "html": value, "value": value, "live_webview": true }));
            }
            let value = if let Some(selector) = selector {
                browser_element(state, &selector)?.html.clone()
            } else {
                state.html.clone()
            };
            Ok(json!({ "surface_id": surface.id, "html": value, "value": value, "live_webview": false }))
        }
        "browser.get.value" => {
            let selector = selector.ok_or_else(|| invalid_params("selector is required"))?;
            let selector_js = js_string_literal(&selector);
            if let Some(value) = eval_live_browser_script(
                &surface.id,
                &format!("document.querySelector({selector_js})?.value ?? ''"),
            ) {
                return Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": true }));
            }
            let value = browser_element(state, &selector)?.value.clone();
            Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": false }))
        }
        "browser.get.attr" => {
            let selector = selector.ok_or_else(|| invalid_params("selector is required"))?;
            let attr = required_string_any(params, &["attr", "name"])?;
            let selector_js = js_string_literal(&selector);
            let attr_js = js_string_literal(&attr);
            if let Some(value) = eval_live_browser_script(
                &surface.id,
                &format!("document.querySelector({selector_js})?.getAttribute({attr_js}) ?? ''"),
            ) {
                return Ok(json!({ "surface_id": surface.id, "attr": attr, "value": value, "live_webview": true }));
            }
            let value = browser_element(state, &selector)?
                .attrs
                .get(&attr)
                .cloned()
                .unwrap_or_default();
            Ok(json!({ "surface_id": surface.id, "attr": attr, "value": value, "live_webview": false }))
        }
        "browser.get.count" => {
            let selector = selector.ok_or_else(|| invalid_params("selector is required"))?;
            let selector_js = js_string_literal(&selector);
            if let Some(value) =
                eval_live_browser_script(&surface.id, &format!("document.querySelectorAll({selector_js}).length"))
            {
                return Ok(json!({ "surface_id": surface.id, "count": value, "value": value, "live_webview": true }));
            }
            let count = if selector.starts_with('#') {
                usize::from(browser_element_id(state, &selector).is_ok())
            } else {
                state.elements.values().filter(|element| element.tag == selector).count()
            };
            Ok(json!({ "surface_id": surface.id, "count": count, "value": count, "live_webview": false }))
        }
        "browser.get.box" => {
            let selector = selector.ok_or_else(|| invalid_params("selector is required"))?;
            let selector_js = js_string_literal(&selector);
            if let Some(value) = eval_live_browser_script(
                &surface.id,
                &format!("(() => {{ const el = document.querySelector({selector_js}); if (!el) return null; const rect = el.getBoundingClientRect(); return {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }}; }})()"),
            )
            .filter(|value| !value.is_null())
            {
                return Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": true }));
            }
            ensure_browser_element(state, &selector)?;
            Ok(json!({ "surface_id": surface.id, "value": { "x": 0, "y": 0, "width": 120, "height": 32 }, "live_webview": false }))
        }
        "browser.get.styles" => {
            let selector = selector.ok_or_else(|| invalid_params("selector is required"))?;
            let selector_js = js_string_literal(&selector);
            if let Some(property) = optional_string_any(params, &["property"]) {
                let property_js = js_string_literal(&property);
                if let Some(value) = eval_live_browser_script(
                    &surface.id,
                    &format!("(() => {{ const el = document.querySelector({selector_js}); if (!el) return null; return getComputedStyle(el).getPropertyValue({property_js}); }})()"),
                )
                .filter(|value| !value.is_null())
                {
                    return Ok(json!({ "surface_id": surface.id, "property": property, "value": value, "live_webview": true }));
                }
            } else if let Some(value) = eval_live_browser_script(
                &surface.id,
                &format!("(() => {{ const el = document.querySelector({selector_js}); if (!el) return null; const style = getComputedStyle(el); return {{ display: style.display, visibility: style.visibility, opacity: style.opacity, color: style.color, backgroundColor: style.backgroundColor }}; }})()"),
            )
            .filter(|value| !value.is_null())
            {
                return Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": true }));
            }
            ensure_browser_element(state, &selector)?;
            let styles = json!({
                "display": if selector == "#hidden" { "none" } else { "block" },
                "color": if selector == "#style-target" { "rgb(255, 0, 0)" } else { "rgb(0, 0, 0)" },
            });
            if let Some(property) = optional_string_any(params, &["property"]) {
                Ok(json!({ "surface_id": surface.id, "property": property, "value": styles.get(&property).cloned().unwrap_or(Value::Null), "live_webview": false }))
            } else {
                Ok(json!({ "surface_id": surface.id, "value": styles, "live_webview": false }))
            }
        }
        _ => Err(not_supported(format!("{method} is not available on Linux yet"))),
    }
}

fn browser_is(
    surface: &SurfaceRecord,
    method: &str,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    let selector = required_string_any(params, &["selector"])?;
    let selector_js = js_string_literal(&selector);
    let live_script = match method {
        "browser.is.visible" => Some(format!("(() => {{ const el = document.querySelector({selector_js}); if (!el) return false; const style = getComputedStyle(el); const rect = el.getBoundingClientRect(); return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity || 1) !== 0 && rect.width > 0 && rect.height > 0; }})()")),
        "browser.is.enabled" => Some(format!("(() => {{ const el = document.querySelector({selector_js}); return !!el && !el.disabled && !el.hasAttribute('disabled') && el.getAttribute('aria-disabled') !== 'true'; }})()")),
        "browser.is.checked" => Some(format!("(() => {{ const el = document.querySelector({selector_js}); return !!el && !!el.checked; }})()")),
        _ => None,
    };
    if let Some(script) = live_script {
        if let Some(value @ Value::Bool(_)) = eval_live_browser_script(&surface.id, &script) {
            return Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": true }));
        }
    }
    let element = browser_element(&surface.browser_state, &selector)?;
    let value = match method {
        "browser.is.visible" => element.visible,
        "browser.is.enabled" => !element.disabled,
        "browser.is.checked" => element.checked,
        _ => return Err(not_supported(format!("{method} is not available on Linux yet"))),
    };
    Ok(json!({ "surface_id": surface.id, "value": value, "live_webview": false }))
}

fn browser_find(
    surface: &SurfaceRecord,
    method: &str,
    params: Option<&Value>,
) -> Result<Value, RpcError> {
    if let Some(selector) = live_browser_find(surface, method, params) {
        return Ok(json!({ "surface_id": surface.id, "selector": selector, "count": 1, "live_webview": true }));
    }

    let state = &surface.browser_state;
    let selector = match method {
        "browser.find.text" => {
            let text = required_string_any(params, &["text"])?;
            state
                .elements
                .values()
                .find(|element| element.text.contains(&text))
                .map(|element| format!("#{}", element.id))
        }
        "browser.find.label" | "browser.find.placeholder" | "browser.find.alt" | "browser.find.title" | "browser.find.testid" | "browser.find.role" => {
            optional_string_any(params, &["selector"]).or_else(|| state.elements.keys().next().map(|id| format!("#{id}")))
        }
        "browser.find.first" => state.elements.keys().next().map(|id| format!("#{id}")),
        "browser.find.last" => state.elements.keys().next_back().map(|id| format!("#{id}")),
        "browser.find.nth" => {
            let index = params
                .and_then(|value| value.get("index"))
                .and_then(usize_from_value)
                .unwrap_or(1)
                .saturating_sub(1);
            state.elements.keys().nth(index).map(|id| format!("#{id}"))
        }
        _ => None,
    }
    .ok_or_else(|| browser_not_found(format!("{method} did not match an element")))?;
    Ok(json!({ "surface_id": surface.id, "selector": selector, "count": 1, "live_webview": false }))
}

fn live_browser_find(
    surface: &SurfaceRecord,
    method: &str,
    params: Option<&Value>,
) -> Option<String> {
    let selector_fn = live_selector_function();
    let script = match method {
        "browser.find.text" => {
            let text = js_string_literal(&required_string_any(params, &["text"]).ok()?);
            format!("(() => {{ {selector_fn} const needle = {text}; const el = Array.from(document.querySelectorAll('body *')).find((candidate) => (candidate.innerText || candidate.textContent || '').includes(needle)); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.first" => {
            let selector = js_string_literal(
                &optional_string_any(params, &["selector"]).unwrap_or_else(|| "*".to_string()),
            );
            format!("(() => {{ {selector_fn} const el = document.querySelector({selector}); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.last" => {
            let selector = js_string_literal(
                &optional_string_any(params, &["selector"]).unwrap_or_else(|| "*".to_string()),
            );
            format!("(() => {{ {selector_fn} const matches = Array.from(document.querySelectorAll({selector})); const el = matches[matches.length - 1]; return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.nth" => {
            let selector = js_string_literal(
                &optional_string_any(params, &["selector"]).unwrap_or_else(|| "*".to_string()),
            );
            let index = params
                .and_then(|value| value.get("index"))
                .and_then(usize_from_value)
                .unwrap_or(1)
                .saturating_sub(1);
            format!("(() => {{ {selector_fn} const el = Array.from(document.querySelectorAll({selector}))[{index}]; return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.placeholder" => {
            let text = js_string_literal(&required_string_any(params, &["text", "placeholder"]).ok()?);
            format!("(() => {{ {selector_fn} const needle = {text}; const el = Array.from(document.querySelectorAll('[placeholder]')).find((candidate) => candidate.getAttribute('placeholder')?.includes(needle)); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.alt" => {
            let text = js_string_literal(&required_string_any(params, &["text", "alt"]).ok()?);
            format!("(() => {{ {selector_fn} const needle = {text}; const el = Array.from(document.querySelectorAll('[alt]')).find((candidate) => candidate.getAttribute('alt')?.includes(needle)); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.title" => {
            let text = js_string_literal(&required_string_any(params, &["text", "title"]).ok()?);
            format!("(() => {{ {selector_fn} const needle = {text}; const el = Array.from(document.querySelectorAll('[title]')).find((candidate) => candidate.getAttribute('title')?.includes(needle)); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.testid" => {
            let testid = js_string_literal(&required_string_any(params, &["testid", "testId", "text"]).ok()?);
            format!("(() => {{ {selector_fn} const value = {testid}; const el = document.querySelector(`[data-testid=\"${{CSS.escape(value)}}\"], [data-test-id=\"${{CSS.escape(value)}}\"]`); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.role" => {
            let role = js_string_literal(&required_string_any(params, &["role", "text"]).ok()?);
            format!("(() => {{ {selector_fn} const value = {role}; const el = document.querySelector(`[role=\"${{CSS.escape(value)}}\"]`); return el ? cmuxSelector(el) : null; }})()")
        }
        "browser.find.label" => {
            let text = js_string_literal(&required_string_any(params, &["text", "label"]).ok()?);
            format!("(() => {{ {selector_fn} const needle = {text}; const label = Array.from(document.querySelectorAll('label')).find((candidate) => (candidate.innerText || candidate.textContent || '').includes(needle)); if (!label) return null; const el = label.control || label.querySelector('input,textarea,select,button') || label; return cmuxSelector(el); }})()")
        }
        _ => return None,
    };

    eval_live_browser_script(&surface.id, &script)
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
}

fn live_selector_function() -> &'static str {
    "const cmuxEscape = (value) => globalThis.CSS && CSS.escape ? CSS.escape(value) : String(value).replace(/[^a-zA-Z0-9_-]/g, '\\\\$&'); const cmuxSelector = (el) => { if (el.id) return `#${cmuxEscape(el.id)}`; const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id'); if (testid) return `[data-testid=\"${cmuxEscape(testid)}\"]`; const tag = el.tagName.toLowerCase(); const siblings = Array.from(el.parentElement ? el.parentElement.children : document.querySelectorAll(tag)).filter((candidate) => candidate.tagName === el.tagName); const index = siblings.indexOf(el) + 1; return `${tag}:nth-of-type(${index})`; };"
}

fn browser_inner_text(state: &BrowserState) -> String {
    let mut text = String::new();
    if !state.title.is_empty() {
        text.push_str(&state.title);
        text.push('\n');
    }
    for element in state.elements.values() {
        if !element.text.is_empty() {
            text.push_str(&element.text);
            text.push('\n');
        }
        if !element.value.is_empty() {
            text.push_str(&element.value);
            text.push('\n');
        }
    }
    text
}

fn eval_browser_script(state: &BrowserState, script: &str) -> Value {
    if script.contains("document.readyState") {
        return json!("complete");
    }
    if script.contains("document.body") && script.contains("innerText") {
        return json!(browser_inner_text(state));
    }
    if script.contains("document.activeElement") {
        return json!(state.active_element_id.clone().unwrap_or_default());
    }
    if script.contains("window.__hover") && script.contains("window.__dbl") && script.contains("window.__keys") {
        return json!({
            "hover": state.hover_count,
            "dbl": state.dbl_count,
            "down": state.key_down_count,
            "up": state.key_up_count,
            "press": state.key_press_count,
        });
    }
    if script.contains("scrollTop") {
        if let Some(selector) = selector_from_script(script) {
            if let Ok(element) = browser_element(state, &selector) {
                return json!(element.scroll_top);
            }
        }
    }
    if script.contains("getBoundingClientRect") {
        return json!(true);
    }
    if let Some(selector) = selector_from_script(script) {
        if script.contains("!== null") {
            return json!(browser_element_id(state, &selector).is_ok());
        }
        if script.contains(".value") {
            return json!(browser_element(state, &selector).map(|element| element.value.clone()).unwrap_or_default());
        }
        if script.contains(".textContent") || script.contains("innerText") {
            return json!(browser_element(state, &selector).map(|element| element.text.clone()).unwrap_or_default());
        }
    }
    Value::Null
}

fn eval_live_browser_script(surface_id: &str, script: &str) -> Option<Value> {
    let result =
        browser::evaluate_expression_json(surface_id, script, std::time::Duration::from_secs(2))?;
    let text = match result {
        Ok(text) => text,
        Err(_) => {
            match browser::run_statement_json(
                surface_id,
                script,
                std::time::Duration::from_secs(2),
            )? {
                Ok(text) => text,
                Err(_) => return None,
            }
        }
    };
    if text.is_empty() || text == "undefined" {
        return Some(Value::Null);
    }
    serde_json::from_str(&text).ok().or_else(|| Some(Value::String(text)))
}

fn run_live_browser_script(surface_id: &str, script: &str) -> bool {
    let Some(result) =
        browser::run_statement_json(surface_id, script, std::time::Duration::from_secs(2))
    else {
        return false;
    };
    let Ok(text) = result else {
        return false;
    };
    matches!(serde_json::from_str::<Value>(&text), Ok(Value::Bool(true)))
}

fn dispatch_live_keyboard_event(surface_id: &str, key: &str, phase: &str) -> bool {
    let key_js = js_string_literal(key);
    let event_script = |event_name: &str| {
        format!(
            "const target = document.activeElement || document.body || document; const key = {key_js}; target.dispatchEvent(new KeyboardEvent('{event_name}', {{ key, bubbles: true, cancelable: true }}));"
        )
    };
    let script = match phase {
        "keydown" => format!("{} return true;", event_script("keydown")),
        "keyup" => format!("{} return true;", event_script("keyup")),
        "press" => format!(
            "{} {} {} return true;",
            event_script("keydown"),
            event_script("keypress"),
            event_script("keyup")
        ),
        _ => return false,
    };
    run_live_browser_script(surface_id, &script)
}

fn js_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "''".to_string())
}

fn selector_from_script(script: &str) -> Option<String> {
    for marker in ["querySelector('", "querySelector(\""] {
        let start = script.find(marker)? + marker.len();
        let quote = if marker.ends_with("'") { '\'' } else { '"' };
        let end = script[start..].find(quote)?;
        return Some(script[start..start + end].to_string());
    }
    None
}

fn ensure_default_pane(workspace: &mut WorkspaceRecord) -> usize {
    if let Some(index) = workspace.panes.first().map(|_| 0) {
        return index;
    }
    let pane = PaneRecord {
        id: next_pane_id(),
        workspace_id: workspace.id.clone(),
        surface_ids: Vec::new(),
        active_surface_id: None,
    };
    workspace.panes.push(pane);
    workspace.panes.len() - 1
}

fn required_workspace_param(params: Option<&Value>) -> Result<String, RpcError> {
    optional_string_any(params, &["workspace", "workspaceId", "workspace_id"])
        .ok_or_else(|| invalid_params("workspace is required"))
}

fn required_surface_param(params: Option<&Value>) -> Result<String, RpcError> {
    optional_string_any(params, &["surface", "surfaceId", "surface_id", "panel", "panelId", "panel_id"])
        .ok_or_else(|| invalid_params("surface is required"))
}

fn required_pane_param(params: Option<&Value>) -> Result<String, RpcError> {
    optional_string_any(params, &["pane", "paneId", "pane_id"])
        .ok_or_else(|| invalid_params("pane is required"))
}

fn with_default_surface_kind(params: Option<Value>, kind: SurfaceKind) -> Option<Value> {
    let mut object = params
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    object
        .entry("type")
        .or_insert_with(|| serde_json::to_value(kind).unwrap_or(Value::String("terminal".to_string())));
    Some(Value::Object(object))
}

fn next_workspace_id() -> String {
    let _ = NEXT_WORKSPACE_ORDINAL.fetch_add(1, Ordering::Relaxed);
    next_uuid("workspace")
}

fn next_surface_id() -> String {
    let _ = NEXT_SURFACE_ORDINAL.fetch_add(1, Ordering::Relaxed);
    next_uuid("surface")
}

fn next_pane_id() -> String {
    let _ = NEXT_PANE_ORDINAL.fetch_add(1, Ordering::Relaxed);
    next_uuid("pane")
}

fn next_notification_id() -> String {
    let _ = NEXT_NOTIFICATION_ORDINAL.fetch_add(1, Ordering::Relaxed);
    next_uuid("notification")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn next_uuid(namespace: &str) -> String {
    let ordinal = NEXT_UUID_ORDINAL.fetch_add(1, Ordering::Relaxed);
    let namespace_hash = namespace
        .bytes()
        .fold(0_u64, |hash, byte| hash.wrapping_mul(131).wrapping_add(byte as u64));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let high = nanos ^ namespace_hash;
    let low = ordinal ^ namespace_hash.rotate_left(17);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (high >> 32) as u32,
        (high >> 16) as u16,
        high as u16 & 0x0fff,
        (low >> 48) as u16 & 0x0fff,
        low & 0x0000_ffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(state: &AppState, method: &str, params: Value) -> Value {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });
        serde_json::from_str(&dispatch_line(&request.to_string(), state)).unwrap()
    }

    #[test]
    fn capabilities_identify_linux_surface_area() {
        let state = AppState::default();
        let response = rpc(&state, "system.capabilities", json!({}));
        assert_eq!(response["result"]["platform"], "linux");
        assert_eq!(response["result"]["features"]["sshRelay"], true);
    }

    #[test]
    fn workspace_and_surface_lifecycle_round_trip() {
        let state = AppState::default();
        let created = rpc(&state, "workspace.create", json!({ "title": "CI" }));
        let workspace_id = created["result"]["workspace"]["id"].as_str().unwrap();

        let surface = rpc(
            &state,
            "surface.create",
            json!({
                "workspaceId": workspace_id,
                "title": "Browser",
                "type": "browser"
            }),
        );
        assert_eq!(surface["result"]["surface"]["workspaceId"], workspace_id);
        assert_eq!(surface["result"]["surface"]["kind"], "browser");

        let listed = rpc(&state, "surface.list", json!({ "workspaceId": workspace_id }));
        assert_eq!(listed["result"]["surfaces"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unknown_workspace_returns_invalid_params() {
        let state = AppState::default();
        let response = rpc(
            &state,
            "surface.create",
            json!({ "workspaceId": "workspace-missing" }),
        );
        assert_eq!(response["error"]["code"], -32602);
    }
}
