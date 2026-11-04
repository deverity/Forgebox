import { describe, expect, it } from "vitest";
import { renderCli } from "../../ipc/cli";
import {
  CSP_BLOCKED,
  CSP_REMOTE,
  FRAME_SANDBOX,
  MAX_SRCDOC_CHARS,
  envText,
  headerLines,
  prepareHtml,
  senderName,
} from "./model";

const HOSTILE = [
  "<html><head><style>p{color:red}</style><base href='https://evil.test/'></head><body>",
  "<script>alert(1)</script>",
  "<p onclick='x()' onmouseover=\"y()\">hi <a href='https://a.test/x' target='_blank'>a</a>",
  " <a href='javascript:alert(1)'>js</a> <a href='mailto:me@x.test'>mail</a></p>",
  "<img src='https://tracker.test/p.png'><img src='data:image/png;base64,AAAA'><img src='cid:logo'>",
  "<iframe src='https://evil.test'></iframe><object data='x'></object>",
  "<form action='https://evil.test'><input></form><link rel='stylesheet' href='https://evil.test/a.css'>",
  "<meta http-equiv='refresh' content='0;url=https://evil.test'>",
  "</body></html>",
].join("");

describe("prepareHtml", () => {
  it("drops active content, handlers and every href", () => {
    const { srcdoc } = prepareHtml(HOSTILE, false);
    expect(srcdoc).not.toBeNull();
    const html = srcdoc ?? "";
    for (const forbidden of [
      "<script",
      "onclick",
      "onmouseover",
      "href=",
      "<iframe",
      "<object",
      "<form",
      "<link",
      "refresh",
      "<base",
      "javascript:",
    ]) {
      expect(html).not.toContain(forbidden);
    }
    expect(html).toContain("p{color:red}");
    expect(html).toContain(`content="${CSP_BLOCKED}"`);
  });

  it("lists only the distinct http(s) links, in order", () => {
    const { links } = prepareHtml(HOSTILE + "<a href='https://a.test/x'>again</a>", false);
    expect(links).toEqual(["https://a.test/x"]);
  });

  it("counts remote images and widens only img-src when allowed", () => {
    expect(prepareHtml(HOSTILE, false).remoteImages).toBe(1);
    const allowed = prepareHtml(HOSTILE, true).srcdoc ?? "";
    expect(allowed).toContain(`content="${CSP_REMOTE}"`);
    expect(CSP_REMOTE).toContain("default-src 'none'");
    expect(CSP_REMOTE).not.toContain("script-src");
  });

  it("refuses to build a document above the cap", () => {
    const big = prepareHtml(`<p>${"x".repeat(MAX_SRCDOC_CHARS)}</p>`, false);
    expect(big.tooLarge).toBe(true);
    expect(big.srcdoc).toBeNull();
  });

  it("never grants scripts or same-origin to the frame", () => {
    expect(FRAME_SANDBOX.split(" ").sort()).toEqual([
      "allow-popups",
      "allow-popups-to-escape-sandbox",
    ]);
  });
});

describe("helpers", () => {
  it("formats the sender, headers and .env text", () => {
    expect(senderName('"Ana Souza" <ana@x.test>')).toBe("Ana Souza");
    expect(senderName("Forgebox App <app@loja.test>")).toBe("Forgebox App");
    expect(senderName("<bare@x.test>")).toBe("bare@x.test");
    expect(senderName("plain@x.test")).toBe("plain@x.test");
    expect(headerLines({ Received: ["a", "b"], Subject: ["s"] })).toEqual([
      "Received: a",
      "Received: b",
      "Subject: s",
    ]);
    expect(envText({ MAIL_HOST: "127.0.0.1", MAIL_PORT: "1025" })).toBe(
      "MAIL_HOST=127.0.0.1\nMAIL_PORT=1025",
    );
  });

  it("renders the CLI hints of the screen", () => {
    expect(renderCli("mail.mark_read", { ids: ["m1"], read: true })).toBe("forge mail read m1");
    expect(renderCli("mail.delete", { ids: ["m1"], confirm: true })).toBe("forge mail delete m1");
    expect(renderCli("mail.env", {})).toBe("forge mail env");
    expect(`${renderCli("mail.clear", { confirm: true }) ?? ""} --yes`).toBe(
      "forge mail clear --yes",
    );
  });
});
