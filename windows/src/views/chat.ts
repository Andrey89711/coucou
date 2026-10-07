// Local multi-conversation chat UI. Conversation metadata and messages are
// persisted by Rust; the webview only mirrors the currently open thread.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type ChatContext } from "../core/bridge";
import {
  applyConversation,
  createNewChat,
  deleteChat,
  openChat,
  refreshChatList,
} from "../core/chat-history";
import { Sound } from "../core/sound";
import { State, type ChatMessage, type ChatSummary } from "../core/state";
import type { ViewHost } from "./views";

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h("div", { class: "chat-row user" }, h("div", { class: "bubble", text: message.content }));
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

function typingDots(): HTMLElement {
  return h("div", { class: "chat-row" }, h("div", { class: "typing" }, h("i"), h("i"), h("i")));
}

function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

function providerLabel(
  item: Pick<ChatSummary, "provider" | "auth"> | null = State.activeConversation,
): string {
  if (!item) return "AI";
  if (item.provider === "claude") return "Claude";
  return item.auth === "chatgpt" ? "ChatGPT" : "OpenAI";
}

function historyRow(item: ChatSummary, disabled: boolean): HTMLElement {
  const remove = h("button", {
    class: "chat-history-delete",
    title: "Delete conversation",
    "aria-label": "Delete conversation",
    disabled: disabled ? "true" : undefined,
  });
  remove.addEventListener("click", (event) => {
    event.stopPropagation();
    if (!disabled) void deleteChat(item.id);
  });

  const row = h(
    "div",
    {
      class: `chat-history-row${item.id === State.activeConversation?.id ? " active" : ""}`,
      title: item.title,
      role: "button",
      tabindex: "0",
    },
    h(
      "span",
      { class: "chat-history-copy" },
      h("strong", { text: item.title }),
      h("small", { text: `${providerLabel(item)} · ${item.model}` }),
    ),
    remove,
  );
  row.addEventListener("click", () => {
    if (!disabled) void openChat(item.id);
  });
  return row;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const historyPanel = h("div", { class: "chat-history-panel" });
  const thread = h("div", { class: "chat-thread" }, chipRow, log);
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const historyButton = h("button", {
    class: "chat-tool chat-history-toggle",
    title: "Past conversations",
    text: "☰",
  });
  const title = h("div", { class: "chat-title", text: "New chat" });
  const add = h("button", { class: "chat-tool", title: "New chat", text: "+" });
  const expand = h("button", {
    class: "chat-tool chat-expand",
    title: "Expand chat",
    text: "↕",
  });
  const toolbar = h("div", { class: "chat-toolbar" }, historyButton, title, add, expand);
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, toolbar, historyPanel, thread, bar)),
  );
  const card = el.querySelector(".card") as HTMLElement;

  let sending = false;
  let renderedMessages = "";
  let renderedHistory = "";

  async function runToolbarAction(action: () => Promise<void>) {
    if (sending) return;
    try {
      await action();
      onHeightChange();
    } catch (error) {
      State.noteMessage = String(error).replace(/^Error:\s*/, "");
      State.view = "note";
      State.notify();
    }
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    if (!State.activeConversation) await createNewChat(false);
    const conversationId = State.activeConversation?.id;
    if (!conversationId) return;

    input.value = "";
    sending = true;
    Sound.play("send");

    const previous = [...State.chatHistory];
    const file = State.droppedFile;
    const context: ChatContext | null =
      previous.length === 0 && file ? { kind: "file", name: file.name, path: file.path } : null;
    State.chatHistory.push({ id: Date.now(), role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    try {
      const reply = await Bridge.chatSend(conversationId, query, context);
      applyConversation(reply.conversation);
      await refreshChatList();
      Sound.play("finish");
    } catch (error) {
      State.chatHistory = previous;
      State.stateOverride = null;
      State.noteMessage = String(error).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (event) => {
    if ((event as KeyboardEvent).key === "Enter") {
      event.preventDefault();
      void submit();
    }
    event.stopPropagation();
  });
  add.addEventListener("click", () => void runToolbarAction(() => createNewChat()));
  historyButton.addEventListener("click", () => {
    if (sending) return;
    State.chatHistoryOpen = !State.chatHistoryOpen;
    if (State.chatHistoryOpen) State.chatExpanded = true;
    State.notify();
    onHeightChange();
  });
  expand.addEventListener("click", () => {
    State.chatExpanded = !State.chatExpanded;
    State.notify();
    onHeightChange();
  });

  return {
    el,
    sync() {
      const active = State.activeConversation;
      const provider = active?.provider ?? State.settings.chatProvider;
      const label = providerLabel();
      card.style.setProperty(
        "--wash",
        provider === "openai" ? "rgba(16,163,127,0.46)" : "rgba(99,102,241,0.5)",
      );
      title.textContent = active?.title ?? "New chat";
      expand.classList.toggle("on", State.chatExpanded);
      expand.title = State.chatExpanded ? "Compact chat" : "Expand chat";
      historyButton.classList.toggle("on", State.chatHistoryOpen);
      thread.hidden = State.chatHistoryOpen;
      historyPanel.hidden = !State.chatHistoryOpen;
      add.toggleAttribute("disabled", sending);
      historyButton.toggleAttribute("disabled", sending);
      expand.toggleAttribute("disabled", sending);

      const file = State.droppedFile;
      const wantChip = [label, active?.model, file?.name].filter(Boolean).join(" · ");
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const thinking = State.stateOverride === "thinking";
      const messageKey = `${active?.id ?? "none"}:${thinking}:${State.chatHistory
        .map((message) => `${message.id}:${message.content.length}`)
        .join(",")}`;
      if (messageKey !== renderedMessages) {
        renderedMessages = messageKey;
        clear(log);
        for (const message of State.chatHistory) log.append(bubble(message));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      const historyKey = `${State.chatHistoryOpen}:${active?.id}:${sending}:${State.conversations
        .map((item) => `${item.id}:${item.title}:${item.updatedAt}`)
        .join(",")}`;
      if (historyKey !== renderedHistory) {
        renderedHistory = historyKey;
        clear(historyPanel);
        if (State.conversations.length === 0) {
          historyPanel.append(h("div", { class: "chat-history-empty", text: "No saved conversations" }));
        } else {
          for (const item of State.conversations) historyPanel.append(historyRow(item, sending));
        }
      }

      input.placeholder = State.chatHistory.length === 0 ? `Ask ${label} anything…` : "Continue…";
      input.disabled = sending;
      send.toggleAttribute("disabled", sending);
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
