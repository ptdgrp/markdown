mod support;

use ptdgrp_markdown::{BlockScanStatus, MarkdownNode, Parser, ParserOptions, VisitControl};
use support::semantic::semantic_digest;

/// 每个元素是一个顶层 Block 的完整源码（含结尾空行分隔）。
fn blocks() -> Vec<&'static str> {
    vec![
        "# Title\n\n",
        "Para one with *emphasis*.\n\n",
        "> quote line\n> more quote\n\n",
        "```rust\nlet code = 1;\n```\n\n",
        "- item a\n- item b\n\n",
        "Last para.\n",
    ]
}

fn source() -> String {
    blocks().concat()
}

fn kind_name(node: &MarkdownNode) -> &'static str {
    match node {
        MarkdownNode::Heading(_) => "heading",
        MarkdownNode::Paragraph => "paragraph",
        MarkdownNode::BlockQuote => "blockquote",
        MarkdownNode::Code(_) => "code",
        MarkdownNode::List(_) => "list",
        MarkdownNode::ListItem(_) => "listitem",
        _ => "other",
    }
}

#[test]
fn parse_blocks_returns_an_inspectable_block_document() {
    let source = source();
    let blocks: ptdgrp_markdown::BlockDocument<'_> = Parser::new(&source)
        .parse_blocks()
        .expect("block parse should succeed");

    assert_eq!(blocks.block_status(), BlockScanStatus::Complete);
    assert_eq!(blocks.source(), source);
    assert!(blocks.tree().get_first_child(0).is_some());

    let document = blocks
        .materialize_all()
        .expect("inline materialization should succeed");
    assert_eq!(
        document.to_html(),
        Parser::new(&source).parse().unwrap().to_html()
    );
}

#[test]
fn block_document_can_prepare_semantics() {
    let source = "# Heading\n\nParagraph ^paragraph\n";
    let semantic = Parser::new_with_options(source, ParserOptions::default().enabled_ofm())
        .parse_blocks()
        .expect("block parse should succeed")
        .prepare_semantics()
        .expect("semantic preparation should succeed");

    assert_eq!(semantic.target_count(), 2);
}

#[test]
fn dispatches_only_document_direct_children() {
    let source = source();
    let mut kinds = Vec::new();
    let phase = Parser::new(&source)
        .parse_blocks_with(
            |_| true,
            |event| {
                // 顶层事件的父节点必须是 Document 根
                assert_eq!(event.tree().get_parent(event.node_id()), 0);
                kinds.push(kind_name(&event.node().body));
                VisitControl::Continue
            },
        )
        .unwrap();
    assert_eq!(phase.block_status(), BlockScanStatus::Complete);
    assert_eq!(
        kinds,
        [
            "heading",
            "paragraph",
            "blockquote",
            "code",
            "list",
            "paragraph"
        ],
    );
}

#[test]
fn block_events_expose_one_based_start_and_end_lines() {
    let source = "# Title\n\nPara one\ncontinued\n\n> quote\n> more\n\n```\ncode\n```\n";
    let mut lines = Vec::new();

    Parser::new(source)
        .parse_blocks_with(
            |_| true,
            |event| {
                lines.push((
                    kind_name(&event.node().body),
                    event.start_line(),
                    event.end_line(),
                ));
                VisitControl::Continue
            },
        )
        .unwrap();

    assert_eq!(
        lines,
        [
            ("heading", 1, 1),
            ("paragraph", 3, 4),
            ("blockquote", 6, 7),
            ("code", 9, 11),
        ]
    );
}

#[test]
fn block_event_lines_match_node_span_locations() {
    let source = "---\ntitle: hello\n---\n\n# 标题\n\nbody\r\ncontinued";
    let mut event_lines = Vec::new();
    let document = Parser::new_with_options(source, ParserOptions::default().enabled_ofm())
        .parse_blocks_with(
            |_| true,
            |event| {
                event_lines.push((event.node().span, event.start_line(), event.end_line()));
                VisitControl::Continue
            },
        )
        .unwrap()
        .finish()
        .unwrap();

    for (span, start_line, end_line) in event_lines {
        assert_eq!(start_line, document.location_at(span.start as usize).line);
        assert_eq!(end_line, document.location_at(span.end as usize).line);
    }
}

#[test]
fn block_event_line_range_can_be_read_end_first() {
    let mut lines = Vec::new();
    Parser::new("first\ncontinued\n\nsecond")
        .parse_blocks_with(
            |_| true,
            |event| {
                let end = event.end_line();
                lines.push((event.start_line(), end));
                VisitControl::Continue
            },
        )
        .unwrap();

    assert_eq!(lines, [(1, 2), (4, 4)]);
}

#[test]
fn top_level_event_iterates_descendant_blocks_in_preorder() {
    let source = "> outer\n> - item\n>   continuation\n";
    let mut descendants = Vec::new();

    let document = Parser::new(source)
        .parse_blocks_with(
            |_| true,
            |event| {
                descendants.extend(event.descendant_blocks().map(|block| {
                    (
                        block.node_id(),
                        kind_name(&block.node().body),
                        block.start_line(),
                        block.end_line(),
                    )
                }));
                VisitControl::Continue
            },
        )
        .unwrap()
        .finish()
        .unwrap();

    assert_eq!(
        descendants
            .iter()
            .map(|(_, kind, _, _)| *kind)
            .collect::<Vec<_>>(),
        ["paragraph", "list", "listitem", "paragraph"]
    );
    for (node_id, _, start_line, end_line) in descendants {
        let span = document.tree[node_id].span;
        assert_eq!(start_line, document.location_at(span.start as usize).line);
        assert_eq!(end_line, document.location_at(span.end as usize).line);
    }
}

#[test]
fn filter_rejection_does_not_change_parse() {
    let source = source();
    let mut seen = 0usize;
    let document = Parser::new(&source)
        .parse_blocks_with(
            |event| matches!(event.node().body, MarkdownNode::Heading(_)),
            |_| {
                seen += 1;
                VisitControl::Continue
            },
        )
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(seen, 1);

    let plain = Parser::new(&source).parse().unwrap();
    assert_eq!(semantic_digest(&document), semantic_digest(&plain));
    assert_eq!(document.to_html(), plain.to_html());
}

#[test]
fn full_traverse_finish_equals_plain_parse() {
    let source = source();
    let document = Parser::new(&source)
        .parse_blocks_with(|_| true, |_| VisitControl::Continue)
        .unwrap()
        .finish()
        .unwrap();
    let plain = Parser::new(&source).parse().unwrap();
    assert_eq!(semantic_digest(&document), semantic_digest(&plain));
    assert_eq!(document.to_html(), plain.to_html());
}

#[test]
fn full_traverse_equals_plain_parse_on_curated_corpus() {
    let source = include_str!("../bench/fixtures/curated/_data.md");
    let options = ParserOptions::default().enabled_ofm();
    let document = Parser::new_with_options(source, options.clone())
        .parse_blocks_with(|_| true, |_| VisitControl::Continue)
        .unwrap()
        .finish()
        .unwrap();
    let plain = Parser::new_with_options(source, options).parse().unwrap();
    assert_eq!(semantic_digest(&document), semantic_digest(&plain));
    assert_eq!(document.to_html(), plain.to_html());
}

#[test]
fn stop_at_each_boundary_matches_direct_prefix_parse() {
    let blocks = blocks();
    let source = source();
    for stop_at in 0..blocks.len() {
        let mut fired = 0usize;
        let phase = Parser::new(&source)
            .parse_blocks_with(
                |_| true,
                |_| {
                    if fired == stop_at {
                        VisitControl::Stop
                    } else {
                        fired += 1;
                        VisitControl::Continue
                    }
                },
            )
            .unwrap();
        // 无论停在中途还是 EOF 边界，Stop 的状态语义都是 Stopped
        assert_eq!(
            phase.block_status(),
            BlockScanStatus::Stopped,
            "stop_at={stop_at}"
        );
        let stopped = phase.finish().unwrap();

        let prefix: String = blocks[..=stop_at].concat();
        let direct = Parser::new(&prefix).parse().unwrap();
        assert_eq!(
            semantic_digest(&stopped),
            semantic_digest(&direct),
            "stop_at={stop_at} prefix={prefix:?}"
        );
        assert_eq!(stopped.to_html(), direct.to_html(), "stop_at={stop_at}");
    }
}

#[test]
fn stop_before_first_event_yields_empty_document() {
    let source = source();
    let phase = Parser::new(&source)
        .parse_blocks_with(|_| true, |_| VisitControl::Stop)
        .unwrap();
    assert_eq!(phase.block_status(), BlockScanStatus::Stopped);
    let document = phase.finish().unwrap();
    let empty = Parser::new("# Title\n").parse().unwrap();
    // 只接受第一个顶层 Block（事件对象本身）
    assert_eq!(semantic_digest(&document), semantic_digest(&empty));
}

#[test]
fn eof_final_event_fires_exactly_once() {
    // 容器在 EOF 处仍打开：list 在 EOF finalize 后派发一次
    let mut count = 0usize;
    Parser::new("- item a\n- item b\n")
        .parse_blocks_with(
            |_| true,
            |_| {
                count += 1;
                VisitControl::Continue
            },
        )
        .unwrap();
    assert_eq!(count, 1);

    let mut count = 0usize;
    Parser::new("only one paragraph")
        .parse_blocks_with(
            |_| true,
            |_| {
                count += 1;
                VisitControl::Continue
            },
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn frontmatter_does_not_produce_event() {
    let source = "---\ntitle: hello\n---\n\nPara.\n";
    let mut kinds = Vec::new();
    Parser::new_with_options(source, ParserOptions::default().enabled_ofm())
        .parse_blocks_with(
            |_| true,
            |event| {
                kinds.push(kind_name(&event.node().body));
                VisitControl::Continue
            },
        )
        .unwrap();
    assert_eq!(kinds, ["paragraph"]);
}

#[test]
fn checked_variant_propagates_and_succeeds() {
    let source = source();
    let phase = Parser::new(&source)
        .parse_blocks_with(|_| true, |_| VisitControl::Continue)
        .expect("scan should succeed");
    assert_eq!(phase.block_status(), BlockScanStatus::Complete);
    let document = phase.finish().expect("finish should succeed");
    assert_eq!(
        document.to_html(),
        Parser::new(&source).parse().unwrap().to_html(),
    );
}
