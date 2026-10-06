// OpenAI Responses API client. The API key and file bytes stay in Rust.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::claude::{ChatContext, ChatReply};
use crate::secrets;

const ENDPOINT: &str = "https://api.openai.com/v1/responses";
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";

const SYSTEM_PROMPT: &str =
    "You are Mochi, a personal AI assistant living at the top of the user's screen. \
Help with research, coding, recommendations, tasks, and questions. Respond in the user's language. \
Be complete but concise. Use plain text with line breaks and no markdown formatting.";

#[derive(Default)]
pub struct OpenAiChat {
    messages: Mutex<Vec<Value>>,
}

impl OpenAiChat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

pub async fn send(
    chat: &OpenAiChat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("openai-api-key")
        .ok_or_else(|| "OpenAI API key missing. Open settings.".to_string())?;

    let mut content = Vec::new();
    if chat.is_empty() {
        match context {
            Some(ChatContext::File { name, path }) => {
                content.extend(file_content(&name, &path));
            }
            Some(ChatContext::Window {
                app_name,
                title,
                url,
            }) => {
                let mut text = format!("Context — App: {app_name}, Window: {title}");
                if let Some(url) = url {
                    text.push_str(&format!(", URL: {url}"));
                }
                content.push(json!({ "type": "input_text", "text": text }));
            }
            None => {}
        }
    }
    content.push(json!({ "type": "input_text", "text": query }));
    chat.push(json!({ "role": "user", "content": content }));

    let body = json!({
        "model": model,
        "instructions": SYSTEM_PROMPT,
        "input": chat.snapshot(),
        "store": false,
    });

    let response = match call(&key, &body).await {
        Ok(value) => value,
        Err(err) => {
            chat.pop();
            return Err(err);
        }
    };

    let text = response_text(&response);
    if text.is_empty() {
        chat.pop();
        return Err("OpenAI returned no response text.".into());
    }

    chat.push(json!({
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text }],
    }));
    Ok(ChatReply { text })
}

async fn call(key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(ENDPOINT)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("OpenAI API {status}: {detail}"));
    }

    serde_json::from_str(&text).map_err(|e| format!("Bad OpenAI response: {e}"))
}

fn response_text(response: &Value) -> String {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn file_content(name: &str, path: &str) -> Vec<Value> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match extension.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };

    if let Some(media_type) = media_type {
        if let Ok(bytes) = std::fs::read(path) {
            return vec![
                json!({ "type": "input_text", "text": format!("File: {name}") }),
                json!({
                    "type": "input_image",
                    "image_url": format!("data:{media_type};base64,{}", crate::claude::base64_for(&bytes)),
                }),
            ];
        }
    }

    let text = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.len() <= MAX_INLINE_TEXT)
        .and_then(|_| std::fs::read_to_string(path).ok());
    match text {
        Some(text) => vec![json!({
            "type": "input_text",
            "text": format!("File: {name}\n\nFile contents:\n{text}"),
        })],
        None => vec![json!({
            "type": "input_text",
            "text": format!("File attached: {name}. Its contents could not be read as text."),
        })],
    }
}

#[cfg(test)]
mod tests {
    use super::response_text;
    use serde_json::json;

    #[test]
    fn collects_text_after_non_message_output_items() {
        let response = json!({
            "output": [
                { "type": "reasoning", "content": [] },
                { "type": "message", "content": [
                    { "type": "output_text", "text": "First" },
                    { "type": "output_text", "text": "Second" }
                ]}
            ]
        });
        assert_eq!(response_text(&response), "First\nSecond");
    }
}
