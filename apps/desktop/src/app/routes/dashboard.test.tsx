// The Dashboard is the default route, so it is what the window shows on first paint. Its texts
// must load with its chunk: a page that suspends on a missing namespace would replace the whole
// shell (sidebar, content toolbar) with the route pending fallback until they arrive.
// This must be the first `renderApp()` of the file, before anything loads the namespaces.
import { screen } from "@testing-library/react";
import i18n from "i18next";
import { expect, it } from "vitest";
import { renderApp } from "../testApp";

it("keeps the shell on screen while the default route loads its texts", async () => {
  expect(i18n.hasLoadedNamespace("dashboard")).toBe(false);
  await renderApp();
  expect(screen.getByTestId("app-shell")).toBeInTheDocument();
  expect(screen.getByTestId("sidebar-version")).toBeInTheDocument();
  expect(screen.getByTestId("dashboard-tiles")).toBeInTheDocument();
  expect(screen.queryByText("Loading screen…")).toBeNull();
});
