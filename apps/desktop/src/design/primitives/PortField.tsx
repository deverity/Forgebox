import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { PortCheckDto } from "../../ipc/generated";
import { useCapability, useForgeApi } from "../../state";
import { TextField, type TextFieldProps } from "./TextField";

/** Debounce of `net.port_check` while typing. */
export const PORT_CHECK_DEBOUNCE_MS = 250;

export interface PortFieldProps extends Omit<TextFieldProps, "value" | "onChange" | "hint"> {
  value: string;
  onChange: (value: string) => void;
  protocol?: "tcp" | "udp";
}

/** A valid TCP/UDP port number, or `null`. */
export function parsePort(text: string): number | null {
  if (!/^\d{1,5}$/.test(text)) return null;
  const port = Number(text);
  return port >= 1 && port <= 65535 ? port : null;
}

type Check =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "done"; result: PortCheckDto }
  | { kind: "failed" };

/**
 * Port input with a live check: `net.port_check` runs 250 ms after the last keystroke (one
 * request per window) and the hint says "livre" or "em uso por mysqld.exe (pid)".
 */
export function PortField({ value, onChange, protocol, ...rest }: PortFieldProps) {
  const { t } = useTranslation("common");
  const api = useForgeApi();
  const capability = useCapability("net.port_check");
  const available = capability.available;
  const port = parsePort(value);
  // The answer belongs to the (port, protocol) it was asked for; any other value is "checking".
  const key = `${String(port)}/${protocol ?? ""}`;
  const [answer, setAnswer] = useState<{ key: string; check: Check } | null>(null);
  let check: Check = { kind: "idle" };
  if (port !== null && available) {
    check = answer?.key === key ? answer.check : { kind: "checking" };
  }

  useEffect(() => {
    if (port === null || !available) return undefined;
    let stale = false;
    const timer = setTimeout(() => {
      api.call("net.port_check", { port, ...(protocol === undefined ? {} : { protocol }) }).then(
        (result) => {
          if (!stale) setAnswer({ key, check: { kind: "done", result } });
        },
        () => {
          if (!stale) setAnswer({ key, check: { kind: "failed" } });
        },
      );
    }, PORT_CHECK_DEBOUNCE_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [api, available, port, protocol, key]);

  let hint: string | undefined;
  if (check.kind === "checking") hint = t("ui.portChecking");
  else if (check.kind === "done") {
    const { free, holder, reserved_by: owner } = check.result;
    if (holder) hint = t("ui.portHeldBy", { exe: holder.exe, pid: holder.pid });
    else if (free) hint = t("ui.portFree");
    else if (owner) hint = t("ui.portReserved", { owner });
    else hint = t("ui.portBusy");
  }
  const invalid = value !== "" && port === null ? t("ui.portInvalid") : undefined;

  return (
    <div
      data-port-field={check.kind === "done" ? (check.result.free ? "free" : "busy") : check.kind}
    >
      <TextField
        width="sm"
        {...rest}
        inputMode="numeric"
        value={value}
        onChange={(event) => {
          onChange(event.target.value);
        }}
        {...(invalid === undefined ? {} : { error: invalid })}
        {...(hint === undefined ? {} : { hint })}
      />
    </div>
  );
}
