use editor::{Editor, EditorSettings};
use gpui::{
    Anchor, Context, Entity, EventEmitter,
    Focusable, InteractiveElement, ParentElement, Render, Styled,
    Window,
};
use search::{BufferSearchBar, buffer_search};
use settings::{Settings, SettingsStore};
use ui::{
    ButtonStyle, ContextMenu, IconButton, IconName, IconSize,
    PopoverMenu, PopoverMenuHandle, prelude::*,
};
use workspace::{
    ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, item::ItemHandle,
};
use rope::Point;

pub struct QuickActionBar {
    active_item: Option<Box<dyn ItemHandle>>,
    buffer_search_bar: Entity<BufferSearchBar>,
    show: bool,
    toggle_settings_handle: PopoverMenuHandle<ContextMenu>,
}

impl QuickActionBar {
    pub fn new(
        buffer_search_bar: Entity<BufferSearchBar>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            active_item: None,
            buffer_search_bar,
            show: true,
            toggle_settings_handle: Default::default(),
        };
        this.apply_settings(cx);
        cx.observe_global::<SettingsStore>(|this, cx| this.apply_settings(cx))
            .detach();
        this
    }

    fn active_editor(&self) -> Option<Entity<Editor>> {
        self.active_item
            .as_ref()
            .and_then(|item| item.downcast::<Editor>())
    }

    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let new_show = EditorSettings::get_global(cx).toolbar.quick_actions;
        if new_show != self.show {
            self.show = new_show;
            cx.emit(ToolbarItemEvent::ChangeLocation(
                self.get_toolbar_item_location(),
            ));
        }
    }

    fn get_toolbar_item_location(&self) -> ToolbarItemLocation {
        if self.show && self.active_editor().is_some() {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl Render for QuickActionBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = self.active_editor() else {
            return div().id("empty quick action bar");
        };

        let row_column = editor.update(cx, |editor, cx| {
            if editor.selections.count() != 0 {
                let map = editor.display_snapshot(cx);
                let newest = editor.selections.newest::<Point>(&map);
                let point = newest.head();
                format!("{}:{}", point.row + 1, point.column + 1)
            } else {
                format!("?:?")
            }
        });

        let editor_value = editor.read(cx);
        let minimap_enabled = editor_value.minimap().is_some();
        let stacked_tabs =
            editor.read_with(cx, |editor, cx| {
                match editor.workspace() {
                    Some(ws) => ws.read_with(cx, |ws, cx| {
                        ws.show_tab_bar_stacked(cx)
                    }),
                    None => false
                }
            });
        let show_line_numbers = editor_value.line_numbers_enabled(cx);

        let tab_size = editor_value.tab_size(cx);
        let hard_tabs = editor_value.hard_tabs(cx);

        let language =
            editor.read_with(cx, |editor, cx| {
                if let Some(buffer) = editor.active_buffer(cx)
                    && let Some(language) = buffer.read(cx).language()
                {
                    language.name().0.to_string()
                } else {
                    "Unknown".to_string()
                }
            });
        let encoding =
            editor.read_with(cx, |editor, cx| {
                if let Some(buffer) = editor.active_buffer(cx) {
                    buffer.read(cx).encoding().name()
                } else {
                    "Unknown"
                }
            });
        let line_ending =
            editor.read_with(cx, |editor, cx| {
                if let Some(buffer) = editor.active_buffer(cx) {
                    let line_ending = buffer.read(cx).line_ending();
                    line_ending.label()
                } else {
                    "Unknown"
                }
            });

        let buffer_search_bar = self.buffer_search_bar.clone();
        let buffer_search_visible = !buffer_search_bar.read(cx).is_dismissed();

        let editor_focus_handle = editor.focus_handle(cx);
        let editor_settings_dropdown = {
            PopoverMenu::new("editor-settings")
                .trigger(
                    IconButton::new("toggle_editor_settings_icon", IconName::ListTodo)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .toggle_state(self.toggle_settings_handle.is_deployed())
                )
                .anchor(Anchor::TopRight)
                .with_handle(self.toggle_settings_handle.clone())
                .menu(move |window, cx| {
                    Some(ContextMenu::build(window, cx, {
                        let focus_handle = editor_focus_handle.clone();
                        |menu, _, _| {
                            let language = language.clone();
                            menu.context(focus_handle)
                                .toggleable_entry(
                                    "Minimap",
                                    minimap_enabled,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::ToggleMinimap)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::ToggleMinimap,
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                )
                                .toggleable_entry(
                                    "Stacked Tabs",
                                    stacked_tabs,
                                    IconPosition::Start,
                                    Some(Box::new(workspace::ToggleStackedTabs)),
                                    |window, cx| {
                                        window.dispatch_action(
                                            Box::new(workspace::ToggleStackedTabs),
                                            cx
                                        );
                                    }
                                )
                                .toggleable_entry(
                                    "Line Numbers",
                                    show_line_numbers,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::ToggleLineNumbers)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::ToggleLineNumbers,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )
                                .separator()
                                .toggleable_entry(
                                    "Search",
                                    buffer_search_visible,
                                    IconPosition::Start,
                                    Some(Box::new(search::buffer_search::Deploy::find())),
                                    {
                                        let buffer_search_bar = buffer_search_bar.clone();
                                        move |window, cx| {
                                            buffer_search_bar.update(cx, |search_bar, cx| {
                                                search_bar.toggle(&buffer_search::Deploy::find(), window, cx)
                                            });
                                        }
                                    }
                                )
                                .toggleable_entry(
                                    "Go To Line",
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::ToggleGoToLine)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::ToggleGoToLine,
                                                window,
                                                cx
                                            );
                                        }
                                    }
                                )
                                .separator()
                                .toggleable_entry(
                                    language,
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(language_selector::Toggle)),
                                    |window, cx| {
                                        window.dispatch_action(Box::new(language_selector::Toggle), cx);
                                    }
                                )
                                .toggleable_entry(
                                    encoding,
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(encoding_selector::Toggle)),
                                    |window, cx| {
                                        window.dispatch_action(Box::new(encoding_selector::Toggle), cx);
                                    }
                                )
                                .toggleable_entry(
                                    line_ending,
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(line_ending_selector::Toggle)),
                                    |window, cx| {
                                        window.dispatch_action(Box::new(line_ending_selector::Toggle), cx);
                                    }
                                )
                                .separator()
                                .toggleable_entry(
                                    "Tabs",
                                    hard_tabs,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::UseTabs)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::UseTabs,
                                                window,
                                                cx
                                            );
                                        }
                                    }
                                )
                                .toggleable_entry(
                                    "Spaces",
                                    !hard_tabs,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::UseSpaces)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::UseSpaces,
                                                window,
                                                cx
                                            );
                                        }
                                    }
                                )
                                .separator()
                                .toggleable_entry(
                                    "Tab Width: 2",
                                    tab_size == 2,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::TabWidth2)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::TabWidth2,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )
                                .toggleable_entry(
                                    "Tab Width: 3",
                                    tab_size == 3,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::TabWidth2)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::TabWidth3,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )
                                .toggleable_entry(
                                    "Tab Width: 4",
                                    tab_size == 4,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::TabWidth2)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::TabWidth4,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )
                                .toggleable_entry(
                                    "Tab Width: 8",
                                    tab_size == 8,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::TabWidth2)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::TabWidth8,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )
                                .separator()
                                .toggleable_entry(
                                    "Convert Spaces to Tabs",
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::ConvertIndentationToTabs)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::ConvertIndentationToTabs,
                                                window,
                                                cx
                                            );
                                        }
                                    }
                                )
                                .toggleable_entry(
                                    "Convert Tabs to Spaces",
                                    false,
                                    IconPosition::Start,
                                    Some(Box::new(editor::actions::ConvertIndentationToSpaces)),
                                    {
                                        let editor_focus_handle = editor_focus_handle.clone();
                                        move |window, cx| {
                                            editor_focus_handle.dispatch_action(
                                                &editor::actions::ConvertIndentationToSpaces,
                                                window,
                                                cx
                                            );
                                        }
                                    }
                                )
                        }
                    }))
                })
        };

        h_flex()
            .id("quick action bar")
            .gap(DynamicSpacing::Base01.rems(cx))
            .child(ui::Button::new("go-to-line", row_column).size(ButtonSize::Compact).on_click({
                let editor_focus_handle = editor.focus_handle(cx);
                move |_ev, window, cx| {
                    editor_focus_handle.dispatch_action(
                        &editor::actions::ToggleGoToLine,
                        window,
                        cx
                    );
                }
            }))
            .gap_1()
            .child(editor_settings_dropdown)
    }
}

impl EventEmitter<ToolbarItemEvent> for QuickActionBar {}

impl ToolbarItemView for QuickActionBar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active_item = active_pane_item.map(ItemHandle::boxed_clone);
        self.get_toolbar_item_location()
    }
}
