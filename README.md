<p align="center"><img src="assets/logo/app-icon-256.png" width="128" alt="Editor icon"></p>

# Editor

A fast, native code editor for **reading markdown** and **looking things up
in code**. It starts in about 0.1 s, uses tens of megabytes instead of
gigabytes, opens huge files, and follows the system's light/dark mode. It has
no AI features.

It is written in Rust on [GPUI](https://gpui.rs), the GPU-accelerated UI
framework behind Zed. macOS comes first; Linux and Windows run from the same
code. The plan and the reasoning behind the technology choice are in
[`docs/PLAN.md`](docs/PLAN.md).

## Features

- File tree sidebar (lazy, respects `.gitignore`, dims ignored files) with
  file-type icons from the Seti theme that VS Code and Cursor use, also in
  tabs and `⌘T`
- Tabs: a single click opens a preview tab, editing or a double click pins it
- Seamless markdown preview: `.md` files open rendered, and `⇧⌘V` flips to source
- Fast syntax highlighting (tree-sitter, viewport only, 20 languages)
- `⌘T` fuzzy file finder (nucleo, with background indexing)
- `⇧⌘F` project search and replace (ripgrep's engine, streaming results)
- `⌘F` find and replace in a file, with regex, case, and whole-word options
- Multiple selections: `⌘D`, `⌘L`, `⇧⌘L`, `⌃⌘G`, `⌃⇧↑/↓`, `⌘`-click,
  and **`⌥`-drag column selection**
- Huge files: a rope buffer, viewport-only shaping, and background loading
  and parsing
- `editor` CLI: `editor .`, `editor file.rs:42:7`, and `editor -w` for
  `$EDITOR`
- Native menus, transparent title bar, sidebar vibrancy, and automatic
  light/dark mode

## Install (macOS)

```sh
script/bundle-macos --install      # builds dist/Editor.app, copies it to /Applications, links `editor`
```

Or run **Editor → Install 'editor' Command…** from the app. To make Editor the
default for text and code files, run **Editor → Make Default Editor…**.

To use it for git commit messages and other `$EDITOR` prompts:

```sh
export EDITOR="editor -w"
```

## Usage

```
editor                 # focus Editor (launches it if needed)
editor .               # open the current folder
editor README.md       # open a file (rendered, since it's markdown)
editor src/main.rs:42  # open at a line (and :column)
editor -w FILE         # wait until the tab is closed
editor -n ...          # new window
some-command | editor -
```

## Development

```sh
cargo run -p editor-app -- .     # run the app on this repo
cargo run -p editor-cli -- .     # the CLI (talks to / launches the app next to it)
cargo test --workspace           # core, project, syntax, ipc and CLI tests
EDITOR_TRACE_STARTUP=1 cargo run --release -p editor-app   # print startup timings
```

Linux needs the GPUI system libraries:
`libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig1-dev libfreetype-dev libx11-xcb-dev libvulkan1`.

| Crate | What it is |
|-------|------------|
| `crates/text` | Rope document, selections, every editing and multi-cursor command, undo, search. No UI. |
| `crates/syntax` | Tree-sitter highlighting: lazy grammars, incremental parsing, visible-range queries. |
| `crates/project` | Directory listing, fuzzy file finder, project search and replace. |
| `crates/ipc` | CLI ⇄ app protocol over a Unix socket. |
| `crates/cli` | The `editor` command (about 500 KB, no GUI dependencies). |
| `crates/app` | The GPUI app. |

File-type icons are from [Seti UI](https://github.com/jesseweed/seti-ui)
(MIT, see `assets/file-icons/LICENSE.md`). To refresh them, run
`script/gen-file-icons <seti-ui checkout>`.
