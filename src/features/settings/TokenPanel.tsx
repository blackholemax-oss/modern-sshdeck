/**
 * 省 Token 与审批审计面板。
 *
 * 两者都是「只读观测面」：显示压缩节省与审批留痕，不做任何控制。
 * 数据来自后端统计累计器与 audit.jsonl。
 */
import { useEffect, useState } from "react";
import { RefreshCw, Trash2, ShieldCheck, ShieldAlert, ShieldQuestion } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { api, SshdeckError } from "@/shared/api";
import type { AuditEntryDto, CompressionStatsDto } from "@/shared/ipc";

/** 字节数的可读格式。 */
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

const DECISION_META: Record<
  AuditEntryDto["decision"],
  { label: string; variant: "default" | "secondary" | "destructive" | "outline" | "success" }
> = {
  auto: { label: "自动执行", variant: "secondary" },
  approved: { label: "已批准", variant: "success" },
  rejected: { label: "已拒绝", variant: "destructive" },
  timeout: { label: "超时", variant: "outline" },
  aborted: { label: "已中止", variant: "outline" },
};

const RISK_META: Record<string, { label: string; icon: React.ReactNode; cls: string }> = {
  read_only: { label: "只读", icon: <ShieldCheck className="size-3" />, cls: "text-emerald-500" },
  mutating: { label: "变更", icon: <ShieldQuestion className="size-3" />, cls: "text-amber-500" },
  dangerous: { label: "高危", icon: <ShieldAlert className="size-3" />, cls: "text-destructive" },
};

export function TokenPanel() {
  const [stats, setStats] = useState<CompressionStatsDto | null>(null);
  const [audits, setAudits] = useState<AuditEntryDto[]>([]);
  const [count, setCount] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const reload = async () => {
    try {
      const [s, a, c] = await Promise.all([
        api.tokenStats(),
        api.auditTail(30),
        api.auditCount(),
      ]);
      setStats(s);
      setAudits(a);
      setCount(c);
      setError(null);
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
    }
  };

  useEffect(() => {
    void reload();
    // 每 5 秒刷新一次，观察运行中的压缩效果
    const t = setInterval(() => void reload(), 5000);
    return () => clearInterval(t);
  }, []);

  const ratioPct = stats ? (stats.savedRatio * 100).toFixed(1) : "0.0";

  return (
    <div className="space-y-4">
      {error ? (
        <div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}

      {/* 压缩统计 */}
      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold">工具输出压缩（RTK）</h4>
          <div className="flex items-center gap-1">
            <Button variant="ghost" size="sm" onClick={() => void reload()}>
              <RefreshCw className="size-3.5" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              title="重置统计"
              onClick={async () => {
                await api.tokenStatsReset();
                void reload();
              }}
            >
              <Trash2 className="size-3.5" />
            </Button>
          </div>
        </div>

        {stats && stats.calls > 0 ? (
          <>
            <div className="flex items-baseline gap-2">
              <span className="text-2xl font-semibold tabular-nums">{ratioPct}%</span>
              <span className="text-[11px] text-muted-foreground">已节省</span>
            </div>
            <div className="grid grid-cols-2 gap-2 text-[11px]">
              <Stat label="压缩前" value={fmtBytes(stats.beforeBytes)} />
              <Stat label="压缩后" value={fmtBytes(stats.afterBytes)} />
              <Stat label="节省" value={fmtBytes(stats.savedBytes)} />
              <Stat label="压缩块数" value={`${stats.blocksCompressed} / ${stats.calls} 次`} />
            </div>
          </>
        ) : (
          <p className="text-[11px] text-muted-foreground">
            还没有压缩记录。Agent 执行工具后，大输出会自动被压缩以节省 token。
          </p>
        )}

        <p className="text-[10px] leading-relaxed text-muted-foreground">
          规则：报错输出永不压缩；小于 2 KB 不处理；压缩后更大则回退；整条管线幂等。
          压缩只影响送模型的上下文，界面里显示给你的仍是原始输出。
        </p>
      </section>

      <Separator />

      {/* 审批审计 */}
      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold">
            审批审计 <span className="text-muted-foreground">({count})</span>
          </h4>
          <Button variant="ghost" size="sm" onClick={() => void reload()}>
            <RefreshCw className="size-3.5" />
          </Button>
        </div>

        {audits.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            还没有审批记录。Agent 执行命令后，每次放行/拒绝都会留痕。
          </p>
        ) : (
          <div className="space-y-1">
            {audits
              .slice()
              .reverse()
              .map((a, i) => {
                const risk = RISK_META[a.risk] ?? {
                  label: a.risk,
                  icon: null,
                  cls: "text-muted-foreground",
                };
                const dec = DECISION_META[a.decision] ?? {
                  label: a.decision,
                  variant: "outline" as const,
                };
                return (
                  <div
                    key={`${a.ts}-${i}`}
                    className="rounded border border-border px-2 py-1.5 text-[11px]"
                  >
                    <div className="flex items-center gap-1.5">
                      <span className={risk.cls}>{risk.icon}</span>
                      <span className="text-muted-foreground">{risk.label}</span>
                      <Badge variant={dec.variant}>{dec.label}</Badge>
                      <span className="ml-auto font-mono text-[10px] text-muted-foreground">
                        {new Date(a.ts).toLocaleTimeString()}
                      </span>
                    </div>
                    <pre className="mt-1 overflow-x-auto font-mono text-[10px] whitespace-pre-wrap text-foreground/80">
                      {a.command}
                    </pre>
                    {a.reason ? (
                      <p className="mt-0.5 text-[10px] text-muted-foreground">{a.reason}</p>
                    ) : null}
                  </div>
                );
              })}
          </div>
        )}
      </section>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded border border-border px-2 py-1.5">
      <div className="text-[10px] text-muted-foreground">{label}</div>
      <div className="font-mono tabular-nums">{value}</div>
    </div>
  );
}
