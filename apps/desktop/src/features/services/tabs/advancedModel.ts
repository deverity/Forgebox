// Pure model of the Avançado tab: the draft of `services.update` and the patch that carries
// only what changed (absent fields keep their value on the daemon).
import type { AdvancedPatch } from "../../../actions/services";
import type { InstanceAdvancedDto, RestartOn } from "../../../ipc/generated";

export interface AdvancedDraft {
  autostart: boolean;
  /** One argument per line. */
  args: string;
  /** One `KEY=value` per line. */
  env: string;
  on: RestartOn;
  max: string;
  windowSecs: string;
  version: string;
}

export const RESTART_ON: readonly RestartOn[] = ["never", "failure", "always"];

/** `KEY=value` lines, sorted by key. */
export function envText(env: Readonly<Record<string, string>>): string {
  return Object.keys(env)
    .sort()
    .map((key) => `${key}=${env[key] ?? ""}`)
    .join("\n");
}

export function draftOf(advanced: InstanceAdvancedDto, version: string): AdvancedDraft {
  return {
    autostart: advanced.autostart,
    args: (advanced.args ?? []).join("\n"),
    env: envText(advanced.env ?? {}),
    on: advanced.restart_policy.on,
    max: String(advanced.restart_policy.max),
    windowSecs: String(advanced.restart_policy.window_secs),
    version,
  };
}

/** Non-empty lines, trimmed. */
export function lines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

const ENV_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Parses the env text; `null` when a line is not `KEY=value`. */
export function parseEnv(text: string): Record<string, string> | null {
  const env: Record<string, string> = {};
  for (const line of lines(text)) {
    const eq = line.indexOf("=");
    if (eq <= 0) return null;
    const key = line.slice(0, eq).trim();
    if (!ENV_KEY.test(key)) return null;
    env[key] = line.slice(eq + 1);
  }
  return env;
}

export type AdvancedProblem = "env" | "max" | "window";

const POSITIVE = /^\d{1,9}$/;

/** What is wrong with the draft, per field; empty when it can be sent. */
export function advancedProblems(draft: AdvancedDraft): Partial<Record<AdvancedProblem, true>> {
  const problems: Partial<Record<AdvancedProblem, true>> = {};
  if (parseEnv(draft.env) === null) problems.env = true;
  if (!POSITIVE.test(draft.max)) problems.max = true;
  if (!POSITIVE.test(draft.windowSecs) || Number(draft.windowSecs) < 1) problems.window = true;
  return problems;
}

function sameList(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((item, index) => item === b[index]);
}

/** The `services.update` fields that differ from `base`; `{}` when nothing changed. */
export function advancedPatch(base: AdvancedDraft, draft: AdvancedDraft): AdvancedPatch {
  const patch: {
    autostart?: boolean;
    launch?: { args?: string[]; env?: Record<string, string> };
    restart_policy?: { on?: RestartOn; max?: number; window_secs?: number };
    version?: string;
  } = {};
  if (draft.autostart !== base.autostart) patch.autostart = draft.autostart;
  const launch: { args?: string[]; env?: Record<string, string> } = {};
  if (!sameList(lines(draft.args), lines(base.args))) launch.args = lines(draft.args);
  const env = parseEnv(draft.env);
  if (env !== null && envText(env) !== envText(parseEnv(base.env) ?? {})) launch.env = env;
  if (Object.keys(launch).length > 0) patch.launch = launch;
  const policy: { on?: RestartOn; max?: number; window_secs?: number } = {};
  if (draft.on !== base.on) policy.on = draft.on;
  if (draft.max !== base.max && POSITIVE.test(draft.max)) policy.max = Number(draft.max);
  if (draft.windowSecs !== base.windowSecs && POSITIVE.test(draft.windowSecs)) {
    policy.window_secs = Number(draft.windowSecs);
  }
  if (Object.keys(policy).length > 0) patch.restart_policy = policy;
  if (draft.version !== base.version) patch.version = draft.version;
  return patch;
}
