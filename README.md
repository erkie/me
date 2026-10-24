<p align="center"><img src="assets/logo/app-icon-256.png" width="128" alt="Me icon"></p>

# Me

**Me** (short for "my editor") is a fast, native code editor for **reading
markdown** and **looking things up in code**. It starts in about 0.1 s, uses
tens of megabytes instead of gigabytes, opens huge files, and follows the
system's light/dark mode. It has no AI features.

It is written in Rust on [GPUI](https://gpui.rs), the GPU-accelerated UI
framework behind Zed. macOS comes first; Linux and Windows run from the same
code. The plan and the reasoning behind the technology choice are in
[`docs/PLAN.md`](docs/PLAN.md).

## Features

- File tree sidebar (lazy, respects `.gitignore`, dims ignored files) with
  file-type icons from the Seti theme that VS Code and Cursor use, and a
  right-click menu: new file or folder, rename, move to trash, reveal in
  Finder, copy path
- Tabs: a single click opens a preview tab, editing or a double click pins
  it; drag to reorder, right-click for close others / reveal / copy path,
  double-click the empty tab bar for a new file
- Seamless markdown preview: `.md`/`.mdc` files open rendered (local
  images, code highlighted like the editor, **Mermaid diagrams** rendered
  natively), and `⇧⌘V` flips to source
- Images open as pictures; SVGs flip between picture and source
- `⌘K` command palette with every command and its shortcut
- Fast syntax highlighting (tree-sitter, viewport only, 20 languages, plus
  `#!` detection for scripts without an extension)
- Word wrap (`⌥Z`), `⌘+`/`⌘-` text size, clickable status bar for syntax
  and indentation
- `⌘T` fuzzy file finder (nucleo, with background indexing)
- `⇧⌘F` project search and replace (ripgrep's engine, streaming results)
- `⌘F` find and replace in a file, with regex, case, and whole-word options
- Multiple selections: `⌘D`, `⌘L`, `⇧⌘L`, `⌃⌘G`, `⌃⇧↑/↓`, `⌘`-click,
  and **`⌥`-drag column selection**
- Never loses work: closing a window or quitting keeps unsaved text, tabs
  and cursors, and brings them back (per folder)
- Huge files: a rope buffer, viewport-only shaping, and background loading
  and parsing
- `me` CLI: `me .`, `me file.rs:42:7`, and `me -w` for `$EDITOR`
- Native menus and context menus, transparent title bar, sidebar
  vibrancy, and automatic light/dark mode

## Install (macOS)

```sh
script/bundle-macos --install      # builds dist/Me.app, copies it to /Applications, links `me`
```

Or run **Me → Install 'me' Command…** from the app. To make Me the default
for text and code files, run **Me → Make Me the Default Editor…**.

To use it for git commit messages and other `$EDITOR` prompts:

```sh
export EDITOR="me -w"
```

## Usage

```
me                 # focus Me (launches it if needed)
me .               # open the current folder
me README.md       # open a file (rendered, since it's markdown)
me src/main.rs:42  # open at a line (and :column)
me -w FILE         # wait until the tab is closed
me -n ...          # new window
some-command | me -
```

## Development

```sh
cargo run -p me-app -- .         # run the app on this repo
cargo run -p me-cli -- .         # the CLI (talks to / launches the app next to it)
cargo test --workspace           # core, project, syntax, ipc and CLI tests
ME_TRACE_STARTUP=1 cargo run --release -p me-app   # print startup timings
```

Linux needs the GPUI system libraries:
`libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig1-dev libfreetype-dev libx11-xcb-dev libvulkan1`.

| Crate | What it is |
|-------|------------|
| `crates/text` | Rope document, selections, every editing and multi-cursor command, undo, search. No UI. |
| `crates/syntax` | Tree-sitter highlighting: lazy grammars, incremental parsing, visible-range queries. |
| `crates/project` | Directory listing, fuzzy file finder, project search and replace. |
| `crates/ipc` | CLI ⇄ app protocol over a Unix socket. |
| `crates/cli` | The `me` command (about 500 KB, no GUI dependencies). |
| `crates/app` | The GPUI app. |

Settings (`font_size`, `word_wrap`) and window sessions live in
`~/Library/Application Support/Me` (`~/.config/me` on Linux).

Mermaid diagrams are rendered by [merman](https://github.com/Latias94/merman),
a Rust port of Mermaid.

File-type icons are from [Seti UI](https://github.com/jesseweed/seti-ui)
(MIT, see `assets/file-icons/LICENSE.md`). To refresh them, run
`script/gen-file-icons <seti-ui checkout>`.
