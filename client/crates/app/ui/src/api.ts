// Typed wrappers over the Tauri `invoke` bridge. The webview never talks to the
// daemon directly (no cross-origin fetch) — every call goes through a Rust
// command in client/crates/app/src/commands.rs.

import { invoke } from "@tauri-apps/api/core";
import type { Status, VersionReport } from "./types";

export function getStatus(): Promise<Status> {
  return invoke<Status>("get_status");
}

export function getVersions(): Promise<VersionReport> {
  return invoke<VersionReport>("get_versions");
}

export function examplesStatus(): Promise<unknown> {
  return invoke("examples_status");
}

export function downloadExamples(): Promise<{ message?: string; ok?: boolean }> {
  return invoke("download_examples");
}

export function runExample(id: string): Promise<void> {
  return invoke("run_example", { id });
}

export function stopExample(id: string): Promise<void> {
  return invoke("stop_example", { id });
}

export function startDaemon(): Promise<void> {
  return invoke("start_daemon");
}

export function stopDaemon(): Promise<void> {
  return invoke("stop_daemon");
}

export function restartDaemon(): Promise<void> {
  return invoke("restart_daemon");
}

export function setHttps(enabled: boolean): Promise<void> {
  return invoke("set_https", { enabled });
}

export function openExternal(url: string): Promise<void> {
  return invoke("open_external", { url });
}
