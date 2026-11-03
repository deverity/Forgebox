import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../../design/primitives/Button";
import { SegmentedControl } from "../../design/primitives/SegmentedControl";
import { Toggle } from "../../design/primitives/Toggle";
import type { AutostartStatus, UpdateChannel, UpdatePhase } from "./api";
import { getUpdaterStore, useUpdaterSnapshot } from "./store";

const STATE_KEY = {
  idle: "state.idle",
  checking: "state.checking",
  up_to_date: "state.upToDate",
  available: "state.available",
  downloading: "state.downloading",
  stopping: "state.stopping",
  installing: "state.installing",
  error: "state.error",
} as const satisfies Record<UpdatePhase, string>;

const CHANNELS = ["stable", "beta"] as const satisfies readonly UpdateChannel[];

const BUSY: readonly UpdatePhase[] = ["checking", "downloading", "stopping", "installing"];

/**
 * "App updates" section of Settings: version, channel, check now, changelog before applying
 * and an explicit confirmation (the apply stops the daemon and restarts the app). In the
 * 0.2.0 release build the updater is inactive and the panel only says so.
 */
export function UpdaterPanel() {
  const { t } = useTranslation("updater");
  const snapshot = useUpdaterSnapshot();
  const [channel, setChannel] = useState<UpdateChannel>("stable");
  const [confirming, setConfirming] = useState(false);
  const store = getUpdaterStore();

  useEffect(() => {
    let cancelled = false;
    store.api.channel().then(
      (value) => {
        if (!cancelled) setChannel(value);
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [store]);

  const busy = BUSY.includes(snapshot.phase);
  const available = snapshot.available;

  const check = () => {
    store.api.check().then(
      () => store.refresh(),
      () => store.refresh(),
    );
  };
  const apply = () => {
    setConfirming(false);
    store.api.apply().catch(() => store.refresh());
  };

  return (
    <section aria-labelledby="updater-title" className="flex flex-col gap-3 px-4">
      <h2 id="updater-title" className="text-title">
        {t("title")}
      </h2>
      {snapshot.current_version === "" ? null : (
        <p className="text-caption text-fg-secondary">
          {t("version", { version: snapshot.current_version })}
        </p>
      )}
      {!snapshot.active ? (
        <p data-testid="updater-inactive" className="text-body text-fg-secondary">
          {t("inactive")}
        </p>
      ) : (
        <>
          <SegmentedControl
            label={t("channel.label")}
            value={channel}
            options={CHANNELS.map((value) => ({ value, label: t(`channel.${value}`) }))}
            disabled={busy}
            onValueChange={(value) => {
              const next: UpdateChannel = value === "beta" ? "beta" : "stable";
              setChannel(next);
              store.api.setChannel(next).catch(() => undefined);
            }}
          />
          <p role="status" data-testid="updater-state" className="text-body">
            {t(STATE_KEY[snapshot.phase], { version: available?.version ?? "" })}
          </p>
          {snapshot.error === null ? null : (
            <p role="alert" data-testid="updater-error" className="text-body text-state-failed">
              {snapshot.error.message}
            </p>
          )}
          {available === null ? null : (
            <div className="flex flex-col gap-1" data-testid="updater-changelog">
              <h3 className="text-body font-semibold">{t("changelog")}</h3>
              <pre className="max-h-48 overflow-y-auto whitespace-pre-wrap text-caption text-fg-secondary">
                {available.notes ?? t("noChangelog")}
              </pre>
            </div>
          )}
          <div className="flex items-center gap-2">
            <Button onClick={check} loading={snapshot.phase === "checking"} disabled={busy}>
              {t("check")}
            </Button>
            {available === null || confirming ? null : (
              <Button
                variant="primary"
                disabled={busy}
                onClick={() => {
                  setConfirming(true);
                }}
              >
                {t("apply")}
              </Button>
            )}
          </div>
          {confirming ? (
            <div role="alertdialog" aria-label={t("apply")} className="flex flex-col gap-2">
              <p className="text-body">{t("confirm")}</p>
              <div className="flex gap-2">
                <Button variant="primary" onClick={apply}>
                  {t("confirmYes")}
                </Button>
                <Button
                  onClick={() => {
                    setConfirming(false);
                  }}
                >
                  {t("confirmNo")}
                </Button>
              </div>
            </div>
          ) : null}
        </>
      )}
    </section>
  );
}

/**
 * "Start with Windows (minimized)" switch for Settings and onboarding. Disabled with the
 * localized reason in portable mode.
 */
export function AutostartToggle() {
  const { t } = useTranslation("updater");
  const store = getUpdaterStore();
  const [status, setStatus] = useState<AutostartStatus | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    store.api.autostart().then(
      (value) => {
        if (!cancelled) setStatus(value);
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [store]);

  if (status === null) return null;
  return (
    <div className="flex flex-col gap-1 px-4">
      <div className="flex items-center gap-3">
        <Toggle
          label={t("autostart.label")}
          checked={status.enabled}
          disabled={!status.available}
          onCheckedChange={(enabled) => {
            setFailure(null);
            store.api.setAutostart(enabled).then(setStatus, (error: unknown) => {
              const message =
                typeof error === "object" && error !== null && "message" in error
                  ? String(error.message)
                  : String(error);
              setFailure(message);
            });
          }}
        />
        <span className="text-body">{t("autostart.label")}</span>
      </div>
      {status.reason === null ? null : (
        <p data-testid="autostart-reason" className="text-caption text-fg-secondary">
          {status.reason}
        </p>
      )}
      {failure === null ? null : (
        <p role="alert" className="text-caption text-state-failed">
          {failure}
        </p>
      )}
    </div>
  );
}
