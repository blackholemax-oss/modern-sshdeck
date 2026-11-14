/**
 * 中栏终端工作区：标签栏 + 分屏 + 状态订阅。
 *
 * 订阅策略：这里**不**订阅 `terminal:data`（输出由每个 pane 自行定向订阅）。
 * 只订阅低频的 `session:state` / `terminal:closed` 来维护标签页的状态点。
 */
import { useEffect, useMemo, useState } from "react";
import {
  Columns2,
  Grid2x2,
  Plus,
  Rows2,
  Square,
  X,
  Circle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { api, events } from "@/shared/api";
import type { SessionInfo, SessionState } from "@/shared/ipc";
import { resolveTheme, useSettingsStore } from "@/stores/settings";
import { useSessionsStore, type PaneLayout } from "@/stores/sessions";

import { useConnectIntent } from "@/features/connections/connectIntent";

import { LAYOUT_GRID, LAYOUT_LABEL, LAYOUTS, PANE_COUNT } from "./layout";
import { TerminalPane } from "./TerminalPane";

import { useTranslation } from "react-i18next";

const LAYOUT_ICON: Record<PaneLayout, typeof Square> = {
  single: Square,
  "split-h": Columns2,
  "split-v": Rows2,
  quad: Grid2x2,
};

const STATE_DOT: Record<SessionState, string> = {
  connecting: "bg-amber-500 animate-pulse",
  auth: "bg-amber-500 animate-pulse",
  connected: "bg-emerald-500",
  closed: "bg-neutral-500",
  failed: "bg-red-500",
};

const STATE_LABEL: Record<SessionState, string> = {
  connecting: "连接中",
  auth: "认证中",
  connected: "已连接",
  closed: "已断开",
  failed: "失败",
};

export function TerminalWorkspace() {
  const { t } = useTranslation();
  const tabs = useSessionsStore((s) => s.tabs);
  const activeTabId = useSessionsStore((s) => s.activeTabId);
  const sessions = useSessionsStore((s) => s.sessions);
  const setActiveTab = useSessionsStore((s) => s.setActiveTab);
  const closeTab = useSessionsStore((s) => s.closeTab);
  const setLayout = useSessionsStore((s) => s.setLayout);
  const removeSession = useSessionsStore((s) => s.removeSession);

  // 终端区底色跟随主题，避免浅色界面下露出一块黑底
  const themePref = useSettingsStore((s) => s.config?.ui.theme) ?? "dark";
  const resolvedTheme = resolveTheme(themePref);

  const [activePane, setActivePane] = useState(0);

  const activeTab = useMemo(
    () => tabs.find((t) => t.id === activeTabId) ?? null,
    [tabs, activeTabId],
  );

  // 低频状态订阅：会话状态迁移 → 更新 store（标签状态点由 store 驱动）。
  useEffect(() => {
    let disposed = false;
    let unlistenState: (() => void) | undefined;
    let unlistenClosed: (() => void) | undefined;

    void (async () => {
      const offState = await events.onSessionState((payload) => {
        const prev = useSessionsStore.getState().sessions[payload.sessionId];
        const info: SessionInfo = {
          id: payload.sessionId,
          host: prev?.host ?? "",
          port: prev?.port ?? 0,
          username: prev?.username ?? "",
          state: payload.state,
          createdAt: prev?.createdAt ?? Date.now(),
          hostKeyFingerprint: prev?.hostKeyFingerprint,
          hostKeyStatus: prev?.hostKeyStatus,
          linkId: prev?.linkId,
        };
        useSessionsStore.getState().upsertSession(info);
      });
      if (disposed) {
        offState();
        return;
      }
      unlistenState = offState;

      const offClosed = await events.onTerminalClosed((payload) => {
        const prev = useSessionsStore.getState().sessions[payload.sessionId];
        if (prev) {
          useSessionsStore.getState().upsertSession({ ...prev, state: "closed" });
        }
      });
      if (disposed) {
        offClosed();
        return;
      }
      unlistenClosed = offClosed;
    })();

    return () => {
      disposed = true;
      unlistenState?.();
      unlistenClosed?.();
    };
  }, []);

  // 关闭标签：先断开后端会话，再移除本地状态。
  const handleCloseTab = (tabId: string) => {
    const tab = useSessionsStore.getState().tabs.find((t) => t.id === tabId);
    if (tab) {
      for (const sid of tab.panes) {
        if (!sid) continue;
        void api
          .disconnect(sid)
          .catch(() => undefined)
          .finally(() => removeSession(sid));
      }
    }
    closeTab(tabId);
  };

  const handleLayout = (layout: PaneLayout) => {
    if (!activeTab) return;
    setLayout(activeTab.id, layout);
    setActivePane(0);
  };

  const paneCount = activeTab ? PANE_COUNT[activeTab.layout] : 0;
  const panes = activeTab ? activeTab.panes.slice(0, paneCount) : [];

  return (
    <div className="flex h-full min-h-0 w-full flex-col">
      {/* 标签栏 */}
      <div className="flex shrink-0 items-stretch gap-1 overflow-x-auto border-b border-border px-1 pt-1">
        {tabs.length === 0 ? (
          <span className="flex h-8 items-center px-2 text-xs text-muted-foreground">
            {t("terminal.noSession")}
          </span>
        ) : null}

        {tabs.map((tab) => {
          const isActive = tab.id === activeTabId;
          const primary = tab.panes.find((p): p is string => p !== null);
          const state = primary
            ? (sessions[primary]?.state ?? "connecting")
            : "closed";
          return (
            <div
              key={tab.id}
              className={cn(
                "group flex h-8 max-w-56 shrink-0 cursor-pointer items-center gap-1.5 rounded-t-md border border-b-0 px-2 text-xs transition-colors",
                isActive
                  ? "border-border bg-card text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-accent/50",
              )}
              onClick={() => {
                setActiveTab(tab.id);
                setActivePane(0);
              }}
              title={`${tab.title} — ${STATE_LABEL[state]}`}
            >
              <span className={cn("size-1.5 shrink-0 rounded-full", STATE_DOT[state])} />
              <span className="truncate">{tab.title}</span>
              <button
                type="button"
                aria-label={t("terminal.closeTab", { title: tab.title })}
                className="ml-0.5 rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:bg-destructive/20 hover:text-destructive group-hover:opacity-100"
                onClick={(e) => {
                  e.stopPropagation();
                  handleCloseTab(tab.id);
                }}
              >
                <X className="size-3" />
              </button>
            </div>
          );
        })}
      </div>

      {/* 工具栏：分屏切换 */}
      <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-2 py-1">
        <div className="flex items-center gap-0.5">
          {LAYOUTS.map((layout) => {
            const Icon = LAYOUT_ICON[layout];
            return (
              <Button
                key={layout}
                variant={activeTab?.layout === layout ? "secondary" : "ghost"}
                size="sm"
                className="h-7 px-2"
                disabled={!activeTab}
                title={`${LAYOUT_LABEL[layout]}（${PANE_COUNT[layout]} 屏，空位可{t("terminal.clickToConnect")}）`}
                onClick={() => handleLayout(layout)}
              >
                <Icon className="size-3.5" />
              </Button>
            );
          })}
        </div>

        <div className="flex items-center gap-2 text-[11px] text-muted-foreground">
          {activeTab ? (
            <span>
              {LAYOUT_LABEL[activeTab.layout]} · {activeTab.panes.filter(Boolean).length}/{PANE_COUNT[activeTab.layout]} 会话
            </span>
          ) : null}
        </div>
      </div>

      {/* 分屏区 */}
      <div
        className="min-h-0 flex-1 transition-colors"
        style={{ background: resolvedTheme === "light" ? "#fafafa" : "#1e1e1e" }}
      >
        {!activeTab ? (
          <div className="flex h-full flex-col items-center justify-center gap-1 text-neutral-500">
            <Circle className="size-5 opacity-40" />
            <span className="text-sm">{t("terminal.clickToConnect")}</span>
            <span className="text-xs opacity-60">
              <Plus className="mb-0.5 inline size-3" /> {t("terminal.selectHostFirst")}
            </span>
          </div>
        ) : (
          <div className={cn("grid h-full min-h-0 w-full grid-rows-1", LAYOUT_GRID[activeTab.layout])}>
            {panes.map((sid, index) => (
              <div
                key={`${activeTab.id}-pane-${index}`}
                className="relative min-h-0 min-w-0"
                onMouseDown={() => setActivePane(index)}
              >
                <TerminalPane
                  sessionId={sid}
                  session={sid ? sessions[sid] : undefined}
                  active={activePane === index}
                  onRequestConnect={() =>
                    useConnectIntent.getState().requestInto({
                      tabId: activeTab.id,
                      paneIndex: index,
                    })
                  }
                />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
