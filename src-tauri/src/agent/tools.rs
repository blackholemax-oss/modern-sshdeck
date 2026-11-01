//! Agent 工具定义。
//!
//! **所有权**：Backend-C 完整实现。

use crate::llm::types::{FunctionSpec, ToolSpec};

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            kind: "function".into(),
            function: FunctionSpec {
                name: "run_command".into(),
                description: "在当前 SSH 会话执行一条 shell 命令并返回输出".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令" }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolSpec {
            kind: "function".into(),
            function: FunctionSpec {
                name: "list_sessions".into(),
                description: "列出当前已连接的 SSH 会话".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            },
        },
    ]
}
