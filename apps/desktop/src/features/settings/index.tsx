import { PlaceholderPage } from "../../app/shell/PlaceholderPage";
import { LanguageSwitcher } from "../../i18n/LanguageSwitcher";

/** Placeholder until M2-E3-T16 (which adds the registered sections); keeps the language picker. */
export function SettingsPage() {
  return (
    <PlaceholderPage id="settings">
      <div className="px-4">
        <LanguageSwitcher />
      </div>
    </PlaceholderPage>
  );
}
