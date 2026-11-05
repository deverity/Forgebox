// PHP screen actions (docs/07 §5.5). Registered here so the ESLint rule
// `forgebox/no-direct-mutation` and the parity gate see them.
import type { XdebugMode } from "../ipc/generated";
import { defineAction } from "./registry";

/** What the extension toggle sends: the series (or exact version) and the extension name. */
export interface PhpExtensionInput {
  version: string;
  extension: string;
}

/** Turns an extension on (`forge php ext enable 8.4 intl`). */
export const enablePhpExtension = defineAction<"php.ext_enable", PhpExtensionInput>({
  id: "php.ext_enable",
  method: "php.ext_enable",
  toParams: ({ version, extension }) => ({ version, extension }),
});

/** Turns an extension off (`forge php ext disable 8.4 intl`). */
export const disablePhpExtension = defineAction<"php.ext_disable", PhpExtensionInput>({
  id: "php.ext_disable",
  method: "php.ext_disable",
  toParams: ({ version, extension }) => ({ version, extension }),
});

/** Xdebug mode of a series; `port` is omitted to keep the stored or default one. */
export interface PhpXdebugInput {
  version: string;
  mode: XdebugMode;
  port?: number | null;
}

/** Sets the Xdebug mode, installing the extension when it is missing (`forge php xdebug 8.4 trigger`). */
export const setPhpXdebug = defineAction<"php.xdebug", PhpXdebugInput>({
  id: "php.xdebug",
  method: "php.xdebug",
  toParams: ({ version, mode, port }) => ({
    version,
    mode,
    ...(port === undefined || port === null ? {} : { port }),
  }),
});

/** Opens the series' php.ini in the editor on the daemon's machine (`forge php ini 8.4 --open`). */
export const openPhpIni = defineAction<"php.ini_open", string>({
  id: "php.ini_open",
  method: "php.ini_open",
  toParams: (version) => ({ version }),
});
