use crate::Editor;

use collections::HashMap;
use gpui::{App, Task, Window};
use lsp::LanguageServerName;
use project::{Location, project_settings::ProjectSettings};
use settings::Settings as _;
use task::{TaskContext, TaskVariables, VariableName};
use text::{BufferId, ToOffset, ToPoint};

impl Editor {
    pub fn task_context(&self, window: &mut Window, cx: &mut App) -> Task<Option<TaskContext>> {
        let Some(project) = self.project.clone() else {
            return Task::ready(None);
        };
        let display_snapshot = self.display_snapshot(cx);
        let selection = self.selections.newest_adjusted(&display_snapshot);
        let start = display_snapshot
            .buffer_snapshot()
            .anchor_after(selection.start);
        let end = display_snapshot
            .buffer_snapshot()
            .anchor_after(selection.end);
        let Some((buffer_snapshot, range)) = display_snapshot
            .buffer_snapshot()
            .anchor_range_to_buffer_anchor_range(start..end)
        else {
            return Task::ready(None);
        };
        let Some(buffer) = self.buffer.read(cx).buffer(buffer_snapshot.remote_id()) else {
            return Task::ready(None);
        };
        let location = Location { buffer, range };
        let captured_variables = {
            let mut variables = TaskVariables::default();
            let buffer = location.buffer.read(cx);
            let buffer_id = buffer.remote_id();
            let snapshot = buffer.snapshot();
            let starting_point = location.range.start.to_point(&snapshot);
            let starting_offset = starting_point.to_offset(&snapshot);
            for (_, tasks) in self
                .tasks
                .range((buffer_id, 0)..(buffer_id, starting_point.row + 1))
            {
                if !tasks
                    .context_range
                    .contains(&crate::BufferOffset(starting_offset))
                {
                    continue;
                }
                for (capture_name, value) in tasks.extra_variables.iter() {
                    variables.insert(
                        VariableName::Custom(capture_name.to_owned().into()),
                        value.clone(),
                    );
                }
            }
            variables
        };

        project.update(cx, |project, cx| {
            project.task_store().update(cx, |task_store, cx| {
                task_store.task_context_for_location(captured_variables, location, cx)
            })
        })
    }
}
