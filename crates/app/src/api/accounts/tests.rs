use super::*;

#[test]
fn open_trades_error_becomes_conflict_without_account_id() {
    let id = Uuid::new_v4();
    let api_err = update_account_error_to_api_error(
        id,
        trading_accounts::UpdateAccountError::OpenTrades {
            account_id: id,
            count: 2,
        },
    );
    assert_eq!(api_err.0, StatusCode::CONFLICT);
    assert_eq!(
        api_err.1,
        "failed to deactivate account: 2 open/closing trade(s) exist; close them first"
    );
    assert!(
        !api_err.1.contains(&id.to_string()),
        "409 message must not contain the account id (already in the URL path): {}",
        api_err.1
    );
}

#[test]
fn other_error_falls_back_to_internal_server_error() {
    let api_err = update_account_error_to_api_error(
        Uuid::new_v4(),
        trading_accounts::UpdateAccountError::Other(anyhow::anyhow!("boom")),
    );
    assert_eq!(api_err.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(api_err.1, "internal error");
}
