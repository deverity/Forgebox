import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18next from "i18next";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Progress } from "./Progress";
import { Skeleton, SkeletonSlot } from "./Skeleton";
import { mockReducedMotion, restoreMotion } from "./testUtils";
import { ToastViewport } from "./Toast";
import { toast } from "./toastStore";
import { Tooltip } from "./Tooltip";
import { surfaceMotion } from "./useSurfaceMotion";

restoreMotion();
afterEach(() => {
  act(() => {
    toast.clear();
  });
});

describe("Toast", () => {
  it("announces info politely and errors assertively (live regions exist before content)", () => {
    render(<ToastViewport />);
    const status = screen.getByRole("status");
    const alert = screen.getByRole("alert");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(alert).toHaveAttribute("aria-live", "assertive");
    act(() => {
      toast.show({ message: "Saved" });
      toast.show({ kind: "error", message: "Port 80 busy" });
    });
    expect(within(status).getByText("Saved")).toBeInTheDocument();
    expect(within(alert).getByText("Port 80 busy")).toBeInTheDocument();
  });

  it("undo runs the callback and removes the toast", async () => {
    const user = userEvent.setup();
    const onUndo = vi.fn();
    render(<ToastViewport />);
    act(() => {
      toast.show({ message: "Site removed", undo: { onUndo } });
    });
    await user.click(screen.getByRole("button", { name: "Undo" }));
    expect(onUndo).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(screen.queryByText("Site removed")).not.toBeInTheDocument();
    });
  });

  it("stacks several toasts and caps the stack", () => {
    render(<ToastViewport />);
    act(() => {
      for (let i = 0; i < 6; i += 1) toast.show({ message: `msg ${String(i)}` });
    });
    expect(screen.getAllByText(/^msg /)).toHaveLength(4);
    expect(screen.queryByText("msg 0")).not.toBeInTheDocument();
    expect(screen.getByText("msg 5")).toBeInTheDocument();
  });

  it("auto-dismisses after its duration", async () => {
    render(<ToastViewport />);
    act(() => {
      toast.show({ message: "Quick", durationMs: 50 });
    });
    await waitFor(() => {
      expect(screen.queryByText("Quick")).not.toBeInTheDocument();
    });
  });

  it("uses the 100 ms fade under reduced motion", () => {
    mockReducedMotion(true);
    render(<ToastViewport />);
    act(() => {
      toast.show({ message: "Calm" });
    });
    expect(screen.getByText("Calm").parentElement).toHaveAttribute("data-motion", "reduced");
  });
});

describe("surfaceMotion", () => {
  it("resolves sheet, popover and toast to an opacity-only 100 ms linear fade when reduced", () => {
    mockReducedMotion(true);
    for (const kind of ["sheet", "popover", "toast"] as const) {
      const m = surfaceMotion(kind);
      expect(m.reduced).toBe(true);
      expect(m.initial).toEqual({ opacity: 0 });
      expect(m.exit).toEqual({ opacity: 0 });
      expect(m.transition).toMatchObject({ type: "tween", duration: 0.1, ease: "linear" });
    }
  });

  it("uses 280 ms for the sheet and the 260/26 spring for popovers otherwise", () => {
    mockReducedMotion(false);
    expect(surfaceMotion("sheet").transition).toMatchObject({ type: "tween", duration: 0.28 });
    expect(surfaceMotion("popover").transition).toMatchObject({
      type: "spring",
      stiffness: 260,
      damping: 26,
    });
  });
});

describe("Progress", () => {
  it("shows percent, speed and ETA while running", () => {
    render(
      <Progress value={42} label="Downloading PHP" speedBps={12 * 1024 * 1024} etaSeconds={75} />,
    );
    const bar = screen.getByRole("progressbar", { name: "Downloading PHP" });
    expect(bar).toHaveAttribute("aria-valuenow", "42");
    expect(screen.getByText("42% · 12 MB/s · 1m 15s left")).toBeInTheDocument();
    expect(bar.closest("[data-state]")).toHaveAttribute("data-state", "loading");
  });

  it("morphs to a check with a done label at 100%", () => {
    const { container } = render(
      <Progress value={100} label="Downloading PHP" speedBps={1} etaSeconds={0} />,
    );
    expect(screen.getByText("Done")).toBeInTheDocument();
    expect(screen.queryByText(/MB\/s|left/)).not.toBeInTheDocument();
    expect(container.querySelector("[data-part=check]")).not.toBeNull();
    expect(container.firstElementChild).toHaveAttribute("data-state", "done");
  });

  it("supports the circular variant and indeterminate state", () => {
    const { container, rerender } = render(
      <Progress value={30} variant="circular" label="Index" />,
    );
    expect(container.querySelector("svg circle")).not.toBeNull();
    rerender(<Progress value={null} label="Index" />);
    expect(container.firstElementChild).toHaveAttribute("data-state", "indeterminate");
  });

  it("formats speed and ETA in pt-BR", async () => {
    await i18next.changeLanguage("pt-BR");
    try {
      render(<Progress value={50} label="Baixando" speedBps={2 * 1024 * 1024} etaSeconds={30} />);
      expect(screen.getByText("50% · 2 MB/s · faltam 30s")).toBeInTheDocument();
    } finally {
      await i18next.changeLanguage("en-US");
    }
  });
});

describe("Skeleton", () => {
  it("keeps the slot height from skeleton to content", () => {
    const { container, rerender } = render(
      <SkeletonSlot loading height={64} label="Loading">
        <div>content</div>
      </SkeletonSlot>,
    );
    const slot = container.querySelector<HTMLElement>("[data-skeleton-slot]");
    expect(slot?.style.height).toBe("64px");
    expect(container.querySelector<HTMLElement>("[data-skeleton]")?.style.height).toBe("64px");
    expect(slot).toHaveAttribute("aria-busy", "true");
    rerender(
      <SkeletonSlot loading={false} height={64} label="Loading">
        <div>content</div>
      </SkeletonSlot>,
    );
    expect(screen.getByText("content")).toBeInTheDocument();
    expect(container.querySelector<HTMLElement>("[data-skeleton-slot]")?.style.height).toBe("64px");
    expect(container.querySelector("[data-skeleton]")).toBeNull();
  });

  it("has an explicit fixed height", () => {
    const { container } = render(<Skeleton height={20} width={80} />);
    const el = container.firstElementChild as HTMLElement;
    expect(el.style.height).toBe("20px");
    expect(el.style.width).toBe("80px");
  });
});

describe("Tooltip", () => {
  it("shows the content and the CLI command slot on focus", async () => {
    const user = userEvent.setup();
    render(
      <Tooltip content="Restart service" command="forge restart nginx" delayMs={0}>
        <button type="button">restart</button>
      </Tooltip>,
    );
    await user.tab();
    const tips = await screen.findAllByText("forge restart nginx");
    expect(tips[0]).toHaveAttribute("data-slot", "command");
    expect(screen.getAllByText("Restart service").length).toBeGreaterThan(0);
  });
});
