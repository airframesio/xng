import type { Backend, BackendEvent, ToolEvent } from "./types";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const opener = await import("@tauri-apps/plugin-opener");
  return {
    native: true,
    appInfo: () => invoke("app_info"),
    listDevices: () => invoke("list_devices", { filter: null }),
    resolveTune: (request) => invoke("resolve_tune", { request }),
    start: (request, onEvent) => {
      const onEventChannel = new Channel<BackendEvent>();
      onEventChannel.onmessage = onEvent;
      return invoke("start_session", { request, onEvent: onEventChannel });
    },
    stop: () => invoke("stop_session"),
    startTool: (request, onEvent) => {
      const onEventChannel = new Channel<ToolEvent>();
      onEventChannel.onmessage = onEvent;
      return invoke("start_tool", { request, onEvent: onEventChannel });
    },
    stopTool: () => invoke("stop_tool"),
    writeText: (path, contents) => invoke("write_text", { path, contents }),
    exportEntities: (format, path) => invoke("export_entities", { format, path }),
    setDecodeThreads: (threads) => invoke("set_decode_threads", { threads }),
    pickFile: async (title, filters) => {
      const r = await dialog.open({ title, multiple: false, directory: false, filters });
      return typeof r === "string" ? r : null;
    },
    pickSave: (title, defaultPath) => dialog.save({ title, defaultPath }),
    openUrl: (url) => opener.openUrl(url),
  };
}

let cached: Promise<Backend> | undefined;

/** The Rust engine inside the app, or a simulated one in a plain browser
 *  (`npm run dev` without Tauri), which is handy for working on the UI. */
export function backend(): Promise<Backend> {
  cached ??= isTauri ? tauriBackend() : import("./demo").then((m) => m.demoBackend());
  return cached;
}
