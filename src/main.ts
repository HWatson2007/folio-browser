import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import "./styles.css";

type NavigationStatus = "attempted" | "started" | "completed";

interface HistoryEntry {
  id: number;
  attemptedAt: number;
  updatedAt: number;
  url: string;
  title: string | null;
  status: NavigationStatus;
  source: string;
  submittedInput: string | null;
  searchQuery: string | null;
  searchUrl: string | null;
}

interface HistoryPage {
  entries: HistoryEntry[];
  total: number;
}

interface NavigationEvent {
  url: string;
  status: NavigationStatus;
  title: string | null;
}

type DownloadStatus = "requested" | "downloading" | "completed" | "failed" | "canceled" | "interrupted";

interface DownloadEntry {
  id: number;
  requestedAt: number;
  updatedAt: number;
  completedAt: number | null;
  url: string;
  sourcePageUrl: string | null;
  suggestedFilename: string;
  path: string | null;
  mimeType: string | null;
  contentDisposition: string | null;
  status: DownloadStatus;
  bytesReceived: number;
  totalBytes: number | null;
  interruptReason: string | null;
}

interface ProfileSummary {
  id: string;
  name: string;
  color: string;
  createdAt: number;
  lastUsedAt: number | null;
  running: boolean;
}

const byId = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing element #${id}`);
  return element as T;
};

const addressForm = byId<HTMLFormElement>("address-form");
const addressInput = byId<HTMLInputElement>("address-input");
const historyButton = byId<HTMLButtonElement>("history-button");
const historyPanel = byId<HTMLElement>("history-panel");
const downloadsButton = byId<HTMLButtonElement>("downloads-button");
const downloadsCount = byId<HTMLElement>("downloads-count");
const downloadsPanel = byId<HTMLElement>("downloads-panel");
const downloadsList = byId<HTMLElement>("downloads-list");
const downloadsSummary = byId<HTMLElement>("downloads-summary");
const openWarning = byId<HTMLElement>("open-warning");
const openWarningMessage = byId<HTMLElement>("open-warning-message");
const historySearch = byId<HTMLInputElement>("history-search");
const historyList = byId<HTMLElement>("history-list");
const historySummary = byId<HTMLElement>("history-summary");
const historyPagination = byId<HTMLElement>("history-pagination");
const historyPrevButton = byId<HTMLButtonElement>("history-prev");
const historyNextButton = byId<HTMLButtonElement>("history-next");
const historyPageLabel = byId<HTMLElement>("history-page-label");
const loadIndicator = byId<HTMLElement>("load-indicator");
const profileBadge = byId<HTMLElement>("profile-badge");
const toast = byId<HTMLElement>("toast");

let profileSlug = "profile";

let historyEntries: HistoryEntry[] = [];
let historyTotal = 0;
let historyPage = 0;
const HISTORY_PAGE_SIZE = 200;
let historyQuery = "";
let historySearchSeq = 0;
let downloadEntries: DownloadEntry[] = [];
let historyOpen = false;
let downloadsOpen = false;
let pendingExecutableId: number | null = null;
let toastTimer: number | undefined;
let resizeTimer: number | undefined;
let searchTimer: number | undefined;

const dayFormatter = new Intl.DateTimeFormat(undefined, {
  weekday: "long",
  year: "numeric",
  month: "long",
  day: "numeric",
});

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
});

function showToast(message: string): void {
  toast.textContent = message;
  toast.classList.add("is-visible");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 2800);
}

function formatDay(timestamp: number): string {
  return dayFormatter.format(new Date(timestamp));
}

function formatTime(timestamp: number): string {
  return timeFormatter.format(new Date(timestamp));
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes.toLocaleString()} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= 1024; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value.toLocaleString(undefined, { maximumFractionDigits: value < 10 ? 1 : 0 })} ${unit}`;
}

function downloadDetail(entry: DownloadEntry): string {
  if (entry.status === "downloading" || entry.status === "requested") {
    if (entry.totalBytes != null) {
      return `${formatBytes(entry.bytesReceived)} of ${formatBytes(entry.totalBytes)}`;
    }
    return entry.bytesReceived > 0 ? formatBytes(entry.bytesReceived) : "Preparing download";
  }
  if (entry.status === "completed") return `${formatBytes(entry.bytesReceived)} · Complete`;
  if (entry.status === "canceled") return "Canceled";
  if (entry.status === "interrupted") return "Interrupted when the browser closed";
  return "Download failed";
}

function isExecutable(path: string | null): boolean {
  return Boolean(path && /\.(exe|msi|msix|bat|cmd|com|scr|ps1|vbs|js|jar)$/i.test(path));
}

function makeDownloadAction(label: string, action: () => Promise<void>, className = ""): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `download-action ${className}`.trim();
  button.textContent = label;
  button.addEventListener("click", () => void action().catch((error) => showToast(String(error))));
  return button;
}

function requestOpen(entry: DownloadEntry): Promise<void> {
  if (!isExecutable(entry.path)) return invoke("open_download", { id: entry.id });
  pendingExecutableId = entry.id;
  openWarningMessage.textContent = `${entry.suggestedFilename} can make changes to this computer. Open it only if you trust its source.`;
  openWarning.hidden = false;
  byId<HTMLButtonElement>("cancel-open-button").focus();
  return Promise.resolve();
}

function renderDownloads(): void {
  downloadsList.replaceChildren();
  const activeCount = downloadEntries.filter((entry) => entry.status === "requested" || entry.status === "downloading").length;
  downloadsCount.hidden = activeCount === 0;
  downloadsCount.textContent = activeCount.toLocaleString();
  downloadsSummary.textContent = downloadEntries.length === 0
    ? "No files have been requested by this profile."
    : `${downloadEntries.length.toLocaleString()} recorded ${downloadEntries.length === 1 ? "download" : "downloads"}, stored separately for this profile.`;

  if (downloadEntries.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-history";
    empty.textContent = "Downloaded files will be listed here.";
    downloadsList.append(empty);
    return;
  }

  for (const entry of downloadEntries) {
    const row = document.createElement("article");
    row.className = `download-entry status-${entry.status}`;

    const marker = document.createElement("div");
    marker.className = "download-marker";
    marker.textContent = entry.suggestedFilename.slice(0, 1).toLocaleUpperCase() || "↓";
    marker.setAttribute("aria-hidden", "true");

    const body = document.createElement("div");
    body.className = "download-body";
    const heading = document.createElement("div");
    heading.className = "download-title-row";
    const title = document.createElement("h2");
    title.textContent = entry.suggestedFilename;
    title.title = entry.path ?? entry.suggestedFilename;
    const status = document.createElement("span");
    status.className = "download-status";
    status.textContent = entry.status;
    heading.append(title, status);

    const detail = document.createElement("p");
    detail.className = "download-detail";
    detail.textContent = `${downloadDetail(entry)} · ${formatDay(entry.requestedAt)}, ${formatTime(entry.requestedAt)}`;

    if (entry.status === "downloading" || entry.status === "requested") {
      const track = document.createElement("div");
      track.className = "download-progress";
      const bar = document.createElement("span");
      if (entry.totalBytes && entry.totalBytes > 0) {
        bar.style.width = `${Math.min(100, (entry.bytesReceived / entry.totalBytes) * 100)}%`;
      } else {
        track.classList.add("is-indeterminate");
      }
      track.append(bar);
      body.append(heading, detail, track);
    } else {
      body.append(heading, detail);
    }

    const source = document.createElement("p");
    source.className = "download-source";
    source.textContent = entry.url;
    source.title = entry.sourcePageUrl ? `Requested from ${entry.sourcePageUrl}` : entry.url;
    body.append(source);
    if (entry.path) {
      const location = document.createElement("p");
      location.className = "download-location";
      location.textContent = entry.path;
      location.title = entry.path;
      body.append(location);
    }

    const actions = document.createElement("div");
    actions.className = "download-actions";
    if (entry.status === "requested" || entry.status === "downloading") {
      actions.append(makeDownloadAction("Cancel", () => invoke("cancel_download", { id: entry.id }), "danger"));
    } else if (entry.status === "completed") {
      actions.append(
        makeDownloadAction("Open", () => requestOpen(entry), "primary"),
        makeDownloadAction("Show in folder", () => invoke("show_download_in_folder", { id: entry.id })),
      );
    }
    row.append(marker, body, actions);
    downloadsList.append(row);
  }
}

async function refreshDownloads(): Promise<void> {
  downloadEntries = await invoke<DownloadEntry[]>("get_downloads");
  renderDownloads();
}

function displayTitle(entry: HistoryEntry): string {
  if (entry.searchQuery) return `Search: ${entry.searchQuery}`;
  if (entry.title?.trim()) return entry.title;
  try {
    return new URL(entry.url).hostname || entry.url;
  } catch {
    return entry.url;
  }
}

function clampHistoryPage(page: number): number {
  if (historyTotal === 0) return 0;
  const maxPage = Math.max(0, Math.ceil(historyTotal / HISTORY_PAGE_SIZE) - 1);
  return Math.min(Math.max(0, page), maxPage);
}

function renderHistory(): void {
  historyList.replaceChildren();

  const totalPages = historyTotal === 0 ? 0 : Math.ceil(historyTotal / HISTORY_PAGE_SIZE);
  const currentPage = totalPages === 0 ? 0 : historyPage + 1;
  const showing = historyEntries.length;
  const offset = historyPage * HISTORY_PAGE_SIZE;
  const rangeStart = historyTotal === 0 || showing === 0 ? 0 : offset + 1;
  const rangeEnd = offset + showing;

  if (historyQuery) {
    historySummary.textContent =
      historyTotal === 0
        ? "No ledger entries match this search."
        : `${rangeStart.toLocaleString()}–${rangeEnd.toLocaleString()} of ${historyTotal.toLocaleString()} matching entries · page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
  } else {
    historySummary.textContent =
      historyTotal === 0
        ? "The ledger is empty. Your first navigation will appear here."
        : `${rangeStart.toLocaleString()}–${rangeEnd.toLocaleString()} of ${historyTotal.toLocaleString()} navigation attempts · page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
  }

  if (showing === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-history";
    empty.textContent = historyQuery ? "No ledger entries match this search." : "The ledger is empty. Your first navigation will appear here.";
    historyList.append(empty);
  } else {
    let previousDay = "";
    for (const entry of historyEntries) {
      const day = formatDay(entry.attemptedAt);
      if (day !== previousDay) {
        const dayHeading = document.createElement("h2");
        dayHeading.className = "day-heading";
        dayHeading.textContent = day;
        historyList.append(dayHeading);
        previousDay = day;
      }

      const row = document.createElement("article");
      row.className = "history-entry";

      const time = document.createElement("time");
      time.dateTime = new Date(entry.attemptedAt).toISOString();
      time.textContent = formatTime(entry.attemptedAt);

      const details = document.createElement("div");
      details.className = "entry-details";

      const titleButton = document.createElement("button");
      titleButton.className = "entry-title";
      titleButton.type = "button";
      titleButton.textContent = displayTitle(entry);
      titleButton.title = `Open ${entry.url}`;
      titleButton.addEventListener("click", async () => {
        await closeHistory();
        await invoke("navigate", { input: entry.url, source: "history" });
      });

      const url = document.createElement("p");
      url.className = "entry-url";
      url.textContent = entry.url;

      const metadata = document.createElement("p");
      metadata.className = "entry-metadata";
      const searchDetail = entry.submittedInput ? ` | submitted: ${entry.submittedInput}` : "";
      metadata.textContent = `${entry.status} | ${entry.source}${searchDetail}`;

      details.append(titleButton, url, metadata);
      row.append(time, details);
      historyList.append(row);
    }
  }

  const hasPages = totalPages > 1;
  historyPagination.hidden = !hasPages;
  if (hasPages) {
    historyPageLabel.textContent = `Page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
    historyPrevButton.disabled = historyPage <= 0;
    historyNextButton.disabled = historyPage >= totalPages - 1;
  }
}

async function fetchHistoryPage(page: number, query: string): Promise<void> {
  const seq = ++historySearchSeq;
  const offset = page * HISTORY_PAGE_SIZE;
  const normalizedQuery = query.trim() ? query.trim() : null;
  const result = await invoke<HistoryPage>("get_history_page", {
    limit: HISTORY_PAGE_SIZE,
    offset,
    query: normalizedQuery,
  });
  if (seq !== historySearchSeq) return;
  historyEntries = result.entries;
  historyTotal = result.total;
  const clamped = clampHistoryPage(page);
  if (clamped !== page) {
    // Total shrank while browsing; refetch clamped page.
    await fetchHistoryPage(clamped, query);
    return;
  }
  historyPage = clamped;
  renderHistory();
}

async function refreshHistory(): Promise<void> {
  const clamped = clampHistoryPage(historyPage);
  await fetchHistoryPage(clamped, historyQuery);
}

async function openHistory(): Promise<void> {
  if (downloadsOpen) await closeDownloads();
  historyOpen = true;
  historyButton.setAttribute("aria-expanded", "true");
  historyPanel.setAttribute("aria-hidden", "false");
  historyPanel.classList.add("is-open");
  await invoke("set_content_visible", { visible: false });
  await refreshHistory();
  historySearch.focus();
}

async function closeHistory(): Promise<void> {
  if (!historyOpen) return;
  historyOpen = false;
  historyButton.setAttribute("aria-expanded", "false");
  historyPanel.setAttribute("aria-hidden", "true");
  historyPanel.classList.remove("is-open");
  await invoke("set_content_visible", { visible: true });
}

async function openDownloads(): Promise<void> {
  if (historyOpen) await closeHistory();
  downloadsOpen = true;
  downloadsButton.setAttribute("aria-expanded", "true");
  downloadsPanel.setAttribute("aria-hidden", "false");
  downloadsPanel.classList.add("is-open");
  await invoke("set_content_visible", { visible: false });
  await refreshDownloads();
}

async function closeDownloads(): Promise<void> {
  if (!downloadsOpen) return;
  downloadsOpen = false;
  downloadsButton.setAttribute("aria-expanded", "false");
  downloadsPanel.setAttribute("aria-hidden", "true");
  downloadsPanel.classList.remove("is-open");
  await invoke("set_content_visible", { visible: true });
}

async function exportHistory(format: "json" | "csv"): Promise<void> {
  const path = await save({
    title: `Export ${profileSlug} browsing history as ${format.toUpperCase()}`,
    defaultPath: `folio-history-${profileSlug}-${new Date().toISOString().slice(0, 10)}.${format}`,
    filters: [{ name: format.toUpperCase(), extensions: [format] }],
  });
  if (!path) return;

  const count = await invoke<number>("export_history", { path, format });
  showToast(`Exported ${count.toLocaleString()} entries.`);
}

function syncContentOffset(): void {
  window.clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    const toolbarHeight = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--toolbar-height"),
    );
    void invoke("set_content_offset", { offset: toolbarHeight });
  }, 80);
}

addressForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = addressInput.value.trim();
  if (!input) return;
  try {
    await invoke("navigate", { input, source: "address" });
  } catch (error) {
    showToast(String(error));
  }
});

byId<HTMLButtonElement>("back-button").addEventListener("click", () => invoke("go_back"));
byId<HTMLButtonElement>("forward-button").addEventListener("click", () => invoke("go_forward"));
byId<HTMLButtonElement>("reload-button").addEventListener("click", () => invoke("reload"));
byId<HTMLButtonElement>("home-button").addEventListener("click", () => invoke("navigate_home"));
historyButton.addEventListener("click", () => (historyOpen ? closeHistory() : openHistory()));
downloadsButton.addEventListener("click", () => (downloadsOpen ? closeDownloads() : openDownloads()));
byId<HTMLButtonElement>("close-history-button").addEventListener("click", closeHistory);
byId<HTMLButtonElement>("close-downloads-button").addEventListener("click", closeDownloads);
byId<HTMLButtonElement>("cancel-open-button").addEventListener("click", () => {
  pendingExecutableId = null;
  openWarning.hidden = true;
});
byId<HTMLButtonElement>("confirm-open-button").addEventListener("click", async () => {
  const id = pendingExecutableId;
  pendingExecutableId = null;
  openWarning.hidden = true;
  if (id == null) return;
  try {
    await invoke("open_download", { id });
  } catch (error) {
    showToast(String(error));
  }
});
historySearch.addEventListener("input", () => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    historyQuery = historySearch.value.trim();
    historyPage = 0;
    void fetchHistoryPage(0, historyQuery).catch((error) => showToast(String(error)));
  }, 400);
});
historyPrevButton.addEventListener("click", () => {
  if (historyPage <= 0) return;
  void fetchHistoryPage(historyPage - 1, historyQuery).catch((error) => showToast(String(error)));
});
historyNextButton.addEventListener("click", () => {
  const maxPage = Math.max(0, Math.ceil(historyTotal / HISTORY_PAGE_SIZE) - 1);
  if (historyPage >= maxPage) return;
  void fetchHistoryPage(historyPage + 1, historyQuery).catch((error) => showToast(String(error)));
});
byId<HTMLButtonElement>("export-json-button").addEventListener("click", () => exportHistory("json"));
byId<HTMLButtonElement>("export-csv-button").addEventListener("click", () => exportHistory("csv"));

window.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.key.toLocaleLowerCase() === "l") {
    event.preventDefault();
    addressInput.focus();
    addressInput.select();
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "h") {
    event.preventDefault();
    void (historyOpen ? closeHistory() : openHistory());
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "j") {
    event.preventDefault();
    void (downloadsOpen ? closeDownloads() : openDownloads());
  } else if (event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    void invoke("go_back");
  } else if (event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    void invoke("go_forward");
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "r") {
    event.preventDefault();
    void invoke("reload");
  } else if (event.key === "Escape" && !openWarning.hidden) {
    pendingExecutableId = null;
    openWarning.hidden = true;
  } else if (event.key === "Escape" && historyOpen) {
    void closeHistory();
  } else if (event.key === "Escape" && downloadsOpen) {
    void closeDownloads();
  }
});

window.addEventListener("resize", syncContentOffset);

void listen<NavigationEvent>("browser:navigation", ({ payload }) => {
  addressInput.value = payload.url;
  loadIndicator.classList.toggle("is-loading", payload.status !== "completed");
  if (historyOpen) void refreshHistory();
});

void listen<string>("browser:popup-requested", ({ payload }) => {
  void invoke("navigate", { input: payload, source: "popup" });
});

void listen<DownloadEntry>("browser:download", ({ payload }) => {
  const index = downloadEntries.findIndex((entry) => entry.id === payload.id);
  const previousStatus = index >= 0 ? (downloadEntries[index]?.status ?? null) : null;
  if (index >= 0) downloadEntries[index] = payload;
  else downloadEntries.unshift(payload);
  renderDownloads();
  if (previousStatus !== payload.status) {
    if (payload.status === "completed") showToast(`${payload.suggestedFilename} downloaded.`);
    else if (payload.status === "failed") showToast(`${payload.suggestedFilename} could not be downloaded.`);
  }
});

void invoke<string>("current_url").then((url) => {
  addressInput.value = url;
});
void invoke<ProfileSummary>("get_current_profile").then((profile) => {
  profileBadge.textContent = profile.name;
  profileBadge.title = `Current profile: ${profile.name}`;
  profileSlug =
    profile.name
      .toLocaleLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "profile";
});
void refreshDownloads().catch((error) => showToast(String(error)));
syncContentOffset();
