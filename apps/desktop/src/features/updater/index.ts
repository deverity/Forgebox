export { AutostartToggle, UpdaterPanel } from "./UpdaterPanel";
export { useUpdaterSnapshot } from "./store";
export type { AutostartStatus, UpdaterSnapshot } from "./api";

/** Loads the strings of the panels; call before rendering them (route loader, onboarding). */
export async function loadUpdaterStrings(): Promise<void> {
  const { default: i18n } = await import("i18next");
  await i18n.loadNamespaces("updater");
}
