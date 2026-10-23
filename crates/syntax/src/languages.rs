//! The built-in language table.

use crate::LanguageDef;

macro_rules! grammar {
    ($krate:ident) => {
        || $krate::LANGUAGE.into()
    };
    ($krate:ident :: $lang:ident) => {
        || $krate::$lang.into()
    };
}

pub static LANGUAGES: &[LanguageDef] = &[
    LanguageDef {
        name: "Rust",
        extensions: &["rs"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_rust),
        highlights: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "JavaScript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_javascript),
        highlights: &[
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ],
    },
    LanguageDef {
        name: "TypeScript",
        extensions: &["ts", "mts", "cts"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        highlights: &[
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ],
    },
    LanguageDef {
        name: "TSX",
        extensions: &["tsx"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_typescript::LANGUAGE_TSX),
        highlights: &[
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ],
    },
    LanguageDef {
        name: "Python",
        extensions: &["py", "pyi", "pyw"],
        filenames: &[],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_python),
        highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Go",
        extensions: &["go"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_go),
        highlights: &[tree_sitter_go::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "JSON",
        extensions: &["json", "jsonc", "json5", "geojson", "webmanifest"],
        filenames: &[".prettierrc", ".babelrc", ".eslintrc"],
        line_comment: None,
        grammar: grammar!(tree_sitter_json),
        highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "TOML",
        extensions: &["toml"],
        filenames: &["Cargo.lock"],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_toml_ng),
        highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "YAML",
        extensions: &["yml", "yaml"],
        filenames: &[],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_yaml),
        highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Shell",
        extensions: &["sh", "bash", "zsh", "command"],
        filenames: &[".bashrc", ".zshrc", ".profile", ".bash_profile", ".zprofile", ".envrc"],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_bash),
        highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY],
    },
    LanguageDef {
        name: "C",
        extensions: &["c", "h"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_c),
        highlights: &[tree_sitter_c::HIGHLIGHT_QUERY],
    },
    LanguageDef {
        name: "C++",
        extensions: &["cc", "cpp", "cxx", "hpp", "hh", "hxx", "mm", "m"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_cpp),
        highlights: &[tree_sitter_cpp::HIGHLIGHT_QUERY, tree_sitter_c::HIGHLIGHT_QUERY],
    },
    LanguageDef {
        name: "CSS",
        extensions: &["css", "scss", "less"],
        filenames: &[],
        line_comment: None,
        grammar: grammar!(tree_sitter_css),
        highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "HTML",
        extensions: &["html", "htm", "xhtml", "svg", "xml", "plist"],
        filenames: &[],
        line_comment: None,
        grammar: grammar!(tree_sitter_html),
        highlights: &[tree_sitter_html::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Java",
        extensions: &["java"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_java),
        highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Ruby",
        extensions: &["rb", "rake", "gemspec", "ru"],
        filenames: &["Gemfile", "Rakefile", "Podfile", "Fastfile", "Brewfile"],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_ruby),
        highlights: &[tree_sitter_ruby::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Swift",
        extensions: &["swift"],
        filenames: &[],
        line_comment: Some("//"),
        grammar: grammar!(tree_sitter_swift),
        highlights: &[tree_sitter_swift::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Lua",
        extensions: &["lua"],
        filenames: &[],
        line_comment: Some("--"),
        grammar: grammar!(tree_sitter_lua),
        highlights: &[tree_sitter_lua::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Make",
        extensions: &["mk", "mak"],
        filenames: &["Makefile", "makefile", "GNUmakefile"],
        line_comment: Some("#"),
        grammar: grammar!(tree_sitter_make),
        highlights: &[tree_sitter_make::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Diff",
        extensions: &["diff", "patch"],
        filenames: &[],
        line_comment: None,
        grammar: grammar!(tree_sitter_diff),
        highlights: &[tree_sitter_diff::HIGHLIGHTS_QUERY],
    },
    LanguageDef {
        name: "Markdown",
        extensions: &["md", "markdown", "mdx", "mdown", "mkd"],
        filenames: &[],
        line_comment: None,
        grammar: grammar!(tree_sitter_md),
        highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
    },
];
