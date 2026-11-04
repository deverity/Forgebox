import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  PHISHING_LINK,
  mailHandlers,
  manyMessages,
  message,
  seedMessages,
  type FakeMessage,
} from "../../../qa/fixtures/scenarios/mail";
import { renderApp, type AppHarness } from "../../app/testApp";
import { fakeError } from "../../ipc/fakeBridge";
import { instance, populatedData, type ScenarioData } from "../../state/testFixtures";
import { DELETE_UNDO_MS, resetPendingDeletes } from "./pendingDeletes";

const native = vi.hoisted(() => ({
  openUrl: vi.fn(() => Promise.resolve(true)),
  saveAttachment: vi.fn(() => Promise.resolve({ status: "saved", path: "C:/x/invoice.txt" })),
  copyText: vi.fn(() => Promise.resolve(true)),
}));
vi.mock("../../ipc/native", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc/native")>()),
  ...native,
}));

const MAIL_ID = "01JINST00000000000000MAILP";
let harness: AppHarness | null = null;

beforeEach(() => {
  // jsdom has no layout: give every element the viewport the virtualizer reads.
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => 1000,
  });
});

afterEach(async () => {
  vi.useRealTimers();
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
  resetPendingDeletes();
  native.openUrl.mockClear();
  native.saveAttachment.mockClear();
  native.copyText.mockClear();
  await harness?.layer.dispose();
  harness = null;
});

function withMailpit(state: "running" | "stopped" = "running"): ScenarioData {
  const data = populatedData();
  data.instances.push(instance(MAIL_ID, "mailpit/default", { state }));
  return data;
}

async function open(
  box: FakeMessage[] = seedMessages(),
  options: { data?: ScenarioData; handlers?: Parameters<typeof renderApp>[0] } = {},
) {
  const data = options.data ?? withMailpit();
  const mail = mailHandlers(data, box);
  harness = await renderApp({
    path: "/mail",
    data,
    ...options.handlers,
    extra: { handlers: { ...mail.handlers, ...options.handlers?.extra?.handlers } },
  });
  return { h: harness, mail };
}

const calls = (h: AppHarness, method: string) => h.fake.calls.filter((c) => c.method === method);
const badge = () => screen.queryByTestId("badge-mail");

describe("Mail screen states", () => {
  it("shows the empty mailbox with the SMTP credentials", async () => {
    await open([]);
    expect(await screen.findByText("No mail yet")).toBeInTheDocument();
    expect(await screen.findByTestId("mail-env")).toHaveTextContent("MAIL_HOST=127.0.0.1");
    expect(badge()).toBeNull();
  });

  it("offers to install Mailpit when no tool declares mail", async () => {
    harness = await renderApp({ path: "/mail" });
    const state = await screen.findByTestId("mail-no-tool");
    expect(within(state).getByText("forge tool install mailpit")).toBeInTheDocument();
    expect(
      within(state).getByRole("link", { name: "Open Mailpit in the catalog" }),
    ).toHaveAttribute("href", "/catalog/mailpit");
  });

  it("shows a skeleton while mail.status is pending", async () => {
    await open(seedMessages(), {
      handlers: {
        extra: { handlers: { "mail.status": () => new Promise(() => undefined) } },
      },
    });
    const busy = await waitFor(() => {
      const element = document.querySelector('[aria-busy="true"]');
      expect(element).not.toBeNull();
      return element;
    });
    expect(busy).toBeInTheDocument();
    expect(screen.queryByTestId("mail-list")).toBeNull();
  });

  it("says Mailpit did not answer and offers to restart it", async () => {
    const { h } = await open(seedMessages(), {
      handlers: {
        extra: {
          handlers: {
            "mail.list": () => {
              fakeError("timeout", "mailpit did not answer in time");
            },
          },
        },
      },
    });
    const error = await screen.findByTestId("mail-error");
    expect(within(error).getByText("Mailpit did not answer")).toBeInTheDocument();
    await userEvent.setup().click(within(error).getByRole("button", { name: "Restart Mailpit" }));
    await waitFor(() => {
      expect(calls(h, "services.restart").length).toBe(1);
    });
    expect(calls(h, "services.restart")[0]?.params).toMatchObject({ targets: ["mailpit/default"] });
  });

  it("explains a stopped Mailpit", async () => {
    await open(seedMessages(), { data: withMailpit("stopped") });
    expect(await screen.findByTestId("mail-stopped")).toBeInTheDocument();
    expect(screen.getByText("Mailpit is stopped")).toBeInTheDocument();
  });
});

describe("Mail screen actions", () => {
  async function ready() {
    const out = await open();
    await screen.findByTestId("mail-list");
    await screen.findByText("Welcome aboard");
    return out;
  }

  it("marks a message read when opened and the badge follows", async () => {
    await ready();
    // Two of the three seeded messages are unread.
    await waitFor(() => {
      expect(badge()).toHaveTextContent("2");
    });
    await userEvent.setup().click(screen.getByText("Welcome aboard"));
    await waitFor(() => {
      expect(badge()).toHaveTextContent("1");
    });
    const pane = await screen.findByTestId("mail-pane");
    // Marking it unread again sticks (opening does not re-read it).
    await userEvent.setup().click(within(pane).getByRole("button", { name: "Mark as unread" }));
    await waitFor(() => {
      expect(badge()).toHaveTextContent("2");
    });
    expect(await within(pane).findByRole("button", { name: "Mark as read" })).toBeInTheDocument();
  });

  it("renders the HTML only in a sandboxed frame with a CSP and no remote requests", async () => {
    await ready();
    await userEvent.setup().click(screen.getByText("Reset your password"));
    const frame = await screen.findByTestId("mail-html-frame");
    expect(frame.getAttribute("sandbox")).toBe("allow-popups allow-popups-to-escape-sandbox");
    const srcdoc = frame.getAttribute("srcdoc") ?? "";
    expect(srcdoc).toContain("default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'");
    expect(srcdoc).not.toContain("<script");
    expect(srcdoc).not.toContain("onclick");
    expect(srcdoc).not.toContain("href=");
    expect(screen.getByTestId("mail-remote-images")).toHaveTextContent("1 remote image blocked");

    await userEvent.setup().click(screen.getByRole("button", { name: "Show images" }));
    expect(screen.getByTestId("mail-html-frame").getAttribute("srcdoc")).toContain(
      "img-src data: cid: http: https:",
    );
  });

  it("confirms a link with its URL before opening it outside", async () => {
    await ready();
    const user = userEvent.setup();
    await user.click(screen.getByText("Reset your password"));
    await screen.findByTestId("mail-html-frame");
    await user.click(screen.getByText("1 link"));
    await user.click(screen.getByRole("button", { name: PHISHING_LINK }));
    const dialog = await screen.findByTestId("mail-link-dialog");
    expect(within(dialog).getByTestId("mail-link-url")).toHaveTextContent(PHISHING_LINK);
    expect(native.openUrl).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole("button", { name: "Open link" }));
    expect(native.openUrl).toHaveBeenCalledWith(PHISHING_LINK);
  });

  it("falls back to the text when the HTML is too large", async () => {
    const big = message({
      subject: "Huge newsletter",
      html: `<p>${"x".repeat(600 * 1024)}</p>`,
      text: "the readable text",
    });
    await open([big]);
    await userEvent.setup().click(await screen.findByText("Huge newsletter"));
    expect(await screen.findByTestId("mail-too-large")).toBeInTheDocument();
    expect(screen.queryByTestId("mail-html-frame")).toBeNull();
    await userEvent.setup().click(screen.getByRole("button", { name: "View text" }));
    expect(await screen.findByTestId("mail-text")).toHaveTextContent("the readable text");
  });

  it("downloads an attachment through the save dialog", async () => {
    await ready();
    const user = userEvent.setup();
    await user.click(screen.getByText("Invoice 2026-11"));
    await user.click(await screen.findByRole("tab", { name: "Attachments (1)" }));
    await user.click(await screen.findByRole("button", { name: "Download invoice.txt" }));
    await waitFor(() => {
      expect(native.saveAttachment).toHaveBeenCalledWith(
        "invoice.txt",
        btoa("hello world"),
        "text/plain",
      );
    });
  });

  it("shows an error and re-enables the button when saving the attachment fails", async () => {
    native.saveAttachment.mockImplementationOnce(() => Promise.reject(new Error("disk full")));
    await ready();
    const user = userEvent.setup();
    await user.click(screen.getByText("Invoice 2026-11"));
    await user.click(await screen.findByRole("tab", { name: "Attachments (1)" }));
    const button = await screen.findByRole("button", { name: "Download invoice.txt" });
    await user.click(button);
    await waitFor(() => {
      expect(native.saveAttachment).toHaveBeenCalledTimes(1);
      expect(button).toBeEnabled();
    });
    expect(await screen.findByText(/disk full|erro|error/i)).toBeInTheDocument();
  });

  it("copies the .env variables", async () => {
    await open([]);
    const user = userEvent.setup();
    await screen.findByTestId("mail-env");
    await user.click(screen.getByRole("button", { name: "Copy .env" }));
    await waitFor(() => {
      expect(native.copyText).toHaveBeenCalledTimes(1);
    });
    const text = (native.copyText.mock.calls[0] as unknown as [string])[0];
    expect(text).toContain("MAIL_HOST=127.0.0.1\nMAIL_PORT=1025");
  });

  it("filters by search text and by unread", async () => {
    const { h } = await ready();
    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox", { name: "Search mail" }), "invoice");
    await waitFor(() => {
      expect(
        calls(h, "mail.list").some((c) => (c.params as { query?: string }).query === "invoice"),
      ).toBe(true);
    });
    await waitFor(() => {
      expect(screen.queryByText("Welcome aboard")).toBeNull();
    });
    expect(screen.getByText("Invoice 2026-11")).toBeInTheDocument();
    await user.clear(screen.getByRole("searchbox", { name: "Search mail" }));
    await user.click(screen.getByRole("radio", { name: "Unread" }));
    await waitFor(() => {
      expect(
        calls(h, "mail.list").some((c) => (c.params as { unread?: boolean }).unread === true),
      ).toBe(true);
    });
    await waitFor(() => {
      expect(screen.queryByText("Invoice 2026-11")).toBeNull();
    });
  });

  it("virtualizes a long mailbox", async () => {
    await open(manyMessages(500));
    await screen.findByTestId("mail-list");
    await screen.findByText("Message 1");
    const rows = document.querySelectorAll("[data-mail-row]");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThan(40);
  });

  it("clears the mailbox after a destructive confirmation that shows the command", async () => {
    const { h } = await ready();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Clear mailbox" }));
    const dialog = await screen.findByTestId("mail-clear-dialog");
    expect(within(dialog).getByText("forge mail clear --yes")).toBeInTheDocument();
    expect(calls(h, "mail.clear")).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "Clear mailbox" }));
    await waitFor(() => {
      expect(calls(h, "mail.clear")).toHaveLength(1);
    });
    expect(calls(h, "mail.clear")[0]?.params).toEqual({ confirm: true });
    expect(await screen.findByText("No mail yet")).toBeInTheDocument();
  });
});

describe("delete with undo", () => {
  async function selectFirst() {
    const out = await open();
    await screen.findByText("Welcome aboard");
    fireEvent.click(screen.getByText("Welcome aboard"));
    await within(await screen.findByTestId("mail-pane")).findByRole("button", { name: "Delete" });
    return out;
  }

  it("hides the message at once and undo cancels the call", async () => {
    const { h } = await selectFirst();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.click(
      within(screen.getByTestId("mail-pane")).getByRole("button", { name: "Delete" }),
    );
    expect(screen.queryByText("Welcome aboard")).toBeNull();
    expect(calls(h, "mail.delete")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    act(() => {
      vi.advanceTimersByTime(DELETE_UNDO_MS + 1000);
    });
    expect(calls(h, "mail.delete")).toHaveLength(0);
    expect(within(screen.getByTestId("mail-list")).getByText("Welcome aboard")).toBeInTheDocument();
  });

  it("calls mail.delete only when the 5 s window ends", async () => {
    const { h, mail } = await selectFirst();
    const before = mail.box.length;
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.click(
      within(screen.getByTestId("mail-pane")).getByRole("button", { name: "Delete" }),
    );
    act(() => {
      vi.advanceTimersByTime(DELETE_UNDO_MS - 100);
    });
    expect(calls(h, "mail.delete")).toHaveLength(0);
    act(() => {
      vi.advanceTimersByTime(200);
    });
    vi.useRealTimers();
    await waitFor(() => {
      expect(calls(h, "mail.delete")).toHaveLength(1);
    });
    expect(calls(h, "mail.delete")[0]?.params).toMatchObject({ confirm: true });
    expect(mail.box.length).toBe(before - 1);
  });
});

describe("live updates", () => {
  it("updates the badge from mail.changed and refetches the list once, with no polling", async () => {
    const { h, mail } = await open();
    await screen.findByText("Welcome aboard");
    await waitFor(() => {
      expect(badge()).toHaveTextContent("2");
    });
    // No timer ever asks again: the calls made while the screen opened are the only ones.
    const listCalls = calls(h, "mail.list").length;
    const statusCalls = calls(h, "mail.status").length;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1200));
    });
    expect(calls(h, "mail.list")).toHaveLength(listCalls);
    expect(calls(h, "mail.status")).toHaveLength(statusCalls);

    act(() => {
      mail.receive({ fake: h.fake }, message({ subject: "Just arrived" }));
      h.fake.flush();
    });
    await waitFor(() => {
      expect(badge()).toHaveTextContent("3");
    });
    expect(await screen.findByText("Just arrived")).toBeInTheDocument();
    // One refetch for the event burst, nothing more.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 600));
    });
    expect(calls(h, "mail.list")).toHaveLength(listCalls + 1);
  });
});
