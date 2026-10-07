use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::Chat;
use crate::openai::OpenAiChat;

const MAX_CONVERSATIONS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: u64,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Conversation {
    id: String,
    title: String,
    provider: String,
    auth: String,
    model: String,
    created_at: u64,
    updated_at: u64,
    #[serde(default)]
    messages: Vec<StoredMessage>,
    #[serde(default)]
    backend_messages: Vec<Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    active_id: Option<String>,
    #[serde(default)]
    conversations: Vec<Conversation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub auth: String,
    pub model: String,
    pub updated_at: u64,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatView {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub auth: String,
    pub model: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<StoredMessage>,
}

impl From<&Conversation> for ChatView {
    fn from(value: &Conversation) -> Self {
        Self {
            id: value.id.clone(),
            title: value.title.clone(),
            provider: value.provider.clone(),
            auth: value.auth.clone(),
            model: value.model.clone(),
            created_at: value.created_at,
            updated_at: value.updated_at,
            messages: value.messages.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub text: String,
    pub conversation: ChatView,
}

#[derive(Debug, Clone)]
pub struct ConversationConfig {
    pub provider: String,
    pub auth: String,
    pub model: String,
}

pub struct ChatHistory {
    inner: Mutex<HistoryFile>,
}

impl ChatHistory {
    pub fn load() -> Self {
        let mut value: HistoryFile = std::fs::read(history_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        value
            .conversations
            .sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        if value
            .active_id
            .as_ref()
            .is_some_and(|id| !value.conversations.iter().any(|item| &item.id == id))
        {
            value.active_id = value.conversations.first().map(|item| item.id.clone());
        }
        Self {
            inner: Mutex::new(value),
        }
    }

    pub fn list(&self) -> Vec<ChatSummary> {
        let mut items = self
            .inner
            .lock()
            .unwrap()
            .conversations
            .iter()
            .map(|item| ChatSummary {
                id: item.id.clone(),
                title: item.title.clone(),
                provider: item.provider.clone(),
                auth: item.auth.clone(),
                model: item.model.clone(),
                updated_at: item.updated_at,
                message_count: item.messages.len(),
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        items
    }

    pub fn current(&self, claude: &Chat, openai: &OpenAiChat) -> Result<Option<ChatView>, String> {
        let mut history = self.inner.lock().unwrap();
        let selected = history
            .active_id
            .as_ref()
            .and_then(|id| history.conversations.iter().find(|item| &item.id == id))
            .or_else(|| history.conversations.first())
            .cloned();
        let Some(conversation) = selected else {
            claude.reset();
            openai.reset();
            return Ok(None);
        };
        history.active_id = Some(conversation.id.clone());
        load_backend(&conversation, claude, openai);
        save_locked(&history)?;
        Ok(Some(ChatView::from(&conversation)))
    }

    pub fn create(
        &self,
        provider: String,
        auth: String,
        model: String,
        claude: &Chat,
        openai: &OpenAiChat,
    ) -> Result<ChatView, String> {
        let now = now();
        let conversation = Conversation {
            id: new_id(),
            title: "New chat".into(),
            provider,
            auth,
            model,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            backend_messages: Vec::new(),
        };
        let view = ChatView::from(&conversation);
        let mut history = self.inner.lock().unwrap();
        history.active_id = Some(conversation.id.clone());
        history.conversations.insert(0, conversation);
        history.conversations.truncate(MAX_CONVERSATIONS);
        save_locked(&history)?;
        claude.reset();
        openai.reset();
        Ok(view)
    }

    pub fn open(&self, id: &str, claude: &Chat, openai: &OpenAiChat) -> Result<ChatView, String> {
        let mut history = self.inner.lock().unwrap();
        let conversation = history
            .conversations
            .iter()
            .find(|item| item.id == id)
            .cloned()
            .ok_or_else(|| "Conversation not found.".to_string())?;
        history.active_id = Some(conversation.id.clone());
        save_locked(&history)?;
        load_backend(&conversation, claude, openai);
        Ok(ChatView::from(&conversation))
    }

    pub fn delete(
        &self,
        id: &str,
        claude: &Chat,
        openai: &OpenAiChat,
    ) -> Result<Option<ChatView>, String> {
        let mut history = self.inner.lock().unwrap();
        let was_active = history.active_id.as_deref() == Some(id);
        let before = history.conversations.len();
        history.conversations.retain(|item| item.id != id);
        if history.conversations.len() == before {
            return Err("Conversation not found.".into());
        }
        if was_active {
            history.active_id = history.conversations.first().map(|item| item.id.clone());
        }
        let selected = history
            .active_id
            .as_ref()
            .and_then(|active| history.conversations.iter().find(|item| &item.id == active))
            .cloned();
        save_locked(&history)?;
        match selected {
            Some(conversation) => {
                load_backend(&conversation, claude, openai);
                Ok(Some(ChatView::from(&conversation)))
            }
            None => {
                claude.reset();
                openai.reset();
                Ok(None)
            }
        }
    }

    pub fn config(&self, id: &str) -> Result<ConversationConfig, String> {
        let history = self.inner.lock().unwrap();
        let item = history
            .conversations
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| "Conversation not found.".to_string())?;
        if history.active_id.as_deref() != Some(id) {
            return Err("Open this conversation before sending a message.".into());
        }
        Ok(ConversationConfig {
            provider: item.provider.clone(),
            auth: item.auth.clone(),
            model: item.model.clone(),
        })
    }

    pub fn commit_turn(
        &self,
        id: &str,
        query: String,
        reply: String,
        backend_messages: Vec<Value>,
    ) -> Result<ChatView, String> {
        let mut history = self.inner.lock().unwrap();
        let item = history
            .conversations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "Conversation not found.".to_string())?;
        let base = item.messages.last().map_or(0, |message| message.id);
        if item.messages.is_empty() {
            item.title = title_for(&query);
        }
        item.messages.push(StoredMessage {
            id: base + 1,
            role: "user".into(),
            content: query,
        });
        item.messages.push(StoredMessage {
            id: base + 2,
            role: "assistant".into(),
            content: reply,
        });
        item.backend_messages = backend_messages;
        item.updated_at = now();
        let view = ChatView::from(&*item);
        history.active_id = Some(id.to_string());
        save_locked(&history)?;
        Ok(view)
    }
}

fn load_backend(conversation: &Conversation, claude: &Chat, openai: &OpenAiChat) {
    if conversation.provider == "openai" {
        claude.reset();
        openai.replace(conversation.backend_messages.clone());
    } else {
        openai.reset();
        claude.replace(conversation.backend_messages.clone());
    }
}

fn history_path() -> std::path::PathBuf {
    crate::settings::config_dir().join("chat-history.json")
}

fn save_locked(history: &HistoryFile) -> Result<(), String> {
    let dir = crate::settings::config_dir();
    crate::platform::ensure_private_dir(&dir).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(history).map_err(|error| error.to_string())?;
    std::fs::write(history_path(), bytes).map_err(|error| error.to_string())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_id() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn title_for(query: &str) -> String {
    let clean = query.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = clean.chars();
    let title = chars.by_ref().take(48).collect::<String>();
    if chars.next().is_some() {
        format!("{title}…")
    } else if title.is_empty() {
        "New chat".into()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::title_for;

    #[test]
    fn title_is_compact_and_unicode_safe() {
        assert_eq!(title_for("  hello\n   world  "), "hello world");
        let long = "привет ".repeat(20);
        assert_eq!(title_for(&long).chars().count(), 49);
    }
}
