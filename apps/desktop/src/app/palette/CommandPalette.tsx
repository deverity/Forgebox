import { Command } from "cmdk";
import { Dialog } from "radix-ui";
import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Kbd } from "../../design/primitives/Kbd";
import { renderCli } from "../../ipc/cli";
import { openUrl } from "../../ipc/native";
import { useCapabilities, useInstances, useSites } from "../../state";
import { useErrorTranslate } from "../shell/useErrorText";
import { chordKeys } from "../shortcuts/registry";
import { useShortcutBindings } from "../shortcuts/useShortcuts";
import type { PaletteActions } from "./actions";
import { buildCommands, type PaletteCommand, type PaletteGroup } from "./registry";
import { fold, score } from "./search";

const GROUP_ORDER: readonly PaletteGroup[] = ["actions", "navigation", "services", "sites"];

type Translate = (key: string, values?: Record<string, string | number>) => string;

export interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  actions: PaletteActions;
}

/**
 * Ctrl+K palette (docs/07 §1.4, §8): cmdk on a Radix Dialog (focus trap, Escape, focus return).
 * Each row shows its shortcut (`Kbd`) and the equivalent `forge` command; running one goes
 * through the same action path as the buttons (job toasts). Search is fuzzy over the pt-BR and
 * en-US labels and ignores accents.
 */
export function CommandPalette({ open, onOpenChange, actions }: CommandPaletteProps) {
  const { t } = useTranslation("palette");
  const translateError = useErrorTranslate();
  const capabilities = useCapabilities();
  const instances = useInstances();
  const sites = useSites();
  const bindings = useShortcutBindings();
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  // The palette has no trigger element, so Radix cannot return the focus by itself.
  const returnTo = useRef<HTMLElement | null>(null);

  const commands = useMemo(() => {
    const unavailable = (method: string): string | null => {
      const capability = capabilities.can(method);
      if (capability.available || capability.text === null) return null;
      return translateError(capability.text.key, capability.text.values);
    };
    return buildCommands({
      t: t as unknown as Translate,
      instances: Object.values(instances),
      sites: Object.values(sites),
      unavailable,
      goTo: actions.goTo,
      openService: actions.openService,
      newSite: actions.newSite,
      run: actions.run,
      startAll: actions.startAll,
      stopAll: actions.stopAll,
      openTerminal: actions.openTerminal,
      openUrl,
      cli: {
        startAll: renderCli("services.start", {}),
        stopAll: renderCli("services.stop", {}),
        instance: (action, target) => renderCli(`services.${action}`, { targets: [target] }),
      },
    });
  }, [t, instances, sites, capabilities, translateError, actions]);

  const chordOf = (command: PaletteCommand): string | null =>
    command.shortcutId === undefined
      ? null
      : (bindings.find((b) => b.id === command.shortcutId)?.chord ?? null);

  const choose = (command: PaletteCommand) => {
    if (command.unavailable !== null) return;
    onOpenChange(false);
    command.run();
  };

  const change = (next: boolean) => {
    if (!next) setQuery("");
    onOpenChange(next);
  };

  return (
    <Dialog.Root open={open} onOpenChange={change}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-fg-primary/20" />
        <Dialog.Content
          onOpenAutoFocus={(event) => {
            // The search field takes the focus, not the dialog container.
            event.preventDefault();
            returnTo.current =
              document.activeElement instanceof HTMLElement ? document.activeElement : null;
            inputRef.current?.focus();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            returnTo.current?.focus();
            returnTo.current = null;
          }}
          className="fixed top-[14%] left-1/2 w-[560px] max-w-[calc(100%-32px)] -translate-x-1/2 overflow-hidden rounded-sheet border border-stroke bg-surface-2 text-fg-primary shadow-popover"
        >
          <Dialog.Title className="sr-only">{t("label")}</Dialog.Title>
          <Dialog.Description className="sr-only">{t("placeholder")}</Dialog.Description>
          <Command
            label={t("label")}
            loop
            filter={(_value, search, keywords) => score(search, (keywords ?? []).join(" "))}
          >
            <Command.Input
              ref={inputRef}
              value={query}
              onValueChange={setQuery}
              placeholder={t("placeholder")}
              className="h-11 w-full border-b border-stroke bg-transparent px-4 text-body outline-hidden focus-visible:outline-solid focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent placeholder:text-fg-tertiary"
            />
            <Command.List className="max-h-[360px] overflow-y-auto p-2">
              <Command.Empty className="px-3 py-6 text-center text-callout text-fg-secondary">
                {t("empty", { query })}
              </Command.Empty>
              {GROUP_ORDER.map((group) => {
                const rows = commands.filter((c) => c.group === group);
                if (rows.length === 0) return null;
                return (
                  <Command.Group
                    key={group}
                    heading={t(`groups.${group}`)}
                    className="[&_[cmdk-group-heading]]:px-3 [&_[cmdk-group-heading]]:py-1 [&_[cmdk-group-heading]]:text-caption [&_[cmdk-group-heading]]:text-fg-tertiary"
                  >
                    {rows.map((command) => {
                      const chord = chordOf(command);
                      return (
                        <Command.Item
                          key={command.id}
                          value={command.id}
                          keywords={[fold(command.label), command.searchText]}
                          disabled={command.unavailable !== null}
                          onSelect={() => {
                            choose(command);
                          }}
                          data-forge-cli={command.cli ?? undefined}
                          className="flex min-h-8 cursor-default items-center gap-3 rounded-control px-3 py-1 text-body data-[disabled=true]:text-fg-tertiary data-[selected=true]:bg-accent data-[selected=true]:font-medium data-[selected=true]:text-fg-on-accent"
                        >
                          <span className="min-w-0 flex-1 truncate">{command.label}</span>
                          {command.unavailable === null ? null : (
                            <span className="text-caption">
                              {t("unavailable", { reason: command.unavailable })}
                            </span>
                          )}
                          {command.cli === null ? null : (
                            <code className="font-mono text-caption">{command.cli}</code>
                          )}
                          {chord === null ? null : (
                            <span className="flex gap-0.5">
                              {chordKeys(chord).map((key) => (
                                <Kbd key={key}>{key}</Kbd>
                              ))}
                            </span>
                          )}
                        </Command.Item>
                      );
                    })}
                  </Command.Group>
                );
              })}
            </Command.List>
          </Command>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
