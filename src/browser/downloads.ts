import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { byId } from "./dom";
import { formatBytes, formatDay, formatTime } from "./format";
import type { DownloadEntry, ShowToast } from "./types";

export class DownloadsController {
  private readonly button = byId<HTMLButtonElement>("downloads-button");
  private readonly count = byId<HTMLElement>("downloads-count");
  private readonly panel = byId<HTMLElement>("downloads-panel");
  private readonly list = byId<HTMLElement>("downloads-list");
  private readonly summary = byId<HTMLElement>("downloads-summary");
  private readonly openWarning = byId<HTMLElement>("open-warning");
  private readonly openWarningMessage = byId<HTMLElement>("open-warning-message");

  private entries: DownloadEntry[] = [];
  private openState = false;
  private pendingExecutableId: number | null = null;

  constructor(
    private readonly showToast: ShowToast,
    private readonly beforeOpen: () => Promise<void> | void,
  ) {}

  get isOpen(): boolean {
    return this.openState;
  }

  get hasOpenWarning(): boolean {
    return !this.openWarning.hidden;
  }

  initialize(): void {
    this.button.addEventListener("click", () => void this.toggle());
    byId<HTMLButtonElement>("close-downloads-button").addEventListener("click", () => void this.close());
    byId<HTMLButtonElement>("cancel-open-button").addEventListener("click", () => this.closeOpenWarning());
    byId<HTMLButtonElement>("confirm-open-button").addEventListener("click", () => void this.confirmOpen());

    void listen<DownloadEntry>("browser:download", ({ payload }) => this.applyEvent(payload));
    void this.refresh().catch((error) => this.showToast(String(error)));
  }

  async toggle(): Promise<void> {
    if (this.openState) await this.close();
    else await this.open();
  }

  async open(): Promise<void> {
    await this.beforeOpen();
    this.openState = true;
    this.button.setAttribute("aria-expanded", "true");
    this.panel.setAttribute("aria-hidden", "false");
    this.panel.classList.add("is-open");
    await invoke("set_content_visible", { visible: false });
    await this.refresh();
  }

  async close(): Promise<void> {
    if (!this.openState) return;
    this.openState = false;
    this.button.setAttribute("aria-expanded", "false");
    this.panel.setAttribute("aria-hidden", "true");
    this.panel.classList.remove("is-open");
    await invoke("set_content_visible", { visible: true });
  }

  closeOpenWarning(): void {
    this.pendingExecutableId = null;
    this.openWarning.hidden = true;
  }

  private downloadDetail(entry: DownloadEntry): string {
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

  private isExecutable(path: string | null): boolean {
    return Boolean(path && /\.(exe|msi|msix|bat|cmd|com|scr|ps1|vbs|js|jar)$/i.test(path));
  }

  private makeAction(label: string, action: () => Promise<void>, className = ""): HTMLButtonElement {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `download-action ${className}`.trim();
    button.textContent = label;
    button.addEventListener("click", () => void action().catch((error) => this.showToast(String(error))));
    return button;
  }

  private requestOpen(entry: DownloadEntry): Promise<void> {
    if (!this.isExecutable(entry.path)) return invoke("open_download", { id: entry.id });
    this.pendingExecutableId = entry.id;
    this.openWarningMessage.textContent = `${entry.suggestedFilename} can make changes to this computer. Open it only if you trust its source.`;
    this.openWarning.hidden = false;
    byId<HTMLButtonElement>("cancel-open-button").focus();
    return Promise.resolve();
  }

  private render(): void {
    this.list.replaceChildren();
    const activeCount = this.entries.filter(
      (entry) => entry.status === "requested" || entry.status === "downloading",
    ).length;
    this.count.hidden = activeCount === 0;
    this.count.textContent = activeCount.toLocaleString();
    this.summary.textContent =
      this.entries.length === 0
        ? "No files have been requested by this profile."
        : `${this.entries.length.toLocaleString()} recorded ${this.entries.length === 1 ? "download" : "downloads"}, stored separately for this profile.`;

    if (this.entries.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-history";
      empty.textContent = "Downloaded files will be listed here.";
      this.list.append(empty);
      return;
    }

    for (const entry of this.entries) {
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
      detail.textContent = `${this.downloadDetail(entry)} · ${formatDay(entry.requestedAt)}, ${formatTime(entry.requestedAt)}`;
      body.append(heading, detail);
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
        body.append(track);
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
        actions.append(this.makeAction("Cancel", () => invoke("cancel_download", { id: entry.id }), "danger"));
      } else if (entry.status === "completed") {
        actions.append(
          this.makeAction("Open", () => this.requestOpen(entry), "primary"),
          this.makeAction("Show in folder", () => invoke("show_download_in_folder", { id: entry.id })),
        );
      }
      row.append(marker, body, actions);
      this.list.append(row);
    }
  }

  private async refresh(): Promise<void> {
    this.entries = await invoke<DownloadEntry[]>("get_downloads");
    this.render();
  }

  private applyEvent(entry: DownloadEntry): void {
    const index = this.entries.findIndex((item) => item.id === entry.id);
    const previousStatus = index >= 0 ? (this.entries[index]?.status ?? null) : null;
    if (index >= 0) this.entries[index] = entry;
    else this.entries.unshift(entry);
    this.render();
    if (previousStatus !== entry.status) {
      if (entry.status === "completed") this.showToast(`${entry.suggestedFilename} downloaded.`);
      else if (entry.status === "failed") this.showToast(`${entry.suggestedFilename} could not be downloaded.`);
    }
  }

  private async confirmOpen(): Promise<void> {
    const id = this.pendingExecutableId;
    this.closeOpenWarning();
    if (id == null) return;
    try {
      await invoke("open_download", { id });
    } catch (error) {
      this.showToast(String(error));
    }
  }
}
