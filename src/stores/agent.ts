import { create } from "zustand";

import { api } from "@/shared/api";
import type { AgentApprovalRequiredEvent, Risk } from "@/shared/ipc";

export interface ToolCallView {
  callId: string;
  name: string;
  args: string;
  output?: string;
  ok?: boolean;
}

export interface AgentMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  toolCalls: ToolCallView[];
  pending?: boolean;
}

interface AgentStoreState {
  messages: AgentMessage[];
  streaming: boolean;
  approval: AgentApprovalRequiredEvent | null;
  error: string | null;

  appendUser: (text: string) => void;
  startAssistant: () => string;
  appendDelta: (delta: string) => void;
  addToolCall: (callId: string, name: string, args: string) => void;
  setToolResult: (callId: string, ok: boolean, output: string) => void;
  finishAssistant: () => void;
  setApproval: (a: AgentApprovalRequiredEvent | null) => void;
  setError: (e: string | null) => void;
  reset: () => void;
  /** 把审批结论发给后端。 */
  sendApproval: (approvalId: string, approved: boolean, confirmPhrase?: string) => Promise<void>;
}

export const useAgentStore = create<AgentStoreState>((set) => ({
  messages: [],
  streaming: false,
  approval: null,
  error: null,

  appendUser: (text) =>
    set((s) => ({
      messages: [
        ...s.messages,
        { id: crypto.randomUUID(), role: "user", content: text, toolCalls: [] },
      ],
    })),

  startAssistant: () => {
    const id = crypto.randomUUID();
    set((s) => ({
      streaming: true,
      messages: [
        ...s.messages,
        { id, role: "assistant", content: "", toolCalls: [], pending: true },
      ],
    }));
    return id;
  },

  appendDelta: (delta) =>
    set((s) => {
      const messages = [...s.messages];
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "assistant" && messages[i].pending) {
          messages[i] = { ...messages[i], content: messages[i].content + delta };
          break;
        }
      }
      return { messages };
    }),

  addToolCall: (callId, name, args) =>
    set((s) => {
      const messages = [...s.messages];
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "assistant" && messages[i].pending) {
          messages[i] = {
            ...messages[i],
            toolCalls: [...messages[i].toolCalls, { callId, name, args }],
          };
          break;
        }
      }
      return { messages };
    }),

  setToolResult: (callId, ok, output) =>
    set((s) => {
      const messages = [...s.messages];
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "assistant" && messages[i].pending) {
          messages[i] = {
            ...messages[i],
            toolCalls: messages[i].toolCalls.map((t) =>
              t.callId === callId ? { ...t, ok, output } : t,
            ),
          };
          break;
        }
      }
      return { messages };
    }),

  finishAssistant: () =>
    set((s) => ({
      streaming: false,
      messages: s.messages.map((m) =>
        m.pending ? { ...m, pending: false } : m,
      ),
    })),

  setApproval: (approval) => set({ approval }),
  setError: (error) => set({ error }),
  reset: () => set({ messages: [], streaming: false, approval: null, error: null }),

  sendApproval: async (approvalId, approved, confirmPhrase) => {
    // 高危命令必须带精确短语「确认执行」，后端会再校验一次；
    // 前端这里只是把用户的输入原样传过去，不做任何放宽。
    await api.agentApprove({ approvalId, approved, confirmPhrase });
  },
}));

/** 风险等级 → 中文标签。 */
export function riskLabel(risk: Risk): string {
  switch (risk) {
    case "read_only":
      return "只读";
    case "mutating":
      return "变更";
    case "dangerous":
      return "高危";
  }
}
