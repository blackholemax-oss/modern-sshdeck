/**
 * 会话「无 pane 消费」期间的输出暂存。
 *
 * 为什么需要：`ssh_connect` 返回 `SessionInfo` 时远端 shell 已经启动，提示符
 * 往往在 React 把 pane 挂上去之前就经 `terminal:data` 下发了。若只靠 pane 内
 * 的定向订阅，第一批字节会丢（表现为"没有提示符"）。
 *
 * 做法（刻意不违反"每个 pane 独立订阅"的规则）：
 * - 应用内**只**注册一个捕获订阅（首次连接前调用 `ensureCapture`，幂等）。
 * - 该订阅**不写终端**，只在"该 sessionId 还没有 pane 消费者"时把字节暂存。
 * - pane 挂载时 `markConsumer`，随后 `drainPending` 一次性回放；此后捕获订阅
 *   对已注册的会话直接跳过，真正的写入仍然只发生在 pane 自己的订阅里。
 */
import { events } from "@/shared/api";

import { decodeBase64 } from "./terminalOptions";

/** 单个会话最多暂存多少字节，超过则丢弃最旧的块（防止异常会话撑爆内存）。 */
const MAX_BYTES_PER_SESSION = 256 * 1024;
/** 最多跟踪多少个尚未被消费的会话。 */
const MAX_SESSIONS = 32;

const pending = new Map<string, { chunks: Uint8Array[]; bytes: number }>();
const consumers = new Set<string>();

let captureOff: (() => void) | null = null;
let captureStarted = false;

/**
 * 启动全局捕获订阅（幂等）。应尽早调用，推荐在发起连接之前。
 * 返回的 Promise 永不 reject：捕获失败只影响首批输出的缓冲，不影响主流程。
 */
export function ensureCapture(): void {
  if (captureStarted) return;
  captureStarted = true;
  void events
    .onTerminalData((payload) => {
      const sid = payload.sessionId;
      if (consumers.has(sid)) return; // 已有 pane 在处理，绝不重复写。
      const bytes = decodeBase64(payload.data);
      if (!bytes) return;

      const entry = pending.get(sid) ?? { chunks: [], bytes: 0 };
      entry.chunks.push(bytes);
      entry.bytes += bytes.byteLength;
      while (entry.bytes > MAX_BYTES_PER_SESSION && entry.chunks.length > 1) {
        const dropped = entry.chunks.shift();
        if (!dropped) break;
        entry.bytes -= dropped.byteLength;
      }

      if (!pending.has(sid) && pending.size >= MAX_SESSIONS) {
        const oldest = pending.keys().next().value;
        if (oldest !== undefined) pending.delete(oldest);
      }
      pending.set(sid, entry);
    })
    .then((off) => {
      captureOff = off;
    })
    .catch(() => {
      captureStarted = false;
    });
}

/** 标记该会话已有 pane 消费者，并取回尚未回放的字节。 */
export function takePending(sessionId: string): Uint8Array[] {
  consumers.add(sessionId);
  const entry = pending.get(sessionId);
  pending.delete(sessionId);
  return entry ? entry.chunks : [];
}

/** pane 卸载时解除消费者标记（会话若仍存活，后续输出重新进入暂存）。 */
export function releaseConsumer(sessionId: string): void {
  consumers.delete(sessionId);
}

/** 应用级清理（目前无调用方，保留给未来的热重载/测试）。 */
export function stopCapture(): void {
  if (captureOff) captureOff();
  captureOff = null;
  captureStarted = false;
  consumers.clear();
  pending.clear();
}
