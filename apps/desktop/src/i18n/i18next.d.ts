import "i18next";
import type catalog from "./en-US/catalog.json";
import type common from "./en-US/common.json";
import type dashboard from "./en-US/dashboard.json";
import type databases from "./en-US/databases.json";
import type diagnostics from "./en-US/diagnostics.json";
import type errors from "./en-US/errors.json";
import type gallery from "./en-US/gallery.json";
import type logs from "./en-US/logs.json";
import type mail from "./en-US/mail.json";
import type notifications from "./en-US/notifications.json";
import type palette from "./en-US/palette.json";
import type php from "./en-US/php.json";
import type settings from "./en-US/settings.json";
import type services from "./en-US/services.json";
import type shell from "./en-US/shell.json";
import type sites from "./en-US/sites.json";
import type terminal from "./en-US/terminal.json";
import type tray from "./en-US/tray.json";
import type updater from "./en-US/updater.json";

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
  gallery: typeof gallery;
  updater: typeof updater;
  services: typeof services;
  settings: typeof settings;
  sites: typeof sites;
  catalog: typeof catalog;
  dashboard: typeof dashboard;
  terminal: typeof terminal;
  mail: typeof mail;
  php: typeof php;
  databases: typeof databases;
  diagnostics: typeof diagnostics;
  logs: typeof logs;
}

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: Resources;
  }
}
