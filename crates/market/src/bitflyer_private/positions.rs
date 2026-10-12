use rust_decimal::Decimal;
use urlencoding::encode as url_encode;

use super::http::{BitflyerPrivateApi, truncate_body};
use super::types::{BitflyerApiError, Collateral, ExchangePosition};

impl BitflyerPrivateApi {
    pub async fn get_positions(
        &self,
        product_code: &str,
    ) -> Result<Vec<ExchangePosition>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getpositions?product_code={}",
            url_encode(product_code)
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn get_collateral(&self) -> Result<Collateral, BitflyerApiError> {
        let text = self.request("GET", "/v1/me/getcollateral", "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn fetch_close_sfd(&self, product_code: &str) -> Result<Decimal, BitflyerApiError> {
        let positions = self.get_positions(product_code).await?;
        Ok(positions.iter().map(|p| p.sfd).sum())
    }
}

#[cfg(test)]
mod tests;
