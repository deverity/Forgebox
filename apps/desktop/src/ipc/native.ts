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

export type SaveResult = { status: "saved"; path: string | null } | { status: "cancelled" };

/**
 * Saves a downloaded attachment. In Tauri the `save_attachment` command shows the native
 * "save as" dialog and writes the file (the webview never chooses the path); in a plain
 * browser (QA builds) it falls back to a download link.
 */
export async function saveAttachment(
  fileName: string,
  contentBase64: string,
  contentType: string,
): Promise<SaveResult> {
  if (inTauri()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const path = await invoke<string | null>("save_attachment", {
      fileName,
      contentBase64,
    });
    return path === null ? { status: "cancelled" } : { status: "saved", path };
  }
  const bytes = Uint8Array.from(atob(contentBase64), (char) => char.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([bytes], { type: contentType }));
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  link.click();
  URL.revokeObjectURL(url);
  return { status: "saved", path: null };
}
