/**
 * 左栏上半：主机列表 + 新建连接。
 *
 * 交互约定：
 * - 点击主机行 → 在该行下方展开内联的凭据输入（密码 / 私钥口令），不使用模态弹窗。
 * - 「新建连接」表单内联展开，可勾选「保存为主机」（`api.upsertHost`）。
 * - 所有错误内联展示（`SshdeckError.code` + `message`），绝不使用 alert/confirm。
 * - 连接成功后：若有空 pane 意图则填入该 pane，否则 `useSessionsStore.addTab`。
 */
import { useEffect, useMemo, useState } from "react";
import {
  ChevronDown,
  ChevronUp,
  KeyRound,
  Lock,
  LoaderCircle,
  Plus,
  Server,
  Trash2,
  User,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { api, SshdeckError } from "@/shared/api";
import type { ConnectRequest, HostProfile } from "@/shared/ipc";
import { useConnectionStore } from "@/stores/connection";
import { useSettingsStore } from "@/stores/settings";
import { useSessionsStore, type PaneLayout } from "@/stores/sessions";

import { PANE_COUNT } from "@/features/terminal/layout";
import { ensureCapture } from "@/features/terminal/pendingOutput";

import { useConnectIntent } from "./connectIntent";

/** 建立会话时上报的初始尺寸（随后由 ResizeObserver 纠正）。 */
const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;

interface FormState {
  name: string;
  host: string;
  port: string;
  username: string;
  authType: "password" | "key";
  password: string;
  keyPath: string;
  passphrase: string;
  saveAsHost: boolean;
}

const EMPTY_FORM: FormState = {
  name: "",
  host: "",
  port: "22",
  username: "",
  authType: "password",
  password: "",
  keyPath: "",
  passphrase: "",
  saveAsHost: true,
};

function errorText(err: unknown): string {
  if (err instanceof SshdeckError) return `${err.code}: ${err.message}`;
  if (err instanceof Error) return err.message;
  return String(err);
}

function parsePort(raw: string): number | null {
  const n = Number.parseInt(raw.trim(), 10);
  if (!Number.isInteger(n) || n < 1 || n > 65535) return null;
  return n;
}

/** 让目标 pane 真正可见：需要时把布局升级到能容纳该 index 的最小布局。 */
function layoutForPaneCount(count: number): PaneLayout {
  if (count <= 1) return "single";
  if (count === 2) return "split-h";
  return "quad";
}

export function ConnectionsSidebar() {
  const hosts = useConnectionStore((s) => s.hosts);
  const upsertHostLocal = useConnectionStore((s) => s.upsertHost);
  const removeHostLocal = useConnectionStore((s) => s.removeHost);

  const addTab = useSessionsStore((s) => s.addTab);
  const setPaneSession = useSessionsStore((s) => s.setPaneSession);
  const setLayout = useSessionsStore((s) => s.setLayout);
  const setActiveTab = useSessionsStore((s) => s.setActiveTab);
  const upsertSession = useSessionsStore((s) => s.upsertSession);

  const intent = useConnectIntent();

  // 尽早启动首个输出块的捕获缓冲：`ssh_connect` 返回时远端提示符已发出，
  // 常早于 React 挂载 pane。（幂等）
  useEffect(() => {
    ensureCapture();
  }, []);

  // 成功提示是瞬时的：3 秒后自动收起，避免长时间占位。
  const [notice, setNoticeRaw] = useState<string | null>(null);
  const setNotice = (text: string | null) => {
    setNoticeRaw(text);
  };
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNoticeRaw(null), 3000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  // 新建连接的默认端口 / 用户名来自设置
  const defaultPort = useSettingsStore((s) => s.config?.behavior.defaultPort) ?? 22;
  const defaultUsername = useSettingsStore((s) => s.config?.behavior.defaultUsername) ?? "";

  const emptyForm = useMemo<FormState>(
    () => ({
      ...EMPTY_FORM,
      port: String(defaultPort),
      username: defaultUsername,
    }),
    [defaultPort, defaultUsername],
  );

  // 内联展开状态
  const [formOpen, setFormOpen] = useState(false);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [promptHostId, setPromptHostId] = useState<string | null>(null);
  const [promptPassword, setPromptPassword] = useState("");
  const [promptPassphrase, setPromptPassphrase] = useState("");
  const [pendingHostId, setPendingHostId] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  // 空 pane 请求连接时，自动展开表单（Express 收敛到单一入口）。
  const effectiveFormOpen = formOpen || intent.formOpen;

  const patchForm = (patch: Partial<FormState>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  /** 连接成功后的落位逻辑：填 pane 或新开标签。 */
  const placeSession = (sessionId: string, title: string) => {
    const target = useConnectIntent.getState().target;
    if (target) {
      const tab = useSessionsStore.getState().tabs.find((t) => t.id === target.tabId);
      if (tab) {
        setPaneSession(tab.id, target.paneIndex, sessionId);
        // 仅在当前布局容纳不下该 pane 时升级布局。
        const needed = target.paneIndex + 1;
        const currentCount = PANE_COUNT[tab.layout];
        if (currentCount < needed) {
          setLayout(tab.id, layoutForPaneCount(needed));
        }
        setActiveTab(tab.id);
      } else {
        addTab(sessionId, title);
      }
      useConnectIntent.getState().clear();
      useConnectIntent.getState().setFormOpen(false);
      setFormOpen(false);
      setNotice(`已连接到 ${title}`);
      return;
    }
    addTab(sessionId, title);
    setNotice(`已连接到 ${title}`);
  };

  const doConnect = async (req: ConnectRequest, title: string) => {
    setSubmitting(true);
    setError(null);
    setNotice(null);
    try {
      const info = await api.connect(req);
      upsertSession(info);
      placeSession(info.id, title);
      setPromptHostId(null);
      setPromptPassword("");
      setPromptPassphrase("");
      setForm(emptyForm);
      return true;
    } catch (err) {
      setError(errorText(err));
      return false;
    } finally {
      setSubmitting(false);
    }
  };

  /** 从主机列表直连（凭据由内联输入提供）。 */
  const handleConnectHost = async (host: HostProfile) => {
    const port = host.port;
    const auth =
      host.authType === "key"
        ? {
            kind: "key" as const,
            keyPath: host.keyPath ?? "",
            ...(promptPassphrase ? { passphrase: promptPassphrase } : {}),
          }
        : { kind: "password" as const, password: promptPassword };

    if (host.authType === "password" && !promptPassword) {
      setError("请输入密码");
      return;
    }
    if (host.authType === "key" && !host.keyPath) {
      setError("该主机未配置私钥路径，请用「新建连接」重新保存");
      return;
    }

    setPendingHostId(host.id);
    await doConnect(
      {
        hostId: host.id,
        host: host.host,
        port,
        username: host.username,
        auth,
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
      },
      host.name,
    );
    setPendingHostId(null);
  };

  /** 从「新建连接」表单直连，可选择同时保存为 host profile。 */
  const handleSubmitForm = async () => {
    const port = parsePort(form.port);
    if (!form.host.trim()) return setError("请填写主机地址");
    if (!form.username.trim()) return setError("请填写用户名");
    if (port === null) return setError("端口必须是 1–65535 的整数");
    if (form.authType === "password" && !form.password) return setError("请填写密码");
    if (form.authType === "key" && !form.keyPath.trim()) return setError("请填写私钥路径");

    setError(null);
    const auth =
      form.authType === "key"
        ? {
            kind: "key" as const,
            keyPath: form.keyPath.trim(),
            ...(form.passphrase ? { passphrase: form.passphrase } : {}),
          }
        : { kind: "password" as const, password: form.password };

    const title = form.name.trim() || `${form.username.trim()}@${form.host.trim()}`;

    let saved: HostProfile | null = null;
    if (form.saveAsHost) {
      const existing = hosts.find(
        (h) => h.host.trim() === form.host.trim() && h.username.trim() === form.username.trim(),
      );
      const profile: HostProfile = {
        id: existing?.id ?? crypto.randomUUID(),
        name: title,
        host: form.host.trim(),
        port,
        username: form.username.trim(),
        authType: form.authType,
        ...(form.authType === "key" ? { keyPath: form.keyPath.trim() } : {}),
        updatedAt: Date.now(),
      };
      try {
        const res = await api.upsertHost(profile);
        upsertHostLocal(res);
        saved = res;
      } catch (err) {
        setError(`保存主机失败 — ${errorText(err)}`);
        return;
      }
    }

    const ok = await doConnect(
      {
        ...(saved ? { hostId: saved.id } : {}),
        host: form.host.trim(),
        port,
        username: form.username.trim(),
        auth,
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
      },
      title,
    );
    if (ok) {
      setFormOpen(false);
      useConnectIntent.getState().setFormOpen(false);
    }
  };

  const handleDelete = async (id: string) => {
    setError(null);
    try {
      await api.deleteHost(id);
      removeHostLocal(id);
      setConfirmDeleteId(null);
    } catch (err) {
      setError(`删除主机失败 — ${errorText(err)}`);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 区域标题 */}
      <div className="flex shrink-0 items-center justify-between px-3 py-2">
        <div className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground">
          <Server className="size-3.5" />
          <span>主机</span>
          <Badge variant="secondary" className="h-4 px-1 text-[10px]">
            {hosts.length}
          </Badge>
        </div>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-1.5"
          title="新建连接"
          onClick={() => {
            if (effectiveFormOpen) {
              setFormOpen(false);
              useConnectIntent.getState().setFormOpen(false);
            } else {
              setForm(emptyForm);
              setFormOpen(true);
            }
            setError(null);
          }}
        >
          {effectiveFormOpen ? <ChevronUp className="size-4" /> : <Plus className="size-4" />}
        </Button>
      </div>

      {/* 错误 / 提示条 */}
      {error ? (
        <div className="mx-2 mb-1 flex items-start gap-1.5 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1 text-[11px] break-all text-destructive">
          <span className="shrink-0">⚠</span>
          <span className="min-w-0">{error}</span>
        </div>
      ) : null}
      {notice ? (
        <div className="mx-2 mb-1 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-2 py-1 text-[11px] text-emerald-600 dark:text-emerald-400">
          {notice}
        </div>
      ) : null}

      {/* 新建连接表单 */}
      {effectiveFormOpen ? (
        <div className="mx-2 mb-2 space-y-2 rounded-md border border-border bg-card/40 p-2">
          <div className="grid grid-cols-2 gap-2">
            <div className="col-span-2 space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">名称（可选）</Label>
              <Input
                className="h-7 text-xs"
                value={form.name}
                placeholder="我的服务器"
                onChange={(e) => patchForm({ name: e.target.value })}
              />
            </div>
            <div className="col-span-2 space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">主机</Label>
              <Input
                className="h-7 text-xs"
                value={form.host}
                placeholder="192.168.1.10"
                onChange={(e) => patchForm({ host: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">端口</Label>
              <Input
                className="h-7 text-xs"
                type="number"
                min={1}
                max={65535}
                value={form.port}
                onChange={(e) => patchForm({ port: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">用户名</Label>
              <Input
                className="h-7 text-xs"
                value={form.username}
                placeholder="root"
                onChange={(e) => patchForm({ username: e.target.value })}
              />
            </div>
          </div>

          <div className="flex items-center gap-1">
            {(
              [
                { kind: "password" as const, label: "密码", icon: Lock },
                { kind: "key" as const, label: "私钥", icon: KeyRound },
              ]
            ).map(({ kind, label, icon: Icon }) => (
              <Button
                key={kind}
                size="sm"
                variant={form.authType === kind ? "secondary" : "ghost"}
                className="h-6 px-2 text-[11px]"
                onClick={() => patchForm({ authType: kind })}
              >
                <Icon className="size-3" />
                {label}
              </Button>
            ))}
          </div>

          {form.authType === "password" ? (
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">密码</Label>
              <Input
                className="h-7 text-xs"
                type="password"
                value={form.password}
                placeholder="仅本次连接使用（也可保存到本地配置）"
                onChange={(e) => patchForm({ password: e.target.value })}
              />
            </div>
          ) : (
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">私钥路径</Label>
              <Input
                className="h-7 text-xs"
                value={form.keyPath}
                placeholder="~/.ssh/id_ed25519"
                onChange={(e) => patchForm({ keyPath: e.target.value })}
              />
              <Label className="text-[11px] font-normal text-muted-foreground">
                私钥口令（可选）
              </Label>
              <Input
                className="h-7 text-xs"
                type="password"
                value={form.passphrase}
                onChange={(e) => patchForm({ passphrase: e.target.value })}
              />
            </div>
          )}

          <label className="flex cursor-pointer items-center gap-1.5 text-[11px] text-muted-foreground">
            <input
              type="checkbox"
              checked={form.saveAsHost}
              onChange={(e) => patchForm({ saveAsHost: e.target.checked })}
              className="size-3 accent-current"
            />
            保存为主机
          </label>

          <div className="flex items-center justify-end gap-1">
            <Button
              size="sm"
              className="h-7 text-xs"
              disabled={submitting}
              onClick={() => void handleSubmitForm()}
            >
              {submitting ? <LoaderCircle className="size-3.5 animate-spin" /> : null}
              连接
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-7 text-xs"
              disabled={submitting}
              onClick={() => {
                setForm(emptyForm);
                setFormOpen(false);
                useConnectIntent.getState().setFormOpen(false);
                useConnectIntent.getState().clear();
                setError(null);
              }}
            >
              取消
            </Button>
          </div>
        </div>
      ) : null}

      {/* 主机列表 */}
      <div className="min-h-0 flex-1 space-y-px overflow-y-auto px-2 pb-2">
        {hosts.length === 0 ? (
          <div className="px-1 py-4 text-center text-[11px] text-muted-foreground">
            还没有保存的主机。
            <br />
            点击右上角 ＋ 新建连接。
          </div>
        ) : null}

        {hosts.map((host) => {
          const expanded = promptHostId === host.id;
          const busy = submitting && pendingHostId === host.id;
          return (
            <div
              key={host.id}
              className={cn(
                "rounded-md border border-transparent px-2 py-1.5 transition-colors hover:border-border hover:bg-accent/40",
                expanded && "border-border bg-accent/30",
              )}
            >
              <div className="flex items-center gap-1.5">
                <button
                  type="button"
                  className="min-w-0 flex-1 text-left"
                  disabled={submitting}
                  onClick={() => {
                    setPromptHostId(expanded ? null : host.id);
                    setPromptPassword("");
                    setPromptPassphrase("");
                    setError(null);
                  }}
                >
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-xs font-medium">{host.name}</span>
                    <Badge
                      variant="outline"
                      className="h-4 shrink-0 gap-0.5 px-1 text-[10px]"
                    >
                      {host.authType === "key" ? (
                        <KeyRound className="size-2.5" />
                      ) : (
                        <Lock className="size-2.5" />
                      )}
                      {host.authType === "key" ? "私钥" : "密码"}
                    </Badge>
                  </div>
                  <div className="truncate text-[11px] text-muted-foreground">
                    {host.username}@{host.host}:{host.port}
                  </div>
                </button>

                {busy ? (
                  <LoaderCircle className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
                ) : confirmDeleteId === host.id ? (
                  <div className="flex shrink-0 items-center gap-0.5">
                    <Button
                      size="sm"
                      variant="destructive"
                      className="h-5 px-1 text-[10px]"
                      onClick={() => void handleDelete(host.id)}
                    >
                      删除
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-5 px-1 text-[10px]"
                      onClick={() => setConfirmDeleteId(null)}
                    >
                      取消
                    </Button>
                  </div>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="size-6 shrink-0 p-0 text-muted-foreground hover:text-destructive"
                    title="删除主机"
                    onClick={() => {
                      setConfirmDeleteId(host.id);
                      setPromptHostId(null);
                    }}
                  >
                    <Trash2 className="size-3" />
                  </Button>
                )}
                <span className="shrink-0 text-muted-foreground">
                  {expanded ? (
                    <ChevronUp className="size-3" />
                  ) : (
                    <ChevronDown className="size-3" />
                  )}
                </span>
              </div>

              {/* 内联凭据 */}
              {expanded ? (
                <div className="mt-1.5 space-y-1 border-t border-border pt-1.5">
                  {host.authType === "password" ? (
                    <Input
                      autoFocus
                      className="h-7 text-xs"
                      type="password"
                      placeholder="密码"
                      value={promptPassword}
                      disabled={submitting}
                      onChange={(e) => setPromptPassword(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void handleConnectHost(host);
                        if (e.key === "Escape") setPromptHostId(null);
                      }}
                    />
                  ) : (
                    <Input
                      autoFocus
                      className="h-7 text-xs"
                      type="password"
                      placeholder="私钥口令（如私钥未加密可留空）"
                      value={promptPassphrase}
                      disabled={submitting}
                      onChange={(e) => setPromptPassphrase(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void handleConnectHost(host);
                        if (e.key === "Escape") setPromptHostId(null);
                      }}
                    />
                  )}
                  <div className="flex items-center justify-between">
                    <span className="truncate text-[10px] text-muted-foreground">
                      {host.authType === "key"
                        ? host.keyPath || "（无私钥路径）"
                        : `以 ${host.username} 登录`}
                    </span>
                    <Button
                      size="sm"
                      className="h-6 px-2 text-[11px]"
                      disabled={submitting}
                      onClick={() => void handleConnectHost(host)}
                    >
                      {busy ? <LoaderCircle className="size-3 animate-spin" /> : <User className="size-3" />}
                      连接
                    </Button>
                  </div>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}
