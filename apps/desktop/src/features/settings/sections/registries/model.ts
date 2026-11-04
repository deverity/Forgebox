// Pure helpers of Settings > Registries (docs/04 section 5, docs/05).
import type { RegistryDto } from "../../../../ipc/generated";

export type RegistryTrust = "official" | "third-party" | "unverified";

/** Reasons a typed source is refused before calling the daemon (`RegistrySource::parse`). */
export type UrlProblem = "empty" | "scheme" | "invalid";

const DRIVE_PATH = /^[A-Za-z]:[\\/]/;

/**
 * Mirrors the forms `forge registry add` takes: `http(s)://` (an `index.yaml` or a `.git`),
 * `git+<url>`, `file:///` or an absolute Windows path. The daemon stays the validator; this
 * only catches typos before the round trip.
 */
export function urlProblem(input: string): UrlProblem | undefined {
  const value = input.trim();
  if (value === "") return "empty";
  if (DRIVE_PATH.test(value) || value.startsWith("\\\\")) return undefined;
  const bare = value.startsWith("git+") ? value.slice(4) : value;
  let url: URL;
  try {
    url = new URL(bare);
  } catch {
    return "invalid";
  }
  if (!["http:", "https:", "file:"].includes(url.protocol)) return "scheme";
  if (url.protocol !== "file:" && url.hostname === "") return "invalid";
  return undefined;
}

/**
 * Badge of a registry. The daemon marks only `official`; a registry read over plain HTTP or
 * from a folder carries no transport guarantee, so it shows as unverified (the index signature
 * is still checked by the loader, docs/04 section 5).
 */
export function registryTrust(
  registry: Pick<RegistryDto, "official" | "url" | "kind">,
): RegistryTrust {
  if (registry.official) return "official";
  if (registry.kind === "dir" || registry.url.toLowerCase().startsWith("http://")) {
    return "unverified";
  }
  return "third-party";
}
