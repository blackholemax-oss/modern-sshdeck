//! 高危命令二次确认（契约 `docs/FEATURE-MODEL-PROVIDERS.md` §5.2）。
//!
//! 二次确认是**审批之后的额外闸门**，不是替代品：
//! - `ReadOnly`：无需审批，也无需确认
//! - `Mutating`：单次审批（允许一次 / 拒绝）即可放行
//! - `Dangerous`：单次审批之外，用户必须输入**精确短语** [`CONFIRM_PHRASE`] 才放行；
//!   只差一个字都不放行（不做模糊匹配、不做同义词、不做大小写折叠）
//!
//! 短语比较前会 trim 首尾空白（用户从卡片复制时容易带空格），
//! 但短语内部字符必须完全一致。

use crate::ipc::dto::Risk;

/// 高危命令需要用户精确输入这个短语才放行。
pub const CONFIRM_PHRASE: &str = "确认执行";

/// 一次二次确认判定的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmState {
    /// 高危命令尚未提供确认短语（用户还没填）——仍需二次确认，不放行。
    Required,
    /// 满足要求，可以放行。
    Satisfied,
    /// 已提供短语但不匹配，或用户点了拒绝 —— 不放行。
    Rejected,
    /// 审批等待超时 —— 不放行。
    Timeout,
}

impl ConfirmState {
    /// 是否允许执行。**只有 `Satisfied` 为真**，其余一律拦截。
    pub fn is_satisfied(self) -> bool {
        matches!(self, ConfirmState::Satisfied)
    }

    /// 是否必须拦截（`!is_satisfied()` 的语义化写法）。
    pub fn is_blocking(self) -> bool {
        !self.is_satisfied()
    }

    /// 稳定字符串标识，用于审计与前端展示。
    pub fn label(self) -> &'static str {
        match self {
            ConfirmState::Required => "required",
            ConfirmState::Satisfied => "satisfied",
            ConfirmState::Rejected => "rejected",
            ConfirmState::Timeout => "timeout",
        }
    }
}

/// 短语是否精确匹配（首尾空白 trim，内部字符完全一致）。
pub fn phrase_matches(phrase: Option<&str>) -> bool {
    match phrase {
        Some(p) => p.trim() == CONFIRM_PHRASE,
        None => false,
    }
}

/// 判断一次审批是否满足二次确认要求。
///
/// - `ReadOnly` / `Mutating` → `Satisfied`（这两级由「单次审批」闸门把关）
/// - `Dangerous` + 短语精确匹配 → `Satisfied`
/// - `Dangerous` + 未提供（`None` / 空串）→ `Required`
/// - `Dangerous` + 提供了但不匹配 → `Rejected`
pub fn check_confirm(risk: Risk, phrase: Option<&str>) -> ConfirmState {
    match risk {
        Risk::ReadOnly | Risk::Mutating => ConfirmState::Satisfied,
        Risk::Dangerous => {
            if phrase_matches(phrase) {
                ConfirmState::Satisfied
            } else if phrase.map(|p| p.trim().is_empty()).unwrap_or(true) {
                ConfirmState::Required
            } else {
                ConfirmState::Rejected
            }
        }
    }
}

/// 结合审批按钮结果（允许 / 拒绝）给出完整判定。
///
/// 用户拒绝时不看短语，直接 `Rejected`（fail-safe）。
pub fn resolve(risk: Risk, approved: bool, phrase: Option<&str>) -> ConfirmState {
    if !approved {
        return ConfirmState::Rejected;
    }
    check_confirm(risk, phrase)
}

/// 审批等待超时的判定（`Timeout` 状态只从这里产生）。
pub fn timeout() -> ConfirmState {
    ConfirmState::Timeout
}

/// 审批卡片上展示给用户的二次确认提示文案。
pub fn danger_confirm_hint() -> String {
    format!("高危命令：必须精确输入「{CONFIRM_PHRASE}」才可放行（不做模糊匹配）")
}

/// 二次确认失败时的可读原因（进审计 / 回灌模型）。
pub fn failure_message(state: ConfirmState) -> Option<String> {
    match state {
        ConfirmState::Satisfied => None,
        ConfirmState::Required => Some(format!(
            "高危命令二次确认未完成：未提供确认短语「{CONFIRM_PHRASE}」"
        )),
        ConfirmState::Rejected => Some(format!(
            "高危命令二次确认未通过：确认短语必须精确等于「{CONFIRM_PHRASE}」"
        )),
        ConfirmState::Timeout => Some("审批超时，命令未执行".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_needs_no_confirm() {
        assert_eq!(check_confirm(Risk::ReadOnly, None), ConfirmState::Satisfied);
        assert_eq!(
            check_confirm(Risk::ReadOnly, Some("随便什么")),
            ConfirmState::Satisfied
        );
    }

    #[test]
    fn mutating_needs_single_approval_only() {
        // Mutating 由审批闸门把关，二次确认短语不参与判定
        assert_eq!(check_confirm(Risk::Mutating, None), ConfirmState::Satisfied);
        assert_eq!(
            check_confirm(Risk::Mutating, Some("确认执行")),
            ConfirmState::Satisfied
        );
    }

    #[test]
    fn dangerous_without_phrase_is_required() {
        assert_eq!(check_confirm(Risk::Dangerous, None), ConfirmState::Required);
        assert_eq!(
            check_confirm(Risk::Dangerous, Some("")),
            ConfirmState::Required
        );
        assert_eq!(
            check_confirm(Risk::Dangerous, Some("   ")),
            ConfirmState::Required
        );
    }

    #[test]
    fn dangerous_with_exact_phrase_is_satisfied() {
        assert_eq!(
            check_confirm(Risk::Dangerous, Some(CONFIRM_PHRASE)),
            ConfirmState::Satisfied
        );
        // 首尾空白 trim 后比较
        assert_eq!(
            check_confirm(Risk::Dangerous, Some("\t  确认执行 \r\n")),
            ConfirmState::Satisfied
        );
        // 全角空格同样被 trim（Unicode 空白）
        assert_eq!(
            check_confirm(Risk::Dangerous, Some("\u{3000}确认执行\u{3000}")),
            ConfirmState::Satisfied
        );
    }

    #[test]
    fn dangerous_with_wrong_phrase_is_rejected() {
        for bad in [
            "执行",
            "确认",
            "确认执行。",        // 多一个标点
            "确认执行命令",      // 多字
            "确认 执行",         // 中间多空格
            "请确认执行",
            "confirm",
            "確認執行",          // 繁体
            "确认执行!",
        ] {
            assert_eq!(
                check_confirm(Risk::Dangerous, Some(bad)),
                ConfirmState::Rejected,
                "短语 {bad:?} 不应通过二次确认"
            );
        }
    }

    #[test]
    fn resolve_rejects_when_user_denies() {
        // 用户点拒绝：即使短语正确也不放行
        assert_eq!(
            resolve(Risk::Dangerous, false, Some(CONFIRM_PHRASE)),
            ConfirmState::Rejected
        );
        assert_eq!(resolve(Risk::Mutating, false, None), ConfirmState::Rejected);
        assert_eq!(resolve(Risk::ReadOnly, false, None), ConfirmState::Rejected);
    }

    #[test]
    fn resolve_follows_confirm_gate() {
        assert_eq!(
            resolve(Risk::Dangerous, true, Some(CONFIRM_PHRASE)),
            ConfirmState::Satisfied
        );
        assert_eq!(
            resolve(Risk::Dangerous, true, None),
            ConfirmState::Required
        );
        assert_eq!(resolve(Risk::Mutating, true, None), ConfirmState::Satisfied);
        assert_eq!(resolve(Risk::ReadOnly, true, None), ConfirmState::Satisfied);
    }

    #[test]
    fn only_satisfied_is_allowed() {
        assert!(ConfirmState::Satisfied.is_satisfied());
        assert!(!ConfirmState::Satisfied.is_blocking());
        for blocked in [
            ConfirmState::Required,
            ConfirmState::Rejected,
            ConfirmState::Timeout,
        ] {
            assert!(!blocked.is_satisfied(), "{blocked:?} 不该放行");
            assert!(blocked.is_blocking(), "{blocked:?} 应拦截");
        }
    }

    #[test]
    fn timeout_state_blocks() {
        assert_eq!(timeout(), ConfirmState::Timeout);
        assert!(timeout().is_blocking());
        assert_eq!(timeout().label(), "timeout");
    }

    #[test]
    fn failure_message_is_actionable() {
        assert!(failure_message(ConfirmState::Satisfied).is_none());
        let required = failure_message(ConfirmState::Required).unwrap();
        assert!(required.contains(CONFIRM_PHRASE), "提示必须写明短语");
        let rejected = failure_message(ConfirmState::Rejected).unwrap();
        assert!(rejected.contains("精确"));
        assert!(danger_confirm_hint().contains(CONFIRM_PHRASE));
        assert_eq!(ConfirmState::Required.label(), "required");
        assert_eq!(ConfirmState::Rejected.label(), "rejected");
    }
}
