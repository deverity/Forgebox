import "i18next";
import type common from "./en-US/common.json";
import type errors from "./en-US/errors.json";
import type notifications from "./en-US/notifications.json";
import type palette from "./en-US/palette.json";
import type shell from "./en-US/shell.json";
import type tray from "./en-US/tray.json";

/**
 * Typed keys: en-US is the source of truth. Add each new feature namespace here
 * (the parity test and the Rust lint pick the new file up automatically).
 */
export interface Resources {
  common: typeof common;
  shell: typeof shell;
  errors: typeof errors;
  tray: typeof tray;
  notifications: typeof notifications;
  palette: typeof palette;
}

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: Resources;
  }
}
