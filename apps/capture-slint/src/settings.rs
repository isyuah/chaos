use capture_runtime::{CopyDisposition, RuntimePolicy};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const SETTINGS_SCHEMA_VERSION: u32 = 1;
const SETTINGS_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    pub shortcut: Shortcut,
    pub save_directory: PathBuf,
    pub copy_disposition: CopyDisposition,
    pub shortcut_error: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        let save_directory = dirs::picture_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Chaos Capture");
        Self {
            shortcut: Shortcut::default(),
            save_directory,
            copy_disposition: CopyDisposition::KeepEditorOpen,
            shortcut_error: None,
        }
    }
}

impl AppSettings {
    pub fn runtime_policy(&self) -> RuntimePolicy {
        RuntimePolicy {
            copy_disposition: self.copy_disposition,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    key: ShortcutKey,
}

impl Default for Shortcut {
    fn default() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: true,
            logo: false,
            key: ShortcutKey::Letter('S'),
        }
    }
}

impl Shortcut {
    pub fn global_hotkey(&self) -> Result<global_hotkey::hotkey::HotKey, ShortcutParseError> {
        self.global_hotkey_text().parse().map_err(
            |error: global_hotkey::hotkey::HotKeyParseError| {
                ShortcutParseError::Unsupported(error.to_string())
            },
        )
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn portal_trigger(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("CTRL".to_string());
        }
        if self.alt {
            parts.push("ALT".to_string());
        }
        if self.shift {
            parts.push("SHIFT".to_string());
        }
        if self.logo {
            parts.push("LOGO".to_string());
        }
        parts.push(self.key.portal_name());
        parts.join("+")
    }

    fn global_hotkey_text(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("control".to_string());
        }
        if self.alt {
            parts.push("alt".to_string());
        }
        if self.shift {
            parts.push("shift".to_string());
        }
        if self.logo {
            parts.push("super".to_string());
        }
        parts.push(self.key.global_hotkey_name());
        parts.join("+")
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.logo {
            parts.push("Win".to_string());
        }
        parts.push(self.key.display_name());
        formatter.write_str(&parts.join("+"))
    }
}

impl FromStr for Shortcut {
    type Err = ShortcutParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ShortcutParseError::Empty);
        }

        let tokens = value.split('+').map(str::trim).collect::<Vec<_>>();
        if tokens.iter().any(|token| token.is_empty()) {
            return Err(ShortcutParseError::InvalidFormat);
        }

        let mut shortcut = Self {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            key: ShortcutKey::Letter('S'),
        };
        let mut key = None;
        for token in tokens {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" if key.is_none() => set_once(&mut shortcut.ctrl)?,
                "alt" if key.is_none() => set_once(&mut shortcut.alt)?,
                "shift" if key.is_none() => set_once(&mut shortcut.shift)?,
                "win" | "super" | "logo" if key.is_none() => set_once(&mut shortcut.logo)?,
                _ if key.is_none() => key = Some(ShortcutKey::from_str(token)?),
                _ => return Err(ShortcutParseError::InvalidFormat),
            }
        }
        shortcut.key = key.ok_or(ShortcutParseError::MissingKey)?;

        let has_modifier = shortcut.ctrl || shortcut.alt || shortcut.shift || shortcut.logo;
        if !has_modifier && shortcut.key != ShortcutKey::PrintScreen {
            return Err(ShortcutParseError::ModifierRequired);
        }
        shortcut.global_hotkey()?;
        Ok(shortcut)
    }
}

fn set_once(value: &mut bool) -> Result<(), ShortcutParseError> {
    if *value {
        return Err(ShortcutParseError::DuplicateModifier);
    }
    *value = true;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShortcutKey {
    Letter(char),
    Digit(char),
    Function(u8),
    PrintScreen,
}

impl ShortcutKey {
    fn from_str(value: &str) -> Result<Self, ShortcutParseError> {
        let upper = value.to_ascii_uppercase();
        if upper.len() == 1 {
            let character = upper.chars().next().expect("one-character shortcut key");
            if character.is_ascii_alphabetic() {
                return Ok(Self::Letter(character));
            }
            if character.is_ascii_digit() {
                return Ok(Self::Digit(character));
            }
        }
        if let Some(number) = upper
            .strip_prefix('F')
            .and_then(|number| number.parse::<u8>().ok())
            .filter(|number| (1..=24).contains(number))
        {
            return Ok(Self::Function(number));
        }
        if matches!(upper.as_str(), "PRINT" | "PRINTSCREEN") {
            return Ok(Self::PrintScreen);
        }
        Err(ShortcutParseError::UnsupportedKey(value.to_string()))
    }

    fn display_name(self) -> String {
        match self {
            Self::Letter(character) | Self::Digit(character) => character.to_string(),
            Self::Function(number) => format!("F{number}"),
            Self::PrintScreen => "PrintScreen".to_string(),
        }
    }

    fn global_hotkey_name(self) -> String {
        match self {
            Self::Letter(character) => format!("Key{character}"),
            Self::Digit(character) => format!("Digit{character}"),
            Self::Function(number) => format!("F{number}"),
            Self::PrintScreen => "PrintScreen".to_string(),
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn portal_name(self) -> String {
        match self {
            Self::Letter(character) => character.to_ascii_lowercase().to_string(),
            Self::Digit(character) => character.to_string(),
            Self::Function(number) => format!("F{number}"),
            Self::PrintScreen => "Print".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShortcutParseError {
    #[error("快捷键不能为空")]
    Empty,
    #[error("快捷键格式无效")]
    InvalidFormat,
    #[error("快捷键必须包含一个主键")]
    MissingKey,
    #[error("普通按键至少需要一个 Ctrl、Alt、Shift 或 Win 修饰键")]
    ModifierRequired,
    #[error("快捷键包含重复的修饰键")]
    DuplicateModifier,
    #[error("暂不支持快捷键 {0}；可使用 A-Z、0-9、F1-F24 或 PrintScreen")]
    UnsupportedKey(String),
    #[error("当前平台不支持该快捷键：{0}")]
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn discover() -> Result<Self, SettingsError> {
        let directory = dirs::config_dir().ok_or(SettingsError::ConfigDirectoryUnavailable)?;
        Ok(Self::at(
            directory.join("chaos-capture").join(SETTINGS_FILE_NAME),
        ))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_default(&self) -> LoadedSettings {
        if !self.path.exists() {
            return LoadedSettings {
                settings: AppSettings::default(),
                warning: None,
            };
        }
        match self.load() {
            Ok(settings) => LoadedSettings {
                settings,
                warning: None,
            },
            Err(error) => LoadedSettings {
                settings: AppSettings::default(),
                warning: Some(error.to_string()),
            },
        }
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), SettingsError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| SettingsError::InvalidPath(self.path.clone()))?;
        fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
            path: parent.to_path_buf(),
            source,
        })?;

        let document = SettingsDocument::from(settings);
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| SettingsError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        serde_json::to_writer_pretty(&mut temporary, &document).map_err(|source| {
            SettingsError::Serialize {
                path: self.path.clone(),
                source,
            }
        })?;
        temporary
            .write_all(b"\n")
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| SettingsError::Write {
                path: temporary.path().to_path_buf(),
                source,
            })?;
        temporary
            .persist(&self.path)
            .map_err(|error| SettingsError::Write {
                path: self.path.clone(),
                source: error.error,
            })?;

        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SettingsError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        Ok(())
    }

    fn load(&self) -> Result<AppSettings, SettingsError> {
        let bytes = fs::read(&self.path).map_err(|source| SettingsError::Read {
            path: self.path.clone(),
            source,
        })?;
        let document: SettingsDocument =
            serde_json::from_slice(&bytes).map_err(|source| SettingsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        if document.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema(document.schema_version));
        }
        let shortcut =
            document
                .shortcut
                .parse()
                .map_err(|source| SettingsError::InvalidShortcut {
                    shortcut: document.shortcut.clone(),
                    source,
                })?;
        if document.save_directory.as_os_str().is_empty() {
            return Err(SettingsError::EmptySaveDirectory);
        }
        Ok(AppSettings {
            shortcut,
            save_directory: document.save_directory,
            copy_disposition: document.copy_disposition.into(),
            shortcut_error: document.shortcut_error,
        })
    }
}

pub struct LoadedSettings {
    pub settings: AppSettings,
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("无法确定系统配置目录")]
    ConfigDirectoryUnavailable,
    #[error("设置路径无效：{0}")]
    InvalidPath(PathBuf),
    #[error("无法读取设置文件 {}：{source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("无法解析设置文件 {}：{source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("设置文件版本 {0} 暂不受支持")]
    UnsupportedSchema(u32),
    #[error("设置中的快捷键 {shortcut} 无效：{source}")]
    InvalidShortcut {
        shortcut: String,
        source: ShortcutParseError,
    },
    #[error("保存目录不能为空")]
    EmptySaveDirectory,
    #[error("无法序列化设置文件 {}：{source}", path.display())]
    Serialize {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("无法写入 {}：{source}", path.display())]
    Write { path: PathBuf, source: io::Error },
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsDocument {
    schema_version: u32,
    shortcut: String,
    save_directory: PathBuf,
    copy_disposition: PersistedCopyDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shortcut_error: Option<String>,
}

impl From<&AppSettings> for SettingsDocument {
    fn from(settings: &AppSettings) -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            shortcut: settings.shortcut.to_string(),
            save_directory: settings.save_directory.clone(),
            copy_disposition: settings.copy_disposition.into(),
            shortcut_error: settings.shortcut_error.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedCopyDisposition {
    KeepEditorOpen,
    CloseOverlay,
}

impl From<CopyDisposition> for PersistedCopyDisposition {
    fn from(disposition: CopyDisposition) -> Self {
        match disposition {
            CopyDisposition::KeepEditorOpen => Self::KeepEditorOpen,
            CopyDisposition::CloseOverlay => Self::CloseOverlay,
        }
    }
}

impl From<PersistedCopyDisposition> for CopyDisposition {
    fn from(disposition: PersistedCopyDisposition) -> Self {
        match disposition {
            PersistedCopyDisposition::KeepEditorOpen => Self::KeepEditorOpen,
            PersistedCopyDisposition::CloseOverlay => Self::CloseOverlay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_has_stable_native_and_portal_forms() {
        let shortcut: Shortcut = "shift + ctrl + s".parse().unwrap();
        assert_eq!(shortcut.to_string(), "Ctrl+Shift+S");
        assert_eq!(shortcut.portal_trigger(), "CTRL+SHIFT+s");
        let native = shortcut.global_hotkey().unwrap();
        assert_eq!(native.key, global_hotkey::hotkey::Code::KeyS);
        assert!(native
            .mods
            .contains(global_hotkey::hotkey::Modifiers::CONTROL));
        assert!(native
            .mods
            .contains(global_hotkey::hotkey::Modifiers::SHIFT));
    }

    #[test]
    fn shortcut_rejects_keys_that_would_hijack_typing() {
        assert_eq!(
            "S".parse::<Shortcut>().unwrap_err(),
            ShortcutParseError::ModifierRequired
        );
        assert_eq!(
            "Ctrl+Mouse4".parse::<Shortcut>().unwrap_err(),
            ShortcutParseError::UnsupportedKey("Mouse4".to_string())
        );
        assert_eq!(
            "PrintScreen".parse::<Shortcut>().unwrap().to_string(),
            "PrintScreen"
        );
    }

    #[test]
    fn settings_round_trip_through_versioned_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::at(directory.path().join("settings.json"));
        let settings = AppSettings {
            shortcut: "Ctrl+Alt+F12".parse().unwrap(),
            save_directory: directory.path().join("screenshots"),
            copy_disposition: CopyDisposition::CloseOverlay,
            shortcut_error: Some("快捷键已被占用".to_string()),
        };

        store.save(&settings).unwrap();
        let mut updated = settings.clone();
        updated.shortcut_error = None;
        updated.copy_disposition = CopyDisposition::KeepEditorOpen;
        store.save(&updated).unwrap();
        let loaded = store.load_or_default();

        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings, updated);
    }

    #[test]
    fn corrupt_settings_fall_back_without_overwriting_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"not json").unwrap();
        let store = SettingsStore::at(path.clone());

        let loaded = store.load_or_default();

        assert!(loaded.warning.is_some());
        assert_eq!(fs::read(path).unwrap(), b"not json");
        assert_eq!(loaded.settings.shortcut, Shortcut::default());
    }
}
