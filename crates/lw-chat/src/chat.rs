use dioxus::prelude::*;
use tokio_stream::StreamExt;

use crate::client::ChatClient;
use crate::markdown::Markdown;
use crate::types::{
    ChatContext, ChatEvent, ChatMessage, ChatMode, ChatRequest, ChatRole, ChatSessionResponse,
    CreateSessionRequest, SaveMessageRequest, ToolCallInfo,
};

/// Configuration for the chat panel — passed from the host app.
#[derive(Clone, PartialEq)]
pub struct ChatConfig {
    pub base_url: String,
    pub auth_token: Signal<String>,
    pub tenant: Signal<String>,
    pub project_id: Signal<Option<String>>,
}

/// Main chat panel with history dropdown in header.
#[component]
pub fn ChatPanel(config: ChatConfig) -> Element {
    let mut sessions: Signal<Vec<ChatSessionResponse>> = use_signal(Vec::new);
    let mut active_session_id: Signal<Option<String>> = use_signal(|| None);
    let mut messages: Signal<Vec<ChatMessage>> = use_signal(Vec::new);
    let streaming_text: Signal<String> = use_signal(String::new);
    let streaming_tools: Signal<Vec<ToolCallInfo>> = use_signal(Vec::new);
    let mut is_streaming = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut history_open = use_signal(|| false);
    let mut at_bottom = use_signal(|| true);
    let mut messages_el: Signal<Option<MountedEvent>> = use_signal(|| None);

    // Load sessions on mount and when tenant changes
    let config_load = config.clone();
    use_effect(move || {
        let tenant = config_load.tenant.read().clone();
        if tenant.is_empty() {
            return;
        }
        let token = config_load.auth_token.read().clone();
        let base_url = config_load.base_url.clone();
        spawn(async move {
            let client = ChatClient::new();
            match client.list_sessions(&base_url, &token, &tenant).await {
                Ok(list) => sessions.set(list),
                Err(e) => tracing::warn!("Failed to load sessions: {e}"),
            }
        });
    });

    // Select a session and load its messages
    let config_select = config.clone();
    let select_session = move |session_id: String| {
        let token = config_select.auth_token.read().clone();
        let tenant = config_select.tenant.read().clone();
        let base_url = config_select.base_url.clone();
        active_session_id.set(Some(session_id.clone()));
        messages.set(Vec::new());
        error_msg.set(None);
        history_open.set(false);

        spawn(async move {
            let client = ChatClient::new();
            match client
                .get_session_messages(&base_url, &token, &tenant, &session_id)
                .await
            {
                Ok(msg_list) => {
                    let chat_messages: Vec<ChatMessage> = msg_list
                        .into_iter()
                        .filter(|m| m.role == ChatRole::User || m.role == ChatRole::Assistant)
                        .map(|m| ChatMessage {
                            role: m.role,
                            content: m.content,
                        })
                        .collect();
                    messages.set(chat_messages);
                }
                Err(e) => {
                    error_msg.set(Some(format!("Failed to load messages: {e}")));
                }
            }
        });
    };

    // Start new conversation
    let new_chat = move |_: MouseEvent| {
        active_session_id.set(None);
        messages.set(Vec::new());
        error_msg.set(None);
        history_open.set(false);
    };

    // Delete session
    let config_del = config.clone();
    let delete_session = move |session_id: String| {
        let token = config_del.auth_token.read().clone();
        let tenant = config_del.tenant.read().clone();
        let base_url = config_del.base_url.clone();

        spawn(async move {
            let client = ChatClient::new();
            if let Err(e) = client
                .delete_session(&base_url, &token, &tenant, &session_id)
                .await
            {
                tracing::warn!("Failed to delete session: {e}");
                return;
            }
            sessions.write().retain(|s| s.id != session_id);
            if active_session_id.read().as_deref() == Some(&session_id) {
                active_session_id.set(None);
                messages.set(Vec::new());
            }
        });
    };

    // Auto-scroll to bottom when streaming text updates and user is following
    use_effect(move || {
        let _text = streaming_text.read();
        if *at_bottom.read()
            && let Some(el) = messages_el.read().as_ref()
        {
            let el = el.clone();
            spawn(async move {
                let _ = el.data().scroll_to(ScrollBehavior::Smooth).await;
            });
        }
    });

    // Send message
    let config_send = config.clone();
    let mut do_send = move |_: ()| {
        let text = draft.read().trim().to_string();
        if text.is_empty() || *is_streaming.read() {
            return;
        }

        let tenant = config_send.tenant.read().clone();
        if tenant.is_empty() {
            error_msg.set(Some("Please select a tenant first".to_string()));
            return;
        }

        draft.set(String::new());
        error_msg.set(None);

        messages.write().push(ChatMessage {
            role: ChatRole::User,
            content: text,
        });

        is_streaming.set(true);
        let config_inner = config_send.clone();

        spawn(async move {
            stream_response(
                config_inner,
                messages,
                streaming_text,
                streaming_tools,
                error_msg,
                active_session_id,
                sessions,
            )
            .await;
            is_streaming.set(false);
        });
    };

    let send_click = {
        let mut do_send = do_send.clone();
        move |_: MouseEvent| {
            do_send(());
        }
    };

    let on_keydown = move |e: KeyboardEvent| {
        if e.key() == Key::Enter && !e.modifiers().shift() {
            e.prevent_default();
            do_send(());
        }
    };

    let msgs = messages.read();
    let streaming = streaming_text.read();
    let tools = streaming_tools.read();
    let is_active = *is_streaming.read();
    let err = error_msg.read();
    let no_tenant = config.tenant.read().is_empty();
    let show_history = *history_open.read();
    let current_sessions = sessions.read();
    let current_session = active_session_id.read();

    rsx! {
        div { class: "chat-panel",
            // Header with history dropdown
            div { class: "chat-header",
                span { "Ask Linus" }
                div { class: "chat-header-actions",
                    button {
                        class: "chat-header-btn",
                        onclick: new_chat,
                        title: "New chat",
                        "+"
                    }
                    div { class: "chat-history-wrapper",
                        button {
                            class: if show_history { "chat-header-btn active" } else { "chat-header-btn" },
                            onclick: move |_| history_open.set(!show_history),
                            title: "Chat history",
                            "☰"
                        }
                        // History dropdown
                        if show_history {
                            div { class: "chat-history-dropdown slide-down",
                                div { class: "chat-history-title", "History" }
                                if current_sessions.is_empty() {
                                    div { class: "chat-history-empty", "No conversations yet" }
                                }
                                for session in current_sessions.iter() {
                                    {
                                        let sid = session.id.clone();
                                        let sid_select = session.id.clone();
                                        let sid_delete = session.id.clone();
                                        let is_selected = current_session.as_deref() == Some(&sid);
                                        let title = session.title.clone().unwrap_or_else(|| "New chat".to_string());
                                        let mut select_session = select_session.clone();
                                        let delete_session = delete_session.clone();
                                        rsx! {
                                            div {
                                                class: if is_selected { "chat-history-item active" } else { "chat-history-item" },
                                                onclick: move |_| select_session(sid_select.clone()),
                                                span { class: "chat-history-item-title", "{title}" }
                                                button {
                                                    class: "chat-history-delete",
                                                    onclick: move |e: MouseEvent| {
                                                        e.stop_propagation();
                                                        delete_session(sid_delete.clone());
                                                    },
                                                    title: "Delete",
                                                    "×"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "chat-messages-wrapper", style: "position: relative; flex: 1; overflow: hidden; display: flex; flex-direction: column;",
            div {
                class: "chat-messages",
                onmounted: move |evt: MountedEvent| messages_el.set(Some(evt)),
                onscroll: move |evt: Event<ScrollData>| {
                    let data = evt.data();
                    let gap = data.scroll_height() as f64 - data.scroll_top() - data.client_height() as f64;
                    at_bottom.set(gap < 40.0);
                },
                if msgs.is_empty() && !is_active {
                    div { class: "chat-empty", "Ask a question about your project..." }
                }

                for (i, msg) in msgs.iter().enumerate() {
                    div { key: "{i}", class: "fade-in",
                        ChatBubble { message: msg.clone() }
                    }
                }

                if is_active {
                    for tc in tools.iter() {
                        div { class: "fade-in-left",
                            ToolCallCard { info: tc.clone() }
                        }
                    }

                    if !streaming.is_empty() {
                        div { class: "bubble bubble-assistant",
                            StreamingMarkdown { content: streaming.clone() }
                            span { class: "streaming-cursor" }
                        }
                    } else if tools.is_empty() {
                        div { class: "bubble bubble-assistant",
                            span { class: "streaming-cursor" }
                        }
                    }
                }

                if let Some(err_text) = err.as_ref() {
                    div {
                        class: "fade-in",
                        style: "color: var(--error, #ef4444); font-size: 13px; padding: 8px;",
                        "{err_text}"
                    }
                }
            }
            // Scroll-to-bottom button — only shown when not following tail
            if !*at_bottom.read() {
                button {
                    class: "chat-scroll-btn fade-in",
                    onclick: move |_| {
                        if let Some(el) = messages_el.read().as_ref() {
                            let el = el.clone();
                            spawn(async move {
                                let _ = el.data().scroll_to(
                                    ScrollBehavior::Smooth,
                                ).await;
                            });
                        }
                        at_bottom.set(true);
                    },
                    title: "Scroll to bottom",
                    "↓"
                }
            }
            } // close chat-messages-wrapper

            div { class: "chat-input",
                textarea {
                    value: "{draft}",
                    oninput: move |e| draft.set(e.value()),
                    onkeydown: on_keydown,
                    placeholder: if no_tenant { "Select a tenant first..." } else { "Ask a question..." },
                    rows: "1",
                    disabled: no_tenant,
                }
                button {
                    onclick: send_click,
                    disabled: is_active || draft.read().trim().is_empty() || no_tenant,
                    "Send"
                }
            }
        }
    }
}

/// Stream response with session persistence.
async fn stream_response(
    config: ChatConfig,
    mut messages: Signal<Vec<ChatMessage>>,
    mut streaming_text: Signal<String>,
    mut streaming_tools: Signal<Vec<ToolCallInfo>>,
    mut error_msg: Signal<Option<String>>,
    mut active_session_id: Signal<Option<String>>,
    mut sessions: Signal<Vec<ChatSessionResponse>>,
) {
    streaming_text.set(String::new());
    streaming_tools.set(Vec::new());

    let client = ChatClient::new();
    let token = config.auth_token.read().clone();
    let tenant = config.tenant.read().clone();
    let base_url = config.base_url.clone();

    let user_msg = {
        let msgs = messages.read();
        msgs.last().cloned()
    };
    let Some(user_msg) = user_msg else { return };

    // Create session if new conversation
    let existing_id = active_session_id.read().clone();
    let session_id = match existing_id {
        Some(id) => id,
        None => {
            let context = config.project_id.read().as_ref().map(|pid| ChatContext {
                project_id: Some(pid.clone()),
                mode: Some(ChatMode::Rag),
            });
            let title = truncate_title(&user_msg.content);
            match client
                .create_session(
                    &base_url,
                    &token,
                    &tenant,
                    &CreateSessionRequest {
                        title: Some(title),
                        context,
                    },
                )
                .await
            {
                Ok(session) => {
                    let id = session.id.clone();
                    active_session_id.set(Some(id.clone()));
                    sessions.write().insert(0, session);
                    id
                }
                Err(e) => {
                    error_msg.set(Some(format!("Failed to create session: {e}")));
                    return;
                }
            }
        }
    };

    // Save user message
    let _ = client
        .save_message(
            &base_url,
            &token,
            &tenant,
            &session_id,
            &SaveMessageRequest {
                role: ChatRole::User,
                content: user_msg.content.clone(),
                tool_calls: None,
            },
        )
        .await;

    // Stream completion
    let request = ChatRequest {
        messages: messages.read().clone(),
        context: Some(ChatContext {
            project_id: config.project_id.read().clone(),
            mode: Some(ChatMode::Rag),
        }),
        stream: true,
    };

    match client
        .stream_completion(&base_url, &token, &tenant, request)
        .await
    {
        Ok(stream) => {
            let mut pinned = std::pin::pin!(stream);
            let mut full_text = String::new();

            while let Some(result) = StreamExt::next(&mut pinned).await {
                match result {
                    Ok(event) => {
                        handle_event(
                            event,
                            &mut full_text,
                            &mut streaming_text,
                            &mut streaming_tools,
                        );
                    }
                    Err(e) => {
                        tracing::warn!("SSE parse error: {e}");
                    }
                }
            }

            if !full_text.is_empty() {
                messages.write().push(ChatMessage {
                    role: ChatRole::Assistant,
                    content: full_text.clone(),
                });

                let _ = client
                    .save_message(
                        &base_url,
                        &token,
                        &tenant,
                        &session_id,
                        &SaveMessageRequest {
                            role: ChatRole::Assistant,
                            content: full_text,
                            tool_calls: None,
                        },
                    )
                    .await;
            }
        }
        Err(e) => {
            error_msg.set(Some(format!("Chat error: {e}")));
        }
    }

    streaming_text.set(String::new());
    streaming_tools.set(Vec::new());
}

fn handle_event(
    event: ChatEvent,
    full_text: &mut String,
    streaming_text: &mut Signal<String>,
    streaming_tools: &mut Signal<Vec<ToolCallInfo>>,
) {
    match event {
        ChatEvent::TextDelta { text } => {
            full_text.push_str(&text);
            streaming_text.set(full_text.clone());
        }
        ChatEvent::ThinkingDelta { .. } => {}
        ChatEvent::ToolCallStart {
            tool_call_id,
            tool_name,
        } => {
            streaming_tools.write().push(ToolCallInfo {
                id: tool_call_id,
                name: tool_name,
                args_buffer: String::new(),
                result: None,
            });
        }
        ChatEvent::ToolCallDelta {
            tool_call_id,
            args_partial,
        } => {
            if let Some(tc) = streaming_tools
                .write()
                .iter_mut()
                .find(|t| t.id == tool_call_id)
            {
                tc.args_buffer.push_str(&args_partial);
            }
        }
        ChatEvent::ToolCallResult {
            tool_call_id,
            result,
        } => {
            if let Some(tc) = streaming_tools
                .write()
                .iter_mut()
                .find(|t| t.id == tool_call_id)
            {
                tc.result = Some(result);
            }
        }
        ChatEvent::Done => {}
    }
}

fn truncate_title(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= 60 {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..57])
    }
}

/// Incremental markdown renderer for streaming text.
///
/// Splits content at the last paragraph boundary (double newline).
/// Everything before is stable — rendered as full markdown (cached via use_memo).
/// The trailing partial paragraph is rendered as plain text (cheap to update).
#[component]
fn StreamingMarkdown(content: String) -> Element {
    let split_pos = content.rfind("\n\n").map(|p| p + 2).unwrap_or(0);
    let (stable, partial) = content.split_at(split_pos);

    let stable_owned = stable.to_string();
    let partial_owned = partial.to_string();

    rsx! {
        div { class: "md-content",
            if !stable_owned.is_empty() {
                Markdown { content: stable_owned }
            }
            if !partial_owned.is_empty() {
                span { "{partial_owned}" }
            }
        }
    }
}

#[component]
fn ChatBubble(message: ChatMessage) -> Element {
    let bubble_class = match message.role {
        ChatRole::User => "bubble bubble-user",
        ChatRole::Assistant => "bubble bubble-assistant",
        ChatRole::System => "bubble bubble-assistant",
    };

    rsx! {
        div { class: "{bubble_class}",
            match message.role {
                ChatRole::Assistant => rsx! { Markdown { content: message.content.clone() } },
                ChatRole::User => rsx! { p { "{message.content}" } },
                ChatRole::System => rsx! { p { "{message.content}" } },
            }
        }
    }
}

#[component]
fn ToolCallCard(info: ToolCallInfo) -> Element {
    rsx! {
        div { class: "tool-card",
            div { class: "tool-card-header",
                "🔧 {info.name}"
            }
            if let Some(result) = &info.result {
                div { class: "tool-card-result",
                    "{result}"
                }
            } else {
                div { class: "tool-card-result",
                    "Running..."
                }
            }
        }
    }
}
