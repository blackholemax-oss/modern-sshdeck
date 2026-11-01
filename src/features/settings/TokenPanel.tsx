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
import { useTranslation } from "react-i18next";

/** 字节数的可读格式。 */
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

const DECISION_META: Record<
  AuditEntryDto["decision"],
  { labelKey: string; variant: "default" | "secondary" | "destructive" | "outline" | "success" }
> = {
  auto: { labelKey: "settings.token.audit.auto", variant: "secondary" },
  approved: { labelKey: "settings.token.audit.approved", variant: "success" },
  rejected: { labelKey: "settings.token.audit.rejected", variant: "destructive" },
  timeout: { labelKey: "settings.token.audit.timeout", variant: "outline" },
  aborted: { labelKey: "settings.token.audit.aborted", variant: "outline" },
};

const RISK_META: Record<string, { labelKey: string; icon: React.ReactNode; cls: string }> = {
  read_only: { labelKey: "settings.token.risk.readOnly", icon: <ShieldCheck className="size-3" />, cls: "text-emerald-500" },
  mutating: { labelKey: "settings.token.risk.mutating", icon: <ShieldQuestion className="size-3" />, cls: "text-amber-500" },
  dangerous: { labelKey: "settings.token.risk.dangerous", icon: <ShieldAlert className="size-3" />, cls: "text-destructive" },
};

export function TokenPanel() {
  const { t } = useTranslation();
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
          <h4 className="text-xs font-semibold">{t("settings.token.title")}</h4>
          <div className="flex items-center gap-1">
            <Button variant="ghost" size="sm" onClick={() => void reload()}>
              <RefreshCw className="size-3.5" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              title={t("settings.token.resetStats")}
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
              <span className="text-[11px] text-muted-foreground">{t("settings.token.saved")}</span>
            </div>
            <div className="grid grid-cols-2 gap-2 text-[11px]">
              <Stat label={t("settings.token.beforeAfter")} value={fmtBytes(stats.beforeBytes)} />
              <Stat label={t("settings.token.after")} value={fmtBytes(stats.afterBytes)} />
              <Stat label={t("settings.token.savedAmount")} value={fmtBytes(stats.savedBytes)} />
              <Stat label={t("settings.token.compressedBlocks")} value={`${stats.blocksCompressed} / ${stats.calls}`} />
            </div>
          </>
        ) : (
          <p className="text-[11px] text-muted-foreground">
            {t("settings.token.noStats")}
          </p>
        )}

        <p className="text-[10px] leading-relaxed text-muted-foreground">
          {t("settings.token.rules")}
          {t("settings.token.displayNote")}
        </p>
      </section>

      <Separator />

      {/* 审批审计 */}
      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold">
            {t("settings.token.auditTitle")} <span className="text-muted-foreground">({count})</span>
          </h4>
          <Button variant="ghost" size="sm" onClick={() => void reload()}>
            <RefreshCw className="size-3.5" />
          </Button>
        </div>

        {audits.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            {t("settings.token.auditEmpty")}
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
                      <span className="text-muted-foreground">{risk.labelKey ? t(risk.labelKey) : ""}</span>
                      <Badge variant={dec.variant}>{dec.labelKey ? t(dec.labelKey) : ""}</Badge>
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
