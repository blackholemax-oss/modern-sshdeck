/**
 * 连接意图（跨组件、但不进 React state 的高频通道）。
 *
 * 三个消费方：
 * - `TerminalPane` 的空位被点击 → `requestInto({tabId, paneIndex})`，侧栏据此展开表单；
 * - `ConnectionsSidebar` 连接成功后读 `target`：有目标就填进那个 pane，没目标就新开标签；
 * - 连接结束后 `clear()`，避免下一次连接误填。
 */
import { create } from "zustand";

export interface PaneTarget {
  tabId: string;
  paneIndex: number;
}

interface ConnectIntentState {
  /** 等待填充的空位；null 表示「连接后新开标签」。 */
  target: PaneTarget | null;
  /** 侧栏新建连接表单是否展开。 */
  formOpen: boolean;
  requestInto: (target: PaneTarget) => void;
  setFormOpen: (open: boolean) => void;
  clear: () => void;
}

export const useConnectIntent = create<ConnectIntentState>((set) => ({
  target: null,
  formOpen: false,
  requestInto: (target) => set({ target, formOpen: true }),
  setFormOpen: (formOpen) => set({ formOpen }),
  clear: () => set({ target: null }),
}));
