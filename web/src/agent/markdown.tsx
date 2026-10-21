// A small Markdown subset for agent text, rendered to React elements (never innerHTML):
// paragraphs, `-` lists, pipe tables, **bold**, *italic* and `code`.
import type { ReactNode } from "react";

const INLINE = /(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`)/g;

export function inline(text: string): ReactNode[] {
  return text.split(INLINE).map((part, i) => {
    if (part.startsWith("**") && part.endsWith("**") && part.length > 4) return <strong key={i}>{part.slice(2, -2)}</strong>;
    if (part.startsWith("`") && part.endsWith("`") && part.length > 2) return <code key={i}>{part.slice(1, -1)}</code>;
    if (part.startsWith("*") && part.endsWith("*") && part.length > 2) return <em key={i}>{part.slice(1, -1)}</em>;
    return part;
  });
}

const cells = (row: string) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
const isRule = (row: string) => /^\|?(\s*:?-+:?\s*\|)+\s*:?-*:?\s*\|?$/.test(row.trim());

function Table({ rows }: { rows: string[] }) {
  const [head = [], ...body] = rows.filter((r) => !isRule(r)).map(cells);
  return (
    <table className="md-table">
      <thead><tr>{head.map((c, i) => <th key={i}>{inline(c)}</th>)}</tr></thead>
      <tbody>{body.map((r, i) => <tr key={i}>{r.map((c, j) => <td key={j}>{inline(c)}</td>)}</tr>)}</tbody>
    </table>
  );
}

function Chunk({ chunk }: { chunk: string }) {
  const lines = chunk.split("\n");
  if (lines.every((l) => l.trim().startsWith("|"))) return <Table rows={lines} />;
  if (lines.every((l) => l.trim().startsWith("- "))) {
    return <ul>{lines.map((l, i) => <li key={i}>{inline(l.trim().slice(2))}</li>)}</ul>;
  }
  return <p>{inline(chunk)}</p>;
}

/** Renders `text`, which may still be streaming (a half-written table renders as rows so far). */
export function Markdown({ text }: { text: string }) {
  const chunks = text.split(/\n{2,}/).filter((c) => c.trim() !== "");
  return <div className="md">{chunks.map((c, i) => <Chunk key={i} chunk={c} />)}</div>;
}
