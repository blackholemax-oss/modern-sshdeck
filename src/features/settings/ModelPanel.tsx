/**
 * 模型接入面板（把原「渠道」与「Agent」两个选项卡合并为一个）。
 *
 * ## 设计原则
 *
 * 用户不需要关心「渠道」和「Agent」是两个概念 —— 他只想回答一个问题：
 * **这个应用该用哪个模型。** 所以这里只保留一条主线：
 *
 * ```
 * 接入方式
 *   ├── 渠道接入（优先）── 从 39 家注册表里选一家 + 选模型
 *   └── OpenAI 协议兜底 ── 自填 base_url + model + Key
 * ```
 *
 * ## 「渠道优先，OpenAI 兜底」如何落地
 *
 * 后端 `router::parse_model_id` 已经把这条规则实现了：
 *
 * - `activeModel` 带已知 provider 前缀（如 `ollama/qwen3:8b`）→ 走**渠道**
 * - `activeModel` 不带前缀、或前缀不是已知 provider（如 `deepseek-chat`）
 *   → 走 **`builtin`**，也就是「用 `baseUrl` + `apiKey` 直连的
 *   OpenAI 兼容端点」
 *
 * 也就是说：**没选渠道时，系统本来就自动走 OpenAI 协议接入** ——
 * 底层规则已经正确，缺的只是让界面把这件事说清楚、并且让用户能选择。
 * 这里做的就是把那个隐式行为变成显式的开关。
 *
 * ## 状态判定
 *
 * 界面不猜、不推导，一律用 `activeModel` 里有没有已知 provider 前缀来判定。
 * 这与后端的解析结果**完全一致**，不会出现「界面显示渠道、实际走兜底」的偏差。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CheckCircle2,
  KeyRound,
  RefreshCw,
  XCircle,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { api, SshdeckError } from "@/shared/api";
import type { ProbeResultDto, ProbeStatus, ProviderDto } from "@/shared/ipc";
import { useSettingsStore } from "@/stores/settings";

const CATEGORY_META: Record<string, { labelKey: string; order: number }> = {
  local: { labelKey: "settings.model.categoryLocal", order: 0 },
  free: { labelKey: "settings.model.categoryFree", order: 1 },
  free_tier: { labelKey: "settings.model.categoryFreeTier", order: 2 },
  api_key: { labelKey: "settings.model.categoryApiKey", order: 3 },
};

const CATEGORY_ORDER = ["local", "free", "free_tier", "api_key"];

const STATUS_META: Record<
  ProbeStatus,
  { icon: React.ReactNode; cls: string; labelKey: string }
> = {
  available: { icon: <CheckCircle2 className="size-3.5" />, cls: "text-emerald-500", labelKey: "settings.model.statusAvailable" },
  no_models: { icon: <AlertCircle className="size-3.5" />, cls: "text-amber-500", labelKey: "settings.model.statusNoModels" },
  unreachable: { icon: <XCircle className="size-3.5" />, cls: "text-destructive", labelKey: "settings.model.statusUnreachable" },
  unauthorized: { icon: <KeyRound className="size-3.5" />, cls: "text-amber-500", labelKey: "settings.model.statusUnauthorized" },
  catalog_only: { icon: <AlertCircle className="size-3.5" />, cls: "text-amber-500", labelKey: "settings.model.statusCatalogOnly" },
  unsupported: { icon: <XCircle className="size-3.5" />, cls: "text-muted-foreground", labelKey: "settings.model.statusUnsupported" },
};

/** 接入方式。 */
type Mode = "channel" | "openai";

export function ModelPanel() {
  const { t } = useTranslation();
  const config = useSettingsStore((s) => s.config);
  const patch = useSettingsStore((s) => s.patch);

  const [providers, setProviders] = useState<ProviderDto[]>([]);
  const [loadingProviders, setLoadingProviders] = useState(true);
  const [probing, setProbing] = useState<string | null>(null);
  const [probes, setProbes] = useState<Record<string, ProbeResultDto>>({});
  const [keyDraft, setKeyDraft] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  // 记住最近一次成功使用的渠道，便于「从兜底切回渠道」时延续选择。
  // 用 ref 而非 state：它只是「记住上次」，不需要触发重渲染，
  // 也不会因为 mode 在 channel↔openai 间摆动而被清空。
  const lastProviderRef = useRef<string>("");

  // OpenAI 兜底区的草稿
  const [baseUrlDraft, setBaseUrlDraft] = useState<string | null>(null);
  const [modelDraft, setModelDraft] = useState("");
  const [openaiKeyDraft, setOpenaiKeyDraft] = useState("");
  const [discovered, setDiscovered] = useState<string[]>([]);
  const [discovering, setDiscovering] = useState(false);

  const llm = config?.llm;
  const activeModel = llm?.activeModel ?? "";
  const channels = useMemo(() => llm?.channels ?? [], [llm?.channels]);

  // ── 判定当前接入方式：与后端 `router::parse_model_id` 完全一致 ──
  //
  // 带已知 provider 前缀 → 渠道；否则 → OpenAI 兜底（builtin）。
  const activeProviderId = useMemo(() => {
    const head = activeModel.split("/")[0]?.trim() ?? "";
    if (!head) return "";
    // `builtin/xxx` 是显式的兜底写法
    if (head === "builtin") return "";
    return providers.some((p) => p.id === head) ? head : "";
  }, [activeModel, providers]);

  // 记录最近一次处于渠道模式的 provider，供切回时复用
  useEffect(() => {
    if (activeProviderId) lastProviderRef.current = activeProviderId;
  }, [activeProviderId]);

  const mode: Mode = activeProviderId ? "channel" : "openai";
  const activeProvider = providers.find((p) => p.id === activeProviderId);
  // 「纯模型名」：剥掉 `provider/` 或 `builtin/` 前缀。
  // ⚠️ 之前这里只剥 `builtin/`，导致渠道模式下该值是完整串
  // （`ollama/qwen3:8b`），切到兜底模式时把整串写进了 activeModel，
  // 结果仍被判定为渠道 —— 「切不过去」的根因。
  const activeModelName = activeProviderId
    ? activeModel.slice(activeModel.indexOf("/") + 1)
    : activeModel.replace(/^builtin\//, "");

  const reloadProviders = async () => {
    setLoadingProviders(true);
    try {
      setProviders(await api.llmProviders());
      setError(null);
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
    } finally {
      setLoadingProviders(false);
    }
  };

  useEffect(() => {
    void reloadProviders();
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

  /** 某渠道可选的模型列表（探测结果 ∪ 已配置的覆盖 ∪ 当前正在用的）。 */
  const modelsFor = (p: ProviderDto): string[] => {
    const list = new Set<string>();
    for (const m of probes[p.id]?.models ?? []) list.add(m);
    for (const m of channels.find((c) => c.providerId === p.id)?.modelOverrides ?? []) list.add(m);
    if (p.id === activeProviderId && activeModelName) list.add(activeModelName);
    return Array.from(list);
  };

  const grouped = CATEGORY_ORDER.map((cat) => ({
    cat,
    items: providers
      .filter((p) => p.category === cat)
      .sort((a, b) => a.name.localeCompare(b.name)),
  })).filter((g) => g.items.length > 0);

  /** 切到「OpenAI 协议兜底」：把 activeModel 写成**不带前缀**的形式。 */
  const switchToOpenAi = () => {
    // 优先用上次在兜底模式下用过的模型（`llm.model`），其次沿用当前纯模型名。
    // 写进去的一定是**不带 provider 前缀**的形式，否则后端仍判为渠道。
    const m = (llm?.model || activeModelName || "").trim();
    setModelDraft(m);
    // 直接写：`activeModel` 不带 provider 前缀 → 后端 `parse_model_id` 归到 builtin，
    // 即「用 baseUrl + apiKey 直连的 OpenAI 兼容端点」。
    void patch({ llm: { activeModel: m } });
  };

  /** 切到某个渠道（用已探测到的首个模型，或该渠道的覆盖模型）。 */
  const switchToChannel = (providerId: string, model?: string) => {
    const ch = channels.find((c) => c.providerId === providerId);

    // 模型来源优先级：
    //  ① 调用方指定
    //  ② 该渠道已配置的模型覆盖列表（用户之前选过的）
    //  ③ 刚探测到的首个模型
    //  ④ **当前正在用的模型**（关键：切模式时不该把已知可用的模型丢掉）
    const first =
      model ||
      ch?.modelOverrides?.[0] ||
      probes[providerId]?.models?.[0] ||
      // 关键兜底：即便没探测过，也沿用当前纯模型名 ——
      // 它本来就是这家渠道的模型（用户之前就是从这里选的）
      activeModelName;

    if (!first) {
      setError(
        t("settings.model.errorProbeFirst", {
          name: providers.find((p) => p.id === providerId)?.name ?? providerId,
        }),
      );
      return;
    }
    void patch({ llm: { activeModel: `${providerId}/${first}` } });
    setError(null);
  };

  const discoverOpenAi = async () => {
    setDiscovering(true);
    try {
      const list = await api.agentListModels();
      setDiscovered(list.map((m) => m.id));
      setError(null);
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
    } finally {
      setDiscovering(false);
    }
  };

  const baseUrlValue = baseUrlDraft ?? llm?.baseUrl ?? "";

  return (
    <div className="space-y-4">
      {/* ─────────── 当前状态条：一句话说明「现在用的是哪条路」 ─────────── */}
      <div className="rounded-lg border border-border bg-muted/30 px-3 py-2">
        <div className="flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">{t("settings.model.currentLabel")}</span>
          {mode === "channel" ? (
            <>
              <Badge variant="secondary">{t("settings.model.modeChannel")}</Badge>
              <span className="font-medium">{activeProvider?.name ?? activeProviderId}</span>
            </>
          ) : (
            <>
              <Badge variant="outline">{t("settings.model.modeOpenAI")}</Badge>
              <span className="font-medium">{t("settings.model.directEndpoint")}</span>
            </>
          )}
        </div>
        <div className="mt-1 flex items-center gap-1.5 text-[11px] text-muted-foreground">
          <span className="font-mono">{activeModel || t("settings.model.notSet")}</span>
          {mode === "channel" && activeProvider ? (
            <span className="font-mono">@ {activeProvider.baseUrl ?? t("settings.model.customEndpoint")}</span>
          ) : (
            <span className="font-mono">@ {baseUrlValue || t("settings.model.notSetAddress")}</span>
          )}
        </div>
      </div>

      {error ? (
        <div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-[11px] text-destructive">
          {error}
        </div>
      ) : null}

      {/* ─────────── 接入方式选择 ─────────── */}
      <div className="space-y-1.5">
        <Label className="text-xs">{t("settings.model.accessMode")}</Label>
        <div className="grid grid-cols-2 gap-2">
          {(
            [
              {
                key: "channel" as Mode,
                titleKey: "settings.model.channelTitle",
                descKey: "settings.model.channelDesc",
              },
              {
                key: "openai" as Mode,
                titleKey: "settings.model.openaiTitle",
                descKey: "settings.model.openaiDesc",
              },
            ]
          ).map((o) => {
            const on = mode === o.key;
            return (
              <button
                key={o.key}
                type="button"
                onClick={() => {
                  if (o.key === "openai") {
                    switchToOpenAi();
                    return;
                  }
                  // 切回渠道：优先延续当前渠道；否则用最近一次用过的渠道（记住的 provider）
                  if (activeProviderId) {
                    switchToChannel(activeProviderId);
                    return;
                  }
                  // 记忆来源（按可靠性排序）：
                  //  ① 本次会话用过 / 上次处于渠道模式的 provider
                  //  ② 已保存过配置（Key 或端点覆盖）的渠道
                  //  ③ **靠 baseUrl 反查**：兜底模式下用户填的地址若与某家渠道的
                  //     默认端点一致，那它显然就是这家渠道（最可靠的推断）
                  //  ④ 唯一「本机」渠道
                  //  ⑤ 探测到可用模型的第一个渠道
                  const norm = (u?: string) =>
                    (u ?? "").replace(/\/+$/, "").replace(/\/v1$/, "").toLowerCase();
                  const byBaseUrl = norm(llm?.baseUrl);

                  // ⚠️ `baseUrl` 可能仍是出厂默认（如 api.deepseek.com）而从未被用户改过，
                  // 只按它推断会把 Ollama 的模型错挂到 DeepSeek 上（实测踩过）。
                  // 因此顺序是：记忆 → 明确配置过的渠道 → baseUrl 命中的**本机/免费**渠道
                  // → 探测到可用模型的渠道。付费渠道仅在没有其他线索时才用。
                  const direct = providers.find(
                    (p) => p.baseUrl && norm(p.baseUrl) === byBaseUrl,
                  );
                  const directIsLocalOrFree =
                    direct && (direct.category === "local" || direct.category === "free");

                  // 推断当前模型属于哪家渠道：用模型名做匹配（探测到的清单最准）
                  const byModel = Object.entries(probes).find(([, r]) =>
                    r?.models?.includes(activeModelName),
                  )?.[0];

                  const remembered =
                    lastProviderRef.current ||
                    channels.find((c) => c.enabled && c.hasKey)?.providerId ||
                    byModel ||
                    (directIsLocalOrFree ? direct.id : undefined) ||
                    (providers.filter((p) => p.category === "local").length === 1
                      ? providers.find((p) => p.category === "local")?.id
                      : undefined) ||
                    Object.keys(probes).find((id) => (probes[id]?.models?.length ?? 0) > 0) ||
                    (direct ? direct.id : undefined) ||
                    "";
                  if (remembered) {
                    switchToChannel(remembered);
                    return;
                  }
                  // 什么都没配置过：停在渠道列表里，提示用户选一家
                  setError(t("settings.model.errorPickChannel"));
                }}
                className={`rounded-md border px-2.5 py-2 text-left transition-colors ${
                  on
                    ? "border-primary bg-primary/5"
                    : "border-border hover:bg-accent/40"
                }`}
              >
                <span className="flex items-center gap-1.5 text-[11px] font-medium">
                  <span
                    className={`inline-block size-2 rounded-full ${
                      on ? "bg-primary" : "border border-muted-foreground/60"
                    }`}
                  />
                  {t(o.titleKey)}
                </span>
                <span className="mt-0.5 block text-[10px] leading-relaxed text-muted-foreground">
                  {t(o.descKey)}
                </span>
              </button>
            );
          })}
        </div>
        <p className="text-[10px] leading-relaxed text-muted-foreground">
          {t("settings.model.accessModeHint")}
        </p>
      </div>

      <Separator />

      {/* ─────────── 模式 A：渠道接入 ─────────── */}
      {mode === "channel" ? (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h4 className="text-xs font-semibold">{t("settings.model.chooseChannel")}</h4>
            <Button variant="ghost" size="sm" onClick={() => void reloadProviders()} disabled={loadingProviders}>
              <RefreshCw className={loadingProviders ? "size-3.5 animate-spin" : "size-3.5"} />
            </Button>
          </div>

          {grouped.map(({ cat, items }) => (
            <section key={cat} className="space-y-1.5">
              <div className="flex items-baseline gap-2">
                <span className="text-[11px] font-medium">{CATEGORY_META[cat] ? t(CATEGORY_META[cat].labelKey) : cat}</span>
                <span className="text-[10px] text-muted-foreground">
                  {t("settings.model.providersCount", { count: items.length })}
                </span>
              </div>

              {items.map((p) => {
                const probe = probes[p.id];
                const isProbing = probing === p.id;
                const isActive = p.id === activeProviderId;

                return (
                  <div
                    key={p.id}
                    className={`rounded-lg border px-2.5 py-2 text-xs ${
                      isActive ? "border-primary/60 bg-primary/5" : "border-border"
                    }`}
                  >
                    <div className="flex items-center gap-2">
                      <span className="font-medium">{p.name}</span>
                      {isActive ? <Badge variant="secondary">{t("settings.model.current")}</Badge> : null}
                      {p.hasKey ? <Badge variant="outline">{t("settings.model.keyStoredShort")}</Badge> : null}
                      {probe ? (
                        <span className={`ml-auto flex items-center gap-1 ${STATUS_META[probe.status]?.cls}`}>
                          {STATUS_META[probe.status]?.icon}
                          {probe ? t(STATUS_META[probe.status].labelKey) : ""}
                        </span>
                      ) : null}
                    </div>

                    <div className="mt-2 flex flex-wrap items-center gap-1.5">
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-7 text-[11px]"
                        disabled={isProbing}
                        onClick={() => void doProbe(p)}
                      >
                        {isProbing ? t("settings.model.testing") : t("settings.model.testConnection")}
                      </Button>

                      {modelsFor(p) .length > 0 ? (
                        <Select
                          value={isActive ? activeModelName : undefined}
                          onValueChange={(m) => switchToChannel(p.id, m)}
                        >
                          <SelectTrigger className="h-7 w-44 text-[11px]">
                            <SelectValue placeholder={t("settings.model.modelsCount", { count: modelsFor(p).length })} />
                          </SelectTrigger>
                          <SelectContent>
                            {modelsFor(p).map((m) => (
                              <SelectItem key={m} value={m} className="text-[11px]">
                                {m}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      ) : (
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 text-[11px] text-muted-foreground"
                          onClick={() => void doProbe(p)}
                        >
                          {t("settings.model.chooseModelAfterProbe")}
                        </Button>
                      )}

                      {modelsFor(p).length > 0 && !isActive ? (
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-7 text-[11px]"
                          onClick={() => switchToChannel(p.id)}
                        >
                          {t("settings.model.setAsCurrent")}
                        </Button>
                      ) : null}

                      {p.requiresKey && !p.hasKey ? (
                        <>
                          <Input
                            type="password"
                            className="h-7 w-36 text-[11px]"
                            placeholder={t("settings.model.keyPlaceholder")}
                            value={keyDraft[p.id] ?? ""}
                            onChange={(e) => setKeyDraft((prev) => ({ ...prev, [p.id]: e.target.value }))}
                          />
                          <Button
                            variant="outline"
                            size="sm"
                            className="h-7 text-[11px]"
                            disabled={!(keyDraft[p.id] ?? "").trim()}
                            onClick={() => {
                              void patch({
                                llm: { channels: [{ providerId: p.id, apiKey: keyDraft[p.id].trim() }] },
                              });
                              setKeyDraft((prev) => ({ ...prev, [p.id]: "" }));
                            }}
                          >
                            {t("common.save")}
                          </Button>
                        </>
                      ) : null}

                      {p.hasKey ? (
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 text-[11px] text-muted-foreground"
                          onClick={() =>
                            void patch({ llm: { channels: [{ providerId: p.id, clearApiKey: true }] } })
                          }
                        >
                          清除 Key
                        </Button>
                      ) : null}
                    </div>

                    {probe ? (
                      <p className={`mt-1 text-[10px] ${STATUS_META[probe.status]?.cls}`}>{probe.message}</p>
                    ) : p.note ? (
                      <p className="mt-1 text-[10px] leading-relaxed text-muted-foreground">{p.note}</p>
                    ) : null}
                  </div>
                );
              })}
            </section>
          ))}
        </div>
      ) : (
        /* ─────────── 模式 B：OpenAI 协议兜底 ─────────── */
        <div className="space-y-4">
          <section className="space-y-1.5">
            <Label className="text-xs">{t("settings.model.baseUrl")}</Label>
            <Input
              placeholder={t("settings.model.baseUrlPlaceholder")}
              value={baseUrlValue}
              onChange={(e) => setBaseUrlDraft(e.target.value)}
              onBlur={() => {
                const v = (baseUrlDraft ?? "").trim();
                if (v && v !== llm?.baseUrl) patch({ llm: { baseUrl: v } });
              }}
              className="text-xs"
            />
            <p className="text-[10px] text-muted-foreground">
              {t("settings.model.baseUrlHint")}
            </p>
          </section>

          <section className="space-y-1.5">
            <Label className="text-xs">{t("settings.model.model")}</Label>
            <div className="flex items-center gap-1.5">
              <Input
                placeholder={t("settings.model.modelPlaceholder")}
                value={modelDraft || activeModelName}
                onChange={(e) => setModelDraft(e.target.value)}
                onBlur={() => {
                  const v = modelDraft.trim();
                  if (v && v !== activeModelName) patch({ llm: { activeModel: v } });
                }}
                className="text-xs"
              />
              <Button
                variant="outline"
                size="sm"
                className="h-7 shrink-0 text-[11px]"
                disabled={discovering}
                onClick={() => void discoverOpenAi()}
              >
                {discovering ? t("settings.model.testing") : t("settings.model.discover")}
              </Button>
            </div>
            {discovered.length > 0 ? (
              <Select
                value={activeModelName}
                onValueChange={(m) => {
                  setModelDraft(m);
                  void patch({ llm: { activeModel: m } });
                }}
              >
                <SelectTrigger className="h-7 text-[11px]">
                  <SelectValue placeholder={t("settings.model.discoveredCount", { count: discovered.length })} />
                </SelectTrigger>
                <SelectContent>
                  {discovered.map((m) => (
                    <SelectItem key={m} value={m} className="text-[11px]">
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : null}
          </section>

          <section className="space-y-1.5">
            <Label className="text-xs">API Key</Label>
            {llm?.hasApiKey ? (
              <div className="flex items-center justify-between rounded-md border border-border px-3 py-2">
                <span className="text-[11px] text-muted-foreground">{t("settings.model.apiKeyConfigured")}</span>
                <Button variant="ghost" size="sm" className="h-7 text-[11px]" onClick={() => patch({ llm: { clearApiKey: true } })}>
                  清除
                </Button>
              </div>
            ) : (
              <div className="flex items-center gap-1.5">
                <Input
                  type="password"
                  placeholder={t("settings.model.apiKeyPlaceholder")}
                  value={openaiKeyDraft}
                  onChange={(e) => setOpenaiKeyDraft(e.target.value)}
                  className="text-xs"
                />
                <Button
                  size="sm"
                  className="h-8 shrink-0 text-[11px]"
                  disabled={!openaiKeyDraft.trim()}
                  onClick={() => {
                    patch({ llm: { apiKey: openaiKeyDraft.trim() } });
                    setOpenaiKeyDraft("");
                  }}
                >
                  {t("common.save")}
                </Button>
              </div>
            )}
            <p className="text-[10px] text-muted-foreground">
              {t("settings.model.apiKeyHint")}
            </p>
          </section>

          {!llm?.hasApiKey && baseUrlValue.includes("127.0.0.1") === false ? (
            <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-600 dark:text-amber-400">
              {t("settings.model.needsKeyWarning")}
            </div>
          ) : null}
        </div>
      )}

      <Separator />

      {/* ─────────── 与接入方式无关的通用参数 ─────────── */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between">
          <Label className="text-xs">{t("settings.model.temperature")}</Label>
          <span className="font-mono text-[11px] text-muted-foreground">
            {(llm?.temperature ?? 0.2).toFixed(2)}
          </span>
        </div>
        <input
          type="range"
          className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-muted accent-primary"
          value={Math.round((llm?.temperature ?? 0.2) * 100)}
          min={0}
          max={200}
          step={5}
          onChange={(e) => patch({ llm: { temperature: Number(e.target.value) / 100 } })}
        />
        <p className="text-[10px] text-muted-foreground">
          {t("settings.model.temperatureHint")}
        </p>
      </section>
    </div>
  );
}
