// Pure logic of the onboarding wizard (docs/07 section 5.1): steps, stored choices, TLD
// validation and the commands the "Comandos equivalentes" panel lists. No React, no IPC.
import type { ForgeApiError } from "../../ipc/errors";

export const ONBOARDING_ROUTE = "/onboarding";
export const DEFAULT_ROOT = "C:/Forgebox";
export const DEFAULT_TLD = ".test";
/** Built-in "Vazio" card: no stack, nothing to download. */
export const EMPTY_STACK = "empty";
/** Tag that puts a stack on the onboarding cards (`registry/stacks/*.yaml`). */
export const ONBOARDING_TAG = "onboarding";

export const STEP_IDS = ["root", "stack", "options", "run"] as const;
export type StepId = (typeof STEP_IDS)[number];

export function isStepId(value: unknown): value is StepId {
  return typeof value === "string" && (STEP_IDS as readonly string[]).includes(value);
}

/** Steps shown: step 1 only exists for a local daemon whose root the user may choose. */
export function visibleSteps(showRoot: boolean): readonly StepId[] {
  return showRoot ? STEP_IDS : STEP_IDS.filter((id) => id !== "root");
}

/** Suffixes reserved for local use (docs/11 section 1, the helper's allowlist). */
export const LOCAL_TLD_SUFFIXES: readonly string[] = [
  "test",
  "localhost",
  "invalid",
  "example",
  "local",
  "internal",
  "home.arpa",
  "lan",
  "home",
  "corp",
];

/** `Test` and ` .TEST ` give `.test`; empty stays empty. */
export function normalizeTld(raw: string): string {
  const text = raw.trim().toLowerCase().replace(/^\.+/, "");
  return text === "" ? "" : `.${text}`;
}

export type TldProblem = "empty" | "syntax" | "reserved";

const LABEL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;

/** Why `raw` cannot be the TLD of local sites; `null` when it can. */
export function tldProblem(raw: string): TldProblem | null {
  const tld = normalizeTld(raw);
  if (tld === "") return "empty";
  const name = tld.slice(1);
  if (name.length > 63 || !name.split(".").every((label) => LABEL.test(label))) return "syntax";
  const local = LOCAL_TLD_SUFFIXES.some((suffix) => name === suffix || name.endsWith(`.${suffix}`));
  return local ? null : "reserved";
}

/** `C:/Forgebox` gives `C:/Forgebox/www` (the daemon's default parked directory). */
export function defaultParkedDir(root: string): string {
  const base = root.replaceAll("\\", "/").replace(/\/+$/, "");
  return `${base}/www`;
}

/** What the user decided; everything is persisted through `onboarding_set`. */
export interface Choices {
  root: string;
  parkedDir: string;
  /** The user typed the parked directory (otherwise it follows the root). */
  parkedEdited: boolean;
  tld: string;
  /** Stack id, `EMPTY_STACK`, or `null` before the choice. */
  stack: string | null;
  /** Overrides of the stack's versions: tool id to series. */
  versions: Record<string, string>;
  path: boolean;
  dns: boolean;
  ca: boolean;
  autostart: boolean;
  parity: boolean;
  /** Step-1 work already done, so going back and forth never repeats it. */
  applied: { root: string; tld: string; park: string };
}

export function defaultChoices(root: string = DEFAULT_ROOT): Choices {
  return {
    root,
    parkedDir: defaultParkedDir(root),
    parkedEdited: false,
    tld: DEFAULT_TLD,
    stack: null,
    versions: {},
    path: true,
    dns: true,
    ca: true,
    autostart: false,
    parity: true,
    applied: { root: "", tld: "", park: "" },
  };
}

function str(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}
function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}
function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}

/** Reads the stored choices over the defaults; unknown or mistyped keys are ignored. */
export function parseChoices(raw: Record<string, unknown>, root: string): Choices {
  const base = defaultChoices(str(raw.root, root));
  const edited = bool(raw.parkedEdited, false);
  const versions: Record<string, string> = {};
  for (const [tool, series] of Object.entries(record(raw.versions))) {
    if (typeof series === "string") versions[tool] = series;
  }
  const applied = record(raw.applied);
  return {
    root: base.root,
    parkedEdited: edited,
    parkedDir: edited ? str(raw.parkedDir, base.parkedDir) : defaultParkedDir(base.root),
    tld: str(raw.tld, base.tld),
    stack: typeof raw.stack === "string" ? raw.stack : null,
    versions,
    path: bool(raw.path, base.path),
    dns: bool(raw.dns, base.dns),
    ca: bool(raw.ca, base.ca),
    autostart: bool(raw.autostart, base.autostart),
    parity: bool(raw.parity, base.parity),
    applied: {
      root: str(applied.root, ""),
      tld: str(applied.tld, ""),
      park: str(applied.park, ""),
    },
  };
}

/** `tools.plan` params for the chosen stack with the user's version overrides. */
export function planParams(
  stack: string,
  versions: Readonly<Record<string, string>>,
): { stack: string; specs: string[] } {
  return {
    stack,
    specs: Object.entries(versions).map(([tool, series]) => `${tool}@${series}`),
  };
}

// --- step 4 --------------------------------------------------------------------------------

export const RUN_STEP_IDS = ["parity", "install", "path", "dns", "ca", "autostart"] as const;
export type RunStepId = (typeof RUN_STEP_IDS)[number];
export type RunStatus = "pending" | "running" | "done" | "failed" | "skipped";

export interface RunStep {
  id: RunStepId;
  status: RunStatus;
  error: ForgeApiError | null;
  /** Why it is `skipped`: the user, a refused UAC, or nothing to do. */
  skip: "user" | "elevation" | "nothing" | null;
}

/** The steps this run performs, in the order of the acceptance criteria. */
export function runPlan(
  choices: Choices,
  options: { pathAvailable: boolean; autostartAvailable: boolean },
): RunStepId[] {
  const steps: RunStepId[] = [];
  if (!choices.parity) steps.push("parity");
  steps.push("install");
  if (choices.path && options.pathAvailable) steps.push("path");
  if (choices.dns) steps.push("dns");
  if (choices.ca) steps.push("ca");
  if (choices.autostart && options.autostartAvailable) steps.push("autostart");
  return steps;
}

/** A refused UAC arrives as `needs_elevation` (the helper's answer, docs/11 section 1). */
export function isElevationRefused(error: ForgeApiError): boolean {
  return error.code === "needs_elevation" || error.reason === "needs_elevation";
}
