import { useEffect, useState } from "react";
import { Settings as SettingsIcon } from "lucide-react";

import { ConnectionsSidebar } from "@/features/connections/ConnectionsSidebar";
import { FileBrowserPanel } from "@/features/files/FileBrowserPanel";
import { ForwardPanel } from "@/features/forward/ForwardPanel";
import { TerminalWorkspace } from "@/features/terminal/TerminalWorkspace";
import { AgentPanel } from "@/features/agent/AgentPanel";
import { SettingsPanel } from "@/features/settings/SettingsPanel";
import { TitleBar } from "@/components/TitleBar";
import { Button } from "@/components/ui/button";
import { api } from "@/shared/api";
import { useConnectionStore } from "@/stores/connection";
import { useSessionsStore } from "@/stores/sessions";
import { useTranslation } from "react-i18next";

import { resolveLang, setLanguage, setupI18n } from "@/i18n";
import { applyThemeClass, resolveTheme, useSettingsStore } from "@/stores/settings";

export function App() {
  const { t } = useTranslation();
  // 当前激活的会话 id（文件面板绑定在它上面）
  const activeTabId = useSessionsStore((s) => s.activeTabId);
  const tabs = useSessionsStore((s) => s.tabs);
  const activeSessionId =
    tabs.find((tab) => tab.id === activeTabId)?.panes.find(Boolean) ?? null;
  const setHosts = useConnectionStore((s) => s.setHosts);
  const setConfig = useConnectionStore((s) => s.setConfig);

  const settings = useSettingsStore((s) => s.config);
  const setSettings = useSettingsStore((s) => s.setConfig);

  const [settingsOpen, setSettingsOpen] = useState(false);

  // 启动引导：拉主机列表与配置，同时把配置镜像进 settings store。
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [hosts, config] = await Promise.all([api.listHosts(), api.getConfig()]);
        if (cancelled) return;
        setHosts(hosts);
        setConfig(config);
        setSettings(config);
      } catch (err) {
        console.error("bootstrap failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [setHosts, setConfig, setSettings]);

  // 主题：把偏好解析成 light/dark 并写到 <html> 的 class 上。
  const themePref = settings?.ui.theme ?? "dark";
  useEffect(() => {
    const apply = () => applyThemeClass(resolveTheme(themePref));
    apply();

    // 跟随系统：监听系统配色变化
    if (themePref === "system" && window.matchMedia) {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      const onChange = () => apply();
      mq.addEventListener("change", onChange);
      return () => mq.removeEventListener("change", onChange);
    }
    return undefined;
  }, [themePref]);

  // 语言：`ui.language` 三态（system / zh / en）。
  // `system` 时按系统语言解析，因此首次启动就与系统一致。
  const langPref = settings?.ui.language ?? "system";
  useEffect(() => {
    const lang = resolveLang(langPref);
    setupI18n(lang);
    void setLanguage(lang);
  }, [langPref]);

  const sidebarWidth = settings?.ui.sidebarWidth ?? 240;
  const agentWidth = settings?.ui.agentPanelWidth ?? 384;
  const showAgent = settings?.ui.showAgentPanel ?? true;

  return (
    <div
      id="app-shell"
      className="flex h-full w-full flex-col overflow-hidden bg-background text-foreground"
    >
      {/* 自定义标题栏：颜色取自主题变量，随主题切换自动更新 */}
      <TitleBar />

      {/* 内容区 */}
      <div className="flex min-h-0 flex-1 overflow-hidden">
      {/* 左栏：主机 / 转发 / 设置入口 */}
      <aside
        className="flex shrink-0 flex-col border-r border-border"
        style={{ width: sidebarWidth }}
      >
        <ConnectionsSidebar />
        <div className="border-t border-border">
          <ForwardPanel />
        </div>
        {/* 远程文件：只有绑定了会话时才出现（没有会话时 SFTP 无从谈起） */}
        {activeSessionId ? (
          <div className="flex min-h-0 flex-1 flex-col border-t border-border">
            <div className="shrink-0 px-2 py-1.5 text-[11px] font-medium text-muted-foreground">
              {t("files.title")}
            </div>
            <FileBrowserPanel sessionId={activeSessionId} />
          </div>
        ) : null}
        <div className="mt-auto border-t border-border p-2">
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-start gap-2 text-muted-foreground hover:text-foreground"
            onClick={() => setSettingsOpen(true)}
          >
            <SettingsIcon className="size-4" />
            {t("sidebar.settings")}
          </Button>
        </div>
      </aside>

      {/* 中栏：终端工作区 */}
      <main className="min-w-0 flex-1">
        <TerminalWorkspace />
      </main>

      {/* 右栏：Agent（可被设置隐藏） */}
      {showAgent ? (
        <aside
          className="flex shrink-0 flex-col border-l border-border"
          style={{ width: agentWidth }}
        >
          <AgentPanel />
        </aside>
      ) : null}

      </div>

      <SettingsPanel open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </div>
  );
}
