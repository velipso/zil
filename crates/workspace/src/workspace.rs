pub mod history_manager;
pub mod invalid_item_view;
pub mod item;
mod modal_layer;
mod multi_workspace;
pub mod notifications;
pub mod pane;
pub mod pane_group;
pub mod path_list {
    pub use util::path_list::{PathList, SerializedPathList};
}
pub mod path_link;
pub mod searchable;
pub mod security_modal;
pub mod focus_follows_mouse;
mod toast_layer;
mod toolbar;
mod workspace_settings;

pub use crate::notifications::NotificationFrame;
pub use multi_workspace::MultiWorkspace;
pub use path_list::{PathList, SerializedPathList};
pub use remote::{
    RemoteConnectionIdentity, remote_connection_identity, same_remote_connection_identity,
};
pub use toast_layer::{ToastAction, ToastLayer, ToastView};

use anyhow::{Context as _, Result, anyhow};
use client::{ErrorExt, proto::{self, ErrorCode, PeerId}};
use collections::{HashMap, HashSet, hash_map};
use futures::{
    Future, FutureExt, StreamExt,
    channel::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    future::Shared,
};
use gpui::{
    Action, AnyView, AnyWeakView, App, AsyncApp, AsyncWindowContext, Bounds,
    Context, CursorStyle, Decorations, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Global, HitboxBehavior, Hsla, KeyContext, Keystroke, ManagedView, MouseButton,
    PathPromptOptions, Point, PromptLevel, Render, ResizeEdge, Size, Stateful, Subscription,
    SystemWindowTabController, Task, TaskExt, Tiling, WeakEntity, WindowBounds, WindowHandle,
    WindowOptions, actions, canvas, point, relative, size, transparent_black,
};
pub use history_manager::*;
pub use item::{
    FollowableItem, FollowableItemHandle, Item, ItemHandle, ItemSettings,
    ProjectItem, SerializableItem, SerializableItemHandle, WeakItemHandle,
};
use itertools::Itertools;
use language::{LanguageRegistry, Rope};
pub use modal_layer::*;
use notifications::{
    DetachAndPromptErr, Notifications, dismiss_app_notification,
    simple_message_notification::MessageNotification,
};
pub use pane::*;
pub use pane_group::{
    ActivePaneDecorator, HANDLE_HITBOX_SIZE, Member, PaneAxis, PaneGroup, PaneRenderContext,
    SplitDirection,
};
use project::{
    DirectoryLister, Project, ProjectEntryId, ProjectPath, ResolvedPath, Worktree, WorktreeId,
    WorktreeSettings,
    project_settings::ProjectSettings,
    trusted_worktrees::{RemoteHostLocation, TrustedWorktrees},
};
use schemars::JsonSchema;
use serde::Deserialize;
use session::AppSession;
use settings::{CenteredPaddingSettings, Settings, SettingsLocation, SettingsStore};

use std::{
    any::TypeId,
    borrow::Cow,
    cell::RefCell,
    cmp,
    collections::VecDeque,
    env,
    hash::Hash,
    num::NonZeroU32,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::Duration,
};
use theme::{ActiveTheme, SystemAppearance};
use theme_settings::ThemeSettings;
pub use toolbar::{
    PaneSearchBarCallbacks, Toolbar, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView,
};
pub use ui;
use ui::{Window, prelude::*};
use util::{
    ResultExt,
    paths::{PathStyle, SanitizedPath},
    rel_path::RelPath,
    serde::default_true,
};
use uuid::Uuid;
pub use workspace_settings::{AutosaveSetting, FocusFollowsMouse, TabBarSettings, WorkspaceSettings};
use zed_actions::{theme::ToggleMode};

use crate::{item::ItemBufferKind, notifications::NotificationId};
use crate::security_modal::SecurityModal;

pub type GroupId = i64;
pub type PaneId = i64;
pub type ItemId = u64;

pub const SERIALIZATION_THROTTLE_TIME: Duration = Duration::from_millis(200);

static ZED_WINDOW_SIZE: LazyLock<Option<Size<Pixels>>> = LazyLock::new(|| {
    env::var("ZED_WINDOW_SIZE")
        .ok()
        .as_deref()
        .and_then(parse_pixel_size_env_var)
});

static ZED_WINDOW_POSITION: LazyLock<Option<Point<Pixels>>> = LazyLock::new(|| {
    env::var("ZED_WINDOW_POSITION")
        .ok()
        .as_deref()
        .and_then(parse_pixel_position_env_var)
});

/// Opens a file or directory.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = workspace)]
pub struct Open {
    /// When true, opens in a new window. When false, adds to the current
    /// window as a new workspace (multi-workspace).
    #[serde(default = "Open::default_create_new_window")]
    pub create_new_window: bool,
}

impl Open {
    pub const DEFAULT: Self = Self {
        create_new_window: false,
    };

    /// Used by `#[serde(default)]` on the `create_new_window` field so that
    /// the serde default and `Open::DEFAULT` stay in sync.
    fn default_create_new_window() -> bool {
        Self::DEFAULT.create_new_window
    }
}

impl Default for Open {
    fn default() -> Self {
        Self::DEFAULT
    }
}

actions!(
    workspace,
    [
        /// Activates the next pane in the workspace.
        ActivateNextPane,
        /// Activates the previous pane in the workspace.
        ActivatePreviousPane,
        /// Activates the last pane in the workspace.
        ActivateLastPane,
        /// Switches to the next window.
        ActivateNextWindow,
        /// Switches to the previous window.
        ActivatePreviousWindow,
        /// Clears all notifications.
        ClearAllNotifications,
        /// Clears all navigation history, including forward/backward navigation, recently opened files, and recently closed tabs. **This action is irreversible**.
        ClearNavigationHistory,
        /// Closes the current window.
        CloseWindow,
        /// Creates a new file.
        NewFile,
        /// Creates a new file in a vertical split.
        NewFileSplitVertical,
        /// Creates a new file in a horizontal split.
        NewFileSplitHorizontal,
        /// Opens a new search.
        NewSearch,
        /// Opens a new window.
        NewWindow,
        /// Opens multiple files.
        OpenFiles,
        /// Reloads the active item.
        ReloadActiveItem,
        /// Reloads the application
        Reload,
        /// Formats and saves the current file, regardless of the format_on_save setting.
        FormatAndSave,
        /// Saves the current file with a new name.
        SaveAs,
        /// Saves without formatting.
        SaveWithoutFormat,
        /// Shuts down all debug adapters.
        ShutdownDebugAdapters,
        /// Suppresses the current notification.
        SuppressNotification,
        /// Toggles centered layout mode.
        ToggleCenteredLayout,
        /// Toggles read-only mode for the active item (if supported by that item).
        ToggleReadOnlyFile,
        /// Toggles between scrolling/stacked tabs.
        ToggleStackedTabs,
        /// If any worktrees are in restricted mode, shows a modal with possible actions.
        /// If the modal is shown already, closes it without trusting any worktree.
        ToggleWorktreeSecurity,
        /// Clears all trusted worktrees, placing them in restricted mode on next open.
        /// Requires restart to take effect on already opened projects.
        ClearTrustedWorktrees,
        /// Toggles expansion of the selected item.
        ToggleExpandItem,
    ]
);

/// Activates a specific pane by its index.
#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
pub struct ActivatePane(pub usize);

/// Moves an item to a specific pane by index.
#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct MoveItemToPane {
    #[serde(default = "default_1")]
    pub destination: usize,
    #[serde(default = "default_true")]
    pub focus: bool,
    #[serde(default)]
    pub clone: bool,
}

fn default_1() -> usize {
    1
}

/// Moves an item to a pane in the specified direction.
#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct MoveItemToPaneInDirection {
    #[serde(default = "default_right")]
    pub direction: SplitDirection,
    #[serde(default = "default_true")]
    pub focus: bool,
    #[serde(default)]
    pub clone: bool,
}

/// Creates a new file in a split of the desired direction.
#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct NewFileSplit(pub SplitDirection);

fn default_right() -> SplitDirection {
    SplitDirection::Right
}

/// Saves all open files in the workspace.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct SaveAll {
    #[serde(default)]
    pub save_intent: Option<SaveIntent>,
}

/// Saves the current file with the specified options.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct Save {
    #[serde(default)]
    pub save_intent: Option<SaveIntent>,
}

/// Moves Focus to the central panes in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = workspace)]
pub struct FocusCenterPane;

///  Closes all items and panes in the workspace.
#[derive(Clone, PartialEq, Debug, Deserialize, Default, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct CloseAllItemsAndPanes {
    #[serde(default)]
    pub save_intent: Option<SaveIntent>,
}

/// Closes all inactive tabs and panes in the workspace.
#[derive(Clone, PartialEq, Debug, Deserialize, Default, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct CloseInactiveTabsAndPanes {
    #[serde(default)]
    pub save_intent: Option<SaveIntent>,
}

/// Closes the active item across all panes.
#[derive(Clone, PartialEq, Debug, Deserialize, Default, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct CloseItemInAllPanes {
    #[serde(default)]
    pub save_intent: Option<SaveIntent>,
}

/// Sends a sequence of keystrokes to the active element.
#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
pub struct SendKeystrokes(pub String);

/// Opens a new terminal in the center.
#[derive(Default, PartialEq, Eq, Clone, Deserialize, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct NewCenterTerminal {
    /// If true, creates a local terminal even in remote projects.
    #[serde(default)]
    pub local: bool,
}

actions!(
    workspace,
    [
        /// Activates the pane to the left.
        ActivatePaneLeft,
        /// Activates the pane to the right.
        ActivatePaneRight,
        /// Activates the pane above.
        ActivatePaneUp,
        /// Activates the pane below.
        ActivatePaneDown,
        /// Swaps the current pane with the one to the left.
        SwapPaneLeft,
        /// Swaps the current pane with the one to the right.
        SwapPaneRight,
        /// Swaps the current pane with the one above.
        SwapPaneUp,
        /// Swaps the current pane with the one below.
        SwapPaneDown,
        // Swaps the current pane with the first available adjacent pane (searching in order: below, above, right, left) and activates that pane.
        SwapPaneAdjacent,
        /// Move the current pane to be at the far left.
        MovePaneLeft,
        /// Move the current pane to be at the far right.
        MovePaneRight,
        /// Move the current pane to be at the very top.
        MovePaneUp,
        /// Move the current pane to be at the very bottom.
        MovePaneDown,
    ]
);

#[derive(PartialEq, Eq, Debug)]
pub enum CloseIntent {
    /// Quit the program entirely.
    Quit,
    /// Close a window.
    CloseWindow,
    /// Replace the workspace in an existing window.
    ReplaceWindow,
}

#[derive(Clone)]
pub struct Toast {
    id: NotificationId,
    msg: Cow<'static, str>,
    autohide: bool,
    on_click: Option<(Cow<'static, str>, Arc<dyn Fn(&mut Window, &mut App)>)>,
}

impl Toast {
    pub fn new<I: Into<Cow<'static, str>>>(id: NotificationId, msg: I) -> Self {
        Toast {
            id,
            msg: msg.into(),
            on_click: None,
            autohide: false,
        }
    }

    pub fn on_click<F, M>(mut self, message: M, on_click: F) -> Self
    where
        M: Into<Cow<'static, str>>,
        F: Fn(&mut Window, &mut App) + 'static,
    {
        self.on_click = Some((message.into(), Arc::new(on_click)));
        self
    }

    pub fn autohide(mut self) -> Self {
        self.autohide = true;
        self
    }
}

impl PartialEq for Toast {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.msg == other.msg
            && self.on_click.is_some() == other.on_click.is_some()
    }
}

/// Opens a new terminal with the specified working directory.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = workspace)]
#[serde(deny_unknown_fields)]
pub struct OpenTerminal {
    pub working_directory: PathBuf,
    /// If true, creates a local terminal even in remote projects.
    #[serde(default)]
    pub local: bool,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Hash,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct WorkspaceId(i64);

impl WorkspaceId {
    pub fn from_i64(value: i64) -> Self {
        Self(value)
    }
}

impl From<WorkspaceId> for i64 {
    fn from(val: WorkspaceId) -> Self {
        val.0
    }
}

fn prompt_and_open_paths(
    app_state: Arc<AppState>,
    options: PathPromptOptions,
    create_new_window: bool,
    cx: &mut App,
) {
    if let Some(workspace_window) =
        workspace_windows_for_location(cx)
            .into_iter()
            .next()
    {
        workspace_window
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    prompt_for_open_path_and_open(
                        workspace,
                        app_state,
                        options,
                        create_new_window,
                        window,
                        cx,
                    );
                });
            })
            .ok();
    } else {
        let task = Workspace::new_local(
            Vec::new(),
            app_state.clone(),
            None,
            None,
            None,
            OpenMode::Activate,
            cx,
        );
        cx.spawn(async move |cx| {
            let OpenResult { window, .. } = task.await?;
            window.update(cx, |multi_workspace, window, cx| {
                window.activate_window();
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    prompt_for_open_path_and_open(
                        workspace,
                        app_state,
                        options,
                        create_new_window,
                        window,
                        cx,
                    );
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }
}

pub fn prompt_for_open_path_and_open(
    workspace: &mut Workspace,
    app_state: Arc<AppState>,
    options: PathPromptOptions,
    create_new_window: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let paths = workspace.prompt_for_open_path(
        options,
        DirectoryLister::new(workspace.project().clone(), app_state.fs.clone()),
        window,
        cx,
    );
    let multi_workspace_handle = window.window_handle().downcast::<MultiWorkspace>();
    cx.spawn_in(window, async move |this, cx| {
        let Some(paths) = paths.await.log_err().flatten() else {
            return;
        };
        if !create_new_window {
            if let Some(handle) = multi_workspace_handle {
                if let Some(task) = handle
                    .update(cx, |multi_workspace, window, cx| {
                        multi_workspace.open_project(paths, OpenMode::Activate, window, cx)
                    })
                    .log_err()
                {
                    task.await.log_err();
                }
                return;
            }
        }
        if let Some(task) = this
            .update_in(cx, |this, window, cx| {
                this.open_workspace_for_paths(OpenMode::NewWindow, paths, window, cx)
            })
            .log_err()
        {
            task.await.log_err();
        }
    })
    .detach();
}

pub fn init(app_state: Arc<AppState>, cx: &mut App) {
    toast_layer::init(cx);
    history_manager::init(app_state.fs.clone(), cx);

    cx.on_action(|_: &CloseWindow, cx| Workspace::close_global(cx))
        .on_action(|_: &Reload, cx| reload(cx))
        .on_action(|action: &Open, cx: &mut App| {
            let app_state = AppState::global(cx);
            prompt_and_open_paths(
                app_state,
                PathPromptOptions {
                    files: true,
                    directories: true,
                    multiple: true,
                    prompt: None,
                },
                action.create_new_window,
                cx,
            );
        })
        .on_action(|_: &OpenFiles, cx: &mut App| {
            let directories = cx.can_select_mixed_files_and_dirs();
            let app_state = AppState::global(cx);
            prompt_and_open_paths(
                app_state,
                PathPromptOptions {
                    files: true,
                    directories,
                    multiple: true,
                    prompt: None,
                },
                true,
                cx,
            );
        });
}

type BuildProjectItemForPathFn =
    fn(
        &Entity<Project>,
        &ProjectPath,
        &mut Window,
        &mut App,
    ) -> Option<Task<Result<(Option<ProjectEntryId>, WorkspaceItemBuilder)>>>;

#[derive(Clone, Default)]
struct ProjectItemRegistry {
    build_project_item_for_path_fns: Vec<BuildProjectItemForPathFn>,
}

impl ProjectItemRegistry {
    fn register<T: ProjectItem>(&mut self) {
        self.build_project_item_for_path_fns
            .push(|project, project_path, window, cx| {
                let project_path = project_path.clone();
                let is_file = project
                    .read(cx)
                    .entry_for_path(&project_path, cx)
                    .is_some_and(|entry| entry.is_file());
                let entry_abs_path = project.read(cx).absolute_path(&project_path, cx);
                let is_local = project.read(cx).is_local();
                let project_item =
                    <T::Item as project::ProjectItem>::try_open(project, &project_path, cx)?;
                let project = project.clone();
                Some(window.spawn(cx, async move |cx| {
                    match project_item.await.with_context(|| {
                        format!(
                            "opening project path {:?}",
                            entry_abs_path.as_deref().unwrap_or(&project_path.path.as_std_path())
                        )
                    }) {
                        Ok(project_item) => {
                            let project_item = project_item;
                            let project_entry_id: Option<ProjectEntryId> =
                                project_item.read_with(cx, project::ProjectItem::entry_id);
                            let build_workspace_item = Box::new(
                                |pane: &mut Pane, window: &mut Window, cx: &mut Context<Pane>| {
                                    Box::new(cx.new(|cx| {
                                        T::for_project_item(
                                            project,
                                            Some(pane),
                                            project_item,
                                            window,
                                            cx,
                                        )
                                    })) as Box<dyn ItemHandle>
                                },
                            ) as Box<_>;
                            Ok((project_entry_id, build_workspace_item))
                        }
                        Err(e) => {
                            log::warn!("Failed to open a project item: {e:#}");
                            if e.error_code() == ErrorCode::Internal {
                                if let Some(abs_path) =
                                    entry_abs_path.as_deref().filter(|_| is_file)
                                {
                                    if let Some(broken_project_item_view) =
                                        cx.update(|window, cx| {
                                            T::for_broken_project_item(
                                                abs_path, is_local, &e, window, cx,
                                            )
                                        })?
                                    {
                                        let build_workspace_item = Box::new(
                                            move |_: &mut Pane, _: &mut Window, cx: &mut Context<Pane>| {
                                                cx.new(|_| broken_project_item_view).boxed_clone()
                                            },
                                        )
                                        as Box<_>;
                                        return Ok((None, build_workspace_item));
                                    }
                                }
                            }
                            Err(e)
                        }
                    }
                }))
            });
    }

    fn open_path(
        &self,
        project: &Entity<Project>,
        path: &ProjectPath,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<(Option<ProjectEntryId>, WorkspaceItemBuilder)>> {
        let Some(open_project_item) = self
            .build_project_item_for_path_fns
            .iter()
            .rev()
            .find_map(|open_project_item| open_project_item(project, path, window, cx))
        else {
            return Task::ready(Err(anyhow!("cannot open file {:?}", path.path)));
        };
        open_project_item
    }
}

type WorkspaceItemBuilder =
    Box<dyn FnOnce(&mut Pane, &mut Window, &mut Context<Pane>) -> Box<dyn ItemHandle>>;

impl Global for ProjectItemRegistry {}

/// Registers a [ProjectItem] for the app. When opening a file, all the registered
/// items will get a chance to open the file, starting from the project item that
/// was added last.
pub fn register_project_item<I: ProjectItem>(cx: &mut App) {
    cx.default_global::<ProjectItemRegistry>().register::<I>();
}

#[derive(Default)]
pub struct FollowableViewRegistry(HashMap<TypeId, FollowableViewDescriptor>);

struct FollowableViewDescriptor {
    from_state_proto: fn(
        Entity<Workspace>,
        ViewId,
        &mut Option<proto::view::Variant>,
        &mut Window,
        &mut App,
    ) -> Option<Task<Result<Box<dyn FollowableItemHandle>>>>,
    to_followable_view: fn(&AnyView) -> Box<dyn FollowableItemHandle>,
}

impl Global for FollowableViewRegistry {}

impl FollowableViewRegistry {
    pub fn register<I: FollowableItem>(cx: &mut App) {
        cx.default_global::<Self>().0.insert(
            TypeId::of::<I>(),
            FollowableViewDescriptor {
                from_state_proto: |workspace, id, state, window, cx| {
                    I::from_state_proto(workspace, id, state, window, cx).map(|task| {
                        cx.foreground_executor()
                            .spawn(async move { Ok(Box::new(task.await?) as Box<_>) })
                    })
                },
                to_followable_view: |view| Box::new(view.clone().downcast::<I>().unwrap()),
            },
        );
    }

    pub fn from_state_proto(
        workspace: Entity<Workspace>,
        view_id: ViewId,
        mut state: Option<proto::view::Variant>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Task<Result<Box<dyn FollowableItemHandle>>>> {
        cx.update_default_global(|this: &mut Self, cx| {
            this.0.values().find_map(|descriptor| {
                (descriptor.from_state_proto)(workspace.clone(), view_id, &mut state, window, cx)
            })
        })
    }

    pub fn to_followable_view(
        view: impl Into<AnyView>,
        cx: &App,
    ) -> Option<Box<dyn FollowableItemHandle>> {
        let this = cx.try_global::<Self>()?;
        let view = view.into();
        let descriptor = this.0.get(&view.entity_type())?;
        Some((descriptor.to_followable_view)(&view))
    }
}

#[derive(Copy, Clone)]
struct SerializableItemDescriptor {
    view_to_serializable_item: fn(AnyView) -> Box<dyn SerializableItemHandle>,
}

#[derive(Default)]
struct SerializableItemRegistry {
    descriptors_by_type: HashMap<TypeId, SerializableItemDescriptor>,
}

impl Global for SerializableItemRegistry {}

impl SerializableItemRegistry {
    fn view_to_serializable_item_handle(
        view: AnyView,
        cx: &App,
    ) -> Option<Box<dyn SerializableItemHandle>> {
        let this = cx.try_global::<Self>()?;
        let descriptor = this.descriptors_by_type.get(&view.entity_type())?;
        Some((descriptor.view_to_serializable_item)(view))
    }
}

pub struct AppState {
    pub languages: Arc<LanguageRegistry>,
    pub workspace_store: Entity<WorkspaceStore>,
    pub fs: Arc<dyn fs::Fs>,
    pub build_window_options: fn(Option<Uuid>, &mut App) -> WindowOptions,
    pub session: Entity<AppSession>,
}

struct GlobalAppState(Arc<AppState>);

impl Global for GlobalAppState {}

/// Tracks worktree creation progress for the workspace.
/// Read by the title bar to show a loading indicator on the worktree button.
#[derive(Default)]
pub struct ActiveWorktreeCreation {
    pub label: Option<SharedString>,
    pub is_switch: bool,
}

/// Captured workspace state used when switching between worktrees.
/// Stores the layout and open files so they can be restored in the new workspace.
pub struct PreviousWorkspaceState {
    pub open_file_paths: Vec<PathBuf>,
    pub active_file_path: Option<PathBuf>,
}

pub struct WorkspaceStore {
    workspaces: HashSet<(gpui::AnyWindowHandle, WeakEntity<Workspace>)>,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub enum CollaboratorId {
    PeerId(PeerId),
    Agent,
}

impl From<PeerId> for CollaboratorId {
    fn from(peer_id: PeerId) -> Self {
        CollaboratorId::PeerId(peer_id)
    }
}

impl From<&PeerId> for CollaboratorId {
    fn from(peer_id: &PeerId) -> Self {
        CollaboratorId::PeerId(*peer_id)
    }
}

impl AppState {
    #[track_caller]
    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalAppState>().0.clone()
    }
    pub fn try_global(cx: &App) -> Option<Arc<Self>> {
        cx.try_global::<GlobalAppState>()
            .map(|state| state.0.clone())
    }
    pub fn set_global(state: Arc<AppState>, cx: &mut App) {
        cx.set_global(GlobalAppState(state));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test(cx: &mut App) -> Arc<Self> {
        use fs::Fs;
        use session::Session;
        use settings::SettingsStore;

        if !cx.has_global::<SettingsStore>() {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        }

        let fs = fs::FakeFs::new(cx.background_executor().clone());
        <dyn Fs>::set_global(fs.clone(), cx);
        let languages = Arc::new(LanguageRegistry::test(fs.clone(), cx.background_executor().clone()));
        let session = cx.new(|cx| AppSession::new(Session::test(), cx));
        let workspace_store = cx.new(|_cx| WorkspaceStore::new());

        theme_settings::init(theme::LoadThemes::JustBase, cx);

        Arc::new(Self {
            fs,
            languages,
            workspace_store,
            build_window_options: |_, _| Default::default(),
            session,
        })
    }
}

struct DelayedDebouncedEditAction {
    task: Option<Task<()>>,
    cancel_channel: Option<oneshot::Sender<()>>,
}

impl DelayedDebouncedEditAction {
    fn new() -> DelayedDebouncedEditAction {
        DelayedDebouncedEditAction {
            task: None,
            cancel_channel: None,
        }
    }

    fn fire_new<F>(
        &mut self,
        delay: Duration,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        func: F,
    ) where
        F: 'static
            + Send
            + FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> Task<Result<()>>,
    {
        if let Some(channel) = self.cancel_channel.take() {
            _ = channel.send(());
        }

        let (sender, mut receiver) = oneshot::channel::<()>();
        self.cancel_channel = Some(sender);

        let previous_task = self.task.take();
        self.task = Some(cx.spawn_in(window, async move |workspace, cx| {
            let mut timer = cx.background_executor().timer(delay).fuse();
            if let Some(previous_task) = previous_task {
                previous_task.await;
            }

            futures::select_biased! {
                _ = receiver => return,
                    _ = timer => {}
            }

            if let Some(result) = workspace
                .update_in(cx, |workspace, window, cx| (func)(workspace, window, cx))
                .log_err()
            {
                result.await.log_err();
            }
        }));
    }
}

pub enum Event {
    PaneAdded(Entity<Pane>),
    PaneRemoved,
    ItemAdded {
        item: Box<dyn ItemHandle>,
    },
    ActiveItemChanged,
    ItemRemoved {
        item_id: EntityId,
    },
    UserSavedItem {
        pane: WeakEntity<Pane>,
        item: Box<dyn WeakItemHandle>,
        save_intent: SaveIntent,
    },
    ContactRequestedJoin(u64),
    WorkspaceCreated(WeakEntity<Workspace>),
    OpenBundledFile {
        text: Cow<'static, str>,
        title: &'static str,
        language: &'static str,
    },
    ZoomChanged,
    ModalOpened,
    Activate,
    PanelAdded(AnyView),
    WorktreeCreationChanged,
}

/// Controls which types of items should be made visible in the project panel
/// when opened.
#[derive(Debug, Clone)]
pub enum OpenVisible {
    /// Make all opened items visible (both files and directories).
    All,
    /// Don't make any opened items visible.
    None,
    /// Only make opened files visible, not directories.
    OnlyFiles,
    /// Only make opened directories visible, not files.
    OnlyDirectories,
}

enum WorkspaceLocation {
    // Valid local paths or SSH project to serialize
    Location,
    // No valid location found hence clear session id
    DetachFromSession,
    // No valid location found to serialize
    None,
}

type PromptForNewPath = Box<
    dyn Fn(
        &mut Workspace,
        DirectoryLister,
        Option<String>,
        &mut Window,
        &mut Context<Workspace>,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>>,
>;

type PromptForOpenPath = Box<
    dyn Fn(
        &mut Workspace,
        DirectoryLister,
        &mut Window,
        &mut Context<Workspace>,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>>,
>;

#[derive(Default)]
struct DispatchingKeystrokes {
    dispatched: HashSet<Vec<Keystroke>>,
    queue: VecDeque<Keystroke>,
    task: Option<Shared<Task<()>>>,
}

/// Collects everything project-related for a certain window opened.
/// In some way, is a counterpart of a window, as the [`WindowHandle`] could be downcast into `Workspace`.
///
/// A `Workspace` usually consists of 1 or more projects, a central pane group, 3 docks and a status bar.
/// The `Workspace` owns everybody's state and serves as a default, "global context",
/// that can be used to register a global action to be triggered from any place in the window.
pub struct Workspace {
    weak_self: WeakEntity<Self>,
    workspace_actions: Vec<Box<dyn Fn(Div, &Workspace, &mut Window, &mut Context<Self>) -> Div>>,
    zoomed: Option<AnyWeakView>,
    center: PaneGroup,
    panes: Vec<Entity<Pane>>,
    panes_by_item: HashMap<EntityId, WeakEntity<Pane>>,
    active_pane: Entity<Pane>,
    last_active_center_pane: Option<WeakEntity<Pane>>,
    pub(crate) modal_layer: Entity<ModalLayer>,
    toast_layer: Entity<ToastLayer>,
    titlebar_item: Option<AnyView>,
    notifications: Notifications,
    suppressed_notifications: HashSet<NotificationId>,
    project: Entity<Project>,
    follower_states: HashMap<CollaboratorId, FollowerState>,
    last_leaders_by_pane: HashMap<WeakEntity<Pane>, CollaboratorId>,
    auto_watch: AutoWatch,
    window_edited: bool,
    last_window_title: Option<String>,
    dirty_items: HashMap<EntityId, Subscription>,
    database_id: Option<WorkspaceId>,
    app_state: Arc<AppState>,
    dispatching_keystrokes: Rc<RefCell<DispatchingKeystrokes>>,
    _subscriptions: Vec<Subscription>,
    _schedule_serialize_ssh_paths: Option<Task<()>>,
    pane_history_timestamp: Arc<AtomicUsize>,
    bounds: Bounds<Pixels>,
    pub centered_layout: bool,
    bounds_save_task_queued: Option<Task<()>>,
    on_prompt_for_new_path: Option<PromptForNewPath>,
    on_prompt_for_open_path: Option<PromptForOpenPath>,
    serializable_items_tx: UnboundedSender<Box<dyn SerializableItemHandle>>,
    _items_serializer: Task<Result<()>>,
    session_id: Option<String>,
    removing: bool,
    sidebar_focus_handle: Option<FocusHandle>,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    deferred_save_items: Vec<Box<dyn WeakItemHandle>>,
}

impl EventEmitter<Event> for Workspace {}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewId {
    pub creator: CollaboratorId,
    pub id: u64,
}

pub struct FollowerState {
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoWatch {
    Off,
    Active { watched_peer: Option<PeerId> },
    Paused,
}

impl AutoWatch {
    pub fn enabled(&self) -> bool {
        matches!(self, AutoWatch::Active { .. } | AutoWatch::Paused)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
    /// Open the workspace in a new window.
    NewWindow,
    /// Add to the window's multi workspace and activate it.
    #[default]
    Activate,
}

impl Workspace {
    pub fn new(
        workspace_id: Option<WorkspaceId>,
        project: Entity<Project>,
        app_state: Arc<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(_) = TrustedWorktrees::try_get_global(cx) {
            cx.observe_global::<SettingsStore>(|_, cx| {
                if ProjectSettings::get_global(cx).session.trust_all_worktrees {
                    if let Some(trusted_worktrees) = TrustedWorktrees::try_get_global(cx) {
                        trusted_worktrees.update(cx, |trusted_worktrees, cx| {
                            trusted_worktrees.auto_trust_all(cx);
                        })
                    }
                }
            })
            .detach();
        }

        cx.observe_global::<SettingsStore>(|workspace, cx| {
            let settings = WorkspaceSettings::get_global(cx);
            workspace.on_update_default_tab_settings(
                settings.default_tab_size,
                settings.default_hard_tabs,
                cx
            );
        })
        .detach();

        cx.subscribe_in(&project, window, move |this, _, event, window, cx| {
            match event {
                project::Event::RemoteIdChanged(_) => {
                    this.update_window_title(window, cx);
                }

                &project::Event::WorktreeRemoved(_) => {
                    this.update_window_title(window, cx);
                    this.update_history(cx);
                }

                &project::Event::WorktreeAdded(id) => {
                    this.update_window_title(window, cx);
                    if this
                        .project()
                        .read(cx)
                        .worktree_for_id(id, cx)
                        .is_some_and(|wt| wt.read(cx).is_visible())
                    {
                        this.update_history(cx);
                    }
                }
                project::Event::WorktreeUpdatedEntries(..) => {
                    this.update_window_title(window, cx);
                }

                project::Event::DisconnectedFromHost => {
                    todo!("DisconnectedFromHost");
                }

                project::Event::DisconnectedFromRemote {
                    server_not_running: _,
                } => {
                    this.update_window_edited(window, cx);
                }

                project::Event::Closed => {
                    window.remove_window();
                }

                project::Event::DeletedEntry(_, entry_id) => {
                    for pane in this.panes.iter() {
                        pane.update(cx, |pane, cx| {
                            pane.handle_deleted_project_item(*entry_id, window, cx)
                        });
                    }
                }

                project::Event::Toast {
                    notification_id,
                    message,
                    link,
                } => this.show_notification(
                    NotificationId::named(notification_id.clone()),
                    cx,
                    |cx| {
                        let mut notification = MessageNotification::new(message.clone(), cx);
                        if let Some(link) = link {
                            notification = notification
                                .more_info_message(link.label)
                                .more_info_url(link.url);
                        }

                        cx.new(|_| notification)
                    },
                ),

                project::Event::HideToast { notification_id } => {
                    this.dismiss_notification(&NotificationId::named(notification_id.clone()), cx)
                }

                project::Event::LanguageServerPrompt(request) => {
                    struct LanguageServerPrompt;

                    this.show_notification(
                        NotificationId::composite::<LanguageServerPrompt>(request.id),
                        cx,
                        |cx| {
                            cx.new(|cx| {
                                notifications::LanguageServerPrompt::new(request.clone(), cx)
                            })
                        },
                    );
                }

                project::Event::AgentLocationChanged => {
                }

                _ => {}
            }
            cx.notify()
        })
        .detach();

        cx.on_focus_lost(window, |this, window, cx| {
            let focus_handle = this.focus_handle(cx);
            window.focus(&focus_handle, cx);
        })
        .detach();

        let weak_handle = cx.entity().downgrade();
        let pane_history_timestamp = Arc::new(AtomicUsize::new(0));

        let center_pane = cx.new(|cx| {
            let mut center_pane = Pane::new(
                weak_handle.clone(),
                project.clone(),
                pane_history_timestamp.clone(),
                None,
                NewFile.boxed_clone(),
                true,
                window,
                cx,
            );
            center_pane.set_can_split(Some(Arc::new(|_, _, _, _| true)));
            center_pane
        });
        cx.subscribe_in(&center_pane, window, Self::handle_pane_event)
            .detach();

        window.focus(&center_pane.focus_handle(cx), cx);

        cx.emit(Event::PaneAdded(center_pane.clone()));

        let any_window_handle = window.window_handle();
        app_state.workspace_store.update(cx, |store, _| {
            store
                .workspaces
                .insert((any_window_handle, weak_handle.clone()));
        });

        cx.emit(Event::WorkspaceCreated(weak_handle.clone()));
        let modal_layer = cx.new(|_| ModalLayer::new());
        let toast_layer = cx.new(|_| ToastLayer::new());
        cx.subscribe(
            &modal_layer,
            |_, _, _: &modal_layer::ModalOpenedEvent, cx| {
                cx.emit(Event::ModalOpened);
            },
        )
        .detach();

        let multi_workspace = window
            .root::<MultiWorkspace>()
            .flatten()
            .map(|mw| mw.downgrade());

        let session_id = app_state.session.read(cx).id().to_owned();

        let (serializable_items_tx, serializable_items_rx) =
            mpsc::unbounded::<Box<dyn SerializableItemHandle>>();
        let _items_serializer = cx.spawn_in(window, async move |this, cx| {
            Self::serialize_items(&this, serializable_items_rx, cx).await
        });

        let subscriptions = vec![
            cx.observe_window_activation(window, Self::on_window_activation_changed),
            cx.observe_window_bounds(window, move |this, window, cx| {
                if this.bounds_save_task_queued.is_some() {
                    return;
                }
                this.bounds_save_task_queued = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    this.update_in(cx, |this, _window, _cx| {
                        this.bounds_save_task_queued.take();
                    })
                    .ok();
                }));
                cx.notify();
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                let window_appearance = window.appearance();

                *SystemAppearance::global_mut(cx) = SystemAppearance(window_appearance.into());

                theme_settings::reload_theme(cx);
            }),
            cx.on_release({
                let weak_handle = weak_handle.clone();
                move |this, cx| {
                    this.app_state.workspace_store.update(cx, move |store, _| {
                        store.workspaces.retain(|(_, weak)| weak != &weak_handle);
                    })
                }
            }),
        ];

        cx.defer_in(window, move |this, window, cx| {
            this.update_window_title(window, cx);
            this.show_initial_notifications(cx);
        });

        let mut center = PaneGroup::new(center_pane.clone());
        center.set_is_center(true);
        center.mark_positions(cx);

        Workspace {
            weak_self: weak_handle.clone(),
            zoomed: None,
            center,
            panes: vec![center_pane.clone()],
            panes_by_item: Default::default(),
            active_pane: center_pane.clone(),
            last_active_center_pane: Some(center_pane.downgrade()),
            modal_layer,
            toast_layer,
            titlebar_item: None,
            notifications: Notifications::default(),
            suppressed_notifications: HashSet::default(),
            project: project.clone(),
            follower_states: Default::default(),
            last_leaders_by_pane: Default::default(),
            auto_watch: AutoWatch::Off,
            dispatching_keystrokes: Default::default(),
            window_edited: false,
            last_window_title: None,
            dirty_items: Default::default(),
            database_id: workspace_id,
            app_state,
            _schedule_serialize_ssh_paths: None,
            _subscriptions: subscriptions,
            pane_history_timestamp,
            workspace_actions: Default::default(),
            // This data will be incorrect, but it will be overwritten by the time it needs to be used.
            bounds: Default::default(),
            centered_layout: false,
            bounds_save_task_queued: None,
            on_prompt_for_new_path: None,
            on_prompt_for_open_path: None,
            serializable_items_tx,
            _items_serializer,
            session_id: Some(session_id),
            removing: false,
            sidebar_focus_handle: None,
            multi_workspace,
            deferred_save_items: Vec::new(),
        }
    }

    pub fn new_local(
        abs_paths: Vec<PathBuf>,
        app_state: Arc<AppState>,
        requesting_window: Option<WindowHandle<MultiWorkspace>>,
        env: Option<HashMap<String, String>>,
        init: Option<Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send>>,
        open_mode: OpenMode,
        cx: &mut App,
    ) -> Task<anyhow::Result<OpenResult>> {
        let project_handle = Project::local(
            app_state.languages.clone(),
            app_state.fs.clone(),
            env,
            Default::default(),
            cx,
        );

        cx.spawn(async move |cx| {
            let mut paths_to_open = Vec::with_capacity(abs_paths.len());
            for path in abs_paths.into_iter() {
                if let Some(canonical) = app_state.fs.canonicalize(&path).await.ok() {
                    paths_to_open.push(canonical)
                } else {
                    paths_to_open.push(path)
                }
            }

            // Get project paths for all of the abs_paths
            let mut project_paths: Vec<(PathBuf, Option<ProjectPath>)> =
                Vec::with_capacity(paths_to_open.len());

            for path in paths_to_open.into_iter() {
                if let Some((_, project_entry)) = cx
                    .update(|cx| {
                        Workspace::project_path_for_path(project_handle.clone(), &path, true, cx)
                    })
                    .await
                    .log_err()
                {
                    project_paths.push((path, Some(project_entry)));
                } else {
                    project_paths.push((path, None));
                }
            }

            let workspace_id = Default::default();
            let window_to_replace = match open_mode {
                OpenMode::NewWindow => None,
                _ => requesting_window,
            };

            let (window, workspace): (WindowHandle<MultiWorkspace>, Entity<Workspace>) =
                if let Some(window) = window_to_replace {
                    let centered_layout = false;

                    let workspace = window.update(cx, |multi_workspace, window, cx| {
                        let workspace = cx.new(|cx| {
                            let mut workspace = Workspace::new(
                                Some(workspace_id),
                                project_handle.clone(),
                                app_state.clone(),
                                window,
                                cx,
                            );

                            workspace.centered_layout = centered_layout;

                            // Call init callback to add items before window renders
                            if let Some(init) = init {
                                init(&mut workspace, window, cx);
                            }

                            workspace
                        });
                        match open_mode {
                            OpenMode::Activate => {
                                multi_workspace.focus_active_workspace(window, cx);
                            }
                            OpenMode::NewWindow => {
                                unreachable!()
                            }
                        }
                        workspace
                    })?;
                    (window, workspace)
                } else {
                    let window_bounds_override = window_bounds_env_override();

                    let (window_bounds, display) = if let Some(bounds) = window_bounds_override {
                        (Some(WindowBounds::Windowed(bounds)), None)
                    } else {
                        // New window - let GPUI's default_bounds() handle cascading
                        (None, None)
                    };

                    // Use the serialized workspace to construct the new window
                    let mut options = cx.update(|cx| (app_state.build_window_options)(display, cx));
                    options.window_bounds = window_bounds;
                    let centered_layout = false;
                    let window = cx.open_window(options, {
                        let app_state = app_state.clone();
                        let project_handle = project_handle.clone();
                        move |window, cx| {
                            let workspace = cx.new(|cx| {
                                let mut workspace = Workspace::new(
                                    Some(workspace_id),
                                    project_handle,
                                    app_state,
                                    window,
                                    cx,
                                );
                                workspace.centered_layout = centered_layout;

                                // Call init callback to add items before window renders
                                if let Some(init) = init {
                                    init(&mut workspace, window, cx);
                                }

                                workspace
                            });
                            cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
                        }
                    })?;
                    let workspace =
                        window.update(cx, |multi_workspace: &mut MultiWorkspace, _, _cx| {
                            multi_workspace.workspace().clone()
                        })?;
                    (window, workspace)
                };

            let opened_items = window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |_workspace: &mut Workspace, cx| {
                        open_items(project_paths, window, cx)
                    })
                })?
                .await
                .unwrap_or_default();

            window
                .update(cx, |_, _window, cx| {
                    workspace.update(cx, |this: &mut Workspace, cx| {
                        this.update_history(cx);
                    });
                })
                .log_err();

            if open_mode == OpenMode::NewWindow || open_mode == OpenMode::Activate {
                window
                    .update(cx, |_, window, _cx| {
                        window.activate_window();
                    })
                    .log_err();
            }

            // Auto-show the security modal if the project has restricted worktrees
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_worktree_trust_security_modal(false, window, cx);
                    });
                })
                .log_err();

            Ok(OpenResult {
                window,
                workspace,
                opened_items,
            })
        })
    }

    pub fn on_update_default_tab_settings(
        &self,
        default_tab_size: NonZeroU32,
        default_hard_tabs: bool,
        cx: &mut App
    ) {
        self.project.update(cx, |project, cx| {
            project.on_update_default_tab_settings(default_tab_size, default_hard_tabs, cx);
        });
    }

    pub fn weak_handle(&self) -> WeakEntity<Self> {
        self.weak_self.clone()
    }

    pub fn open_item_abs_paths(&self, cx: &App) -> Vec<PathBuf> {
        self.items(cx)
            .filter_map(|item| {
                let project_path = item.project_path(cx)?;
                self.project.read(cx).absolute_path(&project_path, cx)
            })
            .collect()
    }

    pub fn is_edited(&self) -> bool {
        self.window_edited
    }

    pub fn set_sidebar_focus_handle(&mut self, handle: Option<FocusHandle>) {
        self.sidebar_focus_handle = handle;
    }

    pub fn multi_workspace(&self) -> Option<&WeakEntity<MultiWorkspace>> {
        self.multi_workspace.as_ref()
    }

    pub fn set_multi_workspace(
        &mut self,
        multi_workspace: WeakEntity<MultiWorkspace>,
    ) {
        self.multi_workspace = Some(multi_workspace);
    }

    pub fn app_state(&self) -> &Arc<AppState> {
        &self.app_state
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn path_style(&self, cx: &App) -> PathStyle {
        self.project.read(cx).path_style(cx)
    }

    pub fn recently_activated_items(&self, cx: &App) -> HashMap<EntityId, usize> {
        let mut history: HashMap<EntityId, usize> = HashMap::default();

        for pane_handle in &self.panes {
            let pane = pane_handle.read(cx);

            for entry in pane.activation_history() {
                history.insert(
                    entry.entity_id,
                    history
                        .get(&entry.entity_id)
                        .cloned()
                        .unwrap_or(0)
                        .max(entry.timestamp),
                );
            }
        }

        history
    }

    pub fn recent_active_item_by_type<T: 'static>(&self, cx: &App) -> Option<Entity<T>> {
        let mut recent_item: Option<Entity<T>> = None;
        let mut recent_timestamp = 0;
        for pane_handle in &self.panes {
            let pane = pane_handle.read(cx);
            let item_map: HashMap<EntityId, &Box<dyn ItemHandle>> =
                pane.items().map(|item| (item.item_id(), item)).collect();
            for entry in pane.activation_history() {
                if entry.timestamp > recent_timestamp
                    && let Some(&item) = item_map.get(&entry.entity_id)
                    && let Some(typed_item) = item.act_as::<T>(cx)
                {
                    recent_timestamp = entry.timestamp;
                    recent_item = Some(typed_item);
                }
            }
        }
        recent_item
    }

    pub fn recent_navigation_history_iter(
        &self,
        cx: &App,
    ) -> impl Iterator<Item = (ProjectPath, Option<PathBuf>)> + use<> {
        let mut abs_paths_opened: HashMap<PathBuf, HashSet<ProjectPath>> = HashMap::default();
        let mut history: HashMap<ProjectPath, (Option<PathBuf>, usize)> = HashMap::default();

        for pane in &self.panes {
            let pane = pane.read(cx);

            pane.nav_history()
                .for_each_entry(cx, &mut |entry, (project_path, fs_path)| {
                    if let Some(fs_path) = &fs_path {
                        abs_paths_opened
                            .entry(fs_path.clone())
                            .or_default()
                            .insert(project_path.clone());
                    }
                    let timestamp = entry.timestamp;
                    match history.entry(project_path) {
                        hash_map::Entry::Occupied(mut entry) => {
                            let (_, old_timestamp) = entry.get();
                            if &timestamp > old_timestamp {
                                entry.insert((fs_path, timestamp));
                            }
                        }
                        hash_map::Entry::Vacant(entry) => {
                            entry.insert((fs_path, timestamp));
                        }
                    }
                });

            if let Some(item) = pane.active_item()
                && let Some(project_path) = item.project_path(cx)
            {
                let fs_path = self.project.read(cx).absolute_path(&project_path, cx);

                if let Some(fs_path) = &fs_path {
                    abs_paths_opened
                        .entry(fs_path.clone())
                        .or_default()
                        .insert(project_path.clone());
                }

                history.insert(project_path, (fs_path, std::usize::MAX));
            }
        }

        history
            .into_iter()
            .sorted_by_key(|(_, (_, order))| *order)
            .map(|(project_path, (fs_path, _))| (project_path, fs_path))
            .rev()
            .filter(move |(history_path, abs_path)| {
                let latest_project_path_opened = abs_path
                    .as_ref()
                    .and_then(|abs_path| abs_paths_opened.get(abs_path))
                    .and_then(|project_paths| {
                        project_paths
                            .iter()
                            .max_by(|b1, b2| b1.worktree_id.cmp(&b2.worktree_id))
                    });

                latest_project_path_opened.is_none_or(|path| path == history_path)
            })
    }

    pub fn recent_navigation_history(
        &self,
        limit: Option<usize>,
        cx: &App,
    ) -> Vec<(ProjectPath, Option<PathBuf>)> {
        self.recent_navigation_history_iter(cx)
            .take(limit.unwrap_or(usize::MAX))
            .collect()
    }

    pub fn clear_navigation_history(&mut self, _window: &mut Window, cx: &mut Context<Workspace>) {
        for pane in &self.panes {
            pane.update(cx, |pane, cx| pane.nav_history_mut().clear(cx));
        }
    }

    fn navigate_history(
        &mut self,
        pane: WeakEntity<Pane>,
        mode: NavigationMode,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        self.navigate_history_impl(
            pane,
            mode,
            window,
            &mut |history, cx| history.pop(mode, cx),
            cx,
        )
    }

    fn navigate_tag_history(
        &mut self,
        pane: WeakEntity<Pane>,
        mode: TagNavigationMode,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        self.navigate_history_impl(
            pane,
            NavigationMode::Normal,
            window,
            &mut |history, _cx| history.pop_tag(mode),
            cx,
        )
    }

    fn navigate_history_impl(
        &mut self,
        pane: WeakEntity<Pane>,
        mode: NavigationMode,
        window: &mut Window,
        cb: &mut dyn FnMut(&mut NavHistory, &mut App) -> Option<NavigationEntry>,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        let to_load = if let Some(pane) = pane.upgrade() {
            pane.update(cx, |pane, cx| {
                window.focus(&pane.focus_handle(cx), cx);
                loop {
                    // Retrieve the weak item handle from the history.
                    let entry = cb(pane.nav_history_mut(), cx)?;

                    // If the item is still present in this pane, then activate it.
                    if let Some(index) = entry
                        .item
                        .upgrade()
                        .and_then(|v| pane.index_for_item(v.as_ref()))
                    {
                        let prev_active_item_index = pane.active_item_index();
                        pane.nav_history_mut().set_mode(mode);
                        pane.activate_item(index, true, true, window, cx);
                        pane.nav_history_mut().set_mode(NavigationMode::Normal);

                        let mut navigated = prev_active_item_index != pane.active_item_index();
                        if let Some(data) = entry.data {
                            navigated |= pane.active_item()?.navigate(data, window, cx);
                        }

                        if navigated {
                            break None;
                        }
                    } else {
                        // If the item is no longer present in this pane, then retrieve its
                        // path info in order to reopen it.
                        break pane
                            .nav_history()
                            .path_for_item(entry.item.id())
                            .map(|(project_path, abs_path)| (project_path, abs_path, entry));
                    }
                }
            })
        } else {
            None
        };

        if let Some((project_path, abs_path, entry)) = to_load {
            // If the item was no longer present, then load it again from its previous path, first try the local path
            let open_by_project_path = self.load_path(project_path.clone(), window, cx);

            cx.spawn_in(window, async move  |workspace, cx| {
                let open_by_project_path = open_by_project_path.await;
                let mut navigated = false;
                match open_by_project_path
                    .with_context(|| format!("Navigating to {project_path:?}"))
                {
                    Ok((project_entry_id, build_item)) => {
                        let prev_active_item_id = pane.update(cx, |pane, _| {
                            pane.nav_history_mut().set_mode(mode);
                            pane.active_item().map(|p| p.item_id())
                        })?;

                        pane.update_in(cx, |pane, window, cx| {
                            let item = pane.open_item(
                                project_entry_id,
                                project_path,
                                true,
                                true,
                                None,
                                window, cx,
                                build_item,
                            );
                            navigated |= Some(item.item_id()) != prev_active_item_id;
                            pane.nav_history_mut().set_mode(NavigationMode::Normal);
                            if let Some(data) = entry.data {
                                navigated |= item.navigate(data, window, cx);
                            }
                        })?;
                    }
                    Err(open_by_project_path_e) => {
                        // Fall back to opening by abs path, in case an external file was opened and closed,
                        // and its worktree is now dropped
                        if let Some(abs_path) = abs_path {
                            let prev_active_item_id = pane.update(cx, |pane, _| {
                                pane.nav_history_mut().set_mode(mode);
                                pane.active_item().map(|p| p.item_id())
                            })?;
                            let open_by_abs_path = workspace.update_in(cx, |workspace, window, cx| {
                                workspace.open_abs_path(abs_path.clone(), OpenOptions { visible: Some(OpenVisible::None), ..Default::default() }, window, cx)
                            })?;
                            match open_by_abs_path
                                .await
                                .with_context(|| format!("Navigating to {abs_path:?}"))
                            {
                                Ok(item) => {
                                    pane.update_in(cx, |pane, window, cx| {
                                        navigated |= Some(item.item_id()) != prev_active_item_id;
                                        pane.nav_history_mut().set_mode(NavigationMode::Normal);
                                        if let Some(data) = entry.data {
                                            navigated |= item.navigate(data, window, cx);
                                        }
                                    })?;
                                }
                                Err(open_by_abs_path_e) => {
                                    log::error!("Failed to navigate history: {open_by_project_path_e:#} and {open_by_abs_path_e:#}");
                                }
                            }
                        }
                    }
                }

                if !navigated {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            Self::navigate_history(workspace, pane, mode, window, cx)
                        })?
                        .await?;
                }

                Ok(())
            })
        } else {
            Task::ready(Ok(()))
        }
    }

    pub fn go_back(
        &mut self,
        pane: WeakEntity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        self.navigate_history(pane, NavigationMode::GoingBack, window, cx)
    }

    pub fn go_forward(
        &mut self,
        pane: WeakEntity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        self.navigate_history(pane, NavigationMode::GoingForward, window, cx)
    }

    pub fn reopen_closed_item(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>> {
        self.navigate_history(
            self.active_pane().downgrade(),
            NavigationMode::ReopeningClosedItem,
            window,
            cx,
        )
    }

    pub fn set_titlebar_item(&mut self, item: AnyView, _: &mut Window, cx: &mut Context<Self>) {
        self.titlebar_item = Some(item);
        cx.notify();
    }

    pub fn set_prompt_for_new_path(&mut self, prompt: PromptForNewPath) {
        self.on_prompt_for_new_path = Some(prompt)
    }

    pub fn set_prompt_for_open_path(&mut self, prompt: PromptForOpenPath) {
        self.on_prompt_for_open_path = Some(prompt)
    }

    pub fn prompt_for_open_path(
        &mut self,
        path_prompt_options: PathPromptOptions,
        lister: DirectoryLister,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
        // TODO: If `on_prompt_for_open_path` is set, we should always use it
        // rather than gating on `use_system_path_prompts`. This would let tests
        // inject a mock without also having to disable the setting.
        if !WorkspaceSettings::get_global(cx).use_system_path_prompts {
            let prompt = self.on_prompt_for_open_path.take().unwrap();
            let rx = prompt(self, lister, window, cx);
            self.on_prompt_for_open_path = Some(prompt);
            rx
        } else {
            let (tx, rx) = oneshot::channel();
            let abs_path = cx.prompt_for_paths(path_prompt_options);

            cx.spawn_in(window, async move |workspace, cx| {
                let Ok(result) = abs_path.await else {
                    return Ok(());
                };

                match result {
                    Ok(result) => {
                        tx.send(result).ok();
                    }
                    Err(err) => {
                        let rx = workspace.update_in(cx, |workspace, window, cx| {
                            workspace.show_portal_error(err.to_string(), cx);
                            let prompt = workspace.on_prompt_for_open_path.take().unwrap();
                            let rx = prompt(workspace, lister, window, cx);
                            workspace.on_prompt_for_open_path = Some(prompt);
                            rx
                        })?;
                        if let Ok(path) = rx.await {
                            tx.send(path).ok();
                        }
                    }
                };
                anyhow::Ok(())
            })
            .detach();

            rx
        }
    }

    pub fn prompt_for_new_path(
        &mut self,
        lister: DirectoryLister,
        suggested_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
        if self.project.read(cx).is_via_remote_server()
            || !WorkspaceSettings::get_global(cx).use_system_path_prompts
        {
            let prompt = self.on_prompt_for_new_path.take().unwrap();
            let rx = prompt(self, lister, suggested_name, window, cx);
            self.on_prompt_for_new_path = Some(prompt);
            return rx;
        }

        let (tx, rx) = oneshot::channel();
        cx.spawn_in(window, async move |workspace, cx| {
            let abs_path = workspace.update(cx, |workspace, cx| {
                let relative_to = workspace
                    .most_recent_active_path(cx)
                    .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                    .or_else(|| {
                        let project = workspace.project.read(cx);
                        project.visible_worktrees(cx).find_map(|worktree| {
                            Some(worktree.read(cx).as_local()?.abs_path().to_path_buf())
                        })
                    })
                    .or_else(std::env::home_dir)
                    .unwrap_or_else(|| PathBuf::from(""));
                cx.prompt_for_new_path(&relative_to, suggested_name.as_deref())
            })?;
            let abs_path = match abs_path.await? {
                Ok(path) => path,
                Err(err) => {
                    let rx = workspace.update_in(cx, |workspace, window, cx| {
                        workspace.show_portal_error(err.to_string(), cx);

                        let prompt = workspace.on_prompt_for_new_path.take().unwrap();
                        let rx = prompt(workspace, lister, suggested_name, window, cx);
                        workspace.on_prompt_for_new_path = Some(prompt);
                        rx
                    })?;
                    if let Ok(path) = rx.await {
                        tx.send(path).ok();
                    }
                    return anyhow::Ok(());
                }
            };

            tx.send(abs_path.map(|path| vec![path])).ok();
            anyhow::Ok(())
        })
        .detach();

        rx
    }

    pub fn titlebar_item(&self) -> Option<AnyView> {
        self.titlebar_item.clone()
    }

    /// Call the given callback with a workspace whose project is local or remote via WSL (allowing host access).
    ///
    /// If the given workspace has a local project, then it will be passed
    /// to the callback. Otherwise, a new empty window will be created.
    pub fn with_local_workspace<T, F>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        callback: F,
    ) -> Task<Result<T>>
    where
        T: 'static,
        F: 'static + FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> T,
    {
        if self.project.read(cx).is_local() {
            Task::ready(Ok(callback(self, window, cx)))
        } else {
            let env = self.project.read(cx).cli_environment(cx);
            let task = Self::new_local(
                Vec::new(),
                self.app_state.clone(),
                None,
                env,
                None,
                OpenMode::Activate,
                cx,
            );
            cx.spawn_in(window, async move |_vh, cx| {
                let OpenResult {
                    window: multi_workspace_window,
                    ..
                } = task.await?;
                multi_workspace_window.update(cx, |multi_workspace, window, cx| {
                    let workspace = multi_workspace.workspace().clone();
                    workspace.update(cx, |workspace, cx| callback(workspace, window, cx))
                })
            })
        }
    }

    pub fn worktrees<'a>(&self, cx: &'a App) -> impl 'a + Iterator<Item = Entity<Worktree>> {
        self.project.read(cx).worktrees(cx)
    }

    pub fn visible_worktrees<'a>(
        &self,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = Entity<Worktree>> {
        self.project.read(cx).visible_worktrees(cx)
    }

    pub fn worktree_scans_complete(&self, cx: &App) -> impl Future<Output = ()> + 'static + use<> {
        let futures = self
            .worktrees(cx)
            .filter_map(|worktree| worktree.read(cx).as_local())
            .map(|worktree| worktree.scan_complete())
            .collect::<Vec<_>>();
        async move {
            for future in futures {
                future.await;
            }
        }
    }

    pub fn close_global(cx: &mut App) {
        cx.defer(|cx| {
            cx.windows().iter().find(|window| {
                window
                    .update(cx, |_, window, _| {
                        if window.is_window_active() {
                            //This can only get called when the window's project connection has been lost
                            //so we don't need to prompt the user for anything and instead just close the window
                            window.remove_window();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
            });
        });
    }

    pub fn prepare_to_close(
        &mut self,
        close_intent: CloseIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        cx.spawn_in(window, async move |this, cx| {
            this.update(cx, |this, _| {
                if close_intent == CloseIntent::CloseWindow {
                    this.removing = true;
                }
            })?;

            #[cfg(target_os = "macos")]
            let save_last_workspace = false;

            // On Linux and Windows, closing the last window should restore the last workspace.
            #[cfg(not(target_os = "macos"))]
            let save_last_workspace = {
                let remaining_workspaces = cx.update(|_window, cx| {
                    cx.windows()
                        .iter()
                        .filter_map(|window| window.downcast::<MultiWorkspace>())
                        .filter_map(|multi_workspace| {
                            multi_workspace
                                .update(cx, |multi_workspace, _, cx| {
                                    multi_workspace.workspace().read(cx).removing
                                })
                                .ok()
                        })
                        .filter(|removing| !removing)
                        .count()
                })?;

                close_intent != CloseIntent::ReplaceWindow && remaining_workspaces == 0
            };

            // Hot-exit silently writes dirty buffers to the DB; only allow it
            // if the workspace will be reachable again, either via session
            // restore or by reopening its folder paths. Otherwise prompt, so
            // we don't orphan the buffers.
            let allow_hot_exit_serialization = close_intent == CloseIntent::Quit
                || save_last_workspace
                || this
                    .read_with(cx, |workspace, cx| {
                        workspace
                            .project
                            .read(cx)
                            .visible_worktrees(cx)
                            .next()
                            .is_some()
                    })
                    .unwrap_or(false);
            let save_result = this
                .update_in(cx, |this, window, cx| {
                    this.save_all_internal(
                        SaveIntent::Close,
                        allow_hot_exit_serialization,
                        window,
                        cx,
                    )
                })?
                .await;

            // If we're not quitting, but closing, we remove the workspace from
            // the current session.
            if close_intent != CloseIntent::Quit
                && !save_last_workspace
                && save_result.as_ref().is_ok_and(|&res| res)
            {
                this.update_in(cx, |this, window, cx| this.remove_from_session(window, cx))?
                    .await;
            }

            save_result
        })
    }

    fn save_all(&mut self, action: &SaveAll, window: &mut Window, cx: &mut Context<Self>) {
        self.save_all_internal(
            action.save_intent.unwrap_or(SaveIntent::SaveAll),
            true,
            window,
            cx,
        )
        .detach_and_log_err(cx);
    }

    fn send_keystrokes(
        &mut self,
        action: &SendKeystrokes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystrokes: Vec<Keystroke> = action
            .0
            .split(' ')
            .flat_map(|k| Keystroke::parse(k).log_err())
            .map(|k| {
                cx.keyboard_mapper()
                    .map_key_equivalent(k, false)
                    .inner()
                    .clone()
            })
            .collect();
        let _ = self.send_keystrokes_impl(keystrokes, window, cx);
    }

    pub fn send_keystrokes_impl(
        &mut self,
        keystrokes: Vec<Keystroke>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Shared<Task<()>> {
        let mut state = self.dispatching_keystrokes.borrow_mut();
        if !state.dispatched.insert(keystrokes.clone()) {
            cx.propagate();
            return state.task.clone().unwrap();
        }

        state.queue.extend(keystrokes);

        let keystrokes = self.dispatching_keystrokes.clone();
        if state.task.is_none() {
            state.task = Some(
                window
                    .spawn(cx, async move |cx| {
                        // limit to 100 keystrokes to avoid infinite recursion.
                        for _ in 0..100 {
                            let keystroke = {
                                let mut state = keystrokes.borrow_mut();
                                let Some(keystroke) = state.queue.pop_front() else {
                                    state.dispatched.clear();
                                    state.task.take();
                                    return;
                                };
                                keystroke
                            };
                            let focus_changed = cx
                                .update(|window, cx| {
                                    let focused = window.focused(cx);
                                    window.dispatch_keystroke(keystroke.clone(), cx);
                                    if window.focused(cx) != focused {
                                        // dispatch_keystroke may cause the focus to change.
                                        // draw's side effect is to schedule the FocusChanged events in the current flush effect cycle
                                        // And we need that to happen before the next keystroke to keep vim mode happy...
                                        // (Note that the tests always do this implicitly, so you must manually test with something like:
                                        //   "bindings": { "g z": ["workspace::SendKeystrokes", ": j <enter> u"]}
                                        // )
                                        window.draw(cx).clear();
                                        return true;
                                    }
                                    false
                                })
                                .unwrap_or(false);

                            if focus_changed {
                                futures_lite::future::yield_now().await;
                            }
                        }

                        *keystrokes.borrow_mut() = Default::default();
                        log::error!("over 100 keystrokes passed to send_keystrokes");
                    })
                    .shared(),
            );
        }
        state.task.clone().unwrap()
    }

    /// Prompts the user to save or discard each dirty item, returning
    /// `true` if they confirmed (saved/discarded everything) or `false`
    /// if they cancelled. Used before removing worktree roots during
    /// thread archival.
    pub fn prompt_to_save_or_discard_dirty_items(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        self.save_all_internal(SaveIntent::Close, true, window, cx)
    }

    fn save_all_internal(
        &mut self,
        mut save_intent: SaveIntent,
        allow_hot_exit_serialization: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        if self.project.read(cx).is_disconnected(cx) {
            return Task::ready(Ok(true));
        }
        let dirty_items = self
            .panes
            .iter()
            .flat_map(|pane| {
                pane.read(cx).items().filter_map(|item| {
                    if item.is_dirty(cx) {
                        item.tab_content_text(0, cx);
                        Some((pane.downgrade(), item.boxed_clone()))
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();

        let project = self.project.clone();
        cx.spawn_in(window, async move |workspace, cx| {
            let dirty_items = if save_intent == SaveIntent::Close && !dirty_items.is_empty() {
                let mut serialize_tasks = Vec::new();
                let mut remaining_dirty_items = Vec::new();
                if allow_hot_exit_serialization {
                    workspace.update_in(cx, |workspace, window, cx| {
                        for (pane, item) in dirty_items {
                            if let Some(task) = item
                                .to_serializable_item_handle(cx)
                                .and_then(|handle| handle.serialize(workspace, true, window, cx))
                            {
                                serialize_tasks.push((pane, item, task));
                            } else {
                                remaining_dirty_items.push((pane, item));
                            }
                        }
                    })?;

                    for (pane, item, task) in serialize_tasks {
                        if task.await.log_err().is_none() {
                            remaining_dirty_items.push((pane, item));
                        }
                    }
                } else {
                    remaining_dirty_items = dirty_items;
                }

                if !remaining_dirty_items.is_empty() {
                    workspace.update(cx, |_, cx| cx.emit(Event::Activate))?;
                }

                if remaining_dirty_items.len() > 1 {
                    let answer = workspace.update_in(cx, |_, window, cx| {
                        cx.emit(Event::Activate);
                        let detail = Pane::file_names_for_prompt(
                            &mut remaining_dirty_items.iter().map(|(_, handle)| handle),
                            cx,
                        );
                        window.prompt(
                            PromptLevel::Warning,
                            "Do you want to save all changes in the following files?",
                            Some(&detail),
                            &["Save all", "Discard all", "Cancel"],
                            cx,
                        )
                    })?;
                    match answer.await.log_err() {
                        Some(0) => save_intent = SaveIntent::SaveAll,
                        Some(1) => save_intent = SaveIntent::Skip,
                        Some(2) => return Ok(false),
                        _ => {}
                    }
                }

                remaining_dirty_items
            } else {
                dirty_items
            };

            for (pane, item) in dirty_items {
                let (singleton, project_entry_ids) = cx.update(|_, cx| {
                    (
                        item.buffer_kind(cx) == ItemBufferKind::Singleton,
                        item.project_entry_ids(cx),
                    )
                })?;
                if (singleton || !project_entry_ids.is_empty())
                    && !Pane::save_item(project.clone(), &pane, &*item, save_intent, cx).await?
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    pub fn open_workspace_for_paths(
        &mut self,
        mut open_mode: OpenMode,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        let requesting_window = window.window_handle().downcast::<MultiWorkspace>();
        let has_worktree = self.project.read(cx).worktrees(cx).next().is_some();
        let has_dirty_items = self.items(cx).any(|item| item.is_dirty(cx));

        let workspace_is_empty = !has_worktree && !has_dirty_items;
        if workspace_is_empty {
            open_mode = OpenMode::Activate;
        }

        let app_state = self.app_state.clone();

        cx.spawn(async move |_, cx| {
            let OpenResult { workspace, .. } = cx
                .update(|cx| {
                    open_paths(
                        &paths,
                        app_state,
                        OpenOptions {
                            requesting_window,
                            open_mode,
                            workspace_matching: if open_mode == OpenMode::NewWindow {
                                WorkspaceMatching::None
                            } else {
                                WorkspaceMatching::default()
                            },
                            ..Default::default()
                        },
                        cx,
                    )
                })
                .await?;
            Ok(workspace)
        })
    }

    #[allow(clippy::type_complexity)]
    pub fn open_paths(
        &mut self,
        mut abs_paths: Vec<PathBuf>,
        options: OpenOptions,
        pane: Option<WeakEntity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<Option<anyhow::Result<Box<dyn ItemHandle>>>>> {
        let fs = self.app_state.fs.clone();

        let caller_ordered_abs_paths = abs_paths.clone();

        // Sort the paths to ensure we add worktrees for parents before their children.
        abs_paths.sort_unstable();
        cx.spawn_in(window, async move |this, cx| {
            let mut tasks = Vec::with_capacity(abs_paths.len());

            for abs_path in &abs_paths {
                let visible = match options.visible.as_ref().unwrap_or(&OpenVisible::None) {
                    OpenVisible::All => Some(true),
                    OpenVisible::None => Some(false),
                    OpenVisible::OnlyFiles => match fs.metadata(abs_path).await.log_err() {
                        Some(Some(metadata)) => Some(!metadata.is_dir),
                        Some(None) => Some(true),
                        None => None,
                    },
                    OpenVisible::OnlyDirectories => match fs.metadata(abs_path).await.log_err() {
                        Some(Some(metadata)) => Some(metadata.is_dir),
                        Some(None) => Some(false),
                        None => None,
                    },
                };
                let project_path = match visible {
                    Some(visible) => match this
                        .update(cx, |this, cx| {
                            Workspace::project_path_for_path(
                                this.project.clone(),
                                abs_path,
                                visible,
                                cx,
                            )
                        })
                        .log_err()
                    {
                        Some(project_path) => project_path.await.log_err(),
                        None => None,
                    },
                    None => None,
                };

                let this = this.clone();
                let abs_path: Arc<Path> = SanitizedPath::new(&abs_path).as_path().into();
                let fs = fs.clone();
                let pane = pane.clone();
                let task = cx.spawn(async move |cx| {
                    let (worktree, project_path) = project_path?;
                    let (entry_is_directory, worktree_is_local) =
                        worktree.read_with(cx, |worktree, _| {
                            let entry = if project_path.path.as_unix_str().is_empty() {
                                worktree.root_entry()
                            } else {
                                worktree.entry_for_path(&project_path.path)
                            };
                            (entry.map(|entry| entry.is_dir()), worktree.is_local())
                        });
                    let is_directory = match entry_is_directory {
                        Some(is_directory) => is_directory,
                        None if worktree_is_local => fs.is_dir(&abs_path).await,
                        None => false,
                    };

                    if is_directory {
                        // Opening a directory should not race to update the active entry.
                        // We'll select/reveal a deterministic final entry after all paths finish opening.
                        None
                    } else {
                        Some(
                            this.update_in(cx, |this, window, cx| {
                                this.open_path(
                                    project_path,
                                    pane,
                                    options.focus.unwrap_or(true),
                                    window,
                                    cx,
                                )
                            })
                            .ok()?
                            .await,
                        )
                    }
                });
                tasks.push(task);
            }

            let results = futures::future::join_all(tasks).await;

            // Determine the winner using the fake/abstract FS metadata, not `Path::is_dir`.
            let mut winner: Option<(PathBuf, bool)> = None;
            for abs_path in caller_ordered_abs_paths.into_iter().rev() {
                if let Some(Some(metadata)) = fs.metadata(&abs_path).await.log_err() {
                    if !metadata.is_dir {
                        winner = Some((abs_path, false));
                        break;
                    }
                    if winner.is_none() {
                        winner = Some((abs_path, true));
                    }
                } else if winner.is_none() {
                    winner = Some((abs_path, false));
                }
            }

            // Compute the winner entry id on the foreground thread and emit once, after all
            // paths finish opening. This avoids races between concurrently-opening paths
            // (directories in particular) and makes the resulting project panel selection
            // deterministic.
            if let Some((winner_abs_path, winner_is_dir)) = winner {
                'emit_winner: {
                    let winner_abs_path: Arc<Path> =
                        SanitizedPath::new(&winner_abs_path).as_path().into();

                    let visible = match options.visible.as_ref().unwrap_or(&OpenVisible::None) {
                        OpenVisible::All => true,
                        OpenVisible::None => false,
                        OpenVisible::OnlyFiles => !winner_is_dir,
                        OpenVisible::OnlyDirectories => winner_is_dir,
                    };

                    let Some(worktree_task) = this
                        .update(cx, |workspace, cx| {
                            workspace.project.update(cx, |project, cx| {
                                project.find_or_create_worktree(
                                    winner_abs_path.as_ref(),
                                    visible,
                                    cx,
                                )
                            })
                        })
                        .ok()
                    else {
                        break 'emit_winner;
                    };

                    let Ok((worktree, _)) = worktree_task.await else {
                        break 'emit_winner;
                    };

                    let Ok(Some(entry_id)) = this.update(cx, |_, cx| {
                        let worktree = worktree.read(cx);
                        let worktree_abs_path = worktree.abs_path();
                        let entry = if winner_abs_path.as_ref() == worktree_abs_path.as_ref() {
                            worktree.root_entry()
                        } else {
                            winner_abs_path
                                .strip_prefix(worktree_abs_path.as_ref())
                                .ok()
                                .and_then(|relative_path| {
                                    let relative_path =
                                        RelPath::new(relative_path, PathStyle::local())
                                            .log_err()?;
                                    worktree.entry_for_path(&relative_path)
                                })
                        }?;
                        Some(entry.id)
                    }) else {
                        break 'emit_winner;
                    };

                    this.update(cx, |workspace, cx| {
                        workspace.project.update(cx, |_, cx| {
                            cx.emit(project::Event::ActiveEntryChanged(Some(entry_id)));
                        });
                    })
                    .ok();
                }
            }

            results
        })
    }

    pub fn open_resolved_path(
        &mut self,
        path: ResolvedPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        match path {
            ResolvedPath::ProjectPath { project_path, .. } => {
                self.open_path(project_path, None, true, window, cx)
            }
            ResolvedPath::AbsPath { path, .. } => self.open_abs_path(
                PathBuf::from(path),
                OpenOptions {
                    visible: Some(OpenVisible::None),
                    ..Default::default()
                },
                window,
                cx,
            ),
        }
    }

    pub fn absolute_path_of_worktree(
        &self,
        worktree_id: WorktreeId,
        cx: &mut Context<Self>,
    ) -> Option<PathBuf> {
        self.project
            .read(cx)
            .worktree_for_id(worktree_id, cx)
            // TODO: use `abs_path` or `root_dir`
            .map(|wt| wt.read(cx).abs_path().as_ref().to_path_buf())
    }

    pub fn project_path_for_path(
        project: Entity<Project>,
        abs_path: &Path,
        visible: bool,
        cx: &mut App,
    ) -> Task<Result<(Entity<Worktree>, ProjectPath)>> {
        let entry = project.update(cx, |project, cx| {
            project.find_or_create_worktree(abs_path, visible, cx)
        });
        cx.spawn(async move |cx| {
            let (worktree, path) = entry.await?;
            let worktree_id = worktree.read_with(cx, |t, _| t.id());
            Ok((worktree, ProjectPath { worktree_id, path }))
        })
    }

    pub fn items<'a>(&'a self, cx: &'a App) -> impl 'a + Iterator<Item = &'a Box<dyn ItemHandle>> {
        self.panes.iter().flat_map(|pane| pane.read(cx).items())
    }

    pub fn item_of_type<T: Item>(&self, cx: &App) -> Option<Entity<T>> {
        self.items_of_type(cx).max_by_key(|item| item.item_id())
    }

    pub fn items_of_type<'a, T: Item>(
        &'a self,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = Entity<T>> {
        self.panes
            .iter()
            .flat_map(|pane| pane.read(cx).items_of_type())
    }

    pub fn active_item(&self, cx: &App) -> Option<Box<dyn ItemHandle>> {
        self.active_pane().read(cx).active_item()
    }

    pub fn active_item_as<I: 'static>(&self, cx: &App) -> Option<Entity<I>> {
        let item = self.active_item(cx)?;
        item.to_any_view().downcast::<I>().ok()
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.active_item(cx).and_then(|item| item.project_path(cx))
    }

    pub fn most_recent_active_path(&self, cx: &App) -> Option<PathBuf> {
        self.recent_navigation_history_iter(cx)
            .filter_map(|(path, abs_path)| {
                let worktree = self
                    .project
                    .read(cx)
                    .worktree_for_id(path.worktree_id, cx)?;
                if !worktree.read(cx).is_visible() {
                    return None;
                }
                let settings_location = SettingsLocation {
                    worktree_id: path.worktree_id,
                    path: &path.path,
                };
                if WorktreeSettings::get(Some(settings_location), cx).is_path_read_only(&path.path)
                {
                    return None;
                }
                abs_path
            })
            .next()
    }

    pub fn save_active_item(
        &mut self,
        save_intent: SaveIntent,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let project = self.project.clone();
        let pane = self.active_pane();
        let item = pane.read(cx).active_item();
        let pane = pane.downgrade();

        window.spawn(cx, async move |cx| {
            if let Some(item) = item {
                Pane::save_item(project, &pane, item.as_ref(), save_intent, cx)
                    .await
                    .map(|_| ())
            } else {
                Ok(())
            }
        })
    }

    pub fn close_inactive_items_and_panes(
        &mut self,
        action: &CloseInactiveTabsAndPanes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self.close_all_internal(
            true,
            action.save_intent.unwrap_or(SaveIntent::Close),
            window,
            cx,
        ) {
            task.detach_and_log_err(cx)
        }
    }

    pub fn close_all_items_and_panes(
        &mut self,
        action: &CloseAllItemsAndPanes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self.close_all_internal(
            false,
            action.save_intent.unwrap_or(SaveIntent::Close),
            window,
            cx,
        ) {
            task.detach_and_log_err(cx)
        }
    }

    /// Closes the active item across all panes.
    pub fn close_item_in_all_panes(
        &mut self,
        action: &CloseItemInAllPanes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_item) = self.active_pane().read(cx).active_item() else {
            return;
        };

        let save_intent = action.save_intent.unwrap_or(SaveIntent::Close);

        if let Some(project_path) = active_item.project_path(cx) {
            self.close_items_with_project_path(
                &project_path,
                save_intent,
                window,
                cx,
            );
        } else {
            let item_id = active_item.item_id();
            self.active_pane().update(cx, |pane, cx| {
                pane.close_item_by_id(item_id, save_intent, window, cx)
                    .detach_and_log_err(cx);
            });
        }
    }

    /// Closes all items with the given project path across all panes.
    pub fn close_items_with_project_path(
        &mut self,
        project_path: &ProjectPath,
        save_intent: SaveIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.panes().to_vec();
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.close_items_for_project_path(
                    project_path,
                    save_intent,
                    window,
                    cx,
                )
                .detach_and_log_err(cx);
            });
        }
    }

    fn close_all_internal(
        &mut self,
        retain_active_pane: bool,
        save_intent: SaveIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let current_pane = self.active_pane();

        let mut tasks = Vec::new();

        if retain_active_pane {
            let current_pane_close = current_pane.update(cx, |pane, cx| {
                pane.close_other_items(
                    &CloseOtherItems {
                        save_intent: None,
                    },
                    None,
                    window,
                    cx,
                )
            });

            tasks.push(current_pane_close);
        }

        for pane in self.panes() {
            if retain_active_pane && pane.entity_id() == current_pane.entity_id() {
                continue;
            }

            let close_pane_items = pane.update(cx, |pane: &mut Pane, cx| {
                pane.close_all_items(
                    &CloseAllItems {
                        save_intent: Some(save_intent),
                    },
                    window,
                    cx,
                )
            });

            tasks.push(close_pane_items)
        }

        if tasks.is_empty() {
            None
        } else {
            Some(cx.spawn_in(window, async move |_, _| {
                for task in tasks {
                    task.await?
                }
                Ok(())
            }))
        }
    }

    pub fn focus_center_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.active_item(cx) {
            item.item_focus_handle(cx).focus(window, cx);
        } else {
            log::error!("Could not find a focus target when switching focus to the center panes",);
        }
    }

    fn dismiss_zoomed_items_to_reveal(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        // If a center pane is zoomed, unzoom it.
        for pane in &self.panes {
            if pane != &self.active_pane {
                pane.update(cx, |pane, cx| pane.set_zoomed(false, cx));
            }
        }

        cx.notify();
    }

    fn add_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(
                self.weak_handle(),
                self.project.clone(),
                self.pane_history_timestamp.clone(),
                None,
                NewFile.boxed_clone(),
                true,
                window,
                cx,
            );
            pane.set_can_split(Some(Arc::new(|_, _, _, _| true)));
            pane
        });
        cx.subscribe_in(&pane, window, Self::handle_pane_event)
            .detach();
        self.panes.push(pane.clone());

        window.focus(&pane.focus_handle(cx), cx);

        cx.emit(Event::PaneAdded(pane.clone()));
        pane
    }

    pub fn add_item_to_center(
        &mut self,
        item: Box<dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(center_pane) = self.last_active_center_pane.clone() {
            if let Some(center_pane) = center_pane.upgrade() {
                center_pane.update(cx, |pane, cx| {
                    pane.add_item(item, true, true, None, window, cx)
                });
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    pub fn add_item_to_active_pane(
        &mut self,
        item: Box<dyn ItemHandle>,
        destination_index: Option<usize>,
        focus_item: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.add_item(
            self.active_pane.clone(),
            item,
            destination_index,
            false,
            focus_item,
            window,
            cx,
        )
    }

    pub fn add_item(
        &mut self,
        pane: Entity<Pane>,
        item: Box<dyn ItemHandle>,
        destination_index: Option<usize>,
        activate_pane: bool,
        focus_item: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        pane.update(cx, |pane, cx| {
            pane.add_item(
                item,
                activate_pane,
                focus_item,
                destination_index,
                window,
                cx,
            )
        });
    }

    pub fn split_item(
        &mut self,
        split_direction: SplitDirection,
        item: Box<dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_pane = self.split_pane(self.active_pane.clone(), split_direction, window, cx);
        self.add_item(new_pane, item, None, true, true, window, cx);
    }

    pub fn open_abs_path(
        &mut self,
        abs_path: PathBuf,
        options: OpenOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        cx.spawn_in(window, async move |workspace, cx| {
            let open_paths_task_result = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_paths(vec![abs_path.clone()], options, None, window, cx)
                })
                .with_context(|| format!("open abs path {abs_path:?} task spawn"))?
                .await;
            anyhow::ensure!(
                open_paths_task_result.len() == 1,
                "open abs path {abs_path:?} task returned incorrect number of results"
            );
            match open_paths_task_result
                .into_iter()
                .next()
                .expect("ensured single task result")
            {
                Some(open_result) => {
                    open_result.with_context(|| format!("open abs path {abs_path:?} task join"))
                }
                None => anyhow::bail!("open abs path {abs_path:?} task returned None"),
            }
        })
    }

    pub fn split_abs_path(
        &mut self,
        abs_path: PathBuf,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        let project_path_task =
            Workspace::project_path_for_path(self.project.clone(), &abs_path, visible, cx);
        cx.spawn_in(window, async move |this, cx| {
            let (_, path) = project_path_task.await?;
            this.update_in(cx, |this, window, cx| this.split_path(path, window, cx))?
                .await
        })
    }

    pub fn open_path(
        &mut self,
        path: impl Into<ProjectPath>,
        pane: Option<WeakEntity<Pane>>,
        focus_item: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        self.open_path_preview(path, pane, focus_item, true, window, cx)
    }

    pub fn open_path_preview(
        &mut self,
        path: impl Into<ProjectPath>,
        pane: Option<WeakEntity<Pane>>,
        focus_item: bool,
        activate: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        let pane = pane.unwrap_or_else(|| {
            self.last_active_center_pane.clone().unwrap_or_else(|| {
                self.panes
                    .first()
                    .expect("There must be an active pane")
                    .downgrade()
            })
        });

        let project_path = path.into();
        let task = self.load_path(project_path.clone(), window, cx);
        window.spawn(cx, async move |cx| {
            let (project_entry_id, build_item) = task.await?;

            pane.update_in(cx, |pane, window, cx| {
                pane.open_item(
                    project_entry_id,
                    project_path,
                    focus_item,
                    activate,
                    None,
                    window,
                    cx,
                    build_item,
                )
            })
        })
    }

    pub fn split_path(
        &mut self,
        path: impl Into<ProjectPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        self.split_path_preview(path, None, window, cx)
    }

    pub fn split_path_preview(
        &mut self,
        path: impl Into<ProjectPath>,
        split_direction: Option<SplitDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Box<dyn ItemHandle>>> {
        let pane = self.last_active_center_pane.clone().unwrap_or_else(|| {
            self.panes
                .first()
                .expect("There must be an active pane")
                .downgrade()
        });

        if let Member::Pane(center_pane) = &self.center.root
            && center_pane.read(cx).items_len() == 0
        {
            return self.open_path(path, Some(pane), true, window, cx);
        }

        let project_path = path.into();
        let task = self.load_path(project_path.clone(), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let (project_entry_id, build_item) = task.await?;
            this.update_in(cx, move |this, window, cx| -> Option<_> {
                let pane = pane.upgrade()?;
                let new_pane = this.split_pane(
                    pane,
                    split_direction.unwrap_or(SplitDirection::Right),
                    window,
                    cx,
                );
                new_pane.update(cx, |new_pane, cx| {
                    Some(new_pane.open_item(
                        project_entry_id,
                        project_path,
                        true,
                        true,
                        None,
                        window,
                        cx,
                        build_item,
                    ))
                })
            })
            .map(|option| option.context("pane was dropped"))?
        })
    }

    fn load_path(
        &mut self,
        path: ProjectPath,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<(Option<ProjectEntryId>, WorkspaceItemBuilder)>> {
        let registry = cx.default_global::<ProjectItemRegistry>().clone();
        registry.open_path(self.project(), &path, window, cx)
    }

    pub fn find_project_item<T>(
        &self,
        pane: &Entity<Pane>,
        project_item: &Entity<T::Item>,
        cx: &App,
    ) -> Option<Entity<T>>
    where
        T: ProjectItem,
    {
        use project::ProjectItem as _;
        let project_item = project_item.read(cx);
        let entry_id = project_item.entry_id(cx);
        let project_path = project_item.project_path(cx);

        let mut item = None;
        if let Some(entry_id) = entry_id {
            item = pane.read(cx).item_for_entry(entry_id, cx);
        }
        if item.is_none()
            && let Some(project_path) = project_path
        {
            item = pane.read(cx).item_for_path(project_path, cx);
        }

        item.and_then(|item| item.downcast::<T>())
    }

    pub fn is_project_item_open<T>(
        &self,
        pane: &Entity<Pane>,
        project_item: &Entity<T::Item>,
        cx: &App,
    ) -> bool
    where
        T: ProjectItem,
    {
        self.find_project_item::<T>(pane, project_item, cx)
            .is_some()
    }

    pub fn auto_watch_state(&self) -> &AutoWatch {
        &self.auto_watch
    }

    pub fn activate_item(
        &mut self,
        item: &dyn ItemHandle,
        activate_pane: bool,
        focus_item: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let result = self.panes.iter().find_map(|pane| {
            pane.read(cx)
                .index_for_item(item)
                .map(|ix| (pane.clone(), ix))
        });
        if let Some((pane, ix)) = result {
            pane.update(cx, |pane, cx| {
                pane.activate_item(ix, activate_pane, focus_item, window, cx)
            });
            true
        } else {
            false
        }
    }

    fn activate_pane_at_index(
        &mut self,
        action: &ActivatePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.center.panes();
        if let Some(pane) = panes.get(action.0).map(|p| (*p).clone()) {
            window.focus(&pane.focus_handle(cx), cx);
        } else {
            self.split_and_clone(self.active_pane.clone(), SplitDirection::Right, window, cx)
                .detach();
        }
    }

    fn move_item_to_pane_at_index(
        &mut self,
        action: &MoveItemToPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.center.panes();
        let destination = match panes.get(action.destination) {
            Some(&destination) => destination.clone(),
            None => {
                if !action.clone && self.active_pane.read(cx).items_len() < 2 {
                    return;
                }
                let direction = SplitDirection::Right;
                let split_off_pane = self
                    .find_pane_in_direction(direction, cx)
                    .unwrap_or_else(|| self.active_pane.clone());
                let new_pane = self.add_pane(window, cx);
                self.center.split(&split_off_pane, &new_pane, direction, cx);
                new_pane
            }
        };

        if action.clone {
            if self
                .active_pane
                .read(cx)
                .active_item()
                .is_some_and(|item| item.can_split(cx))
            {
                clone_active_item(
                    self.database_id(),
                    &self.active_pane,
                    &destination,
                    action.focus,
                    window,
                    cx,
                );
                return;
            }
        }
        move_active_item(
            &self.active_pane,
            &destination,
            action.focus,
            true,
            window,
            cx,
        )
    }

    pub fn activate_next_pane(&mut self, window: &mut Window, cx: &mut App) {
        let panes = self.center.panes();
        if let Some(ix) = panes.iter().position(|pane| **pane == self.active_pane) {
            let next_ix = (ix + 1) % panes.len();
            let next_pane = panes[next_ix].clone();
            window.focus(&next_pane.focus_handle(cx), cx);
        }
    }

    pub fn activate_previous_pane(&mut self, window: &mut Window, cx: &mut App) {
        let panes = self.center.panes();
        if let Some(ix) = panes.iter().position(|pane| **pane == self.active_pane) {
            let prev_ix = cmp::min(ix.wrapping_sub(1), panes.len() - 1);
            let prev_pane = panes[prev_ix].clone();
            window.focus(&prev_pane.focus_handle(cx), cx);
        }
    }

    pub fn activate_last_pane(&mut self, window: &mut Window, cx: &mut App) {
        let last_pane = self.center.last_pane();
        window.focus(&last_pane.focus_handle(cx), cx);
    }

    pub fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut App,
    ) {
        use ActivateInDirectionTarget as Target;
        enum Origin {
            Sidebar,
            Center,
        }

        let origin: Origin = if self
            .sidebar_focus_handle
            .as_ref()
            .is_some_and(|h| h.contains_focused(window, cx))
        {
            Origin::Sidebar
        } else {
            Origin::Center
        };

        let get_last_active_pane = || {
            let pane = self
                .last_active_center_pane
                .clone()
                .unwrap_or_else(|| {
                    self.panes
                        .first()
                        .expect("There must be an active pane")
                        .downgrade()
                })
                .upgrade()?;
            (pane.read(cx).items_len() != 0).then_some(pane)
        };

        let sidebar_target = self
            .sidebar_focus_handle
            .as_ref()
            .map(|h| Target::Sidebar(h.clone()));

        let sidebar_on_right = false;

        let away_from_sidebar = if sidebar_on_right {
            SplitDirection::Left
        } else {
            SplitDirection::Right
        };

        let target = match (origin, direction) {
            (Origin::Sidebar, dir) if dir == away_from_sidebar =>
                get_last_active_pane().map(Target::Pane),

            (Origin::Sidebar, _) => None,

            // We're in the center, so we first try to go to a different pane,
            // otherwise try to go to a dock.
            (Origin::Center, direction) => {
                if let Some(pane) = self.find_pane_in_direction(direction, cx) {
                    Some(Target::Pane(pane))
                } else {
                    match direction {
                        SplitDirection::Up => None,
                        SplitDirection::Down => None,
                        SplitDirection::Left => {
                            if sidebar_on_right {
                                None
                            } else {
                                sidebar_target
                            }
                        }
                        SplitDirection::Right => {
                            if sidebar_on_right {
                                sidebar_target
                            } else {
                                None
                            }
                        }
                    }
                }
            }
        };

        match target {
            Some(ActivateInDirectionTarget::Pane(pane)) => {
                let pane = pane.read(cx);
                if let Some(item) = pane.active_item() {
                    item.item_focus_handle(cx).focus(window, cx);
                } else {
                    log::error!(
                        "Could not find a focus target when in switching focus in {direction} direction for a pane",
                    );
                }
            }
            Some(ActivateInDirectionTarget::Sidebar(focus_handle)) => {
                focus_handle.focus(window, cx);
            }
            None => {}
        }
    }

    pub fn move_item_to_pane_in_direction(
        &mut self,
        action: &MoveItemToPaneInDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destination = match self.find_pane_in_direction(action.direction, cx) {
            Some(destination) => destination,
            None => {
                if !action.clone && self.active_pane.read(cx).items_len() < 2 {
                    return;
                }
                let new_pane = self.add_pane(window, cx);
                self.center
                    .split(&self.active_pane, &new_pane, action.direction, cx);
                new_pane
            }
        };

        if action.clone {
            if self
                .active_pane
                .read(cx)
                .active_item()
                .is_some_and(|item| item.can_split(cx))
            {
                clone_active_item(
                    self.database_id(),
                    &self.active_pane,
                    &destination,
                    action.focus,
                    window,
                    cx,
                );
                return;
            }
        }
        move_active_item(
            &self.active_pane,
            &destination,
            action.focus,
            true,
            window,
            cx,
        );
    }

    pub fn bounding_box_for_pane(&self, pane: &Entity<Pane>) -> Option<Bounds<Pixels>> {
        self.center.bounding_box_for_pane(pane)
    }

    pub fn find_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        cx: &App,
    ) -> Option<Entity<Pane>> {
        self.center
            .find_pane_in_direction(&self.active_pane, direction, cx)
            .cloned()
    }

    pub fn swap_pane_in_direction(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(to) = self.find_pane_in_direction(direction, cx) {
            self.center.swap(&self.active_pane, &to, cx);
            cx.notify();
        }
    }

    pub fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if self
            .center
            .move_to_border(&self.active_pane, direction, cx)
            .unwrap()
        {
            cx.notify();
        }
    }

    pub fn reset_pane_sizes(&mut self, cx: &mut Context<Self>) {
        self.center.reset_pane_sizes(cx);
        cx.notify();
    }

    fn handle_pane_focused(
        &mut self,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_deferred_saves(window, cx);

        // This is explicitly hoisted out of the following check for pane identity as
        // terminal panel panes are not registered as a center panes.
        if self.active_pane != pane {
            self.set_active_pane(&pane, window, cx);
        }

        if self.last_active_center_pane.is_none() {
            self.last_active_center_pane = Some(pane.downgrade());
        }

        self.dismiss_zoomed_items_to_reveal(cx);
        if pane.read(cx).is_zoomed() {
            self.zoomed = Some(pane.downgrade().into());
        } else {
            self.zoomed = None;
        }
        cx.emit(Event::ZoomChanged);
        pane.update(cx, |pane, _| {
            pane.track_alternate_file_items();
        });

        cx.notify();
    }

    fn set_active_pane(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_pane = pane.clone();
        self.active_item_path_changed(true, window, cx);
        self.last_active_center_pane = Some(pane.downgrade());
    }

    fn flush_deferred_saves(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let deferred = std::mem::take(&mut self.deferred_save_items);
        for weak_item in deferred {
            let Some(item) = weak_item.upgrade() else {
                continue;
            };
            // Skip if focus returned to this item
            let focus_handle = item.item_focus_handle(cx);
            if focus_handle.contains_focused(window, cx) {
                continue;
            }
            Pane::autosave_item(item.as_ref(), self.project.clone(), window, cx)
                .detach_and_log_err(cx);
        }
    }

    fn handle_pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &pane::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::Event::AddItem { item } => {
                item.added_to_pane(self, pane.clone(), window, cx);
                cx.emit(Event::ItemAdded {
                    item: item.boxed_clone(),
                });
            }
            pane::Event::Split { direction, mode } => {
                match mode {
                    SplitMode::ClonePane => {
                        self.split_and_clone(pane.clone(), *direction, window, cx)
                            .detach();
                    }
                    SplitMode::EmptyPane => {
                        self.split_pane(pane.clone(), *direction, window, cx);
                    }
                    SplitMode::MovePane => {
                        self.split_and_move(pane.clone(), *direction, window, cx);
                    }
                };
            }
            pane::Event::JoinIntoNext => {
                self.join_pane_into_next(pane.clone(), window, cx);
            }
            pane::Event::JoinAll => {
                self.join_all_panes(window, cx);
            }
            pane::Event::Remove { focus_on_pane } => {
                self.remove_pane(pane.clone(), focus_on_pane.clone(), window, cx);
            }
            pane::Event::ActivateItem {
                local,
                focus_changed,
            } => {
                window.invalidate_character_coordinates();

                pane.update(cx, |pane, _| {
                    pane.track_alternate_file_items();
                });
                if pane == self.active_pane() {
                    self.active_item_path_changed(*focus_changed, window, cx);
                } else if *local {
                    self.set_active_pane(pane, window, cx);
                }
            }
            pane::Event::UserSavedItem { item, save_intent } => {
                cx.emit(Event::UserSavedItem {
                    pane: pane.downgrade(),
                    item: item.boxed_clone(),
                    save_intent: *save_intent,
                });
            }
            pane::Event::ChangeItemTitle => {
                if *pane == self.active_pane {
                    self.active_item_path_changed(false, window, cx);
                }
            }
            pane::Event::RemovedItem { item } => {
                cx.emit(Event::ActiveItemChanged);
                self.update_window_edited(window, cx);
                if let hash_map::Entry::Occupied(entry) = self.panes_by_item.entry(item.item_id())
                    && entry.get().entity_id() == pane.entity_id()
                {
                    entry.remove();
                }
                cx.emit(Event::ItemRemoved {
                    item_id: item.item_id(),
                });
            }
            pane::Event::Focus => {
                window.invalidate_character_coordinates();
                self.handle_pane_focused(pane.clone(), window, cx);
            }
            pane::Event::ZoomIn => {
                if *pane == self.active_pane {
                    pane.update(cx, |pane, cx| pane.set_zoomed(true, cx));
                    if pane.read(cx).has_focus(window, cx) {
                        self.zoomed = Some(pane.downgrade().into());
                        cx.emit(Event::ZoomChanged);
                    }
                    cx.notify();
                }
            }
            pane::Event::ZoomOut => {
                pane.update(cx, |pane, cx| pane.set_zoomed(false, cx));
                self.zoomed = None;
                cx.emit(Event::ZoomChanged);
                cx.notify();
            }
        }
    }

    pub fn split_pane(
        &mut self,
        pane_to_split: Entity<Pane>,
        split_direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let new_pane = self.add_pane(window, cx);
        self.center
            .split(&pane_to_split, &new_pane, split_direction, cx);
        cx.notify();
        new_pane
    }

    pub fn split_and_move(
        &mut self,
        pane: Entity<Pane>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = pane.update(cx, |pane, cx| pane.take_active_item(window, cx)) else {
            return;
        };
        let new_pane = self.add_pane(window, cx);
        new_pane.update(cx, |pane, cx| {
            pane.add_item(item, true, true, None, window, cx)
        });
        self.center.split(&pane, &new_pane, direction, cx);
        cx.notify();
    }

    pub fn split_and_clone(
        &mut self,
        pane: Entity<Pane>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Pane>>> {
        let Some(item) = pane.read(cx).active_item() else {
            return Task::ready(None);
        };
        if !item.can_split(cx) {
            return Task::ready(None);
        }
        let task = item.clone_on_split(self.database_id(), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Some(clone) = task.await {
                this.update_in(cx, |this, window, cx| {
                    let new_pane = this.add_pane(window, cx);
                    let nav_history = pane.read(cx).fork_nav_history();
                    new_pane.update(cx, |pane, cx| {
                        pane.set_nav_history(nav_history, cx);
                        pane.add_item(clone, true, true, None, window, cx)
                    });
                    this.center.split(&pane, &new_pane, direction, cx);
                    cx.notify();
                    new_pane
                })
                .ok()
            } else {
                None
            }
        })
    }

    pub fn join_all_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_item = self.active_pane.read(cx).active_item();
        for pane in &self.panes {
            join_pane_into_active(&self.active_pane, pane, window, cx);
        }
        if let Some(active_item) = active_item {
            self.activate_item(active_item.as_ref(), true, true, window, cx);
        }
        cx.notify();
    }

    pub fn join_pane_into_next(
        &mut self,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next_pane = self
            .find_pane_in_direction(SplitDirection::Right, cx)
            .or_else(|| self.find_pane_in_direction(SplitDirection::Down, cx))
            .or_else(|| self.find_pane_in_direction(SplitDirection::Left, cx))
            .or_else(|| self.find_pane_in_direction(SplitDirection::Up, cx));
        let Some(next_pane) = next_pane else {
            return;
        };
        move_all_items(&pane, &next_pane, window, cx);
        cx.notify();
    }

    fn remove_pane(
        &mut self,
        pane: Entity<Pane>,
        focus_on: Option<Entity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.center.remove(&pane, cx).unwrap() {
            self.force_remove_pane(&pane, &focus_on, window, cx);
            self.last_leaders_by_pane.remove(&pane.downgrade());
            for removed_item in pane.read(cx).items() {
                self.panes_by_item.remove(&removed_item.item_id());
            }

            cx.notify();
        } else {
            self.active_item_path_changed(true, window, cx);
        }
        cx.emit(Event::PaneRemoved);
    }

    pub fn panes_mut(&mut self) -> &mut [Entity<Pane>] {
        &mut self.panes
    }

    pub fn panes(&self) -> &[Entity<Pane>] {
        &self.panes
    }

    pub fn active_pane(&self) -> &Entity<Pane> {
        &self.active_pane
    }

    pub fn adjacent_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        self.find_pane_in_direction(SplitDirection::Right, cx)
            .unwrap_or_else(|| {
                self.split_pane(self.active_pane.clone(), SplitDirection::Right, window, cx)
            })
    }

    pub fn pane_for(&self, handle: &dyn ItemHandle) -> Option<Entity<Pane>> {
        self.pane_for_item_id(handle.item_id())
    }

    pub fn pane_for_item_id(&self, item_id: EntityId) -> Option<Entity<Pane>> {
        let weak_pane = self.panes_by_item.get(&item_id)?;
        weak_pane.upgrade()
    }

    pub fn pane_for_entity_id(&self, entity_id: EntityId) -> Option<Entity<Pane>> {
        self.panes
            .iter()
            .find(|pane| pane.entity_id() == entity_id)
            .cloned()
    }

    pub fn toggle_stacked_tabs(
        &mut self,
        _: &ToggleStackedTabs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fs = <dyn fs::Fs>::global(cx);

        settings::update_settings_file(fs.clone(), cx, move |content, _cx| {
            let tab_bar = content.tab_bar.get_or_insert_default();
            tab_bar.show_tab_bar_stacked = Some(!tab_bar.show_tab_bar_stacked.unwrap_or(false));
        });

        cx.notify();
    }

    pub fn show_tab_bar_stacked(&self, cx: &App) -> bool {
        TabBarSettings::get_global(cx).show_tab_bar_stacked
    }

    pub(crate) fn active_item_path_changed(
        &mut self,
        focus_changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(Event::ActiveItemChanged);
        let active_entry = self.active_project_path(cx);
        self.project.update(cx, |project, cx| {
            project.set_active_path(active_entry.clone(), cx)
        });

        if focus_changed && let Some(project_path) = &active_entry {
            let git_store_entity = self.project.read(cx).git_store().clone();
            git_store_entity.update(cx, |git_store, cx| {
                git_store.set_active_repo_for_path(project_path, cx);
            });
        }

        self.update_window_title(window, cx);
    }

    fn update_window_title(&mut self, window: &mut Window, cx: &mut App) {
        let project = self.project().read(cx);
        let mut title = String::new();

        for (i, worktree) in project.visible_worktrees(cx).enumerate() {
            let name = worktree.read(cx).root_name_str();

            if i > 0 {
                title.push_str(", ");
            }
            title.push_str(name);
        }

        if title.is_empty() {
            title = "empty project".to_string();
        }

        let active_project_path = self.active_item(cx).and_then(|item| item.project_path(cx));

        if let Some(path) = active_project_path.as_ref() {
            let filename = path.path.file_name().or_else(|| {
                Some(
                    project
                        .worktree_for_id(path.worktree_id, cx)?
                        .read(cx)
                        .root_name_str(),
                )
            });

            if let Some(filename) = filename {
                title.push_str(" — ");
                title.push_str(filename.as_ref());
            }
        }

        let document_path = active_project_path
            .as_ref()
            .and_then(|path| project.absolute_path(path, cx));
        window.set_document_path(document_path.as_deref());

        if let Some(last_title) = self.last_window_title.as_ref()
            && &title == last_title
        {
            return;
        }
        window.set_window_title(&title);
        SystemWindowTabController::update_tab_title(
            cx,
            window.window_handle().window_id(),
            SharedString::from(&title),
        );
        self.last_window_title = Some(title);
    }

    fn update_window_edited(&mut self, window: &mut Window, cx: &mut App) {
        let is_edited = !self.project.read(cx).is_disconnected(cx) && !self.dirty_items.is_empty();
        if is_edited != self.window_edited {
            self.window_edited = is_edited;
            window.set_window_edited(self.window_edited)
        }
    }

    fn update_item_dirty_state(
        &mut self,
        item: &dyn ItemHandle,
        window: &mut Window,
        cx: &mut App,
    ) {
        let is_dirty = item.is_dirty(cx);
        let item_id = item.item_id();
        let was_dirty = self.dirty_items.contains_key(&item_id);
        if is_dirty == was_dirty {
            return;
        }
        if was_dirty {
            self.dirty_items.remove(&item_id);
            self.update_window_edited(window, cx);
            return;
        }

        let workspace = self.weak_handle();
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };
        let on_release_callback = Box::new(move |cx: &mut App| {
            window_handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.dirty_items.remove(&item_id);
                            workspace.update_window_edited(window, cx)
                        })
                        .ok();
                })
                .ok();
        });

        let s = item.on_release(cx, on_release_callback);
        self.dirty_items.insert(item_id, s);
        self.update_window_edited(window, cx);
    }

    fn render_notifications(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Div> {
        if self.notifications.is_empty() {
            None
        } else {
            Some(
                div()
                    .absolute()
                    .right_3()
                    .bottom_3()
                    .w_112()
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .gap_2()
                    .children(
                        self.notifications
                            .iter()
                            .map(|(_, notification)| notification.clone().into_any()),
                    ),
            )
        }
    }

    // RPC handlers

    pub fn on_window_activation_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            // When window is deactivated, flush any deferred saves since focus has left the window
            self.flush_deferred_saves(window, cx);
            for pane in &self.panes {
                pane.update(cx, |pane, cx| {
                    if let Some(item) = pane.active_item() {
                        item.workspace_deactivated(window, cx);
                    }
                    for item in pane.items() {
                        if matches!(
                            item.workspace_settings(cx).autosave,
                            AutosaveSetting::OnWindowChange | AutosaveSetting::OnFocusChange
                        ) {
                            Pane::autosave_item(item.as_ref(), self.project.clone(), window, cx)
                                .detach_and_log_err(cx);
                        }
                    }
                });
            }
        }
    }

    pub fn database_id(&self) -> Option<WorkspaceId> {
        self.database_id
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    pub fn root_paths(&self, cx: &App) -> Vec<Arc<Path>> {
        let project = self.project().read(cx);
        project
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path())
            .collect::<Vec<_>>()
    }

    fn remove_from_session(&mut self, _window: &mut Window, _cx: &mut App) -> Task<()> {
        self.session_id.take();
        Task::ready(())
    }

    fn force_remove_pane(
        &mut self,
        pane: &Entity<Pane>,
        focus_on: &Option<Entity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let removing_active_pane = self.active_pane() == pane;
        self.panes.retain(|p| p != pane);
        if let Some(focus_on) = focus_on {
            if removing_active_pane {
                self.set_active_pane(focus_on, window, cx);
            }
            focus_on.update(cx, |pane, cx| window.focus(&pane.focus_handle(cx), cx));
        } else if removing_active_pane {
            let fallback_pane = self.panes.last().unwrap().clone();
            self.set_active_pane(&fallback_pane, window, cx);
            if !self.has_active_modal(window, cx) {
                fallback_pane.update(cx, |pane, cx| window.focus(&pane.focus_handle(cx), cx));
            }
        }
        if self.last_active_center_pane == Some(pane.downgrade()) {
            self.last_active_center_pane = None;
        }
        cx.notify();
    }

    fn has_any_items_open(&self, cx: &App) -> bool {
        self.panes.iter().any(|pane| pane.read(cx).items_len() > 0)
    }

    fn workspace_location(&self, cx: &App) -> WorkspaceLocation {
        let paths = PathList::new(&self.root_paths(cx));
        if self.project.read(cx).is_local() {
            if !paths.is_empty() || self.has_any_items_open(cx) {
                WorkspaceLocation::Location
            } else {
                WorkspaceLocation::DetachFromSession
            }
        } else {
            WorkspaceLocation::None
        }
    }

    fn update_history(&self, cx: &mut App) {
        let Some(id) = self.database_id() else {
            return;
        };
        if !self.project.read(cx).is_local() {
            return;
        }
        if let Some(manager) = HistoryManager::global(cx) {
            let paths = PathList::new(&self.root_paths(cx));
            manager.update(cx, |this, cx| {
                this.update_history(id, HistoryManagerEntry::new(id, &paths), cx);
            });
        }
    }

    async fn serialize_items(
        this: &WeakEntity<Self>,
        items_rx: UnboundedReceiver<Box<dyn SerializableItemHandle>>,
        cx: &mut AsyncWindowContext,
    ) -> Result<()> {
        const CHUNK_SIZE: usize = 200;

        let mut serializable_items = items_rx.ready_chunks(CHUNK_SIZE);

        while let Some(items_received) = serializable_items.next().await {
            let unique_items =
                items_received
                    .into_iter()
                    .fold(HashMap::default(), |mut acc, item| {
                        acc.entry(item.item_id()).or_insert(item);
                        acc
                    });

            // We use into_iter() here so that the references to the items are moved into
            // the tasks and not kept alive while we're sleeping.
            for (_, item) in unique_items.into_iter() {
                if let Ok(Some(task)) = this.update_in(cx, |workspace, window, cx| {
                    item.serialize(workspace, false, window, cx)
                }) {
                    cx.background_spawn(async move { task.await.log_err() })
                        .detach();
                }
            }

            cx.background_executor()
                .timer(SERIALIZATION_THROTTLE_TIME)
                .await;
        }

        Ok(())
    }

    pub(crate) fn enqueue_item_serialization(
        &mut self,
        item: Box<dyn SerializableItemHandle>,
    ) -> Result<()> {
        self.serializable_items_tx
            .unbounded_send(item)
            .map_err(|err| anyhow!("failed to send serializable item over channel: {err}"))
    }

    pub fn key_context(&self, cx: &App) -> KeyContext {
        let mut context = KeyContext::new_with_defaults();
        context.add("Workspace");
        context.set("keyboard_layout", cx.keyboard_layout().name().to_string());
        context
    }

    /// Multiworkspace uses this to add workspace action handling to itself
    pub fn actions(&self, div: Div, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.add_workspace_actions_listeners(div, window, cx)
            .on_action(cx.listener(
                |_workspace, action_sequence: &settings::ActionSequence, window, cx| {
                    for action in &action_sequence.0 {
                        window.dispatch_action(action.boxed_clone(), cx);
                    }
                },
            ))
            .on_action(cx.listener(Self::close_inactive_items_and_panes))
            .on_action(cx.listener(Self::close_all_items_and_panes))
            .on_action(cx.listener(Self::close_item_in_all_panes))
            .on_action(cx.listener(Self::save_all))
            .on_action(cx.listener(Self::send_keystrokes))
            .on_action(cx.listener(Self::activate_pane_at_index))
            .on_action(cx.listener(Self::move_item_to_pane_at_index))
            .on_action(cx.listener(Self::toggle_theme_mode))
            .on_action(cx.listener(Self::toggle_stacked_tabs))
            .on_action(cx.listener(|workspace, action: &Save, window, cx| {
                workspace
                    .save_active_item(action.save_intent.unwrap_or(SaveIntent::Save), window, cx)
                    .detach_and_prompt_err("Failed to save", window, cx, |_, _, _| None);
            }))
            .on_action(cx.listener(|workspace, _: &FormatAndSave, window, cx| {
                workspace
                    .save_active_item(SaveIntent::FormatAndSave, window, cx)
                    .detach_and_prompt_err("Failed to save", window, cx, |_, _, _| None);
            }))
            .on_action(cx.listener(|workspace, _: &SaveWithoutFormat, window, cx| {
                workspace
                    .save_active_item(SaveIntent::SaveWithoutFormat, window, cx)
                    .detach_and_prompt_err("Failed to save", window, cx, |_, _, _| None);
            }))
            .on_action(cx.listener(|workspace, _: &SaveAs, window, cx| {
                workspace
                    .save_active_item(SaveIntent::SaveAs, window, cx)
                    .detach_and_prompt_err("Failed to save", window, cx, |_, _, _| None);
            }))
            .on_action(
                cx.listener(|workspace, _: &ActivatePreviousPane, window, cx| {
                    workspace.activate_previous_pane(window, cx)
                }),
            )
            .on_action(cx.listener(|workspace, _: &ActivateNextPane, window, cx| {
                workspace.activate_next_pane(window, cx)
            }))
            .on_action(cx.listener(|workspace, _: &ActivateLastPane, window, cx| {
                workspace.activate_last_pane(window, cx)
            }))
            .on_action(
                cx.listener(|workspace, _: &ActivateNextWindow, _window, cx| {
                    workspace.activate_next_window(cx)
                }),
            )
            .on_action(
                cx.listener(|workspace, _: &ActivatePreviousWindow, _window, cx| {
                    workspace.activate_previous_window(cx)
                }),
            )
            .on_action(cx.listener(|workspace, _: &ActivatePaneLeft, window, cx| {
                workspace.activate_pane_in_direction(SplitDirection::Left, window, cx)
            }))
            .on_action(cx.listener(|workspace, _: &ActivatePaneRight, window, cx| {
                workspace.activate_pane_in_direction(SplitDirection::Right, window, cx)
            }))
            .on_action(cx.listener(|workspace, _: &ActivatePaneUp, window, cx| {
                workspace.activate_pane_in_direction(SplitDirection::Up, window, cx)
            }))
            .on_action(cx.listener(|workspace, _: &ActivatePaneDown, window, cx| {
                workspace.activate_pane_in_direction(SplitDirection::Down, window, cx)
            }))
            .on_action(cx.listener(
                |workspace, action: &MoveItemToPaneInDirection, window, cx| {
                    workspace.move_item_to_pane_in_direction(action, window, cx)
                },
            ))
            .on_action(cx.listener(|workspace, _: &SwapPaneLeft, _, cx| {
                workspace.swap_pane_in_direction(SplitDirection::Left, cx)
            }))
            .on_action(cx.listener(|workspace, _: &SwapPaneRight, _, cx| {
                workspace.swap_pane_in_direction(SplitDirection::Right, cx)
            }))
            .on_action(cx.listener(|workspace, _: &SwapPaneUp, _, cx| {
                workspace.swap_pane_in_direction(SplitDirection::Up, cx)
            }))
            .on_action(cx.listener(|workspace, _: &SwapPaneDown, _, cx| {
                workspace.swap_pane_in_direction(SplitDirection::Down, cx)
            }))
            .on_action(cx.listener(|workspace, _: &SwapPaneAdjacent, window, cx| {
                const DIRECTION_PRIORITY: [SplitDirection; 4] = [
                    SplitDirection::Down,
                    SplitDirection::Up,
                    SplitDirection::Right,
                    SplitDirection::Left,
                ];
                for dir in DIRECTION_PRIORITY {
                    if workspace.find_pane_in_direction(dir, cx).is_some() {
                        workspace.swap_pane_in_direction(dir, cx);
                        workspace.activate_pane_in_direction(dir.opposite(), window, cx);
                        break;
                    }
                }
            }))
            .on_action(cx.listener(|workspace, _: &MovePaneLeft, _, cx| {
                workspace.move_pane_to_border(SplitDirection::Left, cx)
            }))
            .on_action(cx.listener(|workspace, _: &MovePaneRight, _, cx| {
                workspace.move_pane_to_border(SplitDirection::Right, cx)
            }))
            .on_action(cx.listener(|workspace, _: &MovePaneUp, _, cx| {
                workspace.move_pane_to_border(SplitDirection::Up, cx)
            }))
            .on_action(cx.listener(|workspace, _: &MovePaneDown, _, cx| {
                workspace.move_pane_to_border(SplitDirection::Down, cx)
            }))
            .on_action(cx.listener(
                |workspace: &mut Workspace, _: &ClearAllNotifications, _, cx| {
                    workspace.clear_all_notifications(cx);
                },
            ))
            .on_action(cx.listener(
                |workspace: &mut Workspace, _: &ClearNavigationHistory, window, cx| {
                    workspace.clear_navigation_history(window, cx);
                },
            ))
            .on_action(cx.listener(
                |workspace: &mut Workspace, _: &SuppressNotification, _, cx| {
                    if let Some((notification_id, _)) = workspace.notifications.pop() {
                        workspace.suppress_notification(&notification_id, cx);
                    }
                },
            ))
            .on_action(cx.listener(
                |workspace: &mut Workspace, _: &ToggleWorktreeSecurity, window, cx| {
                    workspace.show_worktree_trust_security_modal(true, window, cx);
                },
            ))
            .on_action(
                cx.listener(|_: &mut Workspace, _: &ClearTrustedWorktrees, _, cx| {
                    if let Some(trusted_worktrees) = TrustedWorktrees::try_get_global(cx) {
                        trusted_worktrees.update(cx, |trusted_worktrees, _| {
                            trusted_worktrees.clear_trusted_paths()
                        });
                    }
                }),
            )
            .on_action(cx.listener(
                |workspace: &mut Workspace, _: &ReopenClosedItem, window, cx| {
                    workspace.reopen_closed_item(window, cx).detach();
                },
            ))
            .on_action(cx.listener(Workspace::toggle_centered_layout))
            .on_action(
                cx.listener(|workspace, _: &ToggleReadOnlyFile, window, cx| {
                    let pane = workspace.active_pane().clone();
                    if let Some(item) = pane.read(cx).active_item() {
                        item.toggle_read_only(window, cx);
                    }
                }),
            )
            .on_action(cx.listener(|workspace, _: &FocusCenterPane, window, cx| {
                workspace.focus_center_pane(window, cx);
            }))
            .on_action(cx.listener(Workspace::cancel))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_random_database_id(&mut self) {
        self.database_id = Some(WorkspaceId(Uuid::new_v4().as_u64_pair().0 as i64));
    }

    pub fn register_action<A: Action>(
        &mut self,
        callback: impl Fn(&mut Self, &A, &mut Window, &mut Context<Self>) + 'static,
    ) -> &mut Self {
        let callback = Arc::new(callback);

        self.workspace_actions.push(Box::new(move |div, _, _, cx| {
            let callback = callback.clone();
            div.on_action(cx.listener(move |workspace, event, window, cx| {
                (callback)(workspace, event, window, cx)
            }))
        }));
        self
    }
    pub fn register_action_renderer(
        &mut self,
        callback: impl Fn(Div, &Workspace, &mut Window, &mut Context<Self>) -> Div + 'static,
    ) -> &mut Self {
        self.workspace_actions.push(Box::new(callback));
        self
    }

    fn add_workspace_actions_listeners(
        &self,
        mut div: Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        for action in self.workspace_actions.iter() {
            div = (action)(div, self, window, cx)
        }
        div
    }

    pub fn has_active_modal(&self, _: &mut Window, cx: &mut App) -> bool {
        self.modal_layer.read(cx).has_active_modal()
    }

    pub fn active_modal<V: ManagedView + 'static>(&self, cx: &App) -> Option<Entity<V>> {
        self.modal_layer.read(cx).active_modal()
    }

    /// Toggles a modal of type `V`. If a modal of the same type is currently active,
    /// it will be hidden. If a different modal is active, it will be replaced with the new one.
    /// If no modal is active, the new modal will be shown.
    ///
    /// If closing the current modal fails (e.g., due to `on_before_dismiss` returning
    /// `DismissDecision::Dismiss(false)` or `DismissDecision::Pending`), the new modal
    /// will not be shown.
    pub fn toggle_modal<V: ModalView, B>(&mut self, window: &mut Window, cx: &mut App, build: B)
    where
        B: FnOnce(&mut Window, &mut Context<V>) -> V,
    {
        self.modal_layer.update(cx, |modal_layer, cx| {
            modal_layer.toggle_modal(window, cx, build)
        })
    }

    pub fn hide_modal(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.modal_layer
            .update(cx, |modal_layer, cx| modal_layer.hide_modal(window, cx))
    }

    pub fn toggle_status_toast<V: ToastView>(&mut self, entity: Entity<V>, cx: &mut App) {
        self.toast_layer
            .update(cx, |toast_layer, cx| toast_layer.toggle_toast(cx, entity))
    }

    pub fn toggle_centered_layout(
        &mut self,
        _: &ToggleCenteredLayout,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.centered_layout = !self.centered_layout;
        cx.notify();
    }

    fn adjust_padding(padding: Option<f32>) -> f32 {
        padding
            .unwrap_or(CenteredPaddingSettings::default().0)
            .clamp(
                CenteredPaddingSettings::MIN_PADDING,
                CenteredPaddingSettings::MAX_PADDING,
            )
    }

    pub fn for_window(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
        window
            .root::<MultiWorkspace>()
            .flatten()
            .map(|multi_workspace| multi_workspace.read(cx).workspace().clone())
    }

    pub fn zoomed_item(&self) -> Option<&AnyWeakView> {
        self.zoomed.as_ref()
    }

    pub fn activate_next_window(&mut self, cx: &mut Context<Self>) {
        let Some(current_window_id) = cx.active_window().map(|a| a.window_id()) else {
            return;
        };
        let windows = cx.windows();
        let next_window =
            SystemWindowTabController::get_next_tab_group_window(cx, current_window_id).or_else(
                || {
                    windows
                        .iter()
                        .cycle()
                        .skip_while(|window| window.window_id() != current_window_id)
                        .nth(1)
                },
            );

        if let Some(window) = next_window {
            window
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        }
    }

    pub fn activate_previous_window(&mut self, cx: &mut Context<Self>) {
        let Some(current_window_id) = cx.active_window().map(|a| a.window_id()) else {
            return;
        };
        let windows = cx.windows();
        let prev_window =
            SystemWindowTabController::get_prev_tab_group_window(cx, current_window_id).or_else(
                || {
                    windows
                        .iter()
                        .rev()
                        .cycle()
                        .skip_while(|window| window.window_id() != current_window_id)
                        .nth(1)
                },
            );

        if let Some(window) = prev_window {
            window
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        }
    }

    pub fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if cx.stop_active_drag(window) {
        } else if let Some((notification_id, _)) = self.notifications.pop() {
            dismiss_app_notification(&notification_id, cx);
        } else {
            cx.propagate();
        }
    }

    fn toggle_theme_mode(&mut self, _: &ToggleMode, _window: &mut Window, cx: &mut Context<Self>) {
        let current_mode = ThemeSettings::get_global(cx).theme.mode();
        let next_mode = match current_mode {
            Some(theme_settings::ThemeAppearanceMode::Light) => {
                theme_settings::ThemeAppearanceMode::Dark
            }
            Some(theme_settings::ThemeAppearanceMode::Dark) => {
                theme_settings::ThemeAppearanceMode::Light
            }
            Some(theme_settings::ThemeAppearanceMode::System) | None => {
                match cx.theme().appearance() {
                    theme::Appearance::Light => theme_settings::ThemeAppearanceMode::Dark,
                    theme::Appearance::Dark => theme_settings::ThemeAppearanceMode::Light,
                }
            }
        };

        let fs = self.project().read(cx).fs().clone();
        settings::update_settings_file(fs, cx, move |settings, _cx| {
            theme_settings::set_mode(settings, next_mode);
        });
    }

    pub fn show_worktree_trust_security_modal(
        &mut self,
        toggle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(security_modal) = self.active_modal::<SecurityModal>(cx) {
            if toggle {
                security_modal.update(cx, |security_modal, cx| {
                    security_modal.dismiss(cx);
                })
            } else {
                security_modal.update(cx, |security_modal, cx| {
                    security_modal.refresh_restricted_paths(cx);
                });
            }
        } else {
            let has_restricted_worktrees = TrustedWorktrees::has_restricted_worktrees(
                &self.project().read(cx).worktree_store(),
                cx,
            );
            if has_restricted_worktrees {
                let project = self.project().read(cx);
                let remote_host = project
                    .remote_connection_options(cx)
                    .map(RemoteHostLocation::from);
                let worktree_store = project.worktree_store().downgrade();
                self.toggle_modal(window, cx, |_, cx| {
                    SecurityModal::new(worktree_store, remote_host, cx)
                });
            }
        }
    }
}

/// Workspace-local view of a remote participant's location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticipantLocation {
    SharedProject { project_id: u64 },
    UnsharedProject,
    External,
}

impl ParticipantLocation {
    pub fn from_proto(location: Option<proto::ParticipantLocation>) -> Result<Self> {
        match location
            .and_then(|l| l.variant)
            .context("participant location was not provided")?
        {
            proto::participant_location::Variant::SharedProject(project) => {
                Ok(Self::SharedProject {
                    project_id: project.id,
                })
            }
            proto::participant_location::Variant::UnsharedProject(_) => Ok(Self::UnsharedProject),
            proto::participant_location::Variant::External(_) => Ok(Self::External),
        }
    }
}

pub enum ActiveCallEvent {
    ParticipantLocationChanged { participant_id: PeerId },
    RemoteVideoTracksChanged { participant_id: PeerId },
    LocalScreenShareStarted,
    LocalScreenShareStopped,
    RoomLeft,
}

fn window_bounds_env_override() -> Option<Bounds<Pixels>> {
    ZED_WINDOW_POSITION
        .zip(*ZED_WINDOW_SIZE)
        .map(|(position, size)| Bounds {
            origin: position,
            size,
        })
}

fn open_items(
    project_paths_to_open: Vec<(PathBuf, Option<ProjectPath>)>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl 'static + Future<Output = Result<Vec<Option<Result<Box<dyn ItemHandle>>>>>> + use<> {
    cx.spawn_in(window, async move |workspace, cx| {
        let mut opened_items = Vec::with_capacity(project_paths_to_open.len());
        for _ in 0..project_paths_to_open.len() {
            opened_items.push(None);
        }
        assert!(opened_items.len() == project_paths_to_open.len());

        let tasks =
            project_paths_to_open
                .into_iter()
                .enumerate()
                .map(|(ix, (abs_path, project_path))| {
                    let workspace = workspace.clone();
                    cx.spawn(async move |cx| {
                        let file_project_path = project_path?;
                        let abs_path_task = workspace.update(cx, |workspace, cx| {
                            workspace.project().update(cx, |project, cx| {
                                project.resolve_abs_path(abs_path.to_string_lossy().as_ref(), cx)
                            })
                        });

                        // We only want to open file paths here. If one of the items
                        // here is a directory, it was already opened further above
                        // with a `find_or_create_worktree`.
                        if let Ok(task) = abs_path_task
                            && task.await.is_none_or(|p| p.is_file())
                        {
                            return Some((
                                ix,
                                workspace
                                    .update_in(cx, |workspace, window, cx| {
                                        workspace.open_path(
                                            file_project_path,
                                            None,
                                            true,
                                            window,
                                            cx,
                                        )
                                    })
                                    .log_err()?
                                    .await,
                            ));
                        }
                        None
                    })
                });

        let tasks = tasks.collect::<Vec<_>>();

        let tasks = futures::future::join_all(tasks);
        for (ix, path_open_result) in tasks.await.into_iter().flatten() {
            opened_items[ix] = Some(path_open_result);
        }

        Ok(opened_items)
    })
}

#[derive(Clone)]
enum ActivateInDirectionTarget {
    Pane(Entity<Pane>),
    Sidebar(FocusHandle),
}

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_pane.focus_handle(cx)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        static FIRST_PAINT: AtomicBool = AtomicBool::new(true);
        if FIRST_PAINT.swap(false, std::sync::atomic::Ordering::Relaxed) {
            log::info!("Rendered first frame");
        }

        let centered_layout = self.centered_layout
            && self.center.panes().len() == 1
            && self.active_item(cx).is_some();
        let render_padding = |size| {
            (size > 0.0).then(|| {
                div()
                    .h_full()
                    .w(relative(size))
                    .bg(cx.theme().colors().editor_background)
                    .border_color(cx.theme().colors().pane_group_border)
            })
        };
        let paddings = if centered_layout {
            let settings = WorkspaceSettings::get_global(cx).centered_layout;
            (
                render_padding(Self::adjust_padding(
                    settings.left_padding.map(|padding| padding.0),
                )),
                render_padding(Self::adjust_padding(
                    settings.right_padding.map(|padding| padding.0),
                )),
            )
        } else {
            (None, None)
        };
        let ui_font = theme_settings::setup_ui_font(window, cx);

        let theme = cx.theme().clone();
        let colors = theme.colors();
        let notification_entities = self
            .notifications
            .iter()
            .map(|(_, notification)| notification.entity_id())
            .collect::<Vec<_>>();

        let pane_render_context = PaneRenderContext {
            follower_states: &self.follower_states,
            active_pane: &self.active_pane,
            app_state: &self.app_state,
            project: &self.project,
            workspace: &self.weak_self,
        };

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font(ui_font)
            .gap_0()
            .justify_start()
            .items_start()
            .text_color(colors.text)
            .overflow_hidden()
            .children(self.titlebar_item.clone())
            .on_modifiers_changed(move |_, _, cx| {
                for &id in &notification_entities {
                    cx.notify(id);
                }
            })
            .child(
                div()
                    .size_full()
                    .relative()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("workspace")
                            .bg(colors.background)
                            .relative()
                            .flex_1()
                            .w_full()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .border_t_1()
                            .border_b_1()
                            .border_color(colors.border)
                            .child({
                                let this = cx.entity();
                                canvas(
                                    move |bounds, _window, cx| {
                                        this.update(cx, |this, _cx| {
                                            this.bounds = bounds;
                                        })
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full()
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .h_full()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .overflow_hidden()
                                            .child(
                                                h_flex()
                                                    .flex_1()
                                                    .when_some(paddings.0, |this, p| {
                                                        this.child(p.border_r_1())
                                                    })
                                                    .child(self.center.render(
                                                        self.zoomed.as_ref(),
                                                        &pane_render_context,
                                                        window,
                                                        cx,
                                                    ))
                                                    .when_some(paddings.1, |this, p| {
                                                        this.child(p.border_l_1())
                                                    }),
                                            ),
                                    ),
                            )
                            .children(self.zoomed.as_ref().and_then(|view| {
                                let zoomed_view = view.upgrade()?;
                                let div = div()
                                    .occlude()
                                    .absolute()
                                    .overflow_hidden()
                                    .border_color(colors.border)
                                    .bg(colors.background)
                                    .child(zoomed_view)
                                    .inset_0()
                                    .shadow_lg();

                                Some(div.top_2().bottom_2().left_2().right_2().border_1())
                            }))
                            .children(self.render_notifications(window, cx)),
                    )
                    .child(self.toast_layer.clone()),
            )
    }
}

impl WorkspaceStore {
    pub fn new() -> Self {
        Self {
            workspaces: Default::default(),
        }
    }

    pub fn workspaces(&self) -> impl Iterator<Item = &WeakEntity<Workspace>> {
        self.workspaces.iter().map(|(_, weak)| weak)
    }

    pub fn workspaces_with_windows(
        &self,
    ) -> impl Iterator<Item = (gpui::AnyWindowHandle, &WeakEntity<Workspace>)> {
        self.workspaces.iter().map(|(window, weak)| (*window, weak))
    }
}

pub trait WorkspaceHandle {
    fn file_project_paths(&self, cx: &App) -> Vec<ProjectPath>;
}

impl WorkspaceHandle for Entity<Workspace> {
    fn file_project_paths(&self, cx: &App) -> Vec<ProjectPath> {
        self.read(cx)
            .worktrees(cx)
            .flat_map(|worktree| {
                let worktree_id = worktree.read(cx).id();
                worktree.read(cx).files(true, 0).map(move |f| ProjectPath {
                    worktree_id,
                    path: f.path.clone(),
                })
            })
            .collect::<Vec<_>>()
    }
}

actions!(
    collab,
    [
        /// Opens the channel notes for the current call.
        ///
        /// Use `collab_panel::OpenSelectedChannelNotes` to open the channel notes for the selected
        /// channel in the collab panel.
        ///
        /// If you want to open a specific channel, use `zed::OpenZedUrl` with a channel notes URL -
        /// can be copied via "Copy link to section" in the context menu of the channel notes
        /// buffer. These URLs look like `https://zed.dev/channel/channel-name-CHANNEL_ID/notes`.
        OpenChannelNotes,
        /// Mutes your microphone.
        Mute,
        /// Deafens yourself (mute both microphone and speakers).
        Deafen,
        /// Leaves the current call.
        LeaveCall,
        /// Shares the current project with collaborators.
        ShareProject,
        /// Shares your screen with collaborators.
        ScreenShare,
        /// Copies the current room name and session id for debugging purposes.
        CopyRoomId,
    ]
);

/// Opens the channel notes for a specific channel by its ID.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = collab)]
#[serde(deny_unknown_fields)]
pub struct OpenChannelNotesById {
    pub channel_id: u64,
}

pub async fn get_any_active_multi_workspace(
    app_state: Arc<AppState>,
    mut cx: AsyncApp,
) -> anyhow::Result<WindowHandle<MultiWorkspace>> {
    // find an existing workspace to focus and show call controls
    let active_window = activate_any_workspace_window(&mut cx);
    if active_window.is_none() {
        cx.update(|cx| {
            Workspace::new_local(
                vec![],
                app_state.clone(),
                None,
                None,
                None,
                OpenMode::Activate,
                cx,
            )
        })
        .await?;
    }
    activate_any_workspace_window(&mut cx).context("could not open zed")
}

pub fn activate_any_workspace_window(cx: &mut AsyncApp) -> Option<WindowHandle<MultiWorkspace>> {
    cx.update(|cx| {
        if let Some(workspace_window) = cx
            .active_window()
            .and_then(|window| window.downcast::<MultiWorkspace>())
        {
            return Some(workspace_window);
        }

        for window in cx.windows() {
            if let Some(workspace_window) = window.downcast::<MultiWorkspace>() {
                cx.activate(true);
                workspace_window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
                return Some(workspace_window);
            }
        }
        None
    })
}

pub fn workspace_windows_for_location(
    cx: &App,
) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .filter(|multi_workspace| {
            multi_workspace.read(cx).is_ok_and(|multi_workspace| {
                let workspace = multi_workspace.workspace();
                match workspace.read(cx).workspace_location(cx) {
                    WorkspaceLocation::Location => {
                        true
                    }
                    _ => false,
                }
            })
        })
        .collect()
}

pub async fn find_existing_workspace(
    abs_paths: &[PathBuf],
    open_options: &OpenOptions,
    cx: &mut AsyncApp,
) -> (
    Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)>,
    OpenVisible,
) {
    let mut existing: Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> = None;
    let mut open_visible = OpenVisible::All;
    let mut best_match = None;

    if open_options.workspace_matching != WorkspaceMatching::None {
        cx.update(|cx| {
            for window in workspace_windows_for_location(cx) {
                if let Ok(multi_workspace) = window.read(cx) {
                    let workspace = multi_workspace.workspace();
                    let project = workspace.read(cx).project.read(cx);
                    let m = project.visibility_for_paths(
                        abs_paths,
                        open_options.workspace_matching != WorkspaceMatching::MatchSubdirectory,
                        cx,
                    );
                    if m > best_match {
                        existing = Some((window, workspace.clone()));
                        best_match = m;
                    } else if best_match.is_none()
                        && open_options.workspace_matching
                            == WorkspaceMatching::MatchSubdirectory
                    {
                        existing = Some((window, workspace.clone()))
                    }
                }
            }
        });

        let all_paths_are_files = existing
            .as_ref()
            .and_then(|(_, target_workspace)| {
                cx.update(|cx| {
                    let workspace = target_workspace.read(cx);
                    let project = workspace.project.read(cx);
                    let path_style = workspace.path_style(cx);
                    Some(!abs_paths.iter().any(|path| {
                        let path = util::paths::SanitizedPath::new(path);
                        project.worktrees(cx).any(|worktree| {
                            let worktree = worktree.read(cx);
                            let abs_path = worktree.abs_path();
                            path_style
                                .strip_prefix(path.as_ref(), abs_path.as_ref())
                                .and_then(|rel| worktree.entry_for_path(&rel))
                                .is_some_and(|e| e.is_dir())
                        })
                    }))
                })
            })
            .unwrap_or(false);

        if open_options.wait && existing.is_some() && all_paths_are_files {
            cx.update(|cx| {
                let windows = workspace_windows_for_location(cx);
                let window = cx
                    .active_window()
                    .and_then(|window| window.downcast::<MultiWorkspace>())
                    .filter(|window| windows.contains(window))
                    .or_else(|| windows.into_iter().next());
                if let Some(window) = window {
                    if let Ok(multi_workspace) = window.read(cx) {
                        let active_workspace = multi_workspace.workspace().clone();
                        existing = Some((window, active_workspace));
                        open_visible = OpenVisible::None;
                    }
                }
            });
        }
    }
    (existing, open_visible)
}

/// Controls whether to reuse an existing workspace whose worktrees contain the
/// given paths, and how broadly to match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceMatching {
    /// Always open a new workspace. No matching against existing worktrees.
    None,
    /// Match paths against existing worktree roots and files within them.
    #[default]
    MatchExact,
    /// Match paths against existing worktrees including subdirectories, and
    /// fall back to any existing window if no worktree matched.
    ///
    /// For example, `zed -a foo/bar` will activate the `bar` workspace if it
    /// exists, otherwise it will open a new window with `foo/bar` as the root.
    MatchSubdirectory,
}

#[derive(Clone)]
pub struct OpenOptions {
    pub visible: Option<OpenVisible>,
    pub focus: Option<bool>,
    pub workspace_matching: WorkspaceMatching,
    pub wait: bool,
    pub requesting_window: Option<WindowHandle<MultiWorkspace>>,
    pub open_mode: OpenMode,
    pub env: Option<HashMap<String, String>>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            visible: None,
            focus: None,
            workspace_matching: WorkspaceMatching::default(),
            wait: false,
            requesting_window: None,
            open_mode: OpenMode::default(),
            env: None,
        }
    }
}

impl OpenOptions {
    fn should_reuse_existing_window(&self) -> bool {
        self.workspace_matching != WorkspaceMatching::None && self.open_mode != OpenMode::NewWindow
    }
}

/// The result of opening a workspace via [`open_paths`], [`Workspace::new_local`],
/// or [`Workspace::open_workspace_for_paths`].
pub struct OpenResult {
    pub window: WindowHandle<MultiWorkspace>,
    pub workspace: Entity<Workspace>,
    pub opened_items: Vec<Option<anyhow::Result<Box<dyn ItemHandle>>>>,
}

#[allow(clippy::type_complexity)]
pub fn open_paths(
    abs_paths: &[PathBuf],
    app_state: Arc<AppState>,
    open_options: OpenOptions,
    cx: &mut App,
) -> Task<anyhow::Result<OpenResult>> {
    let abs_paths = abs_paths.to_vec();
    #[cfg(target_os = "windows")]
    let wsl_path = abs_paths
        .iter()
        .find_map(|p| util::paths::WslPath::from_path(p));

    cx.spawn(async move |cx| {
        let (mut existing, mut open_visible) = find_existing_workspace(
            &abs_paths,
            &open_options,
            cx,
        )
        .await;

        // Fallback: if no workspace contains the paths and all paths are files,
        // prefer an existing local workspace window (active window first).
        if open_options.should_reuse_existing_window() && existing.is_none() {
            let all_paths = abs_paths.iter().map(|path| app_state.fs.metadata(path));
            let all_metadatas = futures::future::join_all(all_paths)
                .await
                .into_iter()
                .filter_map(|result| result.ok().flatten());

            if all_metadatas.into_iter().all(|file| !file.is_dir) {
                cx.update(|cx| {
                    let windows = workspace_windows_for_location(cx);
                    let window = cx
                        .active_window()
                        .and_then(|window| window.downcast::<MultiWorkspace>())
                        .filter(|window| windows.contains(window))
                        .or_else(|| windows.into_iter().next());
                    if let Some(window) = window {
                        if let Ok(multi_workspace) = window.read(cx) {
                            let active_workspace = multi_workspace.workspace().clone();
                            existing = Some((window, active_workspace));
                            open_visible = OpenVisible::None;
                        }
                    }
                });
            }
        }

        let result = if let Some((existing, target_workspace)) = existing {
            let open_task = existing
                .update(cx, |multi_workspace, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    multi_workspace.focus_active_workspace(window, cx);
                    target_workspace.update(cx, |workspace, cx| {
                        workspace.open_paths(
                            abs_paths,
                            OpenOptions {
                                visible: Some(open_visible),
                                ..Default::default()
                            },
                            None,
                            window,
                            cx,
                        )
                    })
                })?
                .await;

            _ = existing.update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    for item in open_task.iter().flatten() {
                        if let Err(e) = item {
                            workspace.show_error(&e, cx);
                        }
                    }
                });
            });

            Ok(OpenResult { window: existing, workspace: target_workspace, opened_items: open_task })
        } else {
            let result = cx
                .update(move |cx| {
                    Workspace::new_local(
                        abs_paths,
                        app_state.clone(),
                        open_options.requesting_window,
                        open_options.env,
                        None,
                        open_options.open_mode,
                        cx,
                    )
                })
                .await;

            if let Ok(ref result) = result {
                result.window
                    .update(cx, |_, window, _cx| {
                        window.activate_window();
                    })
                    .log_err();
            }

            result
        };

        #[cfg(target_os = "windows")]
        if let Some(util::paths::WslPath{distro, path}) = wsl_path
            && let Ok(ref result) = result
        {
            result.window
                .update(cx, move |multi_workspace, _window, cx| {
                    struct OpenInWsl;
                    let workspace = multi_workspace.workspace().clone();
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_notification(NotificationId::unique::<OpenInWsl>(), cx, move |cx| {
                            let display_path = util::markdown::MarkdownInlineCode(&path.to_string_lossy());
                            let msg = format!("{display_path} is inside a WSL filesystem, some features may not work unless you open it with WSL remote");
                            cx.new(move |cx| {
                                MessageNotification::new(msg, cx)
                                    .primary_message("Open in WSL")
                                    .primary_icon(IconName::FolderOpen)
                                    .primary_on_click(move |window, cx| {
                                        window.dispatch_action(Box::new(remote::OpenWslPath {
                                                distro: remote::WslConnectionOptions {
                                                        distro_name: distro.clone(),
                                                    user: None,
                                                },
                                                paths: vec![path.clone().into()],
                                            }), cx)
                                    })
                            })
                        });
                    });
                })
                .unwrap();
        };
        result
    })
}

pub fn open_new(
    open_options: OpenOptions,
    app_state: Arc<AppState>,
    cx: &mut App,
    init: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static + Send,
) -> Task<anyhow::Result<()>> {
    let addition = open_options.open_mode;
    let task = Workspace::new_local(
        Vec::new(),
        app_state,
        open_options.requesting_window,
        open_options.env,
        Some(Box::new(init)),
        addition,
        cx,
    );

    cx.spawn(async move |cx| {
        let OpenResult { window, .. } = task.await?;
        window
            .update(cx, |_, window, _cx| {
                window.activate_window();
            })
            .ok();
        Ok(())
    })
}

pub fn create_and_open_local_file(
    path: &'static Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    default_content: impl 'static + Send + FnOnce() -> Rope,
) -> Task<Result<Box<dyn ItemHandle>>> {
    cx.spawn_in(window, async move |workspace, cx| {
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone())?;
        if !fs.is_file(path).await {
            fs.create_file(path, Default::default()).await?;
            fs.save(path, &default_content(), Default::default())
                .await?;
        }
        let path = PathBuf::from(path);

        workspace
            .update_in(cx, |_workspace, window, cx| {
                cx.spawn_in(window, async move |workspace, cx| {
                    let path = fs.canonicalize(&path).await.unwrap_or(path);

                    let mut items = workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_paths(
                                vec![path.to_path_buf()],
                                OpenOptions {
                                    visible: Some(OpenVisible::None),
                                    ..Default::default()
                                },
                                None,
                                window,
                                cx,
                            )
                        })?
                        .await;
                    let item = items.pop().flatten();
                    item.with_context(|| format!("path {path:?} is not a file"))?
                })
            })?
            .await
    })
}

pub fn reload(cx: &mut App) {
    let should_confirm = WorkspaceSettings::get_global(cx).confirm_quit;
    let mut workspace_windows = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect::<Vec<_>>();

    // If multiple windows have unsaved changes, and need a save prompt,
    // prompt in the active window before switching to a different window.
    workspace_windows.sort_by_key(|window| window.is_active(cx) == Some(false));

    let mut prompt = None;
    if let (true, Some(window)) = (should_confirm, workspace_windows.first()) {
        prompt = window
            .update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    "Are you sure you want to restart?",
                    None,
                    &["Restart", "Cancel"],
                    cx,
                )
            })
            .ok();
    }

    cx.spawn(async move |cx| {
        if let Some(prompt) = prompt {
            let answer = prompt.await?;
            if answer != 0 {
                return anyhow::Ok(());
            }
        }

        // If the user cancels any save prompt, then keep the app open.
        for window in workspace_windows {
            if let Ok(should_close) = window.update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    workspace.prepare_to_close(CloseIntent::Quit, window, cx)
                })
            }) && !should_close.await?
            {
                return anyhow::Ok(());
            }
        }
        cx.update(|cx| cx.restart());
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn parse_pixel_position_env_var(value: &str) -> Option<Point<Pixels>> {
    let mut parts = value.split(',');
    let x: usize = parts.next()?.parse().ok()?;
    let y: usize = parts.next()?.parse().ok()?;
    Some(point(px(x as f32), px(y as f32)))
}

fn parse_pixel_size_env_var(value: &str) -> Option<Size<Pixels>> {
    let mut parts = value.split(',');
    let width: usize = parts.next()?.parse().ok()?;
    let height: usize = parts.next()?.parse().ok()?;
    Some(size(px(width as f32), px(height as f32)))
}

/// Add client-side decorations (rounded corners, shadows, resize handling) when
/// appropriate.
///
/// The `border_radius_tiling` parameter allows overriding which corners get
/// rounded, independently of the actual window tiling state. This is used
/// specifically for the workspace switcher sidebar: when the sidebar is open,
/// we want square corners on the left (so the sidebar appears flush with the
/// window edge) but we still need the shadow padding for proper visual
/// appearance. Unlike actual window tiling, this only affects border radius -
/// not padding or shadows.
pub fn client_side_decorations(
    element: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
    border_radius_tiling: Tiling,
) -> Stateful<Div> {
    const BORDER_SIZE: Pixels = px(1.0);
    let decorations = window.window_decorations();
    let tiling = match decorations {
        Decorations::Server => Tiling::default(),
        Decorations::Client { tiling } => tiling,
    };

    match decorations {
        Decorations::Client { .. } => window.set_client_inset(theme::CLIENT_SIDE_DECORATION_SHADOW),
        Decorations::Server => window.set_client_inset(px(0.0)),
    }

    struct GlobalResizeEdge(ResizeEdge);
    impl Global for GlobalResizeEdge {}

    div()
        .id("window-backdrop")
        .bg(transparent_black())
        .map(|div| match decorations {
            Decorations::Server => div,
            Decorations::Client { .. } => div
                .when(
                    !(tiling.top
                        || tiling.right
                        || border_radius_tiling.top
                        || border_radius_tiling.right),
                    |div| div.rounded_tr(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                )
                .when(
                    !(tiling.top
                        || tiling.left
                        || border_radius_tiling.top
                        || border_radius_tiling.left),
                    |div| div.rounded_tl(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                )
                .when(
                    !(tiling.bottom
                        || tiling.right
                        || border_radius_tiling.bottom
                        || border_radius_tiling.right),
                    |div| div.rounded_br(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                )
                .when(
                    !(tiling.bottom
                        || tiling.left
                        || border_radius_tiling.bottom
                        || border_radius_tiling.left),
                    |div| div.rounded_bl(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                )
                .when(!tiling.top, |div| {
                    div.pt(theme::CLIENT_SIDE_DECORATION_SHADOW)
                })
                .when(!tiling.bottom, |div| {
                    div.pb(theme::CLIENT_SIDE_DECORATION_SHADOW)
                })
                .when(!tiling.left, |div| {
                    div.pl(theme::CLIENT_SIDE_DECORATION_SHADOW)
                })
                .when(!tiling.right, |div| {
                    div.pr(theme::CLIENT_SIDE_DECORATION_SHADOW)
                })
                .on_mouse_move(move |e, window, cx| {
                    let size = window.window_bounds().get_bounds().size;
                    let pos = e.position;

                    let new_edge =
                        resize_edge(pos, theme::CLIENT_SIDE_DECORATION_SHADOW, size, tiling);

                    let edge = cx.try_global::<GlobalResizeEdge>();
                    if new_edge != edge.map(|edge| edge.0) {
                        window
                            .window_handle()
                            .update(cx, |workspace, _, cx| {
                                cx.notify(workspace.entity_id());
                            })
                            .ok();
                    }
                })
                .on_mouse_down(MouseButton::Left, move |e, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    let pos = e.position;

                    let edge = match resize_edge(
                        pos,
                        theme::CLIENT_SIDE_DECORATION_SHADOW,
                        size,
                        tiling,
                    ) {
                        Some(value) => value,
                        None => return,
                    };

                    window.start_window_resize(edge);
                }),
        })
        .size_full()
        .child(
            div()
                .cursor(CursorStyle::Arrow)
                .map(|div| match decorations {
                    Decorations::Server => div,
                    Decorations::Client { .. } => div
                        .border_color(cx.theme().colors().border)
                        .when(
                            !(tiling.top
                                || tiling.right
                                || border_radius_tiling.top
                                || border_radius_tiling.right),
                            |div| div.rounded_tr(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                        )
                        .when(
                            !(tiling.top
                                || tiling.left
                                || border_radius_tiling.top
                                || border_radius_tiling.left),
                            |div| div.rounded_tl(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                        )
                        .when(
                            !(tiling.bottom
                                || tiling.right
                                || border_radius_tiling.bottom
                                || border_radius_tiling.right),
                            |div| div.rounded_br(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                        )
                        .when(
                            !(tiling.bottom
                                || tiling.left
                                || border_radius_tiling.bottom
                                || border_radius_tiling.left),
                            |div| div.rounded_bl(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                        )
                        .when(!tiling.top, |div| div.border_t(BORDER_SIZE))
                        .when(!tiling.bottom, |div| div.border_b(BORDER_SIZE))
                        .when(!tiling.left, |div| div.border_l(BORDER_SIZE))
                        .when(!tiling.right, |div| div.border_r(BORDER_SIZE))
                        .when(!tiling.is_tiled(), |div| {
                            div.shadow(vec![gpui::BoxShadow {
                                color: Hsla {
                                    h: 0.,
                                    s: 0.,
                                    l: 0.,
                                    a: 0.4,
                                },
                                blur_radius: theme::CLIENT_SIDE_DECORATION_SHADOW / 2.,
                                spread_radius: px(0.),
                                inset: false,
                                offset: point(px(0.0), px(0.0)),
                            }])
                        }),
                })
                .on_mouse_move(|_e, _, cx| {
                    cx.stop_propagation();
                })
                .size_full()
                .child(element),
        )
        .map(|div| match decorations {
            Decorations::Server => div,
            Decorations::Client { tiling, .. } => div.child(
                canvas(
                    |_bounds, window, _| {
                        window.insert_hitbox(
                            Bounds::new(
                                point(px(0.0), px(0.0)),
                                window.window_bounds().get_bounds().size,
                            ),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_bounds, hitbox, window, cx| {
                        let mouse = window.mouse_position();
                        let size = window.window_bounds().get_bounds().size;
                        let Some(edge) =
                            resize_edge(mouse, theme::CLIENT_SIDE_DECORATION_SHADOW, size, tiling)
                        else {
                            return;
                        };
                        cx.set_global(GlobalResizeEdge(edge));
                        window.set_cursor_style(
                            match edge {
                                ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                                ResizeEdge::Left | ResizeEdge::Right => {
                                    CursorStyle::ResizeLeftRight
                                }
                                ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                    CursorStyle::ResizeUpLeftDownRight
                                }
                                ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                    CursorStyle::ResizeUpRightDownLeft
                                }
                            },
                            &hitbox,
                        );
                    },
                )
                .size_full()
                .absolute(),
            ),
        })
}

fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner_size = size(shadow_size * 1.5, shadow_size * 1.5);
    let top_left_bounds = Bounds::new(Point::new(px(0.), px(0.)), corner_size);
    if !tiling.top && top_left_bounds.contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }

    let top_right_bounds = Bounds::new(
        Point::new(window_size.width - corner_size.width, px(0.)),
        corner_size,
    );
    if !tiling.top && top_right_bounds.contains(&pos) {
        return Some(ResizeEdge::TopRight);
    }

    let bottom_left_bounds = Bounds::new(
        Point::new(px(0.), window_size.height - corner_size.height),
        corner_size,
    );
    if !tiling.bottom && bottom_left_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomLeft);
    }

    let bottom_right_bounds = Bounds::new(
        Point::new(
            window_size.width - corner_size.width,
            window_size.height - corner_size.height,
        ),
        corner_size,
    );
    if !tiling.bottom && bottom_right_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

fn join_pane_into_active(
    active_pane: &Entity<Pane>,
    pane: &Entity<Pane>,
    window: &mut Window,
    cx: &mut App,
) {
    if pane == active_pane {
    } else if pane.read(cx).items_len() == 0 {
        pane.update(cx, |_, cx| {
            cx.emit(pane::Event::Remove {
                focus_on_pane: None,
            });
        })
    } else {
        move_all_items(pane, active_pane, window, cx);
    }
}

fn move_all_items(
    from_pane: &Entity<Pane>,
    to_pane: &Entity<Pane>,
    window: &mut Window,
    cx: &mut App,
) {
    let destination_is_different = from_pane != to_pane;
    let mut moved_items = 0;
    for (item_ix, item_handle) in from_pane
        .read(cx)
        .items()
        .enumerate()
        .map(|(ix, item)| (ix, item.clone()))
        .collect::<Vec<_>>()
    {
        let ix = item_ix - moved_items;
        if destination_is_different {
            // Close item from previous pane
            from_pane.update(cx, |source, cx| {
                source.remove_item_and_focus_on_pane(ix, false, to_pane.clone(), window, cx);
            });
            moved_items += 1;
        }

        // This automatically removes duplicate items in the pane
        to_pane.update(cx, |destination, cx| {
            destination.add_item(item_handle, true, true, None, window, cx);
            window.focus(&destination.focus_handle(cx), cx)
        });
    }
}

pub fn move_item(
    source: &Entity<Pane>,
    destination: &Entity<Pane>,
    item_id_to_move: EntityId,
    destination_index: usize,
    activate: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((item_ix, item_handle)) = source
        .read(cx)
        .items()
        .enumerate()
        .find(|(_, item_handle)| item_handle.item_id() == item_id_to_move)
        .map(|(ix, item)| (ix, item.clone()))
    else {
        // Tab was closed during drag
        return;
    };

    if source != destination {
        // Close item from previous pane
        source.update(cx, |source, cx| {
            source.remove_item_and_focus_on_pane(item_ix, false, destination.clone(), window, cx);
        });
    }

    // This automatically removes duplicate items in the pane
    destination.update(cx, |destination, cx| {
        destination.add_item_inner(
            item_handle,
            activate,
            activate,
            activate,
            Some(destination_index),
            window,
            cx,
        );
        if activate {
            window.focus(&destination.focus_handle(cx), cx)
        }
    });
}

pub fn move_active_item(
    source: &Entity<Pane>,
    destination: &Entity<Pane>,
    focus_destination: bool,
    close_if_empty: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if source == destination {
        return;
    }
    let Some(active_item) = source.read(cx).active_item() else {
        return;
    };
    source.update(cx, |source_pane, cx| {
        let item_id = active_item.item_id();
        source_pane.remove_item(item_id, false, close_if_empty, window, cx);
        destination.update(cx, |target_pane, cx| {
            target_pane.add_item(
                active_item,
                focus_destination,
                focus_destination,
                Some(target_pane.items_len()),
                window,
                cx,
            );
        });
    });
}

pub fn clone_active_item(
    workspace_id: Option<WorkspaceId>,
    source: &Entity<Pane>,
    destination: &Entity<Pane>,
    focus_destination: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if source == destination {
        return;
    }
    let Some(active_item) = source.read(cx).active_item() else {
        return;
    };
    if !active_item.can_split(cx) {
        return;
    }
    let destination = destination.downgrade();
    let task = active_item.clone_on_split(workspace_id, window, cx);
    window
        .spawn(cx, async move |cx| {
            let Some(clone) = task.await else {
                return;
            };
            destination
                .update_in(cx, |target_pane, window, cx| {
                    target_pane.add_item(
                        clone,
                        focus_destination,
                        focus_destination,
                        Some(target_pane.items_len()),
                        window,
                        cx,
                    );
                })
                .log_err();
        })
        .detach();
}

#[derive(Debug)]
pub struct WorkspacePosition {
    pub window_bounds: Option<WindowBounds>,
    pub display: Option<Uuid>,
    pub centered_layout: bool,
}

pub fn with_active_or_new_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send + 'static,
) {
    // favor the active multi-workspace... but if one doesn't exist, then just grab one
    let multi_workspace =
        cx.active_window()
            .and_then(|w| w.downcast::<MultiWorkspace>())
            .or_else(||
                cx.windows()
                    .into_iter()
                    .find_map(|w| w.downcast::<MultiWorkspace>())
            );

    match multi_workspace {
        Some(multi_workspace) => {
            cx.defer(move |cx| {
                multi_workspace
                    .update(cx, |multi_workspace, window, cx| {
                        let workspace = multi_workspace.workspace().clone();
                        workspace.update(cx, |workspace, cx| f(workspace, window, cx));
                    })
                    .log_err();
            });
        }
        None => {
            let app_state = AppState::global(cx);
            open_new(
                OpenOptions::default(),
                app_state,
                cx,
                move |workspace, window, cx| f(workspace, window, cx),
            )
            .detach_and_log_err(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

    use super::*;
    use crate::{
        dock::{PanelEvent, test::TestPanel},
        item::{
            ItemBufferKind, ItemEvent,
            test::{TestItem, TestProjectItem},
        },
    };
    use fs::FakeFs;
    use gpui::{
        DismissEvent, Empty, EventEmitter, FocusHandle, Focusable, Render, TestAppContext,
        UpdateGlobal, VisualTestContext, px,
    };
    use project::{Project, ProjectEntryId, WorktreeId};
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use util::rel_path::rel_path;

    #[gpui::test]
    async fn test_tab_disambiguation(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        // Adding an item with no ambiguity renders the tab without detail.
        let item1 = cx.new(|cx| {
            let mut item = TestItem::new(cx);
            item.tab_descriptions = Some(vec!["c", "b1/c", "a/b1/c"]);
            item
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item1.clone()), None, true, window, cx);
        });
        item1.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(0)));

        // Adding an item that creates ambiguity increases the level of detail on
        // both tabs.
        let item2 = cx.new_window_entity(|_window, cx| {
            let mut item = TestItem::new(cx);
            item.tab_descriptions = Some(vec!["c", "b2/c", "a/b2/c"]);
            item
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item2.clone()), None, true, window, cx);
        });
        item1.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(1)));
        item2.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(1)));

        // Adding an item that creates ambiguity increases the level of detail only
        // on the ambiguous tabs. In this case, the ambiguity can't be resolved so
        // we stop at the highest detail available.
        let item3 = cx.new(|cx| {
            let mut item = TestItem::new(cx);
            item.tab_descriptions = Some(vec!["c", "b2/c", "a/b2/c"]);
            item
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item3.clone()), None, true, window, cx);
        });
        item1.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(1)));
        item2.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(3)));
        item3.read_with(cx, |item, _| assert_eq!(item.tab_detail.get(), Some(3)));
    }

    #[gpui::test]
    async fn test_tracking_active_path(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root1",
            json!({
                "one.txt": "",
                "two.txt": "",
            }),
        )
        .await;
        fs.insert_tree(
            "/root2",
            json!({
                "three.txt": "",
            }),
        )
        .await;

        let project = Project::test(fs, ["root1".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let item1 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "one.txt", cx)])
        });
        let item2 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(2, "two.txt", cx)])
        });

        // Add an item to an empty pane
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item1), None, true, window, cx)
        });
        project.update(cx, |project, cx| {
            assert_eq!(
                project.active_entry(),
                project
                    .entry_for_path(&(worktree_id, rel_path("one.txt")).into(), cx)
                    .map(|e| e.id)
            );
        });
        assert_eq!(cx.window_title().as_deref(), Some("root1 — one.txt"));

        // Add a second item to a non-empty pane
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item2), None, true, window, cx)
        });
        assert_eq!(cx.window_title().as_deref(), Some("root1 — two.txt"));
        project.update(cx, |project, cx| {
            assert_eq!(
                project.active_entry(),
                project
                    .entry_for_path(&(worktree_id, rel_path("two.txt")).into(), cx)
                    .map(|e| e.id)
            );
        });

        // Close the active item
        pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(&Default::default(), window, cx)
        })
        .await
        .unwrap();
        assert_eq!(cx.window_title().as_deref(), Some("root1 — one.txt"));
        project.update(cx, |project, cx| {
            assert_eq!(
                project.active_entry(),
                project
                    .entry_for_path(&(worktree_id, rel_path("one.txt")).into(), cx)
                    .map(|e| e.id)
            );
        });

        // Add a project folder
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("root2", true, cx)
            })
            .await
            .unwrap();
        assert_eq!(cx.window_title().as_deref(), Some("root1, root2 — one.txt"));

        // Remove a project folder
        project.update(cx, |project, cx| project.remove_worktree(worktree_id, cx));
        assert_eq!(cx.window_title().as_deref(), Some("root2 — one.txt"));
    }

    #[gpui::test]
    async fn test_document_path_updates_with_active_item(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({
                "one.txt": "",
                "two.txt": "",
            }),
        )
        .await;

        let project = Project::test(fs, ["root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let item1 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[new_test_project_item(
                1,
                "one.txt",
                worktree_id,
                cx,
            )])
        });
        let item2 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[new_test_project_item(
                2,
                "two.txt",
                worktree_id,
                cx,
            )])
        });

        // Initially no document path
        assert_eq!(cx.document_path(), None);

        // Add an item - document path should be set
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item1), None, true, window, cx)
        });
        assert_eq!(
            cx.document_path(),
            Some(std::path::PathBuf::from("root/one.txt"))
        );

        // Add a second item - document path should update
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item2), None, true, window, cx)
        });
        assert_eq!(
            cx.document_path(),
            Some(std::path::PathBuf::from("root/two.txt"))
        );

        // Close the active item - document path should revert to first item
        pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(&Default::default(), window, cx)
        })
        .await
        .unwrap();
        assert_eq!(
            cx.document_path(),
            Some(std::path::PathBuf::from("root/one.txt"))
        );

        // Close all items - document path should be cleared
        pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(&Default::default(), window, cx)
        })
        .await
        .unwrap();
        assert_eq!(cx.document_path(), None);
    }

    #[gpui::test]
    async fn test_close_window(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "one": "" })).await;

        let project = Project::test(fs, ["root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        // When there are no dirty items, there's nothing to do.
        let item1 = cx.new(TestItem::new);
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item1.clone()), None, true, window, cx)
        });
        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        assert!(task.await.unwrap());

        // When there are dirty untitled items, prompt to save each one. If the user
        // cancels any prompt, then abort.
        let item2 = cx.new(|cx| TestItem::new(cx).with_dirty(true));
        let item3 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item2.clone()), None, true, window, cx);
            w.add_item_to_active_pane(Box::new(item3.clone()), None, true, window, cx);
        });
        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        cx.executor().run_until_parked();
        cx.simulate_prompt_answer("Cancel"); // cancel save all
        cx.executor().run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert!(!task.await.unwrap());
    }

    #[gpui::test]
    async fn test_multi_workspace_close_window_multiple_workspaces_cancel(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "one": "" })).await;

        let project_a = Project::test(fs.clone(), ["root".as_ref()], cx).await;
        let project_b = Project::test(fs, ["root".as_ref()], cx).await;
        let multi_workspace_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project_a.clone(), window, cx));
        cx.run_until_parked();

        multi_workspace_handle
            .update(cx, |mw, _window, cx| {
                mw.open_sidebar(cx);
            })
            .unwrap();

        let workspace_a = multi_workspace_handle
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();

        let workspace_b = multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.test_add_workspace(project_b, window, cx)
            })
            .unwrap();

        // Activate workspace A
        multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.activate(workspace_a.clone(), None, window, cx);
            })
            .unwrap();

        let cx = &mut VisualTestContext::from_window(multi_workspace_handle.into(), cx);

        // Workspace A has a clean item
        let item_a = cx.new(TestItem::new);
        workspace_a.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item_a.clone()), None, true, window, cx)
        });

        // Workspace B has a dirty item
        let item_b = cx.new(|cx| TestItem::new(cx).with_dirty(true));
        workspace_b.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item_b.clone()), None, true, window, cx)
        });

        // Verify workspace A is active
        multi_workspace_handle
            .read_with(cx, |mw, _| {
                assert_eq!(mw.workspace(), &workspace_a);
            })
            .unwrap();

        // Dispatch CloseWindow — workspace A will pass, workspace B will prompt
        multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.close_window(&CloseWindow, window, cx);
            })
            .unwrap();
        cx.run_until_parked();

        // Workspace B should now be active since it has dirty items that need attention
        multi_workspace_handle
            .read_with(cx, |mw, _| {
                assert_eq!(
                    mw.workspace(),
                    &workspace_b,
                    "workspace B should be activated when it prompts"
                );
            })
            .unwrap();

        // User cancels the save prompt from workspace B
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        // Window should still exist because workspace B's close was cancelled
        assert!(
            multi_workspace_handle.update(cx, |_, _, _| ()).is_ok(),
            "window should still exist after cancelling one workspace's close"
        );
    }

    #[gpui::test]
    async fn test_remove_workspace_prompts_for_unsaved_changes(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "one": "" })).await;

        let project_a = Project::test(fs.clone(), ["root".as_ref()], cx).await;
        let project_b = Project::test(fs.clone(), ["root".as_ref()], cx).await;
        let multi_workspace_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project_a.clone(), window, cx));
        cx.run_until_parked();

        multi_workspace_handle
            .update(cx, |mw, _window, cx| mw.open_sidebar(cx))
            .unwrap();

        let workspace_a = multi_workspace_handle
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();

        let workspace_b = multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.test_add_workspace(project_b, window, cx)
            })
            .unwrap();

        // Activate workspace A.
        multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.activate(workspace_a.clone(), None, window, cx);
            })
            .unwrap();

        let cx = &mut VisualTestContext::from_window(multi_workspace_handle.into(), cx);

        // Workspace B has a dirty item.
        let item_b = cx.new(|cx| TestItem::new(cx).with_dirty(true));
        workspace_b.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item_b.clone()), None, true, window, cx)
        });

        // Try to remove workspace B. It should prompt because of the dirty item.
        let remove_task = multi_workspace_handle
            .update(cx, |mw, window, cx| {
                mw.remove([workspace_b.clone()], |_, _, _| unreachable!(), window, cx)
            })
            .unwrap();
        cx.run_until_parked();

        // The prompt should have activated workspace B.
        multi_workspace_handle
            .read_with(cx, |mw, _| {
                assert_eq!(
                    mw.workspace(),
                    &workspace_b,
                    "workspace B should be active while prompting"
                );
            })
            .unwrap();

        // Cancel the prompt — user stays on workspace B.
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        let removed = remove_task.await.unwrap();
        assert!(!removed, "removal should have been cancelled");

        multi_workspace_handle
            .read_with(cx, |mw, _cx| {
                assert_eq!(
                    mw.workspace(),
                    &workspace_b,
                    "user should stay on workspace B after cancelling"
                );
                assert_eq!(mw.workspaces().count(), 2, "both workspaces should remain");
            })
            .unwrap();

        // Try again. This time accept the prompt.
        let remove_task = multi_workspace_handle
            .update(cx, |mw, window, cx| {
                // First switch back to A.
                mw.activate(workspace_a.clone(), None, window, cx);
                mw.remove([workspace_b.clone()], |_, _, _| unreachable!(), window, cx)
            })
            .unwrap();
        cx.run_until_parked();

        // Accept the save prompt.
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        let removed = remove_task.await.unwrap();
        assert!(removed, "removal should have succeeded");

        // Should be back on workspace A, and B should be gone.
        multi_workspace_handle
            .read_with(cx, |mw, _cx| {
                assert_eq!(
                    mw.workspace(),
                    &workspace_a,
                    "should be back on workspace A after removing B"
                );
                assert_eq!(mw.workspaces().count(), 1, "only workspace A should remain");
            })
            .unwrap();
    }

    #[gpui::test]
    async fn test_close_window_with_worktrees_hot_exits(cx: &mut TestAppContext) {
        init_test(cx);

        // Register TestItem as a serializable item
        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "one": "" })).await;

        let project = Project::test(fs, ["root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        // When there are dirty untitled items, but they can serialize, then there is no prompt.
        let item1 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        let item2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item1.clone()), None, true, window, cx);
            w.add_item_to_active_pane(Box::new(item2.clone()), None, true, window, cx);
        });
        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        assert!(task.await.unwrap());
    }

    // See https://github.com/zed-industries/zed/issues/55726.
    //
    // macOS only: on Linux/Windows, closing the last window sets
    // `save_last_workspace`, which preserves the session (same as `Quit`),
    // so hot-exit is safe there.
    #[cfg(target_os = "macos")]
    #[gpui::test]
    async fn test_close_window_without_worktrees_prompts(cx: &mut TestAppContext) {
        init_test(cx);

        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        cx.executor().run_until_parked();

        assert!(
            cx.has_pending_prompt(),
            "closing a no-folder workspace with a dirty serializable item should prompt, \
             since the workspace will not be reachable after close"
        );
        cx.simulate_prompt_answer("Don't Save");
        cx.executor().run_until_parked();

        assert!(task.await.unwrap());
    }

    #[gpui::test]
    async fn test_quit_without_worktrees_hot_exits(cx: &mut TestAppContext) {
        init_test(cx);

        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::Quit, window, cx)
        });
        cx.executor().run_until_parked();

        assert!(
            !cx.has_pending_prompt(),
            "quitting should hot-exit silently; the session restore on next \
             launch will bring the dirty buffer back"
        );
        assert!(task.await.unwrap());
    }

    // See https://github.com/zed-industries/zed/issues/55726.
    #[gpui::test]
    async fn test_replace_window_without_worktrees_prompts(cx: &mut TestAppContext) {
        init_test(cx);

        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::ReplaceWindow, window, cx)
        });
        cx.executor().run_until_parked();

        assert!(
            cx.has_pending_prompt(),
            "replacing a workspace with a dirty serializable item should prompt, \
             since the workspace will be detached afterwards"
        );
        cx.simulate_prompt_answer("Don't Save");
        cx.executor().run_until_parked();

        assert!(task.await.unwrap());
    }

    #[gpui::test]
    async fn test_replace_window_with_worktrees_hot_exits(cx: &mut TestAppContext) {
        init_test(cx);

        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "one": "" })).await;

        let project = Project::test(fs, ["root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_serialize(|| Some(Task::ready(Ok(()))))
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::ReplaceWindow, window, cx)
        });
        cx.executor().run_until_parked();

        assert!(
            !cx.has_pending_prompt(),
            "replacing a workspace with folder paths should hot-exit silently; \
             the buffer is recoverable by reopening the project"
        );
        assert!(task.await.unwrap());
    }

    #[gpui::test]
    async fn test_close_window_with_failing_serialize_prompts(cx: &mut TestAppContext) {
        init_test(cx);

        cx.update(|cx| {
            register_serializable_item::<TestItem>(cx);
        });

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx).with_dirty(true).with_serialize(|| {
                Some(Task::ready(Err(anyhow::anyhow!(
                    "FOREIGN KEY constraint failed"
                ))))
            })
        });
        workspace.update_in(cx, |w, window, cx| {
            w.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        let task = workspace.update_in(cx, |w, window, cx| {
            w.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        cx.executor().run_until_parked();

        // The failing serialization must not short-circuit the close; a
        // save/discard prompt must be shown for the dirty scratch item.
        assert!(
            cx.has_pending_prompt(),
            "a save/discard prompt should be shown for the dirty scratch item \
             when its serialization fails"
        );
        cx.simulate_prompt_answer("Don't Save");
        cx.executor().run_until_parked();

        // Preparing to close succeeds, even though serialization failed.
        assert!(task.await.unwrap());
    }

    #[gpui::test]
    async fn test_close_pane_items(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());

        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let item1 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_project_items(&[dirty_project_item(1, "1.txt", cx)])
        });
        let item2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_conflict(true)
                .with_project_items(&[dirty_project_item(2, "2.txt", cx)])
        });
        let item3 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_conflict(true)
                .with_project_items(&[dirty_project_item(3, "3.txt", cx)])
        });
        let item4 = cx.new(|cx| {
            TestItem::new(cx).with_dirty(true).with_project_items(&[{
                let project_item = TestProjectItem::new_untitled(cx);
                project_item.update(cx, |project_item, _| project_item.is_dirty = true);
                project_item
            }])
        });
        let pane = workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item1.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(Box::new(item2.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(Box::new(item3.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(Box::new(item4.clone()), None, true, window, cx);
            workspace.active_pane().clone()
        });

        let close_items = pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(1, true, true, window, cx);
            assert_eq!(pane.active_item().unwrap().item_id(), item2.item_id());
            let item1_id = item1.item_id();
            let item3_id = item3.item_id();
            let item4_id = item4.item_id();
            pane.close_items(window, cx, SaveIntent::Close, &move |id| {
                [item1_id, item3_id, item4_id].contains(&id)
            })
        });
        cx.executor().run_until_parked();

        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save all");

        cx.executor().run_until_parked();

        // Item 1 is saved. There's a prompt to save item 3.
        pane.update(cx, |pane, cx| {
            assert_eq!(item1.read(cx).save_count, 1);
            assert_eq!(item1.read(cx).save_as_count, 0);
            assert_eq!(item1.read(cx).reload_count, 0);
            assert_eq!(pane.items_len(), 3);
            assert_eq!(pane.active_item().unwrap().item_id(), item3.item_id());
        });
        assert!(cx.has_pending_prompt());

        // Cancel saving item 3.
        cx.simulate_prompt_answer("Discard");
        cx.executor().run_until_parked();

        // Item 3 is reloaded. There's a prompt to save item 4.
        pane.update(cx, |pane, cx| {
            assert_eq!(item3.read(cx).save_count, 0);
            assert_eq!(item3.read(cx).save_as_count, 0);
            assert_eq!(item3.read(cx).reload_count, 1);
            assert_eq!(pane.items_len(), 2);
            assert_eq!(pane.active_item().unwrap().item_id(), item4.item_id());
        });

        // There's a prompt for a path for item 4.
        cx.simulate_new_path_selection(|_| Some(Default::default()));
        close_items.await.unwrap();

        // The requested items are closed.
        pane.update(cx, |pane, cx| {
            assert_eq!(item4.read(cx).save_count, 1);
            assert_eq!(item4.read(cx).save_as_count, 1);
            assert_eq!(item4.read(cx).reload_count, 0);
            assert_eq!(pane.items_len(), 1);
            assert_eq!(pane.active_item().unwrap().item_id(), item2.item_id());
        });
    }

    #[gpui::test]
    async fn test_prompting_to_save_only_on_last_item_for_entry(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        // Create several workspace items with single project entries, and two
        // workspace items with multiple project entries.
        let single_entry_items = (0..=4)
            .map(|project_entry_id| {
                cx.new(|cx| {
                    TestItem::new(cx)
                        .with_dirty(true)
                        .with_project_items(&[dirty_project_item(
                            project_entry_id,
                            &format!("{project_entry_id}.txt"),
                            cx,
                        )])
                })
            })
            .collect::<Vec<_>>();
        let item_2_3 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_buffer_kind(ItemBufferKind::Multibuffer)
                .with_project_items(&[
                    single_entry_items[2].read(cx).project_items[0].clone(),
                    single_entry_items[3].read(cx).project_items[0].clone(),
                ])
        });
        let item_3_4 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_buffer_kind(ItemBufferKind::Multibuffer)
                .with_project_items(&[
                    single_entry_items[3].read(cx).project_items[0].clone(),
                    single_entry_items[4].read(cx).project_items[0].clone(),
                ])
        });

        // Create two panes that contain the following project entries:
        //   left pane:
        //     multi-entry items:   (2, 3)
        //     single-entry items:  0, 2, 3, 4
        //   right pane:
        //     single-entry items:  4, 1
        //     multi-entry items:   (3, 4)
        let (left_pane, right_pane) = workspace.update_in(cx, |workspace, window, cx| {
            let left_pane = workspace.active_pane().clone();
            workspace.add_item_to_active_pane(Box::new(item_2_3.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(
                single_entry_items[0].boxed_clone(),
                None,
                true,
                window,
                cx,
            );
            workspace.add_item_to_active_pane(
                single_entry_items[2].boxed_clone(),
                None,
                true,
                window,
                cx,
            );
            workspace.add_item_to_active_pane(
                single_entry_items[3].boxed_clone(),
                None,
                true,
                window,
                cx,
            );
            workspace.add_item_to_active_pane(
                single_entry_items[4].boxed_clone(),
                None,
                true,
                window,
                cx,
            );

            let right_pane =
                workspace.split_and_clone(left_pane.clone(), SplitDirection::Right, window, cx);

            let boxed_clone = single_entry_items[1].boxed_clone();
            let right_pane = window.spawn(cx, async move |cx| {
                right_pane.await.inspect(|right_pane| {
                    right_pane
                        .update_in(cx, |pane, window, cx| {
                            pane.add_item(boxed_clone, true, true, None, window, cx);
                            pane.add_item(Box::new(item_3_4.clone()), true, true, None, window, cx);
                        })
                        .unwrap();
                })
            });

            (left_pane, right_pane)
        });
        let right_pane = right_pane.await.unwrap();
        cx.focus(&right_pane);

        let close = right_pane.update_in(cx, |pane, window, cx| {
            pane.close_all_items(&CloseAllItems::default(), window, cx)
                .unwrap()
        });
        cx.executor().run_until_parked();

        let msg = cx.pending_prompt().unwrap().0;
        assert!(msg.contains("1.txt"));
        assert!(!msg.contains("2.txt"));
        assert!(!msg.contains("3.txt"));
        assert!(!msg.contains("4.txt"));

        // With best-effort close, cancelling item 1 keeps it open but items 4
        // and (3,4) still close since their entries exist in left pane.
        cx.simulate_prompt_answer("Cancel");
        close.await;

        right_pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items_len(), 1);
        });

        // Remove item 3 from left pane, making (2,3) the only item with entry 3.
        left_pane
            .update_in(cx, |left_pane, window, cx| {
                left_pane.close_item_by_id(
                    single_entry_items[3].entity_id(),
                    SaveIntent::Skip,
                    window,
                    cx,
                )
            })
            .await
            .unwrap();

        let close = left_pane.update_in(cx, |pane, window, cx| {
            pane.close_all_items(&CloseAllItems::default(), window, cx)
                .unwrap()
        });
        cx.executor().run_until_parked();

        let details = cx.pending_prompt().unwrap().1;
        assert!(details.contains("0.txt"));
        assert!(details.contains("3.txt"));
        assert!(details.contains("4.txt"));
        // Ideally 2.txt wouldn't appear since entry 2 still exists in item 2.
        // But we can only save whole items, so saving (2,3) for entry 3 includes 2.
        // assert!(!details.contains("2.txt"));

        cx.simulate_prompt_answer("Save all");
        cx.executor().run_until_parked();
        close.await;

        left_pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items_len(), 0);
        });
    }

    #[gpui::test]
    async fn test_autosave(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        let item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
        });
        let item_id = item.entity_id();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        // Autosave on window change.
        item.update(cx, |item, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnWindowChange);
                })
            });
            item.is_dirty = true;
        });

        // Deactivating the window saves the file.
        cx.deactivate_window();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 1));

        // Re-activating the window doesn't save the file.
        cx.update(|window, _| window.activate_window());
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 1));

        // Autosave on focus change.
        item.update_in(cx, |item, window, cx| {
            cx.focus_self(window);
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnFocusChange);
                })
            });
            item.is_dirty = true;
        });
        // Focus leaving the item (via window deactivation) saves the file.
        // Deferred autosaves are flushed when focus lands elsewhere (pane, panel)
        // or when the window is deactivated.
        cx.deactivate_window();
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 2));
        cx.update(|window, _| window.activate_window());

        // Deactivating the window still saves the file.
        item.update_in(cx, |item, window, cx| {
            cx.focus_self(window);
            item.is_dirty = true;
        });
        cx.deactivate_window();
        item.update(cx, |item, _| assert_eq!(item.save_count, 3));

        // Autosave after delay.
        item.update(cx, |item, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::AfterDelay {
                        milliseconds: 500.into(),
                    });
                })
            });
            item.is_dirty = true;
            cx.emit(ItemEvent::Edit);
        });

        // Delay hasn't fully expired, so the file is still dirty and unsaved.
        cx.executor().advance_clock(Duration::from_millis(250));
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 3));

        // After delay expires, the file is saved.
        cx.executor().advance_clock(Duration::from_millis(250));
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 4));

        // Autosave after delay, should save earlier than delay if tab is closed
        item.update(cx, |item, cx| {
            item.is_dirty = true;
            cx.emit(ItemEvent::Edit);
        });
        cx.executor().advance_clock(Duration::from_millis(250));
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 4));

        // // Ensure auto save with delay saves the item on close, even if the timer hasn't yet run out.
        pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &move |id| id == item_id)
        })
        .await
        .unwrap();
        assert!(!cx.has_pending_prompt());
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 5));

        // Add the item again, ensuring autosave is prevented if the underlying file has been deleted.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });
        item.update_in(cx, |item, _window, cx| {
            item.is_dirty = true;
            for project_item in &mut item.project_items {
                project_item.update(cx, |project_item, _| project_item.is_dirty = true);
            }
        });
        cx.run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 5));

        // Autosave on focus change, ensuring closing the tab counts as such.
        item.update(cx, |item, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnFocusChange);
                })
            });
            item.is_dirty = true;
            for project_item in &mut item.project_items {
                project_item.update(cx, |project_item, _| project_item.is_dirty = true);
            }
        });

        pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &move |id| id == item_id)
        })
        .await
        .unwrap();
        assert!(!cx.has_pending_prompt());
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 6));

        // Add the item again, ensuring autosave is prevented if the underlying file has been deleted.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });
        item.update_in(cx, |item, window, cx| {
            item.project_items[0].update(cx, |item, _| {
                item.entry_id = None;
            });
            item.is_dirty = true;
            window.blur();
        });
        cx.run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 6));

        // Ensure autosave is prevented for deleted files also when closing the buffer.
        let _close_items = pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &move |id| id == item_id)
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 6));
    }

    #[gpui::test]
    async fn test_autosave_on_focus_change_in_multibuffer(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        // Create a multibuffer-like item with two child focus handles,
        // simulating individual buffer editors within a multibuffer.
        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
                .with_child_focus_handles(2, cx)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        // Set autosave to OnFocusChange and focus the first child handle,
        // simulating the user's cursor being inside one of the multibuffer's excerpts.
        item.update_in(cx, |item, window, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnFocusChange);
                })
            });
            item.is_dirty = true;
            window.focus(&item.child_focus_handles[0], cx);
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 0));

        // Moving focus from one child to another within the same item should
        // NOT trigger autosave — focus is still within the item's focus hierarchy.
        item.update_in(cx, |item, window, cx| {
            window.focus(&item.child_focus_handles[1], cx);
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 0,
                "Switching focus between children within the same item should not autosave"
            );
        });

        // Focus leaving the item saves the file. This is the core regression scenario:
        // with `on_blur`, this would NOT trigger because `on_blur` only fires when
        // the item's own focus handle is the leaf that lost focus. In a multibuffer,
        // the leaf is always a child focus handle, so `on_blur` never detected
        // focus leaving the item.
        //
        // With deferred saves, the save happens when focus lands on a pane/panel or
        // the window deactivates.
        cx.deactivate_window();
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 1,
                "Window deactivation should trigger autosave when focus was on a child of the item"
            );
        });
        cx.update(|window, _| window.activate_window());

        // Deactivating the window should also trigger autosave when a child of
        // the multibuffer item currently owns focus.
        item.update_in(cx, |item, window, cx| {
            item.is_dirty = true;
            window.focus(&item.child_focus_handles[0], cx);
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| assert_eq!(item.save_count, 1));

        cx.deactivate_window();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 2,
                "Deactivating window should trigger autosave when focus was on a child"
            );
        });
    }

    #[gpui::test]
    async fn test_autosave_deferred_for_modals(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
        });

        item.update_in(cx, |item, window, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnFocusChange);
                })
            });
            item.is_dirty = true;
            cx.focus_self(window);
        });
        cx.executor().run_until_parked();

        // Opening a modal moves focus away from the item, but autosave should be
        // deferred until focus lands on a pane or panel (not saved immediately).
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, TestModal::new);
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 0,
                "Opening a modal should NOT immediately trigger autosave"
            );
        });

        // If focus returns to the same item (modal dismissed), the deferred save
        // should be skipped.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.modal_layer.update(cx, |modal, cx| {
                modal.hide_modal(window, cx);
            });
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 0,
                "Returning focus to the same item should skip deferred save"
            );
        });

        // Open modal again with a dirty item.
        item.update_in(cx, |item, window, cx| {
            item.is_dirty = true;
            cx.focus_self(window);
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, TestModal::new);
        });
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(item.save_count, 0, "Modal open should not trigger save");
        });

        // Window deactivation should flush deferred saves.
        cx.deactivate_window();
        cx.executor().run_until_parked();
        item.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 1,
                "Window deactivation should flush deferred saves"
            );
        });
    }

    #[gpui::test]
    async fn test_autosave_deferred_until_pane_focus(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let item1 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
        });
        let item2 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(2, "2.txt", cx)])
        });

        let pane = workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item1.clone()), None, false, window, cx);
            workspace.add_item_to_active_pane(Box::new(item2.clone()), None, false, window, cx);
            workspace.active_pane().clone()
        });
        // Ensure added_to_pane is called for both items (sets up focus handlers)
        cx.executor().run_until_parked();

        // Activate item1 (at index 0) and focus it.
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(0, true, true, window, cx);
        });
        cx.executor().run_until_parked();

        // Set up OnFocusChange autosave and make item1 dirty.
        item1.update(cx, |item, cx| {
            SettingsStore::update_global(cx, |settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.workspace.autosave = Some(AutosaveSetting::OnFocusChange);
                })
            });
            item.is_dirty = true;
        });
        cx.executor().run_until_parked();

        // Activate item2 via the pane - this should trigger autosave of item1.
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(1, true, true, window, cx);
        });
        cx.executor().run_until_parked();

        item1.read_with(cx, |item, _| {
            assert_eq!(
                item.save_count, 1,
                "Switching to another item should trigger deferred save of the previous item"
            );
        });
    }

    #[gpui::test]
    async fn test_pane_navigation(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());

        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
        });
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let toolbar = pane.read_with(cx, |pane, _| pane.toolbar().clone());
        let toolbar_notify_count = Rc::new(RefCell::new(0));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, true, window, cx);
            let toolbar_notification_count = toolbar_notify_count.clone();
            cx.observe_in(&toolbar, window, move |_, _, _, _| {
                *toolbar_notification_count.borrow_mut() += 1
            })
            .detach();
        });

        pane.read_with(cx, |pane, _| {
            assert!(!pane.can_navigate_backward());
            assert!(!pane.can_navigate_forward());
        });

        item.update_in(cx, |item, _, cx| {
            item.set_state("one".to_string(), cx);
        });

        // Toolbar must be notified to re-render the navigation buttons
        assert_eq!(*toolbar_notify_count.borrow(), 1);

        pane.read_with(cx, |pane, _| {
            assert!(pane.can_navigate_backward());
            assert!(!pane.can_navigate_forward());
        });

        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.go_back(pane.downgrade(), window, cx)
            })
            .await
            .unwrap();

        assert_eq!(*toolbar_notify_count.borrow(), 2);
        pane.read_with(cx, |pane, _| {
            assert!(!pane.can_navigate_backward());
            assert!(pane.can_navigate_forward());
        });
    }

    /// Tests that the navigation history deduplicates entries for the same item.
    ///
    /// When navigating back and forth between items (e.g., A -> B -> A -> B -> A -> B -> C),
    /// the navigation history deduplicates by keeping only the most recent visit to each item,
    /// resulting in [A, B, C] instead of [A, B, A, B, A, B, C]. This ensures that Go Back (Ctrl-O)
    /// navigates through unique items efficiently: C -> B -> A, rather than bouncing between
    /// repeated entries: C -> B -> A -> B -> A -> B -> A.
    ///
    /// This behavior prevents the navigation history from growing unnecessarily large and provides
    /// a better user experience by eliminating redundant navigation steps when jumping between files.
    #[gpui::test]
    async fn test_navigation_history_deduplication(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let item_a = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "a.txt", cx)])
        });
        let item_b = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(2, "b.txt", cx)])
        });
        let item_c = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(3, "c.txt", cx)])
        });

        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item_a.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(Box::new(item_b.clone()), None, true, window, cx);
            workspace.add_item_to_active_pane(Box::new(item_c.clone()), None, true, window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_a, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_b, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_a, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_b, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_a, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_b, false, false, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&item_c, false, false, window, cx);
        });
        cx.run_until_parked();

        let backward_count = pane.read_with(cx, |pane, cx| {
            let mut count = 0;
            pane.nav_history().for_each_entry(cx, &mut |_, _| {
                count += 1;
            });
            count
        });
        assert!(
            backward_count <= 4,
            "Should have at most 4 entries, got {}",
            backward_count
        );

        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.go_back(pane.downgrade(), window, cx)
            })
            .await
            .unwrap();

        let active_item = workspace.read_with(cx, |workspace, cx| {
            workspace.active_item(cx).unwrap().item_id()
        });
        assert_eq!(
            active_item,
            item_b.entity_id(),
            "After first go_back, should be at item B"
        );

        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.go_back(pane.downgrade(), window, cx)
            })
            .await
            .unwrap();

        let active_item = workspace.read_with(cx, |workspace, cx| {
            workspace.active_item(cx).unwrap().item_id()
        });
        assert_eq!(
            active_item,
            item_a.entity_id(),
            "After second go_back, should be at item A"
        );

        pane.read_with(cx, |pane, _| {
            assert!(pane.can_navigate_forward(), "Should be able to go forward");
        });
    }

    #[gpui::test]
    async fn test_activate_last_pane(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        workspace.update_in(cx, |workspace, window, cx| {
            let first_item = cx.new(|cx| {
                TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "1.txt", cx)])
            });
            workspace.add_item_to_active_pane(Box::new(first_item), None, true, window, cx);
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Right,
                window,
                cx,
            );
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Right,
                window,
                cx,
            );
        });

        let (first_pane_id, target_last_pane_id) = workspace.update(cx, |workspace, _cx| {
            let panes = workspace.center.panes();
            assert!(panes.len() >= 2);
            (
                panes.first().expect("at least one pane").entity_id(),
                panes.last().expect("at least one pane").entity_id(),
            )
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_pane_at_index(&ActivatePane(0), window, cx);
        });
        workspace.update(cx, |workspace, _| {
            assert_eq!(workspace.active_pane().entity_id(), first_pane_id);
            assert_ne!(workspace.active_pane().entity_id(), target_last_pane_id);
        });

        cx.dispatch_action(ActivateLastPane);

        workspace.update(cx, |workspace, _| {
            assert_eq!(workspace.active_pane().entity_id(), target_last_pane_id);
        });
    }

    #[gpui::test]
    async fn test_pane_zoom_in_out(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());

        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        let pane = workspace.update_in(cx, |workspace, _window, _cx| {
            workspace.active_pane().clone()
        });

        // Add an item to the pane so it can be zoomed
        workspace.update_in(cx, |workspace, window, cx| {
            let item = cx.new(TestItem::new);
            workspace.add_item(pane.clone(), Box::new(item), None, true, true, window, cx);
        });

        // Initially not zoomed
        workspace.update_in(cx, |workspace, _window, cx| {
            assert!(!pane.read(cx).is_zoomed(), "Pane starts unzoomed");
            assert!(
                workspace.zoomed.is_none(),
                "Workspace should track no zoomed pane"
            );
            assert!(pane.read(cx).items_len() > 0, "Pane should have items");
        });

        // Zoom In
        pane.update_in(cx, |pane, window, cx| {
            pane.zoom_in(&crate::ZoomIn, window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                pane.read(cx).is_zoomed(),
                "Pane should be zoomed after ZoomIn"
            );
            assert!(
                workspace.zoomed.is_some(),
                "Workspace should track the zoomed pane"
            );
            assert!(
                pane.read(cx).focus_handle(cx).contains_focused(window, cx),
                "ZoomIn should focus the pane"
            );
        });

        // Zoom In again is a no-op
        pane.update_in(cx, |pane, window, cx| {
            pane.zoom_in(&crate::ZoomIn, window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(pane.read(cx).is_zoomed(), "Second ZoomIn keeps pane zoomed");
            assert!(
                workspace.zoomed.is_some(),
                "Workspace still tracks zoomed pane"
            );
            assert!(
                pane.read(cx).focus_handle(cx).contains_focused(window, cx),
                "Pane remains focused after repeated ZoomIn"
            );
        });

        // Zoom Out
        pane.update_in(cx, |pane, window, cx| {
            pane.zoom_out(&crate::ZoomOut, window, cx);
        });

        workspace.update_in(cx, |workspace, _window, cx| {
            assert!(
                !pane.read(cx).is_zoomed(),
                "Pane should unzoom after ZoomOut"
            );
            assert!(
                workspace.zoomed.is_none(),
                "Workspace clears zoom tracking after ZoomOut"
            );
        });

        // Zoom Out again is a no-op
        pane.update_in(cx, |pane, window, cx| {
            pane.zoom_out(&crate::ZoomOut, window, cx);
        });

        workspace.update_in(cx, |workspace, _window, cx| {
            assert!(
                !pane.read(cx).is_zoomed(),
                "Second ZoomOut keeps pane unzoomed"
            );
            assert!(
                workspace.zoomed.is_none(),
                "Workspace remains without zoomed pane"
            );
        });
    }

    #[gpui::test]
    async fn test_join_pane_into_next(cx: &mut gpui::TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());

        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        // Let's arrange the panes like this:
        //
        // +-----------------------+
        // |         top           |
        // +------+--------+-------+
        // | left | center | right |
        // +------+--------+-------+
        // |        bottom         |
        // +-----------------------+

        let top_item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "top.txt", cx)])
        });
        let bottom_item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(2, "bottom.txt", cx)])
        });
        let left_item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(3, "left.txt", cx)])
        });
        let right_item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(4, "right.txt", cx)])
        });
        let center_item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(5, "center.txt", cx)])
        });

        let top_pane_id = workspace.update_in(cx, |workspace, window, cx| {
            let top_pane_id = workspace.active_pane().entity_id();
            workspace.add_item_to_active_pane(Box::new(top_item.clone()), None, false, window, cx);
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Down,
                window,
                cx,
            );
            top_pane_id
        });
        let bottom_pane_id = workspace.update_in(cx, |workspace, window, cx| {
            let bottom_pane_id = workspace.active_pane().entity_id();
            workspace.add_item_to_active_pane(
                Box::new(bottom_item.clone()),
                None,
                false,
                window,
                cx,
            );
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Up,
                window,
                cx,
            );
            bottom_pane_id
        });
        let left_pane_id = workspace.update_in(cx, |workspace, window, cx| {
            let left_pane_id = workspace.active_pane().entity_id();
            workspace.add_item_to_active_pane(Box::new(left_item.clone()), None, false, window, cx);
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Right,
                window,
                cx,
            );
            left_pane_id
        });
        let right_pane_id = workspace.update_in(cx, |workspace, window, cx| {
            let right_pane_id = workspace.active_pane().entity_id();
            workspace.add_item_to_active_pane(
                Box::new(right_item.clone()),
                None,
                false,
                window,
                cx,
            );
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Left,
                window,
                cx,
            );
            right_pane_id
        });
        let center_pane_id = workspace.update_in(cx, |workspace, window, cx| {
            let center_pane_id = workspace.active_pane().entity_id();
            workspace.add_item_to_active_pane(
                Box::new(center_item.clone()),
                None,
                false,
                window,
                cx,
            );
            center_pane_id
        });
        cx.executor().run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert_eq!(center_pane_id, workspace.active_pane().entity_id());

            // Join into next from center pane into right
            workspace.join_pane_into_next(workspace.active_pane().clone(), window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let active_pane = workspace.active_pane();
            assert_eq!(right_pane_id, active_pane.entity_id());
            assert_eq!(2, active_pane.read(cx).items_len());
            let item_ids_in_pane =
                HashSet::from_iter(active_pane.read(cx).items().map(|item| item.item_id()));
            assert!(item_ids_in_pane.contains(&center_item.item_id()));
            assert!(item_ids_in_pane.contains(&right_item.item_id()));

            // Join into next from right pane into bottom
            workspace.join_pane_into_next(workspace.active_pane().clone(), window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let active_pane = workspace.active_pane();
            assert_eq!(bottom_pane_id, active_pane.entity_id());
            assert_eq!(3, active_pane.read(cx).items_len());
            let item_ids_in_pane =
                HashSet::from_iter(active_pane.read(cx).items().map(|item| item.item_id()));
            assert!(item_ids_in_pane.contains(&center_item.item_id()));
            assert!(item_ids_in_pane.contains(&right_item.item_id()));
            assert!(item_ids_in_pane.contains(&bottom_item.item_id()));

            // Join into next from bottom pane into left
            workspace.join_pane_into_next(workspace.active_pane().clone(), window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let active_pane = workspace.active_pane();
            assert_eq!(left_pane_id, active_pane.entity_id());
            assert_eq!(4, active_pane.read(cx).items_len());
            let item_ids_in_pane =
                HashSet::from_iter(active_pane.read(cx).items().map(|item| item.item_id()));
            assert!(item_ids_in_pane.contains(&center_item.item_id()));
            assert!(item_ids_in_pane.contains(&right_item.item_id()));
            assert!(item_ids_in_pane.contains(&bottom_item.item_id()));
            assert!(item_ids_in_pane.contains(&left_item.item_id()));

            // Join into next from left pane into top
            workspace.join_pane_into_next(workspace.active_pane().clone(), window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let active_pane = workspace.active_pane();
            assert_eq!(top_pane_id, active_pane.entity_id());
            assert_eq!(5, active_pane.read(cx).items_len());
            let item_ids_in_pane =
                HashSet::from_iter(active_pane.read(cx).items().map(|item| item.item_id()));
            assert!(item_ids_in_pane.contains(&center_item.item_id()));
            assert!(item_ids_in_pane.contains(&right_item.item_id()));
            assert!(item_ids_in_pane.contains(&bottom_item.item_id()));
            assert!(item_ids_in_pane.contains(&left_item.item_id()));
            assert!(item_ids_in_pane.contains(&top_item.item_id()));

            // Single pane left: no-op
            workspace.join_pane_into_next(workspace.active_pane().clone(), window, cx)
        });

        workspace.update(cx, |workspace, _cx| {
            let active_pane = workspace.active_pane();
            assert_eq!(top_pane_id, active_pane.entity_id());
        });
    }

    fn add_an_item_to_active_pane(
        cx: &mut VisualTestContext,
        workspace: &Entity<Workspace>,
        item_id: u64,
    ) -> Entity<TestItem> {
        let item = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(
                item_id,
                "item{item_id}.txt",
                cx,
            )])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item.clone()), None, false, window, cx);
        });
        item
    }

    fn split_pane(cx: &mut VisualTestContext, workspace: &Entity<Workspace>) -> Entity<Pane> {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_pane(
                workspace.active_pane().clone(),
                SplitDirection::Right,
                window,
                cx,
            )
        })
    }

    #[gpui::test]
    async fn test_join_all_panes(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, None, cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));

        add_an_item_to_active_pane(cx, &workspace, 1);
        split_pane(cx, &workspace);
        add_an_item_to_active_pane(cx, &workspace, 2);
        split_pane(cx, &workspace); // empty pane
        split_pane(cx, &workspace);
        let last_item = add_an_item_to_active_pane(cx, &workspace, 3);

        cx.executor().run_until_parked();

        workspace.update(cx, |workspace, cx| {
            let num_panes = workspace.panes().len();
            let num_items_in_current_pane = workspace.active_pane().read(cx).items().count();
            let active_item = workspace
                .active_pane()
                .read(cx)
                .active_item()
                .expect("item is in focus");

            assert_eq!(num_panes, 4);
            assert_eq!(num_items_in_current_pane, 1);
            assert_eq!(active_item.item_id(), last_item.item_id());
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.join_all_panes(window, cx);
        });

        workspace.update(cx, |workspace, cx| {
            let num_panes = workspace.panes().len();
            let num_items_in_current_pane = workspace.active_pane().read(cx).items().count();
            let active_item = workspace
                .active_pane()
                .read(cx)
                .active_item()
                .expect("item is in focus");

            assert_eq!(num_panes, 1);
            assert_eq!(num_items_in_current_pane, 3);
            assert_eq!(active_item.item_id(), last_item.item_id());
        });
    }

    struct TestModal(FocusHandle);

    impl TestModal {
        fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
            Self(cx.focus_handle())
        }
    }

    impl EventEmitter<DismissEvent> for TestModal {}

    impl Focusable for TestModal {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.0.clone()
        }
    }

    impl ModalView for TestModal {}

    impl Render for TestModal {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<TestModal>,
        ) -> impl IntoElement {
            div().track_focus(&self.0)
        }
    }

    #[gpui::test]
    async fn test_no_save_prompt_when_multi_buffer_dirty_items_closed(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        let dirty_regular_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("1.txt")
                .with_project_items(&[dirty_project_item(1, "1.txt", cx)])
        });
        let dirty_regular_buffer_2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("2.txt")
                .with_project_items(&[dirty_project_item(2, "2.txt", cx)])
        });
        let dirty_multi_buffer_with_both = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_buffer_kind(ItemBufferKind::Multibuffer)
                .with_label("Fake Project Search")
                .with_project_items(&[
                    dirty_regular_buffer.read(cx).project_items[0].clone(),
                    dirty_regular_buffer_2.read(cx).project_items[0].clone(),
                ])
        });
        let multi_buffer_with_both_files_id = dirty_multi_buffer_with_both.item_id();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_regular_buffer.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_regular_buffer_2.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_multi_buffer_with_both.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(2, true, true, window, cx);
            assert_eq!(
                pane.active_item().unwrap().item_id(),
                multi_buffer_with_both_files_id,
                "Should select the multi buffer in the pane"
            );
        });
        let close_all_but_multi_buffer_task = pane.update_in(cx, |pane, window, cx| {
            pane.close_other_items(
                &CloseOtherItems {
                    save_intent: Some(SaveIntent::Save),
                },
                None,
                window,
                cx,
            )
        });
        cx.background_executor.run_until_parked();
        assert!(!cx.has_pending_prompt());
        close_all_but_multi_buffer_task
            .await
            .expect("Closing all buffers but the multi buffer failed");
        pane.update(cx, |pane, cx| {
            assert_eq!(dirty_regular_buffer.read(cx).save_count, 1);
            assert_eq!(dirty_multi_buffer_with_both.read(cx).save_count, 0);
            assert_eq!(dirty_regular_buffer_2.read(cx).save_count, 1);
            assert_eq!(pane.items_len(), 1);
            assert_eq!(
                pane.active_item().unwrap().item_id(),
                multi_buffer_with_both_files_id,
                "Should have only the multi buffer left in the pane"
            );
            assert!(
                dirty_multi_buffer_with_both.read(cx).is_dirty,
                "The multi buffer containing the unsaved buffer should still be dirty"
            );
        });

        dirty_regular_buffer.update(cx, |buffer, cx| {
            buffer.project_items[0].update(cx, |pi, _| pi.is_dirty = true)
        });

        let close_multi_buffer_task = pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(
                &CloseActiveItem {
                    save_intent: Some(SaveIntent::Close),
                },
                window,
                cx,
            )
        });
        cx.background_executor.run_until_parked();
        assert!(
            cx.has_pending_prompt(),
            "Dirty multi buffer should prompt a save dialog"
        );
        cx.simulate_prompt_answer("Save");
        cx.background_executor.run_until_parked();
        close_multi_buffer_task
            .await
            .expect("Closing the multi buffer failed");
        pane.update(cx, |pane, cx| {
            assert_eq!(
                dirty_multi_buffer_with_both.read(cx).save_count,
                1,
                "Multi buffer item should get be saved"
            );
            // Test impl does not save inner items, so we do not assert them
            assert_eq!(
                pane.items_len(),
                0,
                "No more items should be left in the pane"
            );
            assert!(pane.active_item().is_none());
        });
    }

    #[gpui::test]
    async fn test_save_prompt_when_dirty_multi_buffer_closed_with_some_of_its_dirty_items_not_present_in_the_pane(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        let dirty_regular_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("1.txt")
                .with_project_items(&[dirty_project_item(1, "1.txt", cx)])
        });
        let dirty_regular_buffer_2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("2.txt")
                .with_project_items(&[dirty_project_item(2, "2.txt", cx)])
        });
        let clear_regular_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("3.txt")
                .with_project_items(&[TestProjectItem::new(3, "3.txt", cx)])
        });

        let dirty_multi_buffer_with_both = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_buffer_kind(ItemBufferKind::Multibuffer)
                .with_label("Fake Project Search")
                .with_project_items(&[
                    dirty_regular_buffer.read(cx).project_items[0].clone(),
                    dirty_regular_buffer_2.read(cx).project_items[0].clone(),
                    clear_regular_buffer.read(cx).project_items[0].clone(),
                ])
        });
        let multi_buffer_with_both_files_id = dirty_multi_buffer_with_both.item_id();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_regular_buffer.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_multi_buffer_with_both.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(1, true, true, window, cx);
            assert_eq!(
                pane.active_item().unwrap().item_id(),
                multi_buffer_with_both_files_id,
                "Should select the multi buffer in the pane"
            );
        });
        let _close_multi_buffer_task = pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(
                &CloseActiveItem {
                    save_intent: None,
                },
                window,
                cx,
            )
        });
        cx.background_executor.run_until_parked();
        assert!(
            cx.has_pending_prompt(),
            "With one dirty item from the multi buffer not being in the pane, a save prompt should be shown"
        );
    }

    /// Tests that when `close_on_file_delete` is enabled, files are automatically
    /// closed when they are deleted from disk.
    #[gpui::test]
    async fn test_close_on_disk_deletion_enabled(cx: &mut TestAppContext) {
        init_test(cx);

        // Enable the close_on_disk_deletion setting
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.workspace.close_on_file_delete = Some(true);
            });
        });

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        // Create a test item that simulates a file
        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("test.txt")
                .with_project_items(&[TestProjectItem::new(1, "test.txt", cx)])
        });

        // Add item to workspace
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(item.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        // Verify the item is in the pane
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items().count(), 1);
        });

        // Simulate file deletion by setting the item's deleted state
        item.update(cx, |item, _| {
            item.set_has_deleted_file(true);
        });

        // Emit UpdateTab event to trigger the close behavior
        cx.run_until_parked();
        item.update(cx, |_, cx| {
            cx.emit(ItemEvent::UpdateTab);
        });

        // Allow the close operation to complete
        cx.run_until_parked();

        // Verify the item was automatically closed
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.items().count(),
                0,
                "Item should be automatically closed when file is deleted"
            );
        });
    }

    /// Tests that when `close_on_file_delete` is disabled (default), files remain
    /// open with a strikethrough when they are deleted from disk.
    #[gpui::test]
    async fn test_close_on_disk_deletion_disabled(cx: &mut TestAppContext) {
        init_test(cx);

        // Ensure close_on_disk_deletion is disabled (default)
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.workspace.close_on_file_delete = Some(false);
            });
        });

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        // Create a test item that simulates a file
        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("test.txt")
                .with_project_items(&[TestProjectItem::new(1, "test.txt", cx)])
        });

        // Add item to workspace
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(item.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        // Verify the item is in the pane
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items().count(), 1);
        });

        // Simulate file deletion
        item.update(cx, |item, _| {
            item.set_has_deleted_file(true);
        });

        // Emit UpdateTab event
        cx.run_until_parked();
        item.update(cx, |_, cx| {
            cx.emit(ItemEvent::UpdateTab);
        });

        // Allow any potential close operation to complete
        cx.run_until_parked();

        // Verify the item remains open (with strikethrough)
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.items().count(),
                1,
                "Item should remain open when close_on_disk_deletion is disabled"
            );
        });

        // Verify the item shows as deleted
        item.read_with(cx, |item, _| {
            assert!(
                item.has_deleted_file,
                "Item should be marked as having deleted file"
            );
        });
    }

    /// Tests that dirty files are not automatically closed when deleted from disk,
    /// even when `close_on_file_delete` is enabled. This ensures users don't lose
    /// unsaved changes without being prompted.
    #[gpui::test]
    async fn test_close_on_disk_deletion_with_dirty_file(cx: &mut TestAppContext) {
        init_test(cx);

        // Enable the close_on_file_delete setting
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.workspace.close_on_file_delete = Some(true);
            });
        });

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        // Create a dirty test item
        let item = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("test.txt")
                .with_project_items(&[TestProjectItem::new(1, "test.txt", cx)])
        });

        // Add item to workspace
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(item.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        // Simulate file deletion
        item.update(cx, |item, _| {
            item.set_has_deleted_file(true);
        });

        // Emit UpdateTab event to trigger the close behavior
        cx.run_until_parked();
        item.update(cx, |_, cx| {
            cx.emit(ItemEvent::UpdateTab);
        });

        // Allow any potential close operation to complete
        cx.run_until_parked();

        // Verify the item remains open (dirty files are not auto-closed)
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.items().count(),
                1,
                "Dirty items should not be automatically closed even when file is deleted"
            );
        });

        // Verify the item is marked as deleted and still dirty
        item.read_with(cx, |item, _| {
            assert!(
                item.has_deleted_file,
                "Item should be marked as having deleted file"
            );
            assert!(item.is_dirty, "Item should still be dirty");
        });
    }

    /// Tests that navigation history is cleaned up when files are auto-closed
    /// due to deletion from disk.
    #[gpui::test]
    async fn test_close_on_disk_deletion_cleans_navigation_history(cx: &mut TestAppContext) {
        init_test(cx);

        // Enable the close_on_file_delete setting
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.workspace.close_on_file_delete = Some(true);
            });
        });

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        // Create test items
        let item1 = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("test1.txt")
                .with_project_items(&[TestProjectItem::new(1, "test1.txt", cx)])
        });
        let item1_id = item1.item_id();

        let item2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("test2.txt")
                .with_project_items(&[TestProjectItem::new(2, "test2.txt", cx)])
        });

        // Add items to workspace
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(item1.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(item2.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        // Activate item1 to ensure it gets navigation entries
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(0, true, true, window, cx);
        });

        // Switch to item2 and back to create navigation history
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(1, true, true, window, cx);
        });
        cx.run_until_parked();

        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(0, true, true, window, cx);
        });
        cx.run_until_parked();

        // Simulate file deletion for item1
        item1.update(cx, |item, _| {
            item.set_has_deleted_file(true);
        });

        // Emit UpdateTab event to trigger the close behavior
        item1.update(cx, |_, cx| {
            cx.emit(ItemEvent::UpdateTab);
        });
        cx.run_until_parked();

        // Verify item1 was closed
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.items().count(),
                1,
                "Should have 1 item remaining after auto-close"
            );
        });

        // Check navigation history after close
        let has_item = pane.read_with(cx, |pane, cx| {
            let mut has_item = false;
            pane.nav_history().for_each_entry(cx, &mut |entry, _| {
                if entry.item.id() == item1_id {
                    has_item = true;
                }
            });
            has_item
        });

        assert!(
            !has_item,
            "Navigation history should not contain closed item entries"
        );
    }

    #[gpui::test]
    async fn test_no_save_prompt_when_dirty_multi_buffer_closed_with_all_of_its_dirty_items_present_in_the_pane(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let fs = FakeFs::new(cx.background_executor.clone());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        let dirty_regular_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("1.txt")
                .with_project_items(&[dirty_project_item(1, "1.txt", cx)])
        });
        let dirty_regular_buffer_2 = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_label("2.txt")
                .with_project_items(&[dirty_project_item(2, "2.txt", cx)])
        });
        let clear_regular_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_label("3.txt")
                .with_project_items(&[TestProjectItem::new(3, "3.txt", cx)])
        });

        let dirty_multi_buffer = cx.new(|cx| {
            TestItem::new(cx)
                .with_dirty(true)
                .with_buffer_kind(ItemBufferKind::Multibuffer)
                .with_label("Fake Project Search")
                .with_project_items(&[
                    dirty_regular_buffer.read(cx).project_items[0].clone(),
                    dirty_regular_buffer_2.read(cx).project_items[0].clone(),
                    clear_regular_buffer.read(cx).project_items[0].clone(),
                ])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_regular_buffer.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_regular_buffer_2.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
            workspace.add_item(
                pane.clone(),
                Box::new(dirty_multi_buffer.clone()),
                None,
                false,
                false,
                window,
                cx,
            );
        });

        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(2, true, true, window, cx);
            assert_eq!(
                pane.active_item().unwrap().item_id(),
                dirty_multi_buffer.item_id(),
                "Should select the multi buffer in the pane"
            );
        });
        let close_multi_buffer_task = pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(
                &CloseActiveItem {
                    save_intent: None,
                },
                window,
                cx,
            )
        });
        cx.background_executor.run_until_parked();
        assert!(
            !cx.has_pending_prompt(),
            "All dirty items from the multi buffer are in the pane still, no save prompts should be shown"
        );
        close_multi_buffer_task
            .await
            .expect("Closing multi buffer failed");
        pane.update(cx, |pane, cx| {
            assert_eq!(dirty_regular_buffer.read(cx).save_count, 0);
            assert_eq!(dirty_multi_buffer.read(cx).save_count, 0);
            assert_eq!(dirty_regular_buffer_2.read(cx).save_count, 0);
            assert_eq!(
                pane.items()
                    .map(|item| item.item_id())
                    .sorted()
                    .collect::<Vec<_>>(),
                vec![
                    dirty_regular_buffer.item_id(),
                    dirty_regular_buffer_2.item_id(),
                ],
                "Should have no multi buffer left in the pane"
            );
            assert!(dirty_regular_buffer.read(cx).is_dirty);
            assert!(dirty_regular_buffer_2.read(cx).is_dirty);
        });
    }

    #[gpui::test]
    async fn test_active_pane_updates_to_focus_target_on_removal(cx: &mut TestAppContext) {
        assert_active_pane_is_replaced_after_removal(cx, true).await;
    }

    #[gpui::test]
    async fn test_active_pane_updates_to_fallback_on_removal(cx: &mut TestAppContext) {
        assert_active_pane_is_replaced_after_removal(cx, false).await;
    }

    async fn assert_active_pane_is_replaced_after_removal(
        cx: &mut TestAppContext,
        use_focus_target: bool,
    ) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            let first_pane = workspace.active_pane().clone();
            let second_pane =
                workspace.split_pane(first_pane.clone(), SplitDirection::Right, window, cx);
            workspace.set_active_pane(&second_pane, window, cx);

            let focus_target = use_focus_target.then(|| first_pane.clone());
            workspace.remove_pane(second_pane, focus_target, window, cx);

            assert_eq!(workspace.active_pane(), &first_pane);
            assert!(
                workspace
                    .panes()
                    .iter()
                    .any(|pane| pane == workspace.active_pane()),
                "active pane should be one of the remaining workspace panes"
            );
        });
    }

    #[gpui::test]
    async fn test_moving_items_create_panes(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item_1 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "first.txt", cx)])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item_1), None, true, window, cx);
            workspace.move_item_to_pane_in_direction(
                &MoveItemToPaneInDirection {
                    direction: SplitDirection::Right,
                    focus: true,
                    clone: false,
                },
                window,
                cx,
            );
            workspace.move_item_to_pane_at_index(
                &MoveItemToPane {
                    destination: 3,
                    focus: true,
                    clone: false,
                },
                window,
                cx,
            );

            assert_eq!(workspace.panes.len(), 1, "No new panes were created");
            assert_eq!(
                pane_items_paths(&workspace.active_pane, cx),
                vec!["first.txt".to_string()],
                "Single item was not moved anywhere"
            );
        });

        let item_2 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(2, "second.txt", cx)])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item_2), None, true, window, cx);
            assert_eq!(
                pane_items_paths(&workspace.panes[0], cx),
                vec!["first.txt".to_string(), "second.txt".to_string()],
            );
            workspace.move_item_to_pane_in_direction(
                &MoveItemToPaneInDirection {
                    direction: SplitDirection::Right,
                    focus: true,
                    clone: false,
                },
                window,
                cx,
            );

            assert_eq!(workspace.panes.len(), 2, "A new pane should be created");
            assert_eq!(
                pane_items_paths(&workspace.panes[0], cx),
                vec!["first.txt".to_string()],
                "After moving, one item should be left in the original pane"
            );
            assert_eq!(
                pane_items_paths(&workspace.panes[1], cx),
                vec!["second.txt".to_string()],
                "New item should have been moved to the new pane"
            );
        });

        let item_3 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(3, "third.txt", cx)])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            let original_pane = workspace.panes[0].clone();
            workspace.set_active_pane(&original_pane, window, cx);
            workspace.add_item_to_active_pane(Box::new(item_3), None, true, window, cx);
            assert_eq!(workspace.panes.len(), 2, "No new panes were created");
            assert_eq!(
                pane_items_paths(&workspace.active_pane, cx),
                vec!["first.txt".to_string(), "third.txt".to_string()],
                "New pane should be ready to move one item out"
            );

            workspace.move_item_to_pane_at_index(
                &MoveItemToPane {
                    destination: 3,
                    focus: true,
                    clone: false,
                },
                window,
                cx,
            );
            assert_eq!(workspace.panes.len(), 3, "A new pane should be created");
            assert_eq!(
                pane_items_paths(&workspace.active_pane, cx),
                vec!["first.txt".to_string()],
                "After moving, one item should be left in the original pane"
            );
            assert_eq!(
                pane_items_paths(&workspace.panes[1], cx),
                vec!["second.txt".to_string()],
                "Previously created pane should be unchanged"
            );
            assert_eq!(
                pane_items_paths(&workspace.panes[2], cx),
                vec!["third.txt".to_string()],
                "New item should have been moved to the new pane"
            );
        });
    }

    #[gpui::test]
    async fn test_moving_items_can_clone_panes(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let item_1 = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new(1, "first.txt", cx)])
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item_1), None, true, window, cx);
            workspace.move_item_to_pane_in_direction(
                &MoveItemToPaneInDirection {
                    direction: SplitDirection::Right,
                    focus: true,
                    clone: true,
                },
                window,
                cx,
            );
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.move_item_to_pane_at_index(
                &MoveItemToPane {
                    destination: 3,
                    focus: true,
                    clone: true,
                },
                window,
                cx,
            );
        });
        cx.run_until_parked();

        workspace.update(cx, |workspace, cx| {
            assert_eq!(workspace.panes.len(), 3, "Two new panes were created");
            for pane in workspace.panes() {
                assert_eq!(
                    pane_items_paths(pane, cx),
                    vec!["first.txt".to_string()],
                    "Single item exists in all panes"
                );
            }
        });

        // verify that the active pane has been updated after waiting for the
        // pane focus event to fire and resolve
        workspace.read_with(cx, |workspace, _app| {
            assert_eq!(
                workspace.active_pane(),
                &workspace.panes[2],
                "The third pane should be the active one: {:?}",
                workspace.panes
            );
        })
    }

    mod register_project_item_tests {

        use super::*;

        // View
        struct TestPngItemView {
            focus_handle: FocusHandle,
        }
        // Model
        struct TestPngItem {}

        impl project::ProjectItem for TestPngItem {
            fn try_open(
                _project: &Entity<Project>,
                path: &ProjectPath,
                cx: &mut App,
            ) -> Option<Task<anyhow::Result<Entity<Self>>>> {
                if path.path.extension().unwrap() == "png" {
                    Some(cx.spawn(async move |cx| Ok(cx.new(|_| TestPngItem {}))))
                } else {
                    None
                }
            }

            fn entry_id(&self, _: &App) -> Option<ProjectEntryId> {
                None
            }

            fn project_path(&self, _: &App) -> Option<ProjectPath> {
                None
            }

            fn is_dirty(&self) -> bool {
                false
            }
        }

        impl Item for TestPngItemView {
            type Event = ();
            fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
                "".into()
            }
        }
        impl EventEmitter<()> for TestPngItemView {}
        impl Focusable for TestPngItemView {
            fn focus_handle(&self, _cx: &App) -> FocusHandle {
                self.focus_handle.clone()
            }
        }

        impl Render for TestPngItemView {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                Empty
            }
        }

        impl ProjectItem for TestPngItemView {
            type Item = TestPngItem;

            fn for_project_item(
                _project: Entity<Project>,
                _pane: Option<&Pane>,
                _item: Entity<Self::Item>,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) -> Self
            where
                Self: Sized,
            {
                Self {
                    focus_handle: cx.focus_handle(),
                }
            }
        }

        // View
        struct TestIpynbItemView {
            focus_handle: FocusHandle,
        }
        // Model
        struct TestIpynbItem {}

        impl project::ProjectItem for TestIpynbItem {
            fn try_open(
                _project: &Entity<Project>,
                path: &ProjectPath,
                cx: &mut App,
            ) -> Option<Task<anyhow::Result<Entity<Self>>>> {
                if path.path.extension().unwrap() == "ipynb" {
                    Some(cx.spawn(async move |cx| Ok(cx.new(|_| TestIpynbItem {}))))
                } else {
                    None
                }
            }

            fn entry_id(&self, _: &App) -> Option<ProjectEntryId> {
                None
            }

            fn project_path(&self, _: &App) -> Option<ProjectPath> {
                None
            }

            fn is_dirty(&self) -> bool {
                false
            }
        }

        impl Item for TestIpynbItemView {
            type Event = ();
            fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
                "".into()
            }
        }
        impl EventEmitter<()> for TestIpynbItemView {}
        impl Focusable for TestIpynbItemView {
            fn focus_handle(&self, _cx: &App) -> FocusHandle {
                self.focus_handle.clone()
            }
        }

        impl Render for TestIpynbItemView {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                Empty
            }
        }

        impl ProjectItem for TestIpynbItemView {
            type Item = TestIpynbItem;

            fn for_project_item(
                _project: Entity<Project>,
                _pane: Option<&Pane>,
                _item: Entity<Self::Item>,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) -> Self
            where
                Self: Sized,
            {
                Self {
                    focus_handle: cx.focus_handle(),
                }
            }
        }

        struct TestAlternatePngItemView {
            focus_handle: FocusHandle,
        }

        impl Item for TestAlternatePngItemView {
            type Event = ();
            fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
                "".into()
            }
        }

        impl EventEmitter<()> for TestAlternatePngItemView {}
        impl Focusable for TestAlternatePngItemView {
            fn focus_handle(&self, _cx: &App) -> FocusHandle {
                self.focus_handle.clone()
            }
        }

        impl Render for TestAlternatePngItemView {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                Empty
            }
        }

        impl ProjectItem for TestAlternatePngItemView {
            type Item = TestPngItem;

            fn for_project_item(
                _project: Entity<Project>,
                _pane: Option<&Pane>,
                _item: Entity<Self::Item>,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) -> Self
            where
                Self: Sized,
            {
                Self {
                    focus_handle: cx.focus_handle(),
                }
            }
        }

        #[gpui::test]
        async fn test_register_project_item(cx: &mut TestAppContext) {
            init_test(cx);

            cx.update(|cx| {
                register_project_item::<TestPngItemView>(cx);
                register_project_item::<TestIpynbItemView>(cx);
            });

            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(
                "/root1",
                json!({
                    "one.png": "BINARYDATAHERE",
                    "two.ipynb": "{ totally a notebook }",
                    "three.txt": "editing text, sure why not?"
                }),
            )
            .await;

            let project = Project::test(fs, ["root1".as_ref()], cx).await;
            let (workspace, cx) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

            let worktree_id = project.update(cx, |project, cx| {
                project.worktrees(cx).next().unwrap().read(cx).id()
            });

            let handle = workspace
                .update_in(cx, |workspace, window, cx| {
                    let project_path = (worktree_id, rel_path("one.png"));
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .await
                .unwrap();

            // Now we can check if the handle we got back errored or not
            assert_eq!(
                handle.to_any_view().entity_type(),
                TypeId::of::<TestPngItemView>()
            );

            let handle = workspace
                .update_in(cx, |workspace, window, cx| {
                    let project_path = (worktree_id, rel_path("two.ipynb"));
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .await
                .unwrap();

            assert_eq!(
                handle.to_any_view().entity_type(),
                TypeId::of::<TestIpynbItemView>()
            );

            let handle = workspace
                .update_in(cx, |workspace, window, cx| {
                    let project_path = (worktree_id, rel_path("three.txt"));
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .await;
            assert!(handle.is_err());
        }

        #[gpui::test]
        async fn test_register_project_item_two_enter_one_leaves(cx: &mut TestAppContext) {
            init_test(cx);

            cx.update(|cx| {
                register_project_item::<TestPngItemView>(cx);
                register_project_item::<TestAlternatePngItemView>(cx);
            });

            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(
                "/root1",
                json!({
                    "one.png": "BINARYDATAHERE",
                    "two.ipynb": "{ totally a notebook }",
                    "three.txt": "editing text, sure why not?"
                }),
            )
            .await;
            let project = Project::test(fs, ["root1".as_ref()], cx).await;
            let (workspace, cx) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
            let worktree_id = project.update(cx, |project, cx| {
                project.worktrees(cx).next().unwrap().read(cx).id()
            });

            let handle = workspace
                .update_in(cx, |workspace, window, cx| {
                    let project_path = (worktree_id, rel_path("one.png"));
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .await
                .unwrap();

            // This _must_ be the second item registered
            assert_eq!(
                handle.to_any_view().entity_type(),
                TypeId::of::<TestAlternatePngItemView>()
            );

            let handle = workspace
                .update_in(cx, |workspace, window, cx| {
                    let project_path = (worktree_id, rel_path("three.txt"));
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .await;
            assert!(handle.is_err());
        }
    }

    fn pane_items_paths(pane: &Entity<Pane>, cx: &App) -> Vec<String> {
        pane.read(cx)
            .items()
            .flat_map(|item| {
                item.project_paths(cx)
                    .into_iter()
                    .map(|path| path.path.display(PathStyle::local()).into_owned())
            })
            .collect()
    }

    pub fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    #[gpui::test]
    async fn test_toggle_theme_mode_persists_and_updates_active_theme(cx: &mut TestAppContext) {
        use settings::{ThemeName, ThemeSelection};
        use theme::SystemAppearance;
        use zed_actions::theme::ToggleMode;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let settings_fs: Arc<dyn fs::Fs> = fs.clone();

        fs.insert_tree(path!("/root"), json!({ "file.rs": "fn main() {}\n" }))
            .await;

        // Build a test project and workspace view so the test can invoke
        // the workspace action handler the same way the UI would.
        let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        // Seed the settings file with a plain static light theme so the
        // first toggle always starts from a known persisted state.
        workspace.update_in(cx, |_workspace, _window, cx| {
            *SystemAppearance::global_mut(cx) = SystemAppearance(theme::Appearance::Light);
            settings::update_settings_file(settings_fs.clone(), cx, |settings, _cx| {
                settings.theme.theme = Some(ThemeSelection::Static(ThemeName("One Light".into())));
            });
        });
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();

        // Confirm the initial persisted settings contain the static theme
        // we just wrote before any toggling happens.
        let settings_text = SettingsStore::load_settings(&settings_fs).await.unwrap();
        assert!(settings_text.contains(r#""theme": "One Light""#));

        // Toggle once. This should migrate the persisted theme settings
        // into light/dark slots and enable system mode.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_theme_mode(&ToggleMode, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();

        // 1. Static -> Dynamic
        // this assertion checks theme changed from static to dynamic.
        let settings_text = SettingsStore::load_settings(&settings_fs).await.unwrap();
        let parsed: serde_json::Value = settings::parse_json_with_comments(&settings_text).unwrap();
        assert_eq!(
            parsed["theme"],
            serde_json::json!({
                "mode": "system",
                "light": "One Light",
                "dark": "One Dark"
            })
        );

        // 2. Toggle again, suppose it will change the mode to light
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_theme_mode(&ToggleMode, window, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();

        let settings_text = SettingsStore::load_settings(&settings_fs).await.unwrap();
        assert!(settings_text.contains(r#""mode": "light""#));
    }

    fn dirty_project_item(id: u64, path: &str, cx: &mut App) -> Entity<TestProjectItem> {
        let item = TestProjectItem::new(id, path, cx);
        item.update(cx, |item, _| {
            item.is_dirty = true;
        });
        item
    }

    fn new_test_project_item(
        id: u64,
        path: &str,
        worktree_id: WorktreeId,
        cx: &mut App,
    ) -> Entity<TestProjectItem> {
        let item = TestProjectItem::new(id, path, cx);
        item.update(cx, |item, _| {
            if let Some(ref mut project_path) = item.project_path {
                project_path.worktree_id = worktree_id;
            }
        });
        item
    }

    #[gpui::test]
    async fn test_most_recent_active_path_skips_read_only_paths(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({
                "src": { "main.py": "" },
                ".venv": { "lib": { "dep.py": "" } },
            }),
        )
        .await;

        let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        // Configure .venv as read-only
        workspace.update_in(cx, |_workspace, _window, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(r#"{"read_only_files": ["**/.venv/**"]}"#, cx)
                    .ok();
            });
        });

        let item_dep = cx.new(|cx| {
            TestItem::new(cx).with_project_items(&[TestProjectItem::new_in_worktree(
                1001,
                ".venv/lib/dep.py",
                worktree_id,
                cx,
            )])
        });

        // dep.py is active but matches read_only_files → should be skipped
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(item_dep.clone()), None, true, window, cx);
        });
        let path = workspace.read_with(cx, |workspace, cx| workspace.most_recent_active_path(cx));
        assert_eq!(path, None);
    }
}
