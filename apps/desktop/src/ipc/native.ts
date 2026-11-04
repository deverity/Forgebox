// Native helpers of the webview that go through Tauri plugins (clipboard, dialog). They are
// imported lazily so the initial bundle does not carry them, and degrade outside Tauri (QA
// builds in a plain browser): the clipboard falls back to `navigator.clipboard`, the dialog to
// `null` ("the user picked nothing").

function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in globalThis;
}

/** Copies `text`; resolves `false` when nothing could be copied. */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (inTauri()) {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(text);
      return true;
    }
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Reads the clipboard text (paste in the terminal); `null` when unavailable or denied. */
export async function pasteText(): Promise<string | null> {
  try {
    if (inTauri()) {
      const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
      return await readText();
    }
    return await navigator.clipboard.readText();
  } catch {
    return null;
  }
}

export type PickKind = "directory" | "file";

/** Native picker (docs/07 §6); `null` when cancelled or unavailable. Local daemons only. */
export async function pickPath(kind: PickKind, defaultPath?: string): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      directory: kind === "directory",
      multiple: false,
      ...(defaultPath ? { defaultPath } : {}),
    });
    return typeof picked === "string" ? picked : null;
  } catch {
    return null;
  }
}

/** Opens `url` in the default browser (opener plugin); a plain browser tab outside Tauri. */
export async function openUrl(url: string): Promise<boolean> {
  try {
    if (inTauri()) {
      const { openUrl: open } = await import("@tauri-apps/plugin-opener");
      await open(url);
      return true;
    }
    return globalThis.open(url, "_blank", "noopener") !== null;
  } catch {
    return false;
  }
}

/** Shows a file in Explorer (opener plugin); `false` outside Tauri or when it fails. */
export async function revealPath(path: string): Promise<boolean> {
  if (!inTauri()) return false;
  try {
    const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
    await revealItemInDir(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * Subscribes to files or folders dropped on the window (Tauri webview drag-drop, which gives
 * real paths; the DOM `drop` event does not). `onHover` tells when something is dragged over.
 * Returns the unsubscribe function; a no-op outside Tauri (QA builds, tests).
 */
export async function onPathsDropped(
  onDrop: (paths: string[]) => void,
  onHover?: (hovering: boolean) => void,
): Promise<() => void> {
  if (!inTauri()) {
    return () => undefined;
  }
  try {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    return await getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") onHover?.(true);
      else if (payload.type === "leave") onHover?.(false);
      else {
        onHover?.(false);
        onDrop(payload.paths);
      }
    });
  } catch {
    return () => undefined;
  }
}

/** Result of {@link saveTextFile}: where it went, or that the user cancelled. */
export type SaveOutcome = { saved: string } | { cancelled: true };

/**
 * Saves `contents` through the native "Save as" dialog and the `save_text_file` command
 * (dialog plugin `save` + a Rust write; the webview has no file system access). Outside Tauri
 * (QA builds in a plain browser) it downloads the file instead. Rejects when the write fails.
 */
export async function saveTextFile(defaultName: string, contents: string): Promise<SaveOutcome> {
  if (inTauri()) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: defaultName,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path === null) return { cancelled: true };
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("save_text_file", { path, contents });
    return { saved: path };
  }
  const url = URL.createObjectURL(new Blob([contents], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = defaultName;
  link.click();
  setTimeout(() => {
    URL.revokeObjectURL(url);
  }, 0);
  return { saved: defaultName };
}
