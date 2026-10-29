import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { Capability } from "../../state/capabilities";
import { describeError, translateError, type ErrorTranslate } from "../../ipc/describeError";
import { toForgeApiError } from "../../ipc/errors";

type LooseT = (key: string, values?: Record<string, string | number>) => string;

/** `errors`-namespace translator in the shape `translateError` expects. */
export function useErrorTranslate(): ErrorTranslate {
  const { t } = useTranslation("errors");
  return t as unknown as LooseT;
}

/** Turns anything thrown by an action into the localized cause shown in a toast. */
export function useErrorMessage(): (error: unknown) => string {
  const translate = useErrorTranslate();
  return useCallback(
    (error: unknown) => translateError(describeError(toForgeApiError(error)), translate).cause,
    [translate],
  );
}

/** The localized explanation of why `capability` is unavailable (`null` when available). */
export function useCapabilityReason(capability: Capability): string | null {
  const translate = useErrorTranslate();
  if (capability.text === null) return null;
  return translate(capability.text.key, capability.text.values);
}
