/**
 * IPC 调用与事件订阅封装。
 *
 * 规则：
 * - 所有命令统一经过 `invokeCommand`，把后端 `IpcError` 归一化后抛 `SshdeckError`。
 * - 事件订阅统一用 `onEvent`，返回取消订阅函数，便于 React effect 清理。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as contract from "./ipc";

export type { contract as IpcContract };

export class SshdeckError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "SshdeckError";
    this.code = code;
  }
}

function isIpcError(value: unknown): value is contract.IpcError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as contract.IpcError).code === "string" &&
    typeof (value as contract.IpcError).message === "string"
  );
}

/** 把任意 reject 值归一化为 SshdeckError。 */
export function normalizeError(raw: unknown): SshdeckError {
  if (raw instanceof SshdeckError) return raw;
  if (isIpcError(raw)) return new SshdeckError(raw.code, raw.message);
  if (raw instanceof Error) {
    return new SshdeckError(contract.IpcErrorCode.IO_ERROR, raw.message);
  }
  return new SshdeckError(contract.IpcErrorCode.IO_ERROR, String(raw));
}

async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw normalizeError(raw);
  }
}

/** 事件订阅；返回取消函数。 */
async function onEvent<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload));
}

// ───────────────────────────── 命令 ─────────────────────────────

export const api = {
  // 配置
  getConfig: () =>
    invokeCommand<contract.AppConfig>(contract.Commands.appGetConfig),
  setConfig: (patch: contract.AppConfigPatch) =>
    invokeCommand<contract.AppConfig>(contract.Commands.appSetConfig, {
      config: patch,
    }),

  // 主机
  listHosts: () =>
    invokeCommand<contract.HostProfile[]>(contract.Commands.hostsList),
  upsertHost: (host: contract.HostProfile) =>
    invokeCommand<contract.HostProfile>(contract.Commands.hostsUpsert, { host }),
  deleteHost: (id: string) =>
    invokeCommand<void>(contract.Commands.hostsDelete, { id }),

  // SSH
  connect: (req: contract.ConnectRequest) =>
    invokeCommand<contract.SessionInfo>(contract.Commands.sshConnect, { req }),
  disconnect: (sessionId: string) =>
    invokeCommand<void>(contract.Commands.sshDisconnect, { sessionId }),
  listSessions: () =>
    invokeCommand<contract.SessionInfo[]>(contract.Commands.sshListSessions),
  sendInput: (sessionId: string, data: string) =>
    invokeCommand<void>(contract.Commands.terminalInput, { sessionId, data }),
  resize: (sessionId: string, cols: number, rows: number) =>
    invokeCommand<void>(contract.Commands.terminalResize, {
      sessionId,
      cols,
      rows,
    }),

  // 端口转发
  startForward: (req: contract.ForwardRequest) =>
    invokeCommand<contract.ForwardInfo>(contract.Commands.forwardStart, { req }),
  stopForward: (forwardId: string) =>
    invokeCommand<void>(contract.Commands.forwardStop, { forwardId }),
  listForwards: () =>
    invokeCommand<contract.ForwardInfo[]>(contract.Commands.forwardList),

  // Agent
  agentSend: (req: contract.AgentSendRequest) =>
    invokeCommand<void>(contract.Commands.agentSend, { req }),
  agentAbort: (runId: string) =>
    invokeCommand<void>(contract.Commands.agentAbort, { runId }),
  agentApprove: (req: contract.AgentApproveRequest) =>
    invokeCommand<void>(contract.Commands.agentApprove, { req }),
  agentListModels: () =>
    invokeCommand<contract.AgentModelInfo[]>(contract.Commands.agentListModels),
  agentReset: (runId: string) =>
    invokeCommand<void>(contract.Commands.agentReset, { runId }),

  // 模型渠道
  llmProviders: () =>
    invokeCommand<contract.ProviderDto[]>(contract.Commands.llmProviders),
  llmProbeChannel: (providerId: string, baseUrlOverride?: string) =>
    invokeCommand<contract.ProbeResultDto>(contract.Commands.llmProbeChannel, {
      providerId,
      baseUrlOverride: baseUrlOverride ?? null,
    }),
  llmDiscoverModels: (providerId: string, baseUrlOverride?: string) =>
    invokeCommand<string[]>(contract.Commands.llmDiscoverModels, {
      providerId,
      baseUrlOverride: baseUrlOverride ?? null,
    }),

  // 省 Token 统计
  tokenStats: () =>
    invokeCommand<contract.CompressionStatsDto>(contract.Commands.tokenStats),
  tokenStatsReset: () =>
    invokeCommand<void>(contract.Commands.tokenStatsReset),

  // 审计
  auditTail: (limit = 50) =>
    invokeCommand<contract.AuditEntryDto[]>(contract.Commands.auditTail, { limit }),
  auditCount: () => invokeCommand<number>(contract.Commands.auditCount),
};

// ───────────────────────────── 事件 ─────────────────────────────

export const events = {
  onTerminalData: (cb: (p: contract.TerminalDataEvent) => void) =>
    onEvent<contract.TerminalDataEvent>(contract.Events.terminalData, cb),
  onTerminalClosed: (cb: (p: contract.TerminalClosedEvent) => void) =>
    onEvent<contract.TerminalClosedEvent>(contract.Events.terminalClosed, cb),
  onSessionState: (cb: (p: contract.SessionStateEvent) => void) =>
    onEvent<contract.SessionStateEvent>(contract.Events.sessionState, cb),
  onForwardState: (cb: (p: contract.ForwardStateEvent) => void) =>
    onEvent<contract.ForwardStateEvent>(contract.Events.forwardState, cb),
  onAgentDelta: (cb: (p: contract.AgentDeltaEvent) => void) =>
    onEvent<contract.AgentDeltaEvent>(contract.Events.agentDelta, cb),
  onAgentToolCall: (cb: (p: contract.AgentToolCallEvent) => void) =>
    onEvent<contract.AgentToolCallEvent>(contract.Events.agentToolCall, cb),
  onAgentToolResult: (cb: (p: contract.AgentToolResultEvent) => void) =>
    onEvent<contract.AgentToolResultEvent>(contract.Events.agentToolResult, cb),
  onAgentApprovalRequired: (
    cb: (p: contract.AgentApprovalRequiredEvent) => void,
  ) =>
    onEvent<contract.AgentApprovalRequiredEvent>(
      contract.Events.agentApprovalRequired,
      cb,
    ),
  onAgentDone: (cb: (p: contract.AgentDoneEvent) => void) =>
    onEvent<contract.AgentDoneEvent>(contract.Events.agentDone, cb),
  onAgentError: (cb: (p: contract.AgentErrorEvent) => void) =>
    onEvent<contract.AgentErrorEvent>(contract.Events.agentError, cb),
};
