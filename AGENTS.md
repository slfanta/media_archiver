# Project workflow

This is a small Windows desktop tool. Keep changes focused and the interface compact.

For every completed revision:

1. Run the checks appropriate to the change and increment the patch version consistently in the npm, Cargo, lockfile, and Tauri manifests.
2. Build a fresh release executable and Windows NSIS installer. Keep a versioned portable build under `dist/` and copy the newest installer to `dist/latest/Batch-Media-Archiver-Setup.exe`.
3. Create one descriptive Git commit for the revision, then push it to `origin/main`. The user has authorized these routine checkpoints and pushes; proceed without repeated confirmation. If authentication is unavailable, finish the local commit and report the push blocker.
4. Keep generated binaries, downloaded toolchains, caches, and logs out of Git.

The user prefers a charcoal Ubuntu-style interface, no repeated app title or icon inside the page, and live task statistics in the native title bar with `Batch Media Archiver` at the very end. Pagination defaults to 50 and allows at most 200 files per page.

On this machine, the existing MinGW build environment is:

```powershell
$env:RUSTUP_HOME = 'E:\toolchains\rustup'
$env:CARGO_HOME = 'E:\archive_tool\.cargo-home'
$env:CARGO_TARGET_DIR = 'E:\toolchains\cargo-target'
$env:Path = 'E:\toolchains\winlibs\mingw64\bin;' + $env:Path
npm run tauri -- build --target x86_64-pc-windows-gnu --bundles nsis --ci -- --locked
```

Native progress and task-control tests require 7-Zip:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --release --target x86_64-pc-windows-gnu --features tauri/custom-protocol --locked -- --test-threads=1
```
