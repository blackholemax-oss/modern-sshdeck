//! OpenAI 兼容 provider（SSE 流式）。
//!
//! **所有权**：Backend-C 完整实现。
//!
//! 设计要点：
//! - 手写 SSE 行缓冲：`bytes_stream()` 累积到 `String`，按 `\n\n`（兼容 `\r\n\r\n`）切 event，
//!   再逐行解析 `data:` 载荷；不使用 `eventsource-stream` 之类的额外依赖。
//! - `tool_calls` 增量按 `delta.tool_calls[].index` 聚合 `id` / `function.name` / `function.arguments`。
//! - 超时：连接 15s（client builder）、读空闲 90s（每次 `stream.next()` 外包 `tokio::time::timeout`）。
//! - 错误统一映射为 `AppError::Llm`，message 内含 HTTP 状态与响应体摘要（截断）。
//! - `base_url` 归一化：容忍尾斜杠、容忍缺少 `/v1`（Ollama 等）。

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;

use crate::error::{AppError, AppResult};
use crate::llm::provider::LlmProvider;
use crate::llm::types::{ChatMessage, ChatOutcome, Delta, FunctionCall, ToolCall, ToolSpec};

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 读空闲超时（两次 SSE chunk 之间的最长间隔）。
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// 错误响应体摘要最大长度。
const BODY_SUMMARY_LIMIT: usize = 400;

/// 单个工具调用的聚合状态。
#[derive(Debug, Default, Clone)]
struct AggToolCall {
    index: u32,
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

pub struct OpenAiCompatProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f32,
    pub client: Client,
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        temperature: f32,
    ) -> Self {
        Self {
            base_url: normalize_base_url(&base_url.into()),
            api_key: api_key.into(),
            model: model.into(),
            temperature,
            client: Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// 拼接 endpoint；`base_url` 已归一化。
    fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}{path}")
    }

    fn apply_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.header("Accept", "text/event-stream");
        if self.api_key.trim().is_empty() {
            rb
        } else {
            rb.bearer_auth(self.api_key.trim())
        }
    }

    fn chat_url(&self) -> String {
        self.endpoint("/chat/completions")
    }

    fn models_url(&self) -> String {
        self.endpoint("/models")
    }

    /// 读取失败响应体摘要（截断），用于错误信息。
    async fn read_body_summary(&self, resp: reqwest::Response) -> String {
        let status = resp.status();
        let text = match tokio::time::timeout(Duration::from_secs(20), resp.text()).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => format!("<读取响应体失败: {e}>"),
            Err(_) => "<读取响应体超时>".to_string(),
        };
        let trimmed = text.trim();
        let mut summary: String = trimmed.chars().take(BODY_SUMMARY_LIMIT).collect();
        if trimmed.chars().count() > BODY_SUMMARY_LIMIT {
            summary.push_str("…");
        }
        format!("HTTP {status}: {summary}")
    }
}

/// 归一化 `base_url`：
/// - 去掉尾斜杠
/// - 缺少 `/v1`（且 path 为空或 `/`）时补上 ` /v1`
/// - 已带版本段（`/v1`、`/openai`、`/api/v1` 等）则原样保留
pub fn normalize_base_url(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        s = "https://api.deepseek.com/v1".to_string();
    }
    while s.ends_with('/') {
        s.pop();
    }

    // 拆出 scheme://authority 与 path
    let (prefix, path) = match s.find("://") {
        Some(i) => {
            let rest_start = i + 3;
            match s[rest_start..].find('/') {
                Some(j) => {
                    let split = rest_start + j;
                    (s[..split].to_string(), s[split..].to_string())
                }
                None => (s.clone(), String::new()),
            }
        }
        None => (s.clone(), String::new()),
    };

    let path_lower = path.to_ascii_lowercase();

    // 用户直接贴了完整 endpoint：按已知后缀剥离到 base
    for suffix in ["/chat/completions", "/models", "/completions"] {
        if let Some(stripped) = path_lower.strip_suffix(suffix) {
            let base_len = stripped.len();
            let base = &path[..base_len];
            return if base.is_empty() {
                format!("{prefix}/v1")
            } else {
                format!("{prefix}{base}")
            };
        }
    }

    let has_version = path_lower.ends_with("/v1")
        || path_lower.contains("/v1/")
        || path_lower.ends_with("/v1beta")
        || path_lower.contains("/api/")
        || path_lower.ends_with("/openai");

    if path.is_empty() {
        format!("{prefix}/v1")
    } else if has_version {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}{path}/v1")
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> AppResult<ChatOutcome> {
        if self.model.trim().is_empty() {
            return Err(AppError::Llm(
                "未配置模型（llm.model 为空），请先设置模型名".into(),
            ));
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "temperature": self.temperature,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)
                .map_err(|e| AppError::Llm(format!("序列化 tools 失败: {e}")))?;
            body["tool_choice"] = serde_json::json!("auto");
        }

        let resp = self
            .apply_auth(self.client.post(self.chat_url()).json(&body))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AppError::Llm(format!(
                        "请求 LLM 超时（连接超过 {}s）：{e}",
                        CONNECT_TIMEOUT.as_secs()
                    ))
                } else if e.is_connect() {
                    AppError::Llm(format!(
                        "无法连接 LLM 服务 {}：{e}",
                        self.chat_url()
                    ))
                } else {
                    AppError::Llm(format!("请求 LLM 失败: {e}"))
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let summary = self.read_body_summary(resp).await;
            return Err(AppError::Llm(format!(
                "LLM 返回错误（{}），模型 {}：{summary}",
                status.as_u16(),
                self.model
            )));
        }

        // 有些兼容实现会忽略 stream=true 直接返回 JSON
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false);

        let mut acc = StreamAccumulator::default();

        if !is_sse {
            let text = tokio::time::timeout(Duration::from_secs(120), resp.text())
                .await
                .map_err(|_| AppError::Llm("读取 LLM 非流式响应超时".into()))?
                .map_err(|e| AppError::Llm(format!("读取 LLM 响应失败: {e}")))?;
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                AppError::Llm(format!(
                    "LLM 响应不是合法 JSON（{e}）：{}",
                    truncate(&text, BODY_SUMMARY_LIMIT)
                ))
            })?;
            if let Some(err) = v.get("error") {
                return Err(AppError::Llm(format!("LLM 返回错误对象: {err}")));
            }
            acc.absorb_non_stream(&v, on_delta)?;
            return Ok(acc.finish());
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut raw_bytes: Vec<u8> = Vec::new();
        let mut done = false;

        while !done {
            let next = tokio::time::timeout(READ_IDLE_TIMEOUT, stream.next()).await;
            let chunk = match next {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(e))) => {
                    return Err(AppError::Llm(format!("SSE 读取中断: {e}")));
                }
                Ok(None) => break,
                Err(_) => {
                    // 空闲超时：若已有内容则返回已收集部分，否则报错
                    if acc.content.is_empty() && acc.tool_calls.is_empty() {
                        return Err(AppError::Llm(format!(
                            "LLM 流式响应空闲超过 {}s，无任何数据",
                            READ_IDLE_TIMEOUT.as_secs()
                        )));
                    }
                    break;
                }
            };

            // 增量按 UTF-8 边界安全拼接
            raw_bytes.extend_from_slice(&chunk);
            let (valid, rest) = split_utf8(&raw_bytes);
            raw_bytes = rest.to_vec();
            buf.push_str(&valid);

            // 统一换行，按 "\n\n" 切 event
            loop {
                let Some(pos) = find_event_boundary(&buf) else {
                    break;
                };
                let event = buf[..pos].to_string();
                buf = buf[pos..].to_string();

                match parse_event(&event, &mut acc, on_delta) {
                    Ok(true) => {
                        done = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(e) => return Err(e),
                }
            }
        }

        // 冲刷残留（服务端未以空行结束时）
        let tail = std::mem::take(&mut buf);
        let tail_trim = tail.trim();
        if !tail_trim.is_empty() {
            let _ = parse_event(tail_trim, &mut acc, on_delta)?;
        }

        Ok(acc.finish())
    }

    async fn list_models(&self) -> AppResult<Vec<String>> {
        let infos = self.list_model_infos().await?;
        Ok(infos.into_iter().map(|(id, _)| id).collect())
    }
}

impl OpenAiCompatProvider {
    /// 带 `owned_by` 的模型列表，供 `agent_list_models` 使用。
    pub async fn list_model_infos(&self) -> AppResult<Vec<(String, Option<String>)>> {
        let resp = self
            .apply_auth(self.client.get(self.models_url()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AppError::Llm(format!("获取模型列表超时: {e}"))
                } else {
                    AppError::Llm(format!("获取模型列表失败 {}: {e}", self.models_url()))
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let summary = self.read_body_summary(resp).await;
            return Err(AppError::Llm(format!(
                "获取模型列表失败（{}）：{summary}",
                status.as_u16()
            )));
        }

        let text = tokio::time::timeout(Duration::from_secs(30), resp.text())
            .await
            .map_err(|_| AppError::Llm("读取模型列表响应超时".into()))?
            .map_err(|e| AppError::Llm(format!("读取模型列表响应失败: {e}")))?;

        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            AppError::Llm(format!(
                "模型列表不是合法 JSON（{e}）：{}",
                truncate(&text, BODY_SUMMARY_LIMIT)
            ))
        })?;

        if let Some(err) = v.get("error") {
            return Err(AppError::Llm(format!("模型列表返回错误对象: {err}")));
        }

        let arr = v
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| {
                AppError::Llm(format!(
                    "模型列表结构异常（缺少 data 数组）：{}",
                    truncate(&text, BODY_SUMMARY_LIMIT)
                ))
            })?;

        let mut out: Vec<(String, Option<String>)> = Vec::new();
        for item in arr {
            let id = match item.get("id").and_then(|i| i.as_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                _ => continue,
            };
            let owned_by = item
                .get("owned_by")
                .and_then(|o| o.as_str())
                .map(|s| s.to_string());
            out.push((id, owned_by));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

// ───────────────────────────── SSE 解析 ─────────────────────────────

/// 流式聚合状态。
#[derive(Debug, Default)]
struct StreamAccumulator {
    content: String,
    tool_calls: Vec<AggToolCall>,
    finish_reason: Option<String>,
}

impl StreamAccumulator {
    /// 按 index 查找/创建聚合槽。
    fn slot(&mut self, index: u32) -> &mut AggToolCall {
        if let Some(pos) = self.tool_calls.iter().position(|c| c.index == index) {
            return &mut self.tool_calls[pos];
        }
        self.tool_calls.push(AggToolCall {
            index,
            ..Default::default()
        });
        let pos = self.tool_calls.len() - 1;
        &mut self.tool_calls[pos]
    }

    fn finish(&mut self) -> ChatOutcome {
        self.tool_calls.sort_by_key(|c| c.index);
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| ToolCall {
                id: if c.id.is_empty() {
                    format!("call_{}", c.index)
                } else {
                    c.id.clone()
                },
                kind: "function".into(),
                function: FunctionCall {
                    name: c.name.clone(),
                    arguments: if c.arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        c.arguments.clone()
                    },
                },
            })
            .collect();

        let finish_reason = match self.finish_reason.clone() {
            Some(r) if !r.is_empty() => r,
            _ => {
                if tool_calls.is_empty() {
                    "stop".to_string()
                } else {
                    "tool_calls".to_string()
                }
            }
        };

        ChatOutcome {
            finish_reason,
            content: std::mem::take(&mut self.content),
            tool_calls,
        }
    }

    /// 处理一次性 JSON 响应（非 SSE 兼容模式）。
    fn absorb_non_stream(
        &mut self,
        v: &serde_json::Value,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> AppResult<()> {
        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| AppError::Llm("LLM 响应缺少 choices[0]".into()))?;

        if let Some(msg) = choice.get("message") {
            if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    self.content.push_str(text);
                    on_delta(Delta::Text(text.to_string()));
                }
            }
            if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                for (i, tc) in tcs.iter().enumerate() {
                    let index = tc
                        .get("index")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(i as u64) as u32;
                    let id = tc
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let slot = self.slot(index);
                    slot.id = id;
                    slot.name = name;
                    slot.arguments = args;
                    slot.started = true;
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            if !fr.is_empty() {
                self.finish_reason = Some(fr.to_string());
            }
        }
        Ok(())
    }
}

/// 找到下一个 event 边界（`\n\n` 或 `\r\n\r\n`），返回消费长度。
fn find_event_boundary(buf: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    if let Some(p) = buf.find("\n\n") {
        best = Some(p + 2);
    }
    if let Some(p) = buf.find("\r\n\r\n") {
        let cand = p + 4;
        best = match best {
            Some(b) => Some(b.min(cand)),
            None => Some(cand),
        };
    }
    best
}

/// 解析一个 SSE event 文本；返回 `Ok(true)` 表示收到 `[DONE]`。
fn parse_event(
    event: &str,
    acc: &mut StreamAccumulator,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> AppResult<bool> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
        // `event:` / `id:` / `:` 注释 行无需处理
    }
    let data = data_lines.join("\n");
    let data = data.trim();
    if data.is_empty() {
        return Ok(false);
    }
    if data == "[DONE]" {
        return Ok(true);
    }

    let v: serde_json::Value = serde_json::from_str(data).map_err(|e| {
        AppError::Llm(format!(
            "SSE 数据不是合法 JSON（{e}）：{}",
            truncate(data, BODY_SUMMARY_LIMIT)
        ))
    })?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        return Err(AppError::Llm(format!("LLM 流内返回错误: {msg}")));
    }

    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
        return Ok(false);
    };

    for choice in choices {
        let delta = choice.get("delta");

        if let Some(delta) = delta {
            // 文本增量
            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    acc.content.push_str(text);
                    on_delta(Delta::Text(text.to_string()));
                }
            }

            // 工具调用增量
            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let index = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    let id_chunk = tc.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let name_chunk = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let args_chunk = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");

                    let mut emit_start: Option<(String, String)> = None;
                    {
                        let slot = acc.slot(index);
                        if !id_chunk.is_empty() {
                            slot.id = id_chunk.to_string();
                        }
                        if !name_chunk.is_empty() && slot.name.is_empty() {
                            slot.name = name_chunk.to_string();
                        }
                        if !slot.started && (!slot.name.is_empty() || !slot.id.is_empty()) {
                            slot.started = true;
                            emit_start = Some((slot.id.clone(), slot.name.clone()));
                        }
                        if !args_chunk.is_empty() {
                            slot.arguments.push_str(args_chunk);
                        }
                    }
                    if let Some((id, name)) = emit_start {
                        on_delta(Delta::ToolCallStart { index, id, name });
                    }
                    if !args_chunk.is_empty() {
                        on_delta(Delta::ToolCallArgs {
                            index,
                            chunk: args_chunk.to_string(),
                        });
                    }
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            if !fr.is_empty() {
                acc.finish_reason = Some(fr.to_string());
            }
        }
    }

    Ok(false)
}

// ───────────────────────────── 工具函数 ─────────────────────────────

/// 安全切出合法 UTF-8 前缀，返回 `(valid, rest)`。
fn split_utf8(bytes: &[u8]) -> (String, &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), &[]),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            let valid = String::from_utf8_lossy(&bytes[..valid_up_to]).to_string();
            (valid, &bytes[valid_up_to..])
        }
    }
}

fn truncate(s: &str, limit: usize) -> String {
    let mut out: String = s.chars().take(limit).collect();
    if s.chars().count() > limit {
        out.push_str("…");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalization() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:11434"),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1"
        );
        assert_eq!(normalize_base_url(""), "https://api.deepseek.com/v1");
    }

    #[test]
    fn event_boundary_split() {
        let buf = "data: {\"a\":1}\n\ndata: rest";
        assert_eq!(find_event_boundary(buf), Some(15));
        let buf = "data: {\"a\":1}\r\n\r\nnext";
        assert_eq!(find_event_boundary(buf), Some(17));
        assert_eq!(find_event_boundary("no boundary"), None);
    }

    #[test]
    fn parses_text_delta() {
        let mut acc = StreamAccumulator::default();
        let mut deltas: Vec<String> = Vec::new();
        {
            let mut sink = |d: Delta| match d {
                Delta::Text(t) => deltas.push(t),
                _ => {}
            };
            let ev = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}";
            let done = parse_event(ev, &mut acc, &mut sink).unwrap();
            assert!(!done);

            let ev = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}";
            parse_event(ev, &mut acc, &mut sink).unwrap();
        }
        assert_eq!(deltas, vec!["你好".to_string()]);
        assert_eq!(acc.content, "你好");
        assert_eq!(acc.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn aggregates_tool_call_fragments() {
        let mut acc = StreamAccumulator::default();
        let mut events: Vec<Delta> = Vec::new();
        let mut sink = |d: Delta| events.push(d);

        let chunks = [
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"run_command","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"comm"}}]}}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls -la\"}"}}]}}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        for ev in chunks {
            parse_event(ev, &mut acc, &mut sink).unwrap();
        }

        let outcome = acc.finish();
        assert_eq!(outcome.finish_reason, "tool_calls");
        assert_eq!(outcome.tool_calls.len(), 1);
        let tc = &outcome.tool_calls[0];
        assert_eq!(tc.id, "call_a");
        assert_eq!(tc.function.name, "run_command");
        assert_eq!(tc.function.arguments, "{\"command\":\"ls -la\"}");

        // ToolCallStart 只应发出一次
        let starts = events
            .iter()
            .filter(|d| matches!(d, Delta::ToolCallStart { .. }))
            .count();
        assert_eq!(starts, 1);
    }

    #[test]
    fn multiple_tool_calls_by_index() {
        let mut acc = StreamAccumulator::default();
        let mut sink = |_d: Delta| {};
        let ev1 = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c0","function":{"name":"run_command","arguments":"{}"}},{"index":1,"id":"c1","function":{"name":"list_sessions","arguments":"{}"}}]}}]}"#;
        parse_event(ev1, &mut acc, &mut sink).unwrap();
        let outcome = acc.finish();
        assert_eq!(outcome.tool_calls.len(), 2);
        assert_eq!(outcome.tool_calls[0].id, "c0");
        assert_eq!(outcome.tool_calls[1].function.name, "list_sessions");
    }

    #[test]
    fn splits_utf8_across_chunks() {
        let s = "你好".as_bytes();
        let (valid, rest) = split_utf8(&s[..1]);
        assert_eq!(valid, "");
        assert_eq!(rest.len(), 1);
        let combined: Vec<u8> = rest.iter().copied().chain(s[1..].iter().copied()).collect();
        let (valid, rest) = split_utf8(&combined);
        assert_eq!(valid, "你好");
        assert!(rest.is_empty());
    }

    #[test]
    fn done_marker() {
        let mut acc = StreamAccumulator::default();
        let mut sink = |_d: Delta| {};
        assert!(parse_event("data: [DONE]", &mut acc, &mut sink).unwrap());
        assert!(!parse_event(": keep-alive", &mut acc, &mut sink).unwrap());
    }
}
