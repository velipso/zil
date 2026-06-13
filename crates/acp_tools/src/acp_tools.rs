use std::{collections::HashSet, sync::Arc};

use agent_client_protocol::schema as acp;
use agent_servers::AcpDebugMessageDirection;
use collections::HashMap;
use gpui::{
    App, Empty, Entity, EventEmitter, FocusHandle, Focusable, ListState, SharedString,
    StyleRefinement, Subscription, Task, TextStyleRefinement, Window, actions, list, prelude::*,
};
use language::LanguageRegistry;
use markdown::{CodeBlockRenderer, CopyButtonVisibility, Markdown, MarkdownElement, MarkdownStyle};
use project::{AgentId, Project};
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{
    ContextMenu, CopyButton, DropdownMenu, DropdownStyle, IconPosition, Tooltip, WithScrollbar,
    prelude::*,
};
use util::ResultExt as _;
use workspace::{Item, ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView};

actions!(dev, [OpenAcpLogs]);

pub fn init(_cx: &mut App) {}

struct AcpTools {
    project: Entity<Project>,
    focus_handle: FocusHandle,
    expanded: HashSet<usize>,
    watched_connections: HashMap<AgentId, WatchedConnection>,
    selected_connection: Option<AgentId>,
    _workspace_subscription: Option<Subscription>,
    _connection_store_subscription: Option<Subscription>,
}

struct WatchedConnection {
    agent_id: AgentId,
    messages: Vec<WatchedConnectionMessage>,
    list_state: ListState,
    incoming_request_methods: HashMap<acp::RequestId, Arc<str>>,
    outgoing_request_methods: HashMap<acp::RequestId, Arc<str>>,
    _task: Task<()>,
}

impl AcpTools {
    fn select_connection(&mut self, agent_id: Option<AgentId>, cx: &mut Context<Self>) {
        if self.selected_connection == agent_id {
            return;
        }

        self.selected_connection = agent_id;
        self.expanded.clear();
        cx.notify();
    }

    fn restart_selected_connection(&mut self, _cx: &mut Context<Self>) {}

    fn selected_watched_connection(&self) -> Option<&WatchedConnection> {
        let selected_connection = self.selected_connection.as_ref()?;
        self.watched_connections.get(selected_connection)
    }

    fn selected_watched_connection_mut(&mut self) -> Option<&mut WatchedConnection> {
        let selected_connection = self.selected_connection.clone()?;
        self.watched_connections.get_mut(&selected_connection)
    }

    fn connection_menu_entries(&self) -> Vec<SharedString> {
        let mut entries: Vec<_> = self
            .watched_connections
            .values()
            .map(|connection| connection.agent_id.0.clone())
            .collect();
        entries.sort();
        entries
    }

    fn selected_connection_label(&self) -> SharedString {
        self.selected_connection
            .as_ref()
            .map(|agent_id| agent_id.0.clone())
            .unwrap_or_else(|| SharedString::from("No connection selected"))
    }

    fn connection_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ContextMenu> {
        let entries = self.connection_menu_entries();
        let selected_connection = self.selected_connection.clone();
        let acp_tools = cx.entity().downgrade();

        ContextMenu::build(window, cx, move |mut menu, _window, _cx| {
            if entries.is_empty() {
                return menu.entry("No active connections", None, |_, _| {});
            }

            for entry in &entries {
                let label = entry.clone();
                let is_selected = selected_connection
                    .as_ref()
                    .is_some_and(|agent_id| agent_id.0.as_ref() == label.as_ref());
                let acp_tools = acp_tools.clone();
                menu = menu.toggleable_entry(
                    label.clone(),
                    is_selected,
                    IconPosition::Start,
                    None,
                    move |_window, cx| {
                        acp_tools
                            .update(cx, |this, cx| {
                                this.select_connection(Some(AgentId(label.clone())), cx);
                            })
                            .ok();
                    },
                );
            }

            menu
        })
    }

    fn serialize_observed_messages(&self) -> Option<String> {
        let connection = self.selected_watched_connection()?;

        let messages: Vec<serde_json::Value> = connection
            .messages
            .iter()
            .filter_map(|message| {
                let params = match &message.params {
                    Ok(Some(params)) => params.clone(),
                    Ok(None) => serde_json::Value::Null,
                    Err(err) => serde_json::to_value(err).ok()?,
                };
                Some(serde_json::json!({
                    "_direction": match message.direction {
                        AcpDebugMessageDirection::Incoming => "incoming",
                        AcpDebugMessageDirection::Outgoing => "outgoing",
                        AcpDebugMessageDirection::Stderr => "stderr",
                    },
                    "_type": "asdf",
                    "id": message.request_id,
                    "method": message.name.to_string(),
                    "params": params,
                }))
            })
            .collect();

        serde_json::to_string_pretty(&messages).ok()
    }

    fn clear_messages(&mut self, cx: &mut Context<Self>) {
        if let Some(connection) = self.selected_watched_connection_mut() {
            connection.messages.clear();
            connection.list_state.reset(0);
            connection.incoming_request_methods.clear();
            connection.outgoing_request_methods.clear();
            self.expanded.clear();
            cx.notify();
        }
    }

    fn render_message(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(connection) = self.selected_watched_connection() else {
            return Empty.into_any();
        };

        let Some(message) = connection.messages.get(index) else {
            return Empty.into_any();
        };

        let base_size = TextSize::Editor.rems(cx);

        let theme_settings = ThemeSettings::get_global(cx);
        let text_style = window.text_style();

        let colors = cx.theme().colors();
        let expanded = self.expanded.contains(&index);

        v_flex()
            .id(index)
            .group("message")
            .font_buffer(cx)
            .w_full()
            .py_3()
            .pl_4()
            .pr_5()
            .gap_2()
            .items_start()
            .text_size(base_size)
            .border_color(colors.border)
            .border_b_1()
            .hover(|this| this.bg(colors.element_background.opacity(0.5)))
            .child(
                h_flex()
                    .id(("acp-log-message-header", index))
                    .w_full()
                    .gap_2()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.expanded.contains(&index) {
                            this.expanded.remove(&index);
                        } else {
                            this.expanded.insert(index);
                            let project = this.project.clone();
                            let Some(connection) = this.selected_watched_connection_mut() else {
                                return;
                            };
                            let Some(message) = connection.messages.get_mut(index) else {
                                return;
                            };
                            message.expanded(project.read(cx).languages().clone(), cx);
                            connection.list_state.scroll_to_reveal_item(index);
                        }
                        cx.notify()
                    }))
                    .child(match message.direction {
                        AcpDebugMessageDirection::Incoming => Icon::new(IconName::ArrowDown)
                            .color(Color::Error)
                            .size(IconSize::Small),
                        AcpDebugMessageDirection::Outgoing => Icon::new(IconName::ArrowUp)
                            .color(Color::Success)
                            .size(IconSize::Small),
                        AcpDebugMessageDirection::Stderr => Icon::new(IconName::Warning)
                            .color(Color::Warning)
                            .size(IconSize::Small),
                    })
                    .child(
                        Label::new(message.name.clone())
                            .buffer_font(cx)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1())
                    .children(
                        message
                            .request_id
                            .as_ref()
                            .map(|req_id| div().child(ui::Chip::new(req_id.to_string()))),
                    ),
            )
            // I'm aware using markdown is a hack. Trying to get something working for the demo.
            // Will clean up soon!
            .when_some(
                if expanded {
                    message.expanded_params_md.clone()
                } else {
                    message.collapsed_params_md.clone()
                },
                |this, params| {
                    this.child(
                        div().pl_6().w_full().child(
                            MarkdownElement::new(
                                params,
                                MarkdownStyle {
                                    base_text_style: text_style,
                                    selection_background_color: colors.element_selection_background,
                                    syntax: cx.theme().syntax().clone(),
                                    code_block_overflow_x_scroll: true,
                                    code_block: StyleRefinement {
                                        text: TextStyleRefinement {
                                            font_family: Some(
                                                theme_settings.buffer_font.family.clone(),
                                            ),
                                            font_size: Some((base_size * 0.8).into()),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                },
                            )
                            .code_block_renderer(
                                CodeBlockRenderer::Default {
                                    copy_button_visibility: if expanded {
                                        CopyButtonVisibility::VisibleOnHover
                                    } else {
                                        CopyButtonVisibility::Hidden
                                    },
                                    wrap_button_visibility: markdown::WrapButtonVisibility::Hidden,
                                    border: false,
                                },
                            ),
                        ),
                    )
                },
            )
            .into_any()
    }
}

struct WatchedConnectionMessage {
    name: SharedString,
    request_id: Option<acp::RequestId>,
    direction: AcpDebugMessageDirection,
    params: Result<Option<serde_json::Value>, acp::Error>,
    collapsed_params_md: Option<Entity<Markdown>>,
    expanded_params_md: Option<Entity<Markdown>>,
}

impl WatchedConnectionMessage {
    fn expanded(&mut self, language_registry: Arc<LanguageRegistry>, cx: &mut App) {
        let params_md = match &self.params {
            Ok(Some(params)) => Some(expanded_params_md(params, &language_registry, cx)),
            Err(err) => {
                if let Some(err) = &serde_json::to_value(err).log_err() {
                    Some(expanded_params_md(&err, &language_registry, cx))
                } else {
                    None
                }
            }
            _ => None,
        };
        self.expanded_params_md = params_md;
    }
}

fn expanded_params_md(
    params: &serde_json::Value,
    language_registry: &Arc<LanguageRegistry>,
    cx: &mut App,
) -> Entity<Markdown> {
    let params_json = serde_json::to_string_pretty(params).unwrap_or_default();
    let params_md = format!("```json\n{}\n```", params_json);
    cx.new(|cx| Markdown::new(params_md.into(), Some(language_registry.clone()), None, cx))
}

enum AcpToolsEvent {}

impl EventEmitter<AcpToolsEvent> for AcpTools {}

impl Item for AcpTools {
    type Event = AcpToolsEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> ui::SharedString {
        format!(
            "ACP: {}",
            self.selected_watched_connection()
                .map_or("Disconnected", |connection| connection.agent_id.0.as_ref())
        )
        .into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(ui::Icon::new(IconName::Thread))
    }
}

impl Focusable for AcpTools {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AcpTools {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_messages = self
            .selected_watched_connection()
            .is_some_and(|connection| !connection.messages.is_empty());
        let can_restart = false;
        let copied_messages = self.serialize_observed_messages().unwrap_or_default();

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        DropdownMenu::new(
                            "acp-connection-selector",
                            self.selected_connection_label(),
                            self.connection_menu(window, cx),
                        )
                        .style(DropdownStyle::Subtle)
                        .disabled(self.watched_connections.is_empty()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                IconButton::new("restart_connection", IconName::RotateCw)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Restart Connection"))
                                    .disabled(!can_restart)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.restart_selected_connection(cx);
                                    })),
                            )
                            .child(
                                CopyButton::new("copy-all-messages", copied_messages)
                                    .tooltip_label("Copy All Messages")
                                    .disabled(!has_messages),
                            )
                            .child(
                                IconButton::new("clear_messages", IconName::Trash)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Clear Messages"))
                                    .disabled(!has_messages)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.clear_messages(cx);
                                    })),
                            ),
                    ),
            )
            .child(match self.selected_watched_connection() {
                Some(connection) => {
                    if connection.messages.is_empty() {
                        h_flex()
                            .size_full()
                            .justify_center()
                            .items_center()
                            .child("No messages recorded yet")
                            .into_any()
                    } else {
                        div()
                            .size_full()
                            .flex_grow_1()
                            .child(
                                list(
                                    connection.list_state.clone(),
                                    cx.processor(Self::render_message),
                                )
                                .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
                                .size_full(),
                            )
                            .vertical_scrollbar_for(&connection.list_state, window, cx)
                            .into_any()
                    }
                }
                None => div().into_any(),
            })
    }
}

pub struct AcpToolsToolbarItemView {
    acp_tools: Option<Entity<AcpTools>>,
}

impl AcpToolsToolbarItemView {
    pub fn new() -> Self {
        Self { acp_tools: None }
    }
}

impl Render for AcpToolsToolbarItemView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = (&self.acp_tools, cx);
        Empty.into_any_element()
    }
}

impl EventEmitter<ToolbarItemEvent> for AcpToolsToolbarItemView {}

impl ToolbarItemView for AcpToolsToolbarItemView {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        if let Some(item) = active_pane_item
            && let Some(acp_tools) = item.downcast::<AcpTools>()
        {
            self.acp_tools = Some(acp_tools);
            cx.notify();
            return ToolbarItemLocation::Hidden;
        }
        if self.acp_tools.take().is_some() {
            cx.notify();
        }
        ToolbarItemLocation::Hidden
    }
}
