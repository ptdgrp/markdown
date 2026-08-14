# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

empty

## [1.1.4] - 2026-08-11

### Added
- Added lazy, one-based source line ranges to top-level block events.
- Added pre-order traversal of finalized descendant blocks through `TopLevelBlockEvent::descendant_blocks()`.

## [1.1.3] - 2026-08-06

### Added
- Added block-level parsing and rendering for fenced `$$` display math while preserving inline `$$...$$` text.

### Fixed
- Rejected unknown WebAssembly parser option fields instead of silently ignoring them.
- Propagated WebAssembly serialization failures as JavaScript errors instead of discarding them.

## [1.1.2] - 2026-08-04

### Changed
- Refined package and generated-artifact ignore rules for Rust and WebAssembly releases.

## [1.1.1] - 2026-08-04

### Changed
- Added crates.io package metadata and tightened the published crate contents.
- Migrated the playground and deployment workflow from npm to pnpm and refreshed the frontend toolchain.
- Updated the WebAssembly publishing workflow to Rust 1.96 and `wasm-pack` 0.15.0.

### Fixed
- Removed the obsolete top-level-await Vite plugin from the playground configuration.

## [1.1.0] - 2026-08-04

### Added
- Added `Parser::parse_blocks()` and the inspectable `BlockDocument` result for parsing Block structure without eagerly materializing Inline nodes.
- Added a design specification for selective inline parsing and Rust-only block/semantic events, including filtered traversal, early-stop behavior, and inline selection semantics.
- Added a Node.js benchmark that compares the WASM binding with mainstream JS/TS Markdown libraries.
- Added a TypeScript package facade that exposes lazy `Document.tree`, `toHtml()`, and direct heading/link queries while keeping the compact WASM AST transport private.

### Changed
- Extended the allocation-count benchmark to report minimum, midpoint, and maximum parse elapsed times alongside allocation metrics.
- Renamed the public Block-stage result from `BlockPhase` to `BlockDocument`.

## [1.0.5] - 2026-06-10

### Added
- Added benchmark data and comparison tooling for parser performance analysis.
- Added support for Obsidian blockquote/callout lazy continuations and inline footnotes.

### Changed
- Optimized parser hot paths and updated the playground and profiling configuration.

### Fixed
- Added support for external image sizes.

## [1.0.4] - 2026-06-10

### Changed
- Updated wikilink parsing so `\|` is treated as a separator and trailing `\` characters in paths are handled correctly.
- Updated the playground dependency and release-related configuration.

## [1.0.3] - 2026-02-18

### Changed
- Expanded inline emoji-name parsing to accept multilingual Unicode letters/digits (non-symbol text), while keeping `_`, `+`, and `-` compatibility.
- Optimized emoji-name scanning to a linear UTF-8 pass to avoid repeated decoding checks on each step.
- Bumped crate/package versions to `1.0.3` for `markdown` and `markdown-binding`.

## [1.0.2] - 2026-02-18

### Added
- Added a wasm regression test to assert two-phase parsing (`frontmatter_only` + `continue_parse`) matches one-phase full parsing for node count, tags, and HTML output.

### Changed
- Refactored wasm `Document` internals to hold a single `inner: MarkdownDocument` state instead of split `ast` and `tags` fields.
- Bumped crate versions to `1.0.2` for both `markdown` and `markdown-binding`.

### Fixed
- Removed split-state handoff in deferred parsing resume path by moving/restoring the whole document state during `continue_parse`.
- Marked wasm `frontmatter` getter as nullable at the TypeScript boundary (`FrontmatterOrNull`) to match runtime behavior when frontmatter is absent.

## [1.0.1] - 2026-02-17

### Added
- Exposed package version from wasm binding API via `version()` for runtime verification.
- Added two-phase frontmatter parsing flow to improve parser behavior around metadata extraction.
- Improved and expanded TypeScript type annotations/comments in wasm binding output.

### Changed
- Updated release/build docs and examples for current Rust/WASM usage.
- Improved Makefile flow for wasm packaging and profiling commands.

### Fixed
- Cleaned repository metadata artifacts and unrelated files from release history.

## [1.0.0] - 2026-02-16

### Breaking / Major
- Squashed the long feature/fix train into a single release commit for a clean `1.0.0` baseline.
- Bumped crate versions to `1.0.0`:
  - `markdown` (`Cargo.toml`)
  - `markdown-binding` (`wasm-binding/Cargo.toml`)
  - Workspace lockfile metadata (`Cargo.lock`)

### Added
- Full spec-runner setup and grouped suites for CommonMark, GitHub-flavored, and Obsidian-flavored tests.
- Property and regression test suites:
  - `tests/z_property_tests.rs`
  - `tests/z_regression_cases.rs`
- CJK-friendly delimiter and spacing behavior tests (`tests/cjk_friendly/*`).
- Frontmatter YAML parsing support (`src/exts/yaml.rs`) and related parser wiring.
- Additional performance benchmark entries (phase/timing benches).

### Changed
- Large parser/internal refactor:
  - moved from legacy line/tokenizer flow to `scanner` + `span` architecture
  - improved inline/block processing pipeline and reduced allocations in hot paths
- HTML parsing/rendering behavior improved across block/inline/component paths.
- OFM/GFM behavior alignment advanced across lists, footnotes, links, tables, math, and punctuation handling.
- Playground/web-demo structure updated and renamed to `playground`.

### Fixed
- Math parsing and display-math rendering behavior (including multiline `$$...$$` handling and paragraph splitting rules).
- Node location accuracy fixes in inline/span processing paths.
- Multiple spec compliance fixes across headings, lists, breaks, entities, and HTML edge cases.

### Notes
- WebAssembly artifacts remain target-architecture independent at the `.wasm` level (`wasm32-*`), while JS glue target selection still depends on runtime target (`web`/`bundler`/`nodejs`).
