use super::*;
use crate::plugin::tests::{registry_of, temp};

#[test]
fn commands_are_read_one_by_one_and_unreadable_ones_are_reported() {
    let reply = json!({"commands": [
        {"cmd": "toast", "text": "hi"},
        {"cmd": "load", "deck": 2, "file": "/m/a.mp3"},
        {"cmd": "import", "files": ["/m/a.mp3", 5, "/m/b.mp3"]},
        {"cmd": "key", "action": "deck.play_pause.A"},
        {"cmd": "dance"},
        {"cmd": "load", "deck": 0, "file": "/m/a.mp3"},
        {"cmd": "toast"},
        {"text": "no name"},
        "not an object"
    ]});
    let got = commands("p", &reply).unwrap();
    assert_eq!(got.len(), 9);
    assert_eq!(got[0], Ok(Command::Toast("hi".into())));
    assert_eq!(
        got[1],
        Ok(Command::Load {
            deck: 2,
            file: "/m/a.mp3".into()
        })
    );
    // A non-text entry of `files` is skipped, the rest is kept.
    assert_eq!(
        got[2],
        Ok(Command::Import(vec!["/m/a.mp3".into(), "/m/b.mp3".into()]))
    );
    assert_eq!(got[3], Ok(Command::Key("deck.play_pause.A".into())));
    assert_eq!(got[4], Err("p: unknown command \"dance\"".into()));
    assert!(got[5].as_ref().unwrap_err().contains("number from 1"));
    assert!(
        got[6]
            .as_ref()
            .unwrap_err()
            .contains("`toast` needs a text `text`")
    );
    assert!(got[7].as_ref().unwrap_err().contains("without `cmd`"));
    assert!(got[8].is_err());
}

#[test]
fn a_reply_without_commands_is_fine_and_a_malformed_list_is_an_error() {
    assert!(commands("p", &json!({})).unwrap().is_empty());
    assert!(commands("p", &json!({"commands": {}})).is_err());
}

#[test]
fn run_sends_the_action_input_and_context_and_returns_the_commands() {
    let script = r#"read line
case "$line" in *'"action":"go"'*) case "$line" in *'"context":{"decks":[]}'*) case "$line" in *'"input":"hello"'*)
printf '{"commands":[{"cmd":"toast","text":"got it"},{"cmd":"bogus"}]}'; exit 0 ;;
esac ;; esac ;; esac
printf '{"error":"bad request"}'"#;
    let reg = registry_of(
        &temp("action"),
        "p",
        json!({"actions": [{"id": "go", "label": "Go", "input": "Text"}]}),
        script,
    );
    let got = run(&reg.plugins[0], "go", Some("hello"), json!({"decks": []})).unwrap();
    assert_eq!(got[0], Ok(Command::Toast("got it".into())));
    assert!(got[1].is_err());
    // An error reply fails the whole action.
    let e = run(&reg.plugins[0], "go", None, json!({})).unwrap_err();
    assert!(e.starts_with("p: bad request"), "{e}");
}
