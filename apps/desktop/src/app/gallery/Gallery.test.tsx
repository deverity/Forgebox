import { render, screen } from "@testing-library/react";
import i18n from "i18next";
import { beforeAll, describe, expect, it } from "vitest";
import { App, GALLERY_PATH } from "../../App";
import Gallery from "./Gallery";

describe("/__gallery", () => {
  beforeAll(async () => {
    await i18n.loadNamespaces("gallery");
  });

  it("shows every control primitive with its states", () => {
    render(<Gallery />);
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

  it("is routed by pathname from the app shell", async () => {
    window.history.pushState({}, "", GALLERY_PATH);
    render(<App />);
    expect(
      await screen.findByRole("heading", { level: 1, name: "Component gallery" }),
    ).toBeInTheDocument();
    window.history.pushState({}, "", "/");
  });
});
