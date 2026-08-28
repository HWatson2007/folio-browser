import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { byId } from "./browser/dom";
import { DownloadsController } from "./browser/downloads";
import { HistoryController } from "./browser/history";
import { TabsController } from "./browser/tabs";
import type { NavigationEvent, ProfileSummary } from "./browser/types";
import "./styles.css";

const toast = byId<HTMLElement>("toast");
const addressForm = byId<HTMLFormElement>("address-form");
const addressInput = byId<HTMLInputElement>("address-input");
const profileBadge = byId<HTMLElement>("profile-badge");
let toastTimer: number | undefined;
let resizeTimer: number | undefined;

function showToast(message: string): void {
  toast.textContent = message;
  toast.classList.add("is-visible");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 2800);
}

function invokeBrowser(command: string): void {
  void invoke(command).catch((error) => showToast(String(error)));
}

let tabs: TabsController;
let history: HistoryController;
let downloads: DownloadsController;

tabs = new TabsController(showToast, () => !history.isOpen && !downloads.isOpen);
history = new HistoryController(showToast, async () => {
  tabs.closeOverview(true);
  await downloads.close();
});
downloads = new DownloadsController(showToast, async () => {
  tabs.closeOverview(true);
  await history.close();
});

tabs.initialize();
history.initialize();
downloads.initialize();

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

byId<HTMLButtonElement>("back-button").addEventListener("click", () => invokeBrowser("go_back"));
byId<HTMLButtonElement>("forward-button").addEventListener("click", () => invokeBrowser("go_forward"));
byId<HTMLButtonElement>("reload-button").addEventListener("click", () => invokeBrowser("reload"));
byId<HTMLButtonElement>("home-button").addEventListener("click", () => invokeBrowser("navigate_home"));

window.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.key.toLocaleLowerCase() === "t") {
    event.preventDefault();
    void tabs.createBlank();
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "w") {
    event.preventDefault();
    tabs.closeActive();
  } else if (event.ctrlKey && event.key === "Tab") {
    event.preventDefault();
    tabs.cycle(event.shiftKey ? -1 : 1);
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "l") {
    event.preventDefault();
    tabs.focusAddress(false);
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "h") {
    event.preventDefault();
    void history.toggle();
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "j") {
    event.preventDefault();
    void downloads.toggle();
  } else if (event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    invokeBrowser("go_back");
  } else if (event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    invokeBrowser("go_forward");
  } else if (event.ctrlKey && event.key.toLocaleLowerCase() === "r") {
    event.preventDefault();
    invokeBrowser("reload");
  } else if (event.key === "Escape" && downloads.hasOpenWarning) {
    downloads.closeOpenWarning();
  } else if (event.key === "Escape" && history.isOpen) {
    void history.close();
  } else if (event.key === "Escape" && downloads.isOpen) {
    void downloads.close();
  } else if (event.key === "Escape" && tabs.isOverviewOpen) {
    tabs.closeOverview(true);
  }
});

function syncContentOffset(): void {
  window.clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    const toolbarHeight = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--toolbar-height"),
    );
    void invoke("set_content_offset", { offset: toolbarHeight }).catch((error) => showToast(String(error)));
  }, 80);
}

window.addEventListener("resize", () => {
  syncContentOffset();
  tabs.handleResize();
});

void listen<NavigationEvent>("browser:navigation", ({ payload }) => {
  tabs.handleNavigation(payload);
  history.markDirty();
});

void listen<string>("browser:shortcut", ({ payload }) => {
  switch (payload) {
    case "new-tab":
      void tabs.createBlank();
      break;
    case "close-tab":
      tabs.closeActive();
      break;
    case "next-tab":
    case "previous-tab":
      tabs.cycle(payload === "next-tab" ? 1 : -1);
      break;
    case "focus-address":
      tabs.focusAddress();
      break;
    case "reload":
      invokeBrowser("reload");
      break;
    case "back":
      invokeBrowser("go_back");
      break;
    case "forward":
      invokeBrowser("go_forward");
      break;
  }
});

void invoke<ProfileSummary>("get_current_profile")
  .then((profile) => {
    profileBadge.textContent = profile.name;
    profileBadge.title = `Current profile: ${profile.name}`;
    history.setProfileName(profile.name);
  })
  .catch((error) => showToast(String(error)));

syncContentOffset();
