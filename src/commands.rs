//! Command registry: every action has a name (shown in menus and the command
//! palette), a category (menu) and an optional keyboard shortcut.

use egui::{Key, KeyboardShortcut, Modifiers};

macro_rules! commands {
    ($( $id:ident => ($name:expr, $cat:expr, $sc:expr) ),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum Cmd { $($id),* }
        pub const ALL: &[Cmd] = &[$(Cmd::$id),*];
        impl Cmd {
            pub fn name(self) -> &'static str { match self { $(Cmd::$id => $name),* } }
            pub fn category(self) -> &'static str { match self { $(Cmd::$id => $cat),* } }
            pub fn shortcut(self) -> Option<KeyboardShortcut> { match self { $(Cmd::$id => $sc),* } }
        }
    };
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers { shift: true, ..Modifiers::COMMAND };
const CMD_ALT: Modifiers = Modifiers { alt: true, ..Modifiers::COMMAND };
const ALT: Modifiers = Modifiers::ALT;
const ALT_SHIFT: Modifiers = Modifiers { alt: true, shift: true, ..Modifiers::NONE };
const SHIFT: Modifiers = Modifiers::SHIFT;
const NONE: Modifiers = Modifiers::NONE;

const fn ks(m: Modifiers, k: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(m, k))
}

commands! {
    // ---- File ----
    NewFile => ("New File", "File", ks(CMD, Key::N)),
    OpenFile => ("Open File…", "File", ks(CMD, Key::O)),
    OpenFolder => ("Open Folder…", "File", ks(CMD_ALT, Key::O)),
    CloseFolder => ("Close Folder", "File", None),
    GoToFile => ("Go to File…", "File", ks(CMD, Key::P)),
    SaveFile => ("Save File", "File", ks(CMD, Key::S)),
    SaveAs => ("Save File As…", "File", ks(CMD_ALT, Key::S)),
    SaveAll => ("Save All Files", "File", ks(CMD_SHIFT, Key::S)),
    SaveVisibleAs => ("Save Visible Data As…", "File", None),
    SaveParams => ("Set Parameters for File Save", "File", None),
    Reload => ("Reload File", "File", None),
    ReloadParams => ("Reload File with Different Parameters", "File", None),
    ToggleReadOnly => ("Toggle Read-Only", "File", None),
    ImportJsonXml => ("Import From JSON or XML…", "File", None),
    ExportJsonXml => ("Export To JSON or XML…", "File", None),
    ExportPdf => ("Export to PDF…", "File", None),
    Print => ("Print…", "File", ks(CMD_ALT, Key::P)),
    DuplicateFile => ("Duplicate File…", "File", None),
    RenameFile => ("Rename File…", "File", None),
    MoveFile => ("Move File…", "File", None),
    DeleteFile => ("Delete File", "File", None),
    ShowInFileSystem => ("Show File in File System", "File", None),
    OpenInTextEditor => ("Open File in Text Editor", "File", None),
    OpenTerminal => ("Open Terminal to File's Directory", "File", None),
    CopyFileName => ("Copy File Name to Clipboard", "File", None),
    CopyFilePath => ("Copy File Path to Clipboard", "File", None),
    CopyFolderPath => ("Copy Folder Path to Clipboard", "File", None),
    TabLeft => ("Switch to Tab on the Left", "File", ks(CMD_ALT, Key::ArrowLeft)),
    TabRight => ("Switch to Tab on the Right", "File", ks(CMD_ALT, Key::ArrowRight)),
    CloseFile => ("Close File", "File", ks(CMD, Key::W)),
    CloseAll => ("Close All Files", "File", None),
    CloseOthers => ("Close All Files Except Current", "File", None),
    Exit => ("Exit", "File", None),

    // ---- Edit ----
    Undo => ("Undo", "Edit", ks(CMD, Key::Z)),
    Redo => ("Redo", "Edit", ks(CMD, Key::Y)),
    Copy => ("Copy Cell(s)", "Edit", None),
    CopyWithHeader => ("Copy Cell(s) with Header", "Edit", None),
    Cut => ("Cut Cell(s)", "Edit", None),
    Paste => ("Paste Cell(s)", "Edit", None),
    Clear => ("Clear Cell Contents", "Edit", None),
    EditCell => ("Edit Selected Cell", "Edit", None),
    Find => ("Find…", "Edit", ks(CMD, Key::F)),
    FindNext => ("Find Next Match", "Edit", ks(NONE, Key::F3)),
    FindPrev => ("Find Previous Match", "Edit", ks(SHIFT, Key::F3)),
    FindAll => ("Find All Matches (count)", "Edit", None),
    Replace => ("Replace…", "Edit", ks(CMD, Key::H)),
    ReplaceNext => ("Replace Next Match", "Edit", None),
    ReplaceAll => ("Replace All Matches", "Edit", None),

    // ---- Select / navigate ----
    SelectAll => ("Select All", "Select", None),
    SelectRows => ("Select Entire Row(s)", "Select", ks(CMD_SHIFT, Key::R)),
    SelectCols => ("Select Entire Column(s)", "Select", ks(CMD_SHIFT, Key::C)),
    SelectTo => ("Select From Main Cell To…", "Select", None),
    SelectDupRows => ("Select Duplicate Row(s)", "Select", None),
    SelectDupRowsSelCols => ("Select Duplicate Rows (Based on Selected Columns Only)", "Select", None),
    SelectDupCols => ("Select Duplicate Column(s)", "Select", None),
    GoToCell => ("Go to Cell…", "Select", ks(CMD, Key::G)),
    GoTop => ("Go to Top", "Select", ks(CMD, Key::Home)),
    GoBottom => ("Go to Bottom", "Select", ks(CMD, Key::End)),
    GoFarLeft => ("Go to Far Left", "Select", ks(CMD_ALT, Key::Home)),
    GoFarRight => ("Go to Far Right", "Select", ks(CMD_ALT, Key::End)),

    // ---- Insert ----
    InsertRowsBefore => ("Insert Row(s) Before", "Insert", ks(ALT, Key::R)),
    InsertRowsAfter => ("Insert Row(s) After", "Insert", ks(CMD, Key::Enter)),
    InsertRowsMulti => ("Insert Row(s) Before Multiple Times…", "Insert", None),
    InsertColsBefore => ("Insert Column(s) Before", "Insert", ks(ALT, Key::C)),
    InsertColsAfter => ("Insert Column(s) After", "Insert", ks(CMD_SHIFT, Key::Enter)),
    InsertColsMulti => ("Insert Column(s) Before Multiple Times…", "Insert", None),
    InsertCellsDown => ("Insert Cells, Move Rest of Column Down", "Insert", None),
    InsertCellsRight => ("Insert Cells, Move Rest of Row Right", "Insert", ks(ALT, Key::Enter)),
    AddIdColumn => ("Add or Update ID Column", "Insert", None),

    // ---- Remove ----
    RemoveRows => ("Remove Row(s)", "Remove", ks(CMD_SHIFT, Key::K)),
    RemoveCols => ("Remove Column(s)", "Remove", ks(CMD_SHIFT, Key::L)),
    RemoveCellsUp => ("Remove Selected Cells, Move Rest of Column Up", "Remove", None),
    RemoveCellsLeft => ("Remove Selected Cells, Move Rest of Row Left", "Remove", ks(ALT, Key::Backspace)),
    RemoveDupRows => ("Remove Duplicate Row(s)", "Remove", None),
    RemoveDupRowsSelCols => ("Remove Duplicate Rows (Based on Selected Columns Only)", "Remove", None),
    RemoveDupCols => ("Remove Duplicate Column(s)", "Remove", None),
    RemoveDupColsSelRows => ("Remove Duplicate Columns (Based on Selected Rows Only)", "Remove", None),
    RemoveEmptyRows => ("Remove Empty Row(s)", "Remove", None),
    RemoveEmptyRowsSelCols => ("Remove Empty Rows (Based on Selected Columns Only)", "Remove", None),
    RemoveEmptyCols => ("Remove Empty Column(s)", "Remove", None),
    RemoveEmptyColsSelRows => ("Remove Empty Columns (Based on Selected Rows Only)", "Remove", None),
    RemoveExtraneous => ("Remove Extraneous Rows and Columns", "Remove", None),
    RemoveFilteredHidden => ("Remove All Filtered Out and Hidden Data", "Remove", None),

    // ---- Duplicate / move ----
    DuplicateRows => ("Duplicate Row(s)", "Move", ks(CMD_ALT, Key::R)),
    DuplicateRowsMulti => ("Duplicate Row(s) Multiple Times…", "Move", None),
    DuplicateCols => ("Duplicate Column(s)", "Move", ks(CMD_ALT, Key::C)),
    DuplicateColsMulti => ("Duplicate Column(s) Multiple Times…", "Move", None),
    DupDown => ("Duplicate Selected Cell Contents Down", "Move", ks(CMD_ALT, Key::ArrowDown)),
    DupUp => ("Duplicate Selected Cell Contents Up", "Move", ks(CMD_ALT, Key::ArrowUp)),
    MoveCellUp => ("Move Cell Contents Up", "Move", ks(ALT, Key::ArrowUp)),
    MoveCellDown => ("Move Cell Contents Down", "Move", ks(ALT, Key::ArrowDown)),
    MoveCellLeft => ("Move Cell Contents Left", "Move", ks(ALT, Key::ArrowLeft)),
    MoveCellRight => ("Move Cell Contents Right", "Move", ks(ALT, Key::ArrowRight)),
    MoveRowsUp => ("Move Row(s) Up", "Move", ks(ALT_SHIFT, Key::ArrowUp)),
    MoveRowsDown => ("Move Row(s) Down", "Move", ks(ALT_SHIFT, Key::ArrowDown)),
    MoveColsLeft => ("Move Column(s) Left", "Move", ks(ALT_SHIFT, Key::ArrowLeft)),
    MoveColsRight => ("Move Column(s) Right", "Move", ks(ALT_SHIFT, Key::ArrowRight)),
    MoveRowsAbove => ("Move Row(s) Just Above Row…", "Move", None),
    MoveColsLeftOf => ("Move Column(s) to the Left of Column…", "Move", None),

    // ---- Sort ----
    SortColAsc => ("Sort Selected Column - Ascending", "Sort", None),
    SortColDesc => ("Sort Selected Column - Descending", "Sort", None),
    SortRowAsc => ("Sort Selected Row - Ascending", "Sort", None),
    SortRowDesc => ("Sort Selected Row - Descending", "Sort", None),
    SortCellsVAsc => ("Sort Selected Cells Vertically - Ascending", "Sort", None),
    SortCellsVDesc => ("Sort Selected Cells Vertically - Descending", "Sort", None),
    SortCellsHAsc => ("Sort Selected Cells Horizontally - Ascending", "Sort", None),
    SortCellsHDesc => ("Sort Selected Cells Horizontally - Descending", "Sort", None),

    // ---- Filter / hide ----
    AddFilter => ("Add Filter…", "Filter", ks(CMD_SHIFT, Key::F)),
    FilterSelCol => ("Filter Selected Column…", "Filter", None),
    FilterSelContents => ("Filter Selected Column(s) with Selected Cell Contents", "Filter", None),
    FilterValues => ("Select Values in Selected Column to Filter", "Filter", None),
    ViewFilters => ("View/Edit Filter(s)", "Filter", None),
    RefreshFilters => ("Refresh Filters", "Filter", None),
    RemoveFilters => ("Remove All Filters", "Filter", None),
    HideRows => ("Hide Selected Row(s)", "Filter", None),
    HideCols => ("Hide Selected Column(s)", "Filter", None),
    HideAllButRows => ("Hide All But Selected Row(s)", "Filter", None),
    HideAllButCols => ("Hide All But Selected Column(s)", "Filter", None),
    HideEmptyRows => ("Hide Empty Row(s)", "Filter", None),
    HideEmptyCols => ("Hide Empty Column(s)", "Filter", None),
    ShowAllRows => ("Show All Hidden Row(s)", "Filter", None),
    ShowAllCols => ("Show All Hidden Column(s)", "Filter", None),

    // ---- Transform ----
    Transform => ("Transform…", "Transform", None),
    TransposeTable => ("Transpose Table", "Transform", ks(CMD_ALT, Key::T)),

    // ---- Headers ----
    ToggleHeader => ("Toggle Header Row", "Headers", None),
    SetHeaderRow => ("Set Header Rows to Selected Row", "Headers", None),
    RemoveHeader => ("Remove Header Rows", "Headers", None),

    // ---- Analysis ----
    OpenAnalysis => ("Open File Analysis Window", "Analysis", None),
    StatsAll => ("Statistics - All Columns", "Analysis", None),
    StatsSelCols => ("Statistics - Selected Columns", "Analysis", None),
    StatsSelCells => ("Statistics - Selected Cells", "Analysis", None),
    StatsSelCellsByCols => ("Statistics - Selected Cells by Columns", "Analysis", None),
    ColAnalysisAll => ("Column Analysis - All Columns", "Analysis", None),
    ColAnalysisSel => ("Column Analysis - Selected Columns", "Analysis", None),
    UniqueValues => ("Unique Values - Selected Column", "Analysis", None),
    Pivot => ("Pivot Table - Selected Columns", "Analysis", None),
    ColumnLookup => ("Column Lookup…", "Analysis", None),

    // ---- View ----
    CommandPalette => ("Command Launcher", "View", ks(CMD_SHIFT, Key::P)),
    Preferences => ("Edit Preferences", "View", ks(CMD, Key::Comma)),
    ToggleSidebar => ("Toggle Sidebar", "View", ks(CMD_SHIFT, Key::B)),
    ToggleMetadata => ("Toggle File Metadata Bar", "View", ks(CMD_SHIFT, Key::D)),
    ToggleEditorPane => ("Toggle Editor Pane", "View", ks(CMD_SHIFT, Key::E)),
    ToggleRowLabels => ("Toggle Row Labels", "View", None),
    ToggleColLabels => ("Toggle Column Labels", "View", None),
    SetColWidth => ("Set Selected Column Width(s)…", "View", None),
    AutofitCols => ("Auto-Fit Selected Column Width(s)", "View", None),
    ZoomIn => ("Increase Text Size", "View", ks(CMD, Key::Equals)),
    ZoomOut => ("Decrease Text Size", "View", ks(CMD, Key::Minus)),
    ThemeLight => ("Theme: Light", "View", None),
    ThemeDark => ("Theme: Dark", "View", None),
    ThemeDracula => ("Theme: Dracula", "View", None),
    ThemeSolDark => ("Theme: Solarized Dark", "View", None),
    ThemeSolLight => ("Theme: Solarized Light", "View", None),

    // ---- Web ----
    FollowLinks => ("Follow Selected Hyperlink(s)", "Web", ks(CMD, Key::R)),
    WebSearch => ("Web Search for Selected Terms", "Web", None),
    MapSearch => ("Web Search for Selected Places in Map", "Web", None),
    EmailAddresses => ("Email Selected Email Addresses", "Web", None),

    // ---- Help ----
    About => ("About ficsv", "Help", None),
}

/// Commands that edit the grid and must not fire while typing in a text field.
pub fn is_grid_only(c: Cmd) -> bool {
    matches!(c.category(), "Insert" | "Remove" | "Move" | "Sort" | "Select" | "Transform")
        || matches!(c, Cmd::Undo | Cmd::Redo | Cmd::FollowLinks)
}

pub fn shortcut_text(ctx: &egui::Context, c: Cmd) -> String {
    c.shortcut().map(|s| ctx.format_shortcut(&s)).unwrap_or_default()
}

pub const MENUS: &[&str] = &[
    "File", "Edit", "Select", "Insert", "Remove", "Move", "Sort", "Filter", "Transform", "Headers", "Analysis", "View", "Web", "Help",
];

/// Simple fuzzy match: all query chars appear in order. Returns a score (higher = better).
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut prev_match = false;
    for qc in query.to_lowercase().chars() {
        if qc == ' ' {
            continue;
        }
        let mut found = false;
        while ti < t.len() {
            let hit = t[ti] == qc;
            ti += 1;
            if hit {
                score += if prev_match { 5 } else { 1 };
                if ti == 1 || !t[ti - 2].is_alphanumeric() {
                    score += 8;
                }
                found = true;
                prev_match = true;
                break;
            }
            prev_match = false;
        }
        if !found {
            return None;
        }
    }
    Some(score * 100 - t.len() as i32)
}
