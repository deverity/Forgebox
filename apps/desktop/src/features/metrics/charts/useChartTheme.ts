import { useEffect, useState } from "react";

const ROOT_ATTRIBUTES = ["data-theme", "data-vibrancy", "data-effect", "data-contrast", "style"];

/**
 * A string that changes whenever something that affects chart colours changes: the theme,
 * vibrancy or contrast attributes of <html> (appearance.ts), the system accent (`style`) and the
 * OS colour scheme. Charts re-read their palette when it changes.
 */
export function useThemeKey(): string {
  const [key, setKey] = useState(() => themeKey());
  useEffect(() => {
    const update = () => {
      setKey(themeKey());
    };
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ROOT_ATTRIBUTES,
    });
    const scheme =
      typeof window.matchMedia === "function"
        ? window.matchMedia("(prefers-color-scheme: dark)")
        : null;
    scheme?.addEventListener("change", update);
    return () => {
      observer.disconnect();
      scheme?.removeEventListener("change", update);
    };
  }, []);
  return key;
}

function themeKey(): string {
  const d = document.documentElement.dataset;
  const scheme =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  const accent = document.documentElement.style.getPropertyValue("--system-accent");
  return [d.theme, d.vibrancy, d.effect, d.contrast, scheme ? "dark" : "light", accent].join("|");
}
