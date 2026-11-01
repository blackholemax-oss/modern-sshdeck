/**
 * 设置（偏好）状态与主题应用。
 *
 * 设计原则：
 * - 配置的唯一真源是后端 `AppConfig`（落盘在 `state.json`）；这里只做前端镜像，
 *   任何修改都先 patch 后端、成功后用后端返回的完整配置覆盖本地镜像，
 *   避免"前端改了但没存"的不一致。
 * - 写入有节流：拖滑块/连续输入时不会每帧打一次 IPC。
 */
import { create } from "zustand";

import { api, SshdeckError } from "@/shared/api";
import type { AppConfig, AppConfigPatch } from "@/shared/ipc";

export type ThemePreference = AppConfig["ui"]["theme"];

interface SettingsState {
  config: AppConfig | null;
  saving: boolean;
  /** 最近一次保存失败的错误信息（nil 表示无错误） */
  error: string | null;

  setConfig: (config: AppConfig) => void;
  /** 立即写后端并同步本地镜像 */
  patch: (patch: AppConfigPatch) => Promise<void>;
  clearError: () => void;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  config: null,
  saving: false,
  error: null,

  setConfig: (config) => set({ config }),

  patch: async (patch) => {
    set({ saving: true, error: null });
    try {
      const updated = await api.setConfig(patch);
      set({ config: updated, saving: false });
    } catch (err) {
      const message =
        err instanceof SshdeckError
          ? `${err.code}: ${err.message}`
          : err instanceof Error
            ? err.message
            : String(err);
      set({ saving: false, error: message });
      // 保存失败时重新拉一次，保证界面回到后端的真实状态
      try {
        const actual = await api.getConfig();
        set({ config: actual });
      } catch {
        /* ignore */
      }
    }
    void get;
  },

  clearError: () => set({ error: null }),
}));

/** 把用户选择的主题偏好解析成实际生效的 light / dark。 */
export function resolveTheme(pref: ThemePreference): "light" | "dark" {
  if (pref === "system") {
    const prefersDark =
      typeof window !== "undefined" &&
      window.matchMedia?.("(prefers-color-scheme: dark)").matches;
    return prefersDark ? "dark" : "light";
  }
  return pref;
}

/** 把解析后的主题写到 `<html>` 的 class 上（Tailwind v4 的 dark 变体依赖它）。 */
export function applyThemeClass(theme: "light" | "dark") {
  const root = document.documentElement;
  root.classList.toggle("dark", theme === "dark");
  root.style.colorScheme = theme;
}
