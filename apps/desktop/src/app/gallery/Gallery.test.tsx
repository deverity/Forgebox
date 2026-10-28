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
    expect(screen.getAllByRole("img").map((el) => el.getAttribute("aria-label"))).toEqual(
      expect.arrayContaining(["Running", "Starting", "Stopped", "Failed", "Updating"]),
    );
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
