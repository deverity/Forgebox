// Errors of the IPC bridge (ADR-0034): every failure is the API's own
// `ApiError {code, message, details}`; `details` follows `{reason, args}` (ADR-0039).
// The UI localizes from `code` + `details`, never from the English `message`.
import { ERROR_CODES, type ErrorCode } from "./methods";

/**
 * Codes the client side invents (they never cross the wire): `disconnected` = the daemon
 * could not be reached or the connection dropped (after the retry with the same
 * `request_id`). `timeout` and `unauthorized` are daemon codes the bridge also uses.
 */
export const CLIENT_ERROR_CODES = ["disconnected"] as const;
export type ClientErrorCode = (typeof CLIENT_ERROR_CODES)[number];

/** Every code a bridge call can reject with. */
export type ForgeErrorCode = ErrorCode | ClientErrorCode;

/** `ApiError` as the bridge rejects with it. */
export interface ApiErrorDto {
  code: string;
  message: string;
  details?: unknown;
}

/** Where a failure happened (filled by `api.ts`). */
export interface ErrorOrigin {
  method?: string | undefined;
  requestId?: string | undefined;
}

export function isKnownErrorCode(code: string): code is ForgeErrorCode {
  return (
    (ERROR_CODES as readonly string[]).includes(code) ||
    (CLIENT_ERROR_CODES as readonly string[]).includes(code)
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** A failed call, job or stream. */
export class ForgeApiError extends Error {
  override readonly name = "ForgeApiError";
  /** Stable code (`ForgeErrorCode`; unknown codes of newer daemons pass through). */
  readonly code: string;
  readonly details: unknown;
  readonly method: string | undefined;
  readonly requestId: string | undefined;

  constructor(dto: ApiErrorDto, origin: ErrorOrigin = {}) {
    super(dto.message);
    this.code = dto.code;
    this.details = dto.details ?? null;
    this.method = origin.method;
    this.requestId = origin.requestId;
  }

  /** `details.reason`, the stable id the UI translates. */
  get reason(): string | undefined {
    const reason = isRecord(this.details) ? this.details.reason : undefined;
    return typeof reason === "string" ? reason : undefined;
  }

  /** `details.args` (empty when absent). */
  get args(): Record<string, unknown> {
    const args = isRecord(this.details) ? this.details.args : undefined;
    return isRecord(args) ? args : {};
  }

  is(code: ForgeErrorCode): boolean {
    return this.code === code;
  }

  toJSON(): ApiErrorDto {
    return { code: this.code, message: this.message, details: this.details };
  }
}

export function isForgeApiError(error: unknown): error is ForgeApiError {
  return error instanceof ForgeApiError;
}

/**
 * Normalizes whatever a bridge rejected with: an `ApiError` object (Tauri serializes the
 * Rust error), a `ForgeApiError`, a string (Tauri's own failures) or an `Error`.
 */
export function toForgeApiError(error: unknown, origin: ErrorOrigin = {}): ForgeApiError {
  if (error instanceof ForgeApiError) {
    if (error.method !== undefined || origin.method === undefined) return error;
    return new ForgeApiError(error.toJSON(), {
      ...origin,
      requestId: error.requestId ?? origin.requestId,
    });
  }
  if (isRecord(error) && typeof error.code === "string" && typeof error.message === "string") {
    return new ForgeApiError(
      { code: error.code, message: error.message, details: error.details },
      origin,
    );
  }
  if (typeof error === "string") {
    return new ForgeApiError({ code: "internal", message: error }, origin);
  }
  if (error instanceof Error) {
    return new ForgeApiError({ code: "internal", message: error.message }, origin);
  }
  return new ForgeApiError({ code: "internal", message: String(error) }, origin);
}

/** An `ApiErrorDto` with `{reason, args}` details. */
export function apiError(
  code: ForgeErrorCode,
  message: string,
  reason?: string,
  args: Record<string, unknown> = {},
): ApiErrorDto {
  return reason === undefined ? { code, message } : { code, message, details: { reason, args } };
}
