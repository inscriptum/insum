// Differential corpus generator (insum spec §4.8).
//
// Loads the schema artifact (a snapshot of the plugin's runtime schema —
// the single source of truth), builds a real prosemirror-model Schema from
// it, generates valid documents and targeted mutations, and computes every
// case's expected verdict with the REAL library (`Node.fromJSON` +
// `Node.check`). The Rust validator must agree with these verdicts — the
// committed corpus is the gate.
//
// Determinism: mulberry32 with a fixed seed, no wall clock, no
// environment-dependent iteration. Re-running with the same artifact and
// seed must produce a byte-identical corpus.
//
// Scope rules (mirroring the artifact's own validation_limits):
// - noteDoc-rooted cases always keep a leading noteTitle;
// - documents only use node types declared in the artifact;
// - text nodes never carry a `content` field (PM text nodes have none).
//
// Run: npm install && npm run generate  →  ../fixtures/corpus.jsonl

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Node, Schema } from "prosemirror-model";

// ── deterministic RNG ────────────────────────────────────────────────────
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rng = mulberry32(20261109);
const pick = <T>(items: readonly T[]): T => items[Math.floor(rng() * items.length)]!;
const chance = (p: number): boolean => rng() < p;
let caseCounter = 0;
const nextId = (): string => `c${String(++caseCounter).padStart(4, "0")}`;

// ── artifact → PM schema ─────────────────────────────────────────────────
const here = dirname(fileURLToPath(import.meta.url));
const artifactPath =
  process.argv[2] ?? join(here, "..", "..", "schema", "inscriptum-schema.json");
const artifact = JSON.parse(readFileSync(artifactPath, "utf8"));

const nodeNames: string[] = Object.keys(artifact.nodes);
const markNames: string[] = Object.keys(artifact.marks);
// PM's inline/block mixing rule keys on `isInline`; the artifact does not
// carry it — inline nodes are exactly the members of the group "inline"
// (same derivation as the Rust side).
const isInline = (name: string): boolean =>
  (artifact.nodes[name]?.groups ?? []).includes("inline");
const inlineNodeNames = nodeNames.filter(isInline);

const schema = new Schema({
  topNode: artifact.top_node,
  nodes: Object.fromEntries(
    nodeNames.map((name) => {
      const spec = artifact.nodes[name];
      return [
        name,
        {
          content: spec.content ?? undefined,
          group: (spec.groups ?? []).join(" "),
          marks: (spec.marks ?? []).join(" "),
          attrs: Object.fromEntries(
            Object.entries(spec.attrs ?? {}).map(([attr, spec2]) => [
              // the artifact omits `default` for required attributes —
              // pass the key through only when it exists
              attr,
              "default" in (spec2 as object)
                ? { default: (spec2 as { default?: unknown }).default }
                : {},
            ]),
          ),
          inline: isInline(name),
        },
      ];
    }),
  ),
  marks: Object.fromEntries(
    markNames.map((name) => {
      const spec = artifact.marks[name];
      return [
        name,
        {
          group: spec.group ?? undefined,
          attrs: Object.fromEntries(
            Object.entries(spec.attrs ?? {}).map(([attr, spec2]) => [
              attr,
              "default" in (spec2 as object)
                ? { default: (spec2 as { default?: unknown }).default }
                : {},
            ]),
          ),
        },
      ];
    }),
  ),
});

// ── verdict: the REAL library decides ────────────────────────────────────
function pmVerdict(doc: unknown): { ok: boolean; error?: string } {
  try {
    Node.fromJSON(schema, doc).check();
    return { ok: true };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
}

const cases: { id: string; category: string; doc: unknown; expected: "valid" | "invalid" }[] = [];

// Scope guard: cases the differential corpus deliberately does not judge.
// 1) noteDoc roots must lead with noteTitle — the artifact's content rule
//    covers the title family only (its validation_limits declare document
//    families and versions out of scope); title-less or reordered roots
//    are exactly the real-base legacy/plain pages the Rust side skips by
//    design (SCHEMA_FAMILY_UNCOVERED).
// 2) `attrs`, when present, must be a plain object — PM silently treats
//    non-object attrs as absent, while the Rust structural contract
//    (stage A) rejects them; that shape check is outside the §15.4
//    coverage list (attrs/defaults semantics, not garbage shapes).
function assertInScope(doc: unknown): void {
  if (doc === null || typeof doc !== "object") return;
  const d = doc as Record<string, unknown>;
  if (d.type === "noteDoc" && Array.isArray(d.content)) {
    const first = (d.content as unknown[])[0];
    if (first !== null && typeof first === "object") {
      const firstType = (first as Record<string, unknown>).type;
      // only well-shaped roots are judged for family scope; garbage types
      // (non-string, missing) fail on both sides in the same way
      if (typeof firstType === "string" && firstType !== "noteTitle") {
        throw new Error(`out-of-scope case: noteDoc root must lead with noteTitle`);
      }
    }
  }
  if (
    d.attrs !== undefined &&
    d.attrs !== null &&
    (typeof d.attrs !== "object" || Array.isArray(d.attrs))
  ) {
    throw new Error(`out-of-scope case: attrs must be a plain object (PM ignores other shapes)`);
  }
  if (Array.isArray(d.content)) {
    for (const child of d.content) {
      if (child !== null && typeof child === "object") assertInScope(child);
    }
  }
}

function emit(category: string, doc: unknown, expectation?: "valid" | "invalid"): void {
  assertInScope(doc);
  const verdict = pmVerdict(doc);
  const expected = expectation ?? (verdict.ok ? "valid" : "invalid");
  if (expectation && expectation !== (verdict.ok ? "valid" : "invalid")) {
    throw new Error(
      `builder/disagreement for ${category}: expected ${expectation}, PM says ${verdict.ok ? "valid" : `invalid (${verdict.error})`}`,
    );
  }
  cases.push({ id: nextId(), category, doc, expected });
}

// ── builders (inventories come from the artifact, never hand-copied) ─────
const text = (s: string, marks?: unknown[]): unknown => {
  const node: Record<string, unknown> = { type: "text", text: s };
  if (marks && marks.length) node.marks = marks;
  return node;
};
const mark = (type: string, attrs?: Record<string, unknown>): unknown => {
  const m: Record<string, unknown> = { type };
  if (attrs) m.attrs = attrs;
  return m;
};
const node = (type: string, ...content: unknown[]): unknown => ({
  type,
  content,
});
const withAttrs = (type: string, attrs: unknown, ...content: unknown[]): unknown => ({
  type,
  attrs,
  content,
});

const BLOCKS = [
  "paragraph",
  "heading",
  "bulletList",
  "orderedList",
  "taskList",
  "blockquote",
  "horizontalRule",
  "table",
  "hljsCodeBlock",
  "mermaid",
] as const;
const block = (): unknown => {
  switch (pick(BLOCKS)) {
    case "paragraph": {
      const marks: unknown[] = [];
      if (chance(0.4)) marks.push(mark("bold"));
      if (chance(0.3)) marks.push(mark("italic"));
      if (chance(0.1)) marks.push(mark(pick(["code", "highlight", "underline", "strike"])));
      return node("paragraph", text(pick(["Заголовок", "Текст абзаца", "note", "данные 123"]), marks.length ? marks : undefined));
    }
    case "heading":
      return withAttrs("heading", { level: pick([2, 3]) }, text("Раздел"));
    case "bulletList":
      return node("bulletList", node("listItem", node("paragraph", text("пункт списка"))), node("listItem", node("paragraph", text("ещё пункт")), node("bulletList", node("listItem", node("paragraph", text("вложенный"))))));
    case "orderedList":
      return node("orderedList", node("listItem", node("paragraph", text("первый"))), node("listItem", node("paragraph", text("второй"))));
    case "taskList":
      return node("taskList", node("taskItem", node("paragraph", text("сделать")), node("paragraph", text("детали"))));
    case "blockquote":
      return node("blockquote", node("paragraph", text("цитата")), node("blockquote", node("paragraph", text("вложенная цитата"))));
    case "horizontalRule":
      return node("horizontalRule");
    case "table":
      return node(
        "table",
        node("tableRow", node("tableHeader", node("paragraph", text("Колонка"))), node("tableHeader", node("paragraph", text("Ещё")))),
        node("tableRow", node("tableCell", node("paragraph", text("a"))), node("tableCell", node("bulletList", node("listItem", node("paragraph", text("b")))))),
      );
    case "hljsCodeBlock":
      return node(
        "hljsCodeBlock",
        node("hljsCodeBlockRow", text("const x = 1;", [mark("hljsMark", { class: "language-ts" })])),
      );
    case "mermaid":
      return withAttrs("mermaid", { code: "graph TD; A-->B;" });
  }
};

// inline ATOMS of this schema: hardBreak and wikiLink are leaves with no
// content (wikiLink carries its label in attrs); image/attachment are
// block leaves, not inline
const inlineLeaf = (): unknown =>
  pick([() => node("hardBreak"), () => withAttrs("wikiLink", { path: "target", text: "метка" })])();

const blocks = (n: number): unknown[] => Array.from({ length: n }, () => block());

const titlePage = (): unknown => {
  const content: unknown[] = [node("noteTitle", text("Страница"))];
  if (chance(0.4)) {
    content.push(
      node("noteProperty", node("notePropertyName", text("description")), node("notePropertyValue", text("Описание страницы"))),
    );
  }
  content.push(...blocks(1 + Math.floor(rng() * 3)));
  if (chance(0.2)) content.push(withAttrs("image", { src: "assets/pic.png" }));
  return { type: "noteDoc", content };
};

// ── 1. valid: composed title-family pages ────────────────────────────────
for (let i = 0; i < 40; i++) emit("valid-title-page", titlePage(), "valid");

// ── 2. valid: every node type rooted alone (createAndFill-style) ─────────
for (const name of nodeNames) {
  const spec = artifact.nodes[name];
  if (!spec.content) continue; // roots without content are judged as leaves
  if (name === "noteDoc") continue; // scope rule: title-family roots only
  if (name === "noteTitle" || name.startsWith("noteProperty")) continue; // need page context
  if (name === "text" || name === "hardBreak" || name === "wikiLink") continue; // inline atoms
  emit(`solo-node-${name}`, { type: name, content: [] });
}

// minimal correct examples from the artifact itself, when provided
for (const name of nodeNames) {
  const example = artifact.nodes[name].example;
  if (example) emit(`valid-example-${name}`, example, undefined);
}

// ── 3. valid: attrs semantics ────────────────────────────────────────────
emit("valid-attrs-explicit-nulls", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    withAttrs("paragraph", { id: null, align: null }, text("a")),
    withAttrs("heading", { level: 4, align: "center" }, text("H")),
  ],
}, "valid");
emit("valid-attrs-unknown-are-dropped", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    withAttrs("paragraph", { id: "abcdefghij", color: "red", mystery: [1, 2] }, text("a")),
  ],
}, "valid");
emit("valid-attrs-wrong-typed-values-pass", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    withAttrs("heading", { level: "three" }, text("H")),
  ],
}, "valid");
emit("valid-text-unicode", {
  type: "noteDoc",
  content: [node("noteTitle", text("Заголовок — ™")), node("paragraph", text("текст — ™"))],
}, "valid");
emit("valid-mark-attrs-declared", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a", [mark("highlight", { color: "#ffcc00" })])),
  ],
}, "valid");
emit("valid-mark-attrs-missing-use-defaults", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), node("paragraph", text("a", [mark("comment")]))],
}, "valid");
emit("valid-marks-order-free", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a", [mark("italic"), mark("bold")])),
  ],
}, "valid");
emit("valid-inline-leaves-in-paragraph", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a"), inlineLeaf(), text("b"), withAttrs("wikiLink", { path: "doc", text: "ссылка" })),
  ],
}, "valid");
emit("valid-empty-blockquote-content-not-required", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    { type: "paragraph", content: [] },
  ],
}, "valid");

// ── 4. invalid: content expression violations ────────────────────────────
emit("invalid-empty-section-like-list", { type: "bulletList", content: [] });
emit("invalid-bulletlist-with-paragraph-child", {
  type: "bulletList",
  content: [node("paragraph", text("не listItem"))],
});
emit("invalid-tasklist-with-listitem", {
  type: "taskList",
  content: [node("listItem", node("paragraph", text("x")))],
});
emit("invalid-listitem-block-first", {
  type: "listItem",
  content: [node("blockquote", node("paragraph", text("нет первого paragraph")))],
});
emit("invalid-table-empty-rows", { type: "table", content: [] });
emit("invalid-tablerow-paragraph-child", {
  type: "tableRow",
  content: [node("paragraph", text("не ячейка"))],
});
emit("invalid-tablecell-empty", { type: "tableCell", content: [] });
emit("invalid-blockquote-empty", { type: "blockquote", content: [] });
emit("invalid-heading-with-listitem", {
  type: "heading",
  content: [node("listItem", node("paragraph", text("блок в заголовке")))],
});
emit("invalid-paragraph-with-block-child", {
  type: "paragraph",
  content: [node("paragraph", text("вложенный абзац"))],
});
emit("invalid-notedoc-second-title", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), node("noteTitle", text("повтор")), node("paragraph", text("a"))],
});
emit("invalid-text-directly-in-notedoc", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), text("голый текст")],
});
emit("invalid-noteproperty-value-first", {
  type: "noteProperty",
  content: [node("notePropertyValue", text("v")), node("notePropertyName", text("k"))],
});
// the artifact's hljs expression "(hljsCodeBlockRow | paragraph?)+"
// really allows paragraphs inside code blocks — PM confirmed it
emit("valid-hljsblock-paragraph-allowed", {
  type: "hljsCodeBlock",
  content: [node("paragraph", text("код"))],
}, "valid");
emit("invalid-hljsblock-heading-child", {
  type: "hljsCodeBlock",
  content: [node("heading", text("не ряд кода"))],
});

// ── 5. invalid: text nodes ───────────────────────────────────────────────
emit("invalid-empty-text", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), node("paragraph", text(""))],
});
emit("invalid-text-not-a-string", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), { type: "text", text: 42 }],
});
emit("invalid-text-missing", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), { type: "text" }],
});

// ── 6. invalid: unknown types (node and mark) ────────────────────────────
emit("invalid-unknown-node-type", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), { type: "exoticWidget", content: [text("?")] }],
});
emit("invalid-unknown-mark-type", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), node("paragraph", text("a", [mark("rainbow")]))],
});
emit("invalid-unknown-mark-on-title", {
  type: "noteDoc",
  content: [node("noteTitle", text("T", [mark("sparkle")]))],
});

// ── 7. invalid: mark-set rules ───────────────────────────────────────────
emit("invalid-duplicate-mark", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a", [mark("bold"), mark("bold")])),
  ],
});
emit("invalid-duplicate-mark-different-attrs", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a", [mark("highlight", { color: "#111111" }), mark("highlight", { color: "#222222" })])),
  ],
});
// heading's mark set excludes strike/underline/link/textStyle
const headingMarkSet: string[] = artifact.nodes.heading.marks;
for (const banned of ["strike", "underline", "link", "textStyle"]) {
  if (markNames.includes(banned) && !headingMarkSet.includes(banned)) {
    emit(`invalid-mark-not-allowed-in-heading-${banned}`, {
      type: "noteDoc",
      content: [node("noteTitle", text("T")), node("heading", text("H", [mark(banned)]))],
    });
  }
}
// marks allowed on paragraph children must pass inside a heading's sibling
emit("valid-same-mark-in-paragraph", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("paragraph", text("a", [mark(pick(["underline", "strike", "textStyle"]))])),
  ],
}, "valid");

// ── the parent-mark-set rule needs REAL differential coverage: a parent
// whose mark set is restrictive (hljsCodeBlockRow allows only hljsMark).
// A patch removing the child-marks check must flip these to mismatch.
const rowMarks: string[] = artifact.nodes.hljsCodeBlockRow.marks;
if (!rowMarks.includes("bold")) {
  emit("invalid-mark-not-allowed-in-code-row-bold", {
    type: "noteDoc",
    content: [
      node("noteTitle", text("T")),
      node("hljsCodeBlock", node("hljsCodeBlockRow", text("code", [mark("bold")]))),
    ],
  });
}
if (rowMarks.includes("hljsMark")) {
  emit("valid-mark-allowed-in-code-row-hljsmark", {
    type: "noteDoc",
    content: [
      node("noteTitle", text("T")),
      node("hljsCodeBlock", node("hljsCodeBlockRow", text("code", [mark("hljsMark", { class: "l" })]))),
    ],
  }, "valid");
}
// nested: the ROW's mark set judges marks on its DIRECT children (the
// text node), while the inner paragraph judges its own children
if (markNames.includes("italic")) {
  emit("invalid-nested-mark-not-allowed-in-code-row-italic", {
    type: "noteDoc",
    content: [
      node("noteTitle", text("T")),
      node("hljsCodeBlock", node("hljsCodeBlockRow", text("code", [mark("italic")]))),
    ],
  });
}

// ── 8. invalid: shapes the reference library rejects ─────────────────────
emit("invalid-content-not-array", {
  type: "noteDoc",
  content: { type: "paragraph" },
});
emit("invalid-child-not-object", {
  type: "noteDoc",
  content: ["строка вместо узла"],
});
emit("invalid-marks-not-array", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), { type: "paragraph", marks: "bold", content: [] }],
});
emit("invalid-mark-entry-not-object", {
  type: "noteDoc",
  content: [node("noteTitle", text("T")), { type: "paragraph", marks: ["bold"], content: [] }],
});
emit("invalid-node-type-not-string", {
  type: "noteDoc",
  content: [{ type: 42, content: [] }],
});
emit("invalid-node-missing-type", { type: "noteDoc", content: [{ content: [] }] });

// ── 9. valid: nested compositions (lists, tables, quotes) ────────────────
emit("valid-deep-nesting", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("bulletList",
      node("listItem",
        node("paragraph", text("уровень 1")),
        node("bulletList",
          node("listItem",
            node("paragraph", text("уровень 2")),
            node("taskList", node("taskItem", node("paragraph", text("уровень 3")))))))),
    node("blockquote",
      node("paragraph", text("цитата")),
      node("bulletList", node("listItem", node("paragraph", text("список в цитате"))))),
    node("table",
      node("tableRow",
        node("tableCell",
          node("paragraph", text("в ячейке")),
          node("orderedList", node("listItem", node("paragraph", text("и список")))))),
      node("tableRow",
        node("tableHeader", node("paragraph", text("шапка"))))),
    node("hljsCodeBlock",
      node("hljsCodeBlockRow", text("row 1;", [mark("hljsMark", { class: "language-js" })])),
      node("hljsCodeBlockRow", text("row 2;"))),
  ],
}, "valid");
emit("valid-note-properties-page", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("noteProperty", node("notePropertyName", text("description")), node("notePropertyValue", text("v1"))),
    node("noteProperty", node("notePropertyName", text("tags")), node("notePropertyValue", text("a")), node("notePropertyValue", text("b"))),
    node("paragraph", text("текст")),
  ],
}, "valid");
emit("valid-image-in-table-cell", {
  type: "noteDoc",
  content: [
    node("noteTitle", text("T")),
    node("table", node("tableRow", node("tableCell", withAttrs("image", { src: "assets/i.png" })))),
  ],
}, "valid");
emit("valid-title-plus-block-required", {
  type: "noteDoc",
  content: [node("noteTitle", text("Только заголовок")), node("paragraph", text("и блок"))],
}, "valid");

// coverage guards (P-02): the differential corpus must actually exercise
// the parent-mark-set rule — a silent dead emit shows up here
const markSetNegatives = cases.filter((c) =>
  c.category.startsWith("invalid-mark-not-allowed-in-code-row"),
);
if (markSetNegatives.length === 0 || markSetNegatives.some((c) => c.expected !== "invalid")) {
  throw new Error(
    "the parent-mark-set rule has no negative differential coverage — check the hljsCodeBlockRow mark inventory of the artifact",
  );
}

// ── report ───────────────────────────────────────────────────────────────
const validCount = cases.filter((c) => c.expected === "valid").length;
const invalidCount = cases.length - validCount;
const categories = new Set(cases.map((c) => c.category));
console.log(
  `corpus: ${cases.length} cases (${validCount} valid / ${invalidCount} invalid), ${categories.size} categories`,
);

// ── provenance (§4.8): the corpus records WHICH schema snapshot and WHICH
// reference library produced the verdicts — the gate refuses to replay a
// corpus whose provenance does not match the artifact it built
const pmPackage = JSON.parse(
  readFileSync(join(here, "node_modules", "prosemirror-model", "package.json"), "utf8"),
) as { version: string };
const provenance = {
  record: "provenance",
  artifact: "schema/inscriptum-schema.json",
  schema_hash: artifact.schema_hash,
  plugin_version: artifact.plugin_version ?? null,
  prosemirror_model: pmPackage.version,
  seed: 20261109,
};

const outDir = join(here, "..", "fixtures");
mkdirSync(outDir, { recursive: true });
const lines = [
  JSON.stringify(provenance),
  ...cases.map((c) => JSON.stringify({ id: c.id, category: c.category, doc: c.doc, expected: c.expected })),
];
writeFileSync(join(outDir, "corpus.jsonl"), lines.join("\n") + "\n");
console.log(`written: ${join(outDir, "corpus.jsonl")}`);
