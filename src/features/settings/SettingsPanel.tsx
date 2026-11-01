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
import { useState, type ReactNode } from "react";
import { X, RotateCcw, Monitor, Moon, Sun, Type } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api, SshdeckError } from "@/shared/api";
import type { AppConfigPatch } from "@/shared/ipc";
import { resolveTheme, useSettingsStore } from "@/stores/settings";

import { ChannelPanel } from "./ChannelPanel";
import { TokenPanel } from "./TokenPanel";

/** 预设字体栈（都含 Nerd Font 变体，保证远端提示符图标能显示）。 */
const FONT_PRESETS: { label: string; value: string }[] = [
  { label: "内置字体栈（推荐）", value: "" },
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
    label: "MesloLGS NF（Powerlevel10k 推荐）",
    value: '"MesloLGS NF", "Consolas", monospace',
  },
  {
    label: "Consolas（Windows 自带）",
    value: '"Consolas", monospace',
  },
];

const CURSOR_STYLES = [
  { label: "方块", value: "block" },
  { label: "竖线", value: "bar" },
  { label: "下划线", value: "underline" },
] as const;

interface SettingsPanelProps {
  open: boolean;
  onClose: () => void;
}

export function SettingsPanel({ open, onClose }: SettingsPanelProps) {
  const config = useSettingsStore((s) => s.config);
  const patch = useSettingsStore((s) => s.patch);
  const saving = useSettingsStore((s) => s.saving);
  const error = useSettingsStore((s) => s.error);
  const clearError = useSettingsStore((s) => s.clearError);

  // 字号滑块的本地即时值：拖动时先本地响应，松开/停止后再写后端。
  const [fontSizeDraft, setFontSizeDraft] = useState<number | null>(null);

  // Agent 配置：Key 永不回显，只能在输入框里临时填写。
  const [keyDraft, setKeyDraft] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);

  if (!open) return null;

  const ui = config?.ui;
  const term = config?.terminal;
  const behavior = config?.behavior;
  const llm = config?.llm;

  const patchTerminal = (p: AppConfigPatch["terminal"]) =>
    void patch({ terminal: p });
  const patchUi = (p: AppConfigPatch["ui"]) => void patch({ ui: p });
  const patchBehavior = (p: AppConfigPatch["behavior"]) =>
    void patch({ behavior: p });
  const patchLlm = (p: AppConfigPatch["llm"]) => void patch({ llm: p });

  const fontSize = fontSizeDraft ?? term?.fontSize ?? 14;
  const fontFamily = term?.fontFamily ?? "";

  return (
    <div className="fixed inset-0 z-40 flex justify-end">
      {/* 遮罩：点击关闭 */}
      <button
        type="button"
        aria-label="关闭设置"
        className="absolute inset-0 bg-black/50"
        onClick={onClose}
      />

      <aside className="relative z-50 flex h-full w-[580px] flex-col border-l border-border bg-background shadow-xl">
        <header className="flex items-center justify-between border-b border-border px-5 py-3.5">
          <div>
            <h2 className="text-sm font-semibold">设置</h2>
            <p className="mt-0.5 text-[11px] text-muted-foreground">
              修改会立即保存到本地配置
              {saving ? " · 保存中…" : ""}
            </p>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="sm"
              title="恢复默认设置"
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
                  },
                })
              }
            >
              <RotateCcw className="size-3.5" />
              重置
            </Button>
            <Button variant="ghost" size="icon" onClick={onClose}>
              <X className="size-4" />
            </Button>
          </div>
        </header>

        {error ? (
          <div className="mx-5 mt-3 flex items-start justify-between gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
            <span>保存失败 — {error}</span>
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
                外观
              </TabsTrigger>
              <TabsTrigger value="terminal" className="flex-1">
                终端
              </TabsTrigger>
              <TabsTrigger value="behavior" className="flex-1">
                行为
              </TabsTrigger>
              <TabsTrigger value="agent" className="flex-1">
                Agent
              </TabsTrigger>
              <TabsTrigger value="channels" className="flex-1">
                渠道
              </TabsTrigger>
              <TabsTrigger value="token" className="flex-1">
                省 Token
              </TabsTrigger>
            </TabsList>

            {/* ───────────── 外观 ───────────── */}
            <TabsContent value="appearance" className="mt-4 space-y-5">
              <Section title="主题">
                <div className="grid grid-cols-3 gap-2">
                  <ThemeCard
                    icon={<Moon className="size-4" />}
                    label="深色"
                    active={ui?.theme === "dark"}
                    onClick={() => patchUi({ theme: "dark" })}
                  />
                  <ThemeCard
                    icon={<Sun className="size-4" />}
                    label="浅色"
                    active={ui?.theme === "light"}
                    onClick={() => patchUi({ theme: "light" })}
                  />
                  <ThemeCard
                    icon={<Monitor className="size-4" />}
                    label="跟随系统"
                    active={ui?.theme === "system"}
                    onClick={() => patchUi({ theme: "system" })}
                  />
                </div>
              </Section>

              <Separator />

              <Section title="界面语言">
                <Select
                  value={ui?.language ?? "zh"}
                  onValueChange={(v) => patchUi({ language: v as "zh" | "en" })}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="zh">简体中文</SelectItem>
                    <SelectItem value="en">English</SelectItem>
                  </SelectContent>
                </Select>
                <Hint>语言切换当前仅影响少量文案，完整 i18n 待后续接入。</Hint>
              </Section>

              <Separator />

              <Section title="面板宽度">
                <SliderRow
                  label="左栏（主机 / 转发）"
                  value={ui?.sidebarWidth ?? 240}
                  min={160}
                  max={480}
                  step={8}
                  unit="px"
                  onChange={(v) => patchUi({ sidebarWidth: v })}
                />
                <SliderRow
                  label="右栏（运维 Agent）"
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
                  <Label>显示运维 Agent 面板</Label>
                  <Hint>关闭后右栏隐藏，终端区占满剩余宽度</Hint>
                </div>
                <Switch
                  checked={ui?.showAgentPanel ?? true}
                  onCheckedChange={(v) => patchUi({ showAgentPanel: v })}
                />
              </div>
            </TabsContent>

            {/* ───────────── 终端 ───────────── */}
            <TabsContent value="terminal" className="mt-4 space-y-5">
              <Section title="字体">
                <Select
                  value={FONT_PRESETS.some((p) => p.value === fontFamily) ? fontFamily : "custom"}
                  onValueChange={(v) => {
                    if (v !== "custom") patchTerminal({ fontFamily: v });
                  }}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="选择字体" />
                  </SelectTrigger>
                  <SelectContent>
                    {FONT_PRESETS.map((p) => (
                      <SelectItem key={p.label} value={p.value}>
                        {p.label}
                      </SelectItem>
                    ))}
                    <SelectItem value="custom">自定义…</SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  className="mt-2 font-mono text-xs"
                  placeholder='自定义字体栈，如 "Cascadia Code", "Consolas", monospace'
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
                  留空使用内置字体栈（已含常见 Nerd Font 变体，可显示远端提示符图标）
                </Hint>
              </Section>

              <Separator />

              <Section title="字号">
                <SliderRow
                  label="终端字号"
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

              <Section title="回滚缓冲">
                <SliderRow
                  label="保留行数"
                  value={term?.scrollback ?? 5000}
                  min={500}
                  max={100000}
                  step={500}
                  unit="行"
                  onChange={() => undefined}
                  onCommit={(v) => {
                    if (v !== term?.scrollback) patchTerminal({ scrollback: v });
                  }}
                />
                <Hint>行数越大内存占用越高；重启会话后生效。</Hint>
              </Section>

              <Separator />

              <Section title="光标">
                <div className="flex items-center justify-between">
                  <Label>样式</Label>
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
                          {c.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="mt-3 flex items-center justify-between">
                  <Label>闪烁</Label>
                  <Switch
                    checked={term?.cursorBlink ?? true}
                    onCheckedChange={(v) => patchTerminal({ cursorBlink: v })}
                  />
                </div>
              </Section>

              <Separator />

              <Section title="预览">
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
                  <Label>关闭标签时确认</Label>
                  <Hint>避免误关正在运行的会话</Hint>
                </div>
                <Switch
                  checked={behavior?.confirmOnCloseTab ?? true}
                  onCheckedChange={(v) => patchBehavior({ confirmOnCloseTab: v })}
                />
              </div>

              <Separator />

              <div className="flex items-center justify-between">
                <div>
                  <Label>连接断开时提示音</Label>
                  <Hint>会话异常断开时播放系统提示音</Hint>
                </div>
                <Switch
                  checked={behavior?.bellOnDisconnect ?? true}
                  onCheckedChange={(v) => patchBehavior({ bellOnDisconnect: v })}
                />
              </div>

              <Separator />

              <Section title="连接失败时自动重试">
                <SliderRow
                  label="重试次数"
                  value={behavior?.autoReconnectAttempts ?? 0}
                  min={0}
                  max={10}
                  step={1}
                  unit="次"
                  onChange={() => undefined}
                  onCommit={(v) => {
                    if (v !== behavior?.autoReconnectAttempts) {
                      patchBehavior({ autoReconnectAttempts: v });
                    }
                  }}
                />
                <Hint>0 表示不自动重试</Hint>
              </Section>

              <Separator />

              <Section title="新建连接的默认值">
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <Label className="text-xs">默认端口</Label>
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
                    <Label className="text-xs">默认用户名</Label>
                    <Input
                      className="mt-1"
                      placeholder="留空则不填充"
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
            <TabsContent value="agent" className="mt-4 space-y-5">
              <Section title="服务地址">
                <Input
                  placeholder="https://api.deepseek.com/v1"
                  defaultValue={llm?.baseUrl ?? ""}
                  onBlur={(e) => {
                    const v = e.target.value.trim();
                    if (v && v !== llm?.baseUrl) patchLlm({ baseUrl: v });
                  }}
                />
                <Hint>
                  兼容 OpenAI 协议的任意服务：DeepSeek、OpenAI、Qwen、vLLM、
                  Ollama（本地默认 http://127.0.0.1:11434/v1）。缺 /v1 会自动补全。
                </Hint>
              </Section>

              <Section title="模型">
                <Input
                  placeholder="如 deepseek-chat / qwen2.5:7b"
                  defaultValue={llm?.model ?? ""}
                  onBlur={(e) => {
                    const v = e.target.value.trim();
                    if (v !== llm?.model) patchLlm({ model: v });
                  }}
                />
                <div className="mt-2 flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={probing}
                    onClick={async () => {
                      setProbing(true);
                      try {
                        const list = await api.agentListModels();
                        setModels(list.map((m) => m.id));
                        setProbeError(null);
                      } catch (err) {
                        setProbeError(
                          err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err),
                        );
                      } finally {
                        setProbing(false);
                      }
                    }}
                  >
                    {probing ? "探测中…" : "探测可用模型"}
                  </Button>
                  {models.length > 0 ? (
                    <Select
                      onValueChange={(v) => {
                        patchLlm({ model: v });
                      }}
                    >
                      <SelectTrigger className="w-56">
                        <SelectValue placeholder={`发现 ${models.length} 个模型`} />
                      </SelectTrigger>
                      <SelectContent>
                        {models.map((m) => (
                          <SelectItem key={m} value={m}>
                            {m}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  ) : null}
                </div>
                {probeError ? (
                  <p className="mt-1 text-[11px] text-destructive">{probeError}</p>
                ) : null}
              </Section>

              <Separator />

              <Section title="API Key">
                {llm?.hasApiKey ? (
                  <div className="flex items-center justify-between rounded-md border border-border px-3 py-2">
                    <span className="text-xs text-muted-foreground">已配置（保存于本地，不回显）</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => patchLlm({ clearApiKey: true })}
                    >
                      清除
                    </Button>
                  </div>
                ) : (
                  <div className="space-y-2">
                    <Input
                      type="password"
                      placeholder="粘贴 API Key 后点保存"
                      value={keyDraft}
                      onChange={(e) => setKeyDraft(e.target.value)}
                    />
                    <Button
                      size="sm"
                      disabled={!keyDraft.trim()}
                      onClick={() => {
                        patchLlm({ apiKey: keyDraft.trim() });
                        setKeyDraft("");
                      }}
                    >
                      保存 Key
                    </Button>
                  </div>
                )}
                <Hint>
                  Key 只存在本机（`secrets.json`，Unix 下 0600），前端永不回读明文。
                </Hint>
              </Section>

              <Separator />

              <Section title="温度">
                <SliderRow
                  label="temperature"
                  value={Math.round((llm?.temperature ?? 0.2) * 100)}
                  min={0}
                  max={200}
                  step={5}
                  unit=""
                  onChange={(v) => patchLlm({ temperature: v / 100 })}
                />
                <Hint>0 = 更确定，1 以上 = 更发散。运维场景建议 0.1 – 0.3。</Hint>
              </Section>

              {llm && !llm.hasApiKey ? (
                <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400">
                  尚未配置 API Key，Agent 面板无法发起对话。
                </div>
              ) : null}
            </TabsContent>

            {/* ───────────── 聚合渠道 ───────────── */}
            <TabsContent value="channels" className="mt-4">
              <ChannelPanel />
            </TabsContent>

            {/* ───────────── 省 Token 与审计 ───────────── */}
            <TabsContent value="token" className="mt-4">
              <TokenPanel />
            </TabsContent>
          </Tabs>
        </div>

        <footer className="flex items-center justify-between border-t border-border px-5 py-3">
          <span className="text-[11px] text-muted-foreground">
            配置保存在用户目录下的 state.json
          </span>
          <Button size="sm" onClick={onClose}>
            完成
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
        预览（缩放随字号变化）
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
