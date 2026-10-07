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

    pub(crate) fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }

    pub(crate) fn replace(&self, messages: Vec<Value>) {
        *self.messages.lock().unwrap() = messages;
    }
}

pub async fn send(
    chat: &OpenAiChat,
    model: &str,
    auth: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let chatgpt_plan = auth == "chatgpt";
    let key = if chatgpt_plan {
        crate::chatgpt::access_token().await?
    } else {
        secrets::get("openai-api-key")
            .ok_or_else(|| "OpenAI API key missing. Open settings.".to_string())?
    };

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

    let mut body = json!({
        "model": model,
        "instructions": SYSTEM_PROMPT,
        "input": chat.snapshot(),
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if chatgpt_plan {
        body["stream"] = json!(true);
    }

    let response = match if chatgpt_plan {
        call_chatgpt_plan(&key, &body).await
    } else {
        call(&key, &body).await
    } {
        Ok(value) => value,
        Err(err) => {
            chat.pop();
            return Err(err);
        }
    };

    let response_text = response_text(&response);
    if response_text.is_empty() {
        chat.pop();
        return Err("OpenAI returned no response text.".into());
    }

    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            chat.push(item.clone());
        }
    } else {
        chat.push(json!({
            "role": "assistant",
            "content": [{ "type": "output_text", "text": response_text }],
        }));
    }
    Ok(ChatReply {
        text: response_text,
    })
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

/// ChatGPT plan usage requires an SSE request even though the island currently
/// reveals the completed answer in one update. Reading the full body still
/// consumes every event and only succeeds after `response.completed`.
async fn call_chatgpt_plan(token: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(ENDPOINT)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "OpenAI ChatGPT plan {status}: {}",
            api_error(&text)
        ));
    }

    parse_chatgpt_stream(&text)
}

fn parse_chatgpt_stream(text: &str) -> Result<Value, String> {
    let mut completed = false;
    let mut completed_response = None;
    let mut streamed_text = String::new();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    streamed_text.push_str(delta);
                }
            }
            Some("response.completed") => {
                completed = true;
                completed_response = event.get("response").cloned();
            }
            Some("response.failed") | Some("error") => {
                let detail = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("The ChatGPT plan request failed.");
                return Err(detail.to_string());
            }
            Some("response.incomplete") => {
                return Err("OpenAI returned an incomplete response.".into())
            }
            _ => {}
        }
    }
    if !completed {
        return Err("The ChatGPT plan stream ended before completion.".into());
    }
    let mut response = completed_response.unwrap_or_else(|| json!({ "output": [] }));
    if response_text(&response).is_empty() && !streamed_text.trim().is_empty() {
        if !response.is_object() {
            response = json!({ "output": [] });
        }
        let output = response
            .as_object_mut()
            .unwrap()
            .entry("output")
            .or_insert_with(|| json!([]));
        if !output.is_array() {
            *output = json!([]);
        }
        output.as_array_mut().unwrap().push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": streamed_text.trim() }],
        }));
    }
    Ok(response)
}

fn api_error(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.chars().take(200).collect())
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
    use super::{parse_chatgpt_stream, response_text};
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

    #[test]
    fn keeps_streamed_text_when_completed_response_omits_it() {
        let stream = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\",\"encrypted_content\":\"secret\"}]}}\n\n",
        );
        let response = parse_chatgpt_stream(stream).unwrap();
        assert_eq!(response_text(&response), "Hello world");
        assert_eq!(response["output"][0]["type"], "reasoning");
    }
}
