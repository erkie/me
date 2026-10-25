//! Actions and the default keymap.
//!
//! `secondary` is Cmd on macOS and Ctrl elsewhere. Defaults follow Sublime
//! Text, with VS Code aliases where they don't conflict.

use gpui::{App, KeyBinding, SharedString, actions};

actions!(
    editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        MoveWordLeft,
        MoveWordRight,
        MoveLineStart,
        MoveLineEnd,
        MoveDocStart,
        MoveDocEnd,
        PageUp,
        PageDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectLineStart,
        SelectLineEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectPageUp,
        SelectPageDown,
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Newline,
        Tab,
        Outdent,
        IndentLines,
        OutdentLines,
        ToggleComment,
        DuplicateLine,
        MoveLineUp,
        MoveLineDown,
        DeleteLine,
        Undo,
        Redo,
        Copy,
        Cut,
        Paste,
        SelectAll,
        SelectLine,
        SplitIntoLines,
        AddNextOccurrence,
        SkipOccurrence,
        SelectAllOccurrences,
        AddCursorAbove,
        AddCursorBelow,
        Cancel,
        Save,
        SaveAs,
        TogglePreview,
        GoToLine,
        UseSelectionForFind,
        ShowHover,
        ShowCompletions,
        ShowCodeActions,
        MenuUp,
        MenuDown,
        MenuPageUp,
        MenuPageDown,
        MenuConfirm,
        NextSnippetStop,
        PrevSnippetStop,
    ]
);

actions!(
    workspace,
    [
        ToggleFileFinder,
        ToggleProjectSearch,
        Find,
        FindNext,
        FindPrevious,
        ToggleSidebar,
        CloseTab,
        ReopenClosedTab,
        NextTab,
        PreviousTab,
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateTab9,
        RevealInSidebar,
        RevealInFinder,
        CopyPath,
        CopyRelativePath,
        ToggleCommandPalette,
        CloseOtherTabs,
        CloseTabsToTheRight,
        GoToDefinition,
        FindReferences,
        GoToSymbol,
        GoToProjectSymbol,
        NavigateBack,
        NavigateForward,
        RestartLanguageServer,
        StopLanguageServer,
        RenameSymbol,
        ShowProblems,
        NextProblem,
        PreviousProblem,
    ]
);

actions!(
    image,
    [
        HandTool,
        PencilTool,
        EraserTool,
        FillTool,
        EyedropperTool,
        LineTool,
        RectangleTool,
        EllipseTool,
        BiggerBrush,
        SmallerBrush,
        ZoomIn,
        ZoomOut,
        ZoomToFit,
    ]
);

/// Open a file in a pinned tab (from a menu).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenFile {
    pub path: std::path::PathBuf,
}

/// Show a file or folder in the sidebar (from a menu).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct RevealPath {
    pub path: std::path::PathBuf,
}

/// Highlight the active file as `name` (a language name), or as plain text.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = editor, no_json)]
pub struct SetLanguage {
    pub name: Option<SharedString>,
}

/// Indent the active file with tabs or with `width` spaces.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = editor, no_json)]
pub struct SetIndentation {
    pub hard_tabs: bool,
    pub width: usize,
}

actions!(
    app,
    [
        Open,
        OpenFolder,
        NewFile,
        NewWindow,
        CloseWindow,
        Quit,
        About,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        InstallCli,
        MakeDefaultEditor,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        ToggleWordWrap,
        CheckForUpdates,
        RestartToUpdate,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let editor = Some("Editor");
    let workspace = Some("Workspace");
    cx.bind_keys([
        // Movement
        KeyBinding::new("left", MoveLeft, editor),
        KeyBinding::new("right", MoveRight, editor),
        KeyBinding::new("up", MoveUp, editor),
        KeyBinding::new("down", MoveDown, editor),
        KeyBinding::new("alt-left", MoveWordLeft, editor),
        KeyBinding::new("alt-right", MoveWordRight, editor),
        KeyBinding::new("ctrl-left", MoveWordLeft, editor),
        KeyBinding::new("ctrl-right", MoveWordRight, editor),
        KeyBinding::new("home", MoveLineStart, editor),
        KeyBinding::new("end", MoveLineEnd, editor),
        KeyBinding::new("secondary-up", MoveDocStart, editor),
        KeyBinding::new("secondary-down", MoveDocEnd, editor),
        KeyBinding::new("secondary-home", MoveDocStart, editor),
        KeyBinding::new("secondary-end", MoveDocEnd, editor),
        KeyBinding::new("pageup", PageUp, editor),
        KeyBinding::new("pagedown", PageDown, editor),
        KeyBinding::new("shift-left", SelectLeft, editor),
        KeyBinding::new("shift-right", SelectRight, editor),
        KeyBinding::new("shift-up", SelectUp, editor),
        KeyBinding::new("shift-down", SelectDown, editor),
        KeyBinding::new("alt-shift-left", SelectWordLeft, editor),
        KeyBinding::new("alt-shift-right", SelectWordRight, editor),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, editor),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, editor),
        KeyBinding::new("shift-home", SelectLineStart, editor),
        KeyBinding::new("shift-end", SelectLineEnd, editor),
        KeyBinding::new("secondary-shift-up", SelectDocStart, editor),
        KeyBinding::new("secondary-shift-down", SelectDocEnd, editor),
        KeyBinding::new("shift-pageup", SelectPageUp, editor),
        KeyBinding::new("shift-pagedown", SelectPageDown, editor),
        // Editing
        KeyBinding::new("backspace", Backspace, editor),
        KeyBinding::new("shift-backspace", Backspace, editor),
        KeyBinding::new("delete", Delete, editor),
        KeyBinding::new("alt-backspace", DeleteWordLeft, editor),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, editor),
        KeyBinding::new("alt-delete", DeleteWordRight, editor),
        KeyBinding::new("ctrl-delete", DeleteWordRight, editor),
        KeyBinding::new("secondary-backspace", DeleteToLineStart, editor),
        KeyBinding::new("enter", Newline, editor),
        KeyBinding::new("shift-enter", Newline, editor),
        KeyBinding::new("tab", Tab, editor),
        KeyBinding::new("shift-tab", Outdent, editor),
        KeyBinding::new("secondary-]", IndentLines, editor),
        KeyBinding::new("secondary-[", OutdentLines, editor),
        KeyBinding::new("secondary-/", ToggleComment, editor),
        KeyBinding::new("secondary-shift-d", DuplicateLine, editor),
        KeyBinding::new("ctrl-shift-k", DeleteLine, editor),
        KeyBinding::new("secondary-z", Undo, editor),
        KeyBinding::new("secondary-shift-z", Redo, editor),
        KeyBinding::new("secondary-y", Redo, editor),
        KeyBinding::new("secondary-c", Copy, editor),
        KeyBinding::new("secondary-x", Cut, editor),
        KeyBinding::new("secondary-v", Paste, editor),
        KeyBinding::new("secondary-a", SelectAll, editor),
        // Multiple selections
        KeyBinding::new("secondary-l", SelectLine, editor),
        KeyBinding::new("secondary-shift-l", SplitIntoLines, editor),
        KeyBinding::new("secondary-d", AddNextOccurrence, editor),
        KeyBinding::new("ctrl-shift-up", AddCursorAbove, editor),
        KeyBinding::new("ctrl-shift-down", AddCursorBelow, editor),
        KeyBinding::new("secondary-alt-up", AddCursorAbove, editor),
        KeyBinding::new("secondary-alt-down", AddCursorBelow, editor),
        KeyBinding::new("escape", Cancel, editor),
        // Files and view
        KeyBinding::new("secondary-s", Save, editor),
        KeyBinding::new("secondary-shift-s", SaveAs, editor),
        KeyBinding::new("secondary-shift-v", TogglePreview, workspace),
        KeyBinding::new("ctrl-g", GoToLine, workspace),
        KeyBinding::new("secondary-e", UseSelectionForFind, editor),
        // Workspace
        // When an action has several bindings, the last one is shown in
        // menus and the command palette.
        KeyBinding::new("secondary-p", ToggleFileFinder, workspace),
        KeyBinding::new("secondary-t", ToggleFileFinder, workspace),
        KeyBinding::new("secondary-shift-f", ToggleProjectSearch, workspace),
        KeyBinding::new("secondary-f", Find, workspace),
        KeyBinding::new("f3", FindNext, workspace),
        KeyBinding::new("shift-f3", FindPrevious, workspace),
        KeyBinding::new("secondary-b", ToggleSidebar, workspace),
        KeyBinding::new("secondary-w", CloseTab, workspace),
        KeyBinding::new("secondary-shift-t", ReopenClosedTab, workspace),
        KeyBinding::new("secondary-shift-]", NextTab, workspace),
        KeyBinding::new("secondary-shift-[", PreviousTab, workspace),
        KeyBinding::new("secondary-alt-right", NextTab, workspace),
        KeyBinding::new("secondary-alt-left", PreviousTab, workspace),
        KeyBinding::new("ctrl-tab", NextTab, workspace),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, workspace),
        KeyBinding::new("secondary-1", ActivateTab1, workspace),
        KeyBinding::new("secondary-2", ActivateTab2, workspace),
        KeyBinding::new("secondary-3", ActivateTab3, workspace),
        KeyBinding::new("secondary-4", ActivateTab4, workspace),
        KeyBinding::new("secondary-5", ActivateTab5, workspace),
        KeyBinding::new("secondary-6", ActivateTab6, workspace),
        KeyBinding::new("secondary-7", ActivateTab7, workspace),
        KeyBinding::new("secondary-8", ActivateTab8, workspace),
        KeyBinding::new("secondary-9", ActivateTab9, workspace),
        KeyBinding::new("secondary-shift-e", RevealInSidebar, workspace),
        KeyBinding::new("secondary-alt-r", RevealInFinder, workspace),
        KeyBinding::new("secondary-alt-c", CopyPath, workspace),
        KeyBinding::new("secondary-alt-shift-c", CopyRelativePath, workspace),
        KeyBinding::new("secondary-shift-p", ToggleCommandPalette, workspace),
        KeyBinding::new("secondary-k", ToggleCommandPalette, workspace),
        KeyBinding::new("alt-z", ToggleWordWrap, workspace),
        KeyBinding::new("f12", GoToDefinition, workspace),
        KeyBinding::new("shift-f12", FindReferences, workspace),
        KeyBinding::new("secondary-r", GoToSymbol, workspace),
        KeyBinding::new("secondary-shift-r", GoToProjectSymbol, workspace),
        KeyBinding::new("f2", RenameSymbol, workspace),
        KeyBinding::new("secondary-shift-m", ShowProblems, workspace),
        KeyBinding::new("f8", NextProblem, workspace),
        KeyBinding::new("shift-f8", PreviousProblem, workspace),
        KeyBinding::new("secondary-i", ShowHover, editor),
        KeyBinding::new("ctrl-space", ShowCompletions, editor),
        KeyBinding::new("secondary-.", ShowCodeActions, editor),
        KeyBinding::new("ctrl-shift--", NavigateForward, workspace),
        // Ctrl+- is font size off the Mac.
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "ctrl--"
            } else {
                "ctrl-alt--"
            },
            NavigateBack,
            workspace,
        ),
        // App
        KeyBinding::new("secondary-o", Open, None),
        KeyBinding::new("secondary-shift-o", OpenFolder, None),
        KeyBinding::new("secondary-n", NewFile, None),
        KeyBinding::new("secondary-shift-n", NewWindow, None),
        KeyBinding::new("secondary-shift-w", CloseWindow, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-+", IncreaseFontSize, None),
        KeyBinding::new("secondary-shift-=", IncreaseFontSize, None),
        KeyBinding::new("secondary-=", IncreaseFontSize, None),
        KeyBinding::new("secondary--", DecreaseFontSize, None),
        KeyBinding::new("secondary-0", ResetFontSize, None),
    ]);
    // While a completion snippet is being filled in, then while a menu
    // is open. Bound after the plain editor keys so these win; the menu
    // keys win over the snippet's Tab.
    let snippet = Some("Editor && in_snippet");
    let menu = Some("Editor && showing_menu");
    cx.bind_keys([
        KeyBinding::new("tab", NextSnippetStop, snippet),
        KeyBinding::new("shift-tab", PrevSnippetStop, snippet),
        KeyBinding::new("up", MenuUp, menu),
        KeyBinding::new("down", MenuDown, menu),
        KeyBinding::new("pageup", MenuPageUp, menu),
        KeyBinding::new("pagedown", MenuPageDown, menu),
        KeyBinding::new("enter", MenuConfirm, menu),
        KeyBinding::new("tab", MenuConfirm, menu),
    ]);
    // Painting on an image. Bound last: these win over the font size keys
    // and the editor's undo and save while a picture has focus.
    let image = Some("ImagePreview");
    cx.bind_keys([
        KeyBinding::new("h", HandTool, image),
        KeyBinding::new("b", PencilTool, image),
        KeyBinding::new("p", PencilTool, image),
        KeyBinding::new("e", EraserTool, image),
        KeyBinding::new("g", FillTool, image),
        KeyBinding::new("i", EyedropperTool, image),
        KeyBinding::new("l", LineTool, image),
        KeyBinding::new("r", RectangleTool, image),
        KeyBinding::new("o", EllipseTool, image),
        KeyBinding::new("[", SmallerBrush, image),
        KeyBinding::new("]", BiggerBrush, image),
        KeyBinding::new("secondary-+", ZoomIn, image),
        KeyBinding::new("secondary-shift-=", ZoomIn, image),
        KeyBinding::new("secondary-=", ZoomIn, image),
        KeyBinding::new("secondary--", ZoomOut, image),
        KeyBinding::new("secondary-0", ZoomToFit, image),
        KeyBinding::new("secondary-z", Undo, image),
        KeyBinding::new("secondary-shift-z", Redo, image),
        KeyBinding::new("secondary-y", Redo, image),
        KeyBinding::new("secondary-s", Save, image),
        KeyBinding::new("secondary-shift-s", SaveAs, image),
        KeyBinding::new("escape", Cancel, image),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-left", MoveLineStart, editor),
        KeyBinding::new("cmd-right", MoveLineEnd, editor),
        KeyBinding::new("cmd-shift-left", SelectLineStart, editor),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, editor),
        KeyBinding::new("ctrl-a", MoveLineStart, editor),
        KeyBinding::new("ctrl-e", MoveLineEnd, editor),
        KeyBinding::new("ctrl-k", DeleteToLineEnd, editor),
        KeyBinding::new("ctrl-cmd-g", SelectAllOccurrences, editor),
        KeyBinding::new("ctrl-cmd-up", MoveLineUp, editor),
        KeyBinding::new("ctrl-cmd-down", MoveLineDown, editor),
        KeyBinding::new("fn-backspace", Delete, editor),
        KeyBinding::new("cmd-g", FindNext, workspace),
        KeyBinding::new("cmd-shift-g", FindPrevious, workspace),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("alt-f3", SelectAllOccurrences, editor),
        KeyBinding::new("alt-up", MoveLineUp, editor),
        KeyBinding::new("alt-down", MoveLineDown, editor),
    ]);
}
