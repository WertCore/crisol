//! The `Error` hierarchy.
//!
//! Seven built-in kinds plus `AggregateError`, all of which inherit from `Error` — and the
//! inheritance is the part with content. `TypeError.prototype`'s prototype **is**
//! `Error.prototype`, which is why `new TypeError() instanceof Error` is true and why catching
//! `Error` catches all of them. An implementation that gave each kind an independent prototype
//! would pass every test that constructs one and fail every `catch (e) { if (e instanceof
//! Error) }` in the wild.
//!
//! # `name` lives on the prototype, not the instance
//!
//! `new TypeError("x").name` is `"TypeError"` because it is found on
//! `TypeError.prototype.name`, not because the constructor wrote it. That is observable:
//! `Object.keys(new TypeError("x"))` does not contain `"name"`, and assigning
//! `err.name = "Mine"` shadows it rather than changing it. `message` is the opposite — an own
//! property, and only when it is non-empty.

use crisol_value::Value;

/// Which built-in error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `Error`, the root.
    Error,
    /// `EvalError` — kept for compatibility; nothing in the language throws it any more.
    Eval,
    /// `RangeError`.
    Range,
    /// `ReferenceError`.
    Reference,
    /// `SyntaxError`.
    Syntax,
    /// `TypeError`.
    Type,
    /// `URIError`.
    Uri,
    /// `AggregateError`, which carries several others.
    Aggregate,
}

impl ErrorKind {
    /// Every kind.
    pub const ALL: [Self; 8] = [
        Self::Error,
        Self::Eval,
        Self::Range,
        Self::Reference,
        Self::Syntax,
        Self::Type,
        Self::Uri,
        Self::Aggregate,
    ];

    /// The `name` on this kind's prototype.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Eval => "EvalError",
            Self::Range => "RangeError",
            Self::Reference => "ReferenceError",
            Self::Syntax => "SyntaxError",
            Self::Type => "TypeError",
            Self::Uri => "URIError",
            Self::Aggregate => "AggregateError",
        }
    }

    /// What this kind's prototype inherits from.
    ///
    /// `None` only for `Error` itself. Everything else chains to it, which is what makes
    /// `instanceof Error` catch all of them.
    #[must_use]
    pub const fn parent(self) -> Option<Self> {
        match self {
            Self::Error => None,
            _ => Some(Self::Error),
        }
    }

    /// Whether a `catch` on `expected` would catch this kind.
    ///
    /// Walks the prototype chain, so `Type.is_a(Error)` is true and `Error.is_a(Type)` is not.
    #[must_use]
    pub fn is_a(self, expected: Self) -> bool {
        let mut current = Some(self);
        while let Some(kind) = current {
            if kind == expected {
                return true;
            }
            current = kind.parent();
        }
        false
    }
}

/// An error object.
#[derive(Clone, Debug, PartialEq)]
pub struct JsError {
    /// Which built-in it is.
    pub kind: ErrorKind,
    /// `error.message`. Empty means the property is absent, not present-and-empty.
    pub message: String,
    /// `error.cause`, from `new Error(msg, { cause })`. Absent unless the option was given —
    /// and `{ cause: undefined }` **does** create it, which is why this is an `Option<Value>`
    /// rather than a `Value` defaulting to `undefined`.
    pub cause: Option<Value>,
    /// `AggregateError`'s `errors`. Empty for every other kind.
    pub errors: Vec<Value>,
}

impl JsError {
    /// `new Error(message)` and friends.
    #[must_use]
    pub fn new(kind: ErrorKind, message: &str) -> Self {
        Self {
            kind,
            message: message.to_owned(),
            cause: None,
            errors: Vec::new(),
        }
    }

    /// `new AggregateError(errors, message)`.
    #[must_use]
    pub fn aggregate(errors: Vec<Value>, message: &str) -> Self {
        Self {
            kind: ErrorKind::Aggregate,
            message: message.to_owned(),
            cause: None,
            errors,
        }
    }

    /// `new Error(message, { cause })`.
    #[must_use]
    pub fn with_cause(mut self, cause: Value) -> Self {
        self.cause = Some(cause);
        self
    }

    /// `error.name`, which comes from the prototype.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// Whether `message` is an own property.
    ///
    /// `new Error()` has no `message` of its own — it inherits `""` from the prototype — while
    /// `new Error("")` does not create one either, because the spec only defines it when the
    /// argument is not `undefined` *and* the resulting string is used. The distinction shows
    /// up in `Object.keys`.
    #[must_use]
    pub fn has_own_message(&self) -> bool {
        !self.message.is_empty()
    }

    /// `Error.prototype.toString`.
    ///
    /// `"TypeError: boom"`, or just `"TypeError"` when there is no message, or just the message
    /// when the name is empty. The separator appears only when both halves do — a detail that
    /// shows up in every stack trace.
    #[must_use]
    pub fn to_display(&self) -> String {
        let name = self.name();
        if self.message.is_empty() {
            return name.to_owned();
        }
        format!("{name}: {}", self.message)
    }

    /// Whether a `catch` filtering on `kind` would take this.
    #[must_use]
    pub fn is_a(&self, kind: ErrorKind) -> bool {
        self.kind.is_a(kind)
    }
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_display())
    }
}

impl std::error::Error for JsError {}
