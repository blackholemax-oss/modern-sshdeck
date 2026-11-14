/**
 * modern-sshdeck — 前后端 IPC 契约
 *
 * 本文件是前端侧的唯一类型来源，与 `src-tauri/src/ipc/dto.rs` 一一对应。
 * 任何字段变更必须同步修改两个文件与 docs/ARCHITECTURE.md §5。
 */

// ───────────────────────────── 基础 ─────────────────────────────

export type SessionState =
  | "connecting"
  | "auth"
  | "connected"
  | "closed"
  | "failed";

export type ForwardState =
  | "starting"
  | "running"
  | "stopped"
  | "failed"
  /** 持久化规则还没拿到可用连接（等待凭据 / 等待登录） */
  | "pending"
  /** 底层连接断开，正在按退避策略重连 */
  | "reconnecting";

export type Risk = "read_only" | "mutating" | "dangerous";

/** 后端统一错误形状；所有 invoke 失败都会 reject 该对象。 */
export interface IpcError {
  code: string;
  message: string;
}

export const IpcErrorCode = {
  SSH_ERROR: "SSH_ERROR",
  /** 主机密钥校验失败（未知 / 变更 / 被 @revoked） */
  HOST_KEY_ERROR: "HOST_KEY_ERROR",
  FORWARD_ERROR: "FORWARD_ERROR",
  LLM_ERROR: "LLM_ERROR",
  AGENT_ERROR: "AGENT_ERROR",
  NOT_FOUND: "NOT_FOUND",
  INVALID_INPUT: "INVALID_INPUT",
  IO_ERROR: "IO_ERROR",
  PERSIST_ERROR: "PERSIST_ERROR",
} as const;

// ───────────────────────────── 配置 ─────────────────────────────

export interface LlmConfig {
  provider: "openai_compat";
  baseUrl: string;
  model: string;
  /** 前端永远拿不到明文 key，只知道是否已配置 */
  hasApiKey: boolean;
  temperature: number;
  /** 当前生效模型 id，形如 `provider/model`；空串 = 用 builtin 通道 */
  activeModel: string;
  channels: ChannelConfig[];
  combos: ComboConfig[];
}

/** 一家渠道的用户配置（Key 只在后端，前端只有 hasKey）。 */
export interface ChannelConfig {
  providerId: string;
  enabled: boolean;
  baseUrlOverride?: string;
  hasKey: boolean;
  modelOverrides?: string[];
}

export interface ComboConfig {
  name: string;
  strategy: "fallback";
  members: string[];
}

/** 注册表里的一家提供商（与用户配置合并后的视图）。 */
export interface ProviderDto {
  id: string;
  name: string;
  /** local | free | free_tier | api_key */
  category: string;
  baseUrl?: string;
  docsUrl?: string;
  note?: string;
  requiresKey: boolean;
  hasKey: boolean;
  enabled: boolean;
}

export type ProbeStatus =
  | "available"
  | "no_models"
  | "unreachable"
  | "unauthorized"
  | "catalog_only"
  | "unsupported";

export interface ProbeResultDto {
  providerId: string;
  status: ProbeStatus;
  message: string;
  models: string[];
  freeModelCount: number;
}

export interface CompressionStatsDto {
  beforeBytes: number;
  afterBytes: number;
  savedBytes: number;
  blocksCompressed: number;
  calls: number;
  savedRatio: number;
}

export interface AuditEntryDto {
  ts: number;
  sessionId: string;
  command: string;
  risk: string;
  decision: "auto" | "approved" | "rejected" | "timeout" | "aborted";
  reason?: string;
}

export interface TerminalConfig {
  fontSize: number;
  scrollback: number;
  /** 终端字体族覆盖；留空用内置字体栈 */
  fontFamily: string;
  /** 光标样式 */
  cursorStyle: "block" | "bar" | "underline";
  /** 光标是否闪烁 */
  cursorBlink: boolean;
}

export interface UiConfig {
  theme: "dark" | "light" | "system";
  /** 界面语言（当前仅中英） */
  /** "system"（跟随系统）| "zh" | "en" */
  language: "system" | "zh" | "en";
  /** 左栏宽度像素 */
  sidebarWidth: number;
  /** 右栏（Agent）宽度像素 */
  agentPanelWidth: number;
  /** Agent 面板是否显示 */
  showAgentPanel: boolean;
}

/** 主机密钥校验策略（与后端 `HostKeyPolicy` 对应）。 */
export type HostKeyPolicy = "strict" | "accept_new" | "insecure";

/** 主机密钥与 known_hosts 的比对结果。 */
export type HostKeyStatus = "match" | "unknown" | "mismatch" | "revoked";

/** 行为类开关。 */
export interface BehaviorConfig {
  /** 关闭标签前确认 */
  confirmOnCloseTab: boolean;
  /** 连接断开时播放提示音 */
  bellOnDisconnect: boolean;
  /** 连接失败时自动重试次数（0 = 不重试） */
  autoReconnectAttempts: number;
  /** 新建连接表单的默认端口 */
  defaultPort: number;
  /** 新建连接表单的默认用户名 */
  defaultUsername: string;
  /** 主机密钥校验策略 */
  hostKeyPolicy: HostKeyPolicy;
  /** 自定义 known_hosts 路径；空串 = ~/.ssh/known_hosts */
  knownHostsPath: string;
  /** 多路会话复用同一条 SSH 底层连接 */
  sessionReuse: boolean;
  /** 复用连接的空闲保留时长（秒，0 = 关闭即断） */
  sessionReuseIdleSecs: number;
  /** 启动 / 登录后自动恢复持久化的转发规则 */
  forwardAutoRestore: boolean;
  /** 转发断线重连的最大退避（秒） */
  forwardReconnectMaxSecs: number;
}

export interface AppConfig {
  llm: LlmConfig;
  terminal: TerminalConfig;
  ui: UiConfig;
  behavior: BehaviorConfig;
}

/** 写入配置时允许带上新 key（只写不读）。 */
export interface ChannelPatch {
  providerId: string;
  enabled?: boolean;
  baseUrlOverride?: string;
  apiKey?: string;
  clearApiKey?: boolean;
  modelOverrides?: string[];
}

export interface AppConfigPatch {
  llm?: {
    provider?: "openai_compat";
    baseUrl?: string;
    model?: string;
    apiKey?: string;
    temperature?: number;
    /** 显式清空已保存的 key */
    clearApiKey?: boolean;
    /** 切换当前生效模型（`provider/model`） */
    activeModel?: string;
    /** 渠道级增改 */
    channels?: ChannelPatch[];
    /** 降级链全量替换 */
    combos?: ComboConfig[];
  };
  terminal?: Partial<TerminalConfig>;
  ui?: Partial<UiConfig>;
  behavior?: Partial<BehaviorConfig>;
}

// ───────────────────────────── 主机 ─────────────────────────────

export interface HostProfile {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authType: "password" | "key";
  keyPath?: string;
  updatedAt: number;
}

// ───────────────────────────── SSH ─────────────────────────────

export type AuthSpec =
  | { kind: "password"; password: string }
  | { kind: "key"; keyPath: string; passphrase?: string };

export interface ConnectRequest {
  hostId?: string;
  host: string;
  port: number;
  username: string;
  auth: AuthSpec;
  cols: number;
  rows: number;
  /**
   * 用户在指纹确认卡里点「信任并连接」后带上：服务端公钥的 OpenSSH 文本。
   *
   * 只在「未知主机」时生效（写入 known_hosts 后放行）；与已有记录冲突时后端
   * 仍然拒绝连接。
   */
  trustHostKey?: string;
}

export interface SessionInfo {
  id: string;
  host: string;
  port: number;
  username: string;
  state: SessionState;
  createdAt: number;
  /** 服务端主机指纹（SHA256） */
  hostKeyFingerprint?: string;
  /** 主机密钥校验结果：verified / trusted / learned / insecure */
  hostKeyStatus?: "verified" | "trusted" | "learned" | "insecure";
  /** 共享的底层连接 id：同值 = 复用了同一条 SSH 传输（0 次握手） */
  linkId?: string;
}

/** `ssh_pool_stats` 返回：验证连接复用是否生效。 */
export interface LinkStat {
  id: string;
  host: string;
  port: number;
  username: string;
  /** 当前共享者数量 */
  sessions: number;
  /** 这条连接被复用的次数 */
  reuses: number;
  createdAt: number;
  idleMs: number;
}

export interface PoolStats {
  links: LinkStat[];
  /** 累计握手次数 */
  handshakes: number;
  /** 累计复用次数 */
  reuses: number;
  /** 当前可见会话数 */
  sessions: number;
}

/** `ssh_probe_host_key` 返回：只握手不认证，用于未知主机的指纹确认。 */
export interface HostKeyProbe {
  host: string;
  port: number;
  /** SHA256:... */
  fingerprint: string;
  /** ssh-ed25519 / rsa-sha2-256 等 */
  algorithm: string;
  /** 完整公钥（原样回传给 `trustHostKey`） */
  publicKey: string;
  knownHostsPath: string;
  policy: HostKeyPolicy;
  status: HostKeyStatus;
}

// ───────────────────────────── 端口转发 ─────────────────────────────

/**
 * `local`（-L）与 `remote`（-R）已实现；`dynamic`（-D / SOCKS）为预留值，
 * 后端会拒绝并提示。
 */
export type ForwardKind = "local" | "remote" | "dynamic";

export interface ForwardRequest {
  sessionId: string;
  kind: ForwardKind;
  /** -L：本机监听地址；-R：服务端监听地址 */
  listenHost: string;
  /** -L：本机监听端口；-R：服务端监听端口（0 = 由服务端分配） */
  listenPort: number;
  /** -L：远端目标地址；-R：本机侧目标地址 */
  remoteHost: string;
  /** -L：远端目标端口；-R：本机侧目标端口 */
  remotePort: number;
}

/** 修改规则：只传需要改的字段；运行中的规则会原地重启（保留 id）。 */
export interface ForwardPatch {
  sessionId?: string;
  kind?: ForwardKind;
  listenHost?: string;
  listenPort?: number;
  remoteHost?: string;
  remotePort?: number;
}

export interface ForwardInfo {
  id: string;
  /** 当前承载会话；`pending` 时为空串 */
  sessionId: string;
  /** 规则归属的主机档案 id */
  hostId?: string;
  /** 规则归属主机（跨重启持久化后仍可定位） */
  host: string;
  port: number;
  username: string;
  kind: ForwardKind;
  listenHost: string;
  listenPort: number;
  remoteHost: string;
  remotePort: number;
  state: ForwardState;
  /** 规则创建时间（毫秒），用于稳定排序 */
  createdAt: number;
  /** 已写入 state.json（重启后自动恢复） */
  persisted: boolean;
  /** 随启动 / 登录自动拉起 */
  autoStart: boolean;
  error?: string;
}

// ───────────────────────────── Agent ─────────────────────────────

export interface AgentSendRequest {
  runId: string;
  sessionId: string;
  message: string;
}

export interface AgentApproveRequest {
  approvalId: string;
  approved: boolean;
  /** 高危（Dangerous）命令的二次确认短语，必须精确等于「确认执行」 */
  confirmPhrase?: string;
}

export interface AgentModelInfo {
  id: string;
  ownedBy?: string;
}

// ───────────────────────────── 事件 payload ─────────────────────────────

export interface TerminalDataEvent {
  sessionId: string;
  /** base64 编码的输出块 */
  data: string;
}

export interface TerminalClosedEvent {
  sessionId: string;
  reason: string;
}

export interface SessionStateEvent {
  sessionId: string;
  state: SessionState;
  reason?: string;
}

export interface ForwardStateEvent {
  forwardId: string;
  state: ForwardState;
  error?: string;
}

export interface AgentDeltaEvent {
  runId: string;
  delta: string;
}

export interface AgentToolCallEvent {
  runId: string;
  callId: string;
  name: string;
  args: string;
}

export interface AgentToolResultEvent {
  runId: string;
  callId: string;
  ok: boolean;
  output: string;
}

export interface AgentApprovalRequiredEvent {
  runId: string;
  approvalId: string;
  command: string;
  risk: Risk;
  reason: string;
}

export interface AgentDoneEvent {
  runId: string;
  reason: string;
}

export interface AgentErrorEvent {
  runId: string;
  code: string;
  message: string;
}

// ───────────────────────────── 事件名常量 ─────────────────────────────

export const Events = {
  terminalData: "terminal:data",
  terminalClosed: "terminal:closed",
  sessionState: "session:state",
  forwardState: "forward:state",
  agentDelta: "agent:delta",
  agentToolCall: "agent:tool_call",
  agentToolResult: "agent:tool_result",
  agentApprovalRequired: "agent:approval_required",
  agentDone: "agent:done",
  agentError: "agent:error",
} as const;

/** 远程目录项。 */
export interface RemoteEntry {
  name: string;
  path: string;
  /** file | dir | symlink | other */
  kind: string;
  size: number;
  modified: number;
  mode: number;
}

/** 目录列表。 */
export interface DirListing {
  path: string;
  parent?: string;
  entries: RemoteEntry[];
}

/** 文件预览。 */
export interface FilePreview {
  path: string;
  size: number;
  isText: boolean;
  content?: string;
  truncated: boolean;
  reason?: string;
}

// ───────────────────────────── 命令名常量 ─────────────────────────────

export const Commands = {
  appGetConfig: "app_get_config",
  appSetConfig: "app_set_config",
  hostsList: "hosts_list",
  hostsUpsert: "hosts_upsert",
  hostsDelete: "hosts_delete",
  sshConnect: "ssh_connect",
  sshDisconnect: "ssh_disconnect",
  sshListSessions: "ssh_list_sessions",
  sshProbeHostKey: "ssh_probe_host_key",
  sshPoolStats: "ssh_pool_stats",
  terminalInput: "terminal_input",
  terminalResize: "terminal_resize",
  forwardStart: "forward_start",
  forwardStop: "forward_stop",
  forwardGet: "forward_get",
  forwardList: "forward_list",
  forwardUpdate: "forward_update",
  forwardRetry: "forward_retry",
  agentSend: "agent_send",
  agentAbort: "agent_abort",
  agentApprove: "agent_approve",
  agentListModels: "agent_list_models",
  agentReset: "agent_reset",
  llmProviders: "llm_providers",
  llmProbeChannel: "llm_probe_channel",
  llmDiscoverModels: "llm_discover_models",
  tokenStats: "token_stats",
  tokenStatsReset: "token_stats_reset",
  auditTail: "audit_tail",
  auditCount: "audit_count",
  fsListDir: "fs_list_dir",
  fsReadFile: "fs_read_file",
  fsDownload: "fs_download",
  fsUpload: "fs_upload",
} as const;
