import { useEffect, useRef, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { api, events } from "@/shared/api";
import { useAgentStore } from "@/stores/agent";

export function ToolCallCard({
  name,
  args,
  output,
  ok,
}: {
  name: string;
  args: string;
  output?: string;
  ok?: boolean;
}) {
  let command = args;
  try {
    const parsed = JSON.parse(args) as { command?: string };
    if (parsed.command) command = parsed.command;
  } catch {
    /* keep raw */
  }

  return (
    <div className="my-2 rounded-lg border border-border bg-muted/40 p-2.5 text-xs">
      <div className="mb-1.5 flex items-center gap-2">
        <Badge variant="secondary" className="font-mono">
          {name}
        </Badge>
        {ok === undefined ? (
          <Badge variant="outline">执行中…</Badge>
        ) : ok ? (
          <Badge variant="success">成功</Badge>
        ) : (
          <Badge variant="destructive">失败</Badge>
        )}
      </div>
      <pre className="mb-1 overflow-x-auto rounded bg-background/60 px-2 py-1 font-mono text-[11px] text-foreground/80">
        {command}
      </pre>
      {output ? (
        <pre className="max-h-40 overflow-auto rounded bg-background/60 px-2 py-1 font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
          {output}
        </pre>
      ) : null}
    </div>
  );
}

/** 高危命令必须精确输入这个短语才放行（与后端 `guard::confirm` 保持一致）。 */
const CONFIRM_PHRASE = "确认执行";

/** 审批卡片：变更/高危命令的人工放行闸门。 */
export function ApprovalCard() {
  const approval = useAgentStore((s) => s.approval);
  const setApproval = useAgentStore((s) => s.setApproval);
  const sendApproval = useAgentStore((s) => s.sendApproval);

  // 高危命令的二次确认输入。必须在所有 hook 之后才能早退，
  // 否则 approval 从有到无时 hook 数量变化会违反 React 规则。
  const [phrase, setPhrase] = useState("");

  if (!approval) return null;

  const isDangerous = approval.risk === "dangerous";
  const phraseOk = !isDangerous || phrase.trim() === CONFIRM_PHRASE;

  const riskBadge = isDangerous ? (
    <Badge variant="destructive">高危命令</Badge>
  ) : (
    <Badge variant="outline">变更操作</Badge>
  );

  const decide = (approved: boolean) => {
    // 前端只负责把短语原样带上；判定始终在后端 `check_confirm` 里做。
    void sendApproval(approval.approvalId, approved, isDangerous ? phrase.trim() : undefined);
    setPhrase("");
    setApproval(null);
  };

  return (
    <div className="mt-3 rounded-lg border-2 border-destructive/50 bg-destructive/5 p-3">
      <div className="mb-2 flex items-center gap-2">
        {riskBadge}
        <span className="text-xs text-muted-foreground">需要人工确认</span>
      </div>
      <pre className="mb-2 overflow-x-auto rounded bg-background/70 px-2 py-1.5 font-mono text-xs">
        {approval.command}
      </pre>
      <p className="mb-2.5 text-xs text-muted-foreground">{approval.reason}</p>

      {isDangerous ? (
        <div className="mb-2.5 rounded-md border border-destructive/40 bg-background/60 p-2">
          <p className="mb-1.5 text-[11px] text-destructive">
            该命令不可逆或影响面很大。请手动输入
            <code className="mx-1 rounded bg-destructive/10 px-1 font-semibold">
              {CONFIRM_PHRASE}
            </code>
            以确认执行：
          </p>
          <input
            value={phrase}
            onChange={(e) => setPhrase(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && phraseOk) decide(true);
            }}
            placeholder={CONFIRM_PHRASE}
            className="w-full rounded border border-input bg-transparent px-2 py-1 font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
        </div>
      ) : null}

      <div className="flex gap-2">
        <button
          type="button"
          disabled={!phraseOk}
          className="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
          onClick={() => decide(true)}
        >
          {isDangerous ? "确认执行" : "允许一次"}
        </button>
        <button
          type="button"
          className="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent"
          onClick={() => decide(false)}
        >
          拒绝
        </button>
      </div>
    </div>
  );
}

/** Agent 事件接线：面板顶层挂一次即可。 */
export function useAgentEventBridge() {
  const mountedRef = useRef(false);

  useEffect(() => {
    if (mountedRef.current) return;
    mountedRef.current = true;

    const unsubs: Array<Promise<() => void>> = [
      events.onAgentDelta((p) => useAgentStore.getState().appendDelta(p.delta)),
      events.onAgentToolCall((p) =>
        useAgentStore.getState().addToolCall(p.callId, p.name, p.args),
      ),
      events.onAgentToolResult((p) =>
        useAgentStore.getState().setToolResult(p.callId, p.ok, p.output),
      ),
      events.onAgentApprovalRequired((p) => useAgentStore.getState().setApproval(p)),
      events.onAgentDone(() => useAgentStore.getState().finishAssistant()),
      events.onAgentError((p) => {
        useAgentStore.getState().setError(`${p.code}: ${p.message}`);
        useAgentStore.getState().finishAssistant();
      }),
    ];

    return () => {
      mountedRef.current = false;
      unsubs.forEach((p) => {
        p.then((un) => un()).catch(() => undefined);
      });
    };
  }, []);
}

/** 发送审批结论到后端。 */
export async function submitApproval(approvalId: string, approved: boolean) {
  await api.agentApprove({ approvalId, approved });
}
