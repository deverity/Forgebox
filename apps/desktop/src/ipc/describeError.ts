// Cause + actions for any failure the UI shows (docs/07 §1.2: no error without a cause and
// a suggested action). The input is what the API gives: an `ApiError`/`ForgeApiError` (also
// a job's `JobErrorInfo`), or a service diagnosis (`FailureReasonDto`, also found inside
// `details` of service failures). The output is i18n keys of the `errors` namespace plus
// values — never the daemon's English `message` (ADR-0039) — and actions, each with the
// equivalent `forge` command rendered from the method table (ADR-0037).
//
// Precedence: a service diagnosis (`kind`) → a known `details.reason` → the `code` →
// `errors:unknown`. Key variants (`_plain`, `_exe`...) are chosen so every placeholder has
// a value. Actions name the method they would call (so `ActionButton` can gate them by
// capability and make a new intent for it) or a client-only step (`client: "start_daemon"`,
// a download link); `retry` repeats the failed call when the caller passes it.
import { renderCli } from "./cli";
import { CLIENT_ERROR_CODES, ForgeApiError, toForgeApiError, type ApiErrorDto } from "./errors";
import type * as T from "./generated";
import { ERROR_CODES, METHODS, type MethodName } from "./methods";

/** Translatable text: a key of the `errors` namespace and its placeholder values. */
export interface I18nText {
  key: string;
  values: Record<string, string | number>;
}

export const ERRORS_NS = "errors";

/** Kinds of `FailureReasonDto`; a kind missing here fails to compile. */
export const FAILURE_KINDS = [
  "port_in_use",
  "missing_dll",
  "config_invalid",
  "missing_runtime",
  "dependency_not_running",
  "permission",
  "health_timeout",
  "exited_during_start",
  "crash_loop",
  "other",
] as const satisfies readonly T.FailureReasonDto["kind"][];
export type FailureKind = (typeof FAILURE_KINDS)[number];
/** Compile-time exhaustiveness: `false` (a type error here) when a generated kind is missing. */
export const FAILURE_KINDS_COMPLETE: [Exclude<T.FailureReasonDto["kind"], FailureKind>] extends [
  never,
]
  ? true
  : false = true;

/** Error code of each diagnosis (as `forge_daemon::api::convert::failure_error`). */
export function failureCode(failure: T.FailureReasonDto): string {
  switch (failure.kind) {
    case "port_in_use":
      return "port_in_use";
    case "missing_dll":
    case "missing_runtime":
      return "not_found";
    case "config_invalid":
      return "config_invalid";
    case "dependency_not_running":
      return "conflict";
    case "permission":
      return "needs_elevation";
    case "health_timeout":
      return "timeout";
    case "other":
      return failure.code;
    default:
      return "internal";
  }
}

/** Every code `describeError` has a text for (daemon codes + client-only codes). */
export const DESCRIBED_CODES = [...ERROR_CODES, ...CLIENT_ERROR_CODES] as const;

/** `details.reason` values with their own text (bridge, client and queue reasons). */
export const DESCRIBED_REASONS = [
  "daemon_unreachable",
  "daemon_not_running",
  "connection_lost",
  "context_closed",
  "daemon_stopped",
  "reconnect_timeout",
  "no_answer",
  "bridge_busy",
  "token_unreadable",
  "transport",
  "protocol",
  "daemon_not_found",
  "daemon_spawn_failed",
  "daemon_exited",
  "daemon_start_timeout",
  "root_uninitialized",
  "unknown_context",
  "unknown_method",
  "job_cancelled",
] as const;
type DescribedReason = (typeof DESCRIBED_REASONS)[number];

/** Client-only commands (docs/09: "rodam só no cliente, sem método"). */
export const CLIENT_COMMANDS = { start_daemon: "forge daemon start" } as const;

export type ErrorActionId =
  | "retry"
  | "start_daemon"
  | "restart_daemon"
  | "show_ports"
  | "change_port"
  | "run_doctor"
  | "daemon_logs"
  | "service_logs"
  | "open_config"
  | "start_dependency"
  | "start_service"
  | "restart_service"
  | "reinstall"
  | "download_runtime"
  | "check_app_update"
  | "daemon_status"
  | "update_registry";

export interface ErrorAction {
  id: ErrorActionId;
  label: I18nText;
  /** The API method the action calls (`null`: client-side step or link). */
  method: MethodName | null;
  params: Record<string, unknown> | null;
  /** Equivalent `forge` command (`null` when there is none or it needs input first). */
  cli: string | null;
  /** Client-side step: start the context's daemon through the bridge. */
  client?: "start_daemon";
  /** External link (opened by the shell, e.g. a runtime download). */
  href?: string;
  /** The action needs a value first (`change_port`): `bind` gives the params and command. */
  input?: "port";
  bind?: (value: number) => { params: Record<string, unknown>; cli: string | null };
}

export interface ErrorDescription {
  code: string;
  /** `details.reason`, or the diagnosis kind. */
  reason: string | null;
  cause: I18nText;
  /** Raw, untranslated detail for "show details" (stderr of a config check...). */
  detail: string | null;
  actions: ErrorAction[];
}

/** What the caller knows about the failed operation (all optional). */
export interface ErrorContext {
  /** The call that failed and its params (enables `retry`). */
  method?: string;
  params?: unknown;
  /** Service target (`mysql/default`) the failure is about. */
  target?: string;
  /** Tool spec (`php@8.3`) the failure is about (enables `reinstall`). */
  spec?: string;
}

export type DescribableError =
  | ForgeApiError
  | ApiErrorDto
  | T.ApiError
  | T.JobErrorInfo
  | T.FailureReasonDto
  | Error
  | string
  | null
  | undefined;

// --- helpers -----------------------------------------------------------------------------

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isFailure(value: unknown): value is T.FailureReasonDto {
  return (
    isRecord(value) &&
    typeof value.kind === "string" &&
    (FAILURE_KINDS as readonly string[]).includes(value.kind)
  );
}

function text(key: string, values: Record<string, unknown> = {}): I18nText {
  const clean: Record<string, string | number> = {};
  for (const [name, value] of Object.entries(values)) {
    if (typeof value === "string" || typeof value === "number") clean[name] = value;
  }
  return { key, values: clean };
}

function has(values: Record<string, unknown>, ...names: string[]): boolean {
  return names.every((name) => {
    const value = values[name];
    return (typeof value === "string" && value !== "") || typeof value === "number";
  });
}

function seconds(ms: unknown): number | undefined {
  return typeof ms === "number" ? Math.max(1, Math.round(ms / 1000)) : undefined;
}

function isMethod(method: string | undefined): method is MethodName {
  return method !== undefined && Object.hasOwn(METHODS, method);
}

function methodAction(
  id: ErrorActionId,
  method: MethodName,
  params: Record<string, unknown>,
  values: Record<string, unknown> = {},
): ErrorAction {
  return {
    id,
    label: text(`action.${id}`, values),
    method,
    params,
    cli: renderCli(method, params),
  };
}

// --- actions -----------------------------------------------------------------------------

const A = {
  retry(ctx: ErrorContext): ErrorAction[] {
    if (!isMethod(ctx.method)) return [];
    if (isRecord(ctx.params)) return [methodAction("retry", ctx.method, ctx.params)];
    // Params unknown (only the error's method): retry only what takes none.
    const template = METHODS[ctx.method].cli_template as string | null;
    if (template === null || template.includes("{")) return [];
    return [methodAction("retry", ctx.method, {})];
  },
  startDaemon(): ErrorAction[] {
    return [
      {
        id: "start_daemon",
        label: text("action.start_daemon"),
        method: null,
        params: null,
        cli: CLIENT_COMMANDS.start_daemon,
        client: "start_daemon",
      },
    ];
  },
  restartDaemon: () => [methodAction("restart_daemon", "daemon.restart", {})],
  showPorts: () => [methodAction("show_ports", "services.ports", {})],
  doctor: () => [methodAction("run_doctor", "daemon.doctor", {})],
  daemonLogs: () => [methodAction("daemon_logs", "logs.read", { target: "daemon" })],
  daemonStatus: () => [methodAction("daemon_status", "daemon.status", {})],
  appUpdate: () => [methodAction("check_app_update", "system.self_update", { check: true })],
  registry: () => [methodAction("update_registry", "registry.update", {})],
  serviceLogs(ctx: ErrorContext): ErrorAction[] {
    return ctx.target === undefined
      ? []
      : [methodAction("service_logs", "logs.read", { target: ctx.target })];
  },
  openConfig(ctx: ErrorContext): ErrorAction[] {
    return ctx.target === undefined
      ? []
      : [methodAction("open_config", "services.config", { target: ctx.target })];
  },
  startService(ctx: ErrorContext): ErrorAction[] {
    return ctx.target === undefined
      ? []
      : [methodAction("start_service", "services.start", { targets: [ctx.target] })];
  },
  restartService(ctx: ErrorContext): ErrorAction[] {
    return ctx.target === undefined
      ? []
      : [methodAction("restart_service", "services.restart", { targets: [ctx.target] })];
  },
  reinstall(ctx: ErrorContext): ErrorAction[] {
    return ctx.spec === undefined
      ? []
      : [methodAction("reinstall", "tools.install", { specs: [ctx.spec] }, { spec: ctx.spec })];
  },
  changePort(ctx: ErrorContext): ErrorAction[] {
    const target = ctx.target;
    if (target === undefined) return [];
    const bind = (port: number) => {
      const params = { target, settings: { "ports.main": port } };
      return { params, cli: renderCli("services.set", params) };
    };
    return [
      {
        id: "change_port",
        label: text("action.change_port"),
        method: "services.set",
        params: null,
        cli: null,
        input: "port",
        bind,
      },
    ];
  },
};

function codeActions(code: string, reason: string | null, ctx: ErrorContext): ErrorAction[] {
  switch (code) {
    case "port_in_use":
      return [...A.showPorts(), ...A.changePort(ctx), ...A.retry(ctx)];
    case "needs_elevation":
      return [...A.retry(ctx), ...A.doctor()];
    case "not_implemented":
      return [...A.appUpdate(), ...A.daemonStatus()];
    case "unauthorized":
      return [...A.restartDaemon(), ...A.doctor()];
    case "disconnected":
      return reason === "context_closed" ? A.retry(ctx) : [...A.startDaemon(), ...A.retry(ctx)];
    case "timeout":
      return reason === "daemon_start_timeout"
        ? [...A.daemonLogs(), ...A.doctor()]
        : [...A.retry(ctx), ...A.doctor()];
    case "verification_failed":
      return [...A.registry(), ...A.retry(ctx)];
    case "io":
      return reason === "daemon_exited" || reason === "daemon_spawn_failed"
        ? [...A.daemonLogs(), ...A.doctor()]
        : [...A.retry(ctx), ...A.daemonLogs()];
    case "db":
      return [...A.daemonLogs(), ...A.restartDaemon()];
    case "internal":
      return [...A.retry(ctx), ...A.daemonLogs()];
    case "config_invalid":
      return [...A.openConfig(ctx), ...A.serviceLogs(ctx), ...A.doctor()];
    case "not_found":
      return reason === "daemon_not_found" ? A.doctor() : A.retry(ctx);
    case "conflict":
    case "cancelled":
      return A.retry(ctx);
    default:
      // invalid_input, unsupported_platform, forbidden: nothing to do but change the input.
      return [];
  }
}

// --- causes ------------------------------------------------------------------------------

function codeCause(code: string, args: Record<string, unknown>): I18nText {
  switch (code) {
    case "not_found":
      return has(args, "what") ? text("code.not_found", args) : text("code.not_found_plain");
    case "port_in_use": {
      if (has(args, "port", "owner")) return text("code.port_in_use", args);
      return has(args, "port")
        ? text("code.port_in_use_plain", args)
        : text("code.port_in_use_any");
    }
    case "needs_elevation":
      return has(args, "detail")
        ? text("code.needs_elevation", args)
        : text("code.needs_elevation_plain");
    case "timeout": {
      const values = { ...args, seconds: seconds(args.after_ms) };
      return has(values, "operation", "seconds")
        ? text("code.timeout", values)
        : text("code.timeout_plain");
    }
    case "not_implemented":
      if (has(args, "what", "task")) return text("code.not_implemented", args);
      return has(args, "what")
        ? text("code.not_implemented_what", args)
        : text("code.not_implemented_plain");
    default:
      return (DESCRIBED_CODES as readonly string[]).includes(code)
        ? text(`code.${code}`)
        : text("unknown", { code });
  }
}

const REASON_PLACEHOLDERS: Partial<Record<DescribedReason, readonly string[]>> = {
  reconnect_timeout: ["seconds"],
  no_answer: ["seconds"],
  daemon_exited: ["exit_code"],
  daemon_start_timeout: ["seconds"],
  root_uninitialized: ["root"],
  unknown_context: ["context"],
};

function reasonCause(reason: string, args: Record<string, unknown>): I18nText | null {
  if (!(DESCRIBED_REASONS as readonly string[]).includes(reason)) return null;
  const values = { ...args, seconds: seconds(args.after_ms) };
  const needed = REASON_PLACEHOLDERS[reason as DescribedReason] ?? [];
  return has(values, ...needed) ? text(`reason.${reason}`, values) : null;
}

/** The diagnosis fields as placeholder values (without `kind` and the `{reason, args}` copy). */
function failureValues(failure: T.FailureReasonDto): Record<string, unknown> {
  const values: Record<string, unknown> = { ...failure };
  delete values.kind;
  delete values.reason;
  delete values.args;
  return values;
}

function failureCause(failure: T.FailureReasonDto): I18nText {
  const values = failureValues(failure);
  switch (failure.kind) {
    case "port_in_use":
      if (failure.exe !== null && failure.pid !== null) return text("failure.port_in_use", values);
      if (failure.exe !== null) return text("failure.port_in_use_exe", values);
      return text("failure.port_in_use_plain", values);
    case "missing_runtime":
      return failure.installed === null
        ? text("failure.missing_runtime", values)
        : text("failure.missing_runtime_old", values);
    case "health_timeout":
      return text("failure.health_timeout", { seconds: seconds(failure.timeout_ms) });
    case "crash_loop":
      return text("failure.crash_loop", {
        restarts: failure.restarts,
        seconds: seconds(failure.window_ms),
      });
    case "config_invalid":
    case "permission":
    case "other":
      return text(`failure.${failure.kind}`);
    default:
      return text(`failure.${failure.kind}`, values);
  }
}

function failureActions(failure: T.FailureReasonDto, ctx: ErrorContext): ErrorAction[] {
  switch (failure.kind) {
    case "port_in_use":
      return [...A.showPorts(), ...A.changePort(ctx), ...A.startService(ctx)];
    case "missing_dll":
      return [...A.reinstall(ctx), ...A.doctor()];
    case "config_invalid":
      return [...A.openConfig(ctx), ...A.serviceLogs(ctx), ...A.doctor()];
    case "missing_runtime":
      return [
        {
          id: "download_runtime",
          label: text("action.download_runtime"),
          method: null,
          params: null,
          cli: null,
          href: failure.download_url,
        },
        ...A.doctor(),
      ];
    case "dependency_not_running": {
      const params = { targets: [failure.dependency] };
      return [
        methodAction("start_dependency", "services.start", params, {
          dependency: failure.dependency,
        }),
      ];
    }
    case "permission":
      return [...A.serviceLogs(ctx), ...A.doctor()];
    case "health_timeout":
      return [...A.serviceLogs(ctx), ...A.restartService(ctx), ...A.doctor()];
    case "exited_during_start":
      return [...A.serviceLogs(ctx), ...A.startService(ctx), ...A.doctor()];
    case "crash_loop":
      return [...A.serviceLogs(ctx), ...A.restartService(ctx)];
    case "other":
      return codeActions(failure.code, null, ctx);
  }
}

function failureDetail(failure: T.FailureReasonDto): string | null {
  switch (failure.kind) {
    case "config_invalid":
      return failure.stderr === "" ? null : failure.stderr;
    case "permission":
    case "other":
      return failure.message;
    case "health_timeout":
      return failure.last_error;
    default:
      return null;
  }
}

function describeFailure(
  failure: T.FailureReasonDto,
  code: string,
  ctx: ErrorContext,
): ErrorDescription {
  if (failure.kind === "other") {
    // A coded failure without a diagnosis of its own: describe it by its code.
    return {
      ...describeApiError({ code: failure.code, message: failure.message }, ctx),
      detail: failure.message,
    };
  }
  return {
    code,
    reason: failure.kind,
    cause: failureCause(failure),
    detail: failureDetail(failure),
    actions: failureActions(failure, ctx),
  };
}

function describeApiError(error: ApiErrorDto, ctx: ErrorContext): ErrorDescription {
  const forge = error instanceof ForgeApiError ? error : new ForgeApiError(error);
  const details = forge.details;
  if (isFailure(details)) return describeFailure(details, forge.code, ctx);
  const reason = forge.reason ?? null;
  const args = forge.args;
  // The 1.0 shape of `port_in_use` keeps `port`/`owner` at the top of `details`.
  const values = isRecord(details) ? { ...details, ...args } : args;
  const cause =
    (reason !== null && reason !== forge.code ? reasonCause(reason, values) : null) ??
    codeCause(forge.code, values);
  const context: ErrorContext = { ...ctx };
  if (context.method === undefined && forge.method !== undefined) context.method = forge.method;
  return {
    code: forge.code,
    reason,
    cause,
    detail: null,
    actions: codeActions(forge.code, reason, context),
  };
}

/** Cause and actions of a failure (see the module comment). */
export function describeError(error: DescribableError, ctx: ErrorContext = {}): ErrorDescription {
  if (isFailure(error)) return describeFailure(error, failureCode(error), ctx);
  if (error instanceof ForgeApiError) return describeApiError(error, ctx);
  if (isRecord(error) && typeof error.code === "string" && typeof error.message === "string") {
    return describeApiError(
      { code: error.code, message: error.message, details: error.details ?? null },
      ctx,
    );
  }
  return describeApiError(toForgeApiError(error ?? "unknown error"), ctx);
}

/** Any `t` bound to the `errors` namespace (`i18n.getFixedT(lng, "errors")`, `useTranslation`). */
export type ErrorTranslate = (key: string, values?: Record<string, string | number>) => string;

export interface TranslatedError {
  cause: string;
  detail: string | null;
  actions: (ErrorAction & { text: string })[];
}

/** The texts of a description in the language of `t`. */
export function translateError(description: ErrorDescription, t: ErrorTranslate): TranslatedError {
  return {
    cause: t(description.cause.key, description.cause.values),
    detail: description.detail,
    actions: description.actions.map((action) => ({
      ...action,
      text: t(action.label.key, action.label.values),
    })),
  };
}
