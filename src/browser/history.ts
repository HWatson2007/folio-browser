import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { byId } from "./dom";
import { formatDay, formatTime } from "./format";
import type { HistoryEntry, HistoryPage, ShowToast } from "./types";

const PAGE_SIZE = 200;

export class HistoryController {
  private readonly button = byId<HTMLButtonElement>("history-button");
  private readonly panel = byId<HTMLElement>("history-panel");
  private readonly search = byId<HTMLInputElement>("history-search");
  private readonly list = byId<HTMLElement>("history-list");
  private readonly summary = byId<HTMLElement>("history-summary");
  private readonly pagination = byId<HTMLElement>("history-pagination");
  private readonly previousButton = byId<HTMLButtonElement>("history-prev");
  private readonly nextButton = byId<HTMLButtonElement>("history-next");
  private readonly pageLabel = byId<HTMLElement>("history-page-label");

  private entries: HistoryEntry[] = [];
  private total = 0;
  private page = 0;
  private query = "";
  private searchSequence = 0;
  private dirty = false;
  private openState = false;
  private profileSlug = "profile";
  private searchTimer: number | undefined;

  constructor(
    private readonly showToast: ShowToast,
    private readonly beforeOpen: () => Promise<void> | void,
  ) {}

  get isOpen(): boolean {
    return this.openState;
  }

  initialize(): void {
    this.button.addEventListener("click", () => void this.toggle());
    byId<HTMLButtonElement>("close-history-button").addEventListener("click", () => void this.close());
    this.search.addEventListener("input", () => {
      window.clearTimeout(this.searchTimer);
      this.searchTimer = window.setTimeout(() => {
        this.query = this.search.value.trim();
        this.page = 0;
        void this.fetchPage(0, this.query).catch((error) => this.showToast(String(error)));
      }, 400);
    });
    this.previousButton.addEventListener("click", () => {
      if (this.page <= 0) return;
      void this.fetchPage(this.page - 1, this.query).catch((error) => this.showToast(String(error)));
    });
    this.nextButton.addEventListener("click", () => {
      const maxPage = Math.max(0, Math.ceil(this.total / PAGE_SIZE) - 1);
      if (this.page >= maxPage) return;
      void this.fetchPage(this.page + 1, this.query).catch((error) => this.showToast(String(error)));
    });
    byId<HTMLButtonElement>("export-json-button").addEventListener("click", () => void this.export("json"));
    byId<HTMLButtonElement>("export-csv-button").addEventListener("click", () => void this.export("csv"));
  }

  setProfileName(name: string): void {
    this.profileSlug =
      name
        .toLocaleLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, "") || "profile";
  }

  markDirty(): void {
    this.dirty = true;
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
    if (this.dirty || this.total === 0) {
      this.dirty = false;
      await this.refresh();
    }
    this.search.focus();
  }

  async close(): Promise<void> {
    if (!this.openState) return;
    this.openState = false;
    this.button.setAttribute("aria-expanded", "false");
    this.panel.setAttribute("aria-hidden", "true");
    this.panel.classList.remove("is-open");
    await invoke("set_content_visible", { visible: true });
  }

  private displayTitle(entry: HistoryEntry): string {
    if (entry.searchQuery) return `Search: ${entry.searchQuery}`;
    if (entry.title?.trim()) return entry.title;
    try {
      return new URL(entry.url).hostname || entry.url;
    } catch {
      return entry.url;
    }
  }

  private clampPage(page: number): number {
    if (this.total === 0) return 0;
    const maxPage = Math.max(0, Math.ceil(this.total / PAGE_SIZE) - 1);
    return Math.min(Math.max(0, page), maxPage);
  }

  private render(): void {
    this.list.replaceChildren();
    const totalPages = this.total === 0 ? 0 : Math.ceil(this.total / PAGE_SIZE);
    const currentPage = totalPages === 0 ? 0 : this.page + 1;
    const showing = this.entries.length;
    const offset = this.page * PAGE_SIZE;
    const rangeStart = this.total === 0 || showing === 0 ? 0 : offset + 1;
    const rangeEnd = offset + showing;

    if (this.query) {
      this.summary.textContent =
        this.total === 0
          ? "No ledger entries match this search."
          : `${rangeStart.toLocaleString()}–${rangeEnd.toLocaleString()} of ${this.total.toLocaleString()} matching entries · page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
    } else {
      this.summary.textContent =
        this.total === 0
          ? "The ledger is empty. Your first navigation will appear here."
          : `${rangeStart.toLocaleString()}–${rangeEnd.toLocaleString()} of ${this.total.toLocaleString()} navigation attempts · page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
    }

    if (showing === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-history";
      empty.textContent = this.query
        ? "No ledger entries match this search."
        : "The ledger is empty. Your first navigation will appear here.";
      this.list.append(empty);
    } else {
      let previousDay = "";
      for (const entry of this.entries) {
        const day = formatDay(entry.attemptedAt);
        if (day !== previousDay) {
          const heading = document.createElement("h2");
          heading.className = "day-heading";
          heading.textContent = day;
          this.list.append(heading);
          previousDay = day;
        }

        const row = document.createElement("article");
        row.className = "history-entry";
        const time = document.createElement("time");
        time.dateTime = new Date(entry.attemptedAt).toISOString();
        time.textContent = formatTime(entry.attemptedAt);

        const details = document.createElement("div");
        details.className = "entry-details";
        const title = document.createElement("button");
        title.className = "entry-title";
        title.type = "button";
        title.textContent = this.displayTitle(entry);
        title.title = `Open ${entry.url}`;
        title.addEventListener("click", async () => {
          await this.close();
          await invoke("navigate", { input: entry.url, source: "history" });
        });

        const url = document.createElement("p");
        url.className = "entry-url";
        url.textContent = entry.url;
        const metadata = document.createElement("p");
        metadata.className = "entry-metadata";
        const searchDetail = entry.submittedInput ? ` | submitted: ${entry.submittedInput}` : "";
        metadata.textContent = `${entry.status} | ${entry.source}${searchDetail}`;
        details.append(title, url, metadata);
        row.append(time, details);
        this.list.append(row);
      }
    }

    const hasPages = totalPages > 1;
    this.pagination.hidden = !hasPages;
    if (hasPages) {
      this.pageLabel.textContent = `Page ${currentPage.toLocaleString()} of ${totalPages.toLocaleString()}`;
      this.previousButton.disabled = this.page <= 0;
      this.nextButton.disabled = this.page >= totalPages - 1;
    }
  }

  private async fetchPage(page: number, query: string): Promise<void> {
    const sequence = ++this.searchSequence;
    const result = await invoke<HistoryPage>("get_history_page", {
      limit: PAGE_SIZE,
      offset: page * PAGE_SIZE,
      query: query.trim() || null,
    });
    if (sequence !== this.searchSequence) return;
    this.entries = result.entries;
    this.total = result.total;
    const clamped = this.clampPage(page);
    if (clamped !== page) {
      await this.fetchPage(clamped, query);
      return;
    }
    this.page = clamped;
    this.render();
  }

  private async refresh(): Promise<void> {
    await this.fetchPage(this.clampPage(this.page), this.query);
  }

  private async export(format: "json" | "csv"): Promise<void> {
    const path = await save({
      title: `Export ${this.profileSlug} browsing history as ${format.toUpperCase()}`,
      defaultPath: `folio-history-${this.profileSlug}-${new Date().toISOString().slice(0, 10)}.${format}`,
      filters: [{ name: format.toUpperCase(), extensions: [format] }],
    });
    if (!path) return;
    try {
      const count = await invoke<number>("export_history", { path, format });
      this.showToast(`Exported ${count.toLocaleString()} entries.`);
    } catch (error) {
      this.showToast(String(error));
    }
  }
}
