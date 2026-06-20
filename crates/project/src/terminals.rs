use anyhow::Result;
use collections::HashMap;
use gpui::{App, AppContext as _, Context, Entity, Task, WeakEntity};

use async_channel::bounded;
use futures::{FutureExt, future::Shared};
use itertools::Itertools as _;
use language::LanguageName;
use remote::RemoteClient;
use settings::{Settings, SettingsLocation};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};
use task::{Shell, ShellBuilder, ShellKind, SpawnInTerminal};
use terminal::{
    TaskState, TaskStatus, Terminal, TerminalBuilder, insert_zed_terminal_env,
};
use util::{
    command::new_std_command, get_default_system_shell, get_system_shell, maybe, rel_path::RelPath,
};

use crate::{Project, ProjectPath};

pub struct Terminals {
    pub(crate) local_handles: Vec<WeakEntity<terminal::Terminal>>,
}

impl Project {
    pub fn active_entry_directory(&self, cx: &App) -> Option<PathBuf> {
        let entry_id = self.active_entry()?;
        let worktree = self.worktree_for_entry(entry_id, cx)?;
        let worktree = worktree.read(cx);
        let entry = worktree.entry_for_id(entry_id)?;

        let absolute_path = worktree.absolutize(entry.path.as_ref());
        if entry.is_dir() {
            Some(absolute_path)
        } else {
            absolute_path.parent().map(|p| p.to_path_buf())
        }
    }

    pub fn active_project_directory(&self, cx: &App) -> Option<Arc<Path>> {
        self.active_entry()
            .and_then(|entry_id| self.worktree_for_entry(entry_id, cx))
            .into_iter()
            .chain(self.worktrees(cx))
            .find_map(|tree| tree.read(cx).root_dir())
    }

    pub fn first_project_directory(&self, cx: &App) -> Option<PathBuf> {
        let worktree = self.worktrees(cx).next()?;
        let worktree = worktree.read(cx);
        if worktree.root_entry()?.is_dir() {
            Some(worktree.abs_path().to_path_buf())
        } else {
            None
        }
    }

    pub fn create_terminal_task(
        &mut self,
        spawn_task: SpawnInTerminal,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Terminal>>> {
        todo!("create_terminal_task");
    }

    pub fn create_terminal_shell(
        &mut self,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Terminal>>> {
        self.create_terminal_shell_internal(cwd, false, cx)
    }

    /// Creates a local terminal even if the project is remote.
    /// In remote projects: opens in Zed's launch directory (bypasses SSH).
    /// In local projects: opens in the project directory (same as regular terminals).
    pub fn create_local_terminal(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Terminal>>> {
        let working_directory = if self.remote_client.is_some() {
            // Remote project: don't use remote paths, let shell use Zed's cwd
            None
        } else {
            // Local project: use project directory like normal terminals
            self.active_project_directory(cx).map(|p| p.to_path_buf())
        };
        self.create_terminal_shell_internal(working_directory, true, cx)
    }

    /// Internal method for creating terminal shells.
    /// If force_local is true, creates a local terminal even if the project has a remote client.
    /// This allows "breaking out" to a local shell in remote projects.
    fn create_terminal_shell_internal(
        &mut self,
        cwd: Option<PathBuf>,
        force_local: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Terminal>>> {
        todo!("create_terminal_shell_internal");
    }

    pub fn clone_terminal(
        &mut self,
        terminal: &Entity<Terminal>,
        cx: &mut Context<'_, Project>,
        cwd: Option<PathBuf>,
    ) -> Task<Result<Entity<Terminal>>> {
        // We cannot clone the task's terminal, as it will effectively re-spawn the task, which might not be desirable.
        // For now, create a new shell instead.
        if terminal.read(cx).task().is_some() {
            return self.create_terminal_shell(cwd, cx);
        }
        let local_path = if self.is_via_remote_server() {
            None
        } else {
            cwd
        };

        let builder = terminal.read(cx).clone_builder(cx, local_path);
        cx.spawn(async |project, cx| {
            let terminal = builder.await?;
            project.update(cx, |project, cx| {
                let terminal_handle = cx.new(|cx| terminal.subscribe(cx));

                project
                    .terminals
                    .local_handles
                    .push(terminal_handle.downgrade());

                let id = terminal_handle.entity_id();
                cx.observe_release(&terminal_handle, move |project, _terminal, cx| {
                    let handles = &mut project.terminals.local_handles;

                    if let Some(index) = handles
                        .iter()
                        .position(|terminal| terminal.entity_id() == id)
                    {
                        handles.remove(index);
                        cx.notify();
                    }
                })
                .detach();

                terminal_handle
            })
        })
    }

    pub fn exec_in_shell(
        &self,
        command: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<smol::process::Command>> {
        let path = self.first_project_directory(cx);
        let remote_client = self.remote_client.clone();
        let shell = remote_client
            .as_ref()
            .and_then(|remote_client| remote_client.read(cx).shell())
            .map(Shell::Program)
            .unwrap_or(Shell::System);
        let is_windows = self.path_style(cx).is_windows();
        let builder = ShellBuilder::new(&shell, is_windows).non_interactive();
        let (command, args) = builder.build(Some(command), &Vec::new());

        let env_task = self.resolve_directory_environment(
            &shell.program(),
            path.as_ref().map(|p| Arc::from(&**p)),
            remote_client.clone(),
            cx,
        );

        cx.spawn(async move |project, cx| {
            let mut env = env_task.await.unwrap_or_default();

            project.update(cx, move |_, cx| {
                match remote_client {
                    Some(remote_client) => {
                        let command_template = remote_client.read(cx).build_command(
                            Some(command),
                            &args,
                            &env,
                            None,
                            None,
                        )?;
                        let mut command = new_std_command(command_template.program);
                        command.args(command_template.args);
                        command.envs(command_template.env);
                        Ok(command)
                    }
                    None => {
                        let mut command = new_std_command(command);
                        command.args(args);
                        command.envs(env);
                        if let Some(path) = path {
                            command.current_dir(path);
                        }
                        Ok(command)
                    }
                }
                .map(|mut process| {
                    util::set_pre_exec_to_start_new_session(&mut process);
                    smol::process::Command::from(process)
                })
            })?
        })
    }

    pub fn local_terminal_handles(&self) -> &Vec<WeakEntity<terminal::Terminal>> {
        &self.terminals.local_handles
    }

    fn resolve_directory_environment(
        &self,
        shell: &str,
        path: Option<Arc<Path>>,
        remote_client: Option<Entity<RemoteClient>>,
        cx: &mut App,
    ) -> Shared<Task<Option<HashMap<String, String>>>> {
        if let Some(path) = &path {
            let shell = Shell::Program(shell.to_string());
            self.environment
                .update(cx, |project_env, cx| match &remote_client {
                    Some(remote_client) => project_env.remote_directory_environment(
                        &shell,
                        path.clone(),
                        remote_client.clone(),
                        cx,
                    ),
                    None => project_env.local_directory_environment(&shell, path.clone(), cx),
                })
        } else {
            Task::ready(None).shared()
        }
    }
}
