//! UI-neutral application orchestration for the screenshot application.
//!
//! `capture-core` and `capture-annotation` define the capture domain. This
//! crate owns the next boundary up: application settings, session lifecycle,
//! and a stable command/event vocabulary that can be driven by Slint, a CLI,
//! a future settings shell, or an IPC adapter. It deliberately does not own
//! windows, clipboard access, global-hotkey registration, or plugin loading.

use capture_annotation::{CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState};
use capture_core::action::ActionId;
use capture_core::capture::Timing;
use std::path::PathBuf;

/// What the application should do after a successful copy action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDisposition {
    /// Keep the editor open after copying.
    KeepEditorOpen,
    /// Ask the application shell to close the capture overlay.
    CloseOverlay,
}

/// User-configurable application behavior.
///
/// This is intentionally toolkit-neutral. A settings window, CLI, or future
/// plugin host can edit the same value without knowing anything about Slint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    /// A portable accelerator string, for example `Ctrl+Shift+4`.
    pub capture_hotkey: Option<String>,
    pub copy_disposition: CopyDisposition,
    pub save_directory: PathBuf,
    pub capture_virtual_desktop: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            capture_hotkey: None,
            copy_disposition: CopyDisposition::KeepEditorOpen,
            save_directory: PathBuf::from("."),
            capture_virtual_desktop: true,
        }
    }
}

/// Commands accepted by the application runtime.
///
/// `Capture` preserves the existing Core session command vocabulary. The
/// wrapper gives higher-level callers a place to add application behavior
/// without making the UI call the session directly.
#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    /// Start a new capture session and let the shell perform the capture.
    BeginCapture,
    /// Deliver a domain command or captured frame to the runtime.
    Capture(CaptureCommand),
    /// Replace the user-facing application settings.
    SetSettings(AppSettings),
    /// Tell the runtime that a shell-side action finished.
    ActionCompleted {
        action: ActionId,
        success: bool,
        message: Option<String>,
    },
    /// Request an action contributed by a registered plugin.
    InvokePluginAction(PluginActionId),
}

/// Runtime-level events. Domain events are retained instead of translated
/// into UI concepts so a second frontend can render the same session state.
#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Session(CaptureEvent),
    CaptureStarted,
    SettingsChanged(AppSettings),
    ActionRequested(ActionId),
    PluginActionRequested(PluginActionId),
    StatusChanged(String),
    CloseOverlay,
}

/// The application-level owner of one capture session and its policy.
#[derive(Debug, Clone)]
pub struct CaptureRuntime {
    session: CaptureSession,
    settings: AppSettings,
}

impl Default for CaptureRuntime {
    fn default() -> Self {
        Self::new(AppSettings::default())
    }
}

impl CaptureRuntime {
    pub fn new(settings: AppSettings) -> Self {
        Self {
            session: CaptureSession::new(),
            settings,
        }
    }

    pub fn settings(&self) -> &AppSettings {
        &self.settings
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

    /// Dispatch one application command and return all resulting events.
    pub fn dispatch(&mut self, command: RuntimeCommand) -> Vec<RuntimeEvent> {
        match command {
            RuntimeCommand::BeginCapture => self.begin_capture(),
            RuntimeCommand::Capture(command) => self.dispatch_capture(command),
            RuntimeCommand::SetSettings(settings) => {
                self.settings = settings.clone();
                vec![RuntimeEvent::SettingsChanged(settings)]
            }
            RuntimeCommand::ActionCompleted {
                action,
                success,
                message,
            } => self.action_completed(action, success, message),
            RuntimeCommand::InvokePluginAction(action) => {
                vec![RuntimeEvent::PluginActionRequested(action)]
            }
        }
    }

    fn begin_capture(&mut self) -> Vec<RuntimeEvent> {
        // A resident host may start a new capture while the previous overlay is
        // still active. Each request owns a fresh domain session.
        self.session = CaptureSession::new();
        let mut events = vec![RuntimeEvent::CaptureStarted];
        events.extend(self.dispatch_capture(CaptureCommand::Begin));
        events
    }

    fn dispatch_capture(&mut self, command: CaptureCommand) -> Vec<RuntimeEvent> {
        self.session
            .apply(command)
            .into_iter()
            .map(|event| match event {
                CaptureEvent::ActionRequested(action) => RuntimeEvent::ActionRequested(action),
                event => RuntimeEvent::Session(event),
            })
            .collect()
    }

    fn action_completed(
        &self,
        action: ActionId,
        success: bool,
        message: Option<String>,
    ) -> Vec<RuntimeEvent> {
        let fallback = if success {
            format!("{action} completed")
        } else {
            format!("{action} failed")
        };
        let mut events = vec![RuntimeEvent::StatusChanged(message.unwrap_or(fallback))];
        if success
            && action == ActionId::COPY
            && self.settings.copy_disposition == CopyDisposition::CloseOverlay
        {
            events.push(RuntimeEvent::CloseOverlay);
        }
        events
    }
}

/// Metadata exposed by a future plugin registry.
///
/// The first version intentionally contains only declarative fields. Dynamic
/// loading and permission negotiation can be added behind this boundary once
/// the plugin use cases and trust model are concrete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub actions: Vec<String>,
}

/// Owned identifier for an action contributed at runtime by a plugin.
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

/// In-process extension point for trusted plugins.
///
/// A plugin receives the same command/event vocabulary as every other runtime
/// caller. The host remains responsible for permissions, isolation, dynamic
/// loading, and dispatching shell-only effects such as windows or clipboard
/// access.
pub trait RuntimePlugin: Send {
    fn descriptor(&self) -> &PluginDescriptor;

    fn on_event(&mut self, event: &RuntimeEvent) -> Vec<RuntimeCommand>;

    fn invoke(&mut self, action_id: &str) -> Result<Vec<RuntimeCommand>, String>;
}

/// Registry used by an application host to keep plugin identities unique and
/// dispatch runtime events without exposing UI objects to extensions.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Box<dyn RuntimePlugin>>,
}

impl PluginRegistry {
    pub fn register(&mut self, plugin: Box<dyn RuntimePlugin>) -> Result<(), PluginRegistryError> {
        let id = plugin.descriptor().id.trim();
        if id.is_empty() {
            return Err(PluginRegistryError::InvalidId);
        }
        if self
            .plugins
            .iter()
            .any(|registered| registered.descriptor().id == id)
        {
            return Err(PluginRegistryError::DuplicateId(id.to_string()));
        }
        self.plugins.push(plugin);
        Ok(())
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &PluginDescriptor> {
        self.plugins.iter().map(|plugin| plugin.descriptor())
    }

    pub fn dispatch_event(&mut self, event: &RuntimeEvent) -> Vec<RuntimeCommand> {
        self.plugins
            .iter_mut()
            .flat_map(|plugin| plugin.on_event(event))
            .collect()
    }

    pub fn invoke(
        &mut self,
        action: &PluginActionId,
    ) -> Result<Vec<RuntimeCommand>, PluginRegistryError> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|plugin| plugin.descriptor().id == action.plugin_id)
            .ok_or_else(|| PluginRegistryError::UnknownPlugin(action.plugin_id.clone()))?;
        if !plugin
            .descriptor()
            .actions
            .iter()
            .any(|registered| registered == &action.action_id)
        {
            return Err(PluginRegistryError::UnknownAction(action.clone()));
        }
        plugin
            .invoke(&action.action_id)
            .map_err(|message| PluginRegistryError::InvocationFailed {
                action: action.clone(),
                message,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginRegistryError {
    InvalidId,
    DuplicateId(String),
    UnknownPlugin(String),
    UnknownAction(PluginActionId),
    InvocationFailed {
        action: PluginActionId,
        message: String,
    },
}

impl std::fmt::Display for PluginRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("plugin id must not be empty"),
            Self::DuplicateId(id) => write!(formatter, "plugin id is already registered: {id}"),
            Self::UnknownPlugin(id) => write!(formatter, "plugin is not registered: {id}"),
            Self::UnknownAction(action) => write!(
                formatter,
                "plugin action is not registered: {}:{}",
                action.plugin_id, action.action_id
            ),
            Self::InvocationFailed { action, message } => write!(
                formatter,
                "plugin action failed: {}:{}: {message}",
                action.plugin_id, action.action_id
            ),
        }
    }
}

impl std::error::Error for PluginRegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPlugin {
        descriptor: PluginDescriptor,
    }

    impl RuntimePlugin for TestPlugin {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        fn on_event(&mut self, _event: &RuntimeEvent) -> Vec<RuntimeCommand> {
            Vec::new()
        }

        fn invoke(&mut self, _action_id: &str) -> Result<Vec<RuntimeCommand>, String> {
            Ok(vec![RuntimeCommand::BeginCapture])
        }
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
    fn copy_policy_emits_close_event_only_when_enabled() {
        let mut runtime = CaptureRuntime::new(AppSettings {
            copy_disposition: CopyDisposition::CloseOverlay,
            ..AppSettings::default()
        });
        let events = runtime.dispatch(RuntimeCommand::ActionCompleted {
            action: ActionId::COPY,
            success: true,
            message: None,
        });

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::CloseOverlay)));
    }

    #[test]
    fn plugin_ids_are_unique() {
        let plugin = || {
            Box::new(TestPlugin {
                descriptor: PluginDescriptor {
                    id: "example.tool".to_string(),
                    name: "Example".to_string(),
                    version: "1.0.0".to_string(),
                    actions: vec!["example-action".to_string()],
                },
            }) as Box<dyn RuntimePlugin>
        };
        let mut registry = PluginRegistry::default();

        registry.register(plugin()).unwrap();
        assert_eq!(
            registry.register(plugin()),
            Err(PluginRegistryError::DuplicateId("example.tool".to_string()))
        );
    }

    #[test]
    fn plugin_actions_use_owned_runtime_ids() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Box::new(TestPlugin {
                descriptor: PluginDescriptor {
                    id: "example.tool".to_string(),
                    name: "Example".to_string(),
                    version: "1.0.0".to_string(),
                    actions: vec!["capture-again".to_string()],
                },
            }))
            .unwrap();

        let commands = registry
            .invoke(&PluginActionId::new("example.tool", "capture-again"))
            .unwrap();
        assert!(matches!(
            commands.as_slice(),
            [RuntimeCommand::BeginCapture]
        ));
    }
}
