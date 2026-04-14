use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
pub struct DashboardFilter {
    pub exchange: Option<String>,
    pub account_id: Option<Uuid>,
    pub account_type: Option<String>,
    pub strategy: Option<String>,
    pub pair: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TradeFilter {
    pub exchange: Option<String>,
    pub account_id: Option<Uuid>,
    pub strategy: Option<String>,
    pub pair: Option<String>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}
