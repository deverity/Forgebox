import { Fragment, type ReactNode } from "react";
import { openUrl } from "../../ipc/native";
import { LINK_CLASS } from "./links";
import { safeHttpUrl } from "./model";

// A small Markdown renderer for manifest descriptions, which are untrusted (docs/11, docs/04).
// It builds React elements only: raw HTML is never parsed (it shows as text, `<img onerror>`
// included), images are not rendered (their alt text is), and links are http(s) only and open
// in the system browser. No `dangerouslySetInnerHTML` anywhere.

const INLINE =
  /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(__[^_\n]+__)|(!?\[[^\]\n]*\]\([^)\s]*\))|(\*[^*\s][^*\n]*\*)|(_[^_\s][^_\n]*_)/;
const LINK = /^(!?)\[([^\]\n]*)\]\(([^)\s]*)\)$/;

function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a
      href={href}
      rel="noopener noreferrer"
      className={LINK_CLASS}
      onClick={(event) => {
        event.preventDefault();
        void openUrl(href);
      }}
    >
      {children}
    </a>
  );
}

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let rest = text;
  let index = 0;
  while (rest.length > 0) {
    const match = INLINE.exec(rest);
    if (match === null) {
      nodes.push(rest);
      break;
    }
    if (match.index > 0) nodes.push(rest.slice(0, match.index));
    const token = match[0];
    const key = `${keyPrefix}-${String(index)}`;
    index += 1;
    if (token.startsWith("`")) {
      nodes.push(
        <code key={key} className="rounded-control bg-surface-2 px-1 font-mono text-mono">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**") || token.startsWith("__")) {
      nodes.push(<strong key={key}>{renderInline(token.slice(2, -2), key)}</strong>);
    } else if (token.startsWith("*") || token.startsWith("_")) {
      nodes.push(<em key={key}>{renderInline(token.slice(1, -1), key)}</em>);
    } else {
      const link = LINK.exec(token);
      const isImage = link?.[1] === "!";
      const label = link?.[2] ?? token;
      const url = isImage ? null : safeHttpUrl(link?.[3]);
      if (url === null) {
        nodes.push(<Fragment key={key}>{label}</Fragment>);
      } else {
        nodes.push(
          <ExternalLink key={key} href={url}>
            {renderInline(label, key)}
          </ExternalLink>,
        );
      }
    }
    rest = rest.slice(match.index + token.length);
  }
  return nodes;
}

const HEADING = /^(#{1,6})\s+(.*)$/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const ORDERED = /^\s*\d+[.)]\s+(.*)$/;
const QUOTE = /^>\s?(.*)$/;
const RULE = /^\s*([-*_])(\s*\1){2,}\s*$/;

type Heading = "h2" | "h3" | "h4" | "h5" | "h6";
// The page owns h1: Markdown headings start at h2 (heading order stays valid).
const HEADINGS: readonly Heading[] = ["h2", "h3", "h4", "h5", "h6", "h6"];

/** Renders `source` (untrusted Markdown) as safe React elements. */
export function SafeMarkdown({ source }: { source: string }) {
  const lines = source.replaceAll("\r\n", "\n").split("\n");
  const at = (index: number) => lines[index] ?? "";
  const blocks: ReactNode[] = [];
  let i = 0;
  let n = 0;
  const key = () => `b${String(n++)}`;
  while (i < lines.length) {
    const line = at(i);
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    if (line.startsWith("```")) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !at(i).startsWith("```")) {
        code.push(at(i));
        i += 1;
      }
      i += 1;
      blocks.push(
        <pre
          key={key()}
          className="overflow-x-auto rounded-control bg-surface-2 p-2 font-mono text-mono"
        >
          <code>{code.join("\n")}</code>
        </pre>,
      );
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading !== null) {
      const Tag = HEADINGS[(heading[1]?.length ?? 1) - 1] ?? "h6";
      const k = key();
      blocks.push(
        <Tag key={k} className="text-headline">
          {renderInline(heading[2] ?? "", k)}
        </Tag>,
      );
      i += 1;
      continue;
    }
    if (RULE.test(line)) {
      blocks.push(<hr key={key()} className="border-stroke" />);
      i += 1;
      continue;
    }
    const list = BULLET.test(line) ? BULLET : ORDERED.test(line) ? ORDERED : null;
    if (list !== null) {
      const items: string[] = [];
      while (i < lines.length && list.test(at(i))) {
        items.push(list.exec(at(i))?.[1] ?? "");
        i += 1;
      }
      const k = key();
      const children = items.map((item, position) => (
        <li key={position}>{renderInline(item, `${k}-${String(position)}`)}</li>
      ));
      blocks.push(
        list === BULLET ? (
          <ul key={k} className="list-disc pl-5">
            {children}
          </ul>
        ) : (
          <ol key={k} className="list-decimal pl-5">
            {children}
          </ol>
        ),
      );
      continue;
    }
    if (QUOTE.test(line)) {
      const quoted: string[] = [];
      while (i < lines.length && QUOTE.test(at(i))) {
        quoted.push(QUOTE.exec(at(i))?.[1] ?? "");
        i += 1;
      }
      const k = key();
      blocks.push(
        <blockquote key={k} className="border-l-2 border-stroke-strong pl-3 text-fg-secondary">
          {renderInline(quoted.join(" "), k)}
        </blockquote>,
      );
      continue;
    }
    const paragraph: string[] = [];
    while (
      i < lines.length &&
      at(i).trim() !== "" &&
      !at(i).startsWith("```") &&
      !HEADING.test(at(i)) &&
      !BULLET.test(at(i)) &&
      !ORDERED.test(at(i)) &&
      !QUOTE.test(at(i))
    ) {
      paragraph.push(at(i).trim());
      i += 1;
    }
    const k = key();
    blocks.push(<p key={k}>{renderInline(paragraph.join(" "), k)}</p>);
  }
  return (
    <div data-testid="markdown" className="flex flex-col gap-2 text-body">
      {blocks}
    </div>
  );
}
