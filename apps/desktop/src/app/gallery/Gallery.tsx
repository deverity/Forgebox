import { useState } from "react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Badge, BADGE_TONES } from "../../design/primitives/Badge";
import { BUTTON_VARIANTS, Button } from "../../design/primitives/Button";
import { Combobox } from "../../design/primitives/Combobox";
import { IconButton } from "../../design/primitives/IconButton";
import { Kbd } from "../../design/primitives/Kbd";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { Select } from "../../design/primitives/Select";
import { Slider } from "../../design/primitives/Slider";
import { SERVICE_STATES, StatusDot } from "../../design/primitives/StatusDot";
import { TextField } from "../../design/primitives/TextField";
import { Toggle } from "../../design/primitives/Toggle";

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-3 rounded-card border border-stroke bg-surface p-4">
      <h2 className="text-headline">{title}</h2>
      <div className="flex flex-wrap items-start gap-4">{children}</div>
    </section>
  );
}

function RefreshIcon() {
  return (
    <svg
      viewBox="0 0 16 16"
      className="size-4"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M13 8a5 5 0 1 1-1.5-3.5M13 2.5V5h-2.5" />
    </svg>
  );
}

const PHP_VERSIONS = ["8.4", "8.3", "8.2", "7.4"].map((v) => ({ value: v, label: `PHP ${v}` }));
const SITES = ["app.test", "blog.test", "shop.test", "api.test"].map((v) => ({
  value: v,
  label: v,
}));
const [LIGHT, DARK, SYSTEM] = ["light", "dark", "system"] as const;
const SAMPLE_SITE = "app.test";
const SAMPLE_HOST = "app";
const SAMPLE_PORT = "80";
const KEY_CTRL = "Ctrl";
const KEY_K = "K";
const HOST_RE = /^[a-z0-9-]+$/;

/** `/__gallery`: every control primitive with all variants and states (docs/design/components.md). */
export default function Gallery() {
  const { t } = useTranslation("gallery");
  const [toggle, setToggle] = useState(true);
  const [memory, setMemory] = useState(256);
  const [theme, setTheme] = useState<string>(SYSTEM);
  return (
    <main className="flex h-full flex-col gap-4 overflow-auto bg-canvas p-6 text-fg-primary">
      <header>
        <h1 className="text-title-1">{t("title")}</h1>
        <p className="text-callout text-fg-secondary">{t("intro")}</p>
      </header>

      <Section title={t("button.title")}>
        {BUTTON_VARIANTS.map((variant) => (
          <div
            key={variant}
            className="flex flex-col items-start gap-2"
            data-variant-group={variant}
          >
            <Button variant={variant}>{t(`button.${variant}`)}</Button>
            <Button variant={variant} disabled>
              {t("button.disabled")}
            </Button>
            <Button variant={variant} loading>
              {t("button.loadingLabel")}
            </Button>
          </div>
        ))}
      </Section>

      <Section title={t("iconButton.title")}>
        {BUTTON_VARIANTS.map((variant) => (
          <IconButton
            key={variant}
            variant={variant}
            label={t("iconButton.label")}
            icon={<RefreshIcon />}
          />
        ))}
        <IconButton label={t("iconButton.disabled")} icon={<RefreshIcon />} disabled />
        <IconButton label={t("iconButton.loading")} icon={<RefreshIcon />} loading />
      </Section>

      <Section title={t("toggle.title")}>
        <Toggle label={t("toggle.autoStart")} checked={toggle} onCheckedChange={setToggle} />
        <Toggle label={t("toggle.off")} defaultChecked={false} />
        <Toggle label={t("toggle.disabled")} disabled />
        <Toggle label={t("toggle.disabled")} disabled defaultChecked />
      </Section>

      <Section title={t("select.title")}>
        <Select
          label={t("select.label")}
          options={PHP_VERSIONS}
          placeholder={t("select.placeholder")}
        />
        <Select label={t("select.label")} options={PHP_VERSIONS} defaultValue="8.3" />
        <Select label={t("select.disabled")} options={PHP_VERSIONS} defaultValue="8.3" disabled />
      </Section>

      <Section title={t("combobox.title")}>
        <Combobox
          label={t("combobox.label")}
          options={SITES}
          placeholder={t("combobox.placeholder")}
          emptyText={t("combobox.empty")}
        />
        <Combobox
          label={t("combobox.disabled")}
          options={SITES}
          defaultValue={SAMPLE_SITE}
          emptyText={t("combobox.empty")}
          disabled
        />
      </Section>

      <Section title={t("segmented.title")}>
        <SegmentedControl
          label={t("segmented.label")}
          value={theme}
          onValueChange={setTheme}
          options={[
            { value: LIGHT, label: t("segmented.light") },
            { value: DARK, label: t("segmented.dark") },
            { value: SYSTEM, label: t("segmented.system") },
          ]}
        />
        <SegmentedControl
          label={t("segmented.disabled")}
          disabled
          defaultValue={DARK}
          options={[
            { value: LIGHT, label: t("segmented.light") },
            { value: DARK, label: t("segmented.dark") },
          ]}
        />
      </Section>

      <Section title={t("slider.title")}>
        <Slider
          label={t("slider.label")}
          min={64}
          max={1024}
          step={64}
          value={memory}
          onValueChange={setMemory}
          valueText={`${String(memory)} MB`}
        />
        <span className="text-callout tabular-nums text-fg-secondary">{memory} MB</span>
        <Slider label={t("slider.disabled")} defaultValue={128} disabled />
      </Section>

      <Section title={t("textField.title")}>
        <TextField
          label={t("textField.label")}
          hint={t("textField.hint")}
          defaultValue={SAMPLE_HOST.toUpperCase()}
          validate={(v) => (HOST_RE.test(v) ? undefined : t("textField.invalid"))}
        />
        <TextField
          label={t("textField.errorLabel")}
          defaultValue={SAMPLE_PORT}
          error={t("textField.errorMessage")}
        />
        <TextField label={t("textField.disabled")} defaultValue={SAMPLE_HOST} disabled />
      </Section>

      <Section title={t("badge.title")}>
        {BADGE_TONES.map((tone) => (
          <Badge key={tone} tone={tone}>
            {t(`badge.${tone}`)}
          </Badge>
        ))}
      </Section>

      <Section title={t("statusDot.title")}>
        {SERVICE_STATES.map((state) => (
          <StatusDot key={state} state={state} showLabel />
        ))}
      </Section>

      <Section title={t("kbd.title")}>
        <span className="inline-flex items-center gap-1 text-callout text-fg-secondary">
          <Kbd>{KEY_CTRL}</Kbd>+<Kbd>{KEY_K}</Kbd> {t("kbd.palette")}
        </span>
      </Section>
    </main>
  );
}
