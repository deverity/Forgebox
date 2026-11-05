// Whether the window is visible and focused: drives the frame batcher (no rAF while hidden)
// and the metrics poll (only while a consumer is visible and focused, ADR-0034 state layer).
// Tests inject `manualPageActivity`.

export interface PageActivity {
  /** `document.visibilityState === "visible"` (WebView2 reports minimized windows as hidden). */
  visible(): boolean;
  /** The window has keyboard focus. */
  focused(): boolean;
  /** Called on every visibility or focus change; returns the unsubscribe function. */
  subscribe(listener: () => void): () => void;
}

/** The real page: `visibilitychange` on the document, `focus`/`blur` on the window. */
export function browserPageActivity(doc: Document = document, win: Window = window): PageActivity {
  return {
    visible: () => doc.visibilityState !== "hidden",
    focused: () => doc.hasFocus(),
    subscribe(listener) {
      const handler = () => {
        listener();
      };
      doc.addEventListener("visibilitychange", handler);
      win.addEventListener("focus", handler);
      win.addEventListener("blur", handler);
      return () => {
        doc.removeEventListener("visibilitychange", handler);
        win.removeEventListener("focus", handler);
        win.removeEventListener("blur", handler);
      };
    },
  };
}

/** A page under test control. */
export interface ManualPageActivity extends PageActivity {
  set(state: { visible?: boolean; focused?: boolean }): void;
}

export function manualPageActivity(visible = true, focused = true): ManualPageActivity {
  const state = { visible, focused };
  const listeners = new Set<() => void>();
  return {
    visible: () => state.visible,
    focused: () => state.focused,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    set(next) {
      if (next.visible !== undefined) state.visible = next.visible;
      if (next.focused !== undefined) state.focused = next.focused;
      for (const listener of [...listeners]) listener();
    },
  };
}
