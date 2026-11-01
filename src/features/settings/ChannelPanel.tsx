/**
 * 聚合渠道面板：提供商注册表 + 探测 + Key 管理。
 *
 * 分组依据 `category`：本机（免 Key）→ 免费层 → 需 Key → OAuth（不可驱动）。
 * 只有「本机」与已验证的通道支持真探测；OAuth 类明确标注不可驱动，
 * 不提供任何伪造指纹或登录劫持的路径。
 */
import { useEffect, useState } from "react";
import { RefreshCw, CheckCircle2, AlertCircle, XCircle, Download, KeyRound } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api, SshdeckError } from "@/shared/api";
import type { ProbeResultDto, ProbeStatus, ProviderDto } from "@/shared/ipc";
import { useSettingsStore } from "@/stores/settings";

/** 分类的中文名与展示顺序。 */
const CATEGORY_META: Record<string, { label: string; hint: string }> = {
  local: { label: "本机模型", hint: "无需 Key、完全免费，需本机服务在运行" },
  free: { label: "免费通道", hint: "无需 Key 即可调用" },
  free_tier: { label: "免费额度", hint: "需 Key，但有免费额度" },
  api_key: { label: "需 API Key", hint: "需自备 Key" },
  oauth: { label: "OAuth（不可驱动）", hint: "需交互式登录，本应用不驱动" },
};

const CATEGORY_ORDER = ["local", "free", "free_tier", "api_key", "oauth"];

const STATUS_META: Record<ProbeStatus, { icon: React.ReactNode; cls: string; label: string }> = {
  available: {
    icon: <CheckCircle2 className="size-3.5" />,
    cls: "text-emerald-500",
    label: "可用",
  },
  no_models: {
    icon: <AlertCircle className="size-3.5" />,
    cls: "text-amber-500",
    label: "无模型",
  },
  unreachable: {
    icon: <XCircle className="size-3.5" />,
    cls: "text-destructive",
    label: "连不上",
  },
  unauthorized: {
    icon: <KeyRound className="size-3.5" />,
    cls: "text-amber-500",
    label: "Key 无效",
  },
  catalog_only: {
    icon: <AlertCircle className="size-3.5" />,
    cls: "text-amber-500",
    label: "仅目录",
  },
  unsupported: {
    icon: <XCircle className="size-3.5" />,
    cls: "text-muted-foreground",
    label: "不可驱动",
  },
};

export function ChannelPanel() {
  const config = useSettingsStore((s) => s.config);
  const patch = useSettingsStore((s) => s.patch);

  const [providers, setProviders] = useState<ProviderDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [probing, setProbing] = useState<string | null>(null);
  const [probes, setProbes] = useState<Record<string, ProbeResultDto>>({});
  const [keyDraft, setKeyDraft] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);

  const activeModel = config?.llm.activeModel ?? "";
  const channels = config?.llm.channels ?? [];

  const reload = async () => {
    setLoading(true);
    try {
      const list = await api.llmProviders();
      setProviders(list);
      setError(null);
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
  }, []);

  const doProbe = async (p: ProviderDto) => {
    setProbing(p.id);
    try {
      const r = await api.llmProbeChannel(
        p.id,
        channels.find((c) => c.providerId === p.id)?.baseUrlOverride,
      );
      setProbes((prev) => ({ ...prev, [p.id]: r }));
    } catch (err) {
      setProbes((prev) => ({
        ...prev,
        [p.id]: {
          providerId: p.id,
          status: "unreachable",
          message: err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err),
          models: [],
          freeModelCount: 0,
        },
      }));
    } finally {
      setProbing(null);
    }
  };

  const grouped = CATEGORY_ORDER.map((cat) => ({
    cat,
    items: providers.filter((p) => p.category === cat),
  })).filter((g) => g.items.length > 0);

  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-2">
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          选择模型时用 <code className="rounded bg-muted px-1">provider/model</code> 形式。
          本机通道无需 Key，是最稳的免费路径。
        </p>
        <Button variant="ghost" size="sm" onClick={() => void reload()} disabled={loading}>
          <RefreshCw className={loading ? "size-3.5 animate-spin" : "size-3.5"} />
        </Button>
      </div>

      {error ? (
        <div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {error}
        </div>
      ) : null}

      {grouped.map(({ cat, items }) => {
        const meta = CATEGORY_META[cat] ?? { label: cat, hint: "" };
        return (
          <section key={cat} className="space-y-2">
            <div className="flex items-baseline gap-2">
              <h4 className="text-xs font-semibold">{meta.label}</h4>
              <span className="text-[10px] text-muted-foreground">{meta.hint}</span>
            </div>

            <div className="space-y-1.5">
              {items.map((p) => {
                const probe = probes[p.id];
                const chCfg = channels.find((c) => c.providerId === p.id);
                const isProbing = probing === p.id;
                const usable = probe?.status === "available";
                const drivable = p.category !== "oauth";

                return (
                  <div
                    key={p.id}
                    className="rounded-lg border border-border px-2.5 py-2 text-xs"
                  >
                    <div className="flex items-center gap-2">
                      <span className="font-medium">{p.name}</span>
                      <span className="font-mono text-[10px] text-muted-foreground">
                        {p.id}/
                      </span>
                      {p.hasKey ? <Badge variant="secondary">已配置 Key</Badge> : null}
                      {probe ? (
                        <span className={`ml-auto flex items-center gap-1 ${STATUS_META[probe.status]?.cls}`}>
                          {STATUS_META[probe.status]?.icon}
                          {STATUS_META[probe.status]?.label}
                        </span>
                      ) : null}
                    </div>

                    {p.note ? (
                      <p className="mt-1 text-[10px] leading-relaxed text-muted-foreground">
                        {p.note}
                      </p>
                    ) : null}

                    {probe ? (
                      <p className={`mt-1 text-[10px] ${STATUS_META[probe.status]?.cls}`}>
                        {probe.message}
                      </p>
                    ) : null}

                    {drivable ? (
                      <div className="mt-2 flex flex-wrap items-center gap-1.5">
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-7 text-[11px]"
                          disabled={isProbing}
                          onClick={() => void doProbe(p)}
                        >
                          {isProbing ? "探测中…" : "测试连通"}
                        </Button>

                        {usable ? (
                          <Button
                            size="sm"
                            className="h-7 text-[11px]"
                            onClick={() =>
                              void patch({
                                llm: {
                                  activeModel: `${p.id}/${probe.models[0]}`,
                                },
                              })
                            }
                          >
                            设为当前模型
                          </Button>
                        ) : null}

                        {p.requiresKey && !p.hasKey ? (
                          <>
                            <Input
                              type="password"
                              className="h-7 w-40 text-[11px]"
                              placeholder="粘贴 Key"
                              value={keyDraft[p.id] ?? ""}
                              onChange={(e) =>
                                setKeyDraft((prev) => ({ ...prev, [p.id]: e.target.value }))
                              }
                            />
                            <Button
                              variant="outline"
                              size="sm"
                              className="h-7 text-[11px]"
                              disabled={!(keyDraft[p.id] ?? "").trim()}
                              onClick={() => {
                                void patch({
                                  llm: {
                                    channels: [
                                      {
                                        providerId: p.id,
                                        apiKey: keyDraft[p.id].trim(),
                                        enabled: true,
                                      },
                                    ],
                                  },
                                });
                                setKeyDraft((prev) => ({ ...prev, [p.id]: "" }));
                              }}
                            >
                              保存
                            </Button>
                          </>
                        ) : null}

                        {p.hasKey ? (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="h-7 text-[11px] text-muted-foreground"
                            onClick={() =>
                              void patch({
                                llm: {
                                  channels: [{ providerId: p.id, clearApiKey: true }],
                                },
                              })
                            }
                          >
                            清除 Key
                          </Button>
                        ) : null}

                        {!p.baseUrl ? (
                          <Input
                            className="h-7 w-52 font-mono text-[10px]"
                            placeholder="需手工填 Base URL"
                            defaultValue={chCfg?.baseUrlOverride ?? ""}
                            onBlur={(e) => {
                              const v = e.target.value.trim();
                              if (v !== (chCfg?.baseUrlOverride ?? "")) {
                                void patch({
                                  llm: {
                                    channels: [{ providerId: p.id, baseUrlOverride: v }],
                                  },
                                });
                              }
                            }}
                          />
                        ) : null}
                      </div>
                    ) : (
                      <p className="mt-1.5 text-[10px] text-muted-foreground">
                        本应用不代为完成 OAuth 登录，仅列出供参考。
                      </p>
                    )}
                  </div>
                );
              })}
            </div>
          </section>
        );
      })}

      {/* 当前生效模型 */}
      <section className="space-y-1.5 border-t border-border pt-3">
        <Label className="text-xs">当前生效模型</Label>
        <div className="flex items-center gap-2">
          <Input
            className="font-mono text-xs"
            placeholder="provider/model，留空则用下方 base URL + model"
            defaultValue={activeModel}
            onBlur={(e) => {
              const v = e.target.value.trim();
              if (v !== activeModel) void patch({ llm: { activeModel: v } });
            }}
          />
          {usableModelHint(activeModel)}
        </div>
        {activeModel ? (
          <p className="text-[10px] text-muted-foreground">
            形如 <code>ollama/qwen2.5:0.5b</code>；切换后新对话立即生效。
          </p>
        ) : (
          <p className="text-[10px] text-amber-600 dark:text-amber-400">
            未选择模型 —— 将回退到下方「服务地址 + 模型」的 builtin 通道
          </p>
        )}
      </section>
    </div>
  );
}

function usableModelHint(active: string): React.ReactNode {
  if (!active) return null;
  const ok = /^[a-z0-9_-]+\//.test(active);
  return ok ? (
    <Badge variant="success" className="shrink-0">
      已选择
    </Badge>
  ) : (
    <Badge variant="outline" className="shrink-0">
      格式?
    </Badge>
  );
}

/** 给外部复用的下载图标（避免未使用告警）。 */
export const DownloadIcon = Download;
