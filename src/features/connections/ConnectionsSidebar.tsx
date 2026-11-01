/**
 * 左栏上半：主机列表 + 新建连接。
 *
 * 交互约定：
 * - 点击主机行 → 在该行下方展开内联的凭据输入（密码 / 私钥口令），不使用模态弹窗。
 * - 「新建连接」表单内联展开，可勾选「{t("host.saveAsHost")}」（`api.upsertHost`）。
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
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation();
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
      setNotice(t("host.connectedTo", { title }));
      return;
    }
    addTab(sessionId, title);
    setNotice(t("host.connectedTo", { title }));
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
      setError(t("host.errorNoPassword"));
      return;
    }
    if (host.authType === "key" && !host.keyPath) {
      setError(t("host.errorNoKeyPath"));
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
    if (!form.host.trim()) return setError(t("host.errorNoHost"));
    if (!form.username.trim()) return setError(t("host.errorNoUser"));
    if (port === null) return setError(t("host.errorPort"));
    if (form.authType === "password" && !form.password) return setError(t("host.errorNoPassword"));
    if (form.authType === "key" && !form.keyPath.trim()) return setError(t("host.errorNoKeyPath2"));

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
        setError(t("host.saveFailed", { error: errorText(err) }));
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
      setError(t("host.deleteFailed", { error: errorText(err) }));
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 区域标题 */}
      <div className="flex shrink-0 items-center justify-between px-3 py-2">
        <div className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground">
          <Server className="size-3.5" />
          <span>{t("sidebar.hosts")}</span>
          <Badge variant="secondary" className="h-4 px-1 text-[10px]">
            {hosts.length}
          </Badge>
        </div>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-1.5"
          title={t("host.newConnection")}
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
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.nameOptional")}</Label>
              <Input
                className="h-7 text-xs"
                value={form.name}
                placeholder={t("host.namePlaceholder")}
                onChange={(e) => patchForm({ name: e.target.value })}
              />
            </div>
            <div className="col-span-2 space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.host")}</Label>
              <Input
                className="h-7 text-xs"
                value={form.host}
                placeholder="192.168.1.10"
                onChange={(e) => patchForm({ host: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.port")}</Label>
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
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.username")}</Label>
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
                { kind: "password" as const, labelKey: "host.authPassword", icon: Lock },
                { kind: "key" as const, labelKey: "host.authKey", icon: KeyRound },
              ]
            ).map(({ kind, labelKey, icon: Icon }) => (
              <Button
                key={kind}
                size="sm"
                variant={form.authType === kind ? "secondary" : "ghost"}
                className="h-6 px-2 text-[11px]"
                onClick={() => patchForm({ authType: kind })}
              >
                <Icon className="size-3" />
                {t(labelKey)}
              </Button>
            ))}
          </div>

          {form.authType === "password" ? (
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.password")}</Label>
              <Input
                className="h-7 text-xs"
                type="password"
                value={form.password}
                placeholder={t("host.passwordPlaceholder2")}
                onChange={(e) => patchForm({ password: e.target.value })}
              />
            </div>
          ) : (
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("host.privateKeyPath")}</Label>
              <Input
                className="h-7 text-xs"
                value={form.keyPath}
                placeholder="~/.ssh/id_ed25519"
                onChange={(e) => patchForm({ keyPath: e.target.value })}
              />
              <Label className="text-[11px] font-normal text-muted-foreground">
                {t("host.keyPassphraseOptional")}
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
            {t("host.saveAsHost")}
          </label>

          <div className="flex items-center justify-end gap-1">
            <Button
              size="sm"
              className="h-7 text-xs"
              disabled={submitting}
              onClick={() => void handleSubmitForm()}
            >
              {submitting ? <LoaderCircle className="size-3.5 animate-spin" /> : null}
              {t("host.connect")}
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
              {t("common.cancel")}
            </Button>
          </div>
        </div>
      ) : null}

      {/* 主机列表 */}
      <div className="min-h-0 flex-1 space-y-px overflow-y-auto px-2 pb-2">
        {hosts.length === 0 ? (
          <div className="px-1 py-4 text-center text-[11px] text-muted-foreground">
            {t("host.noHosts")}
            <br />
            {t("host.noHostsHint")}
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
                      {host.authType === "key" ? t("host.authKey") : t("host.authPassword")}
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
                      {t("common.remove")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-5 px-1 text-[10px]"
                      onClick={() => setConfirmDeleteId(null)}
                    >
                      {t("common.cancel")}
                    </Button>
                  </div>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="size-6 shrink-0 p-0 text-muted-foreground hover:text-destructive"
                    title={t("host.deleteHost")}
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
                      placeholder={t("host.password")}
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
                      placeholder={t("host.keyPassphrasePlaceholder")}
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
                        ? host.keyPath || t("host.noKeyPath")
                        : t("host.loginAs", { username: host.username })}
                    </span>
                    <Button
                      size="sm"
                      className="h-6 px-2 text-[11px]"
                      disabled={submitting}
                      onClick={() => void handleConnectHost(host)}
                    >
                      {busy ? <LoaderCircle className="size-3 animate-spin" /> : <User className="size-3" />}
                      {t("host.connect")}
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
