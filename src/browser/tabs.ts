import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { byId } from "./dom";
import type { NavigationEvent, ShowToast, TabSummary } from "./types";

export class TabsController {
  private readonly addressInput = byId<HTMLInputElement>("address-input");
  private readonly loadIndicator = byId<HTMLElement>("load-indicator");
  private readonly tabSwitcher = byId<HTMLElement>("tab-switcher");
  private readonly brandTrigger = byId<HTMLButtonElement>("brand-trigger");
  private readonly newTabButton = byId<HTMLButtonElement>("new-tab-button");
  private readonly tabOverview = byId<HTMLElement>("tab-overview");
  private readonly tabList = byId<HTMLElement>("tab-list");

  private tabs: TabSummary[] = [];
  private activeTabId: number | null = null;
  private activeTabUrl = "";
  private overviewOpen = false;
  private blankTabFocusPending = false;
  private blankTabPreviousId: number | null = null;
  private closeTimer: number | undefined;

  constructor(
    private readonly showToast: ShowToast,
    private readonly canOpenOverview: () => boolean,
  ) {}

  get activeId(): number | null {
    return this.activeTabId;
  }

  get isOverviewOpen(): boolean {
    return this.overviewOpen;
  }

  initialize(): void {
    this.tabSwitcher.addEventListener("pointerenter", () => this.openOverview());
    this.tabSwitcher.addEventListener("pointerleave", () => this.closeOverview());
    this.brandTrigger.addEventListener("click", () => this.openOverview());
    this.newTabButton.addEventListener("click", (event) => {
      event.stopPropagation();
      void this.createBlank();
    });

    void listen<TabSummary[]>("browser:tabs", ({ payload }) => this.applyTabs(payload));
    void listen<string>("browser:tab-error", ({ payload }) => {
      this.blankTabFocusPending = false;
      this.blankTabPreviousId = null;
      this.showToast(payload);
    });
    void listen<string>("browser:popup-requested", ({ payload }) => {
      this.closeOverview(true);
      void invoke<void>("create_tab", { url: payload }).catch((error) => this.showToast(String(error)));
    });
    void invoke<TabSummary[]>("get_tabs")
      .then((tabs) => this.applyTabs(tabs))
      .catch((error) => this.showToast(String(error)));
  }

  handleNavigation(event: NavigationEvent): void {
    if (event.tabId !== this.activeTabId) return;
    this.addressInput.value = event.url;
    this.activeTabUrl = event.url;
    this.loadIndicator.classList.toggle("is-loading", event.status !== "completed");
  }

  handleResize(): void {
    if (this.overviewOpen) window.requestAnimationFrame(() => this.syncOverlayHeight());
  }

  focusAddress(raiseChrome = true): void {
    this.closeOverview(true);
    const focusInput = (): void => {
      this.addressInput.focus();
      this.addressInput.select();
    };
    if (!raiseChrome) {
      focusInput();
      return;
    }
    void getCurrentWebview().setFocus().then(focusInput).catch((error) => this.showToast(String(error)));
  }

  closeActive(): void {
    if (this.activeTabId == null) return;
    void invoke("close_tab", { id: this.activeTabId }).catch((error) => this.showToast(String(error)));
  }

  cycle(direction: number): void {
    this.closeOverview(true);
    void invoke("cycle_tab", { direction }).catch((error) => this.showToast(String(error)));
  }

  async createBlank(): Promise<void> {
    this.closeOverview(true);
    this.blankTabFocusPending = true;
    this.blankTabPreviousId = this.activeTabId;
    try {
      await invoke<void>("create_tab", { url: null });
    } catch (error) {
      this.blankTabFocusPending = false;
      this.blankTabPreviousId = null;
      this.showToast(String(error));
    }
  }

  openOverview(): void {
    window.clearTimeout(this.closeTimer);
    if (this.overviewOpen || !this.canOpenOverview()) return;
    this.overviewOpen = true;
    this.render();
    this.tabSwitcher.classList.add("is-open");
    this.brandTrigger.setAttribute("aria-expanded", "true");
    this.tabOverview.setAttribute("aria-hidden", "false");
    window.requestAnimationFrame(() => this.syncOverlayHeight());
  }

  closeOverview(immediate = false): void {
    window.clearTimeout(this.closeTimer);
    const close = (): void => {
      if (!this.overviewOpen) return;
      this.overviewOpen = false;
      this.tabSwitcher.classList.remove("is-open");
      this.brandTrigger.setAttribute("aria-expanded", "false");
      this.tabOverview.setAttribute("aria-hidden", "true");
      void invoke("set_tab_overlay_height", { height: null }).catch((error) => this.showToast(String(error)));
    };
    if (immediate) close();
    else this.closeTimer = window.setTimeout(close, 190);
  }

  private tabFallback(tab: TabSummary): HTMLElement {
    const fallback = document.createElement("span");
    fallback.className = "tab-favicon-fallback";
    fallback.setAttribute("aria-hidden", "true");
    const first = Array.from(tab.title.trim() || "F")[0] ?? "F";
    fallback.textContent = first.toLocaleUpperCase();
    return fallback;
  }

  private render(): void {
    this.tabList.replaceChildren();
    for (const tab of this.tabs) {
      const row = document.createElement("div");
      row.className = `tab-pill${tab.active ? " is-active" : ""}${tab.loading ? " tab-loading" : ""}`;

      const main = document.createElement("button");
      main.type = "button";
      main.className = "tab-main";
      main.title = tab.url || "New tab";
      main.setAttribute("aria-label", `Switch to ${tab.title}`);
      if (tab.active) main.setAttribute("aria-current", "page");

      if (tab.favicon) {
        const icon = document.createElement("img");
        icon.className = "tab-favicon";
        icon.src = tab.favicon;
        icon.alt = "";
        icon.addEventListener("error", () => icon.replaceWith(this.tabFallback(tab)), { once: true });
        main.append(icon);
      } else {
        main.append(this.tabFallback(tab));
      }

      const title = document.createElement("span");
      title.className = "tab-title";
      title.textContent = tab.title || "New Tab";
      main.append(title);
      main.addEventListener("click", () => {
        this.closeOverview(true);
        void invoke("activate_tab", { id: tab.id }).catch((error) => this.showToast(String(error)));
      });

      const close = document.createElement("button");
      close.type = "button";
      close.className = "tab-close";
      close.textContent = "×";
      close.title = `Close ${tab.title}`;
      close.setAttribute("aria-label", `Close ${tab.title}`);
      close.addEventListener("click", (event) => {
        event.stopPropagation();
        void invoke("close_tab", { id: tab.id }).catch((error) => this.showToast(String(error)));
      });

      row.append(main, close);
      this.tabList.append(row);
    }
  }

  private applyTabs(nextTabs: TabSummary[]): void {
    this.tabs = nextTabs;
    const active = this.tabs.find((tab) => tab.active) ?? null;
    const changed = active?.id !== this.activeTabId || (active?.url ?? "") !== this.activeTabUrl;
    this.activeTabId = active?.id ?? null;
    this.activeTabUrl = active?.url ?? "";
    if (changed) this.addressInput.value = this.activeTabUrl;
    this.loadIndicator.classList.toggle("is-loading", active?.loading ?? false);

    if (this.blankTabFocusPending && active && active.id !== this.blankTabPreviousId && !active.url) {
      this.blankTabFocusPending = false;
      this.blankTabPreviousId = null;
      this.addressInput.value = "";
      void getCurrentWebview()
        .setFocus()
        .catch(() => undefined)
        .finally(() => this.addressInput.focus());
    }
    if (this.overviewOpen) {
      this.render();
      window.requestAnimationFrame(() => this.syncOverlayHeight());
    }
  }

  private syncOverlayHeight(): void {
    if (!this.overviewOpen) return;
    const height = Math.ceil(this.tabOverview.getBoundingClientRect().bottom + 12);
    void invoke("set_tab_overlay_height", { height }).catch((error) => this.showToast(String(error)));
  }
}
