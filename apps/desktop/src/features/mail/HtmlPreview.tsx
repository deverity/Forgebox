import { AlertDialog } from "radix-ui";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { openUrl } from "../../ipc/native";
import { FRAME_SANDBOX, prepareHtml } from "./model";

const LINK_DIALOG_TESTID = "mail-link-dialog";

export interface HtmlPreviewProps {
  html: string;
  /** Remote images of this message are allowed (the per-message toggle). */
  allowRemote: boolean;
  onAllowRemote: (allow: boolean) => void;
  /** Switches to the "Texto" tab (the fallback for a message too large to render). */
  onShowText: () => void;
}

/**
 * The message HTML in a locked-down iframe (docs/11 section 6, model.ts): no scripts, no
 * same-origin, a CSP meta that blocks every request but `data:`/`cid:` images, remote images
 * behind a toggle and links listed outside the frame, opened in the system browser after the
 * user confirms the URL.
 */
export function HtmlPreview({ html, allowRemote, onAllowRemote, onShowText }: HtmlPreviewProps) {
  const { t } = useTranslation("mail");
  const [confirming, setConfirming] = useState<string | null>(null);
  const prepared = useMemo(() => prepareHtml(html, allowRemote), [html, allowRemote]);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      {prepared.remoteImages > 0 ? (
        <div
          data-testid="mail-remote-images"
          className="flex items-center gap-3 rounded-control border border-stroke bg-surface-2 px-3 py-2 text-callout"
        >
          <span className="min-w-0 flex-1 text-fg-secondary">
            {allowRemote
              ? t("html.remoteShown", { count: prepared.remoteImages })
              : t("html.remoteBlocked", { count: prepared.remoteImages })}
          </span>
          <Button
            variant="secondary"
            onClick={() => {
              onAllowRemote(!allowRemote);
            }}
          >
            {allowRemote ? t("html.hideRemote") : t("html.showRemote")}
          </Button>
        </div>
      ) : null}
      {prepared.srcdoc === null ? (
        <div
          data-testid="mail-too-large"
          className="flex flex-col items-start gap-2 rounded-control border border-stroke bg-surface-2 p-4"
        >
          <p className="text-callout text-fg-secondary">{t("html.tooLarge")}</p>
          <Button variant="primary" onClick={onShowText}>
            {t("html.viewText")}
          </Button>
        </div>
      ) : (
        <iframe
          data-testid="mail-html-frame"
          title={t("html.frameTitle")}
          sandbox={FRAME_SANDBOX}
          srcDoc={prepared.srcdoc}
          referrerPolicy="no-referrer"
          className="min-h-64 w-full flex-1 rounded-control border border-stroke"
        />
      )}
      {prepared.links.length > 0 ? (
        <details className="text-callout" data-testid="mail-links">
          <summary className="cursor-pointer text-fg-secondary">
            {t("html.links", { count: prepared.links.length })}
          </summary>
          <ul className="mt-1 flex flex-col gap-1">
            {prepared.links.map((link) => (
              <li key={link} className="min-w-0">
                <button
                  type="button"
                  title={link}
                  onClick={() => {
                    setConfirming(link);
                  }}
                  className="block max-w-full truncate rounded-control text-left font-mono text-mono text-fg-primary underline underline-offset-2 focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
                >
                  {link}
                </button>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
      <AlertDialog.Root
        open={confirming !== null}
        onOpenChange={(open) => {
          if (!open) setConfirming(null);
        }}
      >
        <AlertDialog.Portal>
          <AlertDialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
          <AlertDialog.Content
            data-testid={LINK_DIALOG_TESTID}
            className="fixed top-1/2 left-1/2 w-[460px] max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sheet border border-stroke bg-surface-2 p-4 text-fg-primary shadow-popover"
          >
            <AlertDialog.Title className="text-headline">{t("link.title")}</AlertDialog.Title>
            <AlertDialog.Description className="mt-2 text-callout text-fg-secondary">
              {t("link.description")}
            </AlertDialog.Description>
            <code
              data-testid="mail-link-url"
              className="mt-3 block rounded-control border border-stroke bg-surface px-2 py-1 font-mono text-mono break-all"
            >
              {confirming}
            </code>
            <div className="mt-4 flex justify-end gap-2">
              <AlertDialog.Cancel asChild>
                <Button variant="secondary">{t("link.cancel")}</Button>
              </AlertDialog.Cancel>
              <AlertDialog.Action asChild>
                <Button
                  variant="primary"
                  onClick={() => {
                    if (confirming !== null) void openUrl(confirming);
                  }}
                >
                  {t("link.open")}
                </Button>
              </AlertDialog.Action>
            </div>
          </AlertDialog.Content>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </div>
  );
}
