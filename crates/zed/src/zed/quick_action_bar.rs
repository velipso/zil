use editor::actions::{ToggleDiagnostics, ToggleInlineDiagnostics};
use editor::{Editor, EditorSettings};
use gpui::{
    Action, Anchor, ClickEvent, Context, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, ParentElement, Render, Styled, Subscription,
    Window,
};
use project::project_settings::DiagnosticSeverity;
use search::{BufferSearchBar, buffer_search};
use settings::{Settings, SettingsStore};
use ui::{
    ButtonStyle, ContextMenu, ContextMenuEntry, DocumentationSide, IconButton, IconName, IconSize,
    PopoverMenu, PopoverMenuHandle, Tooltip, prelude::*,
};
use workspace::item::ItemBufferKind;
use workspace::{
    ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, item::ItemHandle,
};

pub struct QuickActionBar {
    _inlay_hints_enabled_subscription: Option<Subscription>,
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
            _inlay_hints_enabled_subscription: None,
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

        let supports_inlay_hints = editor.update(cx, |editor, cx| editor.supports_inlay_hints(cx));
        let supports_semantic_tokens =
            editor.update(cx, |editor, cx| editor.supports_semantic_tokens(cx));
        let editor_value = editor.read(cx);
        let selection_menu_enabled = editor_value.selection_menu_enabled(cx);
        let inlay_hints_enabled = editor_value.inlay_hints_enabled();
        let inline_values_enabled = editor_value.inline_values_enabled();
        let semantic_highlights_enabled = editor_value.semantic_highlights_enabled();
        let is_full = editor_value.mode().is_full();
        let diagnostics_enabled = editor_value.diagnostics_enabled()
            && editor_value.diagnostics_max_severity != DiagnosticSeverity::Off;
        let supports_inline_diagnostics = editor_value.inline_diagnostics_enabled();
        let inline_diagnostics_enabled = editor_value.show_inline_diagnostics();
        let auto_signature_help_enabled = editor_value.auto_signature_help_enabled(cx);
        let show_line_numbers = editor_value.line_numbers_enabled(cx);
        let supports_minimap = editor_value.supports_minimap(cx);
        let minimap_enabled = supports_minimap && editor_value.minimap().is_some();
        let focus_handle = editor_value.focus_handle(cx);

        let search_button = (editor.buffer_kind(cx) == ItemBufferKind::Singleton).then(|| {
            QuickActionBarButton::new(
                "toggle buffer search",
                search::SEARCH_ICON,
                !self.buffer_search_bar.read(cx).is_dismissed(),
                Box::new(buffer_search::Deploy::find()),
                focus_handle.clone(),
                "Buffer Search",
                {
                    let buffer_search_bar = self.buffer_search_bar.clone();
                    move |_, window, cx| {
                        buffer_search_bar.update(cx, |search_bar, cx| {
                            search_bar.toggle(&buffer_search::Deploy::find(), window, cx)
                        });
                    }
                },
            )
        });

        let editor_focus_handle = editor.focus_handle(cx);
        let editor = editor.downgrade();
        let editor_settings_dropdown = {
            PopoverMenu::new("editor-settings")
                .trigger_with_tooltip(
                    IconButton::new("toggle_editor_settings_icon", IconName::Sliders)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .toggle_state(self.toggle_settings_handle.is_deployed()),
                    Tooltip::text("Editor Controls"),
                )
                .anchor(Anchor::TopRight)
                .with_handle(self.toggle_settings_handle.clone())
                .menu(move |window, cx| {
                    let menu = ContextMenu::build(window, cx, {
                        let focus_handle = editor_focus_handle.clone();
                        |mut menu, _, _| {
                            menu = menu.context(focus_handle);

                            if supports_inlay_hints {
                                menu = menu.toggleable_entry(
                                    "Inlay Hints",
                                    inlay_hints_enabled,
                                    IconPosition::Start,
                                    Some(editor::actions::ToggleInlayHints.boxed_clone()),
                                    {
                                        let editor = editor.clone();
                                        move |window, cx| {
                                            editor
                                                .update(cx, |editor, cx| {
                                                    editor.toggle_inlay_hints(
                                                        &editor::actions::ToggleInlayHints,
                                                        window,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                        }
                                    },
                                );

                                menu = menu.toggleable_entry(
                                    "Inline Values",
                                    inline_values_enabled,
                                    IconPosition::Start,
                                    Some(editor::actions::ToggleInlineValues.boxed_clone()),
                                    {
                                        let editor = editor.clone();
                                        move |window, cx| {
                                            editor
                                                .update(cx, |editor, cx| {
                                                    editor.toggle_inline_values(
                                                        &editor::actions::ToggleInlineValues,
                                                        window,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                        }
                                    }
                                );
                            }

                            if supports_semantic_tokens {
                                menu = menu.toggleable_entry(
                                    "Semantic Highlights",
                                    semantic_highlights_enabled,
                                    IconPosition::Start,
                                    Some(editor::actions::ToggleSemanticHighlights.boxed_clone()),
                                    {
                                        let editor = editor.clone();
                                        move |window, cx| {
                                            editor
                                                .update(cx, |editor, cx| {
                                                    editor.toggle_semantic_highlights(
                                                        &editor::actions::ToggleSemanticHighlights,
                                                        window,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                        }
                                    },
                                );
                            }

                            if supports_minimap {
                                menu = menu.toggleable_entry("Minimap", minimap_enabled, IconPosition::Start, Some(editor::actions::ToggleMinimap.boxed_clone()), {
                                    let editor = editor.clone();
                                    move |window, cx| {
                                        editor
                                            .update(cx, |editor, cx| {
                                                editor.toggle_minimap(
                                                    &editor::actions::ToggleMinimap,
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    }
                                },)
                            }

                            menu = menu.separator();

                            if is_full {
                                menu = menu.toggleable_entry(
                                    "Diagnostics",
                                    diagnostics_enabled,
                                    IconPosition::Start,
                                    Some(ToggleDiagnostics.boxed_clone()),
                                    {
                                        let editor = editor.clone();
                                        move |window, cx| {
                                            editor
                                                .update(cx, |editor, cx| {
                                                    editor.toggle_diagnostics(
                                                        &ToggleDiagnostics,
                                                        window,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                        }
                                    },
                                );

                                if supports_inline_diagnostics {
                                    let mut inline_diagnostics_item = ContextMenuEntry::new("Inline Diagnostics")
                                        .toggleable(IconPosition::Start, diagnostics_enabled && inline_diagnostics_enabled)
                                        .action(ToggleInlineDiagnostics.boxed_clone())
                                        .handler({
                                            let editor = editor.clone();
                                            move |window, cx| {
                                                editor
                                                    .update(cx, |editor, cx| {
                                                        editor.toggle_inline_diagnostics(
                                                            &ToggleInlineDiagnostics,
                                                            window,
                                                            cx,
                                                        );
                                                    })
                                                    .ok();
                                            }
                                        });
                                    if !diagnostics_enabled {
                                        inline_diagnostics_item = inline_diagnostics_item.disabled(true).documentation_aside(DocumentationSide::Left, |_|  Label::new("Inline diagnostics are not available until regular diagnostics are enabled.").into_any_element());
                                    }
                                    menu = menu.item(inline_diagnostics_item)
                                }

                                menu = menu.separator();
                            }

                            menu = menu.toggleable_entry(
                                "Line Numbers",
                                show_line_numbers,
                                IconPosition::Start,
                                Some(editor::actions::ToggleLineNumbers.boxed_clone()),
                                {
                                    let editor = editor.clone();
                                    move |window, cx| {
                                        editor
                                            .update(cx, |editor, cx| {
                                                editor.toggle_line_numbers(
                                                    &editor::actions::ToggleLineNumbers,
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    }
                                },
                            );

                            menu = menu.toggleable_entry(
                                "Selection Menu",
                                selection_menu_enabled,
                                IconPosition::Start,
                                Some(editor::actions::ToggleSelectionMenu.boxed_clone()),
                                {
                                    let editor = editor.clone();
                                    move |window, cx| {
                                        editor
                                            .update(cx, |editor, cx| {
                                                editor.toggle_selection_menu(
                                                    &editor::actions::ToggleSelectionMenu,
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .ok();
                                    }
                                },
                            );

                            menu = menu.toggleable_entry(
                                "Auto Signature Help",
                                auto_signature_help_enabled,
                                IconPosition::Start,
                                Some(editor::actions::ToggleAutoSignatureHelp.boxed_clone()),
                                {
                                    let editor = editor.clone();
                                    move |window, cx| {
                                        editor
                                            .update(cx, |editor, cx| {
                                                editor.toggle_auto_signature_help_menu(
                                                    &editor::actions::ToggleAutoSignatureHelp,
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    }
                                },
                            );

                            menu
                        }
                    });
                    Some(menu)
                })
        };

        h_flex()
            .id("quick action bar")
            .gap(DynamicSpacing::Base01.rems(cx))
            .children(search_button)
            .child(editor_settings_dropdown)
    }
}

impl EventEmitter<ToolbarItemEvent> for QuickActionBar {}

#[derive(IntoElement)]
struct QuickActionBarButton {
    id: ElementId,
    icon: IconName,
    toggled: bool,
    action: Box<dyn Action>,
    focus_handle: FocusHandle,
    tooltip: SharedString,
    on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

impl QuickActionBarButton {
    fn new(
        id: impl Into<ElementId>,
        icon: IconName,
        toggled: bool,
        action: Box<dyn Action>,
        focus_handle: FocusHandle,
        tooltip: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            toggled,
            action,
            focus_handle,
            tooltip: tooltip.into(),
            on_click: Box::new(on_click),
        }
    }
}

impl RenderOnce for QuickActionBarButton {
    fn render(self, _window: &mut Window, _: &mut App) -> impl IntoElement {
        let tooltip = self.tooltip.clone();
        let action = self.action.boxed_clone();

        IconButton::new(self.id.clone(), self.icon)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .toggle_state(self.toggled)
            .tooltip(move |_window, cx| {
                Tooltip::for_action_in(tooltip.clone(), &*action, &self.focus_handle, cx)
            })
            .on_click(move |event, window, cx| (self.on_click)(event, window, cx))
    }
}

impl ToolbarItemView for QuickActionBar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active_item = active_pane_item.map(ItemHandle::boxed_clone);
        if let Some(active_item) = active_pane_item {
            self._inlay_hints_enabled_subscription.take();

            if let Some(editor) = active_item.downcast::<Editor>() {
                let (
                    mut inlay_hints_enabled,
                    mut supports_inlay_hints,
                    mut supports_semantic_tokens,
                ) = editor.update(cx, |editor, cx| {
                    (
                        editor.inlay_hints_enabled(),
                        editor.supports_inlay_hints(cx),
                        editor.supports_semantic_tokens(cx),
                    )
                });
                self._inlay_hints_enabled_subscription =
                    Some(cx.observe(&editor, move |_, editor, cx| {
                        let (
                            new_inlay_hints_enabled,
                            new_supports_inlay_hints,
                            new_supports_semantic_tokens,
                        ) = editor.update(cx, |editor, cx| {
                            (
                                editor.inlay_hints_enabled(),
                                editor.supports_inlay_hints(cx),
                                editor.supports_semantic_tokens(cx),
                            )
                        });
                        let should_notify = inlay_hints_enabled != new_inlay_hints_enabled
                            || supports_inlay_hints != new_supports_inlay_hints
                            || supports_semantic_tokens != new_supports_semantic_tokens;
                        inlay_hints_enabled = new_inlay_hints_enabled;
                        supports_inlay_hints = new_supports_inlay_hints;
                        supports_semantic_tokens = new_supports_semantic_tokens;
                        if should_notify {
                            cx.notify()
                        }
                    }));
            }
        }
        self.get_toolbar_item_location()
    }
}
