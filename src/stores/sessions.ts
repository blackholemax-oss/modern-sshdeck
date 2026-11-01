/**
 * 会话与终端布局状态。
 *
 * 设计要点：终端输出**不**进入 React 状态，直接由 xterm 命令式写入（见 features/terminal）。
 * 这里只存"哪些标签、每个标签怎么分屏"这类低频结构状态。
 */
import { create } from "zustand";

import type { SessionInfo } from "@/shared/ipc";

export type PaneLayout = "single" | "split-h" | "split-v" | "quad";

export interface TabState {
  id: string;
  title: string;
  /** 每个 pane 对应一个 sessionId；null 表示空位 */
  panes: (string | null)[];
  layout: PaneLayout;
}

export interface SessionsState {
  tabs: TabState[];
  activeTabId: string | null;
  sessions: Record<string, SessionInfo>;

  addTab: (sessionId: string, title: string) => void;
  closeTab: (tabId: string) => void;
  setActiveTab: (tabId: string) => void;
  setLayout: (tabId: string, layout: PaneLayout) => void;
  setPaneSession: (tabId: string, paneIndex: number, sessionId: string | null) => void;
  upsertSession: (info: SessionInfo) => void;
  removeSession: (sessionId: string) => void;
}

export const useSessionsStore = create<SessionsState>((set) => ({
  tabs: [],
  activeTabId: null,
  sessions: {},

  addTab: (sessionId, title) =>
    set((s) => {
      const id = crypto.randomUUID();
      const tab: TabState = { id, title, panes: [sessionId, null, null, null], layout: "single" };
      return { tabs: [...s.tabs, tab], activeTabId: id };
    }),

  closeTab: (tabId) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== tabId);
      const activeTabId =
        s.activeTabId === tabId ? (tabs.length ? tabs[tabs.length - 1].id : null) : s.activeTabId;
      return { tabs, activeTabId };
    }),

  setActiveTab: (tabId) => set({ activeTabId: tabId }),

  setLayout: (tabId, layout) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, layout } : t)),
    })),

  setPaneSession: (tabId, paneIndex, sessionId) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.id !== tabId) return t;
        const panes = [...t.panes];
        panes[paneIndex] = sessionId;
        return { ...t, panes };
      }),
    })),

  upsertSession: (info) =>
    set((s) => ({ sessions: { ...s.sessions, [info.id]: info } })),

  removeSession: (sessionId) =>
    set((s) => {
      const sessions = { ...s.sessions };
      delete sessions[sessionId];
      const tabs = s.tabs.map((t) => ({
        ...t,
        panes: t.panes.map((p) => (p === sessionId ? null : p)),
      }));
      return { sessions, tabs };
    }),
}));
