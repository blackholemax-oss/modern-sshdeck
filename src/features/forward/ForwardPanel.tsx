/**
 * 左栏下半：本地端口转发。
 *
 * - 列出 `useForwardStore.forwards`，展示 `本地 :L → remote:port`、状态徽章、停止按钮。
 * - 新建表单默认填当前 activeTab 的主会话；启动走 `api.startForward`。
 * - 订阅 `forward:state` 就地更新状态（含 failed 的 error 文本）。
 * - 会话关闭时转发随之失效：这里只做展示降级，真正的停止由后端保证。
 */
import { useEffect, useState } from "react";
import {
  ArrowRight,
  ChevronUp,
  LoaderCircle,
  Network,
  Plus,
  Power,
} from "lucide-react";

import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { api, events, SshdeckError } from "@/shared/api";
import type { ForwardInfo, ForwardRequest, ForwardState } from "@/shared/ipc";
import { useForwardStore } from "@/stores/forward";
import { useSessionsStore } from "@/stores/sessions";

const DEFAULT_LISTEN_HOST = "127.0.0.1";

const STATE_BADGE: Record<
  ForwardState,
  { labelKey: string; variant: "default" | "secondary" | "destructive" | "outline" | "success" }
> = {
  starting: { labelKey: "forward.status.starting", variant: "secondary" },
  running: { labelKey: "forward.status.running", variant: "success" },
  stopped: { labelKey: "forward.status.stopped", variant: "outline" },
  failed: { labelKey: "forward.status.failed", variant: "destructive" },
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

interface ForwardForm {
  sessionId: string;
  listenHost: string;
  listenPort: string;
  remoteHost: string;
  remotePort: string;
}

export function ForwardPanel() {
  const { t } = useTranslation();
  const forwards = useForwardStore((s) => s.forwards);
  const upsertForward = useForwardStore((s) => s.upsertForward);
  const removeForward = useForwardStore((s) => s.removeForward);

  const tabs = useSessionsStore((s) => s.tabs);
  const activeTabId = useSessionsStore((s) => s.activeTabId);
  const sessions = useSessionsStore((s) => s.sessions);

  // 当前 activeTab 的首个 pane 会话，作为新转发的默认归属。
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeSessionId = activeTab?.panes.find((p): p is string => p !== null) ?? "";

  // 可用于转发的会话：所有已连接会话（来自 store.sessions）。
  const sessionList = Object.values(sessions);

  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<ForwardForm>({
    sessionId: "",
    listenHost: DEFAULT_LISTEN_HOST,
    listenPort: "8080",
    remoteHost: "127.0.0.1",
    remotePort: "80",
  });
  const [submitting, setSubmitting] = useState(false);
  const [stoppingId, setStoppingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const patchForm = (patch: Partial<ForwardForm>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  // ① 初始化：拉取现存转发规则。
  useEffect(() => {
    let disposed = false;
    void api
      .listForwards()
      .then((list) => {
        if (!disposed) useForwardStore.getState().setForwards(list);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, []);

  // ② forward:state 订阅：就地更新状态与错误文本。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void events
      .onForwardState((payload) => {
        const prev = useForwardStore
          .getState()
          .forwards.find((f) => f.id === payload.forwardId);
        if (!prev) return;
        const next: ForwardInfo = {
          ...prev,
          state: payload.state,
          ...(payload.error !== undefined ? { error: payload.error } : {}),
        };
        if (payload.error === undefined && payload.state !== "failed") {
          delete next.error;
        }
        useForwardStore.getState().upsertForward(next);
      })
      .then((off) => {
        if (disposed) off();
        else unlisten = off;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // 会话选择兜底：没有显式选择时用 activeTab 的会话，再退到列表首项，
  // 避免「下拉框看起来选了一个、实际 state 为空」的错位。
  const effectiveSessionId =
    form.sessionId ||
    activeSessionId ||
    (sessionList.length > 0 ? sessionList[0].id : "");

  const handleStart = async () => {
    const listenPort = parsePort(form.listenPort);
    const remotePort = parsePort(form.remotePort);
    if (!effectiveSessionId) return setError(t("forward.errorNoSession"));
    if (listenPort === null) return setError(t("forward.errorLocalPortInt"));
    if (remotePort === null) return setError(t("forward.errorRemotePortInt"));
    if (!form.remoteHost.trim()) return setError(t("forward.errorRemoteHost"));

    setError(null);
    setSubmitting(true);
    try {
      const req: ForwardRequest = {
        sessionId: effectiveSessionId,
        kind: "local",
        listenHost: form.listenHost.trim() || DEFAULT_LISTEN_HOST,
        listenPort,
        remoteHost: form.remoteHost.trim(),
        remotePort,
      };
      const info = await api.startForward(req);
      upsertForward(info);
      setOpen(false);
      setForm((prev) => ({ ...prev, listenPort: String(listenPort + 1) }));
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSubmitting(false);
    }
  };

  const handleStop = async (id: string) => {
    setError(null);
    setStoppingId(id);
    try {
      await api.stopForward(id);
      removeForward(id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setStoppingId(null);
    }
  };

  return (
    <div className="flex max-h-[40vh] min-h-0 flex-col">
      <div className="flex shrink-0 items-center justify-between px-3 py-2">
        <div className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground">
          <Network className="size-3.5" />
          <span>{t("sidebar.portForward")}</span>
          <Badge variant="secondary" className="h-4 px-1 text-[10px]">
            {forwards.length}
          </Badge>
        </div>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-1.5"
          title={t("forward.add")}
          onClick={() => {
            setOpen((v) => !v);
            setError(null);
          }}
        >
          {open ? <ChevronUp className="size-4" /> : <Plus className="size-4" />}
        </Button>
      </div>

      {error ? (
        <div className="mx-2 mb-1 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1 text-[11px] break-all text-destructive">
          {error}
        </div>
      ) : null}

      {open ? (
        <div className="mx-2 mb-2 space-y-2 rounded-md border border-border bg-card/40 p-2">
          <div className="space-y-1">
            <Label className="text-[11px] font-normal text-muted-foreground">{t("forward.session")}</Label>
            <select
              className="h-7 w-full rounded-md border border-input bg-transparent px-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
              value={effectiveSessionId}
              onChange={(e) => patchForm({ sessionId: e.target.value })}
            >
              {sessionList.length === 0 ? (
                <option value="">{t("forward.noSessionOption")}</option>
              ) : null}
              {sessionList.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.username}@{s.host}:{s.port}（{s.state}）
                </option>
              ))}
            </select>
          </div>

          <div className="grid grid-cols-2 gap-2">
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("forward.localPort")}</Label>
              <Input
                className="h-7 text-xs"
                type="number"
                min={1}
                max={65535}
                value={form.listenPort}
                onChange={(e) => patchForm({ listenPort: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("forward.localAddress")}</Label>
              <Input
                className="h-7 text-xs"
                value={form.listenHost}
                onChange={(e) => patchForm({ listenHost: e.target.value })}
              />
            </div>
            <div className="col-span-2 space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("forward.remoteHost")}</Label>
              <Input
                className="h-7 text-xs"
                value={form.remoteHost}
                placeholder="127.0.0.1"
                onChange={(e) => patchForm({ remoteHost: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">{t("forward.remotePort")}</Label>
              <Input
                className="h-7 text-xs"
                type="number"
                min={1}
                max={65535}
                value={form.remotePort}
                onChange={(e) => patchForm({ remotePort: e.target.value })}
              />
            </div>
          </div>

          <div className="flex items-center justify-between">
            <span className="min-w-0 flex-1 truncate text-[10px] text-muted-foreground">
              :{form.listenPort || "?"} → {form.remoteHost || "?"}:{form.remotePort || "?"}
            </span>
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                className="h-7 shrink-0 text-xs"
                disabled={submitting || !effectiveSessionId}
                title={effectiveSessionId ? t("forward.startForward") : t("forward.noSessionHint")}
                onClick={() => void handleStart()}
              >
                {submitting ? <LoaderCircle className="size-3.5 animate-spin" /> : <Power className="size-3.5" />}
                启动
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-7 text-xs"
                disabled={submitting}
                onClick={() => {
                  setOpen(false);
                  setError(null);
                }}
              >
                {t("common.cancel")}
              </Button>
            </div>
          </div>
        </div>
      ) : null}

      <div className="min-h-0 flex-1 space-y-1 overflow-y-auto px-2 pb-2">
        {forwards.length === 0 ? (
          <div className="px-1 py-3 text-center text-[11px] text-muted-foreground">
            {t("forward.empty")}
          </div>
        ) : null}

        {forwards.map((f) => {
          const badge = STATE_BADGE[f.state];
          const stopping = stoppingId === f.id;
          return (
            <div
              key={f.id}
              className="rounded-md border border-transparent px-2 py-1.5 transition-colors hover:border-border hover:bg-accent/40"
            >
              <div className="flex items-center gap-1.5">
                <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                  :{f.listenPort}
                  <ArrowRight className="mx-0.5 inline size-2.5 text-muted-foreground" />
                  {f.remoteHost}:{f.remotePort}
                </span>
                <Badge
                  variant={badge.variant}
                  className={cn("h-4 shrink-0 px-1 text-[10px]", f.state === "running" && "animate-none")}
                >
                  {t(badge.labelKey)}
                </Badge>
                <Button
                  size="sm"
                  variant="ghost"
                  className="size-5 shrink-0 p-0 text-muted-foreground hover:text-destructive"
                  title={t("forward.stop")}
                  disabled={stopping}
                  onClick={() => void handleStop(f.id)}
                >
                  {stopping ? (
                    <LoaderCircle className="size-3 animate-spin" />
                  ) : (
                    <Power className="size-3" />
                  )}
                </Button>
              </div>
              {f.state === "failed" && f.error ? (
                <div className="mt-0.5 truncate text-[10px] text-destructive" title={f.error}>
                  {f.error}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}
