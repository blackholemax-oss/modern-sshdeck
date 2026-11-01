import { useEffect, useState } from "react";
import { Settings as SettingsIcon } from "lucide-react";

import { ConnectionsSidebar } from "@/features/connections/ConnectionsSidebar";
import { ForwardPanel } from "@/features/forward/ForwardPanel";
import { TerminalWorkspace } from "@/features/terminal/TerminalWorkspace";
import { AgentPanel } from "@/features/agent/AgentPanel";
import { SettingsPanel } from "@/features/settings/SettingsPanel";
import { Button } from "@/components/ui/button";
import { api } from "@/shared/api";
import { useConnectionStore } from "@/stores/connection";
import { applyThemeClass, resolveTheme, useSettingsStore } from "@/stores/settings";

export function App() {
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

  const sidebarWidth = settings?.ui.sidebarWidth ?? 240;
  const agentWidth = settings?.ui.agentPanelWidth ?? 384;
  const showAgent = settings?.ui.showAgentPanel ?? true;

  return (
    <div className="flex h-full w-full overflow-hidden bg-background text-foreground">
      {/* 左栏：主机 / 转发 / 设置入口 */}
      <aside
        className="flex shrink-0 flex-col border-r border-border"
        style={{ width: sidebarWidth }}
      >
        <ConnectionsSidebar />
        <div className="border-t border-border">
          <ForwardPanel />
        </div>
        <div className="mt-auto border-t border-border p-2">
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-start gap-2 text-muted-foreground hover:text-foreground"
            onClick={() => setSettingsOpen(true)}
          >
            <SettingsIcon className="size-4" />
            设置
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

      <SettingsPanel open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </div>
  );
}
