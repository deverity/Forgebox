// Actions of Settings > Rede and Seguranca (docs/07 section 5.11, M2-E3-T17). `net.dns_enable`,
// `tls.ca_install` and `tls.ca_rotate` are shared with Diagnostics and the config follow-ups
// (`diagnostics.ts`, `system.ts`).
import type { CaUninstallParams, TlsRenewParams } from "../ipc/generated";
import { defineAction } from "./registry";

/** Turns the local DNS off (removes the NRPT rule or the `hosts` block): `forge dns disable`. */
export const disableDns = defineAction({
  id: "security.dns_disable",
  method: "net.dns_disable",
  toParams: () => ({}),
});

/** Takes the CA out of one trust store: `forge tls ca uninstall --scope user|machine`. */
export const uninstallCa = defineAction<"tls.ca_uninstall", CaUninstallParams>({
  id: "security.ca_uninstall",
  method: "tls.ca_uninstall",
  toParams: (params) => params,
});

/** Reissues one certificate (`forge tls renew <host>`) or every one (`--all`). */
export const renewCertificate = defineAction<"tls.renew", TlsRenewParams>({
  id: "security.cert_renew",
  method: "tls.renew",
  toParams: (params) => params,
});

/** Reads the CA certificate (PEM) the GUI then saves through the native dialog:
 * `forge tls ca export`. */
export const exportCa = defineAction({
  id: "security.ca_export",
  method: "tls.ca_export",
  toParams: () => ({}),
});
