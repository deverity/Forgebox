// Acceptance of M2-E2-T18 (criterion 1): a hostile icon never becomes an inline <svg> or
// handler attributes, and Markdown with raw HTML or <img onerror> renders as plain text.
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HOSTILE_SVG } from "../../../qa/fixtures/scenarios/catalog";
import { ForgeStateProvider } from "../../state";
import { startHarness, type Harness } from "../../state/testFixtures";
import { CatalogIcon, resetIconCache } from "./CatalogIcon";
import { SafeMarkdown } from "./SafeMarkdown";
import { safeHttpUrl } from "./model";

const opened = vi.hoisted(() => [] as string[]);
vi.mock("../../ipc/native", () => ({
  copyText: () => Promise.resolve(true),
  openUrl: (url: string) => {
    opened.push(url);
    return Promise.resolve(true);
  },
}));

let harness: Harness | null = null;
beforeEach(() => {
  resetIconCache();
  opened.length = 0;
});
afterEach(async () => {
  await harness?.layer.dispose();
  harness = null;
});

describe("CatalogIcon", () => {
  it("renders a hostile SVG only as an <img> with a data URI", async () => {
    harness = await startHarness({
      scenario: {
        notImplemented: [],
        handlers: {
          "tools.icon": ({ tool }) => ({ tool, svg: HOSTILE_SVG, sha256: "a".repeat(64) }),
        },
      },
    });
    const h = harness;
    const { container } = render(
      <ForgeStateProvider layer={h.layer}>
        <CatalogIcon tool="shady" sha256={"a".repeat(64)} />
      </ForgeStateProvider>,
    );
    await waitFor(() => {
      expect(container.querySelector("img")).not.toBeNull();
    });
    const img = container.querySelector("img");
    expect(img?.getAttribute("src")).toMatch(/^data:image\/svg\+xml;charset=utf-8,/);
    // Nothing of the payload is in the document: no inline <svg>, <script>, <image> or handlers.
    expect(container.querySelector("svg")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("image")).toBeNull();
    for (const element of container.querySelectorAll("*")) {
      for (const name of element.getAttributeNames()) {
        expect(name.startsWith("on"), `${element.tagName} has ${name}`).toBe(false);
      }
    }
    expect((window as { __pwned?: number }).__pwned).toBeUndefined();
  });

  it("makes no call for a tool without an icon hash and shows the monogram", async () => {
    harness = await startHarness({ scenario: { notImplemented: [] } });
    const h = harness;
    const { container } = render(
      <ForgeStateProvider layer={h.layer}>
        <CatalogIcon tool="mysql" sha256={null} />
      </ForgeStateProvider>,
    );
    await act(async () => {
      await h.runtime.settled();
    });
    expect(h.count("tools.icon")).toBe(0);
    expect(container.querySelector("img")).toBeNull();
    // A coloured monogram tile (M2-E1-T23), never a neutral or black placeholder.
    expect(container.textContent).toBe("My");
    expect(container.querySelector("[data-tool-tile]")).toHaveAttribute(
      "data-tool-tile",
      "monogram",
    );
  });

  it("fetches an icon once per hash, whatever the tool", async () => {
    harness = await startHarness({
      scenario: {
        notImplemented: [],
        handlers: {
          "tools.icon": ({ tool }) => ({ tool, svg: "<svg/>", sha256: "b".repeat(64) }),
        },
      },
    });
    const h = harness;
    const sha = "b".repeat(64);
    const { rerender } = render(
      <ForgeStateProvider layer={h.layer}>
        <CatalogIcon tool="one" sha256={sha} />
      </ForgeStateProvider>,
    );
    await waitFor(() => {
      expect(document.querySelector("img")).not.toBeNull();
    });
    rerender(
      <ForgeStateProvider layer={h.layer}>
        <CatalogIcon tool="one" sha256={sha} />
        <CatalogIcon tool="two" sha256={sha} />
      </ForgeStateProvider>,
    );
    await waitFor(() => {
      expect(document.querySelectorAll("img")).toHaveLength(2);
    });
    expect(h.count("tools.icon")).toBe(1);
  });
});

describe("SafeMarkdown", () => {
  it("renders raw HTML and <img onerror> as text", () => {
    const source = '<img src=x onerror="alert(1)"> and <script>alert(2)</script> <b>bold?</b>';
    const { container } = render(<SafeMarkdown source={source} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("b")).toBeNull();
    expect(container.textContent).toContain('<img src=x onerror="alert(1)">');
    expect(container.textContent).toContain("<script>alert(2)</script>");
    for (const element of container.querySelectorAll("*")) {
      expect(element.getAttributeNames().some((name) => name.startsWith("on"))).toBe(false);
    }
  });

  it("renders Markdown blocks and inline marks", () => {
    const { container } = render(
      <SafeMarkdown
        source={[
          "# Title",
          "",
          "A **bold** and *soft* word with `code`.",
          "",
          "- one",
          "- two",
          "",
          "1. first",
          "2. second",
          "",
          "```",
          "a < b",
          "```",
        ].join("\n")}
      />,
    );
    expect(container.querySelector("h2")?.textContent).toBe("Title");
    expect(container.querySelector("strong")?.textContent).toBe("bold");
    expect(container.querySelector("em")?.textContent).toBe("soft");
    expect(container.querySelector("p code")?.textContent).toBe("code");
    expect(container.querySelectorAll("ul li")).toHaveLength(2);
    expect(container.querySelectorAll("ol li")).toHaveLength(2);
    expect(container.querySelector("pre code")?.textContent).toBe("a < b");
  });

  it("opens http(s) links externally and drops other schemes and images", async () => {
    const { container } = render(
      <SafeMarkdown
        source={
          "[docs](https://example.com/docs) [bad](javascript:alert(1)) ![pic](https://example.com/p.png)"
        }
      />,
    );
    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0]?.getAttribute("rel")).toBe("noopener noreferrer");
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("bad");
    expect(container.textContent).toContain("pic");
    await userEvent.setup().click(screen.getByRole("link", { name: "docs" }));
    expect(opened).toEqual(["https://example.com/docs"]);
  });
});

describe("safeHttpUrl", () => {
  it("accepts only http and https", () => {
    expect(safeHttpUrl("https://example.com/x")).toBe("https://example.com/x");
    expect(safeHttpUrl("http://example.com/")).toBe("http://example.com/");
    expect(safeHttpUrl("javascript:alert(1)")).toBeNull();
    expect(safeHttpUrl("data:text/html,<b>")).toBeNull();
    expect(safeHttpUrl("file:///c:/x")).toBeNull();
    expect(safeHttpUrl("not a url")).toBeNull();
    expect(safeHttpUrl(null)).toBeNull();
  });
});
