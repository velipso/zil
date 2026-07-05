use anyhow::Result;

use gpui::{
    App, Context, Entity, EntityId, EventEmitter, Focusable, ManagedView,
    Render, Task, TaskExt, Tiling, Window,
};

use std::path::PathBuf;
use ui::prelude::*;
use util::path_list::PathList;

use crate::{
    CloseIntent, CloseWindow, DockPosition, Event as WorkspaceEvent, Item, ModalView, OpenMode,
    Panel, Workspace, WorkspaceId, client_side_decorations,
};

pub enum MultiWorkspaceEvent {
    WorkspaceAdded(Entity<Workspace>),
    WorkspaceRemoved(EntityId),
    ProjectGroupsChanged,
}

pub struct MultiWorkspace {
    active_workspace: Entity<Workspace>,
}

impl EventEmitter<MultiWorkspaceEvent> for MultiWorkspace {}

impl MultiWorkspace {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::subscribe_to_workspace(&workspace, window, cx);
        let weak_self = cx.weak_entity();
        workspace.update(cx, |workspace, _cx| {
            workspace.set_multi_workspace(weak_self);
        });
        Self {
            active_workspace: workspace,
        }
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
                this.activate(workspace.clone(), window, cx);
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
    }

    /// Finds an existing workspace whose root paths and host exactly match.
    pub fn workspace_for_paths(
        &self,
        path_list: &PathList,
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        self.workspace_for_paths_excluding(path_list, &[], cx)
    }

    fn workspace_for_paths_excluding(
        &self,
        path_list: &PathList,
        excluding: &[Entity<Workspace>],
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        for workspace in self.workspaces() {
            if excluding.contains(workspace) {
                continue;
            }
            let root_paths = PathList::new(&workspace.read(cx).root_paths(cx));
            let paths_match = root_paths == *path_list;
            if paths_match {
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
        excluding: &[Entity<Workspace>],
        init: Option<Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send>>,
        open_mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        self.find_or_create_local_workspace_with_source_workspace(
            path_list,
            excluding,
            init,
            open_mode,
            window,
            cx,
        )
    }

    pub fn find_or_create_local_workspace_with_source_workspace(
        &mut self,
        path_list: PathList,
        excluding: &[Entity<Workspace>],
        init: Option<Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send>>,
        open_mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        if let Some(workspace) = self.workspace_for_paths_excluding(&path_list, excluding, cx)
        {
            self.activate(workspace.clone(), window, cx);
            return Task::ready(Ok(workspace));
        }

        let paths = path_list.paths().to_vec();
        let app_state = self.workspace().read(cx).app_state().clone();
        let requesting_window = window.window_handle().downcast::<MultiWorkspace>();
        let excluding = excluding.to_vec();

        cx.spawn(async move |_this, cx| {
            let effective_path_list = PathList::new(&paths);

            if let Some(requesting_window) = requesting_window
                && let Some(workspace) = requesting_window
                    .update(cx, |multi_workspace, window, cx| {
                        multi_workspace
                            .workspace_for_paths_excluding(
                                &effective_path_list,
                                &excluding,
                                cx,
                            )
                            .inspect(|workspace| {
                                multi_workspace.activate(
                                    workspace.clone(),
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
        self.detach_workspace(&old_active_workspace, cx);
        self.focus_active_workspace(window, cx);
        cx.notify();
    }

    /// Detaches a workspace: clears session state, DB binding, cached
    /// group key, and emits `WorkspaceRemoved`. The DB row is preserved
    /// so the workspace still appears in the recent-projects list.
    fn detach_workspace(&mut self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        cx.emit(MultiWorkspaceEvent::WorkspaceRemoved(workspace.entity_id()));
        workspace.update(cx, |workspace, _cx| {
            workspace.session_id.take();
        });
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
    pub fn set_random_database_id(&mut self, cx: &mut Context<Self>) {
        self.workspace().update(cx, |workspace, _cx| {
            workspace.set_random_database_id();
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
                    this.activate(new_active, window, cx);
                })?;
            } else {
                this.update_in(cx, |this, window, cx| {
                    if *this.workspace() != original_active {
                        this.activate(original_active, window, cx);
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
                .child(self.workspace().read(cx).modal_layer.clone()),
            window,
            cx,
            Tiling::default(),
        )
    }
}
