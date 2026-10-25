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
- Images open as pictures you can **paint on**: pencil, eraser, fill,
  eyedropper, line, rectangle and ellipse, brush sizes, a palette and a
  colour picker, zoom down to crisp pixels, undo, and `⌘S` writes the file
  back as PNG, JPEG, GIF, WebP, BMP or TIFF (Save As converts by extension).
  Animated images, icons and 16-bit images stay view only. SVGs flip
  between picture and source
- `⌘K` command palette with every command and its shortcut
- Fast syntax highlighting (tree-sitter, viewport only, 20 languages, plus
  `#!` detection for scripts without an extension)
- Word wrap (`⌥Z`), `⌘+`/`⌘-` text size, clickable status bar for syntax
  and indentation
- `⌘T` fuzzy file finder (nucleo, with background indexing)
- `⇧⌘F` project search and replace (ripgrep's engine, streaming results)
- `⌘F` find and replace in a file, with regex, case, and whole-word options
- **Go to definition** with `⌘`-click or `F12` and **find references**
  with `⇧F12`, through the language's standard server when it's installed
  (rust-analyzer, gopls, typescript-language-server, pyright, clangd, …)
  and a built-in tree-sitter symbol index otherwise, so it works in every
  language we highlight with nothing to install; `⌘R` jumps to a symbol in
  the file, `⇧⌘R` in the project, and `⌃-` / `⌃⇧-` go back and forward
- With a language server: **errors and warnings** underlined as you type
  (`F8` steps through them, `⇧⌘M` lists them all), **hover** for types and
  docs (rest the pointer on a word, or `⌘I`), and **rename** across files
  with `F2`
- Multiple selections: `⌘D`, `⌘L`, `⇧⌘L`, `⌃⌘G`, `⌃⇧↑/↓`, `⌥`-click,
  and **`⌥`-drag column selection**
- Never loses work: closing a window or quitting keeps unsaved text, tabs
  and cursors, and brings them back (per folder)
- Huge files: a rope buffer, viewport-only shaping, and background loading
  and parsing
- `me` CLI: `me .`, `me file.rs:42:7`, and `me -w` for `$EDITOR`
- Native menus and context menus, transparent title bar, sidebar
  vibrancy, and automatic light/dark mode

## Install (macOS)

Download `Me-macos.zip` from the [latest release](https://github.com/erkie/me/releases/latest),
unzip it and move Me.app to /Applications. It isn't notarized, so open it
the first time with right-click → Open. From then on it updates itself (see
[Updates](#updates)). Or build it yourself:

```sh
script/bundle-macos --install      # builds dist/Me.app, copies it to /Applications, links `me`
```

Or run **Me → Install 'me' Command…** from the app. To make Me the default
for text and code files, run **Me → Make Me the Default Editor…**.

To use it for git commit messages and other `$EDITOR` prompts:

```sh
export EDITOR="me -w"
```

## Updates

Every push to master that passes CI becomes a build: the `release` job
builds a universal Me.app (Apple silicon and Intel), uploads it as an
artifact, and publishes it as the GitHub release `build-<n>`, where n is
master's commit count, with the commits since the last build as notes.

Me checks for a newer build half a minute after launch and every six
hours, downloads it in the background and checks it (size, SHA-256, bundle
id, build number, code signature). It's installed when you quit, or right
away with **Restart to Update** in the status bar. **Me → Check for
Updates…** checks now; **About Me** shows the build you have.

Checking needs no account. If you're signed in to the GitHub CLI, or have
`GH_TOKEN`/`GITHUB_TOKEN` or `"github_token"` in `settings.json`, Me sends
that token along (GitHub allows more requests with one; a fork that's
private needs it). Set `"auto_update": false` in `settings.json` to turn
updates off. Development builds (`cargo run`) never update.

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

## Language servers

Me starts a language server the first time you open a file of its
language in a folder, and stops it when the window closes (or ten minutes
after its last file closed). It looks for the server on your login shell's
`PATH`, plus `~/.cargo/bin`, `~/go/bin`, `~/.local/bin`, Homebrew, and the
rbenv, asdf, mise, pyenv and nodenv shims:

| Languages | Server (first one installed wins) |
| --- | --- |
| Rust | `rust-analyzer` (`rustup component add rust-analyzer`) |
| Go | `gopls` |
| TypeScript, JavaScript | `typescript-language-server`, `vtsls` |
| Python | `pyright-langserver`, `basedpyright-langserver`, `pylsp`, `jedi-language-server` |
| C, C++ | `clangd` |
| Ruby | `ruby-lsp` (`gem install ruby-lsp`; RuboCop too if it's in your Gemfile), `solargraph` |
| Java, Swift, Lua, Shell | `jdtls`, `sourcekit-lsp`, `lua-language-server`, `bash-language-server` |

The status bar shows the server and what it's doing; click it to restart
or stop it. Next to the cursor position it counts the folder's errors and
warnings; click it for the list. Without a server (or while it's still
starting) go to definition uses the built-in index, and `F2` edits every
occurrence of the word in the file instead of renaming across files. To pick another server or turn one off,
edit `settings.json` (`~/Library/Application Support/Me` on macOS,
`~/.config/me` elsewhere):

```json
{
  "language_servers": {
    "python": { "command": "pylsp" },
    "rust": { "initialization_options": { "cargo": { "features": "all" } } },
    "typescript": { "enabled": false }
  }
}
```

The keys are `rust`, `go`, `typescript`, `python`, `c`, `java`, `ruby`,
`swift`, `lua` and `shell`.

## Development

```sh
cargo run -p me-app -- .         # run the app on this repo
cargo run -p me-cli -- .         # the CLI (talks to / launches the app next to it)
cargo test --workspace           # core, project, syntax, ipc and CLI tests
ME_TRACE_STARTUP=1 cargo run --release -p me-app   # print startup timings
```

Linux needs the GPUI system libraries:
`libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig1-dev libfreetype-dev libx11-xcb-dev libvulkan1`.

CI runs on pushes to master and on pull requests, on GitHub's Linux and
macOS machines.

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
