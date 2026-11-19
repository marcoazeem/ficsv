# FI CSV

A fast desktop CSV/TSV editor for very large files, written in Rust (egui). Runs on macOS and Windows (and Linux).
Its feature set follows [Modern CSV](https://www.moderncsv.com/), including the Premium and Premium Business features.

## Why it's fast

* The file is **memory-mapped**, never fully loaded. A background thread scans it once (quote-aware, using `memchr`)
  and records where each row starts — about 0.1–0.5 s for 5 M rows / 270 MB. The first screen appears right away.
* Only the rows on screen are parsed. Scrolling tracks a row index, not pixels, so 100 M+ rows scroll smoothly.
* Editing works at any size, with **no read-only mode required**: rows, columns and edits are thin layers over the file.
  Insert, delete, move and sort only reorder ids. Saving streams unchanged rows straight from disk.
* Filter, sort, find and dedupe run on all cores (rayon). Filtering 5 M rows takes about 30 ms; a numeric sort about 0.2 s.

## Features

| Area | What's there |
|---|---|
| Files | Tabs, drag & drop (files or folders), recent files, folder sidebar, Go to File (fuzzy), Save / Save As / Save All / **Save Visible Data As**, reload, reload with different parameters, auto-reload on external change (refresh / ask / ignore), read-only toggle, duplicate / rename / move / delete, show in Finder/Explorer, open in text editor, open terminal, copy name/path |
| Parsing | Delimiter auto-detect (`, ; tab \| space`, ASV), per-extension delimiters, RFC 4180 or "delimiter only", CRLF/LF, BOM. Encodings: UTF-8, UTF-16LE/BE, Windows-125x, ISO-8859-x, KOI8, Shift-JIS, EUC-JP/KR, GBK/GB18030, Big5 |
| Saving | Choose delimiter, encoding, line ending, quoting (as needed / all / never) and BOM. Unedited files save byte-for-byte |
| Editing | In-place editing, editor pane for multi-line cells, copy/cut/paste (TSV, with header), undo/redo for every operation, insert/remove/move/duplicate rows, columns and cells (incl. shift down/right/up/left), duplicate contents up/down, Add ID column |
| Find | Find / replace with case, regex (`$1`), whole word, whole cell, selection only, highlight matches, replace preview, count matches |
| Sort | Double-click a header, or sort by column / by row / selected cells (stable; numeric or text, empties last) |
| Filter & hide | Multiple filters with the Modern CSV query syntax (below); filter by selected cell contents; hide/show rows and columns, hide empty, hide all but selected; remove filtered-out/hidden data |
| Clean-up | Remove or select duplicate rows/columns (all or based on selection), empty rows/columns, extraneous trailing rows/columns |
| Transforms | Upper/lower/title/camel/underscore case, trim, acronym, keep left/middle/right, zero-fill, remove leading zeros, number format (1,234.5 ⇄ 1.234,5), set decimals, prepend/append, **date/time conversion** (custom strftime, ISO 8601, Unix s/ms, Excel serial), fill series, random ints/floats, UUID v4/v7, timestamp, copy main cell, **transpose** (table or selection), reshape, split/join cells |
| Analysis | File Analysis window: **statistics** (count, sum, mean, variance, std dev, median, min, max, percentiles), **column analysis** (fill rate, lengths, data type), **unique values** with histogram (filter in/out), **pivot tables**; **Column Lookup** (VLOOKUP across open files) |
| Import / export | JSON & XML import (repeating-pattern detection or lossless "preserve structure") and export; **PDF export & print** (page setup, selection only, headers on every page, labels, gridlines, centring, page order, title, page numbers) |
| Web | Follow hyperlinks, web search, map search, email addresses |
| UI | Menus for every command, **command launcher** (⌘⇧P / Ctrl+Shift+P) with fuzzy search, context menu, metadata bar, status bar with sum/avg of selection, themes (Light, Dark, Dracula, Solarized Dark/Light), text size, row height, shading, cross-hair highlight, letters or numbers for columns, row/column index base |
| Languages | **Right-to-left** Arabic, Hebrew, Persian, Urdu and **Dhivehi (Thaana)**: bidi reordering + shaping, RTL cells right-aligned. Dhivehi always uses **Faruma**, never MV Boli: a Faruma path set in Preferences is used first, then an installed Faruma, otherwise the copy bundled in the app (`assets/fonts/Faruma.otf`, © The President's Office, Maldives). CJK fallback fonts are loaded too |

### Filter syntax

`foo` contains · `=foo` equals · `f*o` wildcard · `!foo` not · `a | b` or · `a & b` and ·
`>5` `<=10` `50+` `50-` `10-20` numbers · `!` not empty · `#regex` whole query as a regex · `\|` `\&` `\*` escapes

## Build

```sh
cargo run --release -- path/to/file.csv     # run
cargo test                                   # 53 tests
cargo test --release perf_big_file -- --ignored --nocapture   # 5M-row benchmark
```

* **macOS app:** `cargo build --release && scripts/bundle-macos.sh` → `dist/FI CSV.app`
* **Windows:** `cargo build --release` on Windows → `target\release\ficsv.exe` (no console window in release builds)
* **Linux:** needs `libxkbcommon-dev libwayland-dev libgl1-mesa-dev libxcb-*-dev` (Debian/Ubuntu names). `cargo build --release && scripts/package-linux.sh`
  → `dist/ficsv-linux/` with the binary, a `.desktop` launcher, the icon and `install.sh` (per-user install into `~/.local`).
* **CI:** `.github/workflows/build.yml` builds Windows x64, macOS arm64/x64 and Linux x64/arm64 on every push and uploads them as artifacts.

## Code map

| File | Role |
|---|---|
| `src/doc.rs` | Memory map, background indexer, CSV field parser, row/column/edit layers, undo |
| `src/jobs.rs` | Background filter, sort, find/replace, dedupe, streaming save |
| `src/grid.rs` | Virtualised table widget |
| `src/app.rs` | Menus, command handlers, dialogs, palette, status bar |
| `src/commands.rs` | Command registry and shortcuts |
| `src/query.rs` | Filter query language |
| `src/transforms.rs` | Cell transforms |
| `src/analysis.rs` | File Analysis window |
| `src/formats.rs` | JSON/XML import/export, PDF writer |
| `src/rtl.rs` | Bidi/RTL display, fallback fonts, Faruma |
| `src/settings.rs` | Preferences + themes (saved to the user config dir) |

## Known limitations

* Selection is a single rectangle. "Select duplicates" / "Add next match" therefore show only the matching rows instead of building a multi-selection.
* Text being edited in an input box (cell editor, editor pane) uses egui's left-to-right caret. RTL text is displayed correctly in the grid, but the caret moves in logical order while you edit.
* On macOS, "Open With…" from Finder isn't wired up yet (drag & drop, the Open dialog and the command line all work).
* Keyboard shortcuts are fixed (not yet user-customisable).

## Updates

FI CSV checks GitHub for a newer release when it starts (turn this off in Preferences, or use **Help ▸ Check for Updates…**).
When one is available it shows **Install & Restart**: it downloads the zip for that machine from the latest
[GitHub Release](../../releases), replaces the app in place and relaunches. To ship an update, bump `version` in
`Cargo.toml`, commit, and push a tag like `v0.2.2` — CI builds the release and every installed copy picks it up.
(Versions before 0.2.1 don't include the updater, so those users need to download once manually.)

## Download (manual)

Prebuilt macOS (Apple Silicon / Intel) and Windows builds are attached to each [GitHub Release](../../releases).
They are not notarized: on macOS right-click **FI CSV.app** → **Open** the first time; on Windows choose **More info → Run anyway** if SmartScreen warns.
