# modern-sshdeck

跨平台（Linux / Windows）SSH 终端 + 端口转发 + 可接入大模型 API 的运维 Agent。

技术栈：**Rust + Tauri v2** · **React 19 + TypeScript + Tailwind CSS v4 + shadcn/ui** · **xterm.js** · **russh**

---

## 功能

| 能力 | 状态 |
|---|---|
| 多标签 SSH 终端 | ✅ |
| 标签内分屏（单屏 / 左右 / 上下 / 四宫格） | ✅ |
| 密码登录 | ✅ |
| 私钥登录（OpenSSH，支持 passphrase） | ✅ |
| 本地端口转发（`-L`） | ✅ |
| 运维 Agent（OpenAI 兼容 API：DeepSeek / OpenAI / Qwen / vLLM / Ollama） | ✅ |
| Agent 命令分级审批（只读自动 / 变更与高危需人工确认） | ✅ |
| 远程转发 `-R` / SOCKS `-D` | 预留（未实现） |
| known_hosts 严格校验 | 未实现（MVP 展示指纹供人工比对） |

---

## 架构

完整设计见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)。构建环境注意事项见 [`docs/BUILD-ENV.md`](docs/BUILD-ENV.md)。

```
┌──────────────┬──────────────────────┬──────────────┐
│ 连接 / 转发   │   终端工作区          │  Agent 面板   │
│  Sidebar     │   标签 + 分屏 + xterm │  对话 + 审批   │
└──────┬───────┴───────────┬──────────┴───────┬───────┘
       └────────── Tauri IPC (invoke/event) ──┘
                          │
        ┌─────────────────┴───────────────────┐
        │            Rust 后端                │
        │  SessionManager   ForwardRegistry   │
        │  AgentRuntime     LlmProvider       │
        │  CommandGuard     PersistStore      │
        └─────────────────────────────────────┘
```

### 核心模块

| 模块 | 路径 | 职责 |
|---|---|---|
| SSH | `src-tauri/src/ssh/` | 会话建立、认证、shell channel、命令执行 |
| 转发 | `src-tauri/src/forward/` | 本地监听 → SSH direct-tcpip |
| Agent | `src-tauri/src/agent/` | 对话 tool loop、审批闸门 |
| LLM | `src-tauri/src/llm/` | OpenAI 兼容 SSE 流式 + tool_calls |
| 安全 | `src-tauri/src/guard/` | 命令风险分级（fail-safe） |
| 持久化 | `src-tauri/src/persist/` | 配置、主机、密钥 |
| 终端 UI | `src/features/terminal/` | 标签、分屏、xterm 集成 |
| 连接 UI | `src/features/connections/` | 主机列表与表单 |
| 转发 UI | `src/features/forward/` | 转发管理 |
| Agent UI | `src/features/agent/` | 对话、工具卡片、审批卡片 |

---

## 开发

### 前置

- Rust 1.85+（Windows 需 MSVC 工具链）
- Node 20+ / pnpm 9+
- Linux 额外依赖：
  ```bash
  sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev build-essential pkg-config libssl-dev
  ```

### 安装与运行

```bash
pnpm install

# Windows（本机 debug 构建受 MSVC 限制，见 docs/BUILD-ENV.md）
bash scripts/check.sh          # cargo check --release

# Linux / macOS
cargo check --manifest-path src-tauri/Cargo.toml

pnpm tauri dev                 # 开发模式
pnpm tauri build               # 打包
```

> ⚠️ **Windows 本机构建**：受 MSVC 调试信息写入限制，`tauri dev`（debug profile）会失败。
> 使用 `scripts/build-msvc.ps1` 或 `scripts/check.sh` 以 release profile 构建。
> 详见 [`docs/BUILD-ENV.md`](docs/BUILD-ENV.md)。

### 校验

```bash
pnpm typecheck                                   # TypeScript 零错误
bash scripts/check.sh                            # Rust 零错误（release）
bash scripts/check.sh test                       # 全部测试
```

### 测试

当前共 **36 个测试全部通过**：

| 类型 | 数量 | 覆盖内容 |
|---|---|---|
| 单元测试 | 126 | 命令风险分级（含 `sudo -i`/`su root` 高危判定）、SSE 解析与 `tool_calls` 聚合、tool loop 与审批取消、私钥路径展开、转发地址规范化（含 IPv6）、**设置项持久化与非法值校验** |
| 端到端集成测试 | 5 | 真实 russh 服务端 + 真实客户端：SSH 握手、host key SHA256 指纹、密码认证（正确/错误）、PTY 尺寸透传、shell 数据往返、direct-tcpip 通道、端口不可达快速失败 |
| 回归测试 | 2 | PTY `TTY_OP_END` 误用导致的会话立即断开（带超时断言） |
| 真实数据测试 | 1 | 用真实 `git diff` 断言 RTK 压缩率 > 30% 且不丢变更行 |

`src-tauri/tests/e2e_ssh.rs` 会启动一个进程内的真实 SSH 服务端（随机端口），
因此无需外部 sshd 即可验证完整网络链路。

---

## 使用

1. **添加主机**：左栏「新建连接」→ 填主机/端口/用户名 → 密码或私钥
2. **打开终端**：点击主机 → 自动开标签；工具栏可切分屏
3. **端口转发**：左栏下半「新建转发」→ 本地端口 → 远端 host:port
4. **配置 Agent**：左下角「设置」→ **Agent** 页签 → 填服务地址 / 模型 / API Key
5. **对话**：右栏输入需求 → Agent 自动调用工具
   - 只读命令（`ls`/`df`/`ps`…）→ 直接执行
   - 变更/高危命令 → 弹审批卡片，需你点「允许一次」

### 模型接入：免费通道与聚合渠道

设置 → **渠道** 页管理 41 家提供商，按分类分组：

| 分类 | 家数 | 说明 |
|---|---|---|
| 本机模型 | 3 | Ollama / LM Studio / llama.cpp，**无需 Key、零成本** |
| 免费额度 | 15 | OpenRouter、Groq、Cerebras 等，需 Key 但有免费额度 |
| 需 API Key | 21 | DeepSeek、OpenAI、Kimi、MiniMax 等 |
| OAuth | 2 | 需交互式登录，本应用不代为完成，仅列出 |

模型以 `provider/model` 形式选用，例如 `ollama/qwen2.5:0.5b`。

**已实测验证的免费路径**（无需任何 Key）：

```bash
# 本机装 Ollama 后
ollama serve &
ollama pull qwen2.5:0.5b
# 设置 → 渠道 → Ollama → 测试连通 → 设为当前模型
```

实测结果：`ollama/qwen2.5:0.5b` 探测为 available，Agent 能自主调用工具
（真实执行 `uname -a`）并基于结果作答。

> **关于 Kilo 免费池**：网上很多说法称 Kilo 网关可免 Key 白嫖。实测发现它有
> **两套 model id 命名空间，行为相反**：目录里的 `xxx-free`（连字符）大多返回
> **401 需登录**，而网关里的 `xxx:free`（冒号）与自动路由 `kilo-auto/free`
> 确实返回 200 且 `cost: 0`。本项目未把 Kilo 标记为免 Key 通道，也未实现任何
> 伪造客户端指纹绕过风控的做法。

### 省 Token（RTK 压缩）

Agent 的工具输出在送模型前会自动压缩。**实测压缩效果**（真实 `git diff`，800 行级改动）：

| 输入 | 原始 | 压缩后 | 节省 |
|---|---|---|---|
| git diff（12 KB，813 行变更） | 12036 B | 2113 B | **82.4%** |
| grep -rn（648 命中，58.5 KB） | 59876 B | 2185 B | **96.4%** |
| git status（60 文件三分类） | 3917 B | 611 B | **84.4%** |
| find（135 项） | 2539 B | 828 B | **67.4%** |

压缩后的内容末尾带可回查标记（如 `[已压缩：原始 11.8 KB → 2.1 KB，省略 1482 行。
需要完整内容请执行 git diff]`），模型知道看到的是压缩视图。

**五条安全规则**（均有单测）：报错输出永不压缩；小于 2 KB 不处理；
压缩后更大则回退；整条管线幂等；过滤器异常即原样放行。
压缩只作用于送模型的上下文，**界面上显示给你的始终是原始输出**。

设置 → **省 Token** 页可查看实时节省比例与审批审计记录。

### 命令审批

| 风险 | 处置 |
|---|---|
| 只读（`ls`/`df`/`ps`…） | 自动执行，不打断 |
| 变更（写文件、重启服务…） | 单次确认（允许一次 / 拒绝） |
| **高危**（`rm -rf /`、`mkfs`、`sudo -i`…） | **必须手动输入「确认执行」** 才放行 |

每次审批（含自动放行、拒绝、超时、中止）都写入 `<配置目录>/audit.jsonl`，
可在「省 Token → 审批审计」查看。

### 设置

左下角「设置」打开面板，分四个页签，**所有修改立即保存并即时生效**：

| 页签 | 可配置项 |
|---|---|
| **外观** | 主题（深色 / 浅色 / 跟随系统）、界面语言、左栏与 Agent 栏宽度、Agent 面板显隐 |
| **终端** | 字体族（预设 + 自定义字体栈）、字号、回滚行数、光标样式、光标闪烁，含实时预览 |
| **行为** | 关闭标签确认、断线提示音、自动重试次数、新建连接的默认端口与用户名 |
| **Agent** | 服务地址、模型（可一键「探测可用模型」）、API Key（保存/清除）、temperature |

主题切换同步作用于终端配色（深色 VS Code Dark、浅色 Light+），
不会出现「浅色界面里一块黑终端」的割裂感。

字体栈留空时使用内置集合（含常见 Nerd Font 变体，能显示远端提示符图标）。

---

## 配置位置

| 平台 | 路径 |
|---|---|
| Windows | `%APPDATA%\modern-sshdeck\` |
| Linux | `~/.config/modern-sshdeck/` |

- `state.json`：主机列表 + 非敏感配置
- `secrets.json`：LLM API Key（Unix 下 0600 权限）

**安全提示**：MVP 阶段密码与 API Key 以本地明文存储，仅适用于个人可信环境。

---

## 项目状态

MVP 核心链路已实现并通过验证：

| 验证项 | 结果 |
|---|---|
| Rust 编译（release） | 0 error / 2 warning |
| TypeScript 类型检查 | 0 error |
| 前端生产构建 | 成功 |
| 单元 + 端到端测试 | 134/134 通过 |
| 应用启动 | 二进制可运行（进程正常驻留） |
| **真实 SSH 会话** | 已连入 `192.168.0.102`（Arch Linux / zsh / Powerlevel10k），提示符与命令结果正常渲染 |

> 真实主机验证：`blackholemax@192.168.0.102:22` 密码认证成功，host key 指纹
> `SHA256:4aVWEvyavqCCc+qyz2+bb6hSfzyVaMJ2zu6ggmTeHrM`，终端内 `echo` 输出正常回传，会话持续存活。

未完成项（架构已预留）：

- 远程端口转发 `-R` / SOCKS 动态转发 `-D`（`ForwardKind` 已预留，`Remote`/`Dynamic` 当前返回 `InvalidInput`）
- known_hosts 严格校验与 TOFU 指纹确认（MVP 接受任意主机，仅展示 SHA256 指纹供人工比对）
- ssh-agent / 系统钥匙串集成
- SFTP 文件传输、跳板机 ProxyJump、会话录制回放
- keyboard-interactive 认证（部分服务器密码走此路径）

### 已修复的关键缺陷（真实主机联调中发现）

两个缺陷都会导致「连上却没有提示符、会话立即消失」，且都能绕过编译与单测：

1. **PTY `terminal_modes` 混入 `TTY_OP_END`**
   `TTY_OP_END` 是 RFC 4254 里终止 mode 列表的哨兵，只该由协议编码层追加。
   作为真实 mode 传入会让编码后的列表提前终止，服务端解析错位后不回 `pty-req`
   的 reply —— `request_pty(want_reply = true)` 永久挂起，被 `shell_timeout`
   打断后 channel 一并丢弃。已移除，并在 `src-tauri/tests/e2e_terminal.rs`
   加了带超时断言的回归测试。

2. **`watch::channel` 的 receiver 被立即 drop**
   没有常驻 receiver 时 sender 直接进入 closed 状态，之后 `subscribe()` 得到的
   receiver 首次 `changed()` 立刻返回 `Err`，shell task 启动即误判「已关闭」退出。
   已改为把 receiver 常驻在 `SessionEntry.closed_guard`。

顺带把 shell 的双任务（`channel.split()` 读写分治）改为**单任务独占 Channel**：
`split()` 之后通道存活依赖 write half，任一任务退出都会连带另一个，生命周期过于脆弱。

### 已知工程限制

1. **Windows debug 构建不可用**：MSVC 写调试信息失败，`tauri dev` 会挂。必须用 release profile（`scripts/check.sh` / `scripts/build-msvc.ps1`）。Linux 无此问题。
2. **命令分级是词法级**：不做变量展开分析（`CMD=/bin/rm; $CMD -rf /`），这类会 fail-safe 落到 `Mutating`（需审批），但存在**漏报 Dangerous** 的可能。宁可多审批，不会漏放行只读。
3. **未接真实 LLM 进行端到端对话验证**：需要有效的 API Key。SSE 解析、`tool_calls` 聚合、审批流程有单元测试覆盖，但未对真实 provider 发起过请求。
4. **Nerd Font 图标**：远端提示符（Powerlevel10k / starship）会输出私用区图标，未装 Nerd Font 时显示为方框。已把常见 Nerd Font 变体加入终端字体栈，装任一即可正常显示。

---

## License

MIT
