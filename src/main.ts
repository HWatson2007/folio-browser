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

interface NavigationEvent {
  url: string;
  status: NavigationStatus;
  title: string | null;
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
const historySearch = byId<HTMLInputElement>("history-search");
const historyList = byId<HTMLElement>("history-list");
const historySummary = byId<HTMLElement>("history-summary");
const loadIndicator = byId<HTMLElement>("load-indicator");
const profileBadge = byId<HTMLElement>("profile-badge");
const toast = byId<HTMLElement>("toast");

let profileSlug = "profile";

let historyEntries: HistoryEntry[] = [];
let historyOpen = false;
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

function displayTitle(entry: HistoryEntry): string {
  if (entry.searchQuery) return `Search: ${entry.searchQuery}`;
  if (entry.title?.trim()) return entry.title;
  try {
    return new URL(entry.url).hostname || entry.url;
  } catch {
    return entry.url;
  }
}

function matchesHistory(entry: HistoryEntry, query: string): boolean {
  const haystack = [entry.title, entry.url, entry.submittedInput, entry.searchQuery, entry.source]
    .filter(Boolean)
    .join("\n")
    .toLocaleLowerCase();
  return haystack.includes(query.toLocaleLowerCase());
}

function renderHistory(): void {
  const query = historySearch.value.trim();
  const visible = query ? historyEntries.filter((entry) => matchesHistory(entry, query)) : historyEntries;
  historyList.replaceChildren();
  historySummary.textContent = query
    ? `${visible.length.toLocaleString()} of ${historyEntries.length.toLocaleString()} recorded attempts match.`
    : `${historyEntries.length.toLocaleString()} navigation attempts, stored locally and ready to export.`;

  if (visible.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-history";
    empty.textContent = query ? "No ledger entries match this search." : "The ledger is empty. Your first navigation will appear here.";
    historyList.append(empty);
    return;
  }

  let previousDay = "";
  for (const entry of visible) {
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

async function refreshHistory(): Promise<void> {
  historyEntries = await invoke<HistoryEntry[]>("get_history");
  renderHistory();
}

async function openHistory(): Promise<void> {
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
byId<HTMLButtonElement>("close-history-button").addEventListener("click", closeHistory);
historySearch.addEventListener("input", () => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(renderHistory, 800);
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
  } else if (event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    void invoke("go_back");
  } else if (event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    void invoke("go_forward");
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "r") {
    event.preventDefault();
    void invoke("reload");
  } else if (event.key === "Escape" && historyOpen) {
    void closeHistory();
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
syncContentOffset();
