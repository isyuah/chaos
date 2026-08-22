//! Action identifiers shared between Core and the actions crate.
//!
//! An [`ActionId`] is a stable, toolkit-independent handle that the session uses
//! to request an action without depending on the action's implementation.

/// Opaque identifier for a user-facing capture action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(pub &'static str);

impl ActionId {
    pub const COPY: ActionId = ActionId("copy");
    pub const SAVE: ActionId = ActionId("save");
    pub const PIN: ActionId = ActionId("pin");
    pub const ASK_AI: ActionId = ActionId("ask-ai");
    pub const CANCEL: ActionId = ActionId("cancel");

    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
