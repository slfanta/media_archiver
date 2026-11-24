#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::Local;
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tauri::Emitter;
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
struct VideoFile {
    path: String,
    name: String,
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    current: usize,
    total: usize,
    file: String,
    status: String,
    message: String,
}

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "mpe", "m1v", "m2v",
    "mpv", "m2ts", "mts", "ts", "3gp", "3g2", "3gpp", "ogv", "vob", "asf", "divx", "rm", "rmvb",
    "f4v", "amv", "mxf", "qt", "wtv", "dvr-ms",
];

#[tauri::command]
async fn scan_videos(folder: String) -> Result<Vec<VideoFile>, String> {
    tauri::async_runtime::spawn_blocking(move || scan_videos_inner(folder))
        .await
        .map_err(|error| format!("scan worker failed: {error}"))?
}

fn scan_videos_inner(folder: String) -> Result<Vec<VideoFile>, String> {
    let root = Path::new(&folder)
        .canonicalize()
        .map_err(|error| format!("cannot access folder '{}': {error}", folder))?;
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let mut videos = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.map_err(|error| format!("scan failed: {error}"))?;
        if !entry.file_type().is_file() || !is_video(entry.path()) {
            continue;
        }

        let path = entry
            .path()
            .canonicalize()
            .map_err(|error| format!("cannot access '{}': {error}", entry.path().display()))?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        videos.push(VideoFile {
            path: path.to_string_lossy().into_owned(),
            name,
        });
    }

    videos.sort_by(|left, right| left.path.to_lowercase().cmp(&right.path.to_lowercase()));
    Ok(videos)
}

#[tauri::command]
async fn batch_archive(
    folder: String,
    video_paths: Vec<String>,
    password: String,
    delete_originals: bool,
    window: tauri::Window,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        batch_archive_inner(folder, video_paths, password, delete_originals, window)
    })
    .await
    .map_err(|error| format!("archive worker failed: {error}"))?
}

fn batch_archive_inner(
    folder: String,
    video_paths: Vec<String>,
    password: String,
    delete_originals: bool,
    window: tauri::Window,
) -> Result<usize, String> {
    if password.is_empty() {
        return Err("archive password cannot be empty".to_string());
    }
    if video_paths.is_empty() {
        return Err("there are no video files to archive".to_string());
    }
    let start_date = Local::now().format("%Y-%m-%d").to_string();

    let root = Path::new(&folder)
        .canonicalize()
        .map_err(|error| format!("cannot access folder '{}': {error}", folder))?;
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }
    let seven_zip = find_seven_zip().ok_or_else(|| {
        "7-Zip command-line tool (7z.exe) was not found. Install 7-Zip or add it to PATH."
            .to_string()
    })?;

    let video_directories = video_directories(&root, &video_paths)?;
    let password_file_count = create_password_files(&video_directories, &password, &start_date)?;
    let total = video_paths.len();
    emit_progress(
        &window,
        0,
        total,
        String::new(),
        "password-files-created",
        format!(
            "Created {password_file_count} plaintext password file(s) in folders containing videos."
        ),
    );
    let mut archived_count = 0;
    for (index, supplied_path) in video_paths.iter().enumerate() {
        let current = index + 1;
        let name = Path::new(supplied_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        emit_progress(
            &window,
            current,
            total,
            supplied_path.clone(),
            "processing",
            format!("Archiving {name}..."),
        );
        let result = archive_one(
            &seven_zip,
            &root,
            supplied_path,
            &password,
            delete_originals,
        );
        let (file, status, message) = match result {
            Ok((file, message, status)) => {
                archived_count += 1;
                (file, status, message)
            }
            Err((file, message)) => (file, "error", message),
        };
        emit_progress(&window, current, total, file, status, message);
    }

    Ok(archived_count)
}

fn video_directories(root: &Path, video_paths: &[String]) -> Result<BTreeSet<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    for supplied_path in video_paths {
        let source = Path::new(supplied_path)
            .canonicalize()
            .map_err(|error| format!("source unavailable: {error}"))?;
        if !source.starts_with(root) {
            return Err("a video is outside the selected folder".to_string());
        }
        if !source.is_file() || !is_video(&source) {
            return Err("a source is not a supported video file".to_string());
        }
        let parent = source
            .parent()
            .ok_or_else(|| "a video has no parent folder".to_string())?;
        directories.insert(parent.to_path_buf());
    }
    Ok(directories)
}

fn create_password_files(
    directories: &BTreeSet<PathBuf>,
    password: &str,
    start_date: &str,
) -> Result<usize, String> {
    let mut created = Vec::new();
    for directory in directories {
        match create_password_file(directory, password, start_date) {
            Ok(path) => created.push(path),
            Err(error) => {
                let cleanup_errors = rollback_password_files(&created);
                if cleanup_errors.is_empty() {
                    return Err(error);
                }
                return Err(format!(
                    "{error}; cleanup also failed in: {}",
                    cleanup_errors.join("; ")
                ));
            }
        }
    }
    Ok(created.len())
}

fn create_password_file(
    directory: &Path,
    password: &str,
    start_date: &str,
) -> Result<PathBuf, String> {
    let password_component = sanitize_password_component(password);
    let base_name = format!("pw_{password_component}_{start_date}");
    let mut suffix = 0u32;

    loop {
        let numbered_suffix = if suffix == 0 {
            String::new()
        } else {
            format!("_{suffix}")
        };
        let path = directory.join(format!("{base_name}{numbered_suffix}.txt"));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| "too many password-file name collisions".to_string())?;
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "could not create plaintext password file in '{}': {error}",
                    directory.display()
                ));
            }
        };

        if let Err(error) = file.write_all(password.as_bytes()) {
            drop(file);
            return match fs::remove_file(&path) {
                Ok(()) => Err(format!(
                    "could not write plaintext password file in '{}': {error}",
                    directory.display()
                )),
                Err(cleanup_error) => Err(format!(
                    "could not write plaintext password file in '{}' and could not remove the partial file: {cleanup_error}",
                    directory.display()
                )),
            };
        }
        return Ok(path);
    }
}

fn rollback_password_files(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| {
            fs::remove_file(path).err().map(|error| {
                path.parent()
                    .map(|parent| format!("'{}': {error}", parent.display()))
                    .unwrap_or_else(|| error.to_string())
            })
        })
        .collect()
}

fn sanitize_password_component(password: &str) -> String {
    password
        .chars()
        .map(|character| {
            if character <= '\u{1f}' || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect()
}

fn archive_one(
    seven_zip: &Path,
    root: &Path,
    supplied_path: &str,
    password: &str,
    delete_originals: bool,
) -> Result<(String, String, &'static str), (String, String)> {
    let supplied = Path::new(supplied_path);
    let source = supplied.canonicalize().map_err(|error| {
        (
            supplied_path.to_string(),
            format!("source unavailable: {error}"),
        )
    })?;
    let file = source.to_string_lossy().into_owned();
    if !source.starts_with(root) {
        return Err((file, "video is outside the selected folder".to_string()));
    }
    if !source.is_file() || !is_video(&source) {
        return Err((file, "source is not a supported video file".to_string()));
    }

    let parent = source
        .parent()
        .ok_or_else(|| (file.clone(), "video has no parent folder".to_string()))?;
    let file_name = source
        .file_name()
        .ok_or_else(|| (file.clone(), "video has no file name".to_string()))?;
    let archive_name = Path::new(".").join(append_7z_suffix(file_name));
    let source_name = Path::new(".").join(file_name);

    let archive_output = Command::new(seven_zip)
        .current_dir(parent)
        .arg("a")
        .arg("-t7z")
        .arg("-mx=5")
        .arg(format!("-p{password}"))
        .arg("-mhe=on")
        .arg(&archive_name)
        .arg(&source_name)
        .output()
        .map_err(|error| (file.clone(), format!("failed to launch 7-Zip: {error}")))?;
    ensure_success(&archive_output, &file, "7-Zip archive creation")?;

    let test_output = Command::new(seven_zip)
        .current_dir(parent)
        .arg("t")
        .arg(format!("-p{password}"))
        .arg(&archive_name)
        .output()
        .map_err(|error| (file.clone(), format!("failed to verify archive: {error}")))?;
    ensure_success(&test_output, &file, "archive verification")?;

    if delete_originals {
        match std::fs::remove_file(&source) {
            Ok(()) => Ok((
                file,
                "Archive verified; original deleted.".to_string(),
                "done",
            )),
            Err(error) => Ok((
                file,
                format!("Archive verified, but original could not be deleted: {error}"),
                "warning",
            )),
        }
    } else {
        Ok((file, "Archive created and verified.".to_string(), "done"))
    }
}

fn ensure_success(output: &Output, file: &str, action: &str) -> Result<(), (String, String)> {
    if output.status.success() {
        return Ok(());
    }
    let details = String::from_utf8_lossy(&output.stderr);
    let details = details.trim();
    let details = if details.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        details.to_string()
    };
    Err((
        file.to_string(),
        format!(
            "{action} failed: {}",
            if details.is_empty() {
                "unknown error"
            } else {
                &details
            }
        ),
    ))
}

fn append_7z_suffix(file_name: &std::ffi::OsStr) -> OsString {
    let mut archive_name = OsString::from(file_name);
    archive_name.push(".7z");
    archive_name
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| VIDEO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn find_seven_zip() -> Option<PathBuf> {
    let executable = if cfg!(windows) { "7z.exe" } else { "7z" };
    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(executable);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = env::var_os(variable) {
            let candidate = PathBuf::from(base).join("7-Zip").join("7z.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Some(base) = env::var_os("LOCALAPPDATA") {
        let candidate = PathBuf::from(base)
            .join("Programs")
            .join("7-Zip")
            .join("7z.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn emit_progress(
    window: &tauri::Window,
    current: usize,
    total: usize,
    file: String,
    status: &str,
    message: String,
) {
    let _ = window.emit(
        "archive-progress",
        ProgressEvent {
            current,
            total,
            file,
            status: status.to_string(),
            message,
        },
    );
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![scan_videos, batch_archive])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{create_password_file, sanitize_password_component};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn password_filename_replaces_windows_invalid_characters() {
        assert_eq!(
            sanitize_password_component(r#"p/a:ss\w?o*r<d>|"#),
            "p_a_ss_w_o_r_d__"
        );
    }

    #[test]
    fn password_file_contents_are_exact_and_name_collisions_are_numbered() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "archive-tool-password-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();

        let result = (|| {
            let first = create_password_file(&directory, "p/ass:word", "2026-11-24").unwrap();
            let second = create_password_file(&directory, "p/ass:word", "2026-11-24").unwrap();
            assert_eq!(
                first.file_name().unwrap().to_string_lossy(),
                "pw_p_ass_word_2026-11-24.txt"
            );
            assert_eq!(
                second.file_name().unwrap().to_string_lossy(),
                "pw_p_ass_word_2026-11-24_1.txt"
            );
            assert_eq!(fs::read(first).unwrap(), b"p/ass:word");
            assert_eq!(fs::read(second).unwrap(), b"p/ass:word");
        })();

        fs::remove_dir_all(directory).unwrap();
        result
    }
}
