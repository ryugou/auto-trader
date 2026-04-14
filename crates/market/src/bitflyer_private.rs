//! bitFlyer Lightning Private REST API クライアント。
//!
//! 認証: `ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)`
//! レート制限: 200 req / 5 min (IP 単位)。
//!
//! 本モジュールは HTTP 境界までを閉じる。ドメインオブジェクト
//! (Trade, Signal 等) への変換は呼び出し側 (`LiveTrader` in PR 3)
//! が担う。

use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

/// `POST /v1/me/sendchildorder` リクエスト本体。
///
/// `time_in_force` / `minute_to_expire` はデフォルトのまま取引所任せに
/// したいケースが多いので `Option`。PR 3 の LiveTrader は常に
/// `time_in_force = None` (GTC 相当) で送る想定。
#[derive(Debug, Clone, Serialize)]
pub struct SendChildOrderRequest {
    pub product_code: String,
    pub child_order_type: ChildOrderType,
    pub side: Side,
    pub size: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute_to_expire: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendChildOrderResponse {
    pub child_order_acceptance_id: String,
}

/// `GET /v1/me/getchildorders` 個別要素。
///
/// bitFlyer レスポンスはすべて数値を文字列で返すため、
/// `rust_decimal` の `serde-with-str` feature (workspace 既定) で
/// 文字列 → Decimal を直接パースする。
#[derive(Debug, Clone, Deserialize)]
pub struct ChildOrder {
    pub id: u64,
    pub child_order_id: String,
    pub product_code: String,
    pub side: String,
    pub child_order_type: String,
    pub price: Decimal,
    pub average_price: Decimal,
    pub size: Decimal,
    pub child_order_state: ChildOrderState,
    pub expire_date: String,
    pub child_order_date: String,
    pub child_order_acceptance_id: String,
    pub outstanding_size: Decimal,
    pub cancel_size: Decimal,
    pub executed_size: Decimal,
    pub total_commission: Decimal,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderState {
    Active,
    Completed,
    Canceled,
    Expired,
    Rejected,
}

/// `GET /v1/me/getexecutions` 個別要素。
#[derive(Debug, Clone, Deserialize)]
pub struct Execution {
    pub id: u64,
    pub child_order_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub exec_date: String,
    pub child_order_acceptance_id: String,
}

/// `GET /v1/me/getpositions` 個別要素 (FX/CFD 専用)。
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangePosition {
    pub product_code: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub swap_point_accumulate: Decimal,
    pub require_collateral: Decimal,
    pub open_date: String,
    pub leverage: Decimal,
    pub pnl: Decimal,
    pub sfd: Decimal,
}

/// `GET /v1/me/getcollateral` レスポンス。
#[derive(Debug, Clone, Deserialize)]
pub struct Collateral {
    pub collateral: Decimal,
    pub open_position_pnl: Decimal,
    pub require_collateral: Decimal,
    pub keep_rate: Decimal,
}

/// bitFlyer がエラーレスポンスで返す JSON の raw 形。
/// `status` が負数で返り、`error_message` が日本語/英語の
/// 人間向け説明。`data` は null または詳細オブジェクト。
///
/// 後段の `BitflyerApiError` で分類する。
#[derive(Debug, Clone, Deserialize)]
pub struct BitflyerErrorBody {
    pub status: i32,
    pub error_message: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// bitFlyer Private API が返しうる失敗の分類。
///
/// - `InsufficientFunds`: 残高不足。LiveTrader はアカウントを halt 対象に。
/// - `InvalidApiKey` / `InvalidSignature`: 認証失敗。起動時に発火すれば fatal。
/// - `RateLimited`: HTTP 429。governor の前段ガードを通過してしまった場合
///   (= 他プロセスや bitFlyer 側の偏り) の防衛線。
/// - `OrderNotFound`: 存在しない注文 ID。reconciler で手動対処。
/// - `ApiError { status, message }`: 上記に当てはまらない bitFlyer エラー。
/// - `Http`: reqwest 層のエラー。必ず `without_url()` で URL を落とす
///   (PR 1 notify crate と同じ方針)。
/// - `InvalidResponse`: HTTP は成功したが JSON パース失敗。
#[derive(Debug, Error)]
pub enum BitflyerApiError {
    #[error("insufficient funds: {0}")]
    InsufficientFunds(String),
    #[error("invalid api key")]
    InvalidApiKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    #[error("order not found: {0}")]
    OrderNotFound(String),
    #[error("bitflyer api error: status={status} message={message}")]
    ApiError { status: i32, message: String },
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<reqwest::Error> for BitflyerApiError {
    fn from(e: reqwest::Error) -> Self {
        // Slack Webhook 同様、reqwest::Error の Display は URL を含むため
        // without_url() で落とす。api_secret/api_key 自体は header に入る
        // ため URL には載らないが、将来 query string に何らかの識別子を
        // 入れたとき防衛線になる。
        BitflyerApiError::Http(e.without_url())
    }
}

impl BitflyerApiError {
    /// bitFlyer の raw error body から型付きエラーへ分類する。
    pub fn from_body(body: BitflyerErrorBody) -> Self {
        match body.status {
            -200 | -205 => BitflyerApiError::InsufficientFunds(body.error_message),
            -201 => BitflyerApiError::InvalidApiKey,
            -207 => BitflyerApiError::InvalidSignature,
            -208 => BitflyerApiError::OrderNotFound(body.error_message),
            s => BitflyerApiError::ApiError {
                status: s,
                message: body.error_message,
            },
        }
    }
}

/// bitFlyer Private REST API クライアント。
///
/// コンストラクタは `new` (本番) と `new_for_test` (wiremock / 単体
/// テスト) を分離し、テストが本番 URL を誤って叩かないよう型で
/// ガードする。
///
/// `Debug` は手書き実装で `api_key` / `api_secret` を `"***redacted***"` に
/// 置換する。将来 `#[derive(Debug)]` に差し替えると漏洩するため derive 禁止。
#[derive(Clone)]
pub struct BitflyerPrivateApi {
    base_url: String,
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for BitflyerPrivateApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitflyerPrivateApi")
            .field("base_url", &self.base_url)
            .field("api_key", &"***redacted***")
            .field("api_secret", &"***redacted***")
            .finish_non_exhaustive()
    }
}

impl BitflyerPrivateApi {
    /// 本番用コンストラクタ。`base_url` は "https://api.bitflyer.com" 固定想定。
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self::with_base_url("https://api.bitflyer.com".to_string(), api_key, api_secret)
    }

    /// ベース URL を明示するコンストラクタ。テスト・本番切り替え用。
    pub fn with_base_url(base_url: String, api_key: String, api_secret: String) -> Self {
        Self {
            base_url,
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
        }
    }

    /// wiremock テスト専用のコンストラクタ。`#[cfg(test)]` ではなく
    /// `new_for_test` という名前で区別することで統合テスト (別 crate
    /// 境界をまたぐ `crates/market/tests/*.rs`) からも呼べるようにする。
    #[doc(hidden)]
    pub fn new_for_test(base_url: String, api_key: String, api_secret: String) -> Self {
        Self::with_base_url(base_url, api_key, api_secret)
    }

    /// 認証ヘッダ 3 本を生成する pure function (テスト可能)。
    pub(crate) fn auth_headers(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> HashMap<&'static str, String> {
        let sig = sign(&self.api_secret, timestamp, method, path, body);
        let mut h = HashMap::new();
        h.insert("ACCESS-KEY", self.api_key.clone());
        h.insert("ACCESS-TIMESTAMP", timestamp.to_string());
        h.insert("ACCESS-SIGN", sig);
        h
    }

    /// 現在時刻の Unix 秒を 10 進文字列で返す。テストで時刻を固定
    /// したい場合は呼び出し側で auth_headers を直接叩く。
    fn current_timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before 1970");
        now.as_secs().to_string()
    }

    /// 共通 HTTP リクエストラッパー。
    ///
    /// - `method`: "GET" / "POST"
    /// - `path`: 例 "/v1/me/getcollateral" (クエリ文字列を含むこと)
    /// - `body_json`: POST 本体 JSON 文字列 (GET は "")
    ///
    /// 成功時は bitFlyer の raw レスポンスを (2xx, body_string) で返す。
    /// HTTP ステータスが 2xx でも JSON body に `status: <負数>` が
    /// 入っていれば `BitflyerApiError::from_body` で分類する。
    pub(crate) async fn request(
        &self,
        method: &str,
        path: &str,
        body_json: &str,
    ) -> Result<String, BitflyerApiError> {
        let url = format!("{}{}", self.base_url, path);
        let ts = Self::current_timestamp();
        let headers = self.auth_headers(&ts, method, path, body_json);

        let mut req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            _ => {
                return Err(BitflyerApiError::InvalidResponse(format!(
                    "unsupported method: {method}"
                )));
            }
        };
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        if method == "POST" {
            req = req
                .header("Content-Type", "application/json")
                .body(body_json.to_string());
        }

        let resp = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            let retry_after = parse_retry_after(&resp);
            // body 読み取りは 429 確定後でよい (不要なら読まない)
            let _ = resp.text().await;
            return Err(BitflyerApiError::RateLimited { retry_after });
        }

        let text = resp.text().await.map_err(|e| {
            tracing::warn!(method, path, "failed to read response body");
            BitflyerApiError::Http(e.without_url())
        })?;

        if !status.is_success() {
            // 非 2xx レスポンスは body に BitflyerErrorBody が載っている
            // ことが多い。パース失敗したら InvalidResponse に fallback。
            return match serde_json::from_str::<BitflyerErrorBody>(&text) {
                Ok(body) => Err(BitflyerApiError::from_body(body)),
                Err(_) => {
                    // body 全文をログに出さないよう先頭 512 文字にトリム
                    let truncated: String = text.chars().take(512).collect();
                    tracing::warn!(
                        method,
                        path,
                        status = status.as_u16(),
                        "non-2xx response with unrecognised body"
                    );
                    Err(BitflyerApiError::InvalidResponse(format!(
                        "non-2xx status {} body (truncated to 512 chars): {}",
                        status.as_u16(),
                        truncated
                    )))
                }
            };
        }

        // 2xx でも bitFlyer は `{"status":-200,...}` を返すことがある。
        // status フィールドを覗いて負数なら error として扱う。
        if let Ok(body) = serde_json::from_str::<BitflyerErrorBody>(&text)
            && body.status < 0
        {
            return Err(BitflyerApiError::from_body(body));
        }

        Ok(text)
    }

    /// `POST /v1/me/sendchildorder` — 成行/指値注文を発行する。
    pub async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> Result<SendChildOrderResponse, BitflyerApiError> {
        let body = serde_json::to_string(&req)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("serialize: {e}")))?;
        let text = self.request("POST", "/v1/me/sendchildorder", &body).await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }

    /// `GET /v1/me/getchildorders` — `child_order_acceptance_id` で特定の
    /// 注文 (とその状態) を取得する。bitFlyer は複数件返すが、acceptance_id
    /// で絞ると通常 1 件。
    pub async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<ChildOrder>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getchildorders?product_code={}&child_order_acceptance_id={}",
            product_code, child_order_acceptance_id
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }

    /// `GET /v1/me/getexecutions` — 約定一覧を取得する。
    pub async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<Execution>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getexecutions?product_code={}&child_order_acceptance_id={}",
            product_code, child_order_acceptance_id
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }
}

/// HTTP レスポンスの `Retry-After` ヘッダから待機時間を解析する。
///
/// bitFlyer は秒数整数 (例: `"5"`) を返す。HTTP-date 形式は現在サポートしない。
/// 解析失敗時は `None` を返す (= 安全側フォールバック)。
fn parse_retry_after(resp: &reqwest::Response) -> Option<std::time::Duration> {
    let hdr = resp.headers().get("retry-after")?.to_str().ok()?;
    hdr.parse::<u64>().ok().map(std::time::Duration::from_secs)
}

/// bitFlyer Private API の `ACCESS-SIGN` ヘッダを計算する。
///
/// 仕様:
///   ACCESS-SIGN = HMAC-SHA256(api_secret, timestamp + method + path + body)
///
/// - `timestamp`: Unix 秒を 10 進数文字列で表したもの
/// - `method`: 大文字 HTTP メソッド ("GET" / "POST")
/// - `path`: クエリ文字列含むパス (例: "/v1/me/getchildorders?count=100")
/// - `body`: POST 本体 (GET の場合は空文字列 "")
///
/// 返り値は 小文字 16 進数 64 文字。
pub(crate) fn sign(
    api_secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(api_secret.as_bytes())
        .expect("HMAC key size is never rejected by Hmac");
    mac.update(timestamp.as_bytes());
    mac.update(method.as_bytes());
    mac.update(path.as_bytes());
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_matches_known_hmac_sha256_vector() {
        // bitFlyer ドキュメント準拠の例:
        //   timestamp = "1234567890"
        //   method    = "GET"
        //   path      = "/v1/me/getpermissions"
        //   body      = ""
        //   secret    = "test_secret"
        // 上記に対する HMAC-SHA256 の hex 出力。Python 等の
        // 他実装で生成した期待値を固定し、rustfmt/rand シードに
        // 依存しない決定的スナップショットとする。
        let sig = sign(
            "test_secret",
            "1234567890",
            "GET",
            "/v1/me/getpermissions",
            "",
        );
        // 期待値は `python3 -c "import hmac,hashlib;
        //   print(hmac.new(b'test_secret',
        //     b'1234567890GET/v1/me/getpermissions',
        //     hashlib.sha256).hexdigest())"` で生成。
        assert_eq!(
            sig,
            "444e09cb91f6fd945ce0d21fa047e8c66d1fdf87c047c26f39d968c7352cd5c0"
        );
    }

    #[test]
    fn sign_includes_post_body() {
        let with_body = sign(
            "secret",
            "1000",
            "POST",
            "/v1/me/sendchildorder",
            r#"{"product_code":"FX_BTC_JPY"}"#,
        );
        let without_body = sign("secret", "1000", "POST", "/v1/me/sendchildorder", "");
        assert_ne!(with_body, without_body, "body must affect the signature");
    }

    use rust_decimal_macros::dec;

    #[test]
    fn deserialize_send_child_order_response() {
        let json = r#"{"child_order_acceptance_id":"JRF20150707-050237-639234"}"#;
        let r: SendChildOrderResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.child_order_acceptance_id, "JRF20150707-050237-639234");
    }

    #[test]
    fn deserialize_child_order() {
        let json = r#"{
            "id": 138398,
            "child_order_id": "JOR20150707-084555-022523",
            "product_code": "BTC_JPY",
            "side": "BUY",
            "child_order_type": "LIMIT",
            "price": "30000",
            "average_price": "30000",
            "size": "0.1",
            "child_order_state": "COMPLETED",
            "expire_date": "2015-07-14T07:25:47",
            "child_order_date": "2015-07-07T08:45:47",
            "child_order_acceptance_id": "JRF20150707-084555-022523",
            "outstanding_size": "0",
            "cancel_size": "0",
            "executed_size": "0.1",
            "total_commission": "0"
        }"#;
        let o: ChildOrder = serde_json::from_str(json).unwrap();
        assert_eq!(o.child_order_id, "JOR20150707-084555-022523");
        assert_eq!(o.side, "BUY");
        assert_eq!(o.child_order_state, ChildOrderState::Completed);
        assert_eq!(o.size, dec!(0.1));
    }

    #[test]
    fn deserialize_execution() {
        let json = r#"{
            "id": 37233,
            "child_order_id": "JOR20150707-084555-022523",
            "side": "BUY",
            "price": "30000",
            "size": "0.1",
            "commission": "0",
            "exec_date": "2015-07-07T09:57:40.397",
            "child_order_acceptance_id": "JRF20150707-084555-022523"
        }"#;
        let e: Execution = serde_json::from_str(json).unwrap();
        assert_eq!(e.id, 37233);
        assert_eq!(e.size, dec!(0.1));
    }

    #[test]
    fn deserialize_collateral() {
        let json = r#"{
            "collateral": "100000",
            "open_position_pnl": "-715",
            "require_collateral": "19857",
            "keep_rate": "5.0"
        }"#;
        let c: Collateral = serde_json::from_str(json).unwrap();
        assert_eq!(c.collateral, dec!(100000));
        assert_eq!(c.open_position_pnl, dec!(-715));
    }

    #[test]
    fn deserialize_exchange_position() {
        let json = r#"{
            "product_code": "FX_BTC_JPY",
            "side": "BUY",
            "price": "36000",
            "size": "10",
            "commission": "0",
            "swap_point_accumulate": "-35",
            "require_collateral": "120000",
            "open_date": "2015-11-03T10:04:45.011",
            "leverage": "3",
            "pnl": "965",
            "sfd": "0"
        }"#;
        let p: ExchangePosition = serde_json::from_str(json).unwrap();
        assert_eq!(p.product_code, "FX_BTC_JPY");
        assert_eq!(p.size, dec!(10));
    }

    #[test]
    fn deserialize_bitflyer_api_error() {
        let json = r#"{"status": -205, "error_message": "Insufficient fund", "data": null}"#;
        let e: BitflyerErrorBody = serde_json::from_str(json).unwrap();
        assert_eq!(e.status, -205);
        assert_eq!(e.error_message, "Insufficient fund");
    }

    #[test]
    fn auth_headers_contain_key_timestamp_and_sign() {
        let api = BitflyerPrivateApi::new_for_test(
            "http://example.invalid".to_string(),
            "test-key".to_string(),
            "test-secret".to_string(),
        );
        let headers = api.auth_headers("1234567890", "GET", "/v1/me/getcollateral", "");
        assert_eq!(headers.get("ACCESS-KEY").unwrap(), "test-key");
        assert_eq!(headers.get("ACCESS-TIMESTAMP").unwrap(), "1234567890");
        // signature は Task 3 の sign() と一致するはず
        let expected = sign(
            "test-secret",
            "1234567890",
            "GET",
            "/v1/me/getcollateral",
            "",
        );
        assert_eq!(headers.get("ACCESS-SIGN").unwrap(), &expected);
    }

    // --- Batch C regression tests ---

    /// [CRITICAL] Debug impl が api_key / api_secret をリテラルで出力しないことを確認。
    #[test]
    fn debug_redacts_api_key_and_secret() {
        let api = BitflyerPrivateApi::new_for_test(
            "http://example.invalid".to_string(),
            "my-super-key".to_string(),
            "my-super-secret".to_string(),
        );
        let dbg = format!("{:?}", api);
        assert!(
            !dbg.contains("my-super-key"),
            "api_key must not appear in Debug output, got: {dbg}"
        );
        assert!(
            !dbg.contains("my-super-secret"),
            "api_secret must not appear in Debug output, got: {dbg}"
        );
        assert!(
            dbg.contains("***redacted***"),
            "Debug output should contain '***redacted***', got: {dbg}"
        );
    }

    /// [CRITICAL] request() が 503 + 2000 文字ボディを返したとき、
    /// InvalidResponse メッセージが先頭 512 文字にトリムされること。
    #[tokio::test]
    async fn invalid_response_body_is_truncated() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let long_body: String = "x".repeat(2000);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/getcollateral"))
            .respond_with(ResponseTemplate::new(503).set_body_string(long_body.clone()))
            .mount(&server)
            .await;

        let api =
            BitflyerPrivateApi::new_for_test(server.uri(), "key".to_string(), "secret".to_string());
        let err = api
            .request("GET", "/v1/me/getcollateral", "")
            .await
            .unwrap_err();

        let displayed = err.to_string();
        // ボディが丸ごと入っていないこと (2000 文字は含まれないはず)
        assert!(
            displayed.len() < 700,
            "error message must be truncated (< 700 chars), got len={}",
            displayed.len()
        );
        // 先頭 512 文字の 'x' は含まれる
        assert!(
            displayed.contains(&"x".repeat(512)),
            "first 512 chars of body must appear in message"
        );
        // 513 文字目以降の 'x' は含まれない (trunc で切れているはず)
        assert!(
            !displayed.contains(&"x".repeat(513)),
            "body beyond 512 chars must not appear in message"
        );
    }

    /// [IMPORTANT] RateLimited が Retry-After 秒数を保持できること。
    #[tokio::test]
    async fn rate_limited_captures_retry_after() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/getcollateral"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "5")
                    .set_body_string("rate limited"),
            )
            .mount(&server)
            .await;

        let api =
            BitflyerPrivateApi::new_for_test(server.uri(), "key".to_string(), "secret".to_string());
        let err = api
            .request("GET", "/v1/me/getcollateral", "")
            .await
            .unwrap_err();

        match err {
            BitflyerApiError::RateLimited { retry_after } => {
                assert_eq!(
                    retry_after,
                    Some(std::time::Duration::from_secs(5)),
                    "should capture Retry-After: 5"
                );
            }
            other => panic!("expected RateLimited, got {:?}", other),
        }
    }
}
