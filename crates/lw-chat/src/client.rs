use bytes::Bytes;
use futures_core::Stream;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio_stream::StreamExt;

use crate::error::ChatError;
use crate::types::{
    ChatEvent, ChatMessageResponse, ChatRequest, ChatSessionResponse, CreateSessionRequest,
    SaveMessageRequest, UpdateSessionTitleRequest,
};

/// SSE streaming client for chat completions
pub struct ChatClient {
    client: reqwest::Client,
}

impl Default for ChatClient {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl ChatClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stream chat completion events via SSE
    pub async fn stream_completion(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        request: ChatRequest,
    ) -> Result<impl Stream<Item = Result<ChatEvent, ChatError>>, ChatError> {
        let mut headers = Self::auth_headers(token);
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));

        let resp = self
            .client
            .post(format!("{base_url}/api/org/{tenant}/chat/completions"))
            .headers(headers)
            .json(&request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        Ok(parse_sse_stream(resp.bytes_stream()))
    }

    fn auth_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid auth header"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    /// POST /api/org/{tenant}/chat/sessions — create a new session
    pub async fn create_session(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        request: &CreateSessionRequest,
    ) -> Result<ChatSessionResponse, ChatError> {
        let resp = self
            .client
            .post(format!("{base_url}/api/org/{tenant}/chat/sessions"))
            .headers(Self::auth_headers(token))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        resp.json()
            .await
            .map_err(|e| ChatError::Parse(e.to_string()))
    }

    /// GET /api/org/{tenant}/chat/sessions — list user's sessions
    pub async fn list_sessions(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
    ) -> Result<Vec<ChatSessionResponse>, ChatError> {
        let resp = self
            .client
            .get(format!("{base_url}/api/org/{tenant}/chat/sessions"))
            .headers(Self::auth_headers(token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        resp.json()
            .await
            .map_err(|e| ChatError::Parse(e.to_string()))
    }

    /// GET /api/org/{tenant}/chat/sessions/{id}/messages — get session messages
    pub async fn get_session_messages(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        session_id: &str,
    ) -> Result<Vec<ChatMessageResponse>, ChatError> {
        let resp = self
            .client
            .get(format!(
                "{base_url}/api/org/{tenant}/chat/sessions/{session_id}/messages"
            ))
            .headers(Self::auth_headers(token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        resp.json()
            .await
            .map_err(|e| ChatError::Parse(e.to_string()))
    }

    /// POST /api/org/{tenant}/chat/sessions/{id}/messages — save a message
    pub async fn save_message(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        session_id: &str,
        request: &SaveMessageRequest,
    ) -> Result<ChatMessageResponse, ChatError> {
        let resp = self
            .client
            .post(format!(
                "{base_url}/api/org/{tenant}/chat/sessions/{session_id}/messages"
            ))
            .headers(Self::auth_headers(token))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        resp.json()
            .await
            .map_err(|e| ChatError::Parse(e.to_string()))
    }

    /// PATCH /api/org/{tenant}/chat/sessions/{id} — update session title
    pub async fn update_session_title(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        session_id: &str,
        request: &UpdateSessionTitleRequest,
    ) -> Result<(), ChatError> {
        let resp = self
            .client
            .patch(format!(
                "{base_url}/api/org/{tenant}/chat/sessions/{session_id}"
            ))
            .headers(Self::auth_headers(token))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        Ok(())
    }

    /// DELETE /api/org/{tenant}/chat/sessions/{id} — delete a session
    pub async fn delete_session(
        &self,
        base_url: &str,
        token: &str,
        tenant: &str,
        session_id: &str,
    ) -> Result<(), ChatError> {
        let resp = self
            .client
            .delete(format!(
                "{base_url}/api/org/{tenant}/chat/sessions/{session_id}"
            ))
            .headers(Self::auth_headers(token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_default();
            return Err(ChatError::Api { status, message });
        }

        Ok(())
    }
}

/// Parse an SSE byte stream into a stream of ChatEvent results.
///
/// Handles:
/// - Buffering partial lines across chunk boundaries
/// - Filtering `data: ` prefix lines
/// - Skipping empty lines and `data: [DONE]` terminator
/// - JSON deserializing each event line
/// - Yielding parse errors for malformed lines (does not stop the stream)
pub fn parse_sse_stream(
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>>,
) -> impl Stream<Item = Result<ChatEvent, ChatError>> {
    async_stream::stream! {
        let mut pinned = std::pin::pin!(byte_stream);
        let mut buffer = String::new();

        while let Some(chunk_result) = StreamExt::next(&mut pinned).await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(e) => {
                    yield Err(ChatError::Network(e));
                    continue;
                }
            };

            let chunk_str = match std::str::from_utf8(&chunk) {
                Ok(s) => s,
                Err(e) => {
                    yield Err(ChatError::Parse(format!("Invalid UTF-8: {e}")));
                    continue;
                }
            };

            buffer.push_str(chunk_str);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim().to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };

                if data == "[DONE]" {
                    return;
                }

                match serde_json::from_str::<ChatEvent>(data) {
                    Ok(event) => yield Ok(event),
                    Err(e) => yield Err(ChatError::Parse(format!(
                        "Failed to parse SSE event: {e} — raw: {data}"
                    ))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    fn sse_bytes(input: &str) -> impl Stream<Item = Result<Bytes, reqwest::Error>> {
        let bytes = Bytes::from(input.to_string());
        tokio_stream::once(Ok(bytes))
    }

    #[tokio::test]
    async fn god_test_full_conversation_stream() {
        let input = concat!(
            "data: {\"type\":\"thinking_delta\",\"text\":\"Let me search\"}\n",
            "data: {\"type\":\"thinking_delta\",\"text\":\" the knowledge base.\"}\n",
            "\n",
            "data: {\"type\":\"tool_call_start\",\"toolCallId\":\"toolu_01ABC\",\"toolName\":\"knowledge_base_retrieval\"}\n",
            "data: {\"type\":\"tool_call_delta\",\"toolCallId\":\"toolu_01ABC\",\"argsPartial\":\"{\\\"quer\"}\n",
            "data: {\"type\":\"tool_call_delta\",\"toolCallId\":\"toolu_01ABC\",\"argsPartial\":\"y\\\":\\\"refund policy\\\"}\"}\n",
            "data: {\"type\":\"tool_call_result\",\"toolCallId\":\"toolu_01ABC\",\"result\":[{\"text\":\"Refund within 30 days...\"}]}\n",
            "\n",
            "data: {\"type\":\"text_delta\",\"text\":\"Based on\"}\n",
            "data: {\"type\":\"text_delta\",\"text\":\" the knowledge base, \"}\n",
            "data: {\"type\":\"text_delta\",\"text\":\"refunds are available within **30 days**.\"}\n",
            "data: {\"type\":\"done\"}\n",
            "data: [DONE]\n",
        );

        let stream = parse_sse_stream(sse_bytes(input));
        let mut pinned = std::pin::pin!(stream);
        let mut events = Vec::new();

        while let Some(result) = StreamExt::next(&mut pinned).await {
            events.push(result.expect("all events should parse successfully"));
        }

        assert_eq!(events.len(), 10);

        let thinking: String = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::ThinkingDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, "Let me search the knowledge base.");

        let args: String = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::ToolCallDelta { args_partial, .. } => Some(args_partial.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(args, r#"{"query":"refund policy"}"#);

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text,
            "Based on the knowledge base, refunds are available within **30 days**."
        );

        assert!(events.iter().any(|e| matches!(
            e,
            ChatEvent::ToolCallStart {
                tool_call_id,
                tool_name,
            } if tool_call_id == "toolu_01ABC" && tool_name == "knowledge_base_retrieval"
        )));

        assert!(matches!(events.last(), Some(ChatEvent::Done)));
    }

    #[tokio::test]
    async fn malformed_stream_error_tolerance() {
        let input = concat!(
            "data: {\"type\":\"text_delta\",\"text\":\"good\"}\n",
            "\n",
            "data: {broken json here\n",
            "data: {\"type\":\"text_delta\",\"text\":\"still works\"}\n",
            "data: not even a json\n",
            "data: {\"type\":\"unknown_event\",\"foo\":\"bar\"}\n",
            "data: {\"type\":\"done\"}\n",
            "data: [DONE]\n",
        );

        let stream = parse_sse_stream(sse_bytes(input));
        let mut pinned = std::pin::pin!(stream);
        let mut results: Vec<Result<ChatEvent, ChatError>> = Vec::new();

        while let Some(result) = StreamExt::next(&mut pinned).await {
            results.push(result);
        }

        assert!(matches!(
            &results[0],
            Ok(ChatEvent::TextDelta { text }) if text == "good"
        ));
        assert!(matches!(&results[1], Err(ChatError::Parse(_))));
        assert!(matches!(
            &results[2],
            Ok(ChatEvent::TextDelta { text }) if text == "still works"
        ));
        assert!(matches!(&results[3], Err(ChatError::Parse(_))));
        assert!(matches!(&results[4], Err(ChatError::Parse(_))));
        assert!(matches!(&results[5], Ok(ChatEvent::Done)));
        assert_eq!(results.len(), 6);
    }
}
