import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { renderCli } from "../../ipc/cli";
import { copyText } from "../../ipc/native";
import { useForgeQuery } from "../../state";
import { toast } from "../../design/primitives/toastStore";
import { envText } from "./model";

/** SMTP settings of the mailbox (`mail.env`) with "Copiar .env" and the equivalent command. */
export function CredentialsPanel() {
  const { t } = useTranslation("mail");
  const env = useForgeQuery("mail.env", {});
  const [copying, setCopying] = useState(false);
  const vars = env.data?.vars ?? {};
  const text = envText(vars);

  const copy = () => {
    setCopying(true);
    void copyText(text).then((ok) => {
      setCopying(false);
      toast.show({
        kind: ok ? "success" : "error",
        message: ok ? t("credentials.copied") : t("credentials.copyFailed"),
      });
    });
  };

  return (
    <section
      data-testid="mail-credentials"
      aria-label={t("credentials.title")}
      className="flex flex-col gap-2 rounded-control border border-stroke bg-surface p-3"
    >
      <div className="flex items-center gap-2">
        <h2 className="min-w-0 flex-1 text-headline">{t("credentials.title")}</h2>
        <Button variant="secondary" disabled={text === ""} loading={copying} onClick={copy}>
          {t("credentials.copy")}
        </Button>
      </div>
      {env.isPending ? (
        <p className="text-callout text-fg-secondary">{t("credentials.loading")}</p>
      ) : env.isError ? (
        <p role="alert" className="text-callout text-state-failed">
          {t("credentials.failed")}
        </p>
      ) : (
        <pre
          data-testid="mail-env"
          className="overflow-x-auto rounded-control bg-surface-2 p-2 font-mono text-mono text-fg-primary"
        >
          {text}
        </pre>
      )}
      <CliCommand command={renderCli("mail.env", {})} />
    </section>
  );
}
