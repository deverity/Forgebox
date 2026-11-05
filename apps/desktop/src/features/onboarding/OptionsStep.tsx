import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Toggle } from "../../design/primitives/Toggle";
import { DEFAULT_TLD, normalizeTld } from "./model";
import { StepNav } from "./StepNav";
import type { Wizard } from "./useWizard";

function Option({
  id,
  label,
  description,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  description: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  const uid = useId();
  return (
    <li
      data-option={id}
      className="flex items-start gap-4 rounded-card border border-stroke bg-surface p-3"
    >
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="text-headline">{label}</span>
        <p id={uid} className="text-callout text-fg-secondary">
          {description}
        </p>
      </div>
      <Toggle label={label} checked={checked} onCheckedChange={onChange} />
    </li>
  );
}

/**
 * Step 3: what Forgebox may change on this machine. PATH and the Windows startup entry only
 * exist for a local, non-portable daemon; DNS and the CA always do. The sentence of the
 * parity toggle is the one in docs/15 section 8. Nothing runs until step 4.
 */
export function OptionsStep({
  wizard,
  onBack,
  onNext,
}: {
  wizard: Wizard;
  onBack: () => void;
  onNext: () => void;
}) {
  const { t } = useTranslation("onboarding");
  const { choices, update } = wizard;
  const tld = normalizeTld(choices.tld) || DEFAULT_TLD;
  return (
    <div className="flex flex-col gap-4">
      <ul className="flex flex-col gap-2">
        {wizard.pathAvailable ? (
          <Option
            id="path"
            label={t("options.path.label")}
            description={t("options.path.description")}
            checked={choices.path}
            onChange={(path) => {
              update({ path });
            }}
          />
        ) : null}
        <Option
          id="dns"
          label={t("options.dns.label", { tld })}
          description={t("options.dns.description")}
          checked={choices.dns}
          onChange={(dns) => {
            update({ dns });
          }}
        />
        <Option
          id="ca"
          label={t("options.ca.label")}
          description={t("options.ca.description", { tld })}
          checked={choices.ca}
          onChange={(ca) => {
            update({ ca });
          }}
        />
        {wizard.autostart === null ? null : (
          <Option
            id="autostart"
            label={t("options.autostart.label")}
            description={t("options.autostart.description")}
            checked={choices.autostart}
            onChange={(autostart) => {
              update({ autostart });
            }}
          />
        )}
        <Option
          id="parity"
          label={t("options.parity.label")}
          description={t("options.parity.description")}
          checked={choices.parity}
          onChange={(parity) => {
            update({ parity });
          }}
        />
      </ul>
      <StepNav onBack={onBack} onNext={onNext} nextLabel={t("nav.install")} />
    </div>
  );
}
