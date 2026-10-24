//! Tags queries the grammar crates don't ship, or ship incomplete.
//! Captures follow tree-sitter's tags convention: `@name` is the symbol's
//! name and `@definition.<kind>` the whole definition.

/// Constants, statics and `impl` blocks (containers in outlines and the
/// path bar), on top of tree-sitter-rust's own tags.
pub const RUST_EXTRA: &str = r#"
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(impl_item type: (_) @name) @definition.impl
"#;

pub const BASH: &str = r#"
(function_definition name: (word) @name) @definition.function
"#;

/// Markdown headings, as sections so nested headings nest.
pub const MARKDOWN: &str = r#"
(section (atx_heading heading_content: (inline) @name)) @definition.heading
(section (setext_heading heading_content: (paragraph) @name)) @definition.heading
"#;
