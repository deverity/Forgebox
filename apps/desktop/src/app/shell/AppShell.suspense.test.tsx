// The shell paints before any feature text arrives (docs/07 section 4, ADR-0035): only the core
// namespaces are loaded with it. The palette host (shortcuts, mounted by the shell) uses the
// `palette` namespace; a suspension there must stay in its own boundary instead of replacing the
// whole shell with the route pending fallback. This must be the first `renderApp()` of the file,
// before anything loads the `palette` namespace.
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n, { type BackendModule, type ReadCallback } from "i18next";
import { afterEach, expect, it, vi } from "vitest";
import { renderApp } from "../testApp";

afterEach(() => {
  vi.restoreAllMocks();
});

it("stays on screen while the palette texts are still loading", async () => {
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

  const h = await renderApp();
  expect(held.length).toBeGreaterThan(0);
  expect(screen.getByTestId("app-shell")).toBeInTheDocument();
  expect(screen.getByTestId("sidebar-version")).toBeInTheDocument();
  expect(screen.getByTestId("dashboard-tiles")).toBeInTheDocument();
  expect(screen.queryByRole("status", { name: /Loading screen/ })).toBeNull();

  // Once the texts arrive the shortcuts work (Ctrl+2 is the second sidebar screen).
  for (const release of held.splice(0)) release();
  await waitFor(() => {
    expect(i18n.hasLoadedNamespace("palette")).toBe(true);
  });
  const user = userEvent.setup();
  await waitFor(async () => {
    await user.keyboard("{Control>}2{/Control}");
    expect(h.router.state.location.pathname).not.toBe("/dashboard");
  });
  expect(screen.getByTestId("app-shell")).toBeInTheDocument();
});
