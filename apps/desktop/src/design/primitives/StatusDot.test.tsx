import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { mockReducedMotion } from "../../test/matchMedia";
import { SERVICE_STATES, StatusDot, animationFor } from "./StatusDot";
import type { DotAnimation, ServiceState } from "./StatusDot";

const TEXT: Record<ServiceState, string> = {
  running: "Running",
  starting: "Starting",
  stopped: "Stopped",
  failed: "Failed",
  updating: "Updating",
};
const ANIM: Record<ServiceState, DotAnimation> = {
  running: "breathe",
  starting: "pulse",
  stopped: "static",
  failed: "shake",
  updating: "pulse",
};

describe("StatusDot", () => {
  it.each(SERVICE_STATES)("exposes the localized text for %s", (state) => {
    render(<StatusDot state={state} />);
    expect(screen.getByRole("img", { name: TEXT[state] })).toHaveAttribute("data-state", state);
  });

  it("renders the visible label on request", () => {
    render(<StatusDot state="running" showLabel />);
    expect(screen.getByText("Running")).toBeInTheDocument();
  });

  it.each(SERVICE_STATES)("animates %s per docs/07 section 3", (state) => {
    render(<StatusDot state={state} />);
    expect(screen.getByRole("img")).toHaveAttribute("data-animation", ANIM[state]);
    expect(animationFor(state, false)).toBe(ANIM[state]);
  });

  it.each(SERVICE_STATES)("collapses %s to the 100 ms fade under reduced motion", (state) => {
    mockReducedMotion(true);
    render(<StatusDot state={state} />);
    expect(screen.getByRole("img", { name: TEXT[state] })).toHaveAttribute(
      "data-animation",
      "fade",
    );
    expect(animationFor(state, true)).toBe("fade");
  });

  it("actually runs the mount animation (not rendered at the final keyframe)", async () => {
    const { unmount } = render(<StatusDot state="running" />);
    const dot = screen.getByRole("img");
    await waitFor(() => { expect(Number(dot.style.opacity || "1")).toBeLessThan(0.99); }, {
      timeout: 1500,
    });
    unmount();
    render(<StatusDot state="failed" />);
    const failed = screen.getByRole("img");
    await waitFor(() => { expect(failed.style.transform).toMatch(/translateX\((?!0px)/); }, {
      timeout: 1000,
    });
  });
});
