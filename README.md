# Batch Media Archiver

A compact Windows tool for recursively finding videos and creating a password-protected 7-Zip archive beside each original.

- Live per-file archiving and verification progress, with task statistics in the window title.
- Pause/resume the active archive, stop archiving, or cancel a folder scan.
- Paginated queue: 50 files per page by default, configurable up to 200.
- Optional removal of originals after successful archive verification.

Install 7-Zip before using the app. At batch start, the app creates a **plaintext password file in the selected folder and every subfolder**. The delete-originals option starts unchecked.

## Build on Windows

Install Node.js, Rust with a Windows toolchain, and the prerequisites for Tauri 2. Then run:

```powershell
npm ci
npm run tauri -- build --bundles nsis
```

The installer is generated under the Cargo release directory in `bundle/nsis/`. Local revision builds also keep the newest installer at `dist/latest/Batch-Media-Archiver-Setup.exe`. A portable build needs its executable and `WebView2Loader.dll` in the same folder.

Run the frontend type check and production build with `npm run build`. The Rust tests exercise live 7-Zip progress, pause/resume, and cancellation using disposable files; 7-Zip must be installed.
