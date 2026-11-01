import { useEffect, useRef, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { api, SshdeckError } from "@/shared/api";
import { useAgentStore } from "@/stores/agent";
import { useSessionsStore } from "@/stores/sessions";

import { ApprovalCard, ToolCallCard, useAgentEventBridge } from "./AgentCards";

const RUN_ID = "default-run";

export function AgentPanel() {
  useAgentEventBridge();

  const messages = useAgentStore((s) => s.messages);
  const streaming = useAgentStore((s) => s.streaming);
  const error = useAgentStore((s) => s.error);
  const appendUser = useAgentStore((s) => s.appendUser);
  const startAssistant = useAgentStore((s) => s.startAssistant);
  const setError = useAgentStore((s) => s.setError);

  const tabs = useSessionsStore((s) => s.tabs);
  const activeTabId = useSessionsStore((s) => s.activeTabId);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const listRef = useRef<HTMLDivElement | null>(null);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const sessionId = activeTab?.panes.find((p): p is string => Boolean(p)) ?? null;

  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  const submit = async () => {
    const text = input.trim();
    if (!text || sending) return;
    if (!sessionId) {
      setError("请先在终端中连接一个会话，Agent 需要绑定会话才能执行命令。");
      return;
    }

    setError(null);
    setInput("");
    setSending(true);
    appendUser(text);
    startAssistant();

    try {
      await api.agentSend({ runId: RUN_ID, sessionId, message: text });
    } catch (err) {
      const msg = err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err);
      useAgentStore.getState().setError(msg);
      useAgentStore.getState().finishAssistant();
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-border px-4 py-3">
        <div className="flex items-center gap-2">
          <span className="text-sm font-semibold">运维 Agent</span>
          {streaming ? <Badge variant="outline">思考中…</Badge> : null}
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={async () => {
            useAgentStore.getState().reset();
            try {
              await api.agentReset(RUN_ID);
            } catch {
              /* 忽略：不影响前端清空 */
            }
          }}
        >
          清空
        </Button>
      </header>

      <div ref={listRef} className="flex-1 space-y-4 overflow-y-auto px-4 py-4">
        {messages.length === 0 ? (
          <div className="mt-8 text-center text-xs text-muted-foreground">
            <p className="mb-2">可以让 Agent 做什么？</p>
            <p className="mb-1">· 「看下磁盘使用情况」</p>
            <p className="mb-1">· 「这台机器的内存还剩多少」</p>
            <p>· 「分析下最近的应用错误日志」</p>
          </div>
        ) : null}

        {messages.map((m) => (
          <div key={m.id}>
            <div className="mb-1 text-[11px] font-medium text-muted-foreground">
              {m.role === "user" ? "你" : "Agent"}
            </div>
            <div
              className={
                m.role === "user"
                  ? "rounded-lg bg-primary/10 px-3 py-2 text-sm whitespace-pre-wrap"
                  : "rounded-lg border border-border px-3 py-2 text-sm whitespace-pre-wrap"
              }
            >
              {m.content}
              {m.pending && !m.content ? (
                <span className="text-muted-foreground">正在生成…</span>
              ) : null}
            </div>
            {m.toolCalls.map((t) => (
              <ToolCallCard
                key={t.callId}
                name={t.name}
                args={t.args}
                output={t.output}
                ok={t.ok}
              />
            ))}
          </div>
        ))}

        <ApprovalCard />

        {error ? (
          <div className="rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-xs text-destructive">
            {error}
          </div>
        ) : null}
      </div>

      <footer className="border-t border-border p-3">
        {sessionId ? (
          <div className="mb-2 text-[11px] text-muted-foreground">
            绑定会话：{activeTab?.title ?? sessionId}
          </div>
        ) : (
          <div className="mb-2 text-[11px] text-amber-500">
            未绑定会话 —— 先在终端连接一台主机
          </div>
        )}
        <Textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void submit();
            }
          }}
          placeholder="描述你想做的运维操作…（Enter 发送，Shift+Enter 换行）"
          rows={3}
          className="resize-none"
        />
        <div className="mt-2 flex justify-end">
          <Button size="sm" onClick={() => void submit()} disabled={sending || !input.trim()}>
            发送
          </Button>
        </div>
      </footer>
    </div>
  );
}
