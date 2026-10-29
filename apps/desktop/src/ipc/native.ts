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
