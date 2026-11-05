import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { BADGE_TONES, Badge } from "./Badge";
import { Kbd } from "./Kbd";

describe("Badge", () => {
  it.each(BADGE_TONES)("renders the %s tone with its text", (tone) => {
    render(<Badge tone={tone}>Label</Badge>);
    expect(screen.getByText("Label")).toHaveAttribute("data-tone", tone);
  });
});

describe("Kbd", () => {
  it("renders a kbd element", () => {
    render(<Kbd>Ctrl</Kbd>);
    expect(screen.getByText("Ctrl").tagName).toBe("KBD");
  });
});
