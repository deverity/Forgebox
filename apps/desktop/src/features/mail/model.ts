// Pure helpers of the Mail screen: the sandboxed HTML document, the links found in a message
// and the `.env` text. No React, no IPC.
//
// Threat model (docs/11 section 6): message HTML is attacker-controlled. Three independent
// layers keep it harmless: (1) the iframe `sandbox` has no `allow-scripts` and no
// `allow-same-origin`, so nothing runs and the frame has an opaque origin without `__TAURI__`;
// (2) the document carries a CSP meta (`default-src 'none'`) that blocks every request except
// `data:`/`cid:` images and inline styles; remote images are added to `img-src` only after the
// user flips the per-message toggle; (3) the markup is rewritten through an inert `DOMParser`
// document: active elements are dropped and anchors lose their `href`, so a click inside the
// frame can never navigate or open a window. Links are listed by the screen instead and open
// in the system browser after the user confirms the URL.

/** `sandbox` of the preview frame: popups only, never scripts nor same-origin. */
export const FRAME_SANDBOX = "allow-popups allow-popups-to-escape-sandbox";

/** CSP of the preview with remote images blocked (the default). */
export const CSP_BLOCKED = "default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'";
/** CSP once the user allowed remote images of this message. */
export const CSP_REMOTE =
  "default-src 'none'; img-src data: cid: http: https:; style-src 'unsafe-inline'";

/** Largest `srcdoc` rendered, in UTF-16 code units; bigger messages fall back to the text. */
export const MAX_SRCDOC_CHARS = 512 * 1024;

const REMOVED_ELEMENTS = "script, iframe, frame, frameset, object, embed, base, meta, link, form";
const URL_ATTRS = ["src", "srcset", "background", "poster", "data", "action", "formaction"];
const HTTP_URL = /^https?:\/\//i;

export interface PreparedHtml {
  /** The complete document for `iframe.srcdoc`; `null` when it exceeds the cap. */
  srcdoc: string | null;
  /** The document would be bigger than `MAX_SRCDOC_CHARS`. */
  tooLarge: boolean;
  /** Distinct `http(s)` links of the message, in order of appearance. */
  links: string[];
  /** Images that point at a remote host (blocked until the toggle). */
  remoteImages: number;
}

function isRemote(value: string): boolean {
  const trimmed = value.trim();
  return HTTP_URL.test(trimmed) || trimmed.startsWith("//");
}

/**
 * Rewrites `html` for the sandboxed frame and wraps it in a document with the CSP. Never
 * throws: markup the parser cannot make sense of simply ends up as text.
 */
export function prepareHtml(html: string, allowRemoteImages: boolean): PreparedHtml {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const links: string[] = [];
  for (const anchor of doc.querySelectorAll("a[href], area[href]")) {
    const href = anchor.getAttribute("href") ?? "";
    if (HTTP_URL.test(href.trim()) && !links.includes(href.trim())) links.push(href.trim());
    anchor.removeAttribute("href");
    anchor.removeAttribute("target");
    anchor.removeAttribute("ping");
  }
  for (const element of doc.querySelectorAll(REMOVED_ELEMENTS)) element.remove();
  let remoteImages = 0;
  for (const element of doc.querySelectorAll("*")) {
    for (const attr of [...element.attributes]) {
      if (/^on/i.test(attr.name)) element.removeAttribute(attr.name);
    }
    if (element.tagName === "IMG") {
      const src = element.getAttribute("src") ?? "";
      if (isRemote(src) || isRemote(element.getAttribute("srcset") ?? "")) remoteImages += 1;
    }
    if (element.tagName !== "IMG") {
      for (const name of URL_ATTRS) {
        const value = element.getAttribute(name);
        if (value !== null && isRemote(value)) element.removeAttribute(name);
      }
    }
  }
  const csp = allowRemoteImages ? CSP_REMOTE : CSP_BLOCKED;
  const styles = [...doc.head.querySelectorAll("style")].map((style) => style.outerHTML).join("");
  const srcdoc =
    `<!doctype html><html><head><meta charset="utf-8">` +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    `<style>html{color-scheme:light;background:Canvas;color:CanvasText}body{margin:0;padding:12px;font:14px/1.5 system-ui,sans-serif;word-wrap:break-word}` +
    `img{max-width:100%;height:auto}a{cursor:default}</style>${styles}</head>` +
    `<body>${doc.body.innerHTML}</body></html>`;
  const tooLarge = srcdoc.length > MAX_SRCDOC_CHARS;
  return { srcdoc: tooLarge ? null : srcdoc, tooLarge, links, remoteImages };
}

/** `KEY=value` lines for a `.env` file, in the daemon's order. */
export function envText(vars: Readonly<Record<string, string>>): string {
  return Object.entries(vars)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

/** Short display name of an address header (`Ana <ana@x.test>` becomes `Ana`). */
export function senderName(from: string): string {
  const named = /^\s*"?([^"<]+?)"?\s*<[^>]*>\s*$/.exec(from);
  const name = named?.[1]?.trim();
  return name !== undefined && name !== "" ? name : from.replace(/^<|>$/g, "").trim();
}

/** Display text of a header value list. */
export function headerLines(headers: Readonly<Record<string, readonly string[]>>): string[] {
  return Object.entries(headers).flatMap(([name, values]) =>
    values.map((value) => `${name}: ${value}`),
  );
}
