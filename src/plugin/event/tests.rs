use super::*;
use crate::plugin::tests::{registry_of, temp};
use serde_json::json;

#[test]
fn only_plugins_that_list_an_event_get_it_and_they_get_it_with_its_data() {
    let dir = temp("events");
    let mut reg = registry_of(
        &dir,
        "listens",
        json!({"events": ["track_loaded"]}),
        "cat > got.json",
    );
    let deaf = registry_of(
        &dir,
        "deaf",
        json!({"events": ["track_played"]}),
        "cat > got.json",
    );
    reg.plugins.extend(deaf.plugins);
    let handles = fire_in(
        &reg,
        "track_loaded",
        json!({"deck": 1, "track": {"title": "T"}}),
    );
    assert_eq!(handles.len(), 1);
    handles.into_iter().for_each(|h| h.join().unwrap());
    let got: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("listens/got.json")).unwrap())
            .unwrap();
    assert_eq!(
        got,
        json!({"op": "event", "event": "track_loaded", "data": {"deck": 1, "track": {"title": "T"}}, "settings": {}, "lang": "en"})
    );
    assert!(!dir.join("deaf/got.json").exists());
    assert!(fire_in(&reg, "unheard", json!({})).is_empty());
}
