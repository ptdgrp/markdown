//! Parser construction, configuration, errors, and multi-phase parsing.

use crate::ast::MarkdownNode;
use crate::ast::text::TextRef;
use crate::blocks::{BlockMatching, BlockProcessing};
use crate::document::{Document, SourceText};
use crate::exts;
use crate::node::Node;
use crate::scanner::{Scanner, ScannerSnapshot};
use crate::selective::{BlockDocument, BlockScanStatus, TopLevelBlockEvent, VisitControl};
use crate::span::Span;
use crate::tree::Tree;
use crate::{blocks, inlines};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::OnceLock;

/// Opaque state captured after parsing frontmatter.
///
/// A snapshot can be resumed with [`Parser::from_phase_snapshot`] or, for an
/// owned source document, [`Parser::continue_parse_from_snapshot_string`].
pub struct ParserPhaseSnapshot {
    pub(crate) scanner_snapshot: ScannerSnapshot,
    pub(crate) options: ParserOptions,
    pub(crate) text_len: usize,
}

/// Errors that can stop parsing before a document is produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The input exceeded the configured byte limit.
    InputTooLarge {
        /// Configured maximum input size in bytes.
        limit: usize,
        /// Actual input size in bytes.
        actual: usize,
    },
    /// AST construction exceeded the configured node limit.
    NodeLimitExceeded {
        /// Configured maximum number of node slots.
        limit: usize,
        /// Actual number of node slots when parsing stopped.
        actual: usize,
    },
    /// A two-phase snapshot was resumed with source text of a different length.
    SnapshotInputLengthMismatch {
        /// Source length recorded in the snapshot.
        expected: usize,
        /// Length of the source supplied while resuming.
        actual: usize,
    },
    /// A selective inline request contained an unknown or released node ID.
    InvalidSelectionNode {
        /// The invalid node ID.
        node_id: usize,
    },
}

/// Parser configuration.
///
/// Options use a consuming builder style, so they can be chained before being
/// passed to [`Parser::new_with_options`]. The default configuration parses the
/// crate's CommonMark-compatible core syntax with the default Cargo features.
#[derive(Debug, Default, Clone)]
pub struct ParserOptions {
    /// 当 github_flavored 和 obsidian_flavored 未启用时为 `true`
    pub(crate) default_flavored: bool,
    pub(crate) github_flavored: bool,
    pub(crate) gfm_extended_autolink: bool,
    pub(crate) obsidian_flavored: bool,
    pub(crate) jsx_like_component: bool,
    pub(crate) cjk_autocorrect: bool,
    pub(crate) smart_punctuation: bool,
    pub(crate) normalize_chinese_punctuation: bool,
    pub(crate) cjk_friendly_delimiters: bool,
    pub(crate) cjk_nouns: FxHashSet<String>,
    /// 启用从 frontmatter 提取 cjk nouns 并指定字段名称
    pub(crate) cjk_nouns_from_frontmatter: Option<String>,
    pub(crate) max_input_bytes: Option<usize>,
    pub(crate) max_nodes: Option<usize>,
}

impl ParserOptions {
    /// Enables GitHub Flavored Markdown tables, strikethrough, and task lists.
    pub fn enabled_gfm(self) -> Self {
        Self {
            github_flavored: true,
            default_flavored: false,
            ..self
        }
    }
    /// Enables GitHub's extended autolink syntax.
    pub fn enabled_gfm_autolink(self) -> Self {
        Self {
            default_flavored: false,
            github_flavored: true,
            gfm_extended_autolink: true,
            ..self
        }
    }
    /// Enables Obsidian Flavored Markdown extensions.
    ///
    /// This includes syntax such as wikilinks, embeds, callouts, block IDs,
    /// tags, math, and Obsidian comments.
    pub fn enabled_ofm(self) -> Self {
        Self {
            obsidian_flavored: true,
            default_flavored: false,
            ..self
        }
    }
    /// Enables JSX-like component syntax.
    pub fn enabled_jsx_like_component(self) -> Self {
        Self {
            jsx_like_component: true,
            ..self
        }
    }
    /// Inserts spacing between adjacent CJK and ASCII text where appropriate.
    pub fn enabled_cjk_autocorrect(self) -> Self {
        Self {
            cjk_autocorrect: true,
            ..self
        }
    }
    /// Enables typographic punctuation such as smart quotes, dashes, and ellipses.
    pub fn enabled_smart_punctuation(self) -> Self {
        Self {
            smart_punctuation: true,
            ..self
        }
    }
    /// Normalizes punctuation in Chinese text contexts.
    pub fn enabled_normalize_chinese_punctuation(self) -> Self {
        Self {
            normalize_chinese_punctuation: true,
            ..self
        }
    }
    /// Adjusts emphasis delimiter handling around CJK punctuation.
    pub fn enabled_cjk_friendly_delimiters(self) -> Self {
        Self {
            cjk_friendly_delimiters: true,
            ..self
        }
    }
    /// Enables every syntax extension and text-processing option.
    pub fn enabled_all(self) -> Self {
        Self {
            default_flavored: true,
            github_flavored: true,
            gfm_extended_autolink: true,
            obsidian_flavored: true,
            jsx_like_component: true,
            cjk_autocorrect: true,
            smart_punctuation: true,
            normalize_chinese_punctuation: true,
            cjk_friendly_delimiters: true,
            ..self
        }
    }
    /// Rejects inputs larger than `max_input_bytes`.
    pub fn with_max_input_bytes(self, max_input_bytes: usize) -> Self {
        Self {
            max_input_bytes: Some(max_input_bytes),
            ..self
        }
    }
    /// Stops parsing when the AST exceeds `max_nodes` node slots.
    pub fn with_max_nodes(self, max_nodes: usize) -> Self {
        Self {
            max_nodes: Some(max_nodes),
            ..self
        }
    }
    /// Replaces the set of proper nouns excluded from CJK auto-spacing.
    pub fn with_cjk_nouns<I, S>(mut self, nouns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.cjk_nouns.clear();
        self.cjk_nouns.extend(nouns.into_iter().map(Into::into));
        self
    }
    /// Loads additional CJK proper nouns from a frontmatter field.
    ///
    /// The field may contain a string or a list of strings.
    pub fn with_cjk_nouns_from_frontmatter(self, field: impl Into<String>) -> Self {
        Self {
            cjk_nouns_from_frontmatter: Some(field.into()),
            ..self
        }
    }
}

/// A single-use Markdown parser over borrowed source text.
///
/// Construct a parser with [`Parser::new`] or [`Parser::new_with_options`], then
/// consume it with [`Parser::parse`], [`Parser::parse_blocks`], or one of the
/// multi-phase parsing methods.
pub struct Parser<'input> {
    pub(crate) scanner: Scanner<'input>,
    pub(crate) tree: Tree<Node>,
    pub(crate) options: ParserOptions,
    /// 存储在解析 Block 时能接收 inlines 的 block 的 ID 和剩余未处理的 Span
    /// （文档顺序条目 + 稠密索引 + 单/双段内联，见 `crate::pending`）
    pub(crate) inlines: crate::pending::PendingInlines<'input>,
    pub(crate) link_refs: FxHashMap<String, (String, Option<String>)>, // HRefLabel, (Url, Option<Title>)
    pub(crate) footnotes: FxHashMap<String, usize>,                    // label, node_id
    pub(crate) footnote_refs: FxHashMap<String, (usize, usize)>,       // label, index, ref count
    pub(crate) doc: usize,
    /// 应等同于 tree.cur()
    pub(crate) curr_proc_node: usize,
    pub(crate) prev_proc_node: usize,
    pub(crate) last_matched_node: usize,
    pub(crate) last_offset: u32,
    pub(crate) all_closed: bool,
    pub(crate) tags: FxHashSet<String>,
    /// Lazily built source line index, shared with block events and the final document.
    pub(crate) line_starts: OnceLock<Vec<u32>>,
    pub(crate) html_stacks: VecDeque<(String, usize)>, // tag name, node idx
    /// 需要在 inline 结束后执行文本后处理（相邻 Text 合并/校正）的父节点集合
    pub(crate) text_postprocess_parents: FxHashSet<usize>,
    /// 复用的临时容器：reference link 候选节点
    ref_link_candidates_scratch: Vec<usize>,
    /// 全部 FootnoteLink 引用节点 `(node_id, 原始 label)`；最终编号/标签在
    /// `parse_footnote_list` 按源码位置统一确定，与 inline 处理调度无关
    pub(crate) footnote_ref_nodes: Vec<(usize, String)>,
    /// 内联脚注（`^[..]`）创建时的临时自动标签，最终化时按位置序重生成
    pub(crate) inline_footnote_defs: Vec<String>,
    /// 引用定义是否已提取（完整解析与语义准备共用，幂等）
    pub(crate) reference_definitions_extracted: bool,
    /// BlockId 是否已发现（完整解析与语义准备共用，幂等）
    pub(crate) block_ids_discovered: bool,
    /// Heading 块创建即记录（文档序），语义目标增量收集用（v2C C3）
    pub(crate) heading_nodes: Vec<usize>,
    /// `discover_block_ids` 命中的 id 节点（文档序）
    pub(crate) semantic_id_nodes: Vec<usize>,
    /// 顶层（Document 直接子级）节点完成计数：观察者派发的触发信号（v2C C5）
    pub(crate) top_level_finalized: u32,
    /// 全部 Heading 是否已物化（语义准备阶段置位；完整解析路径无需）
    /// P2 delimiter 工作区：索引成链的临时 delimiter，容量跨容器复用
    pub(crate) delimiter_store: Vec<crate::inlines::delimiter::Delimiter>,
    /// P3 bracket 工作区：索引成链（prev 单向）的临时 bracket
    pub(crate) bracket_store: Vec<crate::inlines::bracket::Bracket>,
    /// 复用的临时容器：resolved footnote 列表
    footnote_resolved_scratch: Vec<(usize, usize)>,
    pub(crate) parse_error: Option<ParseError>,
}

impl<'input> Parser<'input> {
    /// Returns the crate version embedded at compile time.
    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    /// Creates a parser with [`ParserOptions::default`].
    pub fn new(text: &'input str) -> Self {
        Self::new_with_options(text, ParserOptions::default())
    }
    /// Creates a parser with explicit options.
    pub fn new_with_options(text: &'input str, options: ParserOptions) -> Self {
        // 预估节点数量：大约每 10 字节一个节点
        // C4：构造期只按 Block 相位预估（选择性会话不为整棵 Inline 树买单）；
        // inline 相位入口按 pending 规模一次 reserve 补齐。
        let estimated_nodes = text.len() / 10;
        let mut tree = Tree::<Node>::with_capacity(estimated_nodes.min(4096));
        let doc = tree.append(Node::new(MarkdownNode::Document, 0));
        let scanner = Scanner::new(text);
        // 预估 inline 容器与总行段数量（C4：条目 + 共享 span arena 双容量）
        let estimated_inlines = text.len() / 80;
        let estimated_spans = text.len() / 32;
        Self {
            scanner,
            inlines: crate::pending::PendingInlines::with_capacity(
                estimated_inlines.min(1024),
                estimated_spans.min(8192),
            ),
            options,
            link_refs: FxHashMap::default(),
            footnotes: FxHashMap::default(),
            footnote_refs: FxHashMap::default(),
            tags: FxHashSet::default(),
            line_starts: OnceLock::new(),
            tree,
            doc,
            curr_proc_node: doc,
            prev_proc_node: doc,
            all_closed: true,
            last_matched_node: doc,
            last_offset: u32::default(),
            html_stacks: VecDeque::new(),
            text_postprocess_parents: FxHashSet::default(),
            ref_link_candidates_scratch: Vec::with_capacity(64),
            footnote_ref_nodes: Vec::new(),
            inline_footnote_defs: Vec::new(),
            reference_definitions_extracted: false,
            block_ids_discovered: false,
            heading_nodes: Vec::new(),
            semantic_id_nodes: Vec::new(),
            top_level_finalized: 0,
            delimiter_store: Vec::new(),
            bracket_store: Vec::new(),
            footnote_resolved_scratch: Vec::with_capacity(32),
            parse_error: None,
        }
    }

    /// Parses the complete document, including all block and inline nodes.
    ///
    /// The returned [`Document`] borrows `text` from this parser's constructor.
    pub fn parse(mut self) -> Result<Document<'input>, ParseError> {
        self.ensure_limits()?;
        self.parse_frontmatter()?;
        self.continue_parse()
    }
    /// Parses a complete document that owns its source string.
    ///
    /// This is useful for WASM, FFI, caches, and any caller that needs the
    /// document to outlive the original input binding.
    pub fn parse_string(
        source: String,
        options: ParserOptions,
    ) -> Result<Document<'static>, ParseError> {
        let (tree, tags, line_starts) = {
            let document = Parser::new_with_options(&source, options).parse()?;
            let Document {
                tree,
                tags,
                line_starts,
                ..
            } = document;
            (tree, tags, line_starts)
        };
        Ok(Document {
            source: SourceText::Owned(source),
            tree,
            tags,
            line_starts,
        })
    }
    /// Parses owned source while materializing inline content only for selected nodes.
    ///
    /// The block tree remains complete. Selecting a container also selects
    /// inline-capable descendants and required footnote definitions. Node IDs
    /// must come from parsing identical source with identical options.
    pub fn parse_selected_string(
        source: String,
        options: ParserOptions,
        node_ids: &[usize],
    ) -> Result<Document<'static>, ParseError> {
        let (tree, tags, line_starts) = {
            let phase = Parser::new_with_options(&source, options)
                .run_block_phase(None)?
                .prepare_semantic_targets()?;
            let mut selection = crate::selective::InlineSelection::default();
            for &id in node_ids {
                selection.select(id);
            }
            let output = phase.parse_selected_inlines(selection)?;
            let Document {
                tree,
                tags,
                line_starts,
                ..
            } = output.document;
            (tree, tags, line_starts)
        };
        Ok(Document {
            source: SourceText::Owned(source),
            tree,
            tags,
            line_starts,
        })
    }
    /// Parses only frontmatter and returns an owned partial document plus a snapshot.
    pub fn parse_frontmatter_phase_string(
        source: String,
        options: ParserOptions,
    ) -> Result<(Document<'static>, ParserPhaseSnapshot), ParseError> {
        let (tree, tags, line_starts, snapshot) = {
            let (document, snapshot) =
                Parser::new_with_options(&source, options).parse_frontmatter_phase()?;
            let Document {
                tree,
                tags,
                line_starts,
                ..
            } = document;
            (tree, tags, line_starts, snapshot)
        };
        Ok((
            Document {
                source: SourceText::Owned(source),
                tree,
                tags,
                line_starts,
            },
            snapshot,
        ))
    }
    /// Resumes an owned document created by [`Parser::parse_frontmatter_phase_string`].
    pub fn continue_parse_from_snapshot_string(
        document: Document<'static>,
        snapshot: ParserPhaseSnapshot,
    ) -> Result<Document<'static>, ParseError> {
        let Document {
            source,
            tree,
            tags,
            line_starts: _,
        } = document;
        match source {
            SourceText::Owned(source) => {
                let (tree, tags, line_starts) = {
                    let parser = Parser::from_phase_snapshot(&source, snapshot, tree, tags)?;
                    let document = parser.continue_parse()?;
                    let Document {
                        tree,
                        tags,
                        line_starts,
                        ..
                    } = document;
                    (tree, tags, line_starts)
                };
                Ok(Document {
                    source: SourceText::Owned(source),
                    tree,
                    tags,
                    line_starts,
                })
            }
            SourceText::Borrowed(source) => {
                let parser = Parser::from_phase_snapshot(source, snapshot, tree, tags)?;
                parser.continue_parse()
            }
        }
    }
    /// Continues parsing after frontmatter has already been processed.
    pub fn continue_parse(mut self) -> Result<Document<'input>, ParseError> {
        self.tree.push();
        self.enter_block_parse();
        if let Some(err) = self.parse_error.take() {
            self.tree.pop();
            return Err(err);
        }
        self.finish_inline_phase()
    }
    /// Block 阶段入口（F1）：`ensure_limits` + frontmatter 后运行带观察者的
    /// Block 扫描，返回持有全部解析器状态的 [`BlockDocument`]。
    pub(crate) fn run_block_phase(
        mut self,
        observer: Option<&mut dyn FnMut(&TopLevelBlockEvent<'_>) -> VisitControl>,
    ) -> Result<BlockDocument<'input>, ParseError> {
        self.ensure_limits()?;
        self.parse_frontmatter()?;
        self.tree.push();
        let status = self.parse_blocks_observed(observer);
        if let Some(err) = self.parse_error.take() {
            self.tree.pop();
            return Err(err);
        }
        Ok(BlockDocument {
            parser: self,
            status,
        })
    }
    /// Inline 阶段与收尾（`continue_parse` 的后半段；
    /// `BlockDocument::materialize_all` 复用同一实现）。
    pub(crate) fn finish_inline_phase(mut self) -> Result<Document<'input>, ParseError> {
        self.enter_inlines_parse();
        if let Some(err) = self.parse_error.take() {
            self.tree.pop();
            return Err(err);
        }
        self.tree.pop();
        Ok(self.into_ast())
    }
    /// Parses frontmatter only and returns a borrowed partial document plus a snapshot.
    ///
    /// Resume with [`Parser::from_phase_snapshot`] and [`Parser::continue_parse`].
    pub fn parse_frontmatter_phase(
        mut self,
    ) -> Result<(Document<'input>, ParserPhaseSnapshot), ParseError> {
        self.ensure_limits()?;
        self.parse_frontmatter()?;
        let snapshot = ParserPhaseSnapshot {
            scanner_snapshot: self.scanner.snapshot(),
            options: self.options,
            text_len: self.scanner.source().len(),
        };
        Ok((
            Document {
                source: SourceText::Borrowed(self.scanner.source_str()),
                tree: self.tree,
                tags: self.tags,
                line_starts: self.line_starts,
            },
            snapshot,
        ))
    }
    /// Reconstructs a parser from a frontmatter-phase snapshot.
    ///
    /// `text` must have the same byte length as the source used to create the
    /// snapshot. The supplied tree and tags normally come from the partial
    /// [`Document`] returned with that snapshot.
    pub fn from_phase_snapshot(
        text: &'input str,
        snapshot: ParserPhaseSnapshot,
        tree: Tree<Node>,
        tags: FxHashSet<String>,
    ) -> Result<Self, ParseError> {
        let actual = text.len();
        if actual != snapshot.text_len {
            return Err(ParseError::SnapshotInputLengthMismatch {
                expected: snapshot.text_len,
                actual,
            });
        }
        let mut parser = Self::new_with_options(text, snapshot.options);
        parser.scanner.resume(&snapshot.scanner_snapshot);
        parser.tree = tree;
        parser.tags = tags;
        parser.curr_proc_node = parser.doc;
        parser.prev_proc_node = parser.doc;
        parser.last_matched_node = parser.doc;
        parser.last_offset = 0;
        parser.all_closed = true;
        parser.parse_error = None;
        Ok(parser)
    }
    fn merge_cjk_nouns_from_frontmatter(&mut self, frontmatter: &crate::exts::yaml::YamlMap) {
        use crate::exts::yaml::YamlValue;

        let Some(value) = self
            .options
            .cjk_nouns_from_frontmatter
            .as_deref()
            .and_then(|field| frontmatter.get(field))
        else {
            return;
        };

        let mut merged = self.options.cjk_nouns.clone();
        let mut push_unique = |s: &str| {
            if !s.is_empty() && !merged.iter().any(|it| it == s) {
                merged.insert(s.to_string());
            }
        };

        match value {
            YamlValue::String(s) => push_unique(s.trim()),
            YamlValue::List(items) => {
                for item in items {
                    if let YamlValue::String(s) = item {
                        push_unique(s.trim());
                    }
                }
            }
            _ => {}
        }
        self.options.cjk_nouns = merged;
    }
    /// Parses a leading frontmatter section into the current parser tree.
    ///
    /// Most callers should use [`Parser::parse`] or
    /// [`Parser::parse_frontmatter_phase`] instead.
    pub fn parse_frontmatter(&mut self) -> Result<(), ParseError> {
        if let Some(frontmatter) = exts::frontmatter::parse(self) {
            self.merge_cjk_nouns_from_frontmatter(&frontmatter);
            let idx = self.tree.append_child(
                self.doc,
                Node::new(MarkdownNode::FrontMatter(Box::new(frontmatter)), 0),
            );
            self.tree[idx].processing = false;
            self.tree[idx].span.end = self.scanner.pos() as u32;
            if self.reach_node_limit() {
                if let Some(err) = self.parse_error.take() {
                    return Err(err);
                }
                return Err(ParseError::NodeLimitExceeded {
                    limit: self.options.max_nodes.unwrap_or(0),
                    actual: self.tree.node_slots_len(),
                });
            }
        }
        Ok(())
    }

    // +9.1691ms
    //     while            +4.6833ms
    //     incorporate_line +4.4858ms
    //     ...              +1ms
    fn enter_block_parse(&mut self) {
        self.parse_blocks_observed(None);
    }
    /// Block 扫描主循环。`observer` 存在时，在每个稳定行边界对新近
    /// finalized 的顶层子节点派发事件；无观察者时与原路径逐行为一致。
    fn parse_blocks_observed(
        &mut self,
        mut observer: Option<&mut dyn FnMut(&TopLevelBlockEvent<'_>) -> VisitControl>,
    ) -> BlockScanStatus {
        // 已派发（或按 predicate 消费）的最后一个顶层子节点
        let mut cursor: Option<usize> = None;
        let mut stopped = false;
        let mut dispatched_mark = self.top_level_finalized;
        while let Some(line) = Span::extract(&mut self.scanner) {
            if self.reach_node_limit() {
                break;
            }
            // 若本行内容随 Stop 被丢弃，doc.end 需要还原到上一行的位置语义
            let prev_last_offset = self.last_offset;
            let last_offset = if line.is_blank() {
                self.last_offset
            } else {
                line.end() as u32
            };
            self.incorporate_line(line);
            self.last_offset = last_offset;
            if self.reach_node_limit() {
                break;
            }
            if let Some(obs) = observer.as_deref_mut() {
                // C5：仅在有顶层节点自上次派发后完成时才扫描（每行边界的无效扫描税）
                if self.top_level_finalized != dispatched_mark {
                    dispatched_mark = self.top_level_finalized;
                    if self.dispatch_top_level_events(obs, &mut cursor, prev_last_offset) {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        if self.parse_error.is_some() {
            return BlockScanStatus::Complete;
        }
        while self.curr_proc_node != self.doc {
            self.finalize(self.curr_proc_node, self.last_offset)
        }
        if !stopped {
            if let Some(obs) = observer.as_deref_mut() {
                // EOF 边界：此时不存在未接受的行，last_offset 无需还原
                stopped = self.dispatch_top_level_events(obs, &mut cursor, self.last_offset);
            }
        }
        self.tree[self.doc].span.end = self.last_offset;
        self.tree.reset();
        if stopped {
            BlockScanStatus::Stopped
        } else {
            BlockScanStatus::Complete
        }
    }
    /// 从 `cursor` 之后遍历 `Document` 直接子节点，对每个已 finalized 的
    /// 节点派发事件；frontmatter 跳过。visitor 返回 `Stop` 时丢弃该节点
    /// 之后的全部内容并返回 `true`。
    fn dispatch_top_level_events(
        &mut self,
        observer: &mut dyn FnMut(&TopLevelBlockEvent<'_>) -> VisitControl,
        cursor: &mut Option<usize>,
        prev_last_offset: u32,
    ) -> bool {
        loop {
            let next = match *cursor {
                Some(prev) => self.tree.get_next(prev),
                None => self.tree.get_first_child(self.doc),
            };
            let Some(child) = next else {
                return false;
            };
            // 仍在处理中的子节点及其后续内容还未稳定
            if self.tree[child].processing {
                return false;
            }
            *cursor = Some(child);
            if matches!(self.tree[child].body, MarkdownNode::FrontMatter(_)) {
                continue;
            }
            let source = self.scanner.source_str();
            let control = {
                let event = TopLevelBlockEvent {
                    node_id: child,
                    tree: &self.tree,
                    source,
                    line_starts: &self.line_starts,
                    lines: Cell::new(None),
                };
                observer(&event)
            };
            match control {
                VisitControl::Continue => continue,
                VisitControl::Stop => {
                    // 当前行开启了将被丢弃的内容时，本行不属于已接受前缀，
                    // last_offset 还原为上一行的值（与直接解析前缀一致）
                    if self.tree.get_next(child).is_some() || self.curr_proc_node != self.doc {
                        self.last_offset = prev_last_offset;
                    }
                    self.discard_unaccepted_after(child);
                    return true;
                }
            }
        }
    }
    /// 终态停止：卸链 `accepted` 之后的全部顶层子节点，并清理它们的
    /// pending inline、footnote 注册、HTML 栈与 fork 状态。
    /// 节点 id 单调分配，`accepted` 之后创建的所有节点 id 均不小于其
    /// 下一个兄弟节点的 id，因此按 id 阈值清理是完备的。
    fn discard_unaccepted_after(&mut self, accepted: usize) {
        if let Some(first_dropped) = self.tree.get_next(accepted) {
            let cutoff = first_dropped;
            let mut cur = Some(first_dropped);
            while let Some(id) = cur {
                cur = self.tree.get_next(id);
                self.tree.unlink(id);
            }
            while let Some(top) = self.tree.peek_up() {
                if top >= cutoff {
                    self.tree.pop();
                } else {
                    break;
                }
            }
            self.inlines.discard_from(cutoff);
            self.footnotes.retain(|_, id| *id < cutoff);
            self.html_stacks.retain(|(_, id)| *id < cutoff);
            self.heading_nodes.retain(|id| *id < cutoff);
            self.semantic_id_nodes.retain(|id| *id < cutoff);
        }
        self.curr_proc_node = self.doc;
        self.prev_proc_node = self.doc;
        self.last_matched_node = self.doc;
        self.all_closed = true;
    }
    // +9.5869ms
    //     inlines::process +8.729ms
    fn enter_inlines_parse(&mut self) {
        if self.reach_node_limit() {
            return;
        }
        // BlockId 无需在此发现：inline 引擎在物化时就是 id 的写入者。
        // discover_block_ids 仅服务语义准备阶段（选择前的可寻址性）。
        self.prepare_reference_definitions();
        // C4：按 pending 规模一次预留 inline 节点空间（经验 ≈3 节点/条目）
        self.tree.reserve_nodes(self.inlines.len() * 3 + 64);
        // 存储本身即文档顺序（P4）；B1 的排序已随之移除
        while let Some((idx, spans)) = self.inlines.take_next_in_document_order() {
            if self.reach_node_limit() {
                return;
            }
            self.materialize_pending_entry(idx, spans);
            if self.reach_node_limit() {
                return;
            }
        }
        if self.reach_node_limit() {
            return;
        }
        self.parse_footnote_list();
    }
    /// 物化一个 pending 条目：完整解析与选择性解析共用的唯一物化器
    /// （末行去尾空白 → inline 引擎 → 组件规范化）。
    pub(crate) fn materialize_pending_entry(
        &mut self,
        idx: usize,
        mut spans: crate::pending::PendingSegments<'input>,
    ) {
        let node = &self.tree[idx].body;
        if !node.accepts_lines() {
            eprintln!("WARNING: Invalid node {node:?} exists inlines");
            return;
        }
        // 去除最后一个 Span 末尾的空白
        if let Some(last) = spans.last_mut() {
            last.trim_end_matches(|b: u8| b == b' ' || b == b'\t');
        }
        inlines::process(idx, self, spans);
        self.normalize_component_children(idx);
    }
    pub fn into_ast(self) -> Document<'input> {
        Document {
            source: SourceText::Borrowed(self.scanner.source_str()),
            tree: self.tree,
            tags: self.tags,
            line_starts: self.line_starts,
        }
    }
    fn ensure_limits(&self) -> Result<(), ParseError> {
        // TextRef::Source 的区间使用 u32 偏移
        let actual = self.scanner.source().len();
        if actual > u32::MAX as usize {
            return Err(ParseError::InputTooLarge {
                limit: u32::MAX as usize,
                actual,
            });
        }
        if let Some(limit) = self.options.max_input_bytes {
            if actual > limit {
                return Err(ParseError::InputTooLarge { limit, actual });
            }
        }
        if let Some(limit) = self.options.max_nodes {
            let actual = self.tree.node_slots_len();
            if actual > limit {
                return Err(ParseError::NodeLimitExceeded { limit, actual });
            }
        }
        Ok(())
    }
    pub(crate) fn reach_node_limit(&mut self) -> bool {
        let Some(limit) = self.options.max_nodes else {
            return false;
        };
        let actual = self.tree.node_slots_len();
        if actual <= limit {
            return false;
        }
        if self.parse_error.is_none() {
            self.parse_error = Some(ParseError::NodeLimitExceeded { limit, actual });
        }
        true
    }
    pub(crate) fn parse_reference_link(&mut self) {
        let mut nodes = std::mem::take(&mut self.ref_link_candidates_scratch);
        nodes.clear();
        nodes.reserve(self.inlines.len().max(16).saturating_sub(nodes.capacity()));
        self.collect_ref_link_candidates(self.doc, &mut nodes);
        for idx in nodes.iter().copied() {
            match self.tree[idx].body {
                MarkdownNode::Paragraph => inlines::process_link_reference(self, idx),
                MarkdownNode::Heading(crate::ast::heading::Heading::SETEXT(_)) => {
                    inlines::process_setext_heading_link_reference(self, idx)
                }
                _ => {}
            }
        }
        nodes.clear();
        self.ref_link_candidates_scratch = nodes;
    }
    /// 只收集 Paragraph 和 SETEXT Heading 节点（用于 reference link 解析）
    fn collect_ref_link_candidates(&self, parent: usize, out: &mut Vec<usize>) {
        let mut next = self.tree.get_first_child(parent);
        while let Some(idx) = next {
            match &self.tree[idx].body {
                MarkdownNode::Paragraph
                | MarkdownNode::Heading(crate::ast::heading::Heading::SETEXT(_)) => {
                    out.push(idx);
                }
                _ => {
                    self.collect_ref_link_candidates(idx, out);
                }
            }
            next = self.tree.get_next(idx);
        }
    }
    pub(crate) fn parse_footnote_list(&mut self) {
        if self.footnote_ref_nodes.is_empty() {
            self.footnotes.clear();
            self.footnote_refs.clear();
            return;
        }
        // 按源码位置排序引用；最终编号、出现序数与自动标签均由位置决定，
        // 与 inline 处理调度无关（如语义准备阶段的 Heading 预物化）。
        // B1 之后完整解析按文档顺序处理，位置序 == 处理序，输出保持不变。
        let mut refs = std::mem::take(&mut self.footnote_ref_nodes);
        refs.sort_by_key(|(id, _)| self.tree[*id].span.start);

        // 位置序首见顺序即最终编号顺序
        let mut order: Vec<String> = Vec::new();
        let mut totals: FxHashMap<String, usize> = FxHashMap::default();
        for (_, label) in &refs {
            if !totals.contains_key(label) {
                order.push(label.clone());
            }
            *totals.entry(label.clone()).or_default() += 1;
        }

        // 每个位置的引用总数，在重命名前按 order 对齐捕获
        let totals_by_pos: Vec<usize> = order.iter().map(|label| totals[label]).collect();

        // 内联脚注自动标签按位置序重生成，镜像创建时的命名方案：
        // 候选从自身最终编号起，跳过位置更靠前的已占用标签。
        // 先整体算出与 order 对齐的最终标签表，再两阶段应用，
        // 避免交换式重命名（1↔2）在原地 rekey 时相互覆盖。
        let provisional: FxHashSet<String> = self.inline_footnote_defs.drain(..).collect();
        let mut final_labels: Vec<String> = Vec::with_capacity(order.len());
        let mut inline_by_pos: Vec<bool> = Vec::with_capacity(order.len());
        {
            let mut taken: FxHashSet<String> = FxHashSet::default();
            for (pos, label) in order.iter().enumerate() {
                let is_inline = provisional.contains(label);
                let final_label = if is_inline {
                    let mut n = pos + 1;
                    loop {
                        let candidate = format!("inline-footnote-{n}");
                        if !taken.contains(&candidate) {
                            break candidate;
                        }
                        n += 1;
                    }
                } else {
                    label.clone()
                };
                taken.insert(final_label.clone());
                final_labels.push(final_label);
                inline_by_pos.push(is_inline);
            }
        }
        let renames: FxHashMap<String, String> = order
            .iter()
            .zip(&final_labels)
            .filter(|(old, new)| old != new)
            .map(|(old, new)| (old.clone(), new.clone()))
            .collect();
        if !renames.is_empty() {
            // 两阶段迁移 footnotes map 与定义 payload：先全部取出，再写入新键
            let moved: Vec<(String, usize)> = renames
                .iter()
                .filter_map(|(old, new)| {
                    self.footnotes
                        .remove(old)
                        .map(|def_id| (new.clone(), def_id))
                })
                .collect();
            for (new_label, def_id) in moved {
                if let MarkdownNode::Footnote(footnote) = &mut self.tree[def_id].body {
                    footnote.label = crate::utils::percent_encode::encode(&new_label, true);
                }
                self.footnotes.insert(new_label, def_id);
            }
            for (_, label) in refs.iter_mut() {
                if let Some(new) = renames.get(label) {
                    *label = new.clone();
                }
            }
        }
        let order = final_labels;

        // 补丁每个引用节点：最终编号 + 位置序出现序数 + 最终标签
        let index_of: FxHashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, label)| (label.as_str(), i + 1))
            .collect();
        let mut occurrence: FxHashMap<&str, usize> = FxHashMap::default();
        for (id, label) in &refs {
            let index = index_of[label.as_str()];
            let seen = occurrence.entry(label.as_str()).or_insert(0);
            *seen += 1;
            if let MarkdownNode::Link(link) = &mut self.tree[*id].body {
                if let crate::ast::link::Link::Footnote(footnote_link) = link.as_mut() {
                    footnote_link.index = index;
                    footnote_link.ref_count = *seen;
                    footnote_link.footnote_label =
                        crate::utils::percent_encode::encode(label, true);
                }
            }
        }

        let mut resolved = std::mem::take(&mut self.footnote_resolved_scratch);
        resolved.clear();
        resolved.reserve(order.len().saturating_sub(resolved.capacity()));
        // FootnoteList 的结束位置按文档位置计算：优先取位置最靠后的内联脚注
        // 定义，否则取位置最靠后的块级定义（等价于旧的"最后创建的定义"，
        // 但与 inline 处理调度无关）。
        let mut end_all: Option<(u32, u32)> = None;
        let mut end_inline: Option<(u32, u32)> = None;
        for ((label, total), is_inline) in order.iter().zip(&totals_by_pos).zip(&inline_by_pos) {
            if let Some(def_id) = self.footnotes.remove(label) {
                resolved.push((def_id, *total));
                let node = &self.tree[def_id];
                let key = node.span.start;
                let entry = (key, node.span.end);
                if end_all.map(|(k, _)| key >= k).unwrap_or(true) {
                    end_all = Some(entry);
                }
                if *is_inline && end_inline.map(|(k, _)| key >= k).unwrap_or(true) {
                    end_inline = Some(entry);
                }
            }
        }
        let end_location = end_inline
            .or(end_all)
            .map(|(_, end)| end)
            .unwrap_or(self.tree[self.doc].span.end);
        inlines::process_footnote_list(self, &resolved, end_location);
        self.footnotes.clear();
        self.footnote_refs.clear();

        resolved.clear();
        self.footnote_resolved_scratch = resolved;
        refs.clear();
        self.footnote_ref_nodes = refs;
    }
    fn incorporate_line(&mut self, mut line: Span<'input>) {
        let mut container = self.doc;
        self.prev_proc_node = self.curr_proc_node;
        while let Some(last_child) = &self
            .tree
            .get_last_child(container)
            .filter(|idx| self.tree[*idx].processing)
        {
            container = *last_child;
            match blocks::process(container, self, &mut line) {
                BlockProcessing::Processed => {
                    return;
                }
                BlockProcessing::Further => {
                    continue;
                }
                BlockProcessing::Unprocessed => {
                    container = self.tree.get_parent(container);
                    break;
                }
            }
        }
        self.all_closed = container == self.prev_proc_node;
        self.last_matched_node = container;
        let mut matched_leaf = !matches!(self.tree[container].body, MarkdownNode::Paragraph)
            && self.tree[container].body.accepts_lines();
        while !matched_leaf {
            if !line.is_indented()
                && !line
                    .get(line.indent_len())
                    .map(|b| Span::is_special_byte(b))
                    .unwrap_or(false)
            {
                line.advance_next_nonspace();
                break;
            }
            match blocks::matcher(container, self, &mut line) {
                BlockMatching::MatchedLeaf => {
                    container = self.curr_proc_node;
                    matched_leaf = true;
                }
                BlockMatching::MatchedContainer => {
                    container = self.curr_proc_node;
                }
                BlockMatching::Unmatched => {
                    line.advance_next_nonspace();
                    break;
                }
            }
        }

        let break_html_paragraph = if !self.all_closed
            && !line.is_blank_to_end()
            && matches!(self.tree[self.curr_proc_node].body, MarkdownNode::Paragraph)
        {
            let indent_len = line.indent_len();
            let has_multi_end_tag_chain = line.get(indent_len) == Some(b'<')
                && count_html_end_tag_chain(line.slice(indent_len, line.len()).as_str()) >= 2;
            if has_multi_end_tag_chain {
                true
            } else {
                let parent = self.tree.get_parent(self.curr_proc_node);
                if matches!(
                    self.tree[parent].body,
                    MarkdownNode::Html(ref h) if matches!(h.as_ref(),
                        crate::ast::html::Html::Block(
                            crate::ast::html::HtmlType::CanonicalBlockTag(..)
                                | crate::ast::html::HtmlType::GenericTag(..)
                                | crate::ast::html::HtmlType::Component(..)
                        )
                    )
                ) {
                    if line.get(indent_len) == Some(b'<') {
                        let mut scan_line = line.slice(indent_len, line.len());
                        let is_end_tag = matches!(
                            crate::blocks::html::scan_html_type(
                                &mut scan_line,
                                false,
                                self.options.jsx_like_component
                            ),
                            Some((
                                _,
                                _,
                                crate::ast::html::HtmlType::CanonicalBlockTag(
                                    _,
                                    crate::ast::html::Flag::End
                                ) | crate::ast::html::HtmlType::GenericTag(
                                    _,
                                    crate::ast::html::Flag::End
                                ) | crate::ast::html::HtmlType::Component(
                                    _,
                                    crate::ast::html::Flag::End
                                )
                            ))
                        );
                        is_end_tag
                            || count_html_end_tag_chain(line.slice(indent_len, line.len()).as_str())
                                >= 2
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        } else {
            false
        };

        if !self.all_closed
            && !line.is_blank_to_end()
            && matches!(self.tree[self.curr_proc_node].body, MarkdownNode::Paragraph)
            && !break_html_paragraph
        {
            // 段落延续行：跳过前导空白
            line.advance_next_nonspace();
            self.append_inline(self.curr_proc_node, line);
        } else {
            if self.tree[self.prev_proc_node].body.support_reprocess() && !line.is_end() {
                blocks::reprocess(self.prev_proc_node, self, &mut line);
            }
            self.close_unmatched_blocks();
            let cur_container = &mut self.tree[container].body;
            if cur_container.accepts_lines() && (!line.is_end() || line.is_blank()) {
                if let MarkdownNode::Html(html) = cur_container {
                    let component_block_open = matches!(
                        html.as_ref(),
                        crate::ast::html::Html::Block(crate::ast::html::HtmlType::Component(
                            _,
                            crate::ast::html::Flag::Begin
                        ))
                    );
                    let snapshot = line.snapshot();
                    if let Some((before, after)) = html.scan_end_span(&mut line) {
                        line.resume(&snapshot);
                        if before > 0 {
                            let sub = line.slice(0, before);
                            if component_block_open {
                                self.append_inline(container, sub);
                            } else {
                                self.append_html_raw_text_line(
                                    container,
                                    sub.to_unescape_string(),
                                    (sub.cursor_or_end() as u32, sub.end() as u32),
                                );
                            }
                        }
                        line.skip(after);
                        self.finalize(container, line.cursor_or_end() as u32);
                        if !line.is_end() {
                            let idx = self
                                .append_block(MarkdownNode::Paragraph, line.cursor_or_end() as u32);
                            self.append_inline(idx, line);
                        }
                    } else {
                        line.resume(&snapshot);
                        if component_block_open {
                            self.append_inline(container, line.slice(0, line.len()));
                        } else {
                            self.append_html_raw_text_line(
                                container,
                                line.to_unescape_string(),
                                (line.cursor_or_end() as u32, line.end() as u32),
                            );
                        }
                    }
                } else if !line.is_end() || line.is_blank() {
                    if matches!(self.tree[container].body, MarkdownNode::Paragraph) {
                        line.advance_next_nonspace();
                    }
                    self.append_inline(container, line);
                }
            } else if !line.is_end() && !line.is_blank() {
                container = self.append_block(MarkdownNode::Paragraph, line.cursor_or_end() as u32);
                self.append_inline(container, line);
            }
        }
    }
    pub(crate) fn append_block(&mut self, node: MarkdownNode, loc: u32) -> usize {
        let is_heading = matches!(node, MarkdownNode::Heading(_));
        // 如果当前处理中的节点无法容纳插入的节点则退回当上一层
        while !self.tree[self.curr_proc_node].body.can_contain(&node) {
            self.finalize(self.curr_proc_node, loc)
        }
        let idx = self.tree.append(Node::new(node, loc));
        if is_heading {
            self.heading_nodes.push(idx);
        }
        self.tree.push();
        self.curr_proc_node = idx;
        self.last_offset = loc;
        // println!(
        //     "创建节点 #{idx} {:?} ↑ {:?} ← {:?} 🤣 {:?}",
        //     self.tree[idx].body,
        //     self.tree.get_parent(idx),
        //     self.tree.get_prev(idx),
        //     self.tree
        //         .get_prev(idx)
        //         .and_then(|idx| self.tree.get_next(idx))
        // );
        idx
    }
    pub(crate) fn append_free_node(&mut self, node: MarkdownNode, loc: u32) -> usize {
        let idx = self.tree.create_node(Node::new(node, loc));
        // #[cfg(debug_assertions)]
        // println!("创建游离节点 #{idx} {:?}", self.tree[idx].body);
        idx
    }
    pub(crate) fn append_to(
        &mut self,
        id: usize,
        node: MarkdownNode,
        location: (u32, u32),
    ) -> usize {
        let needs_text_postprocess = matches!(&node, MarkdownNode::Text(_))
            && self
                .tree
                .get_last_child(id)
                .is_some_and(|last| matches!(self.tree[last].body, MarkdownNode::Text(..)));
        let idx = self.tree.append_child(id, Node::new(node, location.0));
        self.tree[idx].span.end = location.1;
        if needs_text_postprocess {
            self.text_postprocess_parents.insert(id);
        }
        // println!("创建节点 #{idx} {:?}", self.tree[idx].body)
        idx
    }
    pub(crate) fn replace_block(&mut self, node: MarkdownNode, loc: u32) -> Option<usize> {
        self.last_offset = loc;
        let is_heading = matches!(node, MarkdownNode::Heading(_));
        if let Some(idx) = self.tree.peek_up() {
            // println!("替换节点 {:?} => {:?}", self.tree[idx].body, node)
            self.tree[idx].body = node;
            if is_heading {
                self.heading_nodes.push(idx);
            }
            Some(idx)
        } else {
            None
        }
    }
    pub(crate) fn append_inline(&mut self, block_idx: usize, line: Span<'input>) {
        self.inlines.push_line(block_idx, line)
    }
    pub(crate) fn append_text(&mut self, content: impl AsRef<str>, location: (u32, u32)) -> usize {
        // 如果当前处理中的节点无法容纳插入的节点则退回当上一层
        if !self.tree[self.curr_proc_node].body.accepts_lines() {
            panic!(
                "Failed to append text to {:?}  block, the block could not accepts lines",
                self.tree[self.curr_proc_node].body
            )
        }
        let idx = self
            .tree
            .append(Node::new(content.as_ref().into(), location.0));
        self.tree[idx].span.end = location.1;
        // println!("创建节点 #{idx} {:?}", self.tree[idx].body)
        idx
    }
    /// 插入文本当目标节点，这会自动合并相邻 *仍在处理* 的 Text 节点
    pub(crate) fn append_text_to(
        &mut self,
        parent: usize,
        content: &str,
        location: (u32, u32),
    ) -> usize {
        let transformed = if self.options.smart_punctuation
            && crate::utils::smart_punctuation::needs_smart_punctuation(content)
        {
            Some(crate::utils::smart_punctuation::smart_punctuation(content))
        } else {
            None
        };

        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            if let Some(cow) = &transformed {
                text.make_owned(source).push_str(cow.as_ref());
            } else {
                text.make_owned(source).push_str(content);
            }
            self.tree[idx].span.end = location.1;
            return idx;
        }
        match transformed {
            Some(std::borrow::Cow::Owned(s)) => {
                self.append_text_to_owned_no_smart(parent, s, location)
            }
            Some(std::borrow::Cow::Borrowed(s)) => {
                self.append_text_to_no_smart(parent, s, location)
            }
            None => self.append_text_to_no_smart(parent, content, location),
        }
    }

    /// 追加**源码切片**文本（P1b 快路径）：`[start, end)` 为源码绝对 byte 区间。
    /// 与上一个仍在处理的 Text 相邻（上一个为 `Source` 且区间首尾相接）时
    /// 零拷贝扩展区间；否则按需物化拼接；新建节点保存 `TextRef::Source`。
    pub(crate) fn append_text_span_to(
        &mut self,
        parent: usize,
        start: u32,
        end: u32,
        location: (u32, u32),
    ) -> usize {
        debug_assert!(end as usize <= self.scanner.source().len());
        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            match text {
                TextRef::Source(span) if span.end == start => span.end = end,
                _ => text
                    .make_owned(source)
                    .push_str(&source[start as usize..end as usize]),
            }
            self.tree[idx].span.end = location.1;
            return idx;
        }
        let has_left_text = self
            .tree
            .get_last_child(parent)
            .is_some_and(|id| matches!(self.tree[id].body, MarkdownNode::Text(..)));
        let idx = self.tree.append_child(
            parent,
            Node::new(
                MarkdownNode::Text(TextRef::Source(crate::ast::text::SourceSpan::new(
                    start, end,
                ))),
                location.0,
            ),
        );
        self.tree[idx].span.end = location.1;
        if has_left_text {
            self.text_postprocess_parents.insert(parent);
        }
        idx
    }

    #[inline]
    pub(crate) fn append_text_to_no_smart(
        &mut self,
        parent: usize,
        content: &str,
        location: (u32, u32),
    ) -> usize {
        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            text.make_owned(source).push_str(content);
            self.tree[idx].span.end = location.1;
            return idx;
        }

        let has_left_text = self
            .tree
            .get_last_child(parent)
            .is_some_and(|id| matches!(self.tree[id].body, MarkdownNode::Text(..)));

        let text = TextRef::Owned(content.to_owned());
        let idx = self
            .tree
            .append_child(parent, Node::new(MarkdownNode::Text(text), location.0));
        self.tree[idx].span.end = location.1;
        if has_left_text {
            self.text_postprocess_parents.insert(parent);
        }
        idx
    }
    /// 与 append_text_to 相同，但直接接受 String 避免重复分配
    pub(crate) fn append_text_to_owned(
        &mut self,
        parent: usize,
        mut content: String,
        location: (u32, u32),
    ) -> usize {
        // 应用 smart punctuation 转换（dash 和 ellipsis）
        if self.options.smart_punctuation
            && crate::utils::smart_punctuation::needs_smart_punctuation(&content)
        {
            let transformed = crate::utils::smart_punctuation::smart_punctuation(&content);
            if let std::borrow::Cow::Owned(new_content) = transformed {
                content = new_content;
            }
        }

        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            text.make_owned(source).push_str(&content);
            self.tree[idx].span.end = location.1;
            idx
        } else {
            self.append_text_to_owned_no_smart(parent, content, location)
        }
    }
    #[inline]
    pub(crate) fn append_text_to_owned_no_smart(
        &mut self,
        parent: usize,
        content: String,
        location: (u32, u32),
    ) -> usize {
        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            text.make_owned(source).push_str(&content);
            self.tree[idx].span.end = location.1;
            return idx;
        }
        let has_left_text = self
            .tree
            .get_last_child(parent)
            .is_some_and(|id| matches!(self.tree[id].body, MarkdownNode::Text(..)));
        let idx = self.tree.append_child(
            parent,
            Node::new(MarkdownNode::Text(TextRef::Owned(content)), location.0),
        );
        self.tree[idx].span.end = location.1;
        if has_left_text {
            self.text_postprocess_parents.insert(parent);
        }
        idx
    }
    #[inline]
    pub(crate) fn append_text_char_to(
        &mut self,
        parent: usize,
        ch: char,
        location: (u32, u32),
    ) -> usize {
        if let Some((idx, MarkdownNode::Text(text))) = self
            .tree
            .get_last_child(parent)
            .filter(|id| self.tree[*id].processing)
            .map(|id| (id, &mut self.tree[id].body))
        {
            let source = self.scanner.source_str();
            text.make_owned(source).push(ch);
            self.tree[idx].span.end = location.1;
            return idx;
        }
        let has_left_text = self
            .tree
            .get_last_child(parent)
            .is_some_and(|id| matches!(self.tree[id].body, MarkdownNode::Text(..)));
        let idx = self.tree.append_child(
            parent,
            Node::new(MarkdownNode::Text(TextRef::from(ch)), location.0),
        );
        self.tree[idx].span.end = location.1;
        if has_left_text {
            self.text_postprocess_parents.insert(parent);
        }
        idx
    }
    #[inline]
    pub(crate) fn take_text_postprocess_flag(&mut self, parent: usize) -> bool {
        self.text_postprocess_parents.remove(&parent)
    }
    pub(crate) fn mark_as_processed(&mut self, idx: usize) {
        self.tree[idx].processing = false;
    }
    pub(crate) fn current_proc(&self) -> &Node {
        &self.tree[self.curr_proc_node]
    }
    pub(crate) fn close_unmatched_blocks(&mut self) {
        if self.all_closed {
            return;
        }
        loop {
            if self.prev_proc_node == self.last_matched_node {
                break;
            }
            let parent = self.tree.get_parent(self.prev_proc_node);
            self.finalize(self.prev_proc_node, self.last_offset);
            self.prev_proc_node = parent
        }
        self.all_closed = true;
    }
    /// 调用指定节点的 finalize 方法处理并关闭该节点，将当前节点指针移动至父节点
    pub(crate) fn finalize(&mut self, node_id: usize, location: u32) {
        let parent = self.tree.get_parent(node_id);
        assert_ne!(
            node_id, self.doc,
            "Unable call finalize to process the Document Node"
        );
        if !self.tree[node_id].processing {
            self.curr_proc_node = parent;
            return;
        }
        blocks::after(node_id, self, location);
        let node = &mut self.tree[node_id];
        node.processing = false;
        if Some(node_id) == self.tree.peek_up() {
            self.tree.pop();
        }
        if parent == self.doc {
            self.top_level_finalized += 1;
        }
        self.curr_proc_node = parent;
    }

    fn append_html_raw_text_line(
        &mut self,
        parent: usize,
        content: String,
        location: (u32, u32),
    ) -> usize {
        let mut value = content;
        if self.tree.get_last_child(parent).is_some() {
            value.insert(0, '\n');
        }
        self.append_text_to_owned(parent, value, location)
    }

    fn is_component_node(&self, idx: usize) -> bool {
        matches!(
            &self.tree[idx].body,
            MarkdownNode::Html(h)
                if matches!(
                    h.as_ref(),
                    crate::ast::html::Html::Block(crate::ast::html::HtmlType::Component(..))
                        | crate::ast::html::Html::Inline(crate::ast::html::HtmlType::Component(..))
                )
        )
    }

    fn component_name_and_flag(&self, idx: usize) -> Option<(String, crate::ast::html::Flag)> {
        let MarkdownNode::Html(h) = &self.tree[idx].body else {
            return None;
        };
        match h.as_ref() {
            crate::ast::html::Html::Block(crate::ast::html::HtmlType::Component(element, flag))
            | crate::ast::html::Html::Inline(crate::ast::html::HtmlType::Component(
                element,
                flag,
            )) => Some((element.name.clone(), flag.clone())),
            _ => None,
        }
    }

    fn set_component_flag_full(&mut self, idx: usize) {
        let MarkdownNode::Html(h) = &mut self.tree[idx].body else {
            return;
        };
        match h.as_mut() {
            crate::ast::html::Html::Block(crate::ast::html::HtmlType::Component(_, flag))
            | crate::ast::html::Html::Inline(crate::ast::html::HtmlType::Component(_, flag)) => {
                *flag = crate::ast::html::Flag::Full
            }
            _ => {}
        }
    }

    fn remove_whitespace_text_children(&mut self, parent: usize) {
        let source = self.scanner.source_str();
        let mut current = self.tree.get_first_child(parent);
        while let Some(idx) = current {
            current = self.tree.get_next(idx);
            if let MarkdownNode::Text(text) = &self.tree[idx].body {
                if text
                    .resolve(source)
                    .chars()
                    .all(|ch| matches!(ch, ' ' | '\t'))
                {
                    self.tree.remove(idx);
                }
            }
        }
    }

    pub(crate) fn normalize_component_children(&mut self, parent: usize) {
        if !self.is_component_node(parent) {
            return;
        }
        self.remove_whitespace_text_children(parent);

        let mut stack: SmallVec<[(String, usize); 8]> = SmallVec::new();
        let mut current = self.tree.get_first_child(parent);
        while let Some(idx) = current {
            let next = self.tree.get_next(idx);
            if let Some((name, flag)) = self.component_name_and_flag(idx) {
                match flag {
                    crate::ast::html::Flag::Begin => stack.push((name, idx)),
                    crate::ast::html::Flag::End => {
                        if let Some(pos) = stack.iter().rposition(|(n, _)| *n == name) {
                            let (_, begin_idx) = stack.remove(pos);
                            let mut walker = self.tree.get_next(begin_idx);
                            while let Some(child) = walker {
                                if child == idx {
                                    break;
                                }
                                walker = self.tree.get_next(child);
                                self.tree.unlink(child);
                                self.tree.set_parent(child, begin_idx);
                            }
                            self.set_component_flag_full(begin_idx);
                            self.tree.remove(idx);
                            current = self.tree.get_next(begin_idx);
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            current = next;
        }

        let mut child = self.tree.get_first_child(parent);
        while let Some(idx) = child {
            child = self.tree.get_next(idx);
            self.normalize_component_children(idx);
        }
    }
}

fn count_html_end_tag_chain(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut count = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i + 3 > bytes.len() || bytes[i] != b'<' || bytes[i + 1] != b'/' {
            break;
        }
        i += 2;
        let name_start = i;
        if i >= bytes.len() || !bytes[i].is_ascii_alphabetic() {
            return 0;
        }
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            i += 1;
        }
        if i == name_start {
            return 0;
        }
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'>' {
            return 0;
        }
        i += 1;
        count += 1;
    }
    if bytes[i..].iter().all(|b| *b == b' ' || *b == b'\t') {
        count
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{ParseError, Parser, ParserOptions};

    #[test]
    fn parse_rejects_oversized_input() {
        let text = "abcd";
        let parser =
            Parser::new_with_options(text, ParserOptions::default().with_max_input_bytes(3));
        let result = parser.parse();
        assert!(matches!(
            result,
            Err(ParseError::InputTooLarge {
                limit: 3,
                actual: 4
            })
        ));
    }

    #[test]
    fn parse_rejects_node_overflow() {
        let text = "# hi";
        let parser = Parser::new_with_options(text, ParserOptions::default().with_max_nodes(1));
        let result = parser.parse();
        assert!(matches!(
            result,
            Err(ParseError::NodeLimitExceeded { limit: 1, .. })
        ));
    }

    #[test]
    fn snapshot_resume_matches_full_parse() {
        let text = r#"---
title: Hello
---
# Hi

Text
"#;
        let options = ParserOptions::default().enabled_gfm();
        let full = Parser::new_with_options(text, options.clone())
            .parse()
            .expect("full parse should succeed");
        let (deferred_doc, snapshot) = Parser::new_with_options(text, options)
            .parse_frontmatter_phase()
            .expect("frontmatter phase should succeed");
        let resumed =
            Parser::from_phase_snapshot(text, snapshot, deferred_doc.tree, deferred_doc.tags)
                .expect("snapshot restore should succeed")
                .continue_parse()
                .expect("continue parse should succeed");
        assert_eq!(full.to_html(), resumed.to_html());
        assert_eq!(full.len(), resumed.len());
    }

    #[test]
    fn snapshot_resume_rejects_input_length_mismatch() {
        let text = "---\na: 1\n---\ncontent";
        let (deferred_doc, snapshot) = Parser::new(text)
            .parse_frontmatter_phase()
            .expect("frontmatter phase should succeed");
        let result =
            Parser::from_phase_snapshot("x", snapshot, deferred_doc.tree, deferred_doc.tags);
        assert!(matches!(
            result,
            Err(ParseError::SnapshotInputLengthMismatch {
                expected: _,
                actual: 1
            })
        ));
    }
}
