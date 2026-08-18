import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

interface ProfileSummary {
  id: string;
  name: string;
  color: string;
  createdAt: number;
  lastUsedAt: number | null;
  running: boolean;
}

interface DialogState {
  mode: "rename" | "delete";
  id: string;
  name: string;
}

const byId = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing element #${id}`);
  return element as T;
};

const profileList = byId<HTMLUListElement>("profile-list");
const emptyState = byId<HTMLElement>("empty-state");
const createForm = byId<HTMLFormElement>("create-form");
const createInput = byId<HTMLInputElement>("create-input");
const dialog = byId<HTMLElement>("dialog");
const dialogTitle = byId<HTMLElement>("dialog-title");
const dialogMessage = byId<HTMLElement>("dialog-message");
const dialogRename = byId<HTMLElement>("dialog-rename");
const renameInput = byId<HTMLInputElement>("rename-input");
const toast = byId<HTMLElement>("toast");

let profiles: ProfileSummary[] = [];
let dialogState: DialogState | null = null;
let toastTimer: number | undefined;

const relativeFormatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

function showToast(message: string): void {
  toast.textContent = message;
  toast.classList.add("is-visible");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 3200);
}

function relativeTime(timestamp: number): string {
  const seconds = Math.round((timestamp - Date.now()) / 1000);
  const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ["year", 60 * 60 * 24 * 365],
    ["month", 60 * 60 * 24 * 30],
    ["day", 60 * 60 * 24],
    ["hour", 60 * 60],
    ["minute", 60],
  ];
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return relativeFormatter.format(Math.round(seconds / size), unit);
  }
  return relativeFormatter.format(seconds, "second");
}

function metaLine(profile: ProfileSummary): string {
  if (profile.running) return "Open now";
  if (profile.lastUsedAt) return `Last used ${relativeTime(profile.lastUsedAt)}`;
  return "Never opened";
}

function renderProfiles(): void {
  profileList.replaceChildren();
  emptyState.hidden = profiles.length > 0;

  for (const profile of profiles) {
    const card = document.createElement("li");
    card.className = "profile-card";
    card.dataset.id = profile.id;

    const avatar = document.createElement("span");
    avatar.className = "profile-avatar";
    avatar.style.setProperty("--avatar-color", profile.color);
    avatar.textContent = profile.name.trim().charAt(0).toLocaleUpperCase() || "?";

    const info = document.createElement("div");
    info.className = "profile-info";

    const nameRow = document.createElement("div");
    nameRow.className = "profile-name-row";
    const name = document.createElement("span");
    name.className = "profile-name";
    name.textContent = profile.name;
    nameRow.append(name);
    if (profile.running) {
      const badge = document.createElement("span");
      badge.className = "running-badge";
      badge.textContent = "Open";
      nameRow.append(badge);
    }

    const meta = document.createElement("span");
    meta.className = "profile-meta";
    meta.textContent = metaLine(profile);

    info.append(nameRow, meta);

    const actions = document.createElement("div");
    actions.className = "profile-actions";
    const open = document.createElement("button");
    open.type = "button";
    open.className = "open-button";
    open.textContent = "Open";
    open.disabled = profile.running;
    if (profile.running) {
      open.title = "This profile is already open in another window";
    }
    open.addEventListener("click", () => void launchProfile(profile.id));
    const rename = document.createElement("button");
    rename.type = "button";
    rename.className = "quiet-button";
    rename.textContent = "Rename";
    rename.addEventListener("click", () => openRenameDialog(profile));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "quiet-button danger";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => openDeleteDialog(profile));
    actions.append(open, rename, remove);

    card.append(avatar, info, actions);
    profileList.append(card);
  }
}

async function refreshProfiles(): Promise<void> {
  try {
    profiles = await invoke<ProfileSummary[]>("list_profiles");
    renderProfiles();
  } catch (error) {
    showToast(String(error));
  }
}

async function launchProfile(id: string): Promise<void> {
  try {
    await invoke("launch_profile", { id });
    await refreshProfiles();
  } catch (error) {
    showToast(String(error));
  }
}

function openDialog(title: string, message: string, state: DialogState, showRename: boolean): void {
  dialogState = state;
  dialogTitle.textContent = title;
  dialogMessage.textContent = message;
  dialogRename.hidden = !showRename;
  if (showRename) {
    renameInput.value = state.name;
    window.setTimeout(() => {
      renameInput.focus();
      renameInput.select();
    }, 0);
  }
  dialog.hidden = false;
}

function openRenameDialog(profile: ProfileSummary): void {
  openDialog(
    `Rename “${profile.name}”`,
    "The profile keeps its data; only the display name changes.",
    { mode: "rename", id: profile.id, name: profile.name },
    true,
  );
}

function openDeleteDialog(profile: ProfileSummary): void {
  openDialog(
    `Delete “${profile.name}”?`,
    "Its cookies, cache, and browsing ledger will be permanently removed. This cannot be undone.",
    { mode: "delete", id: profile.id, name: profile.name },
    false,
  );
}

function closeDialog(): void {
  dialog.hidden = true;
  dialogState = null;
}

async function confirmDialog(): Promise<void> {
  if (!dialogState) return;
  const state = dialogState;
  try {
    if (state.mode === "rename") {
      await invoke("rename_profile", { id: state.id, name: renameInput.value });
    } else {
      await invoke("delete_profile", { id: state.id });
    }
    closeDialog();
    await refreshProfiles();
  } catch (error) {
    showToast(String(error));
  }
}

createForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const name = createInput.value.trim();
  if (!name) return;
  try {
    await invoke("create_profile", { name });
    createInput.value = "";
    await refreshProfiles();
  } catch (error) {
    showToast(String(error));
  }
});

byId<HTMLButtonElement>("dialog-cancel").addEventListener("click", closeDialog);
byId<HTMLButtonElement>("dialog-confirm").addEventListener("click", () => void confirmDialog());
dialog.addEventListener("click", (event) => {
  if (event.target === dialog) closeDialog();
});

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !dialog.hidden) {
    closeDialog();
  } else if (!dialog.hidden) {
    return;
  } else if (event.key === "Enter" && document.activeElement === createInput) {
    createForm.requestSubmit();
  }
});

void refreshProfiles();
