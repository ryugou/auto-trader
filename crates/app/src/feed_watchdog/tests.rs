use chrono::TimeZone;

use super::*;

fn t(secs_from_epoch: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs_from_epoch, 0)
        .unwrap()
}

fn feed(exchange: &str, pair: &str) -> FeedId {
    (exchange.to_string(), pair.to_string())
}

#[test]
fn suspend_not_detected_below_threshold() {
    let prev = t(0);
    let now = t(299);
    assert_eq!(detect_suspend_gap(prev, now, 300), None);
}

#[test]
fn suspend_detected_exactly_at_threshold() {
    let prev = t(0);
    let now = t(300);
    let gap = detect_suspend_gap(prev, now, 300).expect("boundary must detect");
    assert_eq!(gap.gap_secs, 300);
    assert_eq!(gap.started_at, prev);
    assert_eq!(gap.resumed_at, now);
}

#[test]
fn suspend_detected_above_threshold() {
    let prev = t(0);
    let two_days_and_59_minutes = 2 * 24 * 3600 + 3540;
    let now = t(two_days_and_59_minutes);
    let gap = detect_suspend_gap(prev, now, 300).expect("long gap must detect");
    assert_eq!(gap.gap_secs, two_days_and_59_minutes);
}

#[test]
fn suspend_alert_body_contains_duration_and_jst_timestamps() {
    let gap = SuspendGap {
        started_at: Utc.with_ymd_and_hms(2026, 10, 1, 6, 49, 0).unwrap(),
        resumed_at: Utc.with_ymd_and_hms(2026, 10, 3, 5, 58, 0).unwrap(),
        gap_secs: 169_740,
    };
    let check_interval_secs = 60;
    let body = format_suspend_alert(&gap, check_interval_secs);
    assert!(body.contains("approximately 2829 minute"), "body={body}");
    assert!(body.contains("2026-10-01 15:49:00 JST"), "body={body}");
    assert!(body.contains("2026-10-03 14:58:00 JST"), "body={body}");
    assert!(
        body.contains(&format!("up to {check_interval_secs}s longer")),
        "body must explain the check-interval measurement error margin: {body}"
    );
    assert!(
        body.to_lowercase().contains("sleep") || body.to_lowercase().contains("suspend"),
        "body must mention host sleep/suspend possibility: {body}"
    );
}

#[test]
fn wall_clock_backward_secs_none_when_now_equals_previous() {
    assert_eq!(wall_clock_backward_secs(t(0), t(0)), None);
}

#[test]
fn wall_clock_backward_secs_none_when_now_after_previous() {
    assert_eq!(wall_clock_backward_secs(t(0), t(1)), None);
}

#[test]
fn wall_clock_backward_secs_some_when_now_before_previous() {
    assert_eq!(wall_clock_backward_secs(t(100), t(40)), Some(60));
}

#[test]
fn effectively_market_closed_true_when_recently_recorded() {
    let now = t(1000);
    let closed_at = now - chrono::Duration::seconds(10);
    assert!(is_effectively_market_closed(
        true,
        Some(closed_at),
        now,
        STALE_AFTER
    ));
}

#[test]
fn effectively_market_closed_false_when_record_too_old() {
    let now = t(1000);
    let closed_at = now - chrono::Duration::seconds(STALE_AFTER as i64);
    assert!(!is_effectively_market_closed(
        true,
        Some(closed_at),
        now,
        STALE_AFTER
    ));
}

#[test]
fn effectively_market_closed_false_when_status_is_not_closed() {
    let now = t(1000);
    assert!(!is_effectively_market_closed(
        false,
        Some(now),
        now,
        STALE_AFTER
    ));
}

#[test]
fn effectively_market_closed_false_when_no_record() {
    let now = t(1000);
    assert!(!is_effectively_market_closed(true, None, now, STALE_AFTER));
}

#[test]
fn suspend_grace_tracker_in_grace_immediately_after_resume() {
    let mut tracker = SuspendGraceTracker::new();
    let resumed_at = t(0);
    tracker.note_suspend_resumed(resumed_at, STALE_AFTER);
    assert!(tracker.in_grace_period(resumed_at));
}

#[test]
fn suspend_grace_tracker_grace_ends_at_stale_after_secs() {
    let mut tracker = SuspendGraceTracker::new();
    let resumed_at = t(0);
    tracker.note_suspend_resumed(resumed_at, STALE_AFTER);
    let just_before = resumed_at + chrono::Duration::seconds(STALE_AFTER as i64 - 1);
    assert!(tracker.in_grace_period(just_before));
    let at_boundary = resumed_at + chrono::Duration::seconds(STALE_AFTER as i64);
    assert!(!tracker.in_grace_period(at_boundary));
}

#[test]
fn suspend_grace_tracker_with_no_suspend_never_in_grace() {
    let tracker = SuspendGraceTracker::new();
    assert!(!tracker.in_grace_period(t(0)));
}

#[test]
fn suspend_grace_tracker_latest_call_overrides_previous_grace() {
    let mut tracker = SuspendGraceTracker::new();
    tracker.note_suspend_resumed(t(0), STALE_AFTER);
    let second_resume = t(10);
    tracker.note_suspend_resumed(second_resume, STALE_AFTER);
    let after_first_windows_original_end = t(0) + chrono::Duration::seconds(STALE_AFTER as i64);
    assert!(
        tracker.in_grace_period(after_first_windows_original_end),
        "second note_suspend_resumed should extend grace beyond the first window"
    );
}

const STALE_AFTER: u64 = 600;
const REALERT: u64 = 3600;

fn obs(key: FeedId, market_closed: bool, last_tick_at: Option<DateTime<Utc>>) -> FeedObservation {
    FeedObservation {
        key,
        market_closed,
        last_tick_at,
    }
}

#[test]
fn fresh_feed_produces_no_event() {
    let mut tracker = FeedFreshnessTracker::new();
    let now = t(1000);
    let o = obs(
        feed("gmo_fx", "USD_JPY"),
        false,
        Some(now - chrono::Duration::seconds(10)),
    );
    let ev = tracker.evaluate(&o, now, t(0), STALE_AFTER, REALERT, false);
    assert_eq!(ev, None);
}

#[test]
fn startup_grace_period_suppresses_missing_tick_alert() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let now = started + chrono::Duration::seconds(STALE_AFTER as i64 - 1);
    let o = obs(feed("gmo_fx", "USD_JPY"), false, None);
    let ev = tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false);
    assert_eq!(ev, None);
}

#[test]
fn no_tick_since_startup_becomes_stale_at_threshold() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let now = started + chrono::Duration::seconds(STALE_AFTER as i64);
    let o = obs(feed("gmo_fx", "USD_JPY"), false, None);
    let ev = tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false);
    assert_eq!(ev, Some(FreshnessEvent::NewlyStale { since: now }));
}

#[test]
fn fresh_to_stale_transition_fires_once() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let last_tick = t(0);
    let now_stale = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
    let o = obs(feed("bitflyer_cfd", "FX_BTC_JPY"), false, Some(last_tick));

    let first = tracker.evaluate(&o, now_stale, started, STALE_AFTER, REALERT, false);
    assert_eq!(first, Some(FreshnessEvent::NewlyStale { since: now_stale }));

    let soon_after = now_stale + chrono::Duration::seconds(60);
    let second = tracker.evaluate(&o, soon_after, started, STALE_AFTER, REALERT, false);
    assert_eq!(
        second, None,
        "must not re-fire before realert_interval_secs elapses"
    );
}

#[test]
fn realert_fires_after_interval_elapses_while_still_stale() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let last_tick = t(0);
    let o = obs(feed("bitflyer_cfd", "FX_BTC_JPY"), false, Some(last_tick));

    let first_detect = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
    tracker
        .evaluate(&o, first_detect, started, STALE_AFTER, REALERT, false)
        .expect("first detection must fire");

    let before_realert = first_detect + chrono::Duration::seconds(REALERT as i64 - 1);
    assert_eq!(
        tracker.evaluate(&o, before_realert, started, STALE_AFTER, REALERT, false),
        None
    );

    let at_realert = first_detect + chrono::Duration::seconds(REALERT as i64);
    assert_eq!(
        tracker.evaluate(&o, at_realert, started, STALE_AFTER, REALERT, false),
        Some(FreshnessEvent::StillStale {
            since: first_detect
        })
    );
}

#[test]
fn recovery_fires_once_and_resets_state() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let last_tick = t(0);
    let key = feed("bitflyer_cfd", "FX_BTC_JPY");

    let stale_obs = obs(key.clone(), false, Some(last_tick));
    let detect_at = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
    tracker
        .evaluate(&stale_obs, detect_at, started, STALE_AFTER, REALERT, false)
        .expect("must detect stale first");

    let recovered_at = detect_at + chrono::Duration::seconds(30);
    let fresh_obs = obs(key.clone(), false, Some(recovered_at));
    let ev = tracker.evaluate(
        &fresh_obs,
        recovered_at,
        started,
        STALE_AFTER,
        REALERT,
        false,
    );
    assert_eq!(
        ev,
        Some(FreshnessEvent::Recovered {
            since: detect_at,
            recovered_at
        })
    );

    let after = recovered_at + chrono::Duration::seconds(1);
    let still_fresh_obs = obs(key, false, Some(recovered_at));
    assert_eq!(
        tracker.evaluate(
            &still_fresh_obs,
            after,
            started,
            STALE_AFTER,
            REALERT,
            false
        ),
        None
    );
}

#[test]
fn market_closed_feed_is_never_flagged_stale() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let now = started + chrono::Duration::seconds(STALE_AFTER as i64 * 10);
    let o = obs(feed("gmo_fx", "USD_JPY"), true, None);
    assert_eq!(
        tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false),
        None
    );
}

#[test]
fn market_closed_clears_prior_stale_state_without_recovered_notification() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let key = feed("gmo_fx", "USD_JPY");
    let last_tick = t(0);
    let stale_obs = obs(key.clone(), false, Some(last_tick));
    let detect_at = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
    tracker
        .evaluate(&stale_obs, detect_at, started, STALE_AFTER, REALERT, false)
        .expect("must detect stale first");

    let closed_at = detect_at + chrono::Duration::seconds(10);
    let closed_obs = obs(key, true, Some(last_tick));
    assert_eq!(
        tracker.evaluate(&closed_obs, closed_at, started, STALE_AFTER, REALERT, false),
        None,
        "market_closed transition itself must not fire a 'recovered' notification"
    );
}

#[test]
fn suspend_round_suppresses_new_stale_notification_but_resumes_next_round() {
    let mut tracker = FeedFreshnessTracker::new();
    let started = t(0);
    let key = feed("gmo_fx", "USD_JPY");
    let last_tick = t(0);
    let o = obs(key, false, Some(last_tick));

    let round1 = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
    let ev1 = tracker.evaluate(&o, round1, started, STALE_AFTER, REALERT, true);
    assert_eq!(
        ev1, None,
        "suspend round must suppress the stale notification"
    );

    let round2 = round1 + chrono::Duration::seconds(60);
    let ev2 = tracker.evaluate(&o, round2, started, STALE_AFTER, REALERT, false);
    assert_eq!(ev2, Some(FreshnessEvent::NewlyStale { since: round2 }));
}

#[test]
fn stale_alert_with_tick_shows_last_tick_and_detection_time() {
    let body = format_stale_alert(
        Exchange::GmoFx,
        &Pair::new("USD_JPY"),
        t(600),
        Some(t(0)),
        t(-3600),
    );
    assert!(body.contains("gmo_fx"), "body={body}");
    assert!(
        body.contains("last tick at 2023-11-15 07:13:20 JST"),
        "body={body}"
    );
    assert!(
        body.contains("detected stale at 2023-11-15 07:23:20 JST"),
        "body={body}"
    );
    assert!(!body.contains("no tick received"), "body={body}");
}

#[test]
fn stale_alert_without_tick_shows_watchdog_start() {
    let body = format_stale_alert(Exchange::GmoFx, &Pair::new("USD_JPY"), t(600), None, t(0));
    assert!(
        body.contains("no tick received since watchdog start (2023-11-15 07:13:20 JST)"),
        "body={body}"
    );
    assert!(
        body.contains("detected stale at 2023-11-15 07:23:20 JST"),
        "body={body}"
    );
    assert!(!body.contains("last tick at"), "body={body}");
}

#[test]
fn recovered_alert_states_times_relative_to_detection() {
    let body = format_recovered_alert(Exchange::GmoFx, &Pair::new("USD_JPY"), t(0), t(185));
    assert!(
        body.contains("recovered at 2023-11-15 07:16:25 JST"),
        "body={body}"
    );
    assert!(
        body.contains("stale detected at 2023-11-15 07:13:20 JST"),
        "body={body}"
    );
    assert!(body.contains("~3 minute(s) after detection"), "body={body}");
}

#[tokio::test]
async fn classify_reports_normal_exit() {
    let handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async { Ok(()) });
    let result = handle.await;
    let (title, body) =
        classify_feed_task_result(Exchange::BitflyerCfd, result).expect("normal exit must notify");
    assert_eq!(title, "feed task exited");
    assert!(body.contains("bitflyer_cfd"), "body={body}");
    assert!(body.contains("normally"), "body={body}");
}

#[tokio::test]
async fn classify_reports_error_exit() {
    let handle: tokio::task::JoinHandle<anyhow::Result<()>> =
        tokio::spawn(async { Err(anyhow::anyhow!("ws disconnected")) });
    let result = handle.await;
    let (title, body) =
        classify_feed_task_result(Exchange::Oanda, result).expect("error exit must notify");
    assert_eq!(title, "feed task exited");
    assert!(body.contains("oanda"), "body={body}");
    assert!(body.contains("ws disconnected"), "body={body}");
}

#[tokio::test]
async fn classify_reports_panic_with_message() {
    let handle: tokio::task::JoinHandle<anyhow::Result<()>> =
        tokio::spawn(async { panic!("boom") });
    let result = handle.await;
    let (title, body) =
        classify_feed_task_result(Exchange::GmoFx, result).expect("panic must notify");
    assert_eq!(title, "feed task exited");
    assert!(body.contains("gmo_fx"), "body={body}");
    assert!(body.contains("boom"), "body={body}");
}

#[tokio::test]
async fn classify_returns_none_for_shutdown_abort() {
    let handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok(())
    });
    handle.abort();
    let result = handle.await;
    assert!(result.as_ref().is_err_and(|e| e.is_cancelled()));
    assert_eq!(classify_feed_task_result(Exchange::GmoFx, result), None);
}

#[test]
fn panic_message_extracts_str_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new("plain str panic");
    assert_eq!(panic_message(payload), "plain str panic");
}

#[test]
fn panic_message_extracts_string_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("owned string panic"));
    assert_eq!(panic_message(payload), "owned string panic");
}

#[test]
fn panic_message_falls_back_for_non_string_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
    assert_eq!(panic_message(payload), "panic payload was not a string");
}
