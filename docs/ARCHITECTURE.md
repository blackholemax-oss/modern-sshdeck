# modern-sshdeck 架构设计

> 跨平台（Linux / Windows）SSH 终端 + 端口转发 + 可接入大模型 API 的运维 Agent。
> 技术栈：Rust + Tauri v2 + React 19 + TypeScript + Tailwind CSS v4 + shadcn/ui。

本文是开发的**唯一架构契约**。并行开发的所有模块必须守约：错误码、IPC 命令签名、事件名、
类型定义不得由单个模块私自变更；确需变更时先改本文并同步 `src/shared/ipc.ts` 与
`src-tauri/src/ipc/mod.rs`。

---

## 1. 设计目标与非目标

### 1.1 目标（MVP 范围，已与用户确认）

| 编号 | 能力 | 验收标准 |
|---|---|---|
| G1 | 多标签 SSH 终端 | 同时打开 ≥3 个会话，各自独立 shell，输出不错乱 |
| G2 | 标签内分屏 | 一个标签内最多 2x2 分屏，每个分屏独立 SSH 会话 |
| G3 | 密码登录 | 用户名 + 密码可登录真实主机 |
| G4 | 私钥登录 | 支持 OpenSSH 私钥文件 + passphrase |
| G5 | 端口转发 | `-L` / `-R` 语义；规则可增删改查（查询、修改后原地重启），状态可见 |
| G5b | 主机密钥校验 | 严格比对 `known_hosts`；未知主机需确认指纹，密钥变更一律拒绝 |
| G6 | Agent 对话面板 | 接入 OpenAI 兼容 API（base_url / api_key / model 可配） |
| G7 | Agent 工具调用 | Agent 可执行只读命令（自动），写/高危命令需人工确认 |
| G8 | 双平台构建 | Windows 本地可编译运行；Linux 提供构建脚本与说明 |

### 1.2 非目标（本轮不做，但架构预留）

- SOCKS 动态转发 `-D`（`ForwardKind` 已预留 `Dynamic`，后端明确拒绝并提示）
- ssh-agent / 系统钥匙串（凭据落盘位置见 §7，接口预留 `CredentialStore`）
- SFTP 文件传输、跳板机 ProxyJump、会话录制回放
- 团队协作 / 云端同步

> 已完成（原非目标）：远程端口转发 `-R`（§4.4）、known_hosts 严格校验与指纹确认
> （§4.8，策略 `strict` / `accept_new` / `insecure`）。

### 1.3 关键架构决策（ADR 摘要）

| ADR | 决策 | 理由 | 代价 |
|---|---|---|---|
| ADR-1 | SSH 走 **russh**（纯 Rust，async，tokio） | 不依赖 OpenSSH 二进制，Windows/Linux 行为一致，无需 libssh 的 C 构建 | 需自行处理 known_hosts 与算法协商细节 |
| ADR-2 | 终端编码走 **base64 分块 + 虚拟屏快照** | 重连/分屏切换可恢复屏幕；避免逐字节跨 IPC | 多一层 base64 开销（可接受） |
| ADR-3 | Agent 与 SSH 均在 **后端**，前端不直连 | API Key 不落前端；命令分类与审批在后端可强制拦截 | Agent 只能通过工具接口操作终端 |
| ADR-4 | Agent provider 用 **OpenAI 兼容单一抽象** | 覆盖 DeepSeek/OpenAI/Qwen/vLLM/Ollama | 高级专有特性需降级 |
| ADR-5 | 命令安全分级在 **Rust 侧**，前后端双校验 | 前端审批 UI 可被绕过，后端必须是强制闸门 | 分类规则需持续维护 |
| ADR-6 | 前端状态用 **Zustand**，不用 Redux | 终端高频更新，需要细粒度订阅 | 需自律避免全局 store 膨胀 |

---

## 2. 系统总览

```
┌──────────────────────────────────────────────────────────────────────┐
│                        React 前端 (WebView)                          │
│                                                                      │
│  ┌─────────────┐ ┌──────────────┐ ┌────────────┐ ┌───────────────┐  │
│  │ Connection  │ │ Terminal     │ │ Forward    │ │ Agent         │  │
│  │ Sidebar     │ │ Workspace    │ │ Panel      │ │ Chat Panel    │  │
│  │ (主机列表)   │ │ (标签+分屏)   │ │ (转发管理)  │ │ (对话+审批)    │  │
│  └──────┬──────┘ └──────┬───────┘ └─────┬──────┘ └───────┬───────┘  │
│         │               │               │                │          │
│         └───────────────┴───────┬───────┴────────────────┘          │
│                                 │                                    │
│                    ┌────────────▼─────────────┐                      │
│                    │  Zustand Stores + IPC    │                      │
│                    │  (invoke / listen 封装)   │                      │
│                    └────────────┬─────────────┘                      │
└─────────────────────────────────┼────────────────────────────────────┘
                                  │ Tauri IPC (JSON) + event emit
┌─────────────────────────────────┼────────────────────────────────────┐
│                       Rust 后端 (tauri::App)                          │
│                                  │                                    │
│  ┌───────────────────────────────▼────────────────────────────────┐  │
│  │                    AppState (Arc<Mutex<…>>)                    │  │
│  │  sessions · forwards · agent_runs · pending_approvals          │  │
│  └───────┬──────────────────────────────┬────────────────────────┘  │
│          │                              │                            │
│  ┌───────▼────────┐          ┌──────────▼──────────┐                 │
│  │ SessionManager │          │  Agent Runtime      │                 │
│  │ (russh client) │          │  (tool loop)        │                 │
│  └───────┬────────┘          └──────────┬──────────┘                 │
│          │                              │                            │
│  ┌───────▼────────┐          ┌──────────▼──────────┐                 │
│  │ PortForwarder  │          │  LlmProvider        │                 │
│  │ (tokio Tcp)    │          │  (openai-compat SSE)│                 │
│  └────────────────┘          └─────────────────────┘                 │
│                                                                      │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐         │
│  │ Credential   │ │ CommandGuard │ │ Persistence (JSON)   │         │
│  │ Store        │ │ (risk class) │ │ config / hosts       │         │
│  └──────────────┘ └──────────────┘ └──────────────────────┘         │
└──────────────────────────────────────────────────────────────────────┘
```

**数据流（终端）**：

1. 前端 `terminal_input(session_id, data)` → 后端写 SSH channel stdin
2. 后端读 SSH stdout → base64 编码 → `emit("terminal:data", {sessionId, data})`
3. 前端 xterm.js `write(decoded)`；resize 走 `terminal_resize`

**数据流（Agent）**：

1. 前端 `agent_send(runId, userMessage)` → 后端构造 messages + tools schema
2. `LlmProvider::stream_chat()` → SSE 增量 → `emit("agent:delta")`
3. 模型返回 `tool_calls` → `CommandGuard` 判定风险：
   - `ReadOnly` → 直接执行 → 结果回填 messages → 继续循环
   - `Mutating`/`Dangerous` → `emit("agent:approval_required")` → 等待
     `agent_approve(approvalId, approved)` → 执行或回填拒绝
4. 循环结束 → `emit("agent:done")`

---

## 3. 模块划分与写作用域

为支持并行开发，定义严格的文件写作用域。**任何模块不得修改他人作用域内的文件**。

| 模块 | Owner | 写作用域 |
|---|---|---|
| M0 骨架契约 | Lead | `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/state.rs`, `src-tauri/src/ipc/mod.rs`, `src/shared/**`, `package.json`, `vite.config.ts`, `src-tauri/Cargo.toml`, `docs/ARCHITECTURE.md`, `src/app/**`, `src/components/ui/**`, `src/styles/**` |
| M1 SSH / 转发 | Backend-A | `src-tauri/src/ssh/**`, `src-tauri/src/forward/**`, `src-tauri/tests/ssh_*.rs` |
| M2 终端 UI | Frontend-B | `src/features/terminal/**`, `src/features/connections/**`, `src/features/forward/**` |
| M3 Agent / LLM | Backend-C | `src-tauri/src/agent/**`, `src-tauri/src/llm/**`, `src-tauri/src/guard/**` |
| M4 Agent UI | Frontend-B（阶段2） | `src/features/agent/**` |
| M5 汇总验证 | Lead | 全局 |

---

## 4. Rust 后端详细设计

### 4.1 Crate 结构

```
src-tauri/
├── Cargo.toml
├── build.rs
├── tauri.conf.json
├── capabilities/default.json
└── src/
    ├── main.rs              # 入口，薄
    ├── lib.rs               # App builder，注册所有 command + state
    ├── state.rs             # AppState 定义（Lead）
    ├── error.rs             # Error / Result / IPC 错误码（Lead）
    ├── ipc/
    │   ├── mod.rs           # command 转发层（Lead 定义签名）
    │   ├── dto.rs           # 所有请求/响应 DTO（Lead）
    │   └── events.rs        # 事件名常量 + payload struct（Lead）
    ├── ssh/
    │   ├── mod.rs
    │   ├── manager.rs       # SessionManager: 创建/销毁/列举、主机密钥探针
    │   ├── session.rs       # 单个 russh session + channel task + `-R` 路由
    │   ├── known_hosts.rs   # known_hosts 严格校验（策略 / 通配 / 哈希 / 吊销）
    │   ├── auth.rs          # 密码 / 私钥认证
    │   └── config.rs        # SSH 配置（超时、算法偏好）
    ├── forward/
    │   ├── mod.rs
    │   ├── local.rs         # 本地监听 → SSH direct-tcpip（bridge 双向泵）
    │   ├── remote.rs        # tcpip-forward 请求 / 撤销、本机目标连接
    │   └── registry.rs      # ForwardRegistry: 规则增删改查 + 状态
    ├── agent/
    │   ├── mod.rs
    │   ├── runtime.rs       # 对话循环（tool loop）
    │   ├── prompt.rs        # system prompt 构造
    │   ├── tools.rs         # 工具 schema 定义
    │   └── executor.rs      # 工具 → SSH 命令执行
    ├── llm/
    │   ├── mod.rs
    │   ├── provider.rs      # trait LlmProvider
    │   ├── openai.rs        # OpenAI 兼容实现（SSE 流）
    │   └── types.rs         # ChatMessage / ToolCall / Delta
    ├── guard/
    │   ├── mod.rs
    │   └── classifier.rs    # 命令风险分级
    └── persist/
        ├── mod.rs
        └── store.rs         # 配置 / 主机 / 会话持久化（JSON）
```

### 4.2 核心类型（契约）

```rust
// error.rs
pub enum AppError {
    Ssh(String),          // 连接/认证/通道错误
    HostKey(String),      // 主机密钥校验失败（未知 / 变更 / @revoked）
    Forward(String),      // 监听/转发错误
    Llm(String),          // provider 错误
    Agent(String),        // agent 循环错误
    NotFound(String),     // 会话/转发不存在
    InvalidInput(String), // 参数校验失败
    Io(String),
    Persist(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Ssh(_) => "SSH_ERROR",
            AppError::HostKey(_) => "HOST_KEY_ERROR",
            AppError::Forward(_) => "FORWARD_ERROR",
            AppError::Llm(_) => "LLM_ERROR",
            AppError::Agent(_) => "AGENT_ERROR",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::InvalidInput(_) => "INVALID_INPUT",
            AppError::Io(_) => "IO_ERROR",
            AppError::Persist(_) => "PERSIST_ERROR",
        }
    }
}

// 命令签名序列化形状（前端必有对应类型）
pub struct IpcError {
    pub code: String,
    pub message: String,
}
```

### 4.3 SSH 会话生命周期

```
Idle ──connect()──> Authenticating ──ok──> Connected ──open_shell──> Shell
                          │                   │
                          └──fail──> Failed   └──disconnect()──> Closed
Shell ──channel close / EOF──> Closed
```

- 每个 `Session` 持有 `russh::client::Handle` + 一个 `mpsc::Sender<ChannelMsg>` 写队列
- 读任务独立 tokio task，循环 `channel.wait()`，`Data` → base64 → emit
- 会话断开时 emit `terminal:closed`，前端据此置灰标签并保留滚动内容
- **必须**处理 `WindowChange`（resize）与 `Eof`/`Close`

### 4.4 端口转发（`-L` / `-R`）与规则 CRUD

```
-L: 本地 TCP listener (127.0.0.1:L) ──accept──> 每条连接:
        SSH direct-tcpip("<remote_host>:<remote_port>") ──> 双向 copy

-R: SSH tcpip-forward("<remote_host>:<remote_port>")
        服务端来连 ──> forwarded-tcpip channel ──> 本机连 "<remote_host>:<remote_port>"
        ──> 双向 copy（复用同一个 bridge）
```

- **规则模型**：`ForwardRegistry` 持有 `HashMap<forwardId, ForwardEntry>`。
  每条规则 = `ForwardInfo`（含 `createdAt` 用于稳定排序）+ 一个可重启实例。
  CRUD：`forward_start`（增）、`forward_list` / `forward_get`（查）、
  `forward_update`（改，原地重启并保留 id）、`forward_stop`（删）。
- **修改语义**：先停旧实例（释放本地监听 / `cancel-tcpip-forward`）再按新参数启动。
  新参数启动失败时规则**不丢**，保留为 `state = failed` 并带 `error`。
- **epoch 防竞态**：每次（重）启动自增 `epoch`，旧实例收尾时只有仍是当班实例
  才允许写状态 / emit，避免「update 后显示已停止」。
- `-L` 每条连接独立 SSH channel（`channel_open_direct_tcpip`）。
- `-R` 由 `ClientHandler::server_channel_open_forwarded_tcpip` 收通道 →
  `SessionEntry::remote_routes` 按 `(connected_address, connected_port)` 分发
  （兼容通配监听 / 回环别名 / 端口唯一兜底）；无路由时礼貌关闭通道。
- 实例与 SSH 会话生命周期绑定；会话断开时自动停止并 emit `forward:state = stopped`。
- 端口占用 → `AppError::Forward("listen failed: …")`；服务端拒绝
  `tcpip-forward` → 提示检查 `AllowTcpForwarding` / `GatewayPorts`。

### 4.5 Agent Runtime（tool loop）

```
loop (max 12 iterations):
    stream = provider.stream_chat(messages, tools)
    for delta in stream: emit agent:delta
    if finish_reason == tool_calls:
        for each tool_call:
            risk = guard.classify(cmd)
            if risk != ReadOnly:
                approval = new approval id; emit agent:approval_required
                wait for agent_approve (timeout 300s)
                if rejected: messages += tool result "user rejected"
                else: execute
            else: execute
            messages += tool result
    else: break
emit agent:done
```

**工具定义（MVP）**：

| 工具名 | 参数 | 风险 |
|---|---|---|
| `run_command` | `session_id`, `command` | 由 classifier 判定 |
| `list_sessions` | — | ReadOnly（内部） |
| `read_terminal_tail` | `session_id`, `lines` | ReadOnly（内部） |

**命令分级规则（classifier）**：

- ReadOnly 白名单前缀：`ls`, `cat`, `pwd`, `whoami`, `df`, `free`, `ps`, `top -b -n1`,
  `uptime`, `uname`, `hostname`, `ip a`, `ss -tlnp`, `netstat`, `systemctl status`,
  `journalctl`, `tail`, `head`, `grep`, `find`, `env`, `date`, `du`, `stat`
- Dangerous 关键字：`rm -rf /`, `mkfs`, `dd of=`, `shutdown`, `reboot`, `init 0`,
  `> /dev/sda`, `chmod -R 777 /`, `iptables -F`, `:(){ :|:& };:`
- Mutating：其余一切（写、改、装、重启服务等）
- 复合命令（含 `;`, `&&`, `||`, `|`）→ 取**最高风险**子命令的风险
- 解析失败 / 可疑（`eval`, `$()` 嵌套未知）→ 提升为 `Mutating`（fail-safe）

### 4.6 LLM Provider

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream_chat(
        &self,
        req: ChatRequest,
        sink: DeltaSink,     // 回调：文本增量 / 工具调用增量
    ) -> Result<ChatOutcome, AppError>;
    async fn list_models(&self) -> Result<Vec<String>, AppError>;
}
```

- `OpenAiCompatProvider`：`POST {base_url}/chat/completions`，`stream: true`，解析 `data:` SSE 行
- 支持 `tool_calls` 增量拼接（`index` 聚合）
- 超时：连接 15s，读空闲 90s
- API Key 只存后端，前端仅传"已配置"布尔

### 4.7 AppState

```rust
pub struct AppState {
    pub sessions: SessionManager,
    pub forwards: ForwardRegistry,
    pub agent: AgentRuntime,
    pub config: RwLock<AppConfig>,
    pub pending_approvals: Mutex<HashMap<String, ApprovalSlot>>,
}
```

### 4.8 主机密钥校验（known_hosts）

握手阶段由 `ClientHandler::check_server_key` 强制校验，**失败即断开**，不存在
「先连上再说」的路径。

```
server host key ──> known_hosts 校验 ──┬─ Match      ──> 放行（status = verified）
                                      ├─ Unknown    ──┬─ 策略 strict   ──> 拒绝（HOST_KEY_ERROR）
                                      │              ├─ 策略 accept_new ──> 写入 + 放行（learned）
                                      │              ├─ 策略 insecure  ──> 放行（insecure）
                                      │              └─ 用户确认指纹（trustHostKey 命中）──> 写入 + 放行（trusted）
                                      ├─ Mismatch   ──> 一律拒绝（insecure 除外，且记 WARN）
                                      └─ Revoked    ──> 一律拒绝（insecure 除外，且记 WARN）
```

- 解析/匹配在 `ssh/known_hosts.rs`：`[host]:port`、`*`/`?` 通配、`!` 取反、
  `|1|salt|hash`（HMAC-SHA1）、`@revoked`；逐行返回行号，错误信息可直接定位。
- **密钥变更（Mismatch）不可一键信任**：前端只会对「未知」给出
  「信任并连接」，变更必须人工核对 known_hosts。
- 「信任」是**指纹绑定**的：`ssh_probe_host_key` 只握手不认证，返回指纹与完整公钥；
  前端确认后把它作为 `ConnectRequest.trustHostKey` 回传，后端只有在服务端出示的
  密钥与之**完全一致**时才写盘放行，避免「确认的指纹 ≠ 实际连上的机器」。
- 未知主机在 strict 下会先返回 `HOST_KEY_ERROR`（信息里带 `is not in …`），
  前端据此走确认卡。
- 读 known_hosts 失败视为校验失败（fail-closed）。
- 配置：`behavior.hostKeyPolicy`（默认 `strict`）+ `behavior.knownHostsPath`
  （空 = `~/.ssh/known_hosts`，支持 `~/` 展开）；改完立即对后续连接生效。
- 环境变量 `SSHDECK_KNOWN_HOSTS` 可覆盖默认路径（测试 / 便携场景）。

---

## 5. IPC 契约（前端 ↔ 后端）

**所有 command 返回 `Result<T, IpcError>`。所有事件 payload 均为扁平 JSON 对象。**

### 5.1 Commands

| 命令 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `app_get_config` | — | `AppConfig` | 读取配置（含 LLM 是否已配 key） |
| `app_set_config` | `{config}` | `()` | 写配置 |
| `hosts_list` | — | `HostProfile[]` | 主机列表 |
| `hosts_upsert` | `{host}` | `HostProfile` | 新增/更新 |
| `hosts_delete` | `{id}` | `()` | 删除 |
| `ssh_connect` | `ConnectRequest` | `SessionInfo` | 建立会话（可带 `trustHostKey` 确认指纹） |
| `ssh_disconnect` | `{sessionId}` | `()` | 断开 |
| `ssh_list_sessions` | — | `SessionInfo[]` | 列举 |
| `ssh_probe_host_key` | `{host, port}` | `HostKeyProbe` | 只握手取主机公钥（指纹确认用） |
| `terminal_input` | `{sessionId, data}` | `()` | 键盘输入 |
| `terminal_resize` | `{sessionId, cols, rows}` | `()` | 尺寸变更 |
| `forward_start` | `ForwardRequest` | `ForwardInfo` | 新增规则并启动（`-L` / `-R`） |
| `forward_stop` | `{forwardId}` | `()` | 删除规则并停止 |
| `forward_get` | `{forwardId}` | `ForwardInfo` | 查询单条规则（含失败原因） |
| `forward_list` | `{sessionId?}` | `ForwardInfo[]` | 列举（可按会话过滤，按 `createdAt` 排序） |
| `forward_update` | `{forwardId, patch}` | `ForwardInfo` | 修改规则（原地重启，保留 id） |
| `agent_send` | `{runId, sessionId, message}` | `()` | 发起/继续对话 |
| `agent_abort` | `{runId}` | `()` | 中止 |
| `agent_approve` | `{approvalId, approved}` | `()` | 审批工具调用 |
| `agent_list_models` | — | `string[]` | 探测模型列表 |
| `agent_reset` | `{runId}` | `()` | 清空会话上下文 |

### 5.2 Events

| 事件名 | payload | 说明 |
|---|---|---|
| `terminal:data` | `{sessionId, data}` | base64 输出 |
| `terminal:closed` | `{sessionId, reason}` | 会话结束 |
| `session:state` | `{sessionId, state}` | 状态迁移 |
| `forward:state` | `{forwardId, state, error?}` | 转发状态 |
| `agent:delta` | `{runId, delta}` | 文本增量 |
| `agent:tool_call` | `{runId, callId, name, args}` | 工具调用开始 |
| `agent:tool_result` | `{runId, callId, ok, output}` | 工具结果 |
| `agent:approval_required` | `{runId, approvalId, command, risk, reason}` | 待审批 |
| `agent:done` | `{runId, reason}` | 结束 |
| `agent:error` | `{runId, code, message}` | 错误 |

### 5.3 DTO 定义（与 `src/shared/ipc.ts` 一一对应）

```ts
export type SessionState = "connecting" | "auth" | "connected" | "closed" | "failed";

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

export interface ConnectRequest {
  hostId?: string;
  host: string;
  port: number;
  username: string;
  auth: AuthSpec;
  cols: number;
  rows: number;
  /** 用户确认过的服务端公钥（OpenSSH 文本），仅对「未知主机」生效 */
  trustHostKey?: string;
}

export type AuthSpec =
  | { kind: "password"; password: string }
  | { kind: "key"; keyPath: string; passphrase?: string };

export interface SessionInfo {
  id: string;
  host: string;
  port: number;
  username: string;
  state: SessionState;
  createdAt: number;
  hostKeyFingerprint?: string;
  hostKeyStatus?: "verified" | "trusted" | "learned" | "insecure";
}

export interface HostKeyProbe {
  host: string;
  port: number;
  fingerprint: string;   // SHA256:...
  algorithm: string;     // ssh-ed25519 / rsa-sha2-256 ...
  publicKey: string;     // 回传给 trustHostKey
  knownHostsPath: string;
  policy: "strict" | "accept_new" | "insecure";
  status: "match" | "unknown" | "mismatch" | "revoked";
}

export type ForwardKind = "local" | "remote" | "dynamic"; // dynamic 未实现（后端拒绝）

export interface ForwardRequest {
  sessionId: string;
  kind: ForwardKind;
  listenHost: string;   // -L 本机监听 / -R 服务端监听；默认 127.0.0.1
  listenPort: number;   // 0 = 由系统 / 服务端分配
  remoteHost: string;   // -L 远端目标 / -R 本机目标
  remotePort: number;
}

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
  sessionId: string;
  kind: ForwardKind;
  listenHost: string;
  listenPort: number;
  remoteHost: string;
  remotePort: number;
  state: "starting" | "running" | "stopped" | "failed";
  createdAt: number;    // 毫秒，用于稳定排序
  error?: string;
}

export type Risk = "read_only" | "mutating" | "dangerous";

export interface AppConfig {
  llm: {
    provider: "openai_compat";
    baseUrl: string;
    model: string;
    hasApiKey: boolean;
    temperature: number;
  };
  terminal: { fontSize: number; scrollback: number };
  ui: { theme: "dark" | "light" };
  behavior: {
    /** 主机密钥校验策略 */
    hostKeyPolicy: "strict" | "accept_new" | "insecure";
    /** 空串 = ~/.ssh/known_hosts */
    knownHostsPath: string;
    // …其余行为开关见 src/shared/ipc.ts
  };
}
```

---

## 6. 前端详细设计

### 6.1 目录

```
src/
├── main.tsx
├── app/
│   ├── App.tsx               # 三栏布局
│   ├── providers/            # ThemeProvider
│   └── layout/               # Sidebar, MainArea, RightPanel
├── shared/
│   ├── ipc.ts                # 类型 + invoke 封装
│   └── events.ts             # 事件订阅封装
├── components/ui/            # shadcn 组件（button, input, dialog, tabs, ...）
├── features/
│   ├── connections/          # 主机列表、连接表单
│   ├── terminal/             # 标签 + 分屏 + xterm
│   ├── forward/              # 转发面板
│   └── agent/                # 对话面板 + 审批弹层
├── stores/                   # zustand
└── styles/
```

### 6.2 布局

```
┌──────────┬───────────────────────────────┬──────────────┐
│ Sidebar  │  Terminal Workspace           │ Agent Panel  │
│ 主机列表  │  ┌─ Tabs ─────────────────┐   │  对话流       │
│ 转发列表  │  │ [host-a] [host-b] [+]  │   │  工具卡片     │
│ 设置入口  │  ├────────────────────────┤   │  审批卡片     │
│          │  │  Pane (xterm)          │   │  输入框       │
│          │  │  Pane │ Pane (split)   │   │              │
│          │  └────────────────────────┘   │              │
└──────────┴───────────────────────────────┴──────────────┘
```

- 左栏 240px 可折叠；右栏 380px 可折叠
- 终端区使用 CSS Grid 实现 2x2 分屏；每个 Pane 独立 xterm 实例
- 主题：默认 dark，shadcn CSS 变量切换

### 6.3 终端实现要点（Frontend-B）

- `@xterm/xterm` v5 + `@xterm/addon-fit` + `@xterm/addon-search` + `@xterm/addon-web-links`
- 每个 Pane 一个 `Terminal` 实例，`onData` → `terminal_input`（**需 16ms 节流合并**）
- 监听 `terminal:data` 按 `sessionId` 定向写入（**避免全局订阅导致全量重渲染**）
- `ResizeObserver` → `fit()` → `terminal_resize`（**去抖 100ms**）
- 断开时 `terminal:writeln` 提示，不禁用滚动
- 标签内分屏：`SplitLayout` 状态 `1 | 2h | 2v | 4`

### 6.4 状态管理

```ts
sessionsStore: { sessions, activeTabId, panesByTab }
forwardStore:  { forwards, selectedSessionId }
agentStore:    { messages, streaming, approval, config }
configStore:   { appConfig, hosts }
```

**规则**：高频数据（终端输出）**不进** React state，直接走 xterm 命令式写入。

### 6.5 Agent 面板

- 消息气泡：user / assistant / tool
- 流式渲染：`agent:delta` 追加到当前 assistant 消息
- 工具调用以卡片展示（命令、风险徽章、输出折叠）
- 审批卡片：显示命令全文 + 风险等级 + 「允许一次 / 拒绝」
- 高风险命令徽章用 `destructive` 样式

---

## 7. 安全模型

| 项 | 设计 |
|---|---|
| API Key 存储 | 仅后端内存 + 配置文件（`~/.config/modern-sshdeck/config.json`，`0600`）；前端永不回读明文 |
| 私钥 | 只读取路径与 passphrase；不主动保存私钥内容 |
| 密码 | 可保存至配置文件（MVP），伦理提示：设置页标注"本地明文存储" |
| 命令审批 | 后端强制；`Mutating`/`Dangerous` 未经审批无法执行 |
| 提示注入 | system prompt 声明终端输出为不可信数据；工具结果不回显为指令 |
| Tauri 权限 | `capabilities/default.json` 仅允许必要 core 权限，关闭所有非必需 API |

---

## 8. 跨平台注意事项

| 项 | Windows | Linux |
|---|---|---|
| 构建工具链 | 已验证 `x86_64-pc-windows-gnu` + rust-lld 可链接 | 需 `libwebkit2gtk-4.1-dev` 等系统包 |
| WebView2 | 已安装 149.x，运行时存在 | 用 WebKitGTK |
| 路径 | `%APPDATA%\modern-sshdeck\` | `~/.config/modern-sshdeck/` |
| 私钥路径 | 支持 `~/.ssh` 展开 | 同 |
| 字体渲染 | Conhost/WT 差异，xterm 需显式字体栈 | 同 |

**验证命令**：
```bash
pnpm tauri dev
cargo check --manifest-path src-tauri/Cargo.toml
pnpm tsc --noEmit
```

---

## 9. 并行开发边界（派单依据）

| 任务 | 依赖 | 产出 | 不可越界 |
|---|---|---|---|
| T1 SSH/转发后端 | 骨架契约（M0） | `src-tauri/src/ssh/**`, `forward/**` | 不改 `ipc/mod.rs` 签名，不改 `state.rs` |
| T2 终端 UI | 骨架 + IPC 类型 | `src/features/terminal/**`, `connections/**`, `forward/**` | 不改 `shared/ipc.ts` |
| T3 Agent/LLM 后端 | 骨架契约 | `agent/**`, `llm/**`, `guard/**` | 不改 `ipc/mod.rs` 签名 |
| T4 Agent UI | T3 事件契约 | `src/features/agent/**` | 不改 `shared/ipc.ts` |
| T5 整合验证 | T1–T4 | 全量编译通过 | Lead 专属 |

**事件名与 DTO 在 M0 阶段冻结**；任何变更须回到本文修改并通知全体。

---

## 10. 验收清单

- [ ] `cargo check` 无 error（warning 允许）
- [ ] `pnpm tsc --noEmit` 无 error
- [ ] `pnpm build` 成功
- [ ] 应用启动显示三栏布局
- [ ] 可创建主机配置并连接密码认证会话
- [ ] 终端可输入、可 resize、输出正常
- [ ] 标签可切换、可关闭；标签内可分屏
- [ ] 本地端口转发可启动/停止，状态正确
- [ ] Agent 面板可配置 base_url/model，发消息有流式输出
- [ ] 只读命令自动执行；写命令弹出审批卡片
- [ ] README 含双平台构建说明
