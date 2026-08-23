use arboard::{Clipboard, ImageData};
use capture_annotation::CaptureDocument;
use capture_render::{flatten, RenderedImage};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveMode {
    Default,
    DefaultAndReveal,
    SaveAs,
}

pub(super) fn prepare_save_directory(value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("保存目录不能为空".to_string());
    }
    let mut directory = PathBuf::from(value);
    if directory.is_relative() {
        directory = std::env::current_dir()
            .map_err(|error| format!("无法解析相对保存目录：{error}"))?
            .join(directory);
    }
    fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建保存目录 {}：{error}", directory.display()))?;
    let metadata = fs::metadata(&directory)
        .map_err(|error| format!("无法访问保存目录 {}：{error}", directory.display()))?;
    if !metadata.is_dir() {
        return Err(format!("保存路径不是目录：{}", directory.display()));
    }
    directory
        .canonicalize()
        .map_err(|error| format!("无法规范化保存目录 {}：{error}", directory.display()))
}

pub(super) fn next_capture_path(directory: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建保存目录 {}：{error}", directory.display()))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("系统时间无效：{error}"))?
        .as_millis();
    for sequence in 0..10_000u32 {
        let name = if sequence == 0 {
            format!("capture-{timestamp}.png")
        } else {
            format!("capture-{timestamp}-{sequence}.png")
        };
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("无法生成不重复的截图文件名".to_string())
}

pub(super) fn choose_save_path(
    directory: &Path,
    parent: &slint::Window,
) -> Result<Option<PathBuf>, String> {
    let suggested = next_capture_path(directory)?;
    let file_name = suggested
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("无法生成有效的截图文件名：{}", suggested.display()))?;
    let parent_handle = parent.window_handle();
    Ok(rfd::FileDialog::new()
        .set_title("另存截图为")
        .set_parent(&parent_handle)
        .set_directory(directory)
        .set_file_name(file_name)
        .add_filter("PNG 图片", &["png"])
        .save_file()
        .map(ensure_png_extension))
}

fn ensure_png_extension(mut path: PathBuf) -> PathBuf {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        path.set_extension("png");
    }
    path
}

#[cfg(windows)]
pub(super) fn reveal_saved_file(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
pub(super) fn reveal_saved_file(path: &Path) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("保存路径没有父目录：{}", path.display()))?;
    Command::new("xdg-open")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(super) fn copy_document_to_clipboard(document: &CaptureDocument) -> Result<(), String> {
    let rendered = flatten(document).map_err(|error| error.to_string())?;
    copy_rendered_to_clipboard(&rendered)
}

pub(super) fn copy_rendered_to_clipboard(rendered: &RenderedImage) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_image(ImageData {
            width: rendered.width as usize,
            height: rendered.height as usize,
            bytes: std::borrow::Cow::Owned(rendered.pixels.clone()),
        })
        .map_err(|error| error.to_string())
}

pub(super) fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_text(text.to_string())
        .map_err(|error| error.to_string())
}
