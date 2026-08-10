//! Block-only and selective inline parsing.
//!
//! Use [`Parser::parse_blocks`](crate::Parser::parse_blocks) to inspect a
//! complete block tree without paying the cost of inline materialization. A
//! [`BlockDocument`] can then be completed normally or prepared for semantic
//! target discovery and selective inline parsing.
//!
//! # Select by Block ID
//!
//! ```
//! use ptdgrp_markdown::{InlineSelection, Parser, ParserOptions, VisitControl};
//!
//! let mut phase = Parser::new_with_options(
//!     "Skip.\n\nParse **this**. ^chosen",
//!     ParserOptions::default().enabled_ofm(),
//! )
//! .parse_blocks()?
//! .prepare_semantics()?;
//! let mut selection = InlineSelection::default();
//! let mut chosen = None;
//!
//! phase.visit_semantic_targets(
//!     |target| target.block_id() == Some("chosen"),
//!     &mut selection,
//!     |target, selection| {
//!         chosen = Some(target.node_id());
//!         selection.select(target.node_id());
//!         VisitControl::Stop
//!     },
//! );
//!
//! let output = phase.parse_selected_inlines(selection)?;
//! assert!(output
//!     .document
//!     .tree
//!     .get_first_child(chosen.expect("block ID not found"))
//!     .is_some());
//! # Ok::<(), ptdgrp_markdown::ParseError>(())
//! ```
//!
//! # Select a heading section
//!
//! A section extends from its heading to, but not including, the next sibling
//! heading of the same or a higher level. [`InlineSelection::select`] does not
//! infer this range automatically, so expand it while the block tree is
//! available:
//!
//! ```
//! use ptdgrp_markdown::{InlineSelection, MarkdownNode, Parser, VisitControl};
//!
//! let source = "# Intro\n\nSkip.\n\n## Install\n\nRun **cargo**.\n\n### Linux\n\nUse apt.\n\n## API\n\nSkip.";
//! let mut phase = Parser::new(source)
//!     .parse_blocks()?
//!     .prepare_semantics()?;
//! let mut selection = InlineSelection::default();
//! let mut selected_body = None;
//! let mut boundary = None;
//!
//! phase.visit_semantic_targets(
//!     |target| target.heading().is_some(),
//!     &mut selection,
//!     |target, selection| {
//!         if target.ref_text() != "Install" {
//!             return VisitControl::Continue;
//!         }
//!
//!         let heading_id = target.node_id();
//!         let level = *target.heading().unwrap().level() as u8;
//!         selection.select(heading_id);
//!
//!         let tree = target.tree();
//!         let mut node = tree.get_next(heading_id);
//!         selected_body = node;
//!         while let Some(id) = node {
//!             if let MarkdownNode::Heading(heading) = &tree[id].body {
//!                 if *heading.level() as u8 <= level {
//!                     boundary = Some(id);
//!                     break;
//!                 }
//!             }
//!             selection.select(id);
//!             node = tree.get_next(id);
//!         }
//!         VisitControl::Stop
//!     },
//! );
//!
//! let output = phase.parse_selected_inlines(selection)?;
//! assert!(output
//!     .document
//!     .tree
//!     .get_first_child(selected_body.unwrap())
//!     .is_some());
//! assert!(output
//!     .document
//!     .tree
//!     .get_first_child(boundary.unwrap())
//!     .is_none());
//! # Ok::<(), ptdgrp_markdown::ParseError>(())
//! ```

use crate::ast::MarkdownNode;
use crate::ast::heading::Heading;
use crate::document::Document;
use crate::location::Location;
use crate::node::Node;
use crate::parser::{ParseError, Parser};
use crate::tree::Tree;
use rustc_hash::FxHashSet;
use std::cell::Cell;
use std::sync::OnceLock;

fn source_line_range(node: &Node, source: &str, line_starts: &OnceLock<Vec<u32>>) -> (u64, u64) {
    let starts = line_starts.get_or_init(|| Location::build_line_starts(source));
    let line_at = |offset: u32| starts.partition_point(|&start| start <= offset) as u64;
    (line_at(node.span.start), line_at(node.span.end))
}

/// Controls whether a visitor continues or stops.
///
/// Stopping a block scan is terminal; the discarded suffix cannot be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitControl {
    /// Continue visiting events.
    Continue,
    /// Stop visiting events.
    Stop,
}

/// How a block scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockScanStatus {
    /// The parser reached the end of the source.
    Complete,
    /// A visitor stopped the parser before the end of the source.
    Stopped,
}

/// A finalized top-level block exposed during a block visitor callback.
///
/// The event is read-only and valid only for the callback. Its node ID should
/// not be persisted because later semantic preparation may remove structural
/// nodes such as reference-definition-only paragraphs.
pub struct TopLevelBlockEvent<'event> {
    pub(crate) node_id: usize,
    pub(crate) tree: &'event Tree<Node>,
    pub(crate) source: &'event str,
    pub(crate) line_starts: &'event OnceLock<Vec<u32>>,
    pub(crate) lines: Cell<Option<(u64, u64)>>,
}

impl<'event> TopLevelBlockEvent<'event> {
    /// Returns the event node ID for use during the callback.
    pub fn node_id(&self) -> usize {
        self.node_id
    }
    /// Returns the finalized block node.
    pub fn node(&self) -> &'event Node {
        &self.tree[self.node_id]
    }
    /// Returns the current block tree view.
    pub fn tree(&self) -> &'event Tree<Node> {
        self.tree
    }
    /// Returns the one-based line containing the block's inclusive start offset.
    ///
    /// The event computes and caches its line range on the first line lookup.
    pub fn start_line(&self) -> u64 {
        self.lines().0
    }
    /// Returns the one-based line containing the block's exclusive end offset.
    ///
    /// This is the line component of `node().span.end`, matching
    /// [`Document::location_at`]. The event computes and caches its line range
    /// on the first line lookup.
    pub fn end_line(&self) -> u64 {
        self.lines().1
    }

    /// Iterates over all nested block nodes in pre-order, excluding this block.
    ///
    /// The returned blocks are part of the already-finalized subtree. Iterating
    /// them does not dispatch additional events and has no effect on
    /// [`VisitControl`].
    pub fn descendant_blocks(&self) -> DescendantBlocks<'event> {
        DescendantBlocks {
            tree: self.tree,
            source: self.source,
            line_starts: self.line_starts,
            root_id: self.node_id,
            next_id: self.tree.get_first_child(self.node_id),
        }
    }

    fn lines(&self) -> (u64, u64) {
        if let Some(lines) = self.lines.get() {
            return lines;
        }

        let lines = source_line_range(self.node(), self.source, self.line_starts);
        self.lines.set(Some(lines));
        lines
    }
}

/// A read-only block in a finalized top-level block subtree.
pub struct BlockRef<'event> {
    node_id: usize,
    tree: &'event Tree<Node>,
    source: &'event str,
    line_starts: &'event OnceLock<Vec<u32>>,
    lines: Cell<Option<(u64, u64)>>,
}

impl<'event> BlockRef<'event> {
    /// Returns the block node ID for use during the callback.
    pub fn node_id(&self) -> usize {
        self.node_id
    }

    /// Returns the finalized block node.
    pub fn node(&self) -> &'event Node {
        &self.tree[self.node_id]
    }

    /// Returns the one-based line containing the block's inclusive start offset.
    pub fn start_line(&self) -> u64 {
        self.lines().0
    }

    /// Returns the one-based line containing the block's exclusive end offset.
    pub fn end_line(&self) -> u64 {
        self.lines().1
    }

    fn lines(&self) -> (u64, u64) {
        if let Some(lines) = self.lines.get() {
            return lines;
        }

        let lines = source_line_range(self.node(), self.source, self.line_starts);
        self.lines.set(Some(lines));
        lines
    }
}

/// Pre-order iterator over the nested blocks beneath a top-level block.
pub struct DescendantBlocks<'event> {
    tree: &'event Tree<Node>,
    source: &'event str,
    line_starts: &'event OnceLock<Vec<u32>>,
    root_id: usize,
    next_id: Option<usize>,
}

impl<'event> Iterator for DescendantBlocks<'event> {
    type Item = BlockRef<'event>;

    fn next(&mut self) -> Option<Self::Item> {
        let node_id = self.next_id?;
        debug_assert!(self.tree[node_id].body.is_block_level());

        self.next_id = if let Some(child) = self.tree.get_first_child(node_id) {
            Some(child)
        } else {
            let mut current = node_id;
            loop {
                if let Some(sibling) = self.tree.get_next(current) {
                    break Some(sibling);
                }
                let parent = self.tree.get_parent(current);
                if parent == self.root_id {
                    break None;
                }
                current = parent;
            }
        };

        Some(BlockRef {
            node_id,
            tree: self.tree,
            source: self.source,
            line_starts: self.line_starts,
            lines: Cell::new(None),
        })
    }
}

/// A document whose block scan is complete but whose inline nodes are pending.
///
/// Its tree can be inspected immediately. Consume it with
/// [`BlockDocument::materialize_all`] for a full document or
/// [`BlockDocument::prepare_semantics`] for selective parsing.
pub struct BlockDocument<'input> {
    pub(crate) parser: Parser<'input>,
    pub(crate) status: BlockScanStatus,
}

impl<'input> BlockDocument<'input> {
    /// Returns the original Markdown source.
    pub fn source(&self) -> &'input str {
        self.parser.scanner.source_str()
    }

    /// Returns the complete block tree.
    ///
    /// Inline-capable nodes may not have inline children yet.
    pub fn tree(&self) -> &Tree<Node> {
        &self.parser.tree
    }

    /// Returns how the block scan ended.
    pub fn block_status(&self) -> BlockScanStatus {
        self.status
    }

    /// Materializes every pending inline subtree and returns a full [`Document`].
    pub fn materialize_all(self) -> Result<Document<'input>, ParseError> {
        self.parser.finish_inline_phase()
    }

    /// Discovers headings and OFM block IDs for selective processing.
    pub fn prepare_semantics(mut self) -> Result<SemanticPhase<'input>, ParseError> {
        self.parser.discover_block_ids();
        if let Some(err) = self.parser.parse_error.take() {
            self.parser.tree.pop();
            return Err(err);
        }
        let targets = self.parser.collect_semantic_targets();
        Ok(SemanticPhase {
            parser: self.parser,
            status: self.status,
            targets,
        })
    }

    /// Alias for [`BlockDocument::materialize_all`].
    pub fn finish(self) -> Result<Document<'input>, ParseError> {
        self.materialize_all()
    }
    /// Alias for [`BlockDocument::prepare_semantics`].
    pub fn prepare_semantic_targets(self) -> Result<SemanticPhase<'input>, ParseError> {
        self.prepare_semantics()
    }
}

/// A heading or a node carrying an OFM block ID.
///
/// Structural fields can be inspected without inline parsing. Calling
/// [`SemanticTarget::ref_text`] lazily materializes the target's inline subtree.
pub struct SemanticTarget<'event, 'input> {
    pub(crate) node_id: usize,
    pub(crate) parser: &'event mut Parser<'input>,
}

impl<'event, 'input> SemanticTarget<'event, 'input> {
    /// Returns the target node ID.
    pub fn node_id(&self) -> usize {
        self.node_id
    }
    /// Returns the target node.
    pub fn node(&self) -> &Node {
        &self.parser.tree[self.node_id]
    }
    /// Returns the current tree view.
    pub fn tree(&self) -> &Tree<Node> {
        &self.parser.tree
    }
    /// Returns the heading payload when this target is a heading.
    pub fn heading(&self) -> Option<&Heading> {
        match &self.parser.tree[self.node_id].body {
            MarkdownNode::Heading(heading) => Some(heading),
            _ => None,
        }
    }
    /// Returns the OFM block ID, if present.
    pub fn block_id(&self) -> Option<&str> {
        self.parser.tree[self.node_id]
            .id
            .as_ref()
            .map(|id| id.as_str())
    }
    /// Returns the plain-text projection used for Obsidian-style reference matching.
    ///
    /// Formatting markers and a trailing block ID are omitted. Escapes and
    /// entities are decoded by the normal inline parser. The first call lazily
    /// materializes this target's inline subtree.
    pub fn ref_text(&mut self) -> String {
        if self.parser.inlines.contains(self.node_id) {
            // 任何物化前确保引用定义已提取（幂等；纯结构查询会话零支付）
            self.parser.prepare_reference_definitions();
            let mut ids = vec![self.node_id];
            self.parser.materialize_pending_subset(&mut ids);
        }
        let mut out = String::new();
        let source = self.parser.scanner.source_str();
        let mut stack: Vec<usize> = Vec::new();
        let mut node = self.parser.tree.get_first_child(self.node_id);
        while let Some(id) = node {
            if let MarkdownNode::Text(text) = &self.parser.tree[id].body {
                out.push_str(text.resolve(source));
            }
            if let Some(child) = self.parser.tree.get_first_child(id) {
                stack.push(id);
                node = Some(child);
            } else {
                node = self.parser.tree.get_next(id);
                while node.is_none() {
                    match stack.pop() {
                        Some(parent) => node = self.parser.tree.get_next(parent),
                        None => break,
                    }
                }
            }
        }
        out
    }
}

/// A deduplicated set of node IDs whose inline content should be materialized.
///
/// Selecting a container also selects its inline-capable descendants.
#[derive(Default)]
pub struct InlineSelection {
    pub(crate) selected: FxHashSet<usize>,
}

impl InlineSelection {
    /// Adds a node ID to the selection.
    pub fn select(&mut self, node_id: usize) {
        self.selected.insert(node_id);
    }
    /// Returns `true` when no nodes have been selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
}

/// Parser state after semantic targets have been indexed.
pub struct SemanticPhase<'input> {
    pub(crate) parser: Parser<'input>,
    pub(crate) status: BlockScanStatus,
    pub(crate) targets: Vec<usize>,
}

impl<'input> SemanticPhase<'input> {
    /// Returns how the preceding block scan ended.
    pub fn block_status(&self) -> BlockScanStatus {
        self.status
    }
    /// Returns the number of semantic targets in document order.
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }
    /// Visits semantic targets in document order.
    ///
    /// Rejected targets are skipped. [`VisitControl::Stop`] ends this visit but
    /// does not truncate the tree or modify `selection` implicitly.
    pub fn visit_semantic_targets<F, V>(
        &mut self,
        mut filter: F,
        selection: &mut InlineSelection,
        mut visitor: V,
    ) where
        F: FnMut(&SemanticTarget<'_, 'input>) -> bool,
        V: FnMut(&mut SemanticTarget<'_, 'input>, &mut InlineSelection) -> VisitControl,
    {
        let targets = std::mem::take(&mut self.targets);
        for &node_id in &targets {
            let mut target = SemanticTarget {
                node_id,
                parser: &mut self.parser,
            };
            if !filter(&target) {
                continue;
            }
            match visitor(&mut target, selection) {
                VisitControl::Continue => continue,
                VisitControl::Stop => break,
            }
        }
        self.targets = targets;
    }
    /// Materializes all remaining inline content and returns a full document.
    pub fn finish(self) -> Result<Document<'input>, ParseError> {
        self.parser.finish_inline_phase()
    }
    /// Materializes inline content only for the selected nodes.
    ///
    /// Required descendants and referenced footnote definitions are included.
    /// Unselected nodes retain their block structure without inline children.
    pub fn parse_selected_inlines(
        mut self,
        selection: InlineSelection,
    ) -> Result<SelectiveParseOutput<'input>, ParseError> {
        // 惰性引用定义（C3）：任何物化开始前确保已提取（幂等）
        self.parser.prepare_reference_definitions();
        // 无效/未知节点 ID 在任何物化开始前拒绝
        for &node_id in &selection.selected {
            if !self.parser.tree.node_exists(node_id) {
                self.parser.tree.pop();
                return Err(ParseError::InvalidSelectionNode { node_id });
            }
        }
        // 展开：所选节点 + 子树内全部 pending 后代；祖先/后代重复选择天然去重
        let mut expanded: Vec<usize> = Vec::new();
        for &node_id in &selection.selected {
            self.parser
                .collect_pending_in_subtree(node_id, &mut expanded);
        }
        expanded.sort_unstable();
        expanded.dedup();
        self.parser.materialize_pending_subset(&mut expanded);
        // 选中内容（含语义准备阶段的 Heading）引用的 footnote definition 是必要依赖
        self.parser.materialize_footnote_dependencies();
        if let Some(err) = self.parser.parse_error.take() {
            self.parser.tree.pop();
            return Err(err);
        }
        self.parser.parse_footnote_list();
        if let Some(err) = self.parser.parse_error.take() {
            self.parser.tree.pop();
            return Err(err);
        }
        self.parser.tree.pop();
        let document = self.parser.into_ast();
        Ok(SelectiveParseOutput {
            document,
            block_status: self.status,
        })
    }
}

/// The result of selective inline parsing.
///
/// The document contains a complete block tree, but unselected nodes do not
/// contain inline children.
pub struct SelectiveParseOutput<'source> {
    /// Partially materialized document.
    pub document: Document<'source>,
    /// Status of the block scan that produced the document.
    pub block_status: BlockScanStatus,
}

impl<'input> Parser<'input> {
    /// Parses the complete block structure without materializing inline nodes.
    pub fn parse_blocks(self) -> Result<BlockDocument<'input>, ParseError> {
        self.run_block_phase(None)
    }

    /// Parses blocks while visiting each finalized direct child of the document.
    ///
    /// Rejected events are skipped. Returning [`VisitControl::Stop`] stops source
    /// consumption permanently and returns only the accepted prefix. Frontmatter
    /// and the document root do not produce events.
    pub fn parse_blocks_with<F, V>(
        self,
        mut filter: F,
        mut visitor: V,
    ) -> Result<BlockDocument<'input>, ParseError>
    where
        F: FnMut(&TopLevelBlockEvent<'_>) -> bool,
        V: FnMut(&TopLevelBlockEvent<'_>) -> VisitControl,
    {
        let mut combined = |event: &TopLevelBlockEvent<'_>| -> VisitControl {
            if filter(event) {
                visitor(event)
            } else {
                VisitControl::Continue
            }
        };
        self.run_block_phase(Some(&mut combined))
    }
}
