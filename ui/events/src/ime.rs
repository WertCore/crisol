//! Text input, including composition.
//!
//! ROADMAP §M5's acceptance names IME composition for Japanese on all three platforms. The
//! reason it is called out is that an input method is not a keyboard with extra steps: the
//! user types several keys, the system shows *provisional* text that is not in the document
//! yet, and only later does that text commit — or get abandoned entirely.
//!
//! An engine that treats composition as ordinary key input gets three things wrong at once:
//! the provisional text ends up in the document, undo has an entry per keystroke instead of
//! per word, and cancelling leaves the abandoned text behind.

use std::ops::Range;

/// Text the input method is still deciding about.
///
/// Not in the document. It is rendered in place — usually underlined — and replaced wholesale
/// on every update until it commits or is abandoned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    /// The provisional text.
    pub text: String,
    /// The selection within `text`, as byte offsets, if the input method has one.
    ///
    /// Japanese and Chinese input methods use this to show which clause is being converted.
    pub selection: Option<Range<usize>>,
}

impl Preedit {
    /// Whether there is nothing being composed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// What the platform's input method just did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// Composition started. The engine should show a preedit and stop treating keystrokes as
    /// ordinary text.
    Start,
    /// The provisional text changed. Replaces whatever was there.
    Preedit(Preedit),
    /// The text committed. It becomes real, and composition ends.
    Commit(String),
    /// Composition ended without committing.
    ///
    /// A distinct event from an empty commit, because the two mean opposite things to undo:
    /// an abandoned composition never happened, while an empty commit is a deletion.
    Cancel,
}

/// Where an input method should put its candidate window.
///
/// The platform needs this in the window's coordinate space, and the engine is the only
/// thing that knows where the caret is. Getting it wrong puts the candidate list on the
/// other side of the screen from the text being typed, which is the most common IME bug in
/// applications that implement one at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImeCursorArea {
    /// The caret's box, in logical pixels.
    pub caret: crisol_display_list::Rect,
}

/// Tracks composition for the focused node.
///
/// Holds the provisional text so the rest of the engine does not have to: layout and paint
/// ask for the text to display, and neither needs to know whether it is committed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImeState {
    preedit: Preedit,
    composing: bool,
}

impl ImeState {
    /// Nothing being composed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an input method is mid-composition.
    ///
    /// While this is true, ordinary key handling must not run: the keystrokes belong to the
    /// input method, and treating them as text as well is how a character gets typed twice.
    #[must_use]
    pub fn is_composing(&self) -> bool {
        self.composing
    }

    /// The provisional text, which is rendered but not in the document.
    #[must_use]
    pub fn preedit(&self) -> &Preedit {
        &self.preedit
    }

    /// Applies an input method event.
    ///
    /// Returns the text to insert into the document, which is `Some` only on a commit.
    /// Everything else changes what is displayed without changing what is stored.
    pub fn apply(&mut self, event: &ImeEvent) -> Option<String> {
        match event {
            ImeEvent::Start => {
                self.composing = true;
                self.preedit = Preedit::default();
                None
            }
            ImeEvent::Preedit(preedit) => {
                self.composing = true;
                self.preedit = preedit.clone();
                None
            }
            ImeEvent::Commit(text) => {
                self.composing = false;
                self.preedit = Preedit::default();
                Some(text.clone())
            }
            ImeEvent::Cancel => {
                self.composing = false;
                self.preedit = Preedit::default();
                None
            }
        }
    }

    /// Abandons any composition in progress.
    ///
    /// What focus moving away has to do: the provisional text belongs to the node that was
    /// focused, and carrying it to the next one would paste half a word into it.
    pub fn reset(&mut self) {
        self.composing = false;
        self.preedit = Preedit::default();
    }
}
