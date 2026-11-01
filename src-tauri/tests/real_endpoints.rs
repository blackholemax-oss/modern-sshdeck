//! 真实端点探测（默认 `#[ignore]`，按需手动执行）。
//!
//! 这些测试**不验证业务成功**（那需要真实账号），而是验证：
//! **我们实现里用的端点路径与请求形状是被上游接受的**。
//!
//! 判据是「错误类型」：端点不存在会是 404 / "not found"；
//! 参数格式错会是 "invalid request"；而**参数被接受了、只差凭据**
//! 才返回 "token invalid" / "not authenticated"。
//!
//! 只有第三类才是我们想要的证据。
//!
//! 跑法：
//! ```bash
//! bash scripts/check.sh test 2>&1   # 或者
//! SSHDECK_REAL_ENDPOINTS=1 cargo test --release --test real_endpoints -- --ignored --nocapture
//! ```

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("client")
}

/// TRAE ExchangeToken：验证**四个字段的请求体形状**被接受。
///
/// 实测证据（2026-11）：
/// - 只发 `{"RefreshToken": "x"}` → `10101 无效参数：{__Message.field}`（缺字段）
/// - 发完整 4 字段 → `20101 refresh token is invalid`（**参数格式已通过**）
#[tokio::test]
#[ignore = "依赖外网 api.trae.com.cn，按需手动执行"]
async fn trae_exchange_token_accepts_four_field_body() {
    let body = serde_json::json!({
        "ClientID": "en1oxy7wnw8j9n",
        "RefreshToken": "invalid-test-token",
        "ClientSecret": "-",
        "UserID": "",
    });
    let resp = http()
        .post("https://api.trae.com.cn/cloudide/api/v3/trae/oauth/ExchangeToken")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "Trae/0.1.52")
        .json(&body)
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("[trae] HTTP {status}: {}", &text[..text.len().min(300)]);

    // 关键断言：必须是「token 无效」而不是「参数无效」
    assert!(
        text.contains("20101") || text.contains("refresh token is invalid"),
        "应为 token 无效（说明参数形状已通过），实际: {text}"
    );
    assert!(
        !text.contains("10101"),
        "不应再出现「无效参数」（说明请求体字段缺失）: {text}"
    );
}

/// ZCode 轮询端点：验证路径形状被接受。
///
/// 实测：`GET /api/v1/oauth/cli/poll/<fake-flow>` → `{"code":3004,"msg":"invalid_flow"}`
/// —— 说明**路径正确**（404 才是路径错）。
#[tokio::test]
#[ignore = "依赖外网 zcode.z.ai，按需手动执行"]
async fn zcode_poll_path_exists() {
    let resp = http()
        .get("https://zcode.z.ai/api/v1/oauth/cli/poll/nonexistent-flow")
        .header("Authorization", "Bearer test-secret")
        .header("User-Agent", "ZCode/3.14.3")
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("[zcode] HTTP {status}: {}", &text[..text.len().min(300)]);

    assert_ne!(status, reqwest::StatusCode::NOT_FOUND, "端点路径不应是 404");
    assert!(
        text.contains("invalid_flow") || text.contains("3004"),
        "应为 flow 无效（说明路径正确），实际: {text}"
    );
}

/// Qoder deviceToken/poll：验证两个区域的端点都存在。
///
/// 实测：两个 host 都返回 `401 {"errorCode":"Unauthorized",
/// "errorMessage":"User not authenticated"}` —— **端点存在、只差登录**。
#[tokio::test]
#[ignore = "依赖外网 qoder.com / qoder.com.cn，按需手动执行"]
async fn qoder_poll_path_exists_in_both_regions() {
    for host in ["https://qoder.com", "https://qoder.com.cn"] {
        let url = format!("{host}/api/v1/deviceToken/poll?machineId=t1&deviceId=t2");
        let resp = http().get(&url).send().await.expect("request");
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        println!("[qoder {host}] HTTP {status}: {}", &text[..text.len().min(200)]);

        assert_ne!(status, reqwest::StatusCode::NOT_FOUND, "{host} 端点不应是 404");
        assert!(
            text.contains("Unauthorized") || text.contains("not authenticated"),
            "{host} 应为未认证（说明端点存在），实际: {text}"
        );
    }
}

/// MiniMax 设备码：验证 **PKCE 是否被接受**。
///
/// 实测对照（2026-11）：
/// - 不带 `code_challenge` → `400 {"error":"invalid_request",
///   "error_description":"valid S256 PKCE is required"}`
/// - 带合法 S256 challenge → **返回真实设备码与短码**
///
/// 这是本轮修掉的一个真实缺陷。
#[tokio::test]
#[ignore = "依赖外网 account.minimax.cn，按需手动执行"]
async fn minimax_device_code_accepts_s256_pkce() {
    // 用固定的合法 challenge（来自 RFC 7636 例子），只验证被接受
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    let form = [
        ("client_id", "mcode-public"),
        ("scope", "agent.default"),
        ("audience", "agent-backend"),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];

    let resp = http()
        .post("https://account.minimax.cn/oauth2/device/code")
        .form(&form)
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("[minimax] HTTP {status}: {}", &text[..text.len().min(400)]);

    assert!(
        !text.contains("S256 PKCE is required"),
        "不应再报「需要 PKCE」：{text}"
    );
    assert!(
        text.contains("device_code") || text.contains("user_code"),
        "应返回真实设备码，实际: {text}"
    );
}
