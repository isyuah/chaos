//! UI-neutral application orchestration for the screenshot application.
//!
//! `capture-core` and `capture-annotation` define the capture domain. This
//! crate owns the next boundary up: runtime policy, session lifecycle, and an
//! internal command/event model. These Rust enums are allowed to evolve and
//! are not an IPC wire protocol or plugin ABI.

use capture_annotation::{CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState};
use capture_core::action::ActionId;
use capture_core::capture::Timing;
use std::collections::HashMap;

/// What the application should do after a successful copy action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDisposition {
    KeepEditorOpen,
    CloseOverlay,
}

/// Behavior that the runtime itself applies.
///
/// Host-owned configuration such as hotkeys, save paths, capture sources, and
/// themes deliberately stays outside this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicy {
    pub copy_disposition: CopyDisposition,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            copy_disposition: CopyDisposition::KeepEditorOpen,
        }
    }
}

/// Correlates one host-side action execution with the runtime request that
/// created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionRequestId(u64);

impl ActionRequestId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Result of a host-side action such as clipboard copy or file save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionCompletion {
    Succeeded { message: Option<String> },
    Failed { message: Option<String> },
}

impl ActionCompletion {
    pub fn succeeded(message: impl Into<String>) -> Self {
        Self::Succeeded {
            message: Some(message.into()),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: Some(message.into()),
        }
    }
}

/// Internal application commands accepted by the runtime.
#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    BeginCapture,
    Capture(CaptureCommand),
    SetPolicy(RuntimePolicy),
    CompleteAction {
        request_id: ActionRequestId,
        completion: ActionCompletion,
    },
}

/// Internal application events emitted by the runtime.
#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Session(CaptureEvent),
    CaptureStarted,
    PolicyChanged(RuntimePolicy),
    ActionRequested {
        request_id: ActionRequestId,
        action: ActionId,
    },
    StatusChanged(String),
    CloseOverlay,
    Rejected(RuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    UnknownActionRequest(ActionRequestId),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownActionRequest(request_id) => {
                write!(formatter, "unknown action request: {}", request_id.get())
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Application-level owner of one capture session and its pending actions.
#[derive(Debug, Clone)]
pub struct CaptureRuntime {
    session: CaptureSession,
    policy: RuntimePolicy,
    next_action_request_id: u64,
    pending_actions: HashMap<ActionRequestId, ActionId>,
}

impl Default for CaptureRuntime {
    fn default() -> Self {
        Self::new(RuntimePolicy::default())
    }
}

impl CaptureRuntime {
    pub fn new(policy: RuntimePolicy) -> Self {
        Self {
            session: CaptureSession::new(),
            policy,
            next_action_request_id: 1,
            pending_actions: HashMap::new(),
        }
    }

    pub fn policy(&self) -> &RuntimePolicy {
        &self.policy
    }

    pub fn state(&self) -> &CaptureSessionState {
        self.session.state()
    }

    pub fn timing(&self) -> &Timing {
        self.session.timing()
    }

    pub fn hover_candidate(&self) -> Option<&capture_core::SnapCandidate> {
        self.session.hover_candidate()
    }

    pub fn document(&self) -> Option<capture_annotation::CaptureDocument> {
        match self.session.state() {
            CaptureSessionState::Editing(editor) => Some(editor.document.clone()),
            _ => None,
        }
    }

    pub fn dispatch(&mut self, command: RuntimeCommand) -> Vec<RuntimeEvent> {
        match command {
            RuntimeCommand::BeginCapture => self.begin_capture(),
            RuntimeCommand::Capture(command) => self.dispatch_capture(command),
            RuntimeCommand::SetPolicy(policy) => {
                self.policy = policy.clone();
                vec![RuntimeEvent::PolicyChanged(policy)]
            }
            RuntimeCommand::CompleteAction {
                request_id,
                completion,
            } => self.complete_action(request_id, completion),
        }
    }

    fn begin_capture(&mut self) -> Vec<RuntimeEvent> {
        self.pending_actions.clear();
        self.session = CaptureSession::new();
        let mut events = vec![RuntimeEvent::CaptureStarted];
        events.extend(self.dispatch_capture(CaptureCommand::Begin));
        events
    }

    fn dispatch_capture(&mut self, command: CaptureCommand) -> Vec<RuntimeEvent> {
        let session_events = self.session.apply(command);
        let completed = session_events
            .iter()
            .any(|event| matches!(event, CaptureEvent::Completed));
        let mut events = Vec::with_capacity(session_events.len());
        for event in session_events {
            match event {
                CaptureEvent::ActionRequested(action) => {
                    let request_id = self.allocate_action_request_id();
                    self.pending_actions.insert(request_id, action);
                    events.push(RuntimeEvent::ActionRequested { request_id, action });
                }
                event => events.push(RuntimeEvent::Session(event)),
            }
        }
        if completed {
            self.pending_actions.clear();
        }
        events
    }

    fn allocate_action_request_id(&mut self) -> ActionRequestId {
        loop {
            let request_id = ActionRequestId(self.next_action_request_id);
            self.next_action_request_id = self.next_action_request_id.checked_add(1).unwrap_or(1);
            if !self.pending_actions.contains_key(&request_id) {
                return request_id;
            }
        }
    }

    fn complete_action(
        &mut self,
        request_id: ActionRequestId,
        completion: ActionCompletion,
    ) -> Vec<RuntimeEvent> {
        let Some(action) = self.pending_actions.remove(&request_id) else {
            return vec![RuntimeEvent::Rejected(RuntimeError::UnknownActionRequest(
                request_id,
            ))];
        };
        let (success, message) = match completion {
            ActionCompletion::Succeeded { message } => (true, message),
            ActionCompletion::Failed { message } => (false, message),
        };
        let fallback = if success {
            format!("{action} completed")
        } else {
            format!("{action} failed")
        };
        let mut events = vec![RuntimeEvent::StatusChanged(message.unwrap_or(fallback))];
        if success
            && action == ActionId::COPY
            && self.policy.copy_disposition == CopyDisposition::CloseOverlay
        {
            events.push(RuntimeEvent::CloseOverlay);
        }
        events
    }
}

/// Declarative metadata reserved for a future plugin host.
///
/// No execution trait is exposed yet. The first real plugin will determine the
/// required context, result, permission, and isolation contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub actions: Vec<String>,
}

/// Owned identifier for an action contributed at runtime by a future plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginActionId {
    pub plugin_id: String,
    pub action_id: String,
}

impl PluginActionId {
    pub fn new(plugin_id: impl Into<String>, action_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            action_id: action_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_core::capture::{CapturedFrame, PixelFormat};
    use capture_core::geometry::PhysicalPoint;

    fn frame() -> CapturedFrame {
        CapturedFrame::new(
            vec![0x80; 4 * 4 * 4].into(),
            4,
            4,
            16,
            PhysicalPoint::ZERO,
            PixelFormat::Rgba8,
        )
    }

    fn editing_runtime(policy: RuntimePolicy) -> CaptureRuntime {
        let mut runtime = CaptureRuntime::new(policy);
        runtime.dispatch(RuntimeCommand::BeginCapture);
        runtime.dispatch(RuntimeCommand::Capture(CaptureCommand::FrameReady(frame())));
        runtime.dispatch(RuntimeCommand::Capture(CaptureCommand::CommitSelection));
        runtime
    }

    fn request_action(runtime: &mut CaptureRuntime, action: ActionId) -> ActionRequestId {
        runtime
            .dispatch(RuntimeCommand::Capture(CaptureCommand::InvokeAction(
                action,
            )))
            .into_iter()
            .find_map(|event| match event {
                RuntimeEvent::ActionRequested { request_id, .. } => Some(request_id),
                _ => None,
            })
            .expect("action request")
    }

    #[test]
    fn default_runtime_starts_a_capture_session() {
        let mut runtime = CaptureRuntime::default();
        let events = runtime.dispatch(RuntimeCommand::BeginCapture);

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::CaptureStarted)));
        assert!(matches!(runtime.state(), CaptureSessionState::Preparing));
    }

    #[test]
    fn a_new_capture_replaces_the_previous_session() {
        let mut runtime = CaptureRuntime::default();
        runtime.dispatch(RuntimeCommand::BeginCapture);
        let events = runtime.dispatch(RuntimeCommand::BeginCapture);

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::CaptureStarted)));
        assert!(!events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Session(CaptureEvent::Error(_)))));
        assert!(matches!(runtime.state(), CaptureSessionState::Preparing));
    }

    #[test]
    fn successful_copy_closes_only_for_a_matching_request() {
        let mut runtime = editing_runtime(RuntimePolicy {
            copy_disposition: CopyDisposition::CloseOverlay,
        });
        let request_id = request_action(&mut runtime, ActionId::COPY);
        let events = runtime.dispatch(RuntimeCommand::CompleteAction {
            request_id,
            completion: ActionCompletion::Succeeded { message: None },
        });

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::CloseOverlay)));
    }

    #[test]
    fn unknown_or_already_completed_action_is_rejected() {
        let mut runtime = editing_runtime(RuntimePolicy::default());
        let request_id = request_action(&mut runtime, ActionId::COPY);
        runtime.dispatch(RuntimeCommand::CompleteAction {
            request_id,
            completion: ActionCompletion::Succeeded { message: None },
        });
        let events = runtime.dispatch(RuntimeCommand::CompleteAction {
            request_id,
            completion: ActionCompletion::Succeeded { message: None },
        });

        assert!(matches!(
            events.as_slice(),
            [RuntimeEvent::Rejected(RuntimeError::UnknownActionRequest(id))] if *id == request_id
        ));
    }

    #[test]
    fn starting_a_new_capture_invalidates_pending_actions() {
        let mut runtime = editing_runtime(RuntimePolicy::default());
        let request_id = request_action(&mut runtime, ActionId::SAVE);
        runtime.dispatch(RuntimeCommand::BeginCapture);
        let events = runtime.dispatch(RuntimeCommand::CompleteAction {
            request_id,
            completion: ActionCompletion::Succeeded { message: None },
        });

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Rejected(_))));
    }
}
