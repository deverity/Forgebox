// Acceptance (M2-E1-T9 #5): every ERROR_CODES entry (plus the client-only codes) and every
// FailureReasonDto kind has a cause in pt-BR and en-US; port_in_use, needs_elevation,
// not_implemented and unauthorized have at least one action, each with its `forge` command.
import { beforeAll, describe, expect, expectTypeOf, it } from "vitest";
import { i18n } from "../i18n";
import {
  DESCRIBED_CODES,
  DESCRIBED_REASONS,
  FAILURE_KINDS,
  FAILURE_KINDS_COMPLETE,
  describeError,
  translateError,
  type ErrorAction,
  type ErrorDescription,
  type ErrorTranslate,
} from "./describeError";
import { ForgeApiError } from "./errors";
import type * as T from "./generated";
import { ERROR_CODES } from "./methods";

const LOCALES = ["pt-BR", "en-US"] as const;
let translators: Record<(typeof LOCALES)[number], ErrorTranslate>;

beforeAll(async () => {
  await i18n.loadLanguages([...LOCALES]);
  await i18n.loadNamespaces("errors");
  translators = {
    "pt-BR": i18n.getFixedT("pt-BR", "errors") as unknown as ErrorTranslate,
    "en-US": i18n.getFixedT("en-US", "errors") as unknown as ErrorTranslate,
  };
});

/** Args the daemon sends with each code (ADR-0039 table); the rest have none. */
const CODE_ARGS: Record<string, Record<string, unknown>> = {
  port_in_use: { port: 3306, owner: "mysqld.exe (PID 4412)" },
  needs_elevation: { detail: "dns enable" },
  not_found: { what: "site loja.test" },
  not_implemented: { method: "sites.share", task: "M3-E2-T1", what: "tunnels" },
  timeout: { operation: "stop mysql/default", after_ms: 10_000 },
};

const FAILURES: Record<(typeof FAILURE_KINDS)[number], T.FailureReasonDto> = {
  port_in_use: { kind: "port_in_use", port: 3306, pid: 4412, exe: "mysqld.exe" },
  missing_dll: { kind: "missing_dll", exit_code: -1073741515 },
  config_invalid: { kind: "config_invalid", stderr: "nginx: [emerg] unknown directive" },
  missing_runtime: {
    kind: "missing_runtime",
    flavor: "vs17",
    download_url: "https://aka.ms/vs/17/release/vc_redist.x64.exe",
    installed: "14.29.30133.0",
  },
  dependency_not_running: { kind: "dependency_not_running", dependency: "mysql", state: "stopped" },
  permission: { kind: "permission", message: "access denied" },
  health_timeout: { kind: "health_timeout", timeout_ms: 30_000, last_error: "connection refused" },
  exited_during_start: { kind: "exited_during_start", exit_code: 1 },
  crash_loop: { kind: "crash_loop", restarts: 5, window_ms: 60_000, exit_code: 3 },
  other: { kind: "other", code: "io", message: "disk full" },
};

function expectTranslated(description: ErrorDescription, label: string): void {
  for (const locale of LOCALES) {
    const t = translators[locale];
    const exists = (key: string) => i18n.exists(key, { lng: locale, ns: "errors" });
    expect(exists(description.cause.key), `${label}: ${locale} ${description.cause.key}`).toBe(
      true,
    );
    const text = translateError(description, t);
    expect(text.cause.trim(), label).not.toBe("");
    expect(text.cause, `${label} ${locale}: placeholder left`).not.toMatch(/\{\w+\}/);
    for (const action of text.actions) {
      expect(exists(action.label.key), `${label}: ${locale} ${action.label.key}`).toBe(true);
      expect(action.text, `${label}: ${action.id}`).not.toMatch(/\{\w+\}/);
    }
  }
}

describe("describeError", () => {
  it("knows every kind of the generated FailureReasonDto", () => {
    expect(FAILURE_KINDS_COMPLETE).toBe(true);
    expect(DESCRIBED_CODES).toEqual(expect.arrayContaining([...ERROR_CODES, "disconnected"]));
  });

  it.each(DESCRIBED_CODES.map((code) => [code]))("%s: cause in pt-BR and en-US", (code) => {
    const args = CODE_ARGS[code];
    const withArgs = describeError({
      code,
      message: "english text never shown",
      ...(args === undefined ? {} : { details: { reason: code, args } }),
    });
    expect(withArgs.code).toBe(code);
    expect(withArgs.cause.key).not.toBe("unknown");
    expectTranslated(withArgs, code);
    // Without details (a 1.0 daemon, a client failure): still a full sentence.
    const bare = describeError({ code, message: "x" });
    expect(bare.cause.key).not.toBe("unknown");
    expectTranslated(bare, `${code} (bare)`);
  });

  it.each(FAILURE_KINDS.map((kind) => [kind]))("failure %s: cause in pt-BR and en-US", (kind) => {
    const failure = FAILURES[kind];
    const direct = describeError(failure, { target: "mysql/default", spec: "mysql@8.4" });
    expectTranslated(direct, kind);
    expect(direct.reason).toBe(kind === "other" ? null : kind);
    // The same diagnosis inside a service failure's details (`kind` + `{reason, args}`).
    const args = Object.fromEntries(Object.entries(failure).filter(([key]) => key !== "kind"));
    const wrapped = describeError({
      code: "internal",
      message: "x",
      details: { ...failure, reason: kind, args },
    });
    expect(wrapped.cause).toEqual(direct.cause);
  });

  it.each(DESCRIBED_REASONS.map((reason) => [reason]))("reason %s: own text", (reason) => {
    const args = {
      method: "sites.add",
      after_ms: 30_000,
      exit_code: 3,
      root: "C:/Forgebox",
      context: "hml",
    };
    const description = describeError({
      code: "disconnected",
      message: "x",
      details: { reason, args },
    });
    expect(description.cause.key).toBe(`reason.${reason}`);
    expectTranslated(description, reason);
  });

  it.each([["port_in_use"], ["needs_elevation"], ["not_implemented"], ["unauthorized"]])(
    "%s has at least one action, each with its forge command",
    (code) => {
      const description = describeError({ code, message: "x" });
      expect(description.actions.length).toBeGreaterThanOrEqual(1);
      for (const action of description.actions) {
        expect(action.cli, action.id).toMatch(/^forge /);
      }
    },
  );

  it("renders the commands from the method table", () => {
    const port = describeError(FAILURES.port_in_use, { target: "mysql/default" });
    expect(port.actions.map((a) => [a.id, a.cli])).toEqual([
      ["show_ports", "forge service ports"],
      ["change_port", null],
      ["start_service", "forge start mysql/default"],
    ]);
    const change = port.actions.find((a) => a.id === "change_port");
    expect(change?.input).toBe("port");
    expect(change?.bind?.(3307)).toEqual({
      params: { target: "mysql/default", settings: { "ports.main": "3307" } },
      cli: "forge service set mysql/default ports.main=3307",
    });
    const actions = (code: string) =>
      describeError({ code, message: "x" }).actions.map((a) => a.cli);
    expect(actions("not_implemented")).toEqual([
      "forge self-update --check",
      "forge daemon status",
    ]);
    expect(actions("unauthorized")).toEqual(["forge daemon restart", "forge doctor"]);
    expect(actions("needs_elevation")).toEqual(["forge doctor"]);
    expect(actions("disconnected")).toEqual(["forge daemon start"]);
    expect(describeError(FAILURES.dependency_not_running).actions[0]?.cli).toBe(
      "forge start mysql",
    );
    expect(describeError(FAILURES.missing_runtime).actions[0]?.href).toBe(
      "https://aka.ms/vs/17/release/vc_redist.x64.exe",
    );
  });

  it("builds action params in the method's contract shape", () => {
    // Type level (checked by `tsc`): an action's params are its method's params, so
    // `services.set` settings (`BTreeMap<String, String>`) cannot take a number.
    type SetParams = ReturnType<NonNullable<ErrorAction<"services.set">["bind"]>>["params"];
    expectTypeOf<SetParams>().toEqualTypeOf<T.ServiceSetParams>();
    expectTypeOf<
      ErrorAction<"services.start">["params"]
    >().toEqualTypeOf<T.ServiceTargetsParams | null>();
    // @ts-expect-error: a port is a string in `services.set` settings.
    const wrong: SetParams = { target: "mysql/default", settings: { "ports.main": 3307 } };
    expect(wrong.target).toBe("mysql/default");

    // Runtime: every bound value is a string (serde rejects a number there).
    const change = describeError(FAILURES.port_in_use, { target: "mysql/default" }).actions.find(
      (a) => a.id === "change_port",
    );
    expect(change?.method).toBe("services.set");
    for (const port of [1, 3307, 65535]) {
      const params = change?.bind?.(port).params as SetParams | undefined;
      expect(params?.settings).toEqual({ "ports.main": String(port) });
    }
  });

  it("offers to retry the failed call with its command", () => {
    const error = new ForgeApiError(
      {
        code: "needs_elevation",
        message: "x",
        details: { reason: "needs_elevation", args: { detail: "nrpt" } },
      },
      { method: "net.dns_enable", requestId: "01JREQ00000000000000000001" },
    );
    const description = describeError(error);
    expect(description.actions[0]).toMatchObject({
      id: "retry",
      method: "net.dns_enable",
      cli: "forge dns enable",
    });
    const withParams = describeError(
      { code: "conflict", message: "x" },
      { method: "services.start", params: { targets: ["mysql/default"] } },
    );
    expect(withParams.actions[0]?.cli).toBe("forge start mysql/default");
  });

  it("reads the 1.0 shape of port_in_use (port/owner at the top of details)", () => {
    const description = describeError({
      code: "port_in_use",
      message: "x",
      details: { port: 80, owner: "httpd.exe (PID 7)" },
    });
    expect(description.cause).toEqual({
      key: "code.port_in_use",
      values: { port: 80, owner: "httpd.exe (PID 7)" },
    });
  });

  it("speaks both languages", () => {
    const description = describeError(FAILURES.port_in_use);
    expect(translateError(description, translators["pt-BR"]).cause).toBe(
      "A porta 3306 está em uso por mysqld.exe (PID 4412).",
    );
    expect(translateError(description, translators["en-US"]).cause).toBe(
      "Port 3306 is in use by mysqld.exe (PID 4412).",
    );
  });

  it("describes unknown codes and non-API errors without crashing", () => {
    expect(describeError({ code: "from_the_future", message: "x" }).cause).toEqual({
      key: "unknown",
      values: { code: "from_the_future" },
    });
    expect(describeError(new Error("boom")).code).toBe("internal");
    expect(describeError("text").code).toBe("internal");
    expect(describeError(null).code).toBe("internal");
  });

  it("keeps the raw detail of a failed configuration check", () => {
    expect(describeError(FAILURES.config_invalid).detail).toBe("nginx: [emerg] unknown directive");
  });
});
