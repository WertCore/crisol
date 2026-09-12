//! Composition, which is what §M5 means by "IME composition works for Japanese input".
//!
//! The platform layer is what actually talks to an input method, and no CI runner has one
//! attached. What is testable — and what every application that gets IME wrong gets wrong —
//! is the state machine: provisional text stays out of the document, a cancel is not an empty
//! commit, and moving focus abandons what was being typed.

use crisol_events::{ImeEvent, ImeState, Preedit};

fn preedit(text: &str) -> ImeEvent {
    ImeEvent::Preedit(Preedit {
        text: text.to_owned(),
        selection: None,
    })
}

#[test]
fn nothing_is_composing_to_begin_with() {
    let state = ImeState::new();
    assert!(!state.is_composing());
    assert!(state.preedit().is_empty());
}

/// Typing にほん in a Japanese IME: several keystrokes produce provisional text, which is
/// replaced wholesale each time and is not in the document until it commits.
#[test]
fn provisional_text_stays_out_of_the_document_until_it_commits() {
    let mut state = ImeState::new();

    assert_eq!(state.apply(&ImeEvent::Start), None);
    assert!(state.is_composing());

    for stage in ["に", "にほ", "にほん"] {
        assert_eq!(
            state.apply(&preedit(stage)),
            None,
            "nothing is inserted while the user is still deciding"
        );
        assert_eq!(state.preedit().text, stage, "but it is shown");
    }

    assert_eq!(
        state.apply(&ImeEvent::Commit("日本".to_owned())),
        Some("日本".to_owned()),
        "the converted text is what lands in the document, not the keystrokes"
    );
    assert!(!state.is_composing());
    assert!(state.preedit().is_empty(), "and the preedit is gone");
}

/// A cancel and an empty commit mean opposite things to undo: an abandoned composition never
/// happened, while an empty commit is a deletion.
#[test]
fn cancelling_is_not_an_empty_commit() {
    let mut state = ImeState::new();
    state.apply(&ImeEvent::Start);
    state.apply(&preedit("にほん"));

    assert_eq!(state.apply(&ImeEvent::Cancel), None);
    assert!(!state.is_composing());
    assert!(state.preedit().is_empty());

    let mut other = ImeState::new();
    other.apply(&ImeEvent::Start);
    other.apply(&preedit("にほん"));
    assert_eq!(
        other.apply(&ImeEvent::Commit(String::new())),
        Some(String::new()),
        "an empty commit is still a commit, and the caller may need to act on it"
    );
}

#[test]
fn a_preedit_replaces_rather_than_appends() {
    // An input method sends the whole provisional string each time, not a delta. Appending
    // is how "にほん" becomes "にには ほにほん".
    let mut state = ImeState::new();
    state.apply(&preedit("abc"));
    state.apply(&preedit("x"));
    assert_eq!(state.preedit().text, "x");
}

#[test]
fn the_preedit_carries_the_clause_the_user_is_converting() {
    // Japanese and Chinese input methods underline one clause differently from the rest,
    // and the engine cannot render that without knowing which.
    let mut state = ImeState::new();
    state.apply(&ImeEvent::Preedit(Preedit {
        text: "にほんごのにゅうりょく".to_owned(),
        selection: Some(0..15),
    }));
    assert_eq!(state.preedit().selection, Some(0..15));
}

#[test]
fn a_preedit_without_a_start_still_begins_composition() {
    // Not every platform sends a start event; some go straight to the first preedit.
    let mut state = ImeState::new();
    state.apply(&preedit("に"));
    assert!(state.is_composing());
}

/// Moving focus has to abandon the composition: the provisional text belongs to the node
/// that was focused, and carrying it across would paste half a word into the next one.
#[test]
fn resetting_abandons_what_was_being_typed() {
    let mut state = ImeState::new();
    state.apply(&ImeEvent::Start);
    state.apply(&preedit("にほん"));

    state.reset();

    assert!(!state.is_composing());
    assert!(state.preedit().is_empty());
}

#[test]
fn composition_can_start_again_after_it_ends() {
    let mut state = ImeState::new();
    state.apply(&ImeEvent::Start);
    state.apply(&preedit("a"));
    state.apply(&ImeEvent::Commit("A".to_owned()));

    state.apply(&ImeEvent::Start);
    state.apply(&preedit("b"));
    assert!(state.is_composing());
    assert_eq!(state.preedit().text, "b");
}
