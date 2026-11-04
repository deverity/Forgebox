/* eslint-disable i18next/no-literal-string, no-restricted-syntax, jsx-a11y/click-events-have-key-events, jsx-a11y/alt-text, jsx-a11y/interactive-supports-focus -- qa-only fixture: deliberately renders the violations the qa:check rules must catch (never part of a production build), and its text is not user-facing so it is not translated */
import { useEffect, useState } from "react";
import { defineAction } from "../../actions/registry";
import { ActionButton, ErrorNotice, Popover, Sheet, Skeleton } from "../../design/primitives";
import { FOCUS_RING } from "../../design/primitives/shared";
import { parseCase, type SelftestCase } from "./cases";

// Registered once at module load; a local_only action to prove "disabled with an explanation
// on a remote context". Lives here, not in src/actions: it only exists in qa builds.
const killPort = defineAction({
  id: "selftest.kill_port",
  method: "services.kill_port_holder",
  toParams: () => ({ port: 8080, pid: 1234, confirm: true }),
});

const BUTTON = `h-8 rounded-control border border-stroke-strong px-3 text-body ${FOCUS_RING}`;
const SHORTCUT_PANEL = "selftest-panel";

/** Good surfaces: Radix Sheet and Popover (Escape closes, focus returns to the trigger). */
function GoodSurfaces() {
  return (
    <div className="flex gap-3">
      <Sheet
        trigger={
          <button type="button" className={BUTTON}>
            Open sheet
          </button>
        }
        title="Sheet"
      >
        <p>Sheet body</p>
      </Sheet>
      <Popover
        label="Popover"
        trigger={
          <button type="button" className={BUTTON}>
            Open popover
          </button>
        }
      >
        Popover body
      </Popover>
    </div>
  );
}

/** Ctrl+J toggles the panel: the positive control of the shortcut check. */
function GoodShortcut() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey && event.key.toLowerCase() === "j") {
        event.preventDefault();
        setOpen((value) => !value);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, []);
  return open ? <p id={SHORTCUT_PANEL}>Panel</p> : null;
}

/** A surface that ignores Escape and never returns focus. */
function BadEscape() {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button
        type="button"
        className={BUTTON}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => {
          setOpen(true);
        }}
      >
        Open broken sheet
      </button>
      {open ? (
        <div role="dialog" aria-label="Broken sheet">
          Cannot be dismissed with Escape
        </div>
      ) : null}
    </div>
  );
}

/** Data arrives 300 ms late and pushes the content down: no reserved height. */
function LateBlock() {
  const [ready, setReady] = useState(false);
  useEffect(() => {
    const id = setTimeout(() => {
      setReady(true);
    }, 300);
    return () => {
      clearTimeout(id);
    };
  }, []);
  return (
    <div>
      {ready ? <div style={{ height: 240 }}>Late data</div> : null}
      <div style={{ height: 480 }} className="border border-stroke">
        Content below the late block
      </div>
    </div>
  );
}

function Body({ which }: { which: SelftestCase }) {
  switch (which) {
    case "ok":
      return (
        <div className="flex flex-col gap-4">
          <GoodSurfaces />
          <GoodShortcut />
          <ActionButton action={killPort}>Kill port holder</ActionButton>
          <ErrorNotice error="boom" onRetry={() => undefined} />
          <button type="button" className={BUTTON}>
            Plain button
          </button>
          <div
            data-qa-on-accent=""
            className="rounded-control bg-accent px-3 py-1 text-fg-on-accent"
          >
            Text on accent
          </div>
          <div data-qa-scope="" className="rounded-control bg-sidebar px-3 py-1 text-fg-secondary">
            Text on the sidebar fallback
          </div>
        </div>
      );
    case "axe":
      // A button with no accessible name and an image with no text alternative.
      return (
        <div>
          <button type="button" />
          <img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" />
        </div>
      );
    case "sidebar-contrast":
      // The secondary text token redefined to the sidebar's own colour.
      return (
        <div
          data-qa-scope=""
          style={{ ["--fg-secondary" as string]: "var(--bg-sidebar-solid)" }}
          className="bg-sidebar p-3"
        >
          Text on the sidebar fallback
        </div>
      );
    case "accent-contrast":
      // Hard-coded white text on --accent: unreadable on yellow accents.
      return (
        <div
          data-qa-on-accent=""
          style={{ color: "#fff" }}
          className="rounded-control bg-accent px-3 py-1"
        >
          Text on accent
        </div>
      );
    case "unreachable":
      return (
        <span role="button" onClick={() => undefined}>
          Click only
        </span>
      );
    case "no-focus-ring":
      return (
        <button type="button" style={{ outline: "none" }} className={BUTTON}>
          No ring
        </button>
      );
    case "escape":
      return <BadEscape />;
    case "shortcut":
      // Same shortcut table as `ok`, but nothing listens to Ctrl+J.
      return <p>No shortcut here</p>;
    case "layout-shift":
      return <LateBlock />;
    case "motion":
      return (
        <>
          <style>
            {"@keyframes selftest-slide{from{transform:translateX(-80px)}to{transform:none}}"}
          </style>
          <div style={{ animation: "selftest-slide 600ms ease-out both" }}>Slides in</div>
        </>
      );
    case "contract-cli":
      return (
        <span data-action="selftest.no_cli">
          <button type="button" className={BUTTON}>
            Action without a command
          </button>
        </span>
      );
    case "contract-error":
      // An error state with no code, no cause text and no action.
      return <div data-error-notice="" role="alert" />;
    case "contract-local-only":
      return (
        <span data-action="selftest.local" data-local-only="true" data-forge-cli="forge shell open">
          <button type="button" className={BUTTON}>
            Open on the daemon machine
          </button>
        </span>
      );
    case "eternal-skeleton":
      // A list that never leaves its placeholder: no data, no cause, no action.
      return <Skeleton height={48} />;
    case "contract-client-path":
      return <p>Config saved to C:\Users\someone\forgebox\config.yaml</p>;
  }
}

/** `/qa/selftest?case=<id>`: see cases.ts. */
export default function SelfTest() {
  const which = parseCase(window.location.search);
  return (
    <main className="flex h-full flex-col gap-4 overflow-auto bg-canvas p-6 text-fg-primary">
      <h1 className="text-title-1">QA self-test: {which}</h1>
      <Body which={which} />
    </main>
  );
}
