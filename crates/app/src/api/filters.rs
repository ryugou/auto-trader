use serde::Deserialize;
use uuid::Uuid;

/// Query params for `GET /api/trading-accounts`.
///
/// Default (`include_inactive` absent or `false`) hides retired accounts —
/// callers that need the full roster (e.g. an admin screen listing retired
/// accounts) must opt in explicitly.
#[derive(Debug, Deserialize, Default)]
pub struct AccountsFilter {
    pub include_inactive: Option<bool>,
}

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
