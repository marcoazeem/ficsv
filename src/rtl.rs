//! Right-to-left text support.
//!
//! egui 0.35 shapes text with HarfRust (a HarfBuzz port): each run of glyphs from one
//! font is shaped with its script's direction, so Arabic letters get joined and the
//! characters inside an Arabic/Hebrew run come out right-to-left. What egui does *not*
//! do is the Unicode Bidi Algorithm across runs (word order, numbers and Latin text
//! mixed into RTL text). `visual()` fills that gap: it reorders runs into visual order
//! but keeps the characters inside each RTL run in logical order for the shaper.
//! We also register system fonts that contain Arabic, Hebrew and CJK glyphs.
//! Stored data is never modified — this only affects how text is drawn.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use unicode_bidi::BidiInfo;

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF      // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic Ext
        | 0xFB1D..=0xFDFF    // Hebrew & Arabic presentation forms A
        | 0xFE70..=0xFEFF    // Arabic presentation forms B
        | 0x10800..=0x10FFF  // historic RTL scripts
        | 0x1E800..=0x1EFFF) // Adlam, Arabic math, …
}

/// Joiners used inside Persian/Arabic words; they belong to the surrounding run.
fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}')
}

/// Bidi-mirrored characters: in right-to-left text "(" is drawn as ")" and so on.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        '≤' => '≥',
        '≥' => '≤',
        '⁽' => '⁾',
        '⁾' => '⁽',
        '₍' => '₎',
        '₎' => '₍',
        c => c,
    }
}

/// Fast check used on every visible cell.
pub fn has_rtl(s: &str) -> bool {
    // Every RTL code point is outside ASCII, so pure-ASCII text skips the scan.
    !s.is_ascii() && s.chars().any(is_rtl_char)
}

/// True when the first strongly-directional character is right-to-left
/// (used to right-align a cell, like a spreadsheet would).
pub fn is_rtl_paragraph(s: &str) -> bool {
    for c in s.chars() {
        if is_rtl_char(c) {
            return true;
        }
        if c.is_alphabetic() {
            return false;
        }
    }
    false
}

/// Text arranged for egui: runs in visual order, RTL runs in logical order
/// (egui's shaper reverses and joins them). Unchanged when there is no RTL text.
pub fn visual(s: &str) -> Cow<'_, str> {
    if !has_rtl(s) {
        return Cow::Borrowed(s);
    }
    let info = BidiInfo::new(s, None);
    let mut reordered = String::with_capacity(s.len());
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let text = &s[run.clone()];
            if levels[run.start].is_rtl() {
                // Right-to-left run: reverse it and mirror brackets (Unicode rule L4).
                reordered.extend(text.chars().rev().map(mirror));
            } else {
                reordered.push_str(text);
            }
        }
    }
    // The bidi pass fully reversed each RTL run; flip the letters of each run back.
    let chars: Vec<char> = reordered.chars().collect();
    let mut out = String::with_capacity(reordered.len());
    let mut i = 0;
    while i < chars.len() {
        if is_rtl_char(chars[i]) {
            let start = i;
            while i < chars.len() && (is_rtl_char(chars[i]) || is_joiner(chars[i])) {
                i += 1;
            }
            out.extend(chars[start..i].iter().rev());
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(ficsv_bundled_faruma)]
const BUNDLED_FARUMA: Option<&[u8]> = Some(include_bytes!(env!("FICSV_FARUMA_PATH")));
#[cfg(not(ficsv_bundled_faruma))]
const BUNDLED_FARUMA: Option<&[u8]> = None;

/// Locate the Faruma font (the standard Dhivehi/Thaana typeface).
/// `override_path` (from Preferences) wins when it points at an existing file.
pub fn find_faruma(override_path: &str) -> Option<PathBuf> {
    let o = override_path.trim();
    if !o.is_empty() && Path::new(o).is_file() {
        return Some(PathBuf::from(o));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend(home.iter().map(|h| h.join("Library/Fonts")));
        dirs.extend(["/Library/Fonts", "/System/Library/Fonts", "/System/Library/Fonts/Supplemental"].map(PathBuf::from));
    } else if cfg!(windows) {
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft\\Windows\\Fonts"));
        }
        let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        dirs.push(windir.join("Fonts"));
    } else {
        dirs.extend(home.iter().flat_map(|h| [h.join(".local/share/fonts"), h.join(".fonts")]));
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
    }
    fn scan(dir: &Path, depth: usize) -> Option<PathBuf> {
        let mut subdirs = Vec::new();
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                subdirs.push(p);
                continue;
            }
            let name = p.file_name()?.to_string_lossy().to_lowercase();
            let ext_ok = name.ends_with(".ttf") || name.ends_with(".otf");
            // "Faruma.otf", "faruma.ttf", "MV Faruma.ttf" … but never MV Boli.
            if ext_ok && name.contains("faruma") {
                return Some(p);
            }
        }
        if depth < 3 {
            for d in subdirs {
                if let Some(f) = scan(&d, depth + 1) {
                    return Some(f);
                }
            }
        }
        None
    }
    dirs.iter().find_map(|d| scan(d, 0))
}

/// Faruma font data: Preferences path > installed system font > copy bundled in the app.
pub fn faruma_bytes(override_path: &str, allow_system: bool) -> Option<Vec<u8>> {
    let system = if allow_system { find_faruma(override_path) } else { None };
    system.and_then(|p| std::fs::read(p).ok()).or_else(|| BUNDLED_FARUMA.map(<[u8]>::to_vec))
}

/// Register fallback fonts: Faruma first (so Thaana/Dhivehi is always drawn with Faruma,
/// never MV Boli or another Thaana font), then fonts with Arabic, Hebrew and CJK coverage.
/// Returns a warning when Faruma could not be found.
pub fn install_fonts(ctx: &egui::Context, faruma_override: &str) -> Option<String> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            // Arial has OpenType Arabic + Hebrew shaping tables; Arial Unicode adds wide coverage.
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ]
    } else if cfg!(windows) {
        // Deliberately excludes mvboli.ttf (MV Boli): Dhivehi must use Faruma.
        &[
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
            "C:\\Windows\\Fonts\\tahoma.ttf",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\YuGothM.ttc",
            "C:\\Windows\\Fonts\\malgun.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ]
    };
    let mut fonts = egui::FontDefinitions::default();
    let add = |fonts: &mut egui::FontDefinitions, name: String, bytes: Vec<u8>| {
        fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push(name.clone());
        }
    };
    let faruma_bytes = faruma_bytes(faruma_override, true);
    let warning = match faruma_bytes {
        Some(bytes) => {
            // Pushed before any other fallback: egui picks the first font that has a glyph,
            // so every Thaana character is drawn with Faruma.
            add(&mut fonts, "faruma".into(), bytes);
            None
        }
        None => Some("Faruma font not found — Dhivehi text needs Faruma (install it or set its path in Preferences)".into()),
    };
    for (i, path) in candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            add(&mut fonts, format!("fallback-{i}"), bytes);
        }
    }
    ctx.set_fonts(fonts);
    warning
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_faruma_draws_thaana() {
        // Simulates a PC without Faruma installed: only the bundled copy is used.
        let bytes = faruma_bytes("", false).expect("Faruma must be bundled (assets/fonts/Faruma.otf)");
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("faruma".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push("faruma".into());
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.begin_pass(egui::RawInput::default());
        let ok = ctx.fonts_mut(|f| "ދިވެހި ބަސް".chars().filter(|c| !c.is_whitespace()).all(|c| f.has_glyph(&egui::FontId::proportional(13.0), c)));
        let _ = ctx.end_pass();
        assert!(ok, "every Thaana character must have a Faruma glyph");
    }

    #[test]
    fn ascii_untouched() {
        assert!(matches!(visual("hello, world"), Cow::Borrowed(_)));
        assert!(!has_rtl("café"));
    }

    #[test]
    fn single_rtl_word_kept_logical() {
        // One run: the shaper handles direction, so the text passes through.
        assert_eq!(visual("שלום"), "שלום");
        assert_eq!(visual("سلام"), "سلام");
    }

    #[test]
    fn rtl_word_order_reversed() {
        // Two RTL words: first logical word must appear on the right.
        assert_eq!(visual("דוד כהן"), "כהן דוד");
        assert_eq!(visual("محمد علي"), "علي محمد");
        assert!(is_rtl_paragraph("שלום abc"));
        assert!(!is_rtl_paragraph("abc שלום"));
    }

    #[test]
    fn mixed_direction() {
        // LTR paragraph containing an RTL word and a number.
        // Per UBA rule W2 a number after Arabic letters joins the Arabic run.
        assert_eq!(visual("Order: طلب 42"), "Order: 42 طلب");
        assert_eq!(visual("Order: שלום 42"), "Order: 42 שלום");
        // RTL paragraph with a number: number stays LTR and sits on the left.
        assert_eq!(visual("מחיר 123"), "123 מחיר");
        // Persian with a zero-width non-joiner stays one run.
        assert_eq!(visual("می\u{200C}خواهم"), "می\u{200C}خواهم");
        // Brackets inside RTL text are mirrored.
        assert_eq!(visual("א (ב)"), "(ב) א");
        assert_eq!(visual("ރޯމް (މެއި 4) -"), "- (4 މެއި) ރޯމް");
        // Brackets in LTR text are untouched.
        assert_eq!(visual("Order (طلب)"), "Order (طلب)");
        // Dhivehi (Thaana) is RTL too.
        assert!(has_rtl("ދިވެހި"));
        assert_eq!(visual("ދިވެހި ބަސް"), "ބަސް ދިވެހި");
    }
}
