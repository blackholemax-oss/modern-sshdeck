/**
 * 设置面板（右侧滑出抽屉）。
 *
 * 分区：
 * - 外观：主题（深色 / 浅色 / 跟随系统）、界面语言、左栏与 Agent 栏宽度
 * - 终端：字体族、字号、回滚行数、光标样式与闪烁 + 实时预览
 * - 行为：关闭标签确认、断线提示音、自动重连次数、新建连接默认值
 *
 * 所有修改都走 `useSettingsStore.patch`，先写后端再回填，保证落盘与界面一致。
 */
import { useEffect, useState, type ReactNode } from "react";
import { X, RotateCcw, Monitor, Moon, RefreshCw, Sun, Type } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { api } from "@/shared/api";
import type { AppConfigPatch, PoolStats } from "@/shared/ipc";
import { useTranslation } from "react-i18next";

import { resolveTheme, useSettingsStore } from "@/stores/settings";

import { ModelPanel } from "./ModelPanel";
import { TokenPanel } from "./TokenPanel";

/** 预设字体栈（都含 Nerd Font 变体，保证远端提示符图标能显示）。 */
const FONT_PRESETS: { label?: string; labelKey?: string; value: string }[] = [
  { labelKey: "settings.terminal.fontBuiltin", value: "" },
  {
    label: "Cascadia Code",
    value: '"Cascadia Code", "Cascadia Mono", "Consolas", monospace',
  },
  {
    label: "JetBrains Mono / Nerd Font",
    value: '"JetBrainsMono Nerd Font", "JetBrains Mono", "Consolas", monospace',
  },
  {
    label: "Fira Code / Nerd Font",
    value: '"FiraCode Nerd Font", "Fira Code", "Consolas", monospace',
  },
  {
    labelKey: "settings.terminal.fontMeslo",
    value: '"MesloLGS NF", "Consolas", monospace',
  },
  {
    labelKey: "settings.terminal.fontConsolas",
    value: '"Consolas", monospace',
  },
];

const CURSOR_STYLES: { label?: string; labelKey?: string; value: string }[] = [
  { labelKey: "settings.terminal.cursorBlock", value: "block" },
  { labelKey: "settings.terminal.cursorBar", value: "bar" },
  { labelKey: "settings.terminal.cursorUnderline", value: "underline" },
];

interface SettingsPanelProps {
  open: boolean;
  onClose: () => void;
}

export function SettingsPanel({ open, onClose }: SettingsPanelProps) {
  const config = useSettingsStore((s) => s.config);
  const patch = useSettingsStore((s) => s.patch);
  const { t } = useTranslation();
  const saving = useSettingsStore((s) => s.saving);
  const error = useSettingsStore((s) => s.error);
  const clearError = useSettingsStore((s) => s.clearError);

  // 字号滑块的本地即时值：拖动时先本地响应，松开/停止后再写后端。
  const [fontSizeDraft, setFontSizeDraft] = useState<number | null>(null);

  // 说明：模型接入（原「渠道」+「Agent」）已抽到 `ModelPanel`，
  // 因此本文件不再持有 Key / 模型列表 / 探测状态。

  if (!open) return null;

  const ui = config?.ui;
  const term = config?.terminal;
  const behavior = config?.behavior;

  const patchTerminal = (p: AppConfigPatch["terminal"]) =>
    void patch({ terminal: p });
  const patchUi = (p: AppConfigPatch["ui"]) => void patch({ ui: p });
  const patchBehavior = (p: AppConfigPatch["behavior"]) =>
    void patch({ behavior: p });

  const fontSize = fontSizeDraft ?? term?.fontSize ?? 14;
  const fontFamily = term?.fontFamily ?? "";

  return (
    <div className="fixed inset-0 z-40 flex justify-end">
      {/* 遮罩：点击关闭 */}
      <button
        type="button"
        aria-label={t("settings.title")}
        className="absolute inset-0 bg-black/50"
        onClick={onClose}
      />

      <aside className="relative z-50 flex h-full w-[580px] flex-col border-l border-border bg-background shadow-xl">
        <header className="flex items-center justify-between border-b border-border px-5 py-3.5">
          <div>
            <h2 className="text-sm font-semibold">{t("settings.title")}</h2>
            <p className="mt-0.5 text-[11px] text-muted-foreground">
              {t("settings.subtitle")}
              {saving ? ` · ${t("common.saving")}` : ""}
            </p>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="sm"
              title={t("settings.resetHint")}
              onClick={() =>
                void patch({
                  terminal: {
                    fontSize: 14,
                    scrollback: 5000,
                    fontFamily: "",
                    cursorStyle: "block",
                    cursorBlink: true,
                  },
                  ui: {
                    theme: "dark",
                    language: "zh",
                    sidebarWidth: 240,
                    agentPanelWidth: 384,
                    showAgentPanel: true,
                  },
                  behavior: {
                    confirmOnCloseTab: true,
                    bellOnDisconnect: true,
                    autoReconnectAttempts: 0,
                    defaultPort: 22,
                    defaultUsername: "",
                    hostKeyPolicy: "strict",
                    knownHostsPath: "",
                    sessionReuse: true,
                    sessionReuseIdleSecs: 300,
                    forwardAutoRestore: true,
                    forwardReconnectMaxSecs: 30,
                  },
                })
              }
            >
              <RotateCcw className="size-3.5" />
              {t("common.reset")}
            </Button>
            <Button variant="ghost" size="icon" onClick={onClose}>
              <X className="size-4" />
            </Button>
          </div>
        </header>

        {error ? (
          <div className="mx-5 mt-3 flex items-start justify-between gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
            <span>{t("settings.saveFailed", { error })}</span>
            <button
              type="button"
              className="shrink-0 opacity-70 hover:opacity-100"
              onClick={clearError}
            >
              <X className="size-3.5" />
            </button>
          </div>
        ) : null}

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          <Tabs defaultValue="appearance">
            <TabsList className="w-full">
              <TabsTrigger value="appearance" className="flex-1">
                {t("settings.tabs.appearance")}
              </TabsTrigger>
              <TabsTrigger value="terminal" className="flex-1">
                {t("settings.tabs.terminal")}
              </TabsTrigger>
              <TabsTrigger value="behavior" className="flex-1">
                {t("settings.tabs.behavior")}
              </TabsTrigger>
              <TabsTrigger value="model" className="flex-1">
                {t("settings.tabs.model")}
              </TabsTrigger>
              <TabsTrigger value="token" className="flex-1">
                {t("settings.tabs.token")}
              </TabsTrigger>
            </TabsList>

            {/* ───────────── 外观 ───────────── */}
            <TabsContent value="appearance" className="mt-4 space-y-5">
              <Section title={t("settings.appearance.theme")}>
                <div className="grid grid-cols-3 gap-2">
                  <ThemeCard
                    icon={<Moon className="size-4" />}
                    label={t("theme.dark")}
                    active={ui?.theme === "dark"}
                    onClick={() => patchUi({ theme: "dark" })}
                  />
                  <ThemeCard
                    icon={<Sun className="size-4" />}
                    label={t("theme.light")}
                    active={ui?.theme === "light"}
                    onClick={() => patchUi({ theme: "light" })}
                  />
                  <ThemeCard
                    icon={<Monitor className="size-4" />}
                    label={t("theme.system")}
                    active={ui?.theme === "system"}
                    onClick={() => patchUi({ theme: "system" })}
                  />
                </div>
              </Section>

              <Separator />

              <Section title={t("settings.appearance.language")}>
                <Select
                  value={ui?.language ?? "system"}
                  onValueChange={(v) =>
                    patchUi({ language: v as "system" | "zh" | "en" })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="system">
                      {t("settings.appearance.languageSystem")}
                    </SelectItem>
                    <SelectItem value="zh">简体中文</SelectItem>
                    <SelectItem value="en">English</SelectItem>
                  </SelectContent>
                </Select>
                <Hint>{t("settings.appearance.languageHint")}</Hint>
              </Section>

              <Separator />

              <Section title={t("settings.appearance.panelWidth")}>
                <SliderRow
                  label={t("settings.appearance.sidebarWidth")}
                  value={ui?.sidebarWidth ?? 240}
                  min={160}
                  max={480}
                  step={8}
                  unit="px"
                  onChange={(v) => patchUi({ sidebarWidth: v })}
                />
                <SliderRow
                  label={t("settings.appearance.agentWidth")}
                  value={ui?.agentPanelWidth ?? 384}
                  min={260}
                  max={720}
                  step={8}
                  unit="px"
                  onChange={(v) => patchUi({ agentPanelWidth: v })}
                />
              </Section>

              <Separator />

              <div className="flex items-center justify-between">
                <div>
                  <Label>{t("settings.appearance.showAgent")}</Label>
                  <Hint>{t("settings.appearance.showAgentHint")}</Hint>
                </div>
                <Switch
                  checked={ui?.showAgentPanel ?? true}
                  onCheckedChange={(v) => patchUi({ showAgentPanel: v })}
                />
              </div>
            </TabsContent>

            {/* ───────────── 终端 ───────────── */}
            <TabsContent value="terminal" className="mt-4 space-y-5">
              <Section title={t("settings.terminal.fontFamily")}>
                <Select
                  value={FONT_PRESETS.some((p) => p.value === fontFamily) ? fontFamily : "custom"}
                  onValueChange={(v) => {
                    if (v !== "custom") patchTerminal({ fontFamily: v });
                  }}
                >
                  <SelectTrigger>
                    <SelectValue placeholder={t("settings.terminal.fontPlaceholder")} />
                  </SelectTrigger>
                  <SelectContent>
                    {FONT_PRESETS.map((p) => (
                      <SelectItem key={p.value || "builtin"} value={p.value}>
                        {p.label ?? (p.labelKey ? t(p.labelKey) : p.value)}
                      </SelectItem>
                    ))}
                    <SelectItem value="custom">{t("settings.terminal.fontCustom")}</SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  className="mt-2 font-mono text-xs"
                  placeholder={t("settings.terminal.fontCustomPlaceholder")}
                  defaultValue={fontFamily}
                  onBlur={(e) => {
                    const v = e.target.value.trim();
                    if (v !== fontFamily) patchTerminal({ fontFamily: v });
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = (e.target as HTMLInputElement).value.trim();
                      if (v !== fontFamily) patchTerminal({ fontFamily: v });
                    }
                  }}
                />
                <Hint>
                  {t("settings.terminal.fontFamilyHint")}
                </Hint>
              </Section>

              <Separator />

              <Section title={t("settings.terminal.fontSize")}>
                <SliderRow
                  label={t("settings.terminal.fontSize")}
                  value={fontSize}
                  min={8}
                  max={32}
                  step={1}
                  unit="px"
                  onChange={(v) => setFontSizeDraft(v)}
                  onCommit={(v) => {
                    setFontSizeDraft(null);
                    if (v !== term?.fontSize) patchTerminal({ fontSize: v });
                  }}
                />
              </Section>

              <Section title={t("settings.terminal.scrollback")}>
                <SliderRow
                  label={t("settings.terminal.scrollbackLines")}
                  value={term?.scrollback ?? 5000}
                  min={500}
                  max={100000}
                  step={500}
                  unit={t("settings.terminal.linesUnit")}
                  onChange={() => undefined}
                  onCommit={(v) => {
                    if (v !== term?.scrollback) patchTerminal({ scrollback: v });
                  }}
                />
                <Hint>{t("settings.terminal.scrollbackHint")}</Hint>
              </Section>

              <Separator />

              <Section title={t("settings.terminal.cursor")}>
                <div className="flex items-center justify-between">
                  <Label>{t("settings.terminal.cursorStyle")}</Label>
                  <Select
                    value={term?.cursorStyle ?? "block"}
                    onValueChange={(v) =>
                      patchTerminal({
                        cursorStyle: v as "block" | "bar" | "underline",
                      })
                    }
                  >
                    <SelectTrigger className="w-36">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {CURSOR_STYLES.map((c) => (
                        <SelectItem key={c.value} value={c.value}>
                          {c.label ?? (c.labelKey ? t(c.labelKey) : c.value)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="mt-3 flex items-center justify-between">
                  <Label>{t("settings.terminal.cursorBlink")}</Label>
                  <Switch
                    checked={term?.cursorBlink ?? true}
                    onCheckedChange={(v) => patchTerminal({ cursorBlink: v })}
                  />
                </div>
              </Section>

              <Separator />

              <Section title={t("settings.terminal.preview")}>
                <TerminalPreview
                  fontFamily={fontFamily}
                  fontSize={fontSize}
                />
              </Section>
            </TabsContent>

            {/* ───────────── 行为 ───────────── */}
            <TabsContent value="behavior" className="mt-4 space-y-5">
              <div className="flex items-center justify-between">
                <div>
                  <Label>{t("settings.behavior.confirmCloseTab")}</Label>
                  <Hint>{t("settings.behavior.confirmCloseTabHint")}</Hint>
                </div>
                <Switch
                  checked={behavior?.confirmOnCloseTab ?? true}
                  onCheckedChange={(v) => patchBehavior({ confirmOnCloseTab: v })}
                />
              </div>

              <Separator />

              <div className="flex items-center justify-between">
                <div>
                  <Label>{t("settings.behavior.bellOnDisconnect")}</Label>
                  <Hint>{t("settings.behavior.bellOnDisconnectHint")}</Hint>
                </div>
                <Switch
                  checked={behavior?.bellOnDisconnect ?? true}
                  onCheckedChange={(v) => patchBehavior({ bellOnDisconnect: v })}
                />
              </div>

              <Separator />

              <Section title={t("settings.behavior.autoReconnect")}>
                <SliderRow
                  label={t("settings.behavior.retryCount")}
                  value={behavior?.autoReconnectAttempts ?? 0}
                  min={0}
                  max={10}
                  step={1}
                  unit={t("settings.behavior.timesUnit")}
                  onChange={() => undefined}
                  onCommit={(v) => {
                    if (v !== behavior?.autoReconnectAttempts) {
                      patchBehavior({ autoReconnectAttempts: v });
                    }
                  }}
                />
                <Hint>{t("settings.behavior.autoReconnectHint")}</Hint>
              </Section>

              <Separator />

              <Section title={t("settings.behavior.hostKey")}>
                <Select
                  value={behavior?.hostKeyPolicy ?? "strict"}
                  onValueChange={(v) =>
                    patchBehavior({ hostKeyPolicy: v as "strict" | "accept_new" | "insecure" })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="strict">
                      {t("settings.behavior.hostKeyStrict")}
                    </SelectItem>
                    <SelectItem value="accept_new">
                      {t("settings.behavior.hostKeyAcceptNew")}
                    </SelectItem>
                    <SelectItem value="insecure">
                      {t("settings.behavior.hostKeyInsecure")}
                    </SelectItem>
                  </SelectContent>
                </Select>
                <Hint>
                  {behavior?.hostKeyPolicy === "accept_new"
                    ? t("settings.behavior.hostKeyAcceptNewHint")
                    : behavior?.hostKeyPolicy === "insecure"
                      ? t("settings.behavior.hostKeyInsecureHint")
                      : t("settings.behavior.hostKeyStrictHint")}
                </Hint>
                <div className="pt-1">
                  <Label className="text-xs">{t("settings.behavior.knownHostsPath")}</Label>
                  <Input
                    className="mt-1 font-mono text-xs"
                    placeholder="~/.ssh/known_hosts"
                    defaultValue={behavior?.knownHostsPath ?? ""}
                    onBlur={(e) => {
                      const v = e.target.value.trim();
                      if (v !== (behavior?.knownHostsPath ?? "")) {
                        patchBehavior({ knownHostsPath: v });
                      }
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        const v = (e.target as HTMLInputElement).value.trim();
                        if (v !== (behavior?.knownHostsPath ?? "")) {
                          patchBehavior({ knownHostsPath: v });
                        }
                      }
                    }}
                  />
                  <Hint>{t("settings.behavior.knownHostsPathHint")}</Hint>
                </div>
              </Section>

              <Separator />

              <Section title={t("settings.behavior.reuse")}>
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("settings.behavior.reuseToggle")}</Label>
                    <Hint>{t("settings.behavior.reuseHint")}</Hint>
                  </div>
                  <Switch
                    checked={behavior?.sessionReuse ?? true}
                    onCheckedChange={(v) => patchBehavior({ sessionReuse: v })}
                  />
                </div>

                <div className="grid grid-cols-2 gap-3 pt-2">
                  <div>
                    <Label className="text-xs">{t("settings.behavior.reuseIdle")}</Label>
                    <Input
                      type="number"
                      min={0}
                      max={86400}
                      className="mt-1"
                      defaultValue={behavior?.sessionReuseIdleSecs ?? 300}
                      onBlur={(e) => {
                        const v = Number(e.target.value);
                        if (
                          Number.isFinite(v) &&
                          v >= 0 &&
                          v <= 86400 &&
                          v !== behavior?.sessionReuseIdleSecs
                        ) {
                          patchBehavior({ sessionReuseIdleSecs: v });
                        }
                      }}
                    />
                  </div>
                  <div>
                    <Label className="text-xs">{t("settings.behavior.forwardReconnectMax")}</Label>
                    <Input
                      type="number"
                      min={1}
                      max={300}
                      className="mt-1"
                      defaultValue={behavior?.forwardReconnectMaxSecs ?? 30}
                      onBlur={(e) => {
                        const v = Number(e.target.value);
                        if (
                          Number.isFinite(v) &&
                          v >= 1 &&
                          v <= 300 &&
                          v !== behavior?.forwardReconnectMaxSecs
                        ) {
                          patchBehavior({ forwardReconnectMaxSecs: v });
                        }
                      }}
                    />
                  </div>
                </div>
                <Hint>{t("settings.behavior.reuseIdleHint")}</Hint>

                <div className="mt-3 flex items-center justify-between">
                  <div>
                    <Label>{t("settings.behavior.forwardAutoRestore")}</Label>
                    <Hint>{t("settings.behavior.forwardAutoRestoreHint")}</Hint>
                  </div>
                  <Switch
                    checked={behavior?.forwardAutoRestore ?? true}
                    onCheckedChange={(v) => patchBehavior({ forwardAutoRestore: v })}
                  />
                </div>

                <LinkStats />
              </Section>

              <Separator />

              <Section title={t("settings.behavior.defaultsForNew")}>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <Label className="text-xs">{t("settings.behavior.defaultPort")}</Label>
                    <Input
                      type="number"
                      className="mt-1"
                      defaultValue={behavior?.defaultPort ?? 22}
                      onBlur={(e) => {
                        const v = Number(e.target.value);
                        if (Number.isFinite(v) && v >= 1 && v <= 65535 && v !== behavior?.defaultPort) {
                          patchBehavior({ defaultPort: v });
                        }
                      }}
                    />
                  </div>
                  <div>
                    <Label className="text-xs">{t("settings.behavior.defaultUsername")}</Label>
                    <Input
                      className="mt-1"
                      placeholder={t("settings.behavior.defaultEmptyHint")}
                      defaultValue={behavior?.defaultUsername ?? ""}
                      onBlur={(e) => {
                        const v = e.target.value.trim();
                        if (v !== behavior?.defaultUsername) {
                          patchBehavior({ defaultUsername: v });
                        }
                      }}
                    />
                  </div>
                </div>
              </Section>
            </TabsContent>

            {/* ───────────── Agent（大模型接入） ───────────── */}
            {/* 模型接入：原「渠道」与「Agent」已合并为此单一入口 */}
            <TabsContent value="model" className="mt-4">
              <ModelPanel />
            </TabsContent>

            {/* ───────────── 省 Token 与审计 ───────────── */}
            <TabsContent value="token" className="mt-4">
              <TokenPanel />
            </TabsContent>
          </Tabs>
        </div>

        <footer className="flex items-center justify-between border-t border-border px-5 py-3">
          <span className="text-[11px] text-muted-foreground">
            {t("settings.configPath")}
          </span>
          <Button size="sm" onClick={onClose}>
            {t("common.done")}
          </Button>
        </footer>
      </aside>
    </div>
  );
}

// ───────────────────────────── 子组件 ─────────────────────────────

function Section({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <section className="space-y-2.5">
      <h3 className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
        {title}
      </h3>
      {children}
    </section>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return (
    <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
      {children}
    </p>
  );
}

function ThemeCard({
  icon,
  label,
  active,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex flex-col items-center gap-2 rounded-lg border px-3 py-3 text-xs transition-colors ${
        active
          ? "border-primary bg-primary/10 text-foreground"
          : "border-border text-muted-foreground hover:bg-accent"
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

function SliderRow({
  label,
  value,
  min,
  max,
  step,
  unit,
  onChange,
  onCommit,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  unit: string;
  onChange: (v: number) => void;
  onCommit?: (v: number) => void;
}) {
  const commit = onCommit ?? onChange;
  return (
    <div className="flex items-center gap-3">
      <span className="w-32 shrink-0 text-xs text-muted-foreground">{label}</span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        className="h-1.5 flex-1 cursor-pointer appearance-none rounded-full bg-muted accent-primary"
        onChange={(e) => onChange(Number(e.target.value))}
        onMouseUp={(e) => commit(Number((e.target as HTMLInputElement).value))}
        onTouchEnd={(e) => commit(Number((e.target as HTMLInputElement).value))}
        onKeyUp={(e) => commit(Number((e.target as HTMLInputElement).value))}
      />
      <span className="w-16 shrink-0 text-right font-mono text-xs text-muted-foreground">
        {value}
        {unit}
      </span>
    </div>
  );
}

/** 终端外观实时预览。 */
/* previewBg 依据当前实际主题 */
function TerminalPreview({
  fontFamily,
  fontSize,
}: {
  fontFamily: string;
  fontSize: number;
}) {
  const { t } = useTranslation();
  const themePref = useSettingsStore((s) => s.config?.ui.theme) ?? "dark";
  const resolved = resolveTheme(themePref);
  const previewBg = resolved === "light" ? "#fafafa" : "#1e1e1e";
  const previewFg = resolved === "light" ? "#333333" : "#d4d4d4";
  const family =
    fontFamily && fontFamily.trim()
      ? fontFamily
      : '"Cascadia Code", "JetBrainsMono Nerd Font", "Consolas", monospace';

  return (
    <div className="rounded-md border border-border p-3" style={{ background: previewBg }}>
      <div className="mb-1 flex items-center gap-1.5 text-[10px] text-muted-foreground">
        <Type className="size-3" />
        {t("settings.terminal.previewHint")}
      </div>
      <div
        className="leading-snug whitespace-pre"
        style={{
          fontFamily: family,
          fontSize: `${Math.min(fontSize, 20)}px`,
          color: previewFg,
        }}
      >
        {"blackholemax@archlinux:~$ ls -la\n"}
        <span style={{ color: "#0dbc79" }}>{"drwxr-xr-x  project\n"}</span>
        <span style={{ color: "#e5e510" }}>{"-rw-r--r--  notes.md\n"}</span>
        {"blackholemax@archlinux:~$ "}
        <span style={{ color: "#d4d4d4" }}>█</span>
      </div>
    </div>
  );
}

/**
 * 链路诊断：展示当前 SSH 连接池。
 *
 * 验证复用是否生效：打开同主机第二个标签页后点刷新 —— `handshakes` 不变、
 * `reuses` +1，且两个会话指向同一个 `linkId`。
 */
function LinkStats() {
  const { t } = useTranslation();
  const [stats, setStats] = useState<PoolStats | null>(null);
  const [busy, setBusy] = useState(false);

  const load = async () => {
    setBusy(true);
    try {
      setStats(await api.poolStats());
    } catch {
      // 诊断信息拿不到不影响设置页使用
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  return (
    <div className="mt-3 space-y-1 rounded-md border border-border px-2 py-1.5">
      <div className="flex items-center justify-between">
        <Label>{t("settings.behavior.poolTitle")}</Label>
        <Button
          variant="ghost"
          size="sm"
          className="h-6 px-1.5"
          title={t("settings.behavior.poolRefresh")}
          onClick={() => void load()}
        >
          <RefreshCw className={cn("size-3", busy && "animate-spin")} />
        </Button>
      </div>
      <div className="text-[11px] text-muted-foreground">
        {stats
          ? t("settings.behavior.poolStats", {
              handshakes: stats.handshakes,
              reuses: stats.reuses,
              sessions: stats.sessions,
            })
          : "…"}
      </div>
      {stats && stats.links.length === 0 ? (
        <div className="text-[11px] text-muted-foreground">
          {t("settings.behavior.poolEmpty")}
        </div>
      ) : null}
      {stats?.links.map((l) => (
        <div
          key={l.id}
          className="truncate font-mono text-[10px] text-muted-foreground"
          title={l.id}
        >
          {t("settings.behavior.poolLink", {
            user: l.username,
            host: l.host,
            port: l.port,
            sessions: l.sessions,
            reuses: l.reuses,
          })}
        </div>
      ))}
    </div>
  );
}
