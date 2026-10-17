use rust_decimal::Decimal;
use urlencoding::encode as url_encode;

use super::http::{BitflyerPrivateApi, truncate_body};
use super::types::{BitflyerApiError, Collateral, ExchangePosition};

impl BitflyerPrivateApi {
    /// Returns the open positions of `product_code`.
    ///
    /// # Errors
    ///
    /// - Returns an error if the request cannot be sent or the response body cannot be read.
    /// - Returns an error if the exchange answers with status 429, with the delay it asks for when it gives one.
    /// - Returns an error if the exchange reports insufficient funds (status -200 or -205).
    /// - Returns an error if the exchange reports an invalid API key (status -201).
    /// - Returns an error if the exchange reports an invalid signature (status -207).
    /// - Returns an error if the exchange reports that the order is not found (status -208).
    /// - Returns an error if the exchange reports any other error status.
    /// - Returns an error if an error response cannot be parsed.
    /// - Returns an error if the response body cannot be parsed.
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

    /// Returns the collateral of the account.
    ///
    /// # Errors
    ///
    /// - Returns an error if the request cannot be sent or the response body cannot be read.
    /// - Returns an error if the exchange answers with status 429, with the delay it asks for when it gives one.
    /// - Returns an error if the exchange reports insufficient funds (status -200 or -205).
    /// - Returns an error if the exchange reports an invalid API key (status -201).
    /// - Returns an error if the exchange reports an invalid signature (status -207).
    /// - Returns an error if the exchange reports that the order is not found (status -208).
    /// - Returns an error if the exchange reports any other error status.
    /// - Returns an error if an error response cannot be parsed.
    /// - Returns an error if the response body cannot be parsed.
    pub async fn get_collateral(&self) -> Result<Collateral, BitflyerApiError> {
        let text = self.request("GET", "/v1/me/getcollateral", "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    /// Returns the sum of the SFD of the open positions of `product_code`.
    ///
    /// # Errors
    ///
    /// - Returns an error if the request cannot be sent or the response body cannot be read.
    /// - Returns an error if the exchange answers with status 429, with the delay it asks for when it gives one.
    /// - Returns an error if the exchange reports insufficient funds (status -200 or -205).
    /// - Returns an error if the exchange reports an invalid API key (status -201).
    /// - Returns an error if the exchange reports an invalid signature (status -207).
    /// - Returns an error if the exchange reports that the order is not found (status -208).
    /// - Returns an error if the exchange reports any other error status.
    /// - Returns an error if an error response cannot be parsed.
    /// - Returns an error if the response body cannot be parsed.
    pub async fn fetch_close_sfd(&self, product_code: &str) -> Result<Decimal, BitflyerApiError> {
        let positions = self.get_positions(product_code).await?;
        Ok(positions.iter().map(|p| p.sfd).sum())
    }
}

#[cfg(test)]
mod tests;
