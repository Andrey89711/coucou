import { Bridge, IS_TAURI } from "./bridge";
import { State, type ChatConversation } from "./state";

function browserConversation(): ChatConversation {
  const provider = State.settings.chatProvider;
  return {
    id: "browser-preview",
    title: "New chat",
    provider,
    auth: State.settings.openaiAuth,
    model: provider === "openai" ? State.settings.openaiModel : State.settings.model,
    createdAt: Date.now() / 1000,
    updatedAt: Date.now() / 1000,
    messages: [],
  };
}

export function applyConversation(conversation: ChatConversation) {
  State.activeConversation = conversation;
  State.chatHistory = [...conversation.messages];
  State.stateOverride = null;
  State.noteMessage = null;
  State.notify();
}

export async function refreshChatList() {
  if (!IS_TAURI) {
    State.conversations = State.activeConversation
      ? [{
          ...State.activeConversation,
          messageCount: State.activeConversation.messages.length,
        }]
      : [];
    State.notify();
    return;
  }
  State.conversations = await Bridge.chatHistoryList();
  State.notify();
}

export async function initializeChatHistory() {
  if (!IS_TAURI) {
    applyConversation(browserConversation());
    await refreshChatList();
    return;
  }
  const current = await Bridge.chatHistoryCurrent();
  applyConversation(current ?? await Bridge.chatHistoryNew());
  await refreshChatList();
}

export async function createNewChat(clearAttachment = true) {
  if (clearAttachment) {
    State.droppedFile = null;
    State.promptContext = null;
  }
  const conversation = IS_TAURI ? await Bridge.chatHistoryNew() : browserConversation();
  applyConversation(conversation);
  State.chatHistoryOpen = false;
  await refreshChatList();
}

export async function openChat(id: string) {
  if (!IS_TAURI || id === State.activeConversation?.id) {
    State.chatHistoryOpen = false;
    State.notify();
    return;
  }
  applyConversation(await Bridge.chatHistoryOpen(id));
  State.droppedFile = null;
  State.promptContext = null;
  State.chatHistoryOpen = false;
  await refreshChatList();
}

export async function deleteChat(id: string) {
  if (!IS_TAURI) return;
  const selected = await Bridge.chatHistoryDelete(id);
  if (selected) {
    applyConversation(selected);
  } else {
    applyConversation(await Bridge.chatHistoryNew());
  }
  await refreshChatList();
}
