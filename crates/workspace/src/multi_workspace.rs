use anyhow::Result;
use fs::Fs;

use gpui::{
    AnyView, App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, ManagedView,
    MouseButton, Pixels, Render, Subscription, Task, TaskExt, Tiling, WeakEntity, Window,
    deferred, px,
};
use project::Project;
pub use project::ProjectGroupKey;
use remote::RemoteConnectionOptions;
pub use settings::SidebarSide;

use std::path::PathBuf;
use ui::prelude::*;
use util::path_list::PathList;

use crate::{
    CloseIntent, CloseWindow, DockPosition, Event as WorkspaceEvent, Item, ModalView, OpenMode,
    Panel, Workspace, WorkspaceId, client_side_decorations,
};

#[derive(Default)]
pub struct SidebarRenderState {
    pub open: bool,
    pub side: SidebarSide,
}

pub enum MultiWorkspaceEvent {
    ActiveWorkspaceChanged {
        source_workspace: Option<WeakEntity<Workspace>>,
    },
    WorkspaceAdded(Entity<Workspace>),
    WorkspaceRemoved(EntityId),
    ProjectGroupsChanged,
}

pub trait Sidebar: Focusable + Render + Sized {
    fn width(&self, cx: &App) -> Pixels;
    fn set_width(&mut self, width: Option<Pixels>, cx: &mut Context<Self>);
    fn has_notifications(&self, cx: &App) -> bool;
    fn side(&self, _cx: &App) -> SidebarSide;

    fn is_threads_list_view_active(&self) -> bool {
        true
    }
    /// Makes focus reset back to the search editor upon toggling the sidebar from outside
    fn prepare_for_focus(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
    /// Opens or cycles the thread switcher popup.
    fn toggle_thread_switcher(
        &mut self,
        _select_last: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// Activates the next or previous project.
    fn cycle_project(&mut self, _forward: bool, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Activates the next or previous thread in sidebar order.
    fn cycle_thread(&mut self, _forward: bool, _window: &mut Window, _cx: &mut Context<Self>) {}
}

pub trait SidebarHandle: 'static + Send + Sync {
    fn width(&self, cx: &App) -> Pixels;
    fn set_width(&self, width: Option<Pixels>, cx: &mut App);
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn focus(&self, window: &mut Window, cx: &mut App);
    fn prepare_for_focus(&self, window: &mut Window, cx: &mut App);
    fn has_notifications(&self, cx: &App) -> bool;
    fn to_any(&self) -> AnyView;
    fn entity_id(&self) -> EntityId;
    fn toggle_thread_switcher(&self, select_last: bool, window: &mut Window, cx: &mut App);
    fn cycle_project(&self, forward: bool, window: &mut Window, cx: &mut App);
    fn cycle_thread(&self, forward: bool, window: &mut Window, cx: &mut App);
    fn is_threads_list_view_active(&self, cx: &App) -> bool;
    fn side(&self, cx: &App) -> SidebarSide;
}

#[derive(Clone)]
pub struct DraggedSidebar;

impl Render for DraggedSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl<T: Sidebar> SidebarHandle for Entity<T> {
    fn width(&self, cx: &App) -> Pixels {
        self.read(cx).width(cx)
    }

    fn set_width(&self, width: Option<Pixels>, cx: &mut App) {
        self.update(cx, |this, cx| this.set_width(width, cx))
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn prepare_for_focus(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |this, cx| this.prepare_for_focus(window, cx));
    }

    fn has_notifications(&self, cx: &App) -> bool {
        self.read(cx).has_notifications(cx)
    }

    fn to_any(&self) -> AnyView {
        self.clone().into()
    }

    fn entity_id(&self) -> EntityId {
        Entity::entity_id(self)
    }

    fn toggle_thread_switcher(&self, select_last: bool, window: &mut Window, cx: &mut App) {
        let entity = self.clone();
        window.defer(cx, move |window, cx| {
            entity.update(cx, |this, cx| {
                this.toggle_thread_switcher(select_last, window, cx);
            });
        });
    }

    fn cycle_project(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let entity = self.clone();
        window.defer(cx, move |window, cx| {
            entity.update(cx, |this, cx| {
                this.cycle_project(forward, window, cx);
            });
        });
    }

    fn cycle_thread(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let entity = self.clone();
        window.defer(cx, move |window, cx| {
            entity.update(cx, |this, cx| {
                this.cycle_thread(forward, window, cx);
            });
        });
    }

    fn is_threads_list_view_active(&self, cx: &App) -> bool {
        self.read(cx).is_threads_list_view_active()
    }

    fn side(&self, cx: &App) -> SidebarSide {
        self.read(cx).side(cx)
    }
}

#[derive(Clone)]
pub struct ProjectGroup {
    pub key: ProjectGroupKey,
    pub workspaces: Vec<Entity<Workspace>>,
    pub expanded: bool,
}

pub struct SerializedProjectGroupState {
    pub key: ProjectGroupKey,
    pub expanded: bool,
}

#[derive(Clone)]
pub struct ProjectGroupState {
    pub key: ProjectGroupKey,
    pub expanded: bool,
    pub last_active_workspace: Option<WeakEntity<Workspace>>,
}

pub struct MultiWorkspace {
    project_groups: Vec<ProjectGroupState>,
    active_workspace: Entity<Workspace>,
    sidebar: Option<Box<dyn SidebarHandle>>,
    sidebar_open: bool,
    sidebar_overlay: Option<AnyView>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MultiWorkspaceEvent> for MultiWorkspace {}

impl MultiWorkspace {
    pub fn sidebar_side(&self, cx: &App) -> SidebarSide {
        self.sidebar
            .as_ref()
            .map_or(SidebarSide::Left, |s| s.side(cx))
    }

    pub fn sidebar_render_state(&self, cx: &App) -> SidebarRenderState {
        SidebarRenderState {
            open: false,
            side: self.sidebar_side(cx),
        }
    }

    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::subscribe_to_workspace(&workspace, window, cx);
        let weak_self = cx.weak_entity();
        workspace.update(cx, |workspace, _cx| {
            workspace.set_multi_workspace(weak_self);
        });
        Self {
            project_groups: Vec::new(),
            active_workspace: workspace,
            sidebar: None,
            sidebar_open: false,
            sidebar_overlay: None,
            _subscriptions: Vec::new(),
        }
    }

    pub fn register_sidebar<T: Sidebar>(&mut self, sidebar: Entity<T>, cx: &mut Context<Self>) {
        self._subscriptions
            .push(cx.observe(&sidebar, |_this, _, cx| {
                cx.notify();
            }));
        self.sidebar = Some(Box::new(sidebar));
    }

    pub fn sidebar(&self) -> Option<&dyn SidebarHandle> {
        self.sidebar.as_deref()
    }

    pub fn set_sidebar_overlay(&mut self, overlay: Option<AnyView>, cx: &mut Context<Self>) {
        self.sidebar_overlay = overlay;
        cx.notify();
    }

    pub fn sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    pub fn sidebar_has_notifications(&self, cx: &App) -> bool {
        self.sidebar
            .as_ref()
            .map_or(false, |s| s.has_notifications(cx))
    }

    pub fn is_threads_list_view_active(&self, cx: &App) -> bool {
        self.sidebar
            .as_ref()
            .map_or(false, |s| s.is_threads_list_view_active(cx))
    }

    pub fn toggle_sidebar(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    pub fn close_sidebar_action(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    pub fn focus_sidebar(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    pub fn close_sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = false;
        cx.notify();
    }

    pub fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let workspaces = this.update(cx, |multi_workspace, _cx| {
                multi_workspace.workspaces().cloned().collect::<Vec<_>>()
            })?;

            for workspace in workspaces {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(CloseIntent::CloseWindow, window, cx)
                    })?
                    .await?;
                if !should_continue {
                    return anyhow::Ok(());
                }
            }

            cx.update(|window, _cx| {
                window.remove_window();
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn subscribe_to_workspace(
        workspace: &Entity<Workspace>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(workspace, window, |this, workspace, event, window, cx| {
            if let WorkspaceEvent::Activate = event {
                this.activate(workspace.clone(), None, window, cx);
            }
        })
        .detach();
    }

    fn register_workspace(
        &mut self,
        workspace: &Entity<Workspace>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        Self::subscribe_to_workspace(workspace, window, cx);
        let weak_self = cx.weak_entity();
        workspace.update(cx, |workspace, _| {
            workspace.set_multi_workspace(weak_self);
        });

        let entity = cx.entity();
        cx.defer({
            let workspace = workspace.clone();
            move |cx| {
                entity.update(cx, |this, cx| {
                    this.sync_sidebar_to_workspace(&workspace, cx);
                })
            }
        });
    }

    pub fn project_group_key_for_workspace(
        &self,
        workspace: &Entity<Workspace>,
        cx: &App,
    ) -> ProjectGroupKey {
        workspace.read(cx).project_group_key(cx)
    }

    pub fn project_group_keys(&self) -> Vec<ProjectGroupKey> {
        self.project_groups
            .iter()
            .map(|group| group.key.clone())
            .collect()
    }

    pub fn last_active_workspace_for_group(
        &self,
        key: &ProjectGroupKey,
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        let group = self.project_groups.iter().find(|g| g.key == *key)?;
        let weak = group.last_active_workspace.as_ref()?;
        let workspace = weak.upgrade()?;
        (workspace.read(cx).project_group_key(cx) == *key).then_some(workspace)
    }

    pub fn group_state_by_key(&self, key: &ProjectGroupKey) -> Option<&ProjectGroupState> {
        self.project_groups.iter().find(|group| group.key == *key)
    }

    pub fn group_state_by_key_mut(
        &mut self,
        key: &ProjectGroupKey,
    ) -> Option<&mut ProjectGroupState> {
        self.project_groups
            .iter_mut()
            .find(|group| group.key == *key)
    }

    pub fn set_all_groups_expanded(&mut self, expanded: bool) {
        for group in &mut self.project_groups {
            group.expanded = expanded;
        }
    }

    pub fn move_project_group_up(&mut self, key: &ProjectGroupKey, cx: &mut Context<Self>) -> bool {
        let Some(index) = self
            .project_groups
            .iter()
            .position(|group| group.key == *key)
        else {
            return false;
        };
        if index == 0 {
            return false;
        }
        self.project_groups.swap(index - 1, index);
        cx.emit(MultiWorkspaceEvent::ProjectGroupsChanged);
        cx.notify();
        true
    }

    pub fn move_project_group_down(
        &mut self,
        key: &ProjectGroupKey,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self
            .project_groups
            .iter()
            .position(|group| group.key == *key)
        else {
            return false;
        };
        if index + 1 >= self.project_groups.len() {
            return false;
        }
        self.project_groups.swap(index, index + 1);
        cx.emit(MultiWorkspaceEvent::ProjectGroupsChanged);
        cx.notify();
        true
    }

    pub fn workspaces_for_project_group(
        &self,
        _key: &ProjectGroupKey,
        _cx: &App,
    ) -> Option<Vec<Entity<Workspace>>> {
        None
    }

    pub fn close_workspace(
        &mut self,
        workspace: &Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        let group_key = workspace.read(cx).project_group_key(cx);
        let excluded_workspace = workspace.clone();

        self.remove(
            [workspace.clone()],
            move |this, window, cx| {
                if let Some(workspace) = this
                    .workspaces_for_project_group(&group_key, cx)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|candidate| candidate != &excluded_workspace)
                {
                    return Task::ready(Ok(workspace));
                }

                let current_group_index = this
                    .project_groups
                    .iter()
                    .position(|group| group.key == group_key);

                if let Some(current_group_index) = current_group_index {
                    for distance in 1..this.project_groups.len() {
                        for neighboring_index in [
                            current_group_index.checked_add(distance),
                            current_group_index.checked_sub(distance),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            let Some(neighboring_group) =
                                this.project_groups.get(neighboring_index)
                            else {
                                continue;
                            };

                            if let Some(workspace) = this
                                .last_active_workspace_for_group(&neighboring_group.key, cx)
                                .or_else(|| {
                                    this.workspaces_for_project_group(&neighboring_group.key, cx)
                                        .unwrap_or_default()
                                        .into_iter()
                                        .find(|candidate| candidate != &excluded_workspace)
                                })
                            {
                                return Task::ready(Ok(workspace));
                            }
                        }
                    }
                }

                let neighboring_group_key = current_group_index.and_then(|index| {
                    this.project_groups
                        .get(index + 1)
                        .or_else(|| {
                            index
                                .checked_sub(1)
                                .and_then(|previous| this.project_groups.get(previous))
                        })
                        .map(|group| group.key.clone())
                });

                if let Some(neighboring_group_key) = neighboring_group_key {
                    return this.find_or_create_local_workspace(
                        neighboring_group_key.path_list().clone(),
                        Some(neighboring_group_key),
                        std::slice::from_ref(&excluded_workspace),
                        None,
                        OpenMode::Activate,
                        window,
                        cx,
                    );
                }

                let app_state = this.workspace().read(cx).app_state().clone();
                let project = Project::local(
                    app_state.client.clone(),
                    app_state.languages.clone(),
                    app_state.fs.clone(),
                    None,
                    project::LocalProjectFlags::default(),
                    cx,
                );
                let new_workspace =
                    cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
                Task::ready(Ok(new_workspace))
            },
            window,
            cx,
        )
    }

    pub fn remove_project_group(
        &mut self,
        group_key: &ProjectGroupKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        let pos = self
            .project_groups
            .iter()
            .position(|group| group.key == *group_key);
        let workspaces = self
            .workspaces_for_project_group(group_key, cx)
            .unwrap_or_default();

        // Compute the neighbor while the group is still in the list.
        let neighbor_key = pos.and_then(|pos| {
            self.project_groups
                .get(pos + 1)
                .or_else(|| pos.checked_sub(1).and_then(|i| self.project_groups.get(i)))
                .map(|group| group.key.clone())
        });

        // Now remove the group.
        self.project_groups.retain(|group| group.key != *group_key);
        cx.emit(MultiWorkspaceEvent::ProjectGroupsChanged);

        let excluded_workspaces = workspaces.clone();
        self.remove(
            workspaces,
            move |this, window, cx| {
                if let Some(neighbor_key) = neighbor_key {
                    return this.find_or_create_local_workspace(
                        neighbor_key.path_list().clone(),
                        Some(neighbor_key.clone()),
                        &excluded_workspaces,
                        None,
                        OpenMode::Activate,
                        window,
                        cx,
                    );
                }

                // No other project groups remain — create an empty workspace.
                let app_state = this.workspace().read(cx).app_state().clone();
                let project = Project::local(
                    app_state.client.clone(),
                    app_state.languages.clone(),
                    app_state.fs.clone(),
                    None,
                    project::LocalProjectFlags::default(),
                    cx,
                );
                let new_workspace =
                    cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
                Task::ready(Ok(new_workspace))
            },
            window,
            cx,
        )
    }

    /// Goes through sqlite: serialize -> close -> open new window
    /// This avoids issues with pending tasks having the wrong window
    pub fn open_project_group_in_new_window(
        &mut self,
        key: &ProjectGroupKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let paths: Vec<PathBuf> = key.path_list().ordered_paths().cloned().collect();
        if paths.is_empty() {
            return Task::ready(Ok(()));
        }

        let app_state = self.workspace().read(cx).app_state().clone();
        let remove_task = self.remove_project_group(key, window, cx);

        cx.spawn(async move |_this, cx| {
            let removed = remove_task.await?;
            if !removed {
                return Ok(());
            }

            cx.update(|cx| {
                Workspace::new_local(paths, app_state, None, None, None, OpenMode::NewWindow, cx)
            })
            .await?;

            Ok(())
        })
    }

    /// Finds an existing workspace whose root paths and host exactly match.
    pub fn workspace_for_paths(
        &self,
        path_list: &PathList,
        host: Option<&RemoteConnectionOptions>,
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        self.workspace_for_paths_excluding(path_list, host, &[], cx)
    }

    fn workspace_for_paths_excluding(
        &self,
        path_list: &PathList,
        host: Option<&RemoteConnectionOptions>,
        excluding: &[Entity<Workspace>],
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        for workspace in self.workspaces() {
            if excluding.contains(workspace) {
                continue;
            }
            let root_paths = PathList::new(&workspace.read(cx).root_paths(cx));
            let key = workspace.read(cx).project_group_key(cx);
            let host_matches = key.host().as_ref() == host;
            let paths_match = root_paths == *path_list;
            if host_matches && paths_match {
                return Some(workspace.clone());
            }
        }

        None
    }

    /// Finds an existing workspace in this multi-workspace whose paths match,
    /// or creates a new one (deserializing its saved state from the database).
    /// Never searches other windows or matches workspaces with a superset of
    /// the requested paths.
    ///
    /// `excluding` lists workspaces that should be skipped during the search
    /// (e.g. workspaces that are about to be removed).
    pub fn find_or_create_local_workspace(
        &mut self,
        path_list: PathList,
        project_group: Option<ProjectGroupKey>,
        excluding: &[Entity<Workspace>],
        init: Option<Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send>>,
        open_mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        self.find_or_create_local_workspace_with_source_workspace(
            path_list,
            project_group,
            excluding,
            init,
            open_mode,
            None,
            window,
            cx,
        )
    }

    pub fn find_or_create_local_workspace_with_source_workspace(
        &mut self,
        path_list: PathList,
        project_group: Option<ProjectGroupKey>,
        excluding: &[Entity<Workspace>],
        init: Option<Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send>>,
        open_mode: OpenMode,
        source_workspace: Option<WeakEntity<Workspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        if let Some(workspace) = self.workspace_for_paths_excluding(&path_list, None, excluding, cx)
        {
            self.activate(workspace.clone(), source_workspace, window, cx);
            return Task::ready(Ok(workspace));
        }

        let paths = path_list.paths().to_vec();
        let app_state = self.workspace().read(cx).app_state().clone();
        let requesting_window = window.window_handle().downcast::<MultiWorkspace>();
        let fs = <dyn Fs>::global(cx);
        let excluding = excluding.to_vec();

        cx.spawn(async move |_this, cx| {
            let effective_path_list = if let Some(project_group) = project_group {
                let metadata_tasks: Vec<_> = paths
                    .iter()
                    .map(|path| fs.metadata(path.as_path()))
                    .collect();
                let metadata_results = futures::future::join_all(metadata_tasks).await;
                // Only fall back when every path is definitely absent; real
                // filesystem errors should not be treated as "missing".
                let all_paths_missing = !paths.is_empty()
                    && metadata_results
                        .into_iter()
                        // Ok(None) means the path is definitely absent
                        .all(|result| matches!(result, Ok(None)));

                if all_paths_missing {
                    project_group.path_list().clone()
                } else {
                    PathList::new(&paths)
                }
            } else {
                PathList::new(&paths)
            };

            if let Some(requesting_window) = requesting_window
                && let Some(workspace) = requesting_window
                    .update(cx, |multi_workspace, window, cx| {
                        multi_workspace
                            .workspace_for_paths_excluding(
                                &effective_path_list,
                                None,
                                &excluding,
                                cx,
                            )
                            .inspect(|workspace| {
                                multi_workspace.activate(
                                    workspace.clone(),
                                    source_workspace.clone(),
                                    window,
                                    cx,
                                );
                            })
                    })
                    .ok()
                    .flatten()
            {
                return Ok(workspace);
            }

            let result = cx
                .update(|cx| {
                    Workspace::new_local(
                        effective_path_list.paths().to_vec(),
                        app_state,
                        requesting_window,
                        None,
                        init,
                        open_mode,
                        cx,
                    )
                })
                .await?;
            Ok(result.workspace)
        })
    }

    pub fn workspace(&self) -> &Entity<Workspace> {
        &self.active_workspace
    }

    pub fn workspaces(&self) -> impl Iterator<Item = &Entity<Workspace>> {
        std::iter::once(&self.active_workspace)
    }

    /// Ensures the workspace is in the multiworkspace and makes it the active one.
    pub fn activate(
        &mut self,
        workspace: Entity<Workspace>,
        source_workspace: Option<WeakEntity<Workspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace() == &workspace {
            self.focus_active_workspace(window, cx);
            return;
        }

        let old_active_workspace = self.active_workspace.clone();

        self.register_workspace(&workspace, window, cx);

        self.active_workspace = workspace;

        let active_key = self.active_workspace.read(cx).project_group_key(cx);
        if let Some(group) = self.project_groups.iter_mut().find(|g| g.key == active_key) {
            group.last_active_workspace = Some(self.active_workspace.downgrade());
        }

        self.detach_workspace(&old_active_workspace, cx);

        cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged { source_workspace });
        self.focus_active_workspace(window, cx);
        cx.notify();
    }

    /// Detaches a workspace: clears session state, DB binding, cached
    /// group key, and emits `WorkspaceRemoved`. The DB row is preserved
    /// so the workspace still appears in the recent-projects list.
    fn detach_workspace(&mut self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        for group in &mut self.project_groups {
            if group
                .last_active_workspace
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .as_ref()
                == Some(workspace)
            {
                group.last_active_workspace = None;
            }
        }
        cx.emit(MultiWorkspaceEvent::WorkspaceRemoved(workspace.entity_id()));
        workspace.update(cx, |workspace, _cx| {
            workspace.session_id.take();
        });
    }

    fn sync_sidebar_to_workspace(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        if self.sidebar_open() {
            let sidebar_focus_handle = self.sidebar.as_ref().map(|s| s.focus_handle(cx));
            workspace.update(cx, |workspace, _| {
                workspace.set_sidebar_focus_handle(sidebar_focus_handle);
            });
        }
    }

    pub fn focus_active_workspace(&self, window: &mut Window, cx: &mut App) {
        // If a dock panel is zoomed, focus it instead of the center pane.
        // Otherwise, focusing the center pane triggers dismiss_zoomed_items_to_reveal
        // which closes the zoomed dock.
        let focus_handle = {
            let workspace = self.workspace().read(cx);
            let mut target = None;
            for dock in workspace.all_docks() {
                let dock = dock.read(cx);
                if dock.is_open() {
                    if let Some(panel) = dock.active_panel() {
                        if panel.is_zoomed(window, cx) {
                            target = Some(panel.panel_focus_handle(cx));
                            break;
                        }
                    }
                }
            }
            target.unwrap_or_else(|| {
                let pane = workspace.active_pane().clone();
                pane.read(cx).focus_handle(cx)
            })
        };
        window.focus(&focus_handle, cx);
    }

    pub fn panel<T: Panel>(&self, cx: &App) -> Option<Entity<T>> {
        self.workspace().read(cx).panel::<T>(cx)
    }

    pub fn active_modal<V: ManagedView + 'static>(&self, cx: &App) -> Option<Entity<V>> {
        self.workspace().read(cx).active_modal::<V>(cx)
    }

    pub fn add_panel<T: Panel>(
        &mut self,
        panel: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.add_panel(panel, window, cx);
        });
    }

    pub fn focus_panel<T: Panel>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<T>> {
        self.workspace()
            .update(cx, |workspace, cx| workspace.focus_panel::<T>(window, cx))
    }

    // used in a test
    pub fn toggle_modal<V: ModalView, B>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: B,
    ) where
        B: FnOnce(&mut Window, &mut gpui::Context<V>) -> V,
    {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, build);
        });
    }

    pub fn toggle_dock(
        &mut self,
        dock_side: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_dock(dock_side, window, cx);
        });
    }

    pub fn active_item_as<I: 'static>(&self, cx: &App) -> Option<Entity<I>> {
        self.workspace().read(cx).active_item_as::<I>(cx)
    }

    pub fn items_of_type<'a, T: Item>(
        &'a self,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = Entity<T>> {
        self.workspace().read(cx).items_of_type::<T>(cx)
    }

    pub fn database_id(&self, cx: &App) -> Option<WorkspaceId> {
        self.workspace().read(cx).database_id()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_expand_all_groups(&mut self) {
        self.set_all_groups_expanded(true);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_random_database_id(&mut self, cx: &mut Context<Self>) {
        self.workspace().update(cx, |workspace, _cx| {
            workspace.set_random_database_id();
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_add_project_group(&mut self, group: ProjectGroup) {
        self.project_groups.push(ProjectGroupState {
            key: group.key,
            expanded: group.expanded,
            last_active_workspace: None,
        });
    }

    /// Removes one or more workspaces from this multi-workspace.
    ///
    /// If the active workspace is among those being removed,
    /// `fallback_workspace` is called **synchronously before the removal
    /// begins** to produce a `Task` that resolves to the workspace that
    /// should become active. The fallback must not be one of the
    /// workspaces being removed.
    ///
    /// Returns `true` if any workspaces were actually removed.
    pub fn remove(
        &mut self,
        workspaces: impl IntoIterator<Item = Entity<Workspace>>,
        fallback_workspace: impl FnOnce(
            &mut Self,
            &mut Window,
            &mut Context<Self>,
        ) -> Task<Result<Entity<Workspace>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        let workspaces: Vec<_> = workspaces.into_iter().collect();

        if workspaces.is_empty() {
            return Task::ready(Ok(false));
        }

        let removing_active = workspaces.iter().any(|ws| ws == self.workspace());
        let original_active = self.workspace().clone();

        let fallback_task = removing_active.then(|| fallback_workspace(self, window, cx));

        cx.spawn_in(window, async move |this, cx| {
            // Run the standard workspace close lifecycle for every workspace
            // being removed from this window. This handles save prompting and
            // session cleanup consistently with other replace-in-window flows.
            for workspace in &workspaces {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(CloseIntent::ReplaceWindow, window, cx)
                    })?
                    .await?;

                if !should_continue {
                    return Ok(false);
                }
            }

            // If we're removing the active workspace, await the
            // fallback and switch to it before tearing anything down.
            // Otherwise restore the original active workspace in case
            // prompting switched away from it.
            if let Some(fallback_task) = fallback_task {
                let new_active = fallback_task.await?;

                this.update_in(cx, |this, window, cx| {
                    assert!(
                        !workspaces.contains(&new_active),
                        "fallback workspace must not be one of the workspaces being removed"
                    );
                    this.activate(new_active, None, window, cx);
                })?;
            } else {
                this.update_in(cx, |this, window, cx| {
                    if *this.workspace() != original_active {
                        this.activate(original_active, None, window, cx);
                    }
                })?;
            }

            // Actually remove the workspaces.
            this.update_in(cx, |_this, _, _cx| {
                Ok(false)
            })?
        })
    }

    pub fn open_project(
        &mut self,
        paths: Vec<PathBuf>,
        open_mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        let workspace = self.workspace().clone();
        cx.spawn_in(window, async move |_this, cx| {
            let should_continue = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.prepare_to_close(crate::CloseIntent::ReplaceWindow, window, cx)
                })?
                .await?;
            if should_continue {
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.open_workspace_for_paths(open_mode, paths, window, cx)
                    })?
                    .await
            } else {
                Ok(workspace)
            }
        })
    }
}

impl Render for MultiWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = theme_settings::setup_ui_font(window, cx);
        let text_color = cx.theme().colors().text;

        let workspace = self.workspace().clone();
        let workspace_key_context = workspace.update(cx, |workspace, cx| workspace.key_context(cx));
        let root = workspace.update(cx, |workspace, cx| workspace.actions(h_flex(), window, cx));

        client_side_decorations(
            root.key_context(workspace_key_context)
                .relative()
                .size_full()
                .font(ui_font)
                .text_color(text_color)
                .on_action(cx.listener(Self::close_window))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .size_full()
                        .overflow_hidden()
                        .child(self.workspace().clone()),
                )
                .child(self.workspace().read(cx).modal_layer.clone())
                .children(self.sidebar_overlay.as_ref().map(|view| {
                    deferred(div().absolute().size_full().inset_0().occlude().child(
                        v_flex().h(px(0.0)).top_20().items_center().child(
                            h_flex().occlude().child(view.clone()).on_mouse_down(
                                MouseButton::Left,
                                |_, _, cx| {
                                    cx.stop_propagation();
                                },
                            ),
                        ),
                    ))
                    .with_priority(2)
                })),
            window,
            cx,
            Tiling::default(),
        )
    }
}
