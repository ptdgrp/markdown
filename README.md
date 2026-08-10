# PTDGRP Markdown

PTDGRP Markdown is a high-performance, AST-first UTF-8 Markdown parser written in Rust.
It parses CommonMark, GitHub Flavored Markdown (GFM), and Obsidian Flavored
Markdown (OFM) into a structured tree with source spans. Rust, browser WASM, and
Node.js packages are provided for building editors, renderers, indexes, and other
Markdown-aware tools.

HTML rendering is included for testing and integration work, but the primary
output is the AST.

## Features

- CommonMark block and inline syntax
- GFM tables, strikethrough, task lists, and extended autolinks
- OFM wikilinks, embeds, callouts, block IDs, tags, math, and comments
- Frontmatter parsing
- Source byte spans and on-demand line/column lookup
- Borrowed or owned source documents
- Block-only and selective inline parsing
- Optional CJK spacing, punctuation, and delimiter handling
- Browser and Node.js WASM packages

## Installation

```toml
[dependencies]
ptdgrp-markdown = "1.1"
```

The Rust crate is imported with an underscore:

```rust
use ptdgrp_markdown::{Parser, ParserOptions};
```

## Quick Start

```rust
use ptdgrp_markdown::{MarkdownNode, Parser, ParserOptions};

let document = Parser::new_with_options(
    "# Hello\n\nThis is **Markdown**.",
    ParserOptions::default().enabled_gfm(),
)
.parse()?;

assert!(matches!(document.tree[0].body, MarkdownNode::Document));
println!("nodes: {}", document.tree.len());
println!("{}", document.to_html());

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

`Document` retains the source text because some AST text values are represented
as zero-copy source spans. Use `Document::text` to resolve a `TextRef`:

```rust
use ptdgrp_markdown::{MarkdownNode, Parser};

let document = Parser::new("hello").parse()?;
let paragraph = document.tree.get_first_child(0).unwrap();
let text_node = document.tree.get_first_child(paragraph).unwrap();

if let MarkdownNode::Text(text) = &document.tree[text_node].body {
    assert_eq!(document.text(text), "hello");
}

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

Node ID `0` is the document root. Navigate the arena-backed tree with
`get_first_child`, `get_next`, `get_parent`, and the related `Tree` methods.

## Parser Options

Options use a builder-style API and can be combined:

```rust
use ptdgrp_markdown::{Parser, ParserOptions};

let options = ParserOptions::default()
    .enabled_gfm()
    .enabled_ofm()
    .enabled_smart_punctuation()
    .enabled_cjk_autocorrect();

let document = Parser::new_with_options("# Example", options).parse()?;

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

The main option groups are:

- `enabled_gfm`: tables, strikethrough, and task lists
- `enabled_gfm_autolink`: GFM extended autolinks
- `enabled_ofm`: Obsidian-specific syntax
- `enabled_jsx_like_component`: JSX-like components
- `enabled_smart_punctuation`: typographic quotes, dashes, and ellipses
- `enabled_cjk_autocorrect`: spacing between CJK and ASCII text
- `enabled_normalize_chinese_punctuation`: Chinese punctuation normalization
- `enabled_cjk_friendly_delimiters`: delimiter behavior for CJK punctuation
- `enabled_all`: all parser extensions

## Owned Input

`Parser::new` borrows its input. Use `Parser::parse_string` when the returned
document must own the source:

```rust
use ptdgrp_markdown::{Parser, ParserOptions};

let document = Parser::parse_string(
    String::from("# Owned input"),
    ParserOptions::default(),
)?;

assert_eq!(document.source(), "# Owned input");

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

## Resource Limits

Set explicit limits when parsing untrusted input or exposing the parser through
WASM, NAPI, or a server API:

```rust
use ptdgrp_markdown::{ParseError, Parser, ParserOptions};

let input = "# Limited";
let result = Parser::new_with_options(
    input,
    ParserOptions::default()
        .with_max_input_bytes(128 * 1024 * 1024)
        .with_max_nodes(5_000_000),
)
.parse();

match result {
    Ok(document) => println!("{} nodes", document.tree.len()),
    Err(ParseError::InputTooLarge { limit, actual }) => {
        eprintln!("input is {actual} bytes; limit is {limit}");
    }
    Err(ParseError::NodeLimitExceeded { limit, actual }) => {
        eprintln!("AST has {actual} nodes; limit is {limit}");
    }
    Err(error) => eprintln!("parse failed: {error:?}"),
}
```

## Block-Only Parsing

Use `parse_blocks` to inspect block structure before materializing inline nodes:

```rust
use ptdgrp_markdown::{MarkdownNode, Parser};

let blocks = Parser::new("# Heading\n\nParagraph").parse_blocks()?;
let first = blocks.tree().get_first_child(0).unwrap();
assert!(matches!(blocks.tree()[first].body, MarkdownNode::Heading(_)));

let document = blocks.materialize_all()?;
assert!(document.tree.len() > 2);

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

For heading and OFM block-ID discovery, call `BlockDocument::prepare_semantics`
before deciding which inline subtrees to materialize. See the crate-level Rustdoc
and the `selective` module for the advanced selective parsing API.

`Parser::parse_blocks_with` visits finalized top-level blocks. Each
`TopLevelBlockEvent` exposes `start_line()` and `end_line()` as one-based source
line numbers corresponding to the block node's half-open byte span. The line
range is computed lazily on first access and then cached for that event.
`TopLevelBlockEvent::descendant_blocks()` traverses the finalized nested block
subtree in pre-order without dispatching additional events.

## Parsing a Block by Block ID

Semantic preparation discovers OFM block IDs without materializing every inline
subtree. Visit the targets, select the matching block, and then parse only its
inline content:

```rust
use ptdgrp_markdown::{InlineSelection, Parser, ParserOptions, VisitControl};

let source = "Ignore *this*.\n\nParse **this**. ^chosen\n\nSkip `this`.";
let mut phase = Parser::new_with_options(
    source,
    ParserOptions::default().enabled_ofm(),
)
.parse_blocks()?
.prepare_semantics()?;

let mut selection = InlineSelection::default();
let mut chosen = None;
phase.visit_semantic_targets(
    |target| target.block_id() == Some("chosen"),
    &mut selection,
    |target, selection| {
        chosen = Some(target.node_id());
        selection.select(target.node_id());
        VisitControl::Stop
    },
);

let chosen = chosen.expect("block ID not found");
let output = phase.parse_selected_inlines(selection)?;

assert!(output.document.tree.get_first_child(chosen).is_some());

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

Selecting a container selects all inline-capable descendants in that container.
Required footnote definitions are also materialized automatically.

## Parsing a Section by Heading

A Markdown section starts at a heading and ends before the next heading whose
level is equal to or higher than the starting heading. Selecting a heading node
alone parses only the heading itself, so select its following sibling blocks up
to that boundary:

```rust
use ptdgrp_markdown::{
    InlineSelection, MarkdownNode, Parser, VisitControl,
};

let source = r#"# Introduction

Ignore this.

## Installation

Run **cargo install**.

### Linux

Use your package manager.

## API

Do not parse this section.
"#;

let mut phase = Parser::new(source)
    .parse_blocks()?
    .prepare_semantics()?;
let mut selection = InlineSelection::default();
let mut installation_body = None;
let mut api_heading = None;

phase.visit_semantic_targets(
    |target| target.heading().is_some(),
    &mut selection,
    |target, selection| {
        // ref_text() parses only this heading's inline text for matching.
        if target.ref_text() != "Installation" {
            return VisitControl::Continue;
        }

        let heading_id = target.node_id();
        let level = *target.heading().unwrap().level() as u8;
        selection.select(heading_id);

        let tree = target.tree();
        let mut node = tree.get_next(heading_id);
        installation_body = node;
        while let Some(id) = node {
            if let MarkdownNode::Heading(heading) = &tree[id].body {
                // h1(level 1) < h2(lvel 2)
                if *heading.level() as u8 <= level {
                    api_heading = Some(id);
                    break;
                }
            }
            selection.select(id);
            node = tree.get_next(id);
        }
        VisitControl::Stop
    },
);

let output = phase.parse_selected_inlines(selection)?;
let document = output.document;

assert!(document
    .tree
    .get_first_child(installation_body.unwrap())
    .is_some());
assert!(document.tree.get_first_child(api_heading.unwrap()).is_none());

# Ok::<(), ptdgrp_markdown::ParseError>(())
```

The complete block structure remains available in both cases. Only the selected
range receives inline AST nodes. `SemanticTarget::ref_text` lazily materializes
a heading's own inline text when matching by title.

## Frontmatter

The `frontmatter` feature is enabled by default. A frontmatter section at the
start of a document becomes a `MarkdownNode::FrontMatter` child of the document
root. HTML rendering omits the frontmatter node.

## WASM

### Browser and Bundlers

```bash
npm install @ptdgrp/markdown-wasm
```

```ts
import { parseWithOptions } from "@ptdgrp/markdown-wasm";

const document = parseWithOptions("# Hello", {
  github_flavored: true,
  obsidian_flavored: true,
});

console.log(document.tree);
console.log(document.tags); // Unsorted string[]
console.log(document.frontmatter);
console.log(document.toHtml());
document.dispose();
```

### Node.js

```bash
npm install @ptdgrp/markdown-wasm-node
```

```ts
import { parse } from "@ptdgrp/markdown-wasm-node";

const document = parse("This is $e^{i\\pi}+1=0$");
console.log(document.totalNodes);
console.log(document.toHtml());
document.dispose();
```

The WASM API also supports a `frontmatter_only` parse mode followed by
`continueParse()` for deferred body parsing.

## Performance

Representative CommonMark results from the repository's Criterion comparison
benchmark are shown below. Times are machine-dependent and should only be
compared within the same run.

| Scenario | PTDGRP Markdown | pulldown-cmark | rushdown | comrak |
| --- | ---: | ---: | ---: | ---: |
| Parse, curated (259 KB) | 1.364 ms | 0.803 ms | 1.999 ms | 2.268 ms |
| Parse, corpus (570 KB) | 3.943 ms | 2.717 ms | 5.799 ms | 5.610 ms |
| Parse + HTML, curated | 1.921 ms | 0.965 ms | 2.249 ms | 2.962 ms |
| Parse + HTML, corpus | 4.903 ms | 2.872 ms | 6.834 ms | 7.108 ms |

The parse-only workloads are not identical: PTDGRP Markdown, `rushdown`, and
`comrak` build full ASTs, while `pulldown-cmark` consumes an event stream. The
HTML rows measure each implementation's end-to-end HTML path. See
[`bench/compare/README.md`](bench/compare/README.md) for datasets, commands, and
comparison rules.

## Cargo Features

- `html` (default): enables `Document::to_html`
- `frontmatter` (default): parses leading YAML-like frontmatter

Disable default features when only the AST core is required:

```toml
[dependencies]
ptdgrp-markdown = { version = "1.1", default-features = false }
```

## API Documentation

Build and open the Rust API documentation locally:

```bash
cargo doc --open
```

## Development

```bash
cargo check --workspace
cargo test --workspace
cargo bench -p parser-compare-bench --bench parser_compare
```

## License

MIT
