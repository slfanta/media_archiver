import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import { getCurrentWindow } from '@tauri-apps/api/window'
import './style.css'

type VideoFile = { path: string; name: string }
type ArchiveProgress = {
  current: number
  total: number
  file: string
  status: 'processing' | 'verifying' | 'done' | 'warning' | 'error' | 'cancelled' | 'password-files-created'
  message: string
  percent: number | null
}

type FileStatus = { label: string; className: string; percent: number }
type RenderedStatus = { label: HTMLSpanElement; progress: HTMLProgressElement }
type ScanProgress = { checked: number; found: number; path: string }

const DEFAULT_PAGE_SIZE = 50
const MAX_PAGE_SIZE = 200
const APP_NAME = 'Batch Media Archiver'

const app = document.querySelector<HTMLDivElement>('#app')!
const renderedStatuses = new Map<string, RenderedStatus>()
const statusByPath = new Map<string, FileStatus>()
let selectedFolder = ''
let videos: VideoFile[] = []
let currentPage = 1
let pageSize = DEFAULT_PAGE_SIZE
let activeTask: 'scan' | 'archive' | null = null
let isPaused = false
let stopRequested = false
let taskStartedAt = 0
let taskTimer: ReturnType<typeof setInterval> | undefined
let taskPhase = ''
let taskCurrent = 0
let taskTotal = 0
let taskPercent = 0
let verifiedFiles = 0
let finishedFiles = 0
let scannedItems = 0
let scannedVideos = 0
let lastWindowTitle = ''

app.innerHTML = `
  <main class="shell">
    <header class="header">
      <h1 class="eyebrow">VIDEO ARCHIVE WORKFLOW</h1>
    </header>

    <section class="panel source-panel" aria-labelledby="source-title">
      <div class="section-heading">
        <div>
          <h2 id="source-title">Source folder</h2>
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
    </section>

    <section class="panel options-panel" aria-labelledby="options-title">
      <h2 id="options-title" class="visually-hidden">Archive options</h2>
      <div class="options-grid">
        <div class="password-field">
          <label class="password-label" for="password">Archive password <span class="required">Required</span></label>
          <input id="password" class="password-input" type="password" autocomplete="new-password" placeholder="Password for every archive" />
        </div>
        <label class="delete-option">
          <input id="delete-originals" type="checkbox" />
          <span class="checkbox-copy">
            <strong>Delete originals after verification</strong>
            <span>Only after 7-Zip confirms each archive is valid.</span>
          </span>
        </label>
      </div>
      <details class="archive-details">
        <summary>Encrypted archives · plaintext password copies</summary>
        <p class="helper">Each video gets an encrypted <code>.7z</code> archive beside the original, including encrypted file names. A plaintext password file is created in the selected folder and every subfolder at batch start. Invalid filename characters are replaced with underscores; the file contents remain exact.</p>
      </details>
    </section>

    <section class="panel queue-panel" aria-labelledby="queue-title">
      <div class="queue-heading">
        <div>
          <h2 id="queue-title">Video queue <span id="count" class="count-badge">0</span></h2>
        </div>
        <button id="start-btn" class="button button-primary start-button" type="button" disabled>Start archive</button>
      </div>
      <div class="progress-row" role="status" aria-live="polite">
        <div class="progress-track"><div id="progress-bar" class="progress-bar"></div></div>
        <span id="progress-text" class="progress-text">Ready to scan</span>
      </div>
      <div id="task-controls" class="task-controls" hidden>
        <div id="task-stats" class="task-stats">
          <span id="task-count">0 files</span>
          <span id="task-verified">0 verified</span>
          <span id="task-elapsed">00:00 elapsed</span>
        </div>
        <button id="pause-btn" class="button button-secondary" type="button">Pause</button>
        <button id="stop-btn" class="button button-secondary" type="button">Stop</button>
      </div>
      <ul id="file-list" class="file-list">
        <li class="empty-state">Choose a folder, then scan to preview matching files.</li>
      </ul>
      <div id="pagination" class="pagination" hidden>
        <span id="page-summary" class="page-summary">No files</span>
        <div class="pagination-controls">
          <label class="page-size-label" for="page-size">Per page</label>
          <input
            id="page-size"
            class="page-size-input"
            type="number"
            min="1"
            max="200"
            step="1"
            value="50"
            inputmode="numeric"
            aria-label="Files per page, 1 to 200"
          />
          <button id="prev-page" class="button button-secondary page-button" type="button" disabled>Prev</button>
          <span id="page-indicator" class="page-indicator" aria-live="polite">Page 1 / 1</span>
          <button id="next-page" class="button button-secondary page-button" type="button" disabled>Next</button>
        </div>
      </div>
    </section>

    <details class="log-section">
      <summary class="log-heading">
        <h2 id="log-title">Activity</h2>
        <span class="live-indicator"><span></span> LIVE LOG</span>
      </summary>
      <div id="log" class="log" aria-live="polite"></div>
    </details>
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
const paginationEl = document.querySelector<HTMLDivElement>('#pagination')!
const pageSummary = document.querySelector<HTMLSpanElement>('#page-summary')!
const pageSizeInput = document.querySelector<HTMLInputElement>('#page-size')!
const pageIndicator = document.querySelector<HTMLSpanElement>('#page-indicator')!
const prevPageBtn = document.querySelector<HTMLButtonElement>('#prev-page')!
const nextPageBtn = document.querySelector<HTMLButtonElement>('#next-page')!
const taskControls = document.querySelector<HTMLDivElement>('#task-controls')!
const pauseBtn = document.querySelector<HTMLButtonElement>('#pause-btn')!
const stopBtn = document.querySelector<HTMLButtonElement>('#stop-btn')!
const taskCount = document.querySelector<HTMLSpanElement>('#task-count')!
const taskVerified = document.querySelector<HTMLSpanElement>('#task-verified')!
const taskElapsed = document.querySelector<HTMLSpanElement>('#task-elapsed')!

function setWindowTitle(details = '') {
  const title = details ? `${details} | ${APP_NAME}` : APP_NAME
  if (title === lastWindowTitle) return
  lastWindowTitle = title
  void getCurrentWindow().setTitle(title).catch(() => { lastWindowTitle = '' })
}

function refreshTaskStats() {
  if (!activeTask) return
  const seconds = Math.max(0, Math.floor((Date.now() - taskStartedAt) / 1000))
  const elapsed = `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`
  const phase = stopRequested ? 'Stopping' : isPaused ? 'Paused' : taskPhase
  taskElapsed.textContent = `${elapsed} elapsed`
  if (activeTask === 'scan') {
    taskCount.textContent = `${scannedItems.toLocaleString()} checked`
    taskVerified.textContent = `${scannedVideos.toLocaleString()} videos`
    setWindowTitle(`${phase} · ${taskCount.textContent} · ${taskVerified.textContent} · ${elapsed} elapsed`)
  } else {
    taskCount.textContent = `${finishedFiles}/${taskTotal} finished`
    taskVerified.textContent = `${verifiedFiles} verified`
    setWindowTitle(`${phase} ${taskPercent}% · File ${taskCurrent}/${taskTotal} · ${verifiedFiles} verified · ${elapsed} elapsed`)
  }
}

function log(message: string, type: 'info' | 'ok' | 'warn' | 'error' = 'info') {
  const line = document.createElement('div')
  line.className = `log-line log-${type}`
  line.textContent = `${new Date().toLocaleTimeString()}  ${message}`
  logEl.prepend(line)
  while (logEl.children.length > 200) logEl.lastElementChild?.remove()
}

function setBusy(busy: boolean, task: 'scan' | 'archive' = 'archive') {
  if (taskTimer) clearInterval(taskTimer)
  taskTimer = undefined
  if (!busy) setWindowTitle(progressText.textContent ?? '')
  activeTask = busy ? task : null
  isPaused = false
  stopRequested = false
  browseBtn.disabled = busy
  scanBtn.disabled = busy || !selectedFolder
  passwordInput.disabled = busy
  deleteCheckbox.disabled = busy
  startBtn.disabled = busy || videos.length === 0
  startBtn.textContent = busy && task === 'archive' ? 'Archiving...' : 'Start archive'
  scanBtn.textContent = busy && task === 'scan' ? 'Scanning...' : 'Scan videos'
  taskControls.hidden = !busy
  pauseBtn.hidden = task === 'scan'
  pauseBtn.disabled = false
  pauseBtn.textContent = 'Pause'
  stopBtn.disabled = false
  stopBtn.textContent = task === 'scan' ? 'Stop scan' : 'Stop archive'
  progressBar.classList.toggle('is-scanning', busy && task === 'scan')
  if (busy) {
    taskStartedAt = Date.now()
    taskPhase = task === 'scan' ? 'Scanning' : 'Preparing'
    taskCurrent = 0
    taskTotal = videos.length
    taskPercent = 0
    verifiedFiles = 0
    finishedFiles = 0
    scannedItems = 0
    scannedVideos = 0
    refreshTaskStats()
    taskTimer = setInterval(refreshTaskStats, 1000)
  }
}

pauseBtn.addEventListener('click', async () => {
  if (activeTask !== 'archive' || stopRequested) return
  pauseBtn.disabled = true
  try {
    const paused = !isPaused
    await invoke('control_task', { action: paused ? 'pause' : 'resume' })
    isPaused = paused
    pauseBtn.textContent = paused ? 'Resume' : 'Pause'
    progressText.textContent = paused ? 'Archive paused' : 'Resuming archive...'
    log(paused ? 'Archive paused.' : 'Archive resumed.')
    refreshTaskStats()
  } catch (error) {
    log(`Could not change archive state: ${String(error)}`, 'error')
  } finally {
    pauseBtn.disabled = false
  }
})

stopBtn.addEventListener('click', async () => {
  if (!activeTask || stopRequested) return
  stopBtn.disabled = true
  try {
    await invoke('control_task', { action: 'stop' })
    stopRequested = true
    isPaused = false
    pauseBtn.disabled = true
    progressText.textContent = activeTask === 'scan' ? 'Stopping scan...' : 'Stopping archive...'
    log('Stop requested.', 'warn')
    refreshTaskStats()
  } catch (error) {
    stopBtn.disabled = false
    log(`Could not stop task: ${String(error)}`, 'error')
  }
})

function totalPages() {
  return Math.max(1, Math.ceil(videos.length / pageSize))
}

function clampCurrentPage() {
  const pages = totalPages()
  if (currentPage > pages) currentPage = pages
  if (currentPage < 1) currentPage = 1
}

function renderVideoList() {
  renderedStatuses.clear()
  fileList.replaceChildren()
  clampCurrentPage()

  if (videos.length === 0) {
    const empty = document.createElement('li')
    empty.className = 'empty-state'
    empty.textContent = selectedFolder
      ? 'No supported video files found in this folder.'
      : 'Choose a folder, then scan to preview matching files.'
    fileList.appendChild(empty)
    renderPagination()
    return
  }

  const startIndex = (currentPage - 1) * pageSize
  const pageVideos = videos.slice(startIndex, startIndex + pageSize)
  for (const video of pageVideos) {
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
    const progress = document.createElement('progress')
    progress.max = 100
    progress.setAttribute('aria-label', `Archive progress for ${video.name}`)
    const progressInfo = document.createElement('div')
    progressInfo.className = 'file-progress-info'
    progressInfo.append(status, progress)
    details.append(name, path)
    item.append(details, progressInfo)
    fileList.appendChild(item)
    renderedStatuses.set(video.path, { label: status, progress })
    const saved = statusByPath.get(video.path)
    renderFileStatus(video.path, saved ?? { label: 'Pending', className: '', percent: 0 })
  }

  renderPagination()
}

function renderPagination() {
  const total = videos.length
  paginationEl.hidden = total === 0
  if (total === 0) {
    pageSummary.textContent = 'No files'
    pageIndicator.textContent = 'Page 1 / 1'
    prevPageBtn.disabled = true
    nextPageBtn.disabled = true
    return
  }

  const pages = totalPages()
  const first = (currentPage - 1) * pageSize + 1
  const last = Math.min(total, currentPage * pageSize)
  pageSummary.textContent = `Showing ${first.toLocaleString()}–${last.toLocaleString()} of ${total.toLocaleString()}`
  pageIndicator.textContent = `Page ${currentPage} / ${pages}`
  prevPageBtn.disabled = currentPage <= 1
  nextPageBtn.disabled = currentPage >= pages
}

function renderFileStatus(path: string, status: FileStatus) {
  const element = renderedStatuses.get(path)
  if (element) {
    element.label.textContent = status.label
    element.label.className = `file-status ${status.className}`
    element.progress.className = `file-progress ${status.className}`
    element.progress.value = status.percent
  }
}

function setFileStatus(path: string, label: string, className: string, percent?: number | null) {
  const savedPercent = statusByPath.get(path)?.percent ?? 0
  const status = { label, className, percent: Math.max(0, Math.min(100, percent ?? savedPercent)) }
  statusByPath.set(path, status)
  renderFileStatus(path, status)
}

function applyPageSize(raw: string) {
  const parsed = Math.floor(Number(raw))
  if (!Number.isFinite(parsed) || parsed < 1) {
    pageSizeInput.value = String(pageSize)
    return
  }

  pageSize = Math.min(MAX_PAGE_SIZE, parsed)
  pageSizeInput.value = String(pageSize)
  currentPage = 1
  renderVideoList()
}

prevPageBtn.addEventListener('click', () => {
  if (currentPage <= 1) return
  currentPage -= 1
  renderVideoList()
})

nextPageBtn.addEventListener('click', () => {
  if (currentPage >= totalPages()) return
  currentPage += 1
  renderVideoList()
})

pageSizeInput.addEventListener('change', () => applyPageSize(pageSizeInput.value))

function updateProgress(current: number, total: number, message: string, complete: boolean, fraction = 0) {
  const completed = complete ? current : Math.max(0, current - 1) + fraction
  progressBar.style.width = `${total === 0 ? 0 : Math.round((completed / total) * 100)}%`
  if (!isPaused && !stopRequested) progressText.textContent = message
  progressText.title = message
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
    statusByPath.clear()
    currentPage = 1
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
  statusByPath.clear()
  currentPage = 1
  countEl.textContent = '0'
  renderVideoList()
  progressBar.style.width = '0%'
  progressText.textContent = 'Scanning...'
  setBusy(true, 'scan')
  log('Scanning folder and subfolders for videos...')
  let unlisten: (() => void) | undefined
  try {
    unlisten = await listen<ScanProgress>('scan-progress', ({ payload }) => {
      scannedItems = payload.checked
      scannedVideos = payload.found
      refreshTaskStats()
      if (!stopRequested) {
        progressText.textContent = `Scanning: ${payload.checked.toLocaleString()} checked, ${payload.found.toLocaleString()} videos`
        progressText.title = payload.path
      }
    })
    videos = await invoke<VideoFile[]>('scan_videos', { folder: selectedFolder })
    countEl.textContent = String(videos.length)
    renderVideoList()
    startBtn.disabled = videos.length === 0
    progressText.textContent = `Scan complete: ${videos.length.toLocaleString()} videos`
    log(`Found ${videos.length} video file${videos.length === 1 ? '' : 's'}.`, 'ok')
  } catch (error) {
    progressText.textContent = stopRequested ? 'Scan stopped' : 'Scan failed'
    log(`Scan ${stopRequested ? 'stopped' : 'failed'}: ${String(error)}`, stopRequested ? 'warn' : 'error')
  } finally {
    unlisten?.()
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

  statusByPath.clear()
  currentPage = 1
  renderVideoList()
  setBusy(true)
  updateProgress(0, videos.length, 'Starting batch...', false)
  log(`Starting ${videos.length} archive task(s). Delete originals: ${deleteCheckbox.checked ? 'yes' : 'no'}.`)
  let unlisten: (() => void) | undefined

  try {
    unlisten = await listen<ArchiveProgress>('archive-progress', ({ payload }) => {
      const { current, total, file, status, message, percent } = payload
      if (status === 'password-files-created') {
        log(message, 'warn')
        return
      }
      const complete = status === 'done' || status === 'warning' || status === 'error'
      taskPhase = status === 'verifying' ? 'Verifying' : 'Archiving'
      taskCurrent = current
      taskTotal = total
      taskPercent = percent ?? taskPercent
      finishedFiles = complete ? current : current - 1
      if (status === 'done' || status === 'warning') verifiedFiles += 1
      refreshTaskStats()
      const fraction = status === 'processing' ? (percent ?? 0) / 100 * 0.9
        : status === 'verifying' ? 0.9 + (percent ?? 0) / 100 * 0.1 : 0
      updateProgress(current, total, message, complete, fraction)
      if (status === 'done') {
        setFileStatus(file, 'Verified', 'status-ok', 100)
        log(message, 'ok')
      } else if (status === 'error') {
        setFileStatus(file, 'Failed', 'status-error')
        log(message, 'error')
      } else if (status === 'warning') {
        setFileStatus(file, 'Verified; original kept', 'status-active', 100)
        log(message, 'warn')
      } else if (status === 'cancelled') {
        setFileStatus(file, 'Stopped', 'status-active')
      } else {
        setFileStatus(file, `${status === 'verifying' ? 'Verifying' : 'Archiving'} ${percent ?? 0}%`, 'status-active', percent)
      }
    })

    const archived = await invoke<number>('batch_archive', {
      folder: selectedFolder,
      videoPaths: videos.map((video) => video.path),
      password: passwordInput.value,
      deleteOriginals: deleteCheckbox.checked,
    })
    stopRequested = false
    isPaused = false
    updateProgress(videos.length, videos.length, `Batch complete: ${archived}/${videos.length} verified`, true)
    log(`Batch complete: ${archived}/${videos.length} archive(s) created and verified.`, 'ok')
  } catch (error) {
    progressText.textContent = stopRequested ? 'Archive stopped' : 'Batch failed'
    log(`Batch ${stopRequested ? 'stopped' : 'failed'}: ${String(error)}`, stopRequested ? 'warn' : 'error')
  } finally {
    unlisten?.()
    setBusy(false)
  }
})

renderVideoList()
setWindowTitle()
log('Ready. Choose a folder to begin.')
