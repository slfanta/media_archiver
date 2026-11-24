import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import './style.css'

type VideoFile = { path: string; name: string }
type ArchiveProgress = {
  current: number
  total: number
  file: string
  status: 'processing' | 'done' | 'warning' | 'error' | 'password-files-created'
  message: string
}

const app = document.querySelector<HTMLDivElement>('#app')!
const videosByPath = new Map<string, HTMLSpanElement>()
let selectedFolder = ''
let videos: VideoFile[] = []

app.innerHTML = `
  <main class="shell">
    <header class="header">
      <div class="brand-mark" aria-hidden="true">7z</div>
      <div>
        <p class="eyebrow">VIDEO ARCHIVE WORKFLOW</p>
        <h1>Batch 7z Archiver</h1>
      </div>
    </header>

    <section class="panel source-panel" aria-labelledby="source-title">
      <div class="section-heading">
        <div>
          <p class="eyebrow">01 / SOURCE</p>
          <h2 id="source-title">Choose a folder</h2>
        </div>
        <span class="step-badge">Recursive scan</span>
      </div>
      <div class="folder-row">
        <div class="folder-display">
          <span class="folder-icon" aria-hidden="true">Folder</span>
          <span id="folder-path" class="folder-path">No folder selected</span>
        </div>
        <button id="browse-btn" class="button button-secondary" type="button">Browse</button>
        <button id="scan-btn" class="button button-primary" type="button" disabled>Scan videos</button>
      </div>
      <p class="helper">Every supported video in this folder and its subfolders will be listed before you start.</p>
    </section>

    <section class="panel options-panel" aria-labelledby="options-title">
      <div class="section-heading">
        <div>
          <p class="eyebrow">02 / OPTIONS</p>
          <h2 id="options-title">Set archive options</h2>
        </div>
      </div>
      <label class="password-label" for="password">Archive password <span class="required">Required</span></label>
      <input id="password" class="password-input" type="password" autocomplete="new-password" placeholder="Enter a password for every archive" />
      <p class="helper">Each video gets its own encrypted <code>.7z</code> archive beside the original. File names are encrypted too.</p>
      <p class="helper">At batch start, a plaintext password file is created in each folder containing videos. Invalid filename characters are replaced with underscores; the file contents remain exact.</p>
      <label class="delete-option">
        <input id="delete-originals" type="checkbox" />
        <span class="checkbox-copy">
          <strong>Delete originals after successful verification</strong>
          <span>Original files are only removed after 7-Zip confirms the archive is valid.</span>
        </span>
      </label>
    </section>

    <section class="panel queue-panel" aria-labelledby="queue-title">
      <div class="queue-heading">
        <div>
          <p class="eyebrow">03 / REVIEW</p>
          <h2 id="queue-title">Video queue <span id="count" class="count-badge">0</span></h2>
        </div>
        <button id="start-btn" class="button button-primary start-button" type="button" disabled>Start batch archive</button>
      </div>
      <div class="progress-row" role="status" aria-live="polite">
        <div class="progress-track"><div id="progress-bar" class="progress-bar"></div></div>
        <span id="progress-text" class="progress-text">Ready to scan</span>
      </div>
      <ul id="file-list" class="file-list">
        <li class="empty-state">Choose a folder, then scan to preview matching files.</li>
      </ul>
    </section>

    <section class="log-section" aria-labelledby="log-title">
      <div class="log-heading">
        <h2 id="log-title">Activity</h2>
        <span class="live-indicator"><span></span> LIVE LOG</span>
      </div>
      <div id="log" class="log" aria-live="polite"></div>
    </section>
    <footer>Uses 7-Zip installed on this computer. Archives are saved in each video's original folder.</footer>
  </main>
`

const folderPath = document.querySelector<HTMLSpanElement>('#folder-path')!
const browseBtn = document.querySelector<HTMLButtonElement>('#browse-btn')!
const scanBtn = document.querySelector<HTMLButtonElement>('#scan-btn')!
const passwordInput = document.querySelector<HTMLInputElement>('#password')!
const deleteCheckbox = document.querySelector<HTMLInputElement>('#delete-originals')!
const startBtn = document.querySelector<HTMLButtonElement>('#start-btn')!
const fileList = document.querySelector<HTMLUListElement>('#file-list')!
const progressBar = document.querySelector<HTMLDivElement>('#progress-bar')!
const progressText = document.querySelector<HTMLSpanElement>('#progress-text')!
const logEl = document.querySelector<HTMLDivElement>('#log')!
const countEl = document.querySelector<HTMLSpanElement>('#count')!

function log(message: string, type: 'info' | 'ok' | 'warn' | 'error' = 'info') {
  const line = document.createElement('div')
  line.className = `log-line log-${type}`
  line.textContent = `${new Date().toLocaleTimeString()}  ${message}`
  logEl.prepend(line)
  while (logEl.children.length > 200) logEl.lastElementChild?.remove()
}

function setBusy(busy: boolean) {
  browseBtn.disabled = busy
  scanBtn.disabled = busy || !selectedFolder
  passwordInput.disabled = busy
  deleteCheckbox.disabled = busy
  startBtn.disabled = busy || videos.length === 0
  startBtn.textContent = busy ? 'Archiving...' : 'Start batch archive'
}

function renderVideoList() {
  videosByPath.clear()
  fileList.replaceChildren()

  if (videos.length === 0) {
    const empty = document.createElement('li')
    empty.className = 'empty-state'
    empty.textContent = selectedFolder
      ? 'No supported video files found in this folder.'
      : 'Choose a folder, then scan to preview matching files.'
    fileList.appendChild(empty)
    return
  }

  for (const video of videos) {
    const item = document.createElement('li')
    item.className = 'file-item'
    const details = document.createElement('div')
    details.className = 'file-details'
    const name = document.createElement('span')
    name.className = 'file-name'
    name.textContent = video.name
    name.title = video.path
    const path = document.createElement('span')
    path.className = 'file-path'
    path.textContent = video.path
    const status = document.createElement('span')
    status.className = 'file-status'
    status.textContent = 'Pending'
    details.append(name, path)
    item.append(details, status)
    fileList.appendChild(item)
    videosByPath.set(video.path, status)
  }
}

function setFileStatus(path: string, status: string, className: string) {
  const element = videosByPath.get(path)
  if (element) {
    element.textContent = status
    element.className = `file-status ${className}`
  }
}

function updateProgress(current: number, total: number, message: string, complete: boolean) {
  const completed = complete ? current : Math.max(0, current - 1)
  progressBar.style.width = `${total === 0 ? 0 : Math.round((completed / total) * 100)}%`
  progressText.textContent = message
}

browseBtn.addEventListener('click', async () => {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      defaultPath: selectedFolder || undefined,
      title: 'Select folder containing videos',
    })
    if (typeof selected !== 'string' || !selected) return

    selectedFolder = selected
    folderPath.textContent = selected
    folderPath.title = selected
    videos = []
    countEl.textContent = '0'
    renderVideoList()
    progressBar.style.width = '0%'
    progressText.textContent = 'Ready to scan'
    setBusy(false)
    log(`Selected folder: ${selected}`)
  } catch (error) {
    log(`Could not open folder picker: ${String(error)}`, 'error')
  }
})

scanBtn.addEventListener('click', async () => {
  if (!selectedFolder) return
  videos = []
  countEl.textContent = '0'
  renderVideoList()
  progressBar.style.width = '0%'
  progressText.textContent = 'Scanning...'
  setBusy(true)
  log('Scanning folder and subfolders for videos...')
  try {
    videos = await invoke<VideoFile[]>('scan_videos', { folder: selectedFolder })
    countEl.textContent = String(videos.length)
    renderVideoList()
    startBtn.disabled = videos.length === 0
    log(`Found ${videos.length} video file${videos.length === 1 ? '' : 's'}.`, 'ok')
  } catch (error) {
    log(`Scan failed: ${String(error)}`, 'error')
  } finally {
    setBusy(false)
  }
})

startBtn.addEventListener('click', async () => {
  if (!selectedFolder || videos.length === 0) return
  if (!passwordInput.value) {
    passwordInput.focus()
    log('Enter a password before starting the batch.', 'warn')
    return
  }

  setBusy(true)
  updateProgress(0, videos.length, 'Starting batch...', false)
  log(`Starting ${videos.length} archive task(s). Delete originals: ${deleteCheckbox.checked ? 'yes' : 'no'}.`)
  let unlisten: (() => void) | undefined

  try {
    unlisten = await listen<ArchiveProgress>('archive-progress', ({ payload }) => {
      const { current, total, file, status, message } = payload
      if (status === 'password-files-created') {
        log(message, 'warn')
        return
      }
      const complete = status !== 'processing'
      updateProgress(current, total, message, complete)
      if (status === 'done') {
        setFileStatus(file, 'Verified', 'status-ok')
        log(message, 'ok')
      } else if (status === 'error') {
        setFileStatus(file, 'Failed', 'status-error')
        log(message, 'error')
      } else if (status === 'warning') {
        setFileStatus(file, 'Verified; original kept', 'status-active')
        log(message, 'warn')
      } else {
        setFileStatus(file, 'Archiving', 'status-active')
      }
    })

    const archived = await invoke<number>('batch_archive', {
      folder: selectedFolder,
      videoPaths: videos.map((video) => video.path),
      password: passwordInput.value,
      deleteOriginals: deleteCheckbox.checked,
    })
    updateProgress(videos.length, videos.length, 'Batch complete', true)
    log(`Batch complete: ${archived}/${videos.length} archive(s) created and verified.`, 'ok')
  } catch (error) {
    log(`Batch failed: ${String(error)}`, 'error')
  } finally {
    unlisten?.()
    setBusy(false)
  }
})

log('Ready. Choose a folder to begin.')
