//! bitFlyer Lightning Private REST API クライアント。
//!
//! 認証: `ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)`
//! レート制限: 200 req / 5 min (IP 単位)。
//!
//! 本モジュールは HTTP 境界までを閉じる。ドメインオブジェクト
//! (Trade, Signal 等) への変換は呼び出し側 (`LiveTrader` in PR 3)
//! が担う。

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

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
}
