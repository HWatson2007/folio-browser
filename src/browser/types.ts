export type NavigationStatus = "attempted" | "started" | "completed";

export interface NavigationEvent {
  tabId: number;
  url: string;
  status: NavigationStatus;
  title: string | null;
}

export interface TabSummary {
  id: number;
  title: string;
  url: string;
  favicon: string | null;
  loading: boolean;
  active: boolean;
}

export interface HistoryEntry {
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

export interface HistoryPage {
  entries: HistoryEntry[];
  total: number;
}

export type DownloadStatus =
  | "requested"
  | "downloading"
  | "completed"
  | "failed"
  | "canceled"
  | "interrupted";

export interface DownloadEntry {
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

export interface ProfileSummary {
  id: string;
  name: string;
  color: string;
  createdAt: number;
  lastUsedAt: number | null;
  running: boolean;
}

export type ShowToast = (message: string) => void;
