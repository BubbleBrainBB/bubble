import { registerDesignIpc } from './design/ipc';
import { desktopDataProfile } from './data-environment';
import { setupSessionWindowsIPC, sessionWindows } from './ipc/session-windows';
import { initializeDesktopDefaults } from './libs/desktop-defaults';
import { installRendererRecovery } from './libs/renderer-recovery';
import { app, BrowserWindow, Menu, dialog, shell, ipcMain, nativeTheme, session } from 'electron';
import { queueSessionLink } from './ipc/session-links';
import { autoUpdater } from 'electron-updater';
import * as fs from 'fs';
import * as path from 'path';
import { setupIPCHandlers, cleanup } from './ipc-handlers';
import { registerBrowserIpc, disposeBrowserIpc } from './browser-ipc';
import { registerDesignModeIpc } from './design-mode-ipc';
import { browserManager } from './browserManager';
import {
  assertTrustedIpcSender,
  isDev,
  getPreloadPath,
  getUIPath,
  DEV_SERVER_URL,
  ipcMainHandle,
  ipcMainOn,
} from './util';
import { ensureShellEnvironment } from './libs/shell-environment';
import { listRunningSessions } from './libs/session-store';
import { writeProjectEditorDraftSync } from './libs/editor-draft-flush';
import type { AppUpdateStatus } from '../shared/types';
import { guardWidgetFrameNavigation, installWidgetProtocol, registerWidgetIpc, registerWidgetSchemePrivileged } from './widget-protocol';

// Generative-UI widgets load from bubble-widget:// (see widget-protocol.ts);
// a privileged scheme has to be declared before the app is ready.
registerWidgetSchemePrivileged();
app.on('web-contents-created', (_event, contents) => guardWidgetFrameNavigation(contents));

// close 确认用：数据库未初始化等异常一律按 0 处理，绝不阻塞关窗
function countRunningSessionsSafe(): number {
  try {
    return listRunningSessions().length;
  } catch {
    return 0;
  }
}

// 修复打包后的环境变量问题（macOS/Linux GUI 应用无法继承 shell 的环境变量）。
// 细节见 src/electron/libs/shell-environment.ts：使用 interactive + login shell 抓 env
// （让 .zshrc / .bashrc 里 nvm、volta、fnm 等 CLI 管理器生效），失败时用基于磁盘存在性的
// 常见路径兜底（含 nvm/fnm/asdf 各自的 node bin 目录）。
ensureShellEnvironment();

initializeDesktopDefaults();

let mainWindow: BrowserWindow | null = null;
// Queue cold-start links until the renderer requests its initial session list.
app.on('open-url', (event, url) => {
  if (queueSessionLink(url)) {
    event.preventDefault();
    if (app.isReady()) showMainWindow();
  }
});
for (const arg of process.argv) queueSessionLink(arg);
if (app.isPackaged && desktopDataProfile.profile === 'production') {
  if (!app.requestSingleInstanceLock()) app.quit();
  app.on('second-instance', (_event, argv) => {
    for (const arg of argv) queueSessionLink(arg);
    showMainWindow();
  });
  app.setAsDefaultProtocolClient('bubble-desktop');
}
let updaterInitialized = false;
let devFileWatcher: fs.FSWatcher | null = null;
let updateCheckStarted = false;
let pendingManualUpdateCheck = false;
let latestUpdateStatus: AppUpdateStatus = {
  available: false,
  version: null,
  autoDetected: false,
};
let latestUiResumeState: import('../shared/types').UiResumeState | null = null;
let isQuitting = false;
let allowCloseAfterProjectEditorFlush = false;
let projectEditorCloseFlushInProgress = false;

// Mirror the current unsaved editor content in the main process so close/quit
// can write it synchronously without depending on late renderer IPC.
type PendingProjectEditorDraft = {
  cwd: string;
  filePath: string;
  content: string;
};
let pendingProjectEditorDraft: PendingProjectEditorDraft | null = null;
let handledTerminationSignal = false;

function setPendingProjectEditorDraft(draft: PendingProjectEditorDraft | null): void {
  pendingProjectEditorDraft =
    draft && typeof draft.filePath === 'string' && draft.filePath.length > 0
      ? { cwd: draft.cwd || '', filePath: draft.filePath, content: draft.content ?? '' }
      : null;
}

// Synchronous close/quit fallback. Only writes existing editable text files
// inside the current project root.
function flushPendingProjectEditorDraftSync(suppliedDraft?: PendingProjectEditorDraft | null): void {
  const draft = suppliedDraft === undefined ? pendingProjectEditorDraft : suppliedDraft;
  if (!draft) return;
  if (suppliedDraft === undefined) pendingProjectEditorDraft = null;
  try {
    writeProjectEditorDraftSync(draft);
  } catch (error) {
    console.warn('[ProjectEditor] Failed to flush pending draft synchronously:', error);
  }
}

function flushPendingProjectEditorDraftForTermination(signal: NodeJS.Signals): void {
  if (handledTerminationSignal) return;
  handledTerminationSignal = true;
  isQuitting = true;
  logDevLifecycle('process.signal', { signal });
  flushPendingProjectEditorDraftSync();
  if (mainWindow && !mainWindow.isDestroyed()) {
    saveWindowState(mainWindow);
  }
  if (latestUiResumeState) {
    saveUiResumeState(latestUiResumeState);
  }
  try {
    devFileWatcher?.close();
  } catch {
    // ignore cleanup failures during termination
  }
  devFileWatcher = null;
  disposeBrowserIpc();
  cleanup();
  process.exit(0);
}

process.once('SIGINT', flushPendingProjectEditorDraftForTermination);
process.once('SIGTERM', flushPendingProjectEditorDraftForTermination);

const RELEASES_URL = 'https://github.com/DylanDDeng/bubble-cowork/releases';
const PROJECT_EDITOR_FLUSH_REQUEST_CHANNEL = 'project-editor-flush-request';
const PROJECT_EDITOR_FLUSH_RESPONSE_CHANNEL = 'project-editor-flush-response';

type ProjectEditorFlushResponse = {
  requestId?: string;
  ok?: boolean;
  message?: string;
};

function requestProjectEditorFlush(
  win: BrowserWindow,
  timeoutMs = 5000
): Promise<{ ok: boolean; message?: string }> {
  if (win.isDestroyed() || win.webContents.isDestroyed()) {
    return Promise.resolve({ ok: true });
  }

  const requestId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;

  return new Promise((resolve) => {
    let settled = false;
    const finish = (result: { ok: boolean; message?: string }) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeoutId);
      ipcMain.removeListener(PROJECT_EDITOR_FLUSH_RESPONSE_CHANNEL, handleResponse);
      resolve(result);
    };
    const handleResponse = (event: Electron.IpcMainEvent, response: ProjectEditorFlushResponse) => {
      try {
        assertTrustedIpcSender(event);
      } catch {
        return;
      }
      if (event.sender !== win.webContents) return;
      if (!response || response.requestId !== requestId) return;
      finish({
        ok: response.ok !== false,
        message: response.message,
      });
    };
    const timeoutId = setTimeout(() => {
      finish({ ok: false, message: 'Timed out while saving the project editor before closing.' });
    }, timeoutMs);

    ipcMain.on(PROJECT_EDITOR_FLUSH_RESPONSE_CHANNEL, handleResponse);

    try {
      win.webContents.send(PROJECT_EDITOR_FLUSH_REQUEST_CHANNEL, { requestId });
    } catch (error) {
      finish({
        ok: false,
        message: error instanceof Error ? error.message : String(error),
      });
    }
  });
}

function getMainWindowBackgroundColor(): string {
  return nativeTheme.shouldUseDarkColors ? '#111214' : '#ffffff';
}

function getWindowShellState(win: BrowserWindow): { rounded: boolean } {
  return {
    rounded:
      process.platform === 'darwin' &&
      !win.isMaximized() &&
      !win.isFullScreen() &&
      !win.isSimpleFullScreen(),
  };
}

function emitWindowShellState(win: BrowserWindow): void {
  if (win.isDestroyed()) {
    return;
  }

  win.webContents.send('window-shell-state', getWindowShellState(win));
}

function registerWindowShellState(win: BrowserWindow): void {
  const emit = () => emitWindowShellState(win);

  win.on('maximize', emit);
  win.on('unmaximize', emit);
  win.on('enter-full-screen', emit);
  win.on('leave-full-screen', emit);
  win.webContents.on('did-finish-load', emit);
}

function logDevLifecycle(event: string, details?: unknown): void {
  if (!isDev()) {
    return;
  }

  if (details === undefined) {
    console.log(`[Dev Lifecycle] ${event}`);
    return;
  }

  console.log(`[Dev Lifecycle] ${event}`, details);
}

function shouldAutoOpenDevTools(): boolean {
  return process.env.AEGIS_OPEN_DEVTOOLS === '1';
}

function stripSimpleHtml(input: string): string {
  return input
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<\/p>/gi, '\n\n')
    .replace(/<[^>]+>/g, '')
    .trim();
}

function normalizeReleaseNotes(
  releaseNotes: unknown
): string | null {
  if (typeof releaseNotes === 'string') {
    const normalized = stripSimpleHtml(releaseNotes).trim();
    return normalized.length > 0 ? normalized : null;
  }

  if (Array.isArray(releaseNotes)) {
    const sections = releaseNotes
      .map((entry) => {
        if (!entry || typeof entry !== 'object') {
          return null;
        }

        const version = typeof (entry as { version?: unknown }).version === 'string'
          ? (entry as { version?: string }).version
          : null;
        const note = typeof (entry as { note?: unknown }).note === 'string'
          ? stripSimpleHtml((entry as { note?: string }).note || '')
          : '';

        if (!note.trim()) {
          return null;
        }

        return version ? `Version ${version}\n${note.trim()}` : note.trim();
      })
      .filter((value): value is string => Boolean(value && value.trim()));

    return sections.length > 0 ? sections.join('\n\n') : null;
  }

  return null;
}

function buildUpdateDetail(info: { releaseNotes?: unknown }): string {
  const notes = normalizeReleaseNotes(info.releaseNotes);
  if (!notes) {
    return 'This build uses manual updates. Open the GitHub Releases page to download the latest package.';
  }

  return `${notes}\n\nOpen the GitHub Releases page to download the latest package.`;
}

// 窗口状态持久化
interface WindowState {
  x?: number;
  y?: number;
  width: number;
  height: number;
  isMaximized?: boolean;
}

function getWindowStateFile(): string {
  return path.join(app.getPath('userData'), 'window-state.json');
}

function getUiResumeStateFile(): string {
  return path.join(app.getPath('userData'), 'ui-resume-state.json');
}

function loadUiResumeState(): import('../shared/types').UiResumeState | null {
  try {
    const file = getUiResumeStateFile();
    if (!fs.existsSync(file)) {
      return null;
    }
    return JSON.parse(fs.readFileSync(file, 'utf-8')) as import('../shared/types').UiResumeState;
  } catch {
    return null;
  }
}

function saveUiResumeState(state: import('../shared/types').UiResumeState): void {
  latestUiResumeState = state;
  try {
    fs.writeFileSync(getUiResumeStateFile(), JSON.stringify(state));
  } catch {
    // ignore
  }
}

function clearUiResumeState(): void {
  latestUiResumeState = null;
  try {
    const file = getUiResumeStateFile();
    if (fs.existsSync(file)) {
      fs.unlinkSync(file);
    }
  } catch {
    // ignore
  }
}

// Renderer key-value state persisted in userData instead of localStorage.
// localStorage is scoped to the page origin, which flips between the dev
// server (http://127.0.0.1:<port>) and the dist-react fallback (file://), so
// anything kept there — onboarding flag, draft sessions — silently "resets"
// whenever the origin changes. This store is origin-independent.
let rendererStateCache: Record<string, string> | null = null;
let rendererStateWriteTimer: NodeJS.Timeout | null = null;

function getRendererStateFile(): string {
  return path.join(app.getPath('userData'), 'renderer-state.json');
}

function loadRendererState(): Record<string, string> {
  if (rendererStateCache) {
    return rendererStateCache;
  }
  try {
    const file = getRendererStateFile();
    rendererStateCache = fs.existsSync(file)
      ? (JSON.parse(fs.readFileSync(file, 'utf-8')) as Record<string, string>)
      : {};
  } catch {
    rendererStateCache = {};
  }
  return rendererStateCache;
}

function flushRendererState(): void {
  if (rendererStateWriteTimer) {
    clearTimeout(rendererStateWriteTimer);
    rendererStateWriteTimer = null;
  }
  if (!rendererStateCache) {
    return;
  }
  try {
    const file = getRendererStateFile();
    const tmp = `${file}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(rendererStateCache));
    fs.renameSync(tmp, file);
  } catch {
    // ignore — next write retries
  }
}

function scheduleRendererStateWrite(): void {
  if (rendererStateWriteTimer) {
    return;
  }
  rendererStateWriteTimer = setTimeout(() => {
    rendererStateWriteTimer = null;
    flushRendererState();
  }, 150);
}

function loadWindowState(): WindowState {
  try {
    const windowStateFile = getWindowStateFile();
    if (fs.existsSync(windowStateFile)) {
      return JSON.parse(fs.readFileSync(windowStateFile, 'utf-8'));
    }
  } catch {
    // 忽略错误，使用默认值
  }
  return { width: 1200, height: 800 };
}

function saveWindowState(win: BrowserWindow): void {
  try {
    const windowStateFile = getWindowStateFile();
    const state: WindowState = win.isMaximized()
      ? { ...loadWindowState(), isMaximized: true }
      : { ...win.getBounds(), isMaximized: false };
    fs.writeFileSync(windowStateFile, JSON.stringify(state));
  } catch {
    // 忽略保存错误
  }
}

async function waitForUiFile(filePath: string, timeoutMs = 15000): Promise<void> {
  const startedAt = Date.now();
  while (!fs.existsSync(filePath)) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error(`UI file not found after ${timeoutMs}ms: ${filePath}`);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
}

async function waitForDevServer(url: string, timeoutMs = 15000): Promise<boolean> {
  const startedAt = Date.now();

  while (Date.now() - startedAt <= timeoutMs) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);

    try {
      const response = await fetch(url, {
        method: 'GET',
        cache: 'no-store',
        signal: controller.signal,
      });
      if (response.ok) {
        clearTimeout(timeout);
        return true;
      }
    } catch {
      // keep waiting
    } finally {
      clearTimeout(timeout);
    }

    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  return false;
}

async function loadDistFallbackUi(win: BrowserWindow): Promise<void> {
  try {
    const uiPath = getUIPath();
    await waitForUiFile(uiPath);
    if (win.isDestroyed()) {
      return;
    }
    await win.loadFile(uiPath);
    startDevFileReloadWatcher(win);
  } catch (fallbackError) {
    console.error('[Dev] Failed to load dist-react fallback UI:', fallbackError);
  }
}

async function loadDevUi(win: BrowserWindow): Promise<void> {
  if (!process.env.BUBBLE_DESKTOP_DEV_SERVER) {
    await loadDistFallbackUi(win);
    return;
  }
  const devServerReady = await waitForDevServer(DEV_SERVER_URL, 10000);

  if (!devServerReady) {
    console.warn('[Dev] Vite dev server not reachable in time, falling back to dist-react');
    await loadDistFallbackUi(win);
    return;
  }

  try {
    if (win.isDestroyed()) {
      return;
    }
    await win.loadURL(DEV_SERVER_URL);
  } catch (error) {
    console.warn('[Dev] Failed to load Vite dev server, falling back to dist-react:', error);
    await loadDistFallbackUi(win);
  }
}

function startDevFileReloadWatcher(win: BrowserWindow): void {
  if (devFileWatcher) {
    return;
  }

  const distDir = path.join(app.getAppPath(), 'dist-react');
  if (!fs.existsSync(distDir)) {
    return;
  }

  let reloadTimer: NodeJS.Timeout | null = null;

  try {
    devFileWatcher = fs.watch(distDir, { recursive: true }, () => {
      if (reloadTimer) {
        clearTimeout(reloadTimer);
      }
      reloadTimer = setTimeout(() => {
        if (!win.isDestroyed()) {
          win.webContents.reloadIgnoringCache();
        }
      }, 150);
    });
  } catch (error) {
    console.warn('[Dev] Failed to watch dist-react for reload:', error);
  }
}

function protectRenderer(win: BrowserWindow): void {
  installRendererRecovery(win, {
    isQuitting: () => isQuitting,
    logDirectory: path.join(app.getPath('userData'), 'logs'),
    sampleMemory: () => {
      const pid = win.webContents.getOSProcessId();
      const memory = app.getAppMetrics().find(metric => metric.pid === pid)?.memory;
      return memory && { workingSetKB: memory.workingSetSize, peakWorkingSetKB: memory.peakWorkingSetSize };
    },
    offerRetry: async () => {
      const { response } = await dialog.showMessageBox(win, {
        type: 'error',
        title: 'Bubble display recovery',
        message: 'The display stopped unexpectedly several times.',
        detail: 'Background agents have not been stopped. You can reload the display without restarting Bubble.',
        buttons: ['Reload display', 'Keep background tasks running'],
        defaultId: 0,
        cancelId: 1,
        noLink: true,
      });
      return response === 0;
    },
  });
}

function createWindow(): void {
  const windowState = loadWindowState();

  mainWindow = new BrowserWindow({
    x: windowState.x,
    y: windowState.y,
    width: windowState.width,
    height: windowState.height,
    minWidth: 560,
    minHeight: 600,
    title: desktopDataProfile.appName,
    titleBarStyle: 'hidden',
    trafficLightPosition: { x: 15, y: 15 },
    ...(process.platform === 'darwin' ? { roundedCorners: true } : {}),
    backgroundColor: getMainWindowBackgroundColor(),
    webPreferences: {
      preload: getPreloadPath(),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      // Keep debounce timers running in the background so editor autosave
      // still fires when the user switches away and immediately quits.
      backgroundThrottling: false,
    },
  });

  // 恢复最大化状态
  if (windowState.isMaximized) {
    mainWindow.maximize();
  }

  // Links from the renderer (target=_blank / window.open) must never spawn a
  // bare Electron popup — hand them to the system default browser instead.
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (queueSessionLink(url)) return { action: 'deny' };
    if (/^https?:/i.test(url)) {
      void shell.openExternal(url);
    }
    return { action: 'deny' };
  });
  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (queueSessionLink(url)) { event.preventDefault(); return; }
    // Keep the SPA shell in place; dragged/中键 links go to the system browser.
    if (/^https?:/i.test(url) && !(isDev() && url.startsWith(DEV_SERVER_URL))) {
      event.preventDefault();
      void shell.openExternal(url);
    }
  });

  // 设置 IPC 处理器
  setupIPCHandlers(mainWindow);
  registerBrowserIpc(mainWindow);
  registerDesignModeIpc(mainWindow);
  registerDesignIpc();
  registerWindowShellState(mainWindow);
  protectRenderer(mainWindow);

  // 后台预热 Claude 运行时状态缓存，避免首次发消息时等待 1-5 秒


  if (isDev()) {
    const webContents = mainWindow.webContents;
    webContents.on('preload-error', (_event, preloadPath, error) => {
      console.error('[Dev] Preload error:', preloadPath, error);
    });
    webContents.on('did-fail-load', (_event, errorCode, errorDescription, validatedURL, isMainFrame) => {
      if (!isMainFrame || errorCode === -3) {
        return;
      }
      console.error('[Dev] Renderer load failed:', {
        errorCode,
        errorDescription,
        validatedURL,
      });
      if (!mainWindow || mainWindow.isDestroyed()) {
        return;
      }
      void loadDistFallbackUi(mainWindow);
    });
    webContents.on('unresponsive', () => {
      logDevLifecycle('webContents.unresponsive');
    });
    webContents.on('responsive', () => {
      logDevLifecycle('webContents.responsive');
    });
  }

  // 加载页面
  if (isDev()) {
    void loadDevUi(mainWindow);
    if (shouldAutoOpenDevTools()) {
      mainWindow.webContents.openDevTools();
    }
  } else {
    mainWindow.loadFile(getUIPath());
  }

  // 保存窗口状态
  mainWindow.on('close', (event) => {
    const win = mainWindow;
    logDevLifecycle('mainWindow.close', {
      isQuitting,
      visible: mainWindow?.isVisible() ?? false,
      destroyed: mainWindow?.isDestroyed() ?? false,
    });
    if (!win) {
      return;
    }

    saveWindowState(win);
    if (latestUiResumeState) {
      saveUiResumeState(latestUiResumeState);
    }

    if (allowCloseAfterProjectEditorFlush) {
      allowCloseAfterProjectEditorFlush = false;
      return;
    }

    // Write the mirrored draft before the async renderer flush. The second
    // allowed close must not flush again because the mirror may be stale after
    // the renderer has already saved newer content.
    flushPendingProjectEditorDraftSync();

    event.preventDefault();
    if (projectEditorCloseFlushInProgress) {
      return;
    }

    projectEditorCloseFlushInProgress = true;
    void (async () => {
      const flushResult = await requestProjectEditorFlush(win);
      projectEditorCloseFlushInProgress = false;

      if (!flushResult.ok) {
        console.warn('[ProjectEditor] Failed to flush before close:', flushResult.message);
      }

      if (win.isDestroyed()) {
        return;
      }

      saveWindowState(win);
      if (latestUiResumeState) {
        saveUiResumeState(latestUiResumeState);
      }

      if (process.platform === 'darwin' && !isQuitting) {
        win.hide();
        return;
      }

      // Windows/Linux 上关窗即退出（window-all-closed → app.quit），会杀掉所有
      // 后台 runner——决策点 11：首版仅 macOS 支持"关窗照跑"，非 macOS 在有
      // 任务运行时弹确认，避免静默丢工作。
      if (process.platform !== 'darwin' && !isQuitting) {
        const runningCount = countRunningSessionsSafe();
        if (runningCount > 0) {
          const choice = await dialog.showMessageBox(win, {
            type: 'warning',
            buttons: ['Quit anyway', 'Keep running'],
            defaultId: 1,
            cancelId: 1,
            message: `${runningCount} agent run${runningCount > 1 ? 's are' : ' is'} still in progress.`,
            detail: 'Closing the window quits Bubble on this platform and stops all running agents.',
          });
          if (choice.response === 1) {
            return;
          }
        }
      }

      allowCloseAfterProjectEditorFlush = true;
      win.close();
    })();
  });

  mainWindow.on('closed', () => {
    logDevLifecycle('mainWindow.closed');
    mainWindow = null;
  });
}

function showMainWindow(): void {
  if (!mainWindow || mainWindow.isDestroyed()) {
    createWindow();
    return;
  }

  if (mainWindow.isMinimized()) {
    mainWindow.restore();
  }

  if (!mainWindow.isVisible()) {
    mainWindow.show();
  }

  mainWindow.focus();
}

function broadcastUpdateStatus(): void {
  if (!mainWindow || mainWindow.isDestroyed() || mainWindow.webContents.isDestroyed()) {
    return;
  }

  mainWindow.webContents.send(
    'server-event',
    JSON.stringify({
      type: 'app.update',
      payload: latestUpdateStatus,
    } satisfies import('../shared/types').ServerEvent)
  );
}

function setupAutoUpdater(): void {
  // Local development builds have no update feed.
}

function checkForUpdates(options?: { manual?: boolean }): void {
  if (options?.manual) void dialog.showMessageBox({
    message: 'This Bubble build uses manual updates.',
    detail: app.isPackaged
      ? 'Install a newer local Bubble application to update. Your conversations and settings are stored separately.'
      : 'Restart after rebuilding to use your latest changes.',
  });
}

function scheduleAutomaticUpdateCheck(): void {
  if (!app.isPackaged || updateCheckStarted) {
    return;
  }

  updateCheckStarted = true;
  setTimeout(() => {
    checkForUpdates({ manual: false });
  }, 8000);
}

function setupMenu(): void {
  const isMac = process.platform === 'darwin';
  const role = (
    value: Electron.MenuItemConstructorOptions['role']
  ): Electron.MenuItemConstructorOptions => ({ role: value });
  const separator = (): Electron.MenuItemConstructorOptions => ({ type: 'separator' });
  const template: Electron.MenuItemConstructorOptions[] = [];

  if (isMac) {
    const appSubmenu: Electron.MenuItemConstructorOptions[] = [
      role('about'),
      {
        label: 'Check for Updates...',
        click: () => checkForUpdates({ manual: true }),
      },
      separator(),
      role('services'),
      separator(),
      role('hide'),
      role('hideOthers'),
      role('unhide'),
      separator(),
      role('quit'),
    ];
    template.push({
      label: app.name,
      submenu: appSubmenu,
    });
  }

  const editSubmenu: Electron.MenuItemConstructorOptions[] = [
    role('undo'),
    role('redo'),
    separator(),
    role('cut'),
    role('copy'),
    role('paste'),
  ];
  if (isMac) {
    editSubmenu.push(role('pasteAndMatchStyle'), role('delete'), role('selectAll'));
  } else {
    editSubmenu.push(role('delete'), role('selectAll'));
  }

  template.push({
    label: 'Edit',
    submenu: editSubmenu,
  });

  const viewSubmenu: Electron.MenuItemConstructorOptions[] = [
    role('reload'),
    role('forceReload'),
    role('toggleDevTools'),
    separator(),
    role('resetZoom'),
    role('zoomIn'),
    role('zoomOut'),
    separator(),
    role('togglefullscreen'),
  ];

  template.push({
    label: 'View',
    submenu: viewSubmenu,
  });

  const windowSubmenu: Electron.MenuItemConstructorOptions[] = [
    role('minimize'),
    role('zoom'),
  ];
  if (isMac) {
    windowSubmenu.push(separator(), role('front'), role('window'));
  } else {
    // App tab closing is dispatched by the configurable renderer keymap.
    windowSubmenu.push({ ...role('close'), accelerator: '' });
  }

  template.push({
    label: 'Window',
    submenu: windowSubmenu,
  });

  if (!isMac) {
    const helpSubmenu: Electron.MenuItemConstructorOptions[] = [
      {
        label: 'Check for Updates...',
        click: () => checkForUpdates({ manual: true }),
      },
    ];
    template.push({
      label: 'Help',
      submenu: helpSubmenu,
    });
  }

  const menu = Menu.buildFromTemplate(template);
  Menu.setApplicationMenu(menu);
}

// 应用启动
app.whenReady().then(() => {
  installWidgetProtocol();
  registerWidgetIpc();
  // 生产环境设置 CSP；开发环境不限制（Vite HMR 需要 WebSocket 和 eval）
  if (!isDev()) {
    session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
      // The widget shell carries its own, stricter policy.
      if (details.url.startsWith('bubble-widget:')) {
        callback({ responseHeaders: details.responseHeaders });
        return;
      }
      callback({
        responseHeaders: {
          ...details.responseHeaders,
          'Content-Security-Policy': [
            // http://127.0.0.1:* covers the per-project local preview servers
            // (markdown images, PDF preview iframes); requests are still gated
            // by each server's per-instance random token.
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob: http://127.0.0.1:*; font-src 'self' data:; media-src 'self' blob: data: http://127.0.0.1:*; frame-src 'self' blob: data: http://127.0.0.1:* bubble-widget:; object-src 'self' blob: data:; connect-src 'self'",
          ],
        },
      });
    });
  }

  latestUiResumeState = loadUiResumeState();
  setupMenu();
  setupAutoUpdater();
  ipcMainHandle('check-for-updates', async () => {
    checkForUpdates({ manual: true });
    return { ok: true };
  });
  ipcMainHandle('get-update-status', async () => {
    return latestUpdateStatus;
  });
  ipcMainHandle('get-ui-resume-state', async event => {
    return sessionWindows.has(event.sender.id) ? null : latestUiResumeState;
  });
  ipcMainOn('get-ui-resume-state-sync', (event) => {
    event.returnValue = sessionWindows.has(event.sender.id) ? null : latestUiResumeState;
  });
  ipcMainHandle('save-ui-resume-state', async (_event, state: import('../shared/types').UiResumeState) => {
    if (!sessionWindows.has(_event.sender.id)) saveUiResumeState(state);
    return { ok: true };
  });
  ipcMainOn('save-ui-resume-state-sync', (event, state: import('../shared/types').UiResumeState) => {
    if (!sessionWindows.has(event.sender.id)) saveUiResumeState(state);
    event.returnValue = { ok: true };
  });
  // Origin-independent renderer state. The preload snapshots the full map
  // synchronously at page load (mirrors how localStorage hydrates), then
  // forwards writes here.
  ipcMainOn('renderer-state:get-all-sync', (event) => {
    event.returnValue = sessionWindows.get(event.sender.id)?.rendererState ?? loadRendererState();
  });
  const applyRendererStateBatch = (senderId: number, changes: unknown): void => {
    if (!Array.isArray(changes)) return;
    const secondary = sessionWindows.get(senderId);
    const state = secondary?.rendererState ?? loadRendererState();
    for (const item of changes) {
      if (!Array.isArray(item) || typeof item[0] !== 'string') continue;
      const [key, value] = item;
      if (key === '__proto__' || key === 'constructor' || key === 'prototype') continue;
      if (value === null) delete state[key];
      else if (typeof value === 'string') state[key] = value;
    }
    if (!secondary) scheduleRendererStateWrite();
  };
  ipcMainOn('renderer-state:batch', (event, changes: unknown) => {
    applyRendererStateBatch(event.sender.id, changes);
  });
  ipcMainOn('renderer-state:batch-sync', (event, changes: unknown) => {
    applyRendererStateBatch(event.sender.id, changes);
    flushRendererState();
    event.returnValue = true;
  });
  ipcMainOn('renderer-state:set', (_event, key: string, value: string) => {
    if (typeof key !== 'string' || typeof value !== 'string') return;
    const secondary = sessionWindows.get(_event.sender.id);
    if (secondary) { secondary.rendererState[key] = value; return; }
    loadRendererState()[key] = value;
    scheduleRendererStateWrite();
  });
  ipcMainOn('renderer-state:remove', (_event, key: string) => {
    if (typeof key !== 'string') return;
    const secondary = sessionWindows.get(_event.sender.id);
    if (secondary) { delete secondary.rendererState[key]; return; }
    delete loadRendererState()[key];
    scheduleRendererStateWrite();
  });
  // Keep the pending editor draft mirrored in the main process. Normal edits
  // use async IPC; blur/unload paths use sync IPC before the renderer freezes.
  ipcMainOn('project-editor-draft-update', (_event, draft: PendingProjectEditorDraft | null) => {
    if (!sessionWindows.has(_event.sender.id)) setPendingProjectEditorDraft(draft);
  });
  ipcMainOn('project-editor-draft-update-sync', (event, draft: PendingProjectEditorDraft | null) => {
    if (!sessionWindows.has(event.sender.id)) setPendingProjectEditorDraft(draft);
    event.returnValue = { ok: true };
  });
  // Called from beforeunload/pagehide to synchronously finish the file write
  // without depending on close/before-quit ordering or in-flight async IPC.
  ipcMainOn('write-project-text-file-sync', (event, draft: PendingProjectEditorDraft | null) => {
    if (sessionWindows.has(event.sender.id)) flushPendingProjectEditorDraftSync(draft);
    else { setPendingProjectEditorDraft(draft); flushPendingProjectEditorDraftSync(); }
    event.returnValue = { ok: true };
  });
  ipcMainHandle('get-app-version', async () => {
    return app.getVersion();
  });
  ipcMainHandle('get-window-shell-state', async event => {
    const win = BrowserWindow.fromWebContents(event.sender);
    return win ? getWindowShellState(win) : { rounded: process.platform === 'darwin' };
  });
  ipcMainHandle('set-theme', async (_event, theme: 'light' | 'dark' | 'system') => {
    nativeTheme.themeSource = theme;
    mainWindow?.setBackgroundColor(getMainWindowBackgroundColor());
    browserManager.applyThemeBackground();
    return { ok: true };
  });
  setupSessionWindowsIPC({
    backgroundColor: getMainWindowBackgroundColor,
    rendererState: loadRendererState,
    onCreate: win => {
      registerWindowShellState(win);
      protectRenderer(win);
      let closing = false;
      let flushed = false;
      win.on('close', event => {
        if (flushed) return;
        event.preventDefault();
        if (closing) return;
        closing = true;
        void requestProjectEditorFlush(win).then(result => {
          closing = false;
          if (win.isDestroyed()) return;
          if (!result.ok) { void dialog.showMessageBox(win, { type: 'error', message: 'Could not save editor changes', detail: result.message || 'Try closing the window again.' }); return; }
          flushed = true;
          win.close();
        });
      });
    },
  });
  createWindow();
  scheduleAutomaticUpdateCheck();

  app.on('activate', () => {
    logDevLifecycle('app.activate');
    showMainWindow();
  });
});

// 窗口全部关闭时退出（macOS 除外）
app.on('window-all-closed', () => {
  logDevLifecycle('app.window-all-closed', { platform: process.platform });
  if (process.platform !== 'darwin' || isQuitting) {
    app.quit();
  }
});

// 应用退出前清理
app.on('before-quit', () => {
  logDevLifecycle('app.before-quit');
  isQuitting = true;
  // Persist unsaved editor content before cleanup tears down shared resources.
  flushPendingProjectEditorDraftSync();
  if (mainWindow && !mainWindow.isDestroyed()) {
    saveWindowState(mainWindow);
  }
  if (latestUiResumeState) {
    saveUiResumeState(latestUiResumeState);
  }
  flushRendererState();
  devFileWatcher?.close();
  devFileWatcher = null;
  disposeBrowserIpc();
  cleanup();
});

app.on('will-quit', () => {
  logDevLifecycle('app.will-quit');
  // Last fallback for quit paths that bypass before-quit.
  flushPendingProjectEditorDraftSync();
  flushRendererState();
});

app.on('child-process-gone', (_event, details) => {
  console.error('[Dev] Child process gone:', details);
});

app.on('quit', (_event, exitCode) => {
  logDevLifecycle('app.quit', { exitCode });
});

process.on('uncaughtException', (error) => {
  console.error('[Dev Lifecycle] uncaughtException', error);
});

process.on('unhandledRejection', (reason) => {
  console.error('[Dev Lifecycle] unhandledRejection', reason);
});
