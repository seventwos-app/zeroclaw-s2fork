use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct AnthropicProvider {
    credential: Option<String>,
    base_url: String,
    client: Client,
    /// Optional proxy URL for session-key auth (e.g. Cloudflare Worker).
    /// When set, session-key requests route through this proxy instead of
    /// hitting claude.ai directly (which gets Cloudflare-challenged).
    proxy_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    content: Vec<NativeContentOut>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum NativeContentOut {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

#[derive(Debug, Serialize)]
struct ImageSource {
    #[serde(rename = "type")]
    kind: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    #[serde(default)]
    content: Vec<NativeContentIn>,
}

#[derive(Debug, Deserialize)]
struct NativeContentIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

impl AnthropicProvider {
    pub fn new(credential: Option<&str>) -> Self {
        Self::with_base_url(credential, None)
    }

    pub fn with_base_url(credential: Option<&str>, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .map(|u| u.trim_end_matches('/'))
            .unwrap_or("https://api.anthropic.com")
            .to_string();
        let proxy_url = std::env::var("ZEROCLAW_PROXY_URL")
            .ok()
            .map(|u| u.trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        Self {
            credential: credential
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(ToString::to_string),
            base_url,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
            proxy_url,
        }
    }

    /// Detect if credential is a claude.ai session key.
    fn is_session_key(token: &str) -> bool {
        token.starts_with("sk-ant-sid01-") || token.starts_with("sk-ant-sid02-")
    }

    fn is_oauth_token(token: &str) -> bool {
        token.starts_with("sk-ant-oat01-")
    }

    fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
        credential: &str,
    ) -> reqwest::RequestBuilder {
        if Self::is_oauth_token(credential) {
            // OAuth tokens require Bearer auth + beta header on api.anthropic.com.
            request
                .header("Authorization", format!("Bearer {credential}"))
                .header("anthropic-beta", "oauth-2025-04-20")
        } else {
            // Standard API keys use x-api-key.
            request.header("x-api-key", credential)
        }
    }

    /// Send a message via the Worker proxy (for session-key auth).
    /// The Worker handles claude.ai routing from Cloudflare's edge,
    /// bypassing the Cloudflare challenge that blocks direct reqwest calls.
    async fn chat_via_proxy(
        &self,
        proxy_url: &str,
        messages: &[ChatMessage],
        model: &str,
    ) -> anyhow::Result<String> {
        // Build prompt from messages: combine system + user messages.
        let mut system_parts = Vec::new();
        let mut user_parts = Vec::new();
        for msg in messages {
            match msg.role.as_str() {
                "system" => system_parts.push(msg.content.as_str()),
                "assistant" => {
                    user_parts.push("[Assistant previously said:]");
                    user_parts.push(msg.content.as_str());
                }
                _ => user_parts.push(msg.content.as_str()),
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        let message = user_parts.join("\n\n");

        let mut body = serde_json::json!({
            "message": message,
            "model": model,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::Value::String(sys);
        }

        let response = self
            .client
            .post(format!("{proxy_url}/chat"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Proxy chat failed ({status}): {text}");
        }

        let data: serde_json::Value = response.json().await?;
        data.get("response")
            .and_then(|r| r.as_str())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("No response field in proxy reply"))
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }
        Some(
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.parameters.clone(),
                })
                .collect(),
        )
    }

    fn parse_assistant_tool_call_message(content: &str) -> Option<Vec<NativeContentOut>> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_calls = value
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ProviderToolCall>>(v.clone()).ok())?;

        let mut blocks = Vec::new();
        if let Some(text) = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            blocks.push(NativeContentOut::Text {
                text: text.to_string(),
            });
        }
        for call in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(NativeContentOut::ToolUse {
                id: call.id,
                name: call.name,
                input,
            });
        }
        Some(blocks)
    }

    fn parse_tool_result_message(content: &str) -> Option<NativeMessage> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_use_id = value
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let result = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(NativeMessage {
            role: "user".to_string(),
            content: vec![NativeContentOut::ToolResult {
                tool_use_id,
                content: result,
            }],
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<NativeMessage>) {
        let mut system_prompt = None;
        let mut native_messages = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    if system_prompt.is_none() {
                        system_prompt = Some(msg.content.clone());
                    }
                }
                "assistant" => {
                    if let Some(blocks) = Self::parse_assistant_tool_call_message(&msg.content) {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    } else {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                            }],
                        });
                    }
                }
                "tool" => {
                    if let Some(tool_result) = Self::parse_tool_result_message(&msg.content) {
                        native_messages.push(tool_result);
                    } else {
                        native_messages.push(NativeMessage {
                            role: "user".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                            }],
                        });
                    }
                }
                _ => {
                    // Detect multimodal user messages encoded as JSON:
                    // {"text": "...", "images": [{"data": "base64...", "media_type": "image/jpeg"}]}
                    let content_blocks = if let Ok(v) =
                        serde_json::from_str::<serde_json::Value>(&msg.content)
                    {
                        if v.get("images").and_then(|i| i.as_array()).is_some() {
                            let text = v
                                .get("text")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            let images = v["images"].as_array().unwrap();
                            let mut blocks: Vec<NativeContentOut> = images
                                .iter()
                                .filter_map(|img| {
                                    let data = img.get("data")?.as_str()?.to_string();
                                    let media_type =
                                        img.get("media_type")?.as_str()?.to_string();
                                    Some(NativeContentOut::Image {
                                        source: ImageSource {
                                            kind: "base64".to_string(),
                                            media_type,
                                            data,
                                        },
                                    })
                                })
                                .collect();
                            if !text.is_empty() {
                                blocks.push(NativeContentOut::Text { text });
                            }
                            blocks
                        } else {
                            vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                            }]
                        }
                    } else {
                        vec![NativeContentOut::Text {
                            text: msg.content.clone(),
                        }]
                    };
                    native_messages.push(NativeMessage {
                        role: "user".to_string(),
                        content: content_blocks,
                    });
                }
            }
        }

        (system_prompt, native_messages)
    }

    fn parse_text_response(response: ChatResponse) -> anyhow::Result<String> {
        response
            .content
            .into_iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text)
            .ok_or_else(|| anyhow::anyhow!("No response from Anthropic"))
    }

    fn parse_native_response(response: NativeChatResponse) -> ProviderChatResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in response.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(text) = block.text.map(|t| t.trim().to_string()) {
                        if !text.is_empty() {
                            text_parts.push(text);
                        }
                    }
                }
                "tool_use" => {
                    let name = block.name.unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let arguments = block
                        .input
                        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                    tool_calls.push(ProviderToolCall {
                        id: block.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: arguments.to_string(),
                    });
                }
                _ => {}
            }
        }

        ProviderChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls,
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Anthropic credentials not set. Set CLAUDE_SESSION_KEY, ANTHROPIC_API_KEY, or ANTHROPIC_OAUTH_TOKEN."
            )
        })?;

        // Route through Worker proxy when using a session key + proxy is configured.
        if Self::is_session_key(credential) {
            if let Some(proxy_url) = &self.proxy_url {
                let mut messages = Vec::new();
                if let Some(sys) = system_prompt {
                    messages.push(ChatMessage::system(sys));
                }
                messages.push(ChatMessage::user(message));
                return self.chat_via_proxy(proxy_url, &messages, model).await;
            }
            anyhow::bail!(
                "Session key auth requires ZEROCLAW_PROXY_URL to be set (direct claude.ai access is Cloudflare-blocked)"
            );
        }

        let request = ChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt.map(ToString::to_string),
            messages: vec![Message {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            temperature,
        };

        let mut request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request);

        request = self.apply_auth(request, credential);

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("Anthropic", response).await);
        }

        let chat_response: ChatResponse = response.json().await?;
        Self::parse_text_response(chat_response)
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Anthropic credentials not set. Set CLAUDE_SESSION_KEY, ANTHROPIC_API_KEY, or ANTHROPIC_OAUTH_TOKEN."
            )
        })?;

        // Route through Worker proxy when using a session key.
        if Self::is_session_key(credential) {
            if let Some(proxy_url) = &self.proxy_url {
                let text = self
                    .chat_via_proxy(proxy_url, request.messages, model)
                    .await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: Vec::new(),
                });
            }
            anyhow::bail!(
                "Session key auth requires ZEROCLAW_PROXY_URL to be set (direct claude.ai access is Cloudflare-blocked)"
            );
        }

        let (system_prompt, messages) = Self::convert_messages(request.messages);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt,
            messages,
            temperature,
            tools: Self::convert_tools(request.tools),
        };

        let req = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&native_request);

        let response = self.apply_auth(req, credential).send().await?;
        if !response.status().is_success() {
            return Err(super::api_error("Anthropic", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        Ok(Self::parse_native_response(native_response))
    }

    fn supports_native_tools(&self) -> bool {
        // Session key auth uses claude.ai web API which doesn't support native tool calling.
        if let Some(cred) = &self.credential {
            if Self::is_session_key(cred) {
                return false;
            }
        }
        true
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Anthropic credentials not set. Set CLAUDE_SESSION_KEY, ANTHROPIC_API_KEY, or ANTHROPIC_OAUTH_TOKEN."
            )
        })?;

        // Session key: fall back to text-based chat via proxy.
        if Self::is_session_key(credential) {
            if let Some(proxy_url) = &self.proxy_url {
                let text = self.chat_via_proxy(proxy_url, messages, model).await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: Vec::new(),
                });
            }
            anyhow::bail!(
                "Session key auth requires ZEROCLAW_PROXY_URL to be set (direct claude.ai access is Cloudflare-blocked)"
            );
        }

        let (system_prompt, native_messages) = Self::convert_messages(messages);

        // Convert OpenAI-format tool JSON to Anthropic NativeToolSpec
        let native_tools: Vec<NativeToolSpec> = tools
            .iter()
            .filter_map(|t| {
                let func = t.get("function")?;
                Some(NativeToolSpec {
                    name: func.get("name")?.as_str()?.to_string(),
                    description: func.get("description")?.as_str()?.to_string(),
                    input_schema: func.get("parameters").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                })
            })
            .collect();

        let native_request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt,
            messages: native_messages,
            temperature,
            tools: if native_tools.is_empty() { None } else { Some(native_tools) },
        };

        let req = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&native_request);

        let response = self.apply_auth(req, credential).send().await?;
        if !response.status().is_success() {
            return Err(super::api_error("Anthropic", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        Ok(Self::parse_native_response(native_response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = AnthropicProvider::new(Some("anthropic-test-credential"));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicProvider::new(None);
        assert!(p.credential.is_none());
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicProvider::new(Some(""));
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_whitespace_key() {
        let p = AnthropicProvider::new(Some("  anthropic-test-credential  "));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("anthropic-test-credential"));
    }

    #[test]
    fn creates_with_custom_base_url() {
        let p = AnthropicProvider::with_base_url(
            Some("anthropic-credential"),
            Some("https://api.example.com"),
        );
        assert_eq!(p.base_url, "https://api.example.com");
        assert_eq!(p.credential.as_deref(), Some("anthropic-credential"));
    }

    #[test]
    fn custom_base_url_trims_trailing_slash() {
        let p = AnthropicProvider::with_base_url(None, Some("https://api.example.com/"));
        assert_eq!(p.base_url, "https://api.example.com");
    }

    #[test]
    fn default_base_url_when_none_provided() {
        let p = AnthropicProvider::with_base_url(None, None);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(None, "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("credentials not set"),
            "Expected key error, got: {err}"
        );
    }

    #[test]
    fn session_key_detection_works() {
        assert!(AnthropicProvider::is_session_key(
            "sk-ant-sid01-abcdef1234567890"
        ));
        assert!(AnthropicProvider::is_session_key(
            "sk-ant-sid02-abcdef1234567890"
        ));
        assert!(!AnthropicProvider::is_session_key("sk-ant-oat01-abcdef"));
        assert!(!AnthropicProvider::is_session_key("sk-ant-api-key"));
    }

    #[test]
    fn oauth_token_detection_works() {
        assert!(AnthropicProvider::is_oauth_token("sk-ant-oat01-abcdef"));
        assert!(!AnthropicProvider::is_oauth_token("sk-ant-api-key"));
    }

    #[test]
    fn supports_native_tools_false_for_session_key() {
        let p = AnthropicProvider::new(Some("sk-ant-sid01-test-session-key-value"));
        assert!(!p.supports_native_tools());
    }

    #[test]
    fn supports_native_tools_true_for_api_key() {
        let p = AnthropicProvider::new(Some("sk-ant-api-key-12345"));
        assert!(p.supports_native_tools());
    }

    #[test]
    fn supports_native_tools_true_for_oauth_token() {
        let p = AnthropicProvider::new(Some("sk-ant-oat01-oauth-token"));
        assert!(p.supports_native_tools());
    }

    #[test]
    fn apply_auth_uses_bearer_and_beta_for_oauth_tokens() {
        let provider = AnthropicProvider::new(None);
        let request = provider
            .apply_auth(
                provider.client.get("https://api.anthropic.com/v1/models"),
                "sk-ant-oat01-test-token",
            )
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-ant-oat01-test-token")
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok()),
            Some("oauth-2025-04-20")
        );
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn apply_auth_uses_x_api_key_for_regular_tokens() {
        let provider = AnthropicProvider::new(None);
        let request = provider
            .apply_auth(
                provider.client.get("https://api.anthropic.com/v1/models"),
                "sk-ant-api-key",
            )
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok()),
            Some("sk-ant-api-key")
        );
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("anthropic-beta").is_none());
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn chat_request_serializes_without_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("system"),
            "system field should be skipped when None"
        );
        assert!(json.contains("claude-3-opus"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn chat_request_serializes_with_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are ZeroClaw".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\":\"You are ZeroClaw\""));
    }

    #[test]
    fn chat_response_deserializes() {
        let json = r#"{"content":[{"type":"text","text":"Hello there!"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].kind, "text");
        assert_eq!(resp.content[0].text.as_deref(), Some("Hello there!"));
    }

    #[test]
    fn chat_response_empty_content() {
        let json = r#"{"content":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_empty());
    }

    #[test]
    fn chat_response_multiple_blocks() {
        let json =
            r#"{"content":[{"type":"text","text":"First"},{"type":"text","text":"Second"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.content[0].text.as_deref(), Some("First"));
        assert_eq!(resp.content[1].text.as_deref(), Some("Second"));
    }

    #[test]
    fn temperature_range_serializes() {
        for temp in [0.0, 0.5, 1.0, 2.0] {
            let req = ChatRequest {
                model: "claude-3-opus".to_string(),
                max_tokens: 4096,
                system: None,
                messages: vec![],
                temperature: temp,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(&format!("{temp}")));
        }
    }
}
