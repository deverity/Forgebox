import { Tabs } from "radix-ui";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { runAction } from "../../actions/registry";
import { addProxy, addSite } from "../../actions/sites";
import { useCapabilityReason, useErrorMessage } from "../../app/shell/useErrorText";
import { Button } from "../../design/primitives/Button";
import { CliCommand } from "../../design/primitives/CliCommand";
import { ErrorNotice } from "../../design/primitives/ErrorNotice";
import { PathField } from "../../design/primitives/PathField";
import { Select } from "../../design/primitives/Select";
import { Sheet } from "../../design/primitives/Sheet";
import { TextField } from "../../design/primitives/TextField";
import { toast } from "../../design/primitives/toastStore";
import { Toggle } from "../../design/primitives/Toggle";
import { newIntent } from "../../ipc/api";
import { toForgeApiError, type ForgeApiError } from "../../ipc/errors";
import type * as T from "../../ipc/generated";
import { onPathsDropped } from "../../ipc/native";
import { useCapability, useHello, useRuntime, useSystem, useTools } from "../../state";
import {
  fieldOfInvalidInput,
  hostProblem,
  normalizeHost,
  phpSeries,
  suggestHost,
  upstreamProblem,
  type SiteField,
} from "./model";

/**
 * Framework driver ids of docs/06 section 1.2 (`null` asks the daemon to detect). A choice list
 * for the form, not behavior.
 */
export const DRIVER_IDS = [
  "laravel",
  "symfony",
  "wordpress",
  "cakephp",
  "drupal",
  "magento",
  "generic-php",
  "spa",
  "static",
] as const;

const DEFAULT = "__default";
const AUTO = "__auto";
const DEFAULT_TLD = "test";

export interface CreateSiteSheetProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type Tab = "folder" | "proxy" | "project";

// Values of the tab triggers (kept out of the JSX: they are ids, not text).
const TAB_FOLDER: Tab = "folder";
const TAB_PROXY: Tab = "proxy";
const TAB_PROJECT: Tab = "project";
const ADD_METHOD = "sites.add";
const PROXY_METHOD = "sites.proxy_add";

/** Creates a site from a folder (`sites.add`) or a proxy (`sites.proxy_add`). */
export function CreateSiteSheet({ open, onOpenChange }: CreateSiteSheetProps) {
  const { t } = useTranslation("sites");
  return (
    <Sheet
      open={open}
      onOpenChange={onOpenChange}
      title={t("create.title")}
      description={t("create.description")}
    >
      <CreateForm
        onDone={() => {
          onOpenChange(false);
        }}
      />
    </Sheet>
  );
}

function CreateForm({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation("sites");
  const [tab, setTab] = useState<Tab>("folder");
  return (
    <Tabs.Root
      value={tab}
      onValueChange={(next) => {
        setTab(next as Tab);
      }}
      className="flex flex-col gap-4"
    >
      <Tabs.List
        aria-label={t("create.tabs")}
        className="flex gap-1 rounded-control border border-stroke-strong bg-surface-2 p-0.5"
      >
        <Tabs.Trigger
          value={TAB_FOLDER}
          className="h-7 flex-1 rounded-control px-3 text-body text-fg-secondary focus-visible:outline-2 focus-visible:outline-accent data-[state=active]:bg-surface data-[state=active]:text-fg-primary"
        >
          {t("create.tabFolder")}
        </Tabs.Trigger>
        <Tabs.Trigger
          value={TAB_PROXY}
          className="h-7 flex-1 rounded-control px-3 text-body text-fg-secondary focus-visible:outline-2 focus-visible:outline-accent data-[state=active]:bg-surface data-[state=active]:text-fg-primary"
        >
          {t("create.tabProxy")}
        </Tabs.Trigger>
        {/* Stacks arrive with M3-E2: disabled, the reason is printed right below. */}
        <Tabs.Trigger
          value={TAB_PROJECT}
          disabled
          title={t("create.projectDisabled")}
          className="h-7 flex-1 rounded-control px-3 text-body text-fg-tertiary disabled:cursor-not-allowed"
        >
          {t("create.tabProject")}
        </Tabs.Trigger>
      </Tabs.List>
      <p className="text-caption text-fg-tertiary">{t("create.projectDisabled")}</p>
      <Tabs.Content value={TAB_FOLDER}>
        <FolderForm onDone={onDone} />
      </Tabs.Content>
      <Tabs.Content value={TAB_PROXY}>
        <ProxyForm onDone={onDone} />
      </Tabs.Content>
    </Tabs.Root>
  );
}

/** Runs a site-creating action; a successful job puts the new site in the store at once. */
function useCreate(onDone: () => void) {
  const runtime = useRuntime();
  const { t } = useTranslation("sites");
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<ForgeApiError | null>(null);

  const run = (
    action: typeof addSite | typeof addProxy,
    input: T.SiteAddParams | T.ProxyAddParams,
    onFail: (error: ForgeApiError) => void,
  ) => {
    setRunning(true);
    setFailure(null);
    runAction(runtime.queue, action as typeof addSite, input as T.SiteAddParams, {
      intent: newIntent(),
    }).then(
      (result) => {
        const site = result as T.SiteDto | null;
        if (site !== null && typeof site === "object" && "id" in site) {
          runtime.stores.sites.setState((state) => ({
            sites: { ...state.sites, [site.id]: site },
          }));
          toast.show({ kind: "success", message: t("create.created", { host: site.host }) });
        }
        setRunning(false);
        onDone();
      },
      (error: unknown) => {
        const failed = toForgeApiError(error, { method: action.method });
        setFailure(failed);
        onFail(failed);
        setRunning(false);
      },
    );
  };
  return { running, failure, run };
}

function Submit({
  method,
  running,
}: {
  method: "sites.add" | "sites.proxy_add";
  running: boolean;
}) {
  const { t } = useTranslation("sites");
  const capability = useCapability(method);
  const reason = useCapabilityReason(capability);
  return (
    <div className="flex flex-col gap-1">
      <Button
        type="submit"
        variant="primary"
        loading={running}
        loadingLabel={t("create.submitting")}
        disabled={reason !== null}
        data-disabled-reason={reason ?? undefined}
      >
        {t("create.submit")}
      </Button>
      {reason !== null ? <p className="text-caption text-fg-tertiary">{reason}</p> : null}
    </div>
  );
}

function CommandPreview({ command }: { command: string | null }) {
  const { t } = useTranslation("sites");
  if (command === null) return null;
  return (
    <div className="flex flex-col gap-1">
      <span className="text-caption text-fg-tertiary">{t("create.cli")}</span>
      <CliCommand command={command} />
    </div>
  );
}

function LabeledSelect({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-callout text-fg-secondary">{label}</span>
      {children}
    </div>
  );
}

function without(
  errors: Partial<Record<SiteField, string>>,
  field: SiteField,
): Partial<Record<SiteField, string>> {
  return Object.fromEntries(Object.entries(errors).filter(([key]) => key !== field));
}

function useTld(): string {
  const tld = useSystem((state) => state.dns?.tlds[0]);
  return tld?.replace(/^\./, "") ?? DEFAULT_TLD;
}

function FolderForm({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation("sites");
  const hello = useHello();
  const remote = hello !== null && !hello.local;
  const tld = useTld();
  const tools = useTools((state) => state.tools);
  const errorMessage = useErrorMessage();

  const [dir, setDir] = useState("");
  const [host, setHost] = useState("");
  const [hostEdited, setHostEdited] = useState(false);
  const [lowercased, setLowercased] = useState(false);
  const [php, setPhp] = useState(DEFAULT);
  const [server, setServer] = useState(DEFAULT);
  const [driver, setDriver] = useState(AUTO);
  const [secure, setSecure] = useState(true);
  const [dropping, setDropping] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<Partial<Record<SiteField, string>>>({});
  const { running, failure, run } = useCreate(onDone);

  const suggestion = useMemo(() => suggestHost(dir, tld), [dir, tld]);
  const shownHost = hostEdited ? host : suggestion;

  const servers = useMemo(
    () => Object.values(tools).filter((tool) => tool.category === "webserver"),
    [tools],
  );
  const series = useMemo(() => phpSeries(tools.php), [tools]);

  // Dragging a folder onto the window (Tauri gives real paths); never with a remote daemon.
  const setDirRef = useRef(setDir);
  useEffect(() => {
    if (remote) return undefined;
    let stop: (() => void) | null = null;
    let alive = true;
    void onPathsDropped((paths) => {
      const first = paths[0];
      if (first !== undefined) {
        setDirRef.current(first);
        setFieldErrors({});
      }
    }, setDropping).then((unsubscribe) => {
      if (alive) stop = unsubscribe;
      else unsubscribe();
    });
    return () => {
      alive = false;
      stop?.();
    };
  }, [remote]);

  const hostIssue = hostProblem(shownHost);
  const dirIssue = dir.trim() === "" ? "empty" : null;
  const hostText =
    fieldErrors.host ??
    (submitted && hostIssue !== null
      ? t(hostIssue === "empty" ? "create.hostEmpty" : "create.hostSyntax", { tld })
      : undefined);
  const dirText =
    fieldErrors.dir ?? (submitted && dirIssue !== null ? t("create.folderEmpty") : undefined);

  const params: T.SiteAddParams = {
    dir: dir.trim(),
    host: shownHost === "" ? null : shownHost,
    php: php === DEFAULT ? null : php,
    server: server === DEFAULT ? null : server,
    secure,
    driver: driver === AUTO ? null : driver,
  };

  const submit = () => {
    setSubmitted(true);
    setFieldErrors({});
    if (dirIssue !== null || hostIssue !== null) return;
    run(addSite, params, (error) => {
      const field = fieldOfInvalidInput(error, "host");
      if (field !== null) setFieldErrors({ [field]: errorMessage(error) });
    });
  };

  const dropReason = remote ? t("create.dropZoneRemote") : null;
  const phpOptions = [
    { value: DEFAULT, label: t("create.phpDefault") },
    ...series.map((value) => ({ value, label: value })),
  ];
  const serverOptions = [
    { value: DEFAULT, label: t("create.serverDefault") },
    ...servers.map((tool) => ({ value: tool.id, label: tool.name })),
  ];
  const driverOptions = [
    { value: AUTO, label: t("create.driverAuto") },
    ...DRIVER_IDS.map((value) => ({ value, label: value })),
  ];
  const dropState = dropReason !== null ? "disabled" : dropping ? "active" : "idle";

  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div
        data-testid="drop-zone"
        data-drop-state={dropState}
        aria-disabled={dropReason !== null ? true : undefined}
        data-disabled-reason={dropReason ?? undefined}
        className={`rounded-card border border-dashed p-4 text-center text-callout ${
          dropping ? "border-accent text-fg-primary" : "border-stroke-strong text-fg-secondary"
        } ${dropReason !== null ? "opacity-60" : ""}`}
      >
        {dropReason ?? (dropping ? t("create.dropZoneActive") : t("create.dropZone"))}
      </div>
      <PathField
        label={t("create.folder")}
        value={dir}
        width="full"
        showDisabledBrowse
        {...(dirText === undefined ? {} : { error: dirText })}
        onChange={(value) => {
          setDir(value);
          setFieldErrors((current) => without(current, "dir"));
        }}
      />
      <TextField
        label={t("create.host")}
        width="full"
        value={shownHost}
        autoComplete="off"
        spellCheck={false}
        {...(hostText === undefined ? {} : { error: hostText })}
        hint={
          lowercased
            ? t("create.hostLowercased", { host: shownHost })
            : t("create.hostHint", { tld })
        }
        onChange={(event) => {
          const normalized = normalizeHost(event.target.value);
          setHost(normalized.host);
          setHostEdited(true);
          setLowercased(normalized.lowercased);
          setFieldErrors((current) => without(current, "host"));
        }}
      />
      <div className="grid grid-cols-2 gap-3">
        <LabeledSelect label={t("create.php")}>
          <Select
            label={t("create.php")}
            options={phpOptions}
            value={php}
            onValueChange={setPhp}
            width="full"
          />
        </LabeledSelect>
        <LabeledSelect label={t("create.server")}>
          <Select
            label={t("create.server")}
            options={serverOptions}
            value={server}
            onValueChange={setServer}
            width="full"
          />
        </LabeledSelect>
        <LabeledSelect label={t("create.driver")}>
          <Select
            label={t("create.driver")}
            options={driverOptions}
            value={driver}
            onValueChange={setDriver}
            width="full"
          />
        </LabeledSelect>
        <div className="flex items-end gap-2 pb-1">
          <Toggle label={t("create.https")} checked={secure} onCheckedChange={setSecure} />
          <span className="text-body">{t("create.https")}</span>
        </div>
      </div>
      {failure !== null && Object.keys(fieldErrors).length === 0 ? (
        <ErrorNotice error={failure} context={{ method: ADD_METHOD, params }} />
      ) : null}
      <Submit method="sites.add" running={running} />
      <CommandPreview command={addSite.cli(params)} />
    </form>
  );
}

function ProxyForm({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation("sites");
  const tld = useTld();
  const errorMessage = useErrorMessage();
  const [host, setHost] = useState("");
  const [lowercased, setLowercased] = useState(false);
  const [upstream, setUpstream] = useState("");
  const [websocket, setWebsocket] = useState(false);
  const [secure, setSecure] = useState(true);
  const [submitted, setSubmitted] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<Partial<Record<SiteField, string>>>({});
  const { running, failure, run } = useCreate(onDone);

  const hostIssue = hostProblem(host);
  const upstreamIssue = upstreamProblem(upstream);
  const hostText =
    fieldErrors.host ??
    (submitted && hostIssue !== null
      ? t(hostIssue === "empty" ? "create.hostEmpty" : "create.hostSyntax", { tld })
      : undefined);
  const upstreamText =
    fieldErrors.upstream ??
    (submitted && upstreamIssue !== null
      ? t(upstreamIssue === "empty" ? "create.upstreamEmpty" : "create.upstreamSyntax")
      : undefined);

  const params: T.ProxyAddParams = { host, upstream: upstream.trim(), websocket, secure };

  const submit = () => {
    setSubmitted(true);
    setFieldErrors({});
    if (hostIssue !== null || upstreamIssue !== null) return;
    run(addProxy, params, (error) => {
      const field = fieldOfInvalidInput(error, "host");
      if (field !== null) setFieldErrors({ [field]: errorMessage(error) });
    });
  };

  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <TextField
        label={t("create.host")}
        width="full"
        value={host}
        autoComplete="off"
        spellCheck={false}
        {...(hostText === undefined ? {} : { error: hostText })}
        hint={lowercased ? t("create.hostLowercased", { host }) : t("create.hostHint", { tld })}
        onChange={(event) => {
          const normalized = normalizeHost(event.target.value);
          setHost(normalized.host);
          setLowercased(normalized.lowercased);
          setFieldErrors((current) => without(current, "host"));
        }}
      />
      <TextField
        label={t("create.upstream")}
        width="full"
        value={upstream}
        autoComplete="off"
        spellCheck={false}
        {...(upstreamText === undefined ? {} : { error: upstreamText })}
        hint={t("create.upstreamHint")}
        onChange={(event) => {
          setUpstream(event.target.value);
          setFieldErrors((current) => without(current, "upstream"));
        }}
      />
      <div className="flex items-center gap-2">
        <Toggle label={t("create.websocket")} checked={websocket} onCheckedChange={setWebsocket} />
        <span className="text-body">{t("create.websocket")}</span>
      </div>
      <div className="flex items-center gap-2">
        <Toggle label={t("create.https")} checked={secure} onCheckedChange={setSecure} />
        <span className="text-body">{t("create.https")}</span>
      </div>
      {failure !== null && Object.keys(fieldErrors).length === 0 ? (
        <ErrorNotice error={failure} context={{ method: PROXY_METHOD, params }} />
      ) : null}
      <Submit method="sites.proxy_add" running={running} />
      <CommandPreview command={addProxy.cli(params)} />
    </form>
  );
}
