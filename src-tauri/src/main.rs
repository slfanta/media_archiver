#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::Local;
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
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
    percent: Option<u8>,
}

#[derive(Clone, Copy, Default)]
struct TaskState {
    kind: Option<&'static str>,
    paused: bool,
    cancelled: bool,
}

#[derive(Default)]
struct TaskControl(Mutex<TaskState>);

struct TaskGuard(Arc<TaskControl>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() = TaskState::default();
    }
}

impl TaskControl {
    fn start(self: &Arc<Self>, kind: &'static str) -> Result<TaskGuard, String> {
        let mut state = self.0.lock().unwrap();
        if state.kind.is_some() {
            return Err("another task is already running".to_string());
        }
        *state = TaskState {
            kind: Some(kind),
            ..TaskState::default()
        };
        Ok(TaskGuard(Arc::clone(self)))
    }

    fn snapshot(&self) -> TaskState {
        *self.0.lock().unwrap()
    }

    fn wait_until_ready(&self) -> io::Result<()> {
        loop {
            let state = self.snapshot();
            if state.cancelled {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Task stopped."));
            }
            if !state.paused {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

#[tauri::command]
fn control_task(action: String, control: tauri::State<'_, Arc<TaskControl>>) -> Result<(), String> {
    let mut state = control.inner().0.lock().unwrap();
    if state.kind.is_none() {
        return Err("no task is running".to_string());
    }
    match action.as_str() {
        "pause" | "resume" if state.kind == Some("archive") && !state.cancelled => {
            state.paused = action == "pause";
        }
        "stop" => {
            state.cancelled = true;
            state.paused = false;
        }
        _ => return Err("this task does not support that action".to_string()),
    }
    Ok(())
}

#[derive(Clone, Serialize)]
struct ScanProgress {
    checked: usize,
    found: usize,
    path: String,
}

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "mpe", "m1v", "m2v",
    "mpv", "m2ts", "mts", "ts", "3gp", "3g2", "3gpp", "ogv", "vob", "asf", "divx", "rm", "rmvb",
    "f4v", "amv", "mxf", "qt", "wtv", "dvr-ms",
];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[tauri::command]
async fn scan_videos(
    folder: String,
    window: tauri::Window,
    control: tauri::State<'_, Arc<TaskControl>>,
) -> Result<Vec<VideoFile>, String> {
    let control = Arc::clone(control.inner());
    let task = control.start("scan")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _task = task;
        scan_videos_inner(folder, &control, |checked, found, path| {
            let _ = window.emit(
                "scan-progress",
                ScanProgress {
                    checked,
                    found,
                    path: path.to_string_lossy().into_owned(),
                },
            );
        })
    })
    .await
    .map_err(|error| format!("scan worker failed: {error}"))?
}

fn scan_videos_inner(
    folder: String,
    control: &TaskControl,
    mut on_progress: impl FnMut(usize, usize, &Path),
) -> Result<Vec<VideoFile>, String> {
    let root = Path::new(&folder)
        .canonicalize()
        .map_err(|error| format!("cannot access folder '{}': {error}", folder))?;
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let mut videos = Vec::new();
    let mut checked = 0;
    let mut last_update = Instant::now();
    on_progress(0, 0, &root);
    for entry in WalkDir::new(&root).follow_links(false) {
        control
            .wait_until_ready()
            .map_err(|error| error.to_string())?;
        let entry = entry.map_err(|error| format!("scan failed: {error}"))?;
        checked += 1;
        if last_update.elapsed() >= Duration::from_millis(200) {
            on_progress(checked, videos.len(), entry.path());
            last_update = Instant::now();
        }
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
    control
        .wait_until_ready()
        .map_err(|error| error.to_string())?;
    on_progress(checked, videos.len(), &root);
    Ok(videos)
}

#[tauri::command]
async fn batch_archive(
    folder: String,
    video_paths: Vec<String>,
    password: String,
    delete_originals: bool,
    window: tauri::Window,
    control: tauri::State<'_, Arc<TaskControl>>,
) -> Result<usize, String> {
    let control = Arc::clone(control.inner());
    let task = control.start("archive")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _task = task;
        batch_archive_inner(
            folder,
            video_paths,
            password,
            delete_originals,
            window,
            &control,
        )
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
    control: &TaskControl,
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

    validate_video_paths(&root, &video_paths, control)?;
    let batch_directories = batch_directories(&root, control)?;
    let password_file_count =
        create_password_files(&batch_directories, &password, &start_date, control)?;
    let total = video_paths.len();
    emit_progress(
        &window,
        0,
        total,
        String::new(),
        "password-files-created",
        format!(
            "Created {password_file_count} plaintext password file(s) in the selected folder and its descendants."
        ),
        None,
    );
    let mut archived_count = 0;
    for (index, supplied_path) in video_paths.iter().enumerate() {
        control.wait_until_ready().map_err(|_| {
            format!("Archiving stopped. {archived_count}/{total} archive(s) verified.")
        })?;
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
            Some(0),
        );
        let result = archive_one(
            &seven_zip,
            &root,
            supplied_path,
            &password,
            delete_originals,
            control,
            |status, percent| {
                let action = if status == "verifying" {
                    "Verifying"
                } else {
                    "Archiving"
                };
                emit_progress(
                    &window,
                    current,
                    total,
                    supplied_path.clone(),
                    status,
                    format!("{action} {name}: {percent}%"),
                    Some(percent),
                );
            },
        );
        if control.snapshot().cancelled && result.is_err() {
            emit_progress(
                &window,
                current,
                total,
                supplied_path.clone(),
                "cancelled",
                "Archive stopped; original kept.".to_string(),
                None,
            );
            return Err(format!(
                "Archiving stopped. {archived_count}/{total} archive(s) verified."
            ));
        }
        let (file, status, message) = match result {
            Ok((file, message, status)) => {
                archived_count += 1;
                (file, status, message)
            }
            Err((file, message)) => (file, "error", message),
        };
        let percent = if status == "error" { None } else { Some(100) };
        emit_progress(&window, current, total, file, status, message, percent);
    }

    Ok(archived_count)
}

fn validate_video_paths(
    root: &Path,
    video_paths: &[String],
    control: &TaskControl,
) -> Result<(), String> {
    for supplied_path in video_paths {
        control
            .wait_until_ready()
            .map_err(|error| error.to_string())?;
        let source = Path::new(supplied_path)
            .canonicalize()
            .map_err(|error| format!("source unavailable: {error}"))?;
        if !source.starts_with(root) {
            return Err("a video is outside the selected folder".to_string());
        }
        if !source.is_file() || !is_video(&source) {
            return Err("a source is not a supported video file".to_string());
        }
    }
    Ok(())
}

fn batch_directories(root: &Path, control: &TaskControl) -> Result<BTreeSet<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    for entry in WalkDir::new(root).follow_links(false) {
        control
            .wait_until_ready()
            .map_err(|error| error.to_string())?;
        let entry = entry.map_err(|error| format!("folder scan failed: {error}"))?;
        if entry.file_type().is_dir() {
            directories.insert(entry.path().to_path_buf());
        }
    }
    Ok(directories)
}

fn create_password_files(
    directories: &BTreeSet<PathBuf>,
    password: &str,
    start_date: &str,
    control: &TaskControl,
) -> Result<usize, String> {
    let mut created = Vec::new();
    for directory in directories {
        let result = control
            .wait_until_ready()
            .map_err(|error| error.to_string())
            .and_then(|_| create_password_file(directory, password, start_date));
        match result {
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
    control: &TaskControl,
    mut on_progress: impl FnMut(&str, u8),
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
    let archive_path = parent.join(&archive_name);
    let mut cleanup = IncompleteArchive {
        remove_on_drop: !archive_path.exists(),
        path: archive_path,
    };

    let archive_output = run_seven_zip(
        seven_zip_command(seven_zip)
            .current_dir(parent)
            .arg("a")
            .arg("-t7z")
            .arg("-mx=5")
            .arg(format!("-p{password}"))
            .arg("-mhe=on")
            .arg(&archive_name)
            .arg(&source_name),
        |percent| on_progress("processing", percent),
        control,
    )
    .map_err(|error| (file.clone(), format!("failed to launch 7-Zip: {error}")))?;
    ensure_success(&archive_output, &file, "7-Zip archive creation")?;

    on_progress("verifying", 0);
    let test_output = run_seven_zip(
        seven_zip_command(seven_zip)
            .current_dir(parent)
            .arg("t")
            .arg(format!("-p{password}"))
            .arg(&archive_name),
        |percent| on_progress("verifying", percent),
        control,
    )
    .map_err(|error| (file.clone(), format!("failed to verify archive: {error}")))?;
    ensure_success(&test_output, &file, "archive verification")?;
    control
        .wait_until_ready()
        .map_err(|error| (file.clone(), error.to_string()))?;
    cleanup.remove_on_drop = false;

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

struct IncompleteArchive {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Drop for IncompleteArchive {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn seven_zip_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn run_seven_zip(
    command: &mut Command,
    mut on_progress: impl FnMut(u8),
    control: &TaskControl,
) -> io::Result<Output> {
    control.wait_until_ready()?;
    let mut child = command
        .arg("-bsp1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped 7-Zip stdout");
    let mut stderr = child.stderr.take().expect("piped 7-Zip stderr");
    let (sender, receiver) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        read_seven_zip_progress(stdout, |percent| {
            let _ = sender.send(percent);
        })
    });
    // Drain both streams concurrently so error output cannot block archiving.
    let stderr_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });
    let mut paused = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        let state = control.snapshot();
        if state.cancelled {
            break Err(io::Error::new(io::ErrorKind::Interrupted, "Task stopped."));
        }
        if state.paused != paused {
            if let Err(error) = set_process_paused(&child, state.paused) {
                break Err(error);
            }
            paused = state.paused;
        }
        for percent in receiver.try_iter() {
            on_progress(percent);
        }
        thread::sleep(Duration::from_millis(100));
    };
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("7-Zip progress reader failed"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("7-Zip error reader failed"))?;
    if status.is_ok() {
        for percent in receiver.try_iter() {
            on_progress(percent);
        }
    }
    Ok(Output {
        status: status?,
        stdout: stdout?,
        stderr: stderr?,
    })
}

#[cfg(windows)]
fn set_process_paused(child: &std::process::Child, paused: bool) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "ntdll")]
    extern "system" {
        fn NtSuspendProcess(process: *mut std::ffi::c_void) -> i32;
        fn NtResumeProcess(process: *mut std::ffi::c_void) -> i32;
    }
    let status = unsafe {
        if paused {
            NtSuspendProcess(child.as_raw_handle())
        } else {
            NtResumeProcess(child.as_raw_handle())
        }
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "could not change 7-Zip process state: {status:#x}"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_process_paused(_child: &std::process::Child, _paused: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Pausing requires Windows.",
    ))
}

fn read_seven_zip_progress(
    reader: impl Read,
    mut on_progress: impl FnMut(u8),
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut at_start = true;
    let mut digits = 0;
    let mut percent = 0u16;
    let mut last_percent = None;
    // Progress uses carriage returns rather than newline-terminated records.
    for byte in BufReader::new(reader).bytes() {
        let byte = byte?;
        output.push(byte);
        match byte {
            b'\r' | b'\n' | b'\x08' => {
                at_start = true;
                digits = 0;
                percent = 0;
            }
            b' ' | b'\t' if at_start && digits == 0 => {}
            b'0'..=b'9' if at_start && digits < 3 => {
                percent = percent * 10 + u16::from(byte - b'0');
                digits += 1;
            }
            b'%' if at_start && digits > 0 => {
                if percent <= 100 && last_percent != Some(percent as u8) {
                    on_progress(percent as u8);
                    last_percent = Some(percent as u8);
                }
                at_start = false;
            }
            _ => at_start = false,
        }
    }
    Ok(output)
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
    percent: Option<u8>,
) {
    let _ = window.emit(
        "archive-progress",
        ProgressEvent {
            current,
            total,
            file,
            status: status.to_string(),
            message,
            percent,
        },
    );
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(TaskControl::default()))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan_videos,
            batch_archive,
            control_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn password_files_are_created_in_the_root_and_every_descendant() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "archive-tool-directory-test-{}-{nonce}",
            std::process::id()
        ));
        let nested = root.join("nested");
        let empty = nested.join("empty");
        fs::create_dir_all(&empty).unwrap();

        let result = (|| {
            let control = TaskControl::default();
            let directories = batch_directories(&root, &control).unwrap();
            assert_eq!(directories.len(), 3);
            assert!(directories.contains(&root));
            assert!(directories.contains(&nested));
            assert!(directories.contains(&empty));

            assert_eq!(
                create_password_files(&directories, "secret", "2026-11-24", &control).unwrap(),
                3
            );
            for directory in directories {
                let path = directory.join("pw_secret_2026-11-24.txt");
                assert_eq!(fs::read(path).unwrap(), b"secret");
            }
        })();

        fs::remove_dir_all(root).unwrap();
        result
    }

    #[test]
    fn progress_parser_handles_carriage_returns_and_ignores_percent_in_names() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let size = self.0.len().min(buffer.len()).min(1);
                buffer[..size].copy_from_slice(&self.0[..size]);
                self.0 = &self.0[size..];
                Ok(size)
            }
        }
        let text = b"7-Zip 26.05\r\n  0%\r 19% + 99% movie.mp4\r 19%\r101%\r42%\x08 100%\r\nEverything is Ok";
        let mut updates = Vec::new();
        let output =
            read_seven_zip_progress(OneByte(text), |percent| updates.push(percent)).unwrap();
        assert_eq!(updates, [0, 19, 42, 100]);
        assert_eq!(output, text);
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "archive-tool-progress-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn video(&self) -> PathBuf {
            let mut data = vec![0u8; 16 * 1024 * 1024];
            let mut value = 0x123456789abcdefu64;
            for chunk in data.chunks_mut(8) {
                value ^= value << 13;
                value ^= value >> 7;
                value ^= value << 17;
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            let path = self.0.join("sample 99% video.mp4");
            fs::write(&path, data).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_reports_counts_and_can_be_cancelled() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("video.mp4"), b"fixture").unwrap();
        let control = TaskControl::default();
        let mut found = 0;
        let videos = scan_videos_inner(
            fixture.0.to_string_lossy().into_owned(),
            &control,
            |_, count, _| found = count,
        )
        .unwrap();
        assert_eq!(found, 1);
        assert_eq!(videos.len(), 1);
        let result = scan_videos_inner(
            fixture.0.to_string_lossy().into_owned(),
            &control,
            |_, _, _| control.0.lock().unwrap().cancelled = true,
        );
        assert!(result.unwrap_err().contains("stopped"));
    }

    #[test]
    fn real_seven_zip_streams_progress_and_verifies_archive() {
        let fixture = Fixture::new();
        let source = fixture.video();
        let mut updates = Vec::new();
        let result = archive_one(
            &find_seven_zip().expect("7-Zip is required for this integration test"),
            &fixture.0,
            &source.to_string_lossy(),
            "fixture-only",
            false,
            &TaskControl::default(),
            |stage, percent| updates.push((stage.to_string(), percent)),
        );
        assert_eq!(result.unwrap().2, "done");
        assert!(updates
            .iter()
            .any(|(stage, percent)| stage == "processing" && *percent > 0 && *percent < 100));
        assert!(updates.iter().any(|(stage, _)| stage == "verifying"));
        assert!(source.exists());
        assert!(fixture.0.join("sample 99% video.mp4.7z").exists());
    }

    #[test]
    fn stopping_during_creation_or_verification_keeps_original_and_removes_new_archive() {
        for stop_stage in ["processing", "verifying"] {
            let fixture = Fixture::new();
            let source = fixture.video();
            let control = TaskControl::default();
            let result = archive_one(
                &find_seven_zip().unwrap(),
                &fixture.0,
                &source.to_string_lossy(),
                "fixture-only",
                true,
                &control,
                |stage, _| {
                    if stage == stop_stage {
                        control.0.lock().unwrap().cancelled = true;
                    }
                },
            );
            assert!(result.is_err());
            assert!(source.exists());
            assert!(!fixture.0.join("sample 99% video.mp4.7z").exists());
        }
    }

    #[test]
    fn task_guard_releases_task_after_cancellation() {
        let control = Arc::new(TaskControl::default());
        let task = control.start("scan").unwrap();
        assert!(control.start("archive").is_err());
        control.0.lock().unwrap().cancelled = true;
        assert_eq!(
            control.wait_until_ready().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        drop(task);
        assert!(control.start("archive").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn live_seven_zip_process_pauses_and_resumes() {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetProcessTimes(
                process: *mut std::ffi::c_void,
                created: *mut u64,
                exited: *mut u64,
                kernel: *mut u64,
                user: *mut u64,
            ) -> i32;
        }
        let fixture = Fixture::new();
        let source = fixture.video();
        let mut child = seven_zip_command(&find_seven_zip().unwrap())
            .arg("a")
            .arg("-t7z")
            .arg("-mx=5")
            .arg(fixture.0.join("pause.7z"))
            .arg(source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = || {
            set_process_paused(&child, true).unwrap();
            let cpu_time = || {
                let (mut created, mut exited, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
                assert_ne!(
                    unsafe {
                        GetProcessTimes(
                            child.as_raw_handle(),
                            &mut created,
                            &mut exited,
                            &mut kernel,
                            &mut user,
                        )
                    },
                    0
                );
                kernel + user
            };
            let before = cpu_time();
            thread::sleep(Duration::from_millis(250));
            assert_eq!(cpu_time(), before, "suspended 7-Zip must consume no CPU");
            set_process_paused(&child, false).unwrap();
            assert!(child.wait().unwrap().success());
        };
        // Always reap this test's child, including if an assertion fails.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(result));
        let _ = child.kill();
        let _ = child.wait();
        outcome.unwrap();
    }
}
