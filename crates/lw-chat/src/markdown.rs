use dioxus::prelude::*;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

/// Portable markdown renderer: pulldown-cmark AST → Dioxus RSX elements.
///
/// No `dangerous_inner_html` — works with any Dioxus renderer (webview, Blitz, WGPU).
/// Error-tolerant: never panics on malformed input. pulldown-cmark degrades gracefully
/// (unclosed markers become plain text), and this renderer handles all event variants.
#[component]
pub fn Markdown(content: String) -> Element {
    let tree = use_memo(move || parse_to_tree(&content));
    rsx! {
        div { class: "md-content",
            for node in tree().into_iter() {
                {render_node(node)}
            }
        }
    }
}

// --- Intermediate representation ---

/// A tree node representing parsed markdown content.
#[derive(Debug, Clone, PartialEq)]
enum MdNode {
    Text(String),
    SoftBreak,
    HardBreak,
    Rule,
    InlineCode(String),
    Paragraph(Vec<MdNode>),
    Heading(u8, Vec<MdNode>),
    Emphasis(Vec<MdNode>),
    Strong(Vec<MdNode>),
    Strikethrough(Vec<MdNode>),
    BlockQuote(Vec<MdNode>),
    OrderedList(Vec<MdNode>),
    UnorderedList(Vec<MdNode>),
    ListItem(Vec<MdNode>),
    CodeBlock(String, String),         // (language, content)
    Link(String, String, Vec<MdNode>), // (href, title, children)
    Image(String, String, String),     // (src, title, alt)
    Table(Vec<MdNode>),
    TableHead(Vec<MdNode>),
    TableRow(Vec<MdNode>),
    TableCell(Vec<MdNode>),
    TaskListMarker(bool),
    Footnote(String, Vec<MdNode>),
    DefinitionList(Vec<MdNode>),
    DefinitionTitle(Vec<MdNode>),
    DefinitionDef(Vec<MdNode>),
    FootnoteRef(String),
    Math(String),
    RawHtml(String),
}

/// Parse markdown string into a tree of MdNodes.
fn parse_to_tree(markdown: &str) -> Vec<MdNode> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, opts);
    let events: Vec<Event> = parser.collect();
    let mut pos = 0;
    collect_nodes(&events, &mut pos)
}

/// Consume events starting at `pos`, building MdNodes.
/// Returns when hitting an End tag or exhausting events.
fn collect_nodes(events: &[Event], pos: &mut usize) -> Vec<MdNode> {
    let mut nodes = Vec::new();

    while *pos < events.len() {
        match &events[*pos] {
            Event::Start(tag) => {
                let tag = tag.clone();
                *pos += 1;
                let children = collect_nodes(events, pos);
                nodes.push(tag_to_node(tag, children));
            }
            Event::End(_) => {
                *pos += 1;
                return nodes;
            }
            Event::Text(text) => {
                nodes.push(MdNode::Text(text.to_string()));
                *pos += 1;
            }
            Event::Code(code) => {
                nodes.push(MdNode::InlineCode(code.to_string()));
                *pos += 1;
            }
            Event::SoftBreak => {
                nodes.push(MdNode::SoftBreak);
                *pos += 1;
            }
            Event::HardBreak => {
                nodes.push(MdNode::HardBreak);
                *pos += 1;
            }
            Event::Rule => {
                nodes.push(MdNode::Rule);
                *pos += 1;
            }
            Event::TaskListMarker(checked) => {
                nodes.push(MdNode::TaskListMarker(*checked));
                *pos += 1;
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                nodes.push(MdNode::RawHtml(html.to_string()));
                *pos += 1;
            }
            Event::FootnoteReference(label) => {
                nodes.push(MdNode::FootnoteRef(label.to_string()));
                *pos += 1;
            }
            Event::DisplayMath(math) | Event::InlineMath(math) => {
                nodes.push(MdNode::Math(math.to_string()));
                *pos += 1;
            }
        }
    }

    nodes
}

/// Convert a Start tag + children into an MdNode.
fn tag_to_node(tag: Tag, children: Vec<MdNode>) -> MdNode {
    match tag {
        Tag::Paragraph => MdNode::Paragraph(children),
        Tag::Heading { level, .. } => MdNode::Heading(level as u8, children),
        Tag::Emphasis => MdNode::Emphasis(children),
        Tag::Strong => MdNode::Strong(children),
        Tag::Strikethrough => MdNode::Strikethrough(children),
        Tag::BlockQuote(_) => MdNode::BlockQuote(children),
        Tag::List(Some(_)) => MdNode::OrderedList(children),
        Tag::List(None) => MdNode::UnorderedList(children),
        Tag::Item => MdNode::ListItem(children),
        Tag::CodeBlock(kind) => {
            let lang = match kind {
                CodeBlockKind::Fenced(ref lang) if !lang.is_empty() => lang.to_string(),
                _ => String::new(),
            };
            let content = children_to_text(&children);
            MdNode::CodeBlock(lang, content)
        }
        Tag::Link {
            dest_url, title, ..
        } => MdNode::Link(dest_url.to_string(), title.to_string(), children),
        Tag::Image {
            dest_url, title, ..
        } => {
            let alt = children_to_text(&children);
            MdNode::Image(dest_url.to_string(), title.to_string(), alt)
        }
        Tag::Table(_) => MdNode::Table(children),
        Tag::TableHead => MdNode::TableHead(children),
        Tag::TableRow => MdNode::TableRow(children),
        Tag::TableCell => MdNode::TableCell(children),
        Tag::FootnoteDefinition(label) => MdNode::Footnote(label.to_string(), children),
        Tag::DefinitionList => MdNode::DefinitionList(children),
        Tag::DefinitionListTitle => MdNode::DefinitionTitle(children),
        Tag::DefinitionListDefinition => MdNode::DefinitionDef(children),
        Tag::HtmlBlock | Tag::MetadataBlock(_) => {
            // Render as container with children (plain text)
            MdNode::Paragraph(children)
        }
    }
}

/// Extract plain text from MdNode children.
fn children_to_text(children: &[MdNode]) -> String {
    let mut out = String::new();
    for node in children {
        match node {
            MdNode::Text(t) => out.push_str(t),
            MdNode::InlineCode(c) => out.push_str(c),
            MdNode::SoftBreak => out.push(' '),
            MdNode::HardBreak => out.push('\n'),
            _ => {}
        }
    }
    out
}

// --- Rendering ---

/// Render a single MdNode to a Dioxus Element.
fn render_node(node: MdNode) -> Element {
    match node {
        MdNode::Text(t) => rsx! { "{t}" },
        MdNode::SoftBreak => rsx! { " " },
        MdNode::HardBreak => rsx! { br {} },
        MdNode::Rule => rsx! { hr {} },
        MdNode::InlineCode(c) => rsx! { code { "{c}" } },

        MdNode::Paragraph(children) => rsx! {
            p { for child in children { {render_node(child)} } }
        },

        MdNode::Heading(level, children) => match level {
            1 => rsx! { h1 { for child in children { {render_node(child)} } } },
            2 => rsx! { h2 { for child in children { {render_node(child)} } } },
            3 => rsx! { h3 { for child in children { {render_node(child)} } } },
            4 => rsx! { h4 { for child in children { {render_node(child)} } } },
            5 => rsx! { h5 { for child in children { {render_node(child)} } } },
            _ => rsx! { h6 { for child in children { {render_node(child)} } } },
        },

        MdNode::Emphasis(children) => rsx! {
            em { for child in children { {render_node(child)} } }
        },

        MdNode::Strong(children) => rsx! {
            strong { for child in children { {render_node(child)} } }
        },

        MdNode::Strikethrough(children) => rsx! {
            s { for child in children { {render_node(child)} } }
        },

        MdNode::BlockQuote(children) => rsx! {
            blockquote { for child in children { {render_node(child)} } }
        },

        MdNode::OrderedList(children) => rsx! {
            ol { for child in children { {render_node(child)} } }
        },

        MdNode::UnorderedList(children) => rsx! {
            ul { for child in children { {render_node(child)} } }
        },

        MdNode::ListItem(children) => rsx! {
            li { for child in children { {render_node(child)} } }
        },

        MdNode::CodeBlock(lang, content) => {
            let class = if lang.is_empty() {
                String::new()
            } else {
                format!("language-{lang}")
            };
            rsx! {
                pre {
                    code { class: "{class}", "{content}" }
                }
            }
        }

        MdNode::Link(href, title, children) => rsx! {
            a {
                href: "{href}",
                title: "{title}",
                target: "_blank",
                rel: "noopener noreferrer",
                for child in children { {render_node(child)} }
            }
        },

        MdNode::Image(src, title, alt) => rsx! {
            img { src: "{src}", alt: "{alt}", title: "{title}" }
        },

        MdNode::Table(children) => rsx! {
            table { for child in children { {render_node(child)} } }
        },

        MdNode::TableHead(children) => rsx! {
            thead { tr { for child in children { {render_node(child)} } } }
        },

        MdNode::TableRow(children) => rsx! {
            tr { for child in children { {render_node(child)} } }
        },

        MdNode::TableCell(children) => rsx! {
            td { for child in children { {render_node(child)} } }
        },

        MdNode::TaskListMarker(checked) => rsx! {
            input { r#type: "checkbox", checked, disabled: true }
        },

        MdNode::Footnote(id, children) => rsx! {
            div { id: "fn-{id}", class: "footnote",
                sup { "[{id}]" }
                for child in children { {render_node(child)} }
            }
        },

        MdNode::DefinitionList(children) => rsx! {
            dl { for child in children { {render_node(child)} } }
        },

        MdNode::DefinitionTitle(children) => rsx! {
            dt { for child in children { {render_node(child)} } }
        },

        MdNode::DefinitionDef(children) => rsx! {
            dd { for child in children { {render_node(child)} } }
        },

        MdNode::FootnoteRef(label) => rsx! {
            sup { "[{label}]" }
        },

        MdNode::Math(m) => rsx! { code { "{m}" } },

        MdNode::RawHtml(html) => rsx! { "{html}" },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn god_test_all_markdown_syntax() {
        let input = r#"# Heading 1
## Heading 2
### Heading 3

Regular paragraph with **bold**, *italic*, ~~strikethrough~~, and `inline code`.

[A link](https://example.com) and ![an image](img.png "title").

> Blockquote with **nested bold**

- Unordered item 1
- Unordered item 2
  - Nested item

1. Ordered item 1
2. Ordered item 2

- [x] Task done
- [ ] Task todo

```rust
fn main() {
    println!("hello");
}
```

| Header A | Header B |
|----------|----------|
| Cell 1   | Cell 2   |

---
"#;

        // Parse to tree — should not panic
        let nodes = parse_to_tree(input);
        assert!(!nodes.is_empty(), "should produce non-empty tree");

        // Verify key node types are present in the tree
        let flat = flatten_tree(&nodes);

        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Heading(1, _))),
            "missing h1"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Heading(2, _))),
            "missing h2"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Heading(3, _))),
            "missing h3"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Strong(_))),
            "missing strong"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Emphasis(_))),
            "missing emphasis"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Strikethrough(_))),
            "missing strikethrough"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::InlineCode(_))),
            "missing inline code"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Link(..))),
            "missing link"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Image(..))),
            "missing image"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::BlockQuote(_))),
            "missing blockquote"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::UnorderedList(_))),
            "missing ul"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::OrderedList(_))),
            "missing ol"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::ListItem(_))),
            "missing li"
        );
        assert!(
            flat.iter()
                .any(|n| matches!(n, MdNode::CodeBlock(lang, _) if lang == "rust")),
            "missing code block"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::Table(_))),
            "missing table"
        );
        assert!(
            flat.iter().any(|n| matches!(n, MdNode::TableCell(_))),
            "missing td"
        );
        assert!(flat.iter().any(|n| matches!(n, MdNode::Rule)), "missing hr");
        assert!(
            flat.iter()
                .any(|n| matches!(n, MdNode::TaskListMarker(true))),
            "missing checked task"
        );
        assert!(
            flat.iter()
                .any(|n| matches!(n, MdNode::TaskListMarker(false))),
            "missing unchecked task"
        );

        // Verify render_node doesn't panic on each node
        for node in nodes {
            let _ = render_node(node);
        }
    }

    #[test]
    fn malformed_markdown_no_panic() {
        let input = r#"**unclosed bold
*unclosed italic
[broken link](
```rust
unclosed code block
| table | no |
| closing | row
> blockquote with **unclosed bold
- list with `unclosed code"#;

        let nodes = parse_to_tree(input);
        assert!(!nodes.is_empty(), "should produce non-empty tree");

        // Verify text fragments survive
        let all_text = collect_all_text(&nodes);
        assert!(all_text.contains("unclosed bold"), "missing text");
        assert!(all_text.contains("unclosed italic"), "missing text");
        assert!(all_text.contains("broken link"), "missing text");
        assert!(all_text.contains("unclosed code block"), "missing text");

        // Render doesn't panic
        for node in nodes {
            let _ = render_node(node);
        }
    }

    #[test]
    fn empty_input_no_panic() {
        let nodes = parse_to_tree("");
        assert!(nodes.is_empty());
    }

    /// Flatten tree for assertion convenience
    fn flatten_tree(nodes: &[MdNode]) -> Vec<&MdNode> {
        let mut out = Vec::new();
        for node in nodes {
            out.push(node);
            match node {
                MdNode::Paragraph(c)
                | MdNode::Heading(_, c)
                | MdNode::Emphasis(c)
                | MdNode::Strong(c)
                | MdNode::Strikethrough(c)
                | MdNode::BlockQuote(c)
                | MdNode::OrderedList(c)
                | MdNode::UnorderedList(c)
                | MdNode::ListItem(c)
                | MdNode::Link(_, _, c)
                | MdNode::Table(c)
                | MdNode::TableHead(c)
                | MdNode::TableRow(c)
                | MdNode::TableCell(c)
                | MdNode::Footnote(_, c)
                | MdNode::DefinitionList(c)
                | MdNode::DefinitionTitle(c)
                | MdNode::DefinitionDef(c) => {
                    out.extend(flatten_tree(c));
                }
                _ => {}
            }
        }
        out
    }

    /// Collect all text content from the tree
    fn collect_all_text(nodes: &[MdNode]) -> String {
        let mut out = String::new();
        for node in flatten_tree(nodes) {
            match node {
                MdNode::Text(t) => out.push_str(t),
                MdNode::InlineCode(c) => out.push_str(c),
                MdNode::CodeBlock(_, content) => out.push_str(content),
                MdNode::RawHtml(h) => out.push_str(h),
                _ => {}
            }
        }
        out
    }
}
