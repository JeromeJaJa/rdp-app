import { Fragment, type ReactNode } from "react";
import { be } from "../../state/actions";
import "./markdown.css";

/**
 * The little Markdown that changelogs and release notes use: headings, paragraphs, bullet
 * lists, and inline code, bold and links. Built as React elements (never as HTML), so notes
 * can't inject markup. Link reference lines ("[1.0.0]: https://…") are dropped, and a bare
 * "[text]" shows as its text.
 */
export function Markdown({ text, className }: { text: string; className?: string }) {
  const blocks: ReactNode[] = [];
  let list: string[] = [];
  let para: string[] = [];
  const flush = () => {
    if (list.length) {
      blocks.push(
        <ul key={blocks.length}>
          {list.map((item, i) => (
            <li key={i}>{inline(item)}</li>
          ))}
        </ul>,
      );
      list = [];
    }
    if (para.length) {
      blocks.push(<p key={blocks.length}>{inline(para.join(" "))}</p>);
      para = [];
    }
  };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trimEnd();
    if (/^\[[^\]]+\]:\s+https?:\/\//.test(line)) continue;
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    const item = line.match(/^\s*[-*]\s+(.*)$/);
    if (heading) {
      flush();
      const level = heading[1].length;
      const content = inline(heading[2]);
      blocks.push(
        level <= 2 ? (
          <h3 key={blocks.length}>{content}</h3>
        ) : (
          <h4 key={blocks.length}>{content}</h4>
        ),
      );
    } else if (item) {
      if (para.length) flush();
      list.push(item[1]);
    } else if (!line.trim()) {
      flush();
    } else if (list.length && /^\s+/.test(raw)) {
      // A wrapped list item continues.
      list[list.length - 1] += " " + line.trim();
    } else {
      if (list.length) flush();
      para.push(line.trim());
    }
  }
  flush();
  return <div className={`md ${className ?? ""}`}>{blocks}</div>;
}

const INLINE = /(`[^`]+`|\*\*[^*]+\*\*|\[[^\]]+\]\([^)\s]+\)|\[[^\]]+\])/g;

function inline(text: string): ReactNode {
  const parts = text.split(INLINE);
  return parts.map((part, i) => {
    if (!part) return null;
    if (part.startsWith("`") && part.endsWith("`")) return <code key={i}>{part.slice(1, -1)}</code>;
    if (part.startsWith("**") && part.endsWith("**")) return <strong key={i}>{inline(part.slice(2, -2))}</strong>;
    const link = part.match(/^\[([^\]]+)\]\(([^)\s]+)\)$/);
    if (link) {
      const url = link[2];
      const safe = /^https?:\/\//.test(url);
      return safe ? (
        <a
          key={i}
          href={url}
          onClick={(e) => {
            e.preventDefault();
            void be().openExternal(url).catch(() => {});
          }}
        >
          {inline(link[1])}
        </a>
      ) : (
        <Fragment key={i}>{inline(link[1])}</Fragment>
      );
    }
    const bare = part.match(/^\[([^\]]+)\]$/);
    if (bare) return <Fragment key={i}>{bare[1]}</Fragment>;
    return <Fragment key={i}>{part}</Fragment>;
  });
}
