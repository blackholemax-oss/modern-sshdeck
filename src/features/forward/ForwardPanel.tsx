/**
 * 左栏下半：端口转发规则管理（完整 CRUD）。
 *
 * - 查：挂载时 / 点刷新时走 `api.listForwards()`；点「编辑」时用 `api.getForward(id)`
 *   拉一次最新状态（列表可能是几秒前的快照）。
 * - 增：表单内选 `local(-L)` / `remote(-R)`，启动走 `api.startForward`。
 * - 改：同一表单进入编辑模式，走 `api.updateForward(id, patch)`（后端原地重启，
 *   保留 id；失败时规则仍保留为 `failed`，不会丢）。
 * - 删：行内操作菜单（每条规则一个可展开按钮，内含「编辑 / 删除」）
 *   → `api.stopForward`，成功后从本地列表移除。
 *
 * 订阅 `forward:state` 就地更新状态（含 failed 的 error 文本）；会话关闭时
 * 后端会把状态置为 stopped，这里只做展示降级。
 */
import { useEffect, useState } from "react";
import {
  ArrowLeftRight,
  ArrowRight,
  ChevronDown,
  ChevronUp,
  LoaderCircle,
  Network,
  Pencil,
  Plus,
  Power,
  RefreshCw,
} from "lucide-react";

import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { api, events, SshdeckError } from "@/shared/api";
import type {
  ForwardInfo,
  ForwardKind,
  ForwardRequest,
  ForwardState,
} from "@/shared/ipc";
import { useForwardStore } from "@/stores/forward";
import { useSessionsStore } from "@/stores/sessions";

const DEFAULT_LISTEN_HOST = "127.0.0.1";
const DEFAULT_TARGET_HOST = "127.0.0.1";

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
  kind: Exclude<ForwardKind, "dynamic">;
  sessionId: string;
  listenHost: string;
  listenPort: string;
  remoteHost: string;
  remotePort: string;
}

const EMPTY_FORM: ForwardForm = {
  kind: "local",
  sessionId: "",
  listenHost: DEFAULT_LISTEN_HOST,
  listenPort: "8080",
  remoteHost: DEFAULT_TARGET_HOST,
  remotePort: "80",
};

function formFromInfo(info: ForwardInfo): ForwardForm {
  return {
    kind: info.kind === "remote" ? "remote" : "local",
    sessionId: info.sessionId,
    listenHost: info.listenHost,
    listenPort: String(info.listenPort),
    remoteHost: info.remoteHost,
    remotePort: String(info.remotePort),
  };
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
  const activeTab = tabs.find((tab) => tab.id === activeTabId);
  const activeSessionId = activeTab?.panes.find((p): p is string => p !== null) ?? "";

  const sessionList = Object.values(sessions);

  const [open, setOpen] = useState(false);
  /** 正在编辑的规则 id；null = 新建 */
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<ForwardForm>(EMPTY_FORM);
  const [submitting, setSubmitting] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 展开中的行内操作菜单（规则 id）；同一时刻只允许一个 */
  const [menuId, setMenuId] = useState<string | null>(null);

  const patchForm = (patch: Partial<ForwardForm>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  // 行内菜单：点外部 / 按 Esc 收起。用 pointerdown 而非 click，
  // 这样点另一个规则的操作按钮时是先关旧菜单再开新菜单，不会互相打架。
  useEffect(() => {
    if (!menuId) return;
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as HTMLElement | null;
      if (target?.closest(`[data-forward-menu="${menuId}"]`)) return;
      setMenuId(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuId(null);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [menuId]);

  /** 拉取规则列表（查询）。 */
  const refresh = async (silent = false) => {
    if (!silent) setRefreshing(true);
    try {
      const list = await api.listForwards();
      useForwardStore.getState().setForwards(list);
      if (!silent) setError(null);
    } catch (err) {
      if (!silent) setError(errorText(err));
    } finally {
      if (!silent) setRefreshing(false);
    }
  };

  // ① 初始化：拉取现存转发规则。
  useEffect(() => {
    void refresh(true);
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

  const isRemote = form.kind === "remote";

  const resetEditor = () => {
    setOpen(false);
    setEditingId(null);
    setError(null);
    setForm((prev) => ({ ...EMPTY_FORM, kind: prev.kind, sessionId: prev.sessionId }));
  };

  const startCreate = () => {
    setEditingId(null);
    setError(null);
    setForm({
      ...EMPTY_FORM,
      sessionId: effectiveSessionId,
      listenPort: String(
        Math.min(
          65535,
          forwards.reduce((max, f) => Math.max(max, f.listenPort), 8079) + 1,
        ),
      ),
    });
    setOpen(true);
  };

  /** 进入编辑：先向后端查一次最新状态，避免拿过期快照覆盖。 */
  const startEdit = async (id: string) => {
    setError(null);
    setBusyId(id);
    try {
      const info = await api.getForward(id);
      upsertForward(info);
      setEditingId(id);
      setForm(formFromInfo(info));
      setOpen(true);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusyId(null);
    }
  };

  const handleSubmit = async () => {
    const listenPort = parsePort(form.listenPort);
    const remotePort = parsePort(form.remotePort);
    if (!effectiveSessionId) return setError(t("forward.errorNoSession"));
    if (listenPort === null) return setError(t("forward.errorLocalPortInt"));
    if (remotePort === null) return setError(t("forward.errorRemotePortInt"));
    if (!form.listenHost.trim()) return setError(t("forward.errorListenHost"));
    if (!form.remoteHost.trim()) return setError(t("forward.errorRemoteHost"));

    setError(null);
    setSubmitting(true);
    try {
      const payload = {
        kind: form.kind,
        listenHost: form.listenHost.trim(),
        listenPort,
        remoteHost: form.remoteHost.trim(),
        remotePort,
      };

      let info: ForwardInfo;
      if (editingId) {
        info = await api.updateForward(editingId, {
          ...payload,
          sessionId: effectiveSessionId,
        });
      } else {
        const req: ForwardRequest = { sessionId: effectiveSessionId, ...payload };
        info = await api.startForward(req);
      }

      upsertForward(info);
      if (info.state === "failed") {
        setError(info.error ?? t("forward.startFailed"));
      } else {
        resetEditor();
      }
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSubmitting(false);
    }
  };

  const handleStop = async (id: string) => {
    setError(null);
    setBusyId(id);
    try {
      await api.stopForward(id);
      removeForward(id);
      if (editingId === id) resetEditor();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="flex max-h-[42vh] min-h-0 flex-col">
      <div className="flex shrink-0 items-center justify-between px-3 py-2">
        <div className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground">
          <Network className="size-3.5" />
          <span>{t("sidebar.portForward")}</span>
          <Badge variant="secondary" className="h-4 px-1 text-[10px]">
            {forwards.length}
          </Badge>
        </div>
        <div className="flex items-center gap-0.5">
          <Button
            variant="ghost"
            size="sm"
            className="size-7 p-0"
            title={t("forward.refresh")}
            disabled={refreshing}
            onClick={() => void refresh()}
          >
            <RefreshCw className={cn("size-3.5", refreshing && "animate-spin")} />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 px-1.5"
            title={t("forward.add")}
            onClick={() => {
              if (open && !editingId) resetEditor();
              else startCreate();
            }}
          >
            {open && !editingId ? <ChevronUp className="size-4" /> : <Plus className="size-4" />}
          </Button>
        </div>
      </div>

      {error ? (
        <div className="mx-2 mb-1 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1 text-[11px] break-all text-destructive">
          {error}
        </div>
      ) : null}

      {open ? (
        <div className="mx-2 mb-2 space-y-2 rounded-md border border-border bg-card/40 p-2">
          <div className="flex items-center gap-1">
            {(
              [
                { kind: "local" as const, labelKey: "forward.kindLocal" },
                { kind: "remote" as const, labelKey: "forward.kindRemote" },
              ]
            ).map(({ kind, labelKey }) => (
              <Button
                key={kind}
                size="sm"
                variant={form.kind === kind ? "secondary" : "ghost"}
                className="h-6 px-2 text-[11px]"
                onClick={() => patchForm({ kind })}
              >
                {t(labelKey)}
              </Button>
            ))}
            <span className="ml-auto truncate text-[10px] text-muted-foreground">
              {editingId ? t("forward.editing") : t("forward.creating")}
            </span>
          </div>

          <div className="space-y-1">
            <Label className="text-[11px] font-normal text-muted-foreground">
              {t("forward.session")}
            </Label>
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
              <Label className="text-[11px] font-normal text-muted-foreground">
                {isRemote ? t("forward.listenHostRemote") : t("forward.localAddress")}
              </Label>
              <Input
                className="h-7 text-xs"
                value={form.listenHost}
                onChange={(e) => patchForm({ listenHost: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">
                {isRemote ? t("forward.listenPortRemote") : t("forward.localPort")}
              </Label>
              <Input
                className="h-7 text-xs"
                type="number"
                min={1}
                max={65535}
                value={form.listenPort}
                onChange={(e) => patchForm({ listenPort: e.target.value })}
              />
            </div>
            <div className="col-span-2 space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">
                {isRemote ? t("forward.targetHostRemote") : t("forward.remoteHost")}
              </Label>
              <Input
                className="h-7 text-xs"
                value={form.remoteHost}
                placeholder="127.0.0.1"
                onChange={(e) => patchForm({ remoteHost: e.target.value })}
              />
            </div>
            <div className="space-y-1">
              <Label className="text-[11px] font-normal text-muted-foreground">
                {t("forward.remotePort")}
              </Label>
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
            <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-muted-foreground">
              {isRemote ? "R " : "L "}
              {form.listenHost || "?"}:{form.listenPort || "?"}
              <ArrowRight className="mx-0.5 inline size-2.5" />
              {form.remoteHost || "?"}:{form.remotePort || "?"}
            </span>
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                className="h-7 shrink-0 text-xs"
                disabled={submitting || !effectiveSessionId}
                title={effectiveSessionId ? t("forward.startForward") : t("forward.noSessionHint")}
                onClick={() => void handleSubmit()}
              >
                {submitting ? (
                  <LoaderCircle className="size-3.5 animate-spin" />
                ) : (
                  <Power className="size-3.5" />
                )}
                {editingId ? t("common.save") : t("forward.start")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-7 text-xs"
                disabled={submitting}
                onClick={resetEditor}
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
          const busy = busyId === f.id;
          const remote = f.kind === "remote";
          const menuOpen = menuId === f.id;
          return (
            <div
              key={f.id}
              data-forward-menu={f.id}
              className={cn(
                "rounded-md border px-2 py-1.5 transition-colors",
                menuOpen
                  ? "border-border bg-accent/40"
                  : "border-transparent hover:border-border hover:bg-accent/40",
              )}
            >
              <div className="flex items-center gap-1.5">
                <Badge
                  variant={remote ? "default" : "secondary"}
                  className="h-4 shrink-0 px-1 font-mono text-[10px]"
                  title={remote ? t("forward.kindRemoteHint") : t("forward.kindLocalHint")}
                >
                  {remote ? "-R" : "-L"}
                </Badge>
                <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                  {f.listenHost}:{f.listenPort}
                  <ArrowLeftRight className="mx-0.5 inline size-2.5 text-muted-foreground" />
                  {f.remoteHost}:{f.remotePort}
                </span>
                <Badge
                  variant={badge.variant}
                  className={cn("h-4 shrink-0 px-1 text-[10px]", f.state === "running" && "animate-none")}
                >
                  {t(badge.labelKey)}
                </Badge>
                {/* 编辑 / 删除合并为一个可展开的操作按钮 */}
                <Button
                  size="sm"
                  variant="ghost"
                  className={cn(
                    "size-5 shrink-0 p-0 text-muted-foreground hover:text-foreground",
                    menuOpen && "bg-accent text-foreground",
                  )}
                  title={t("forward.actions")}
                  aria-haspopup="menu"
                  aria-expanded={menuOpen}
                  disabled={busy}
                  onClick={() => setMenuId(menuOpen ? null : f.id)}
                >
                  {busy ? (
                    <LoaderCircle className="size-3 animate-spin" />
                  ) : (
                    <ChevronDown
                      className={cn("size-3.5 transition-transform", menuOpen && "rotate-180")}
                    />
                  )}
                </Button>
              </div>

              {menuOpen ? (
                <div role="menu" className="mt-1 space-y-0.5 border-t border-border/70 pt-1">
                  <button
                    type="button"
                    role="menuitem"
                    className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] text-foreground hover:bg-accent"
                    onClick={() => {
                      setMenuId(null);
                      void startEdit(f.id);
                    }}
                  >
                    <Pencil className="size-3 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 truncate">{t("forward.edit")}</span>
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] text-destructive hover:bg-destructive/10"
                    onClick={() => {
                      setMenuId(null);
                      void handleStop(f.id);
                    }}
                  >
                    <Power className="size-3 shrink-0" />
                    <span className="min-w-0 truncate">{t("forward.stop")}</span>
                  </button>
                </div>
              ) : null}

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
