use super::*;

#[test]
fn clamp_top_k_to_non_negative_floors_negative_input_at_zero() {
    assert_eq!(clamp_top_k_to_non_negative(-5), 0);
}

#[test]
fn clamp_top_k_to_non_negative_keeps_non_negative_input_unchanged() {
    assert_eq!(clamp_top_k_to_non_negative(0), 0);
    assert_eq!(clamp_top_k_to_non_negative(7), 7);
}

#[test]
fn search_hit_with_non_empty_text_keeps_item_with_text() {
    let item = SearchResultItem {
        text: Some("hello".to_string()),
        score: Some(0.5),
        ..Default::default()
    };
    let hit = search_hit_with_non_empty_text(item).expect("hit with text is kept");
    assert_eq!(hit.text, "hello");
    assert_eq!(hit.score, 0.5);
}

#[test]
fn search_hit_with_non_empty_text_drops_missing_text() {
    let item = SearchResultItem {
        text: None,
        ..Default::default()
    };
    assert!(search_hit_with_non_empty_text(item).is_none());
}

#[test]
fn search_hit_with_non_empty_text_drops_empty_text() {
    let item = SearchResultItem {
        text: Some(String::new()),
        ..Default::default()
    };
    assert!(search_hit_with_non_empty_text(item).is_none());
}

#[test]
fn search_hit_with_non_empty_text_defaults_missing_score_to_zero() {
    let item = SearchResultItem {
        text: Some("hello".to_string()),
        score: None,
        ..Default::default()
    };
    let hit = search_hit_with_non_empty_text(item).expect("hit with text is kept");
    assert_eq!(hit.score, 0.0);
}
