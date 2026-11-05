// A shortcut pressed while the `palette` texts are still loading is not lost (M2-GATE-B18). The
// namespace loads once per file, so this must stay the only `renderApp()` of the file.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n, { type BackendModule, type ReadCallback } from "i18next";
import { afterEach, expect, it, vi } from "vitest";
import { toastStore } from "../../design/primitives/toastStore";
import { renderApp } from "../testApp";

afterEach(() => {
  vi.restoreAllMocks();
});

it("honours shortcuts pressed while the palette texts load", async () => {
  expect(i18n.hasLoadedNamespace("palette")).toBe(false);
  // Hold every read of the `palette` namespace until the test releases it.
  const backend = (i18n.services.backendConnector as { backend: BackendModule }).backend;
  const read = backend.read.bind(backend);
  const held: (() => void)[] = [];
  vi.spyOn(backend, "read").mockImplementation((lng: string, ns: string, cb: ReadCallback) => {
    if (ns === "palette") {
      held.push(() => {
        read(lng, ns, cb);
      });
    } else {
      read(lng, ns, cb);
    }
  });
  const user = userEvent.setup();
  const h = await renderApp();
  expect(held.length).toBeGreaterThan(0);

  // They wait for their texts (no raw i18n key in a toast)...
  await user.keyboard("{Control>}3{/Control}");
  await user.keyboard("{Control>}{Shift>}s{/Shift}{/Control}");
  await user.keyboard("{Control>}k{/Control}");
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(h.router.state.location.pathname).toBe("/dashboard");
  expect(h.fake.calls.some((c) => c.method === "services.start")).toBe(false);
  expect(screen.getByTestId("app-shell")).toBeInTheDocument();

  for (const release of held.splice(0)) release();

  // ...and are not forgotten.
  const dialog = await screen.findByRole("dialog");
  await waitFor(() => {
    expect(within(dialog).getByRole("combobox")).toHaveFocus();
  });
  await waitFor(() => {
    expect(h.fake.calls.some((c) => c.method === "services.start")).toBe(true);
  });
  await waitFor(() => {
    expect(toastStore.getSnapshot().map((t) => t.message)).toContain("Start all…");
  });
  expect(h.router.state.location.pathname).toBe("/sites");
});
