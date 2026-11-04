import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { screen, waitFor } from "@testing-library/react";
import i18n from "i18next";
import userEvent from "@testing-library/user-event";
import { beforeAll, describe, expect, it } from "vitest";
import { renderApp } from "../testApp";
import { GALLERY_PATH } from "../routes/root";

describe("/__gallery", () => {
  beforeAll(async () => {
    await i18n.loadNamespaces("gallery");
  });

  /** The primitives named by docs/07 section 6, normalised to their component names. */
  function section6Primitives(): string[] {
    const doc = readFileSync(resolve(__dirname, "../../../../../docs/07-ui-ux.md"), "utf8");
    const body = /^## 6\. Componentes[^\n]*\n([^\n]+)/m.exec(doc)?.[1] ?? "";
    // "Toast (empilhado, ação de desfazer), Progress (linear/circular)": drop the parentheses first.
    const names = body
      .replace(/\([^)]*\)/g, "")
      .split(",")
      .map((name) => name.replace(/[\s.]+/g, ""))
      .filter(Boolean);
    return names.map((name) => name.replace(/^Segmentedcontrol$/i, "SegmentedControl"));
  }

  it("covers 100% of the docs/07 section 6 list (M2-E4-T6)", async () => {
    const expected = section6Primitives();
    expect(expected.length).toBeGreaterThanOrEqual(25);
    await renderApp({ path: GALLERY_PATH });
    await screen.findByRole("heading", { level: 1, name: "Component gallery" });
    const shown = new Set(
      [...document.body.querySelectorAll("[data-gallery]")].flatMap((el) =>
        (el.getAttribute("data-gallery") ?? "").split(/\s+/),
      ),
    );
    expect(expected.filter((name) => !shown.has(name))).toEqual([]);
    expect([...shown].filter((name) => !expected.includes(name))).toEqual([]);
  });

  it("shows every control primitive with its states", async () => {
    await renderApp({ path: GALLERY_PATH });
    await screen.findByRole("heading", { level: 1, name: "Component gallery" });
    for (const name of [
      "Button",
      "IconButton",
      "Toggle",
      "Select",
      "Combobox",
      "SegmentedControl",
      "Slider",
      "TextField",
      "Badge",
      "StatusDot",
      "Kbd",
    ]) {
      expect(screen.getByRole("heading", { level: 2, name })).toBeInTheDocument();
    }
    // The surfaces/feedback section (M2-E1-T12) is mounted by the gallery.
    expect(
      screen.getByRole("heading", { level: 2, name: "Surfaces and feedback" }),
    ).toBeInTheDocument();
    // The gallery shows the StatusDots with their label, so the text names each state.
    for (const state of ["Running", "Starting", "Stopped", "Failed", "Updating"]) {
      expect(screen.getAllByText(state).length).toBeGreaterThan(0);
    }
    // Loading buttons show text (not only a spinner).
    expect(screen.getAllByText("Saving").length).toBeGreaterThanOrEqual(4);
    expect(
      screen.getAllByRole("button", { name: /Primary|Disabled|Saving/ }).length,
    ).toBeGreaterThanOrEqual(3);
  });

  it("renders the real CommandPalette from the gallery button", async () => {
    await renderApp({ path: GALLERY_PATH });
    await screen.findByRole("heading", { level: 1, name: "Component gallery" });
    await userEvent.click(screen.getByRole("button", { name: "Open the palette" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => {
      expect(dialog.querySelector("[cmdk-input]")).not.toBeNull();
    });
  });

  it("is a route of the app router", async () => {
    await renderApp({ path: GALLERY_PATH });
    expect(
      await screen.findByRole("heading", { level: 1, name: "Component gallery" }),
    ).toBeInTheDocument();
  });
});
