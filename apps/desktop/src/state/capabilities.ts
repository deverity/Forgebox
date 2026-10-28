// What this connection may do (ADR-0009, ADR-0039): `METHODS[m].min_role` against
// `hello.role`, `local_only` against `hello.local`, and `hello.not_implemented`, checked in
// the daemon's own order (role, local_only, not implemented) so the reason shown is the one
// the daemon would answer. Unavailable methods carry an i18n text (`errors:capability.*`) for
// the disabled control's explanation.
import type { I18nText } from "../ipc/describeError";
import type * as T from "../ipc/generated";
import { METHODS, type MethodMeta } from "../ipc/methods";

export type CapabilityReason =
  "disconnected" | "unknown_method" | "role" | "local_only" | "not_implemented";

export interface Capability {
  method: string;
  available: boolean;
  reason: CapabilityReason | null;
  /** Explanation for the disabled control (`errors:capability.<reason>`); `null` when available. */
  text: I18nText | null;
}

const ROLE_RANK: Record<T.Role, number> = { viewer: 0, operator: 1, admin: 2 };

function meta(method: string): MethodMeta | undefined {
  return (METHODS as Record<string, MethodMeta | undefined>)[method];
}

function unavailable(
  method: string,
  reason: CapabilityReason,
  values: Record<string, string> = {},
): Capability {
  return { method, available: false, reason, text: { key: `capability.${reason}`, values } };
}

/** Whether `method` can be called on a connection that said `hello` (`null`: not connected). */
export function capabilityOf(method: string, hello: T.HelloDto | null): Capability {
  const info = meta(method);
  if (info === undefined) return unavailable(method, "unknown_method", { method });
  if (hello === null) return unavailable(method, "disconnected");
  if (ROLE_RANK[hello.role] < ROLE_RANK[info.min_role]) {
    return unavailable(method, "role", { role: info.min_role, current: hello.role });
  }
  if (info.local_only && !hello.local) return unavailable(method, "local_only");
  if ((hello.not_implemented ?? []).includes(method)) return unavailable(method, "not_implemented");
  return { method, available: true, reason: null, text: null };
}

/** Every method of the table with its capability (lists, palettes). */
export function capabilityTable(hello: T.HelloDto | null): Record<string, Capability> {
  return Object.fromEntries(
    Object.keys(METHODS).map((method) => [method, capabilityOf(method, hello)]),
  );
}
