//! Hand-rolled ANSI escape handling: no crate does exactly "split into ratatui
//! runs" for us, and pulling one in for this much surface area isn't worth it.
//!
//! The parser tracks one running [`Style`], applies `CSI ... m` (SGR) parameters
//! to it left to right, and cuts a new [`Run`] each time the style actually
//! changes. Every other escape (cursor moves, OSC titles and hyperlinks, the
//! two-byte C1 shorthands) is recognised well enough to be swallowed whole, but
//! never causes a run boundary. That distinction, "consumed" vs "boundary", is
//! the one thing to keep in mind reading the rest of this file.

use std::iter::Peekable;
use std::str::Chars;

use ratatui::style::{Color, Modifier, Style};

/// One stretch of text that shares a style.
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// Splits one line into styled runs, with the escape sequences removed.
///
/// SGR parameters accumulate onto a running [`Style`] instead of replacing it,
/// so `ESC[1m` then later `ESC[32m` combine into bold-green rather than the
/// second escape erasing the first. Every other escape sequence is stripped
/// without ending the current run. Adjacent runs that end up with equal styles
/// are merged, so plain text always comes back as exactly one run.
pub fn parse(line: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            buf.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                if let (params, Some('m')) = consume_csi(&mut chars) {
                    if !buf.is_empty() {
                        runs.push(Run {
                            text: std::mem::take(&mut buf),
                            style,
                        });
                    }
                    apply_sgr(&mut style, &params);
                }
                // Any other final byte (or none, for an unterminated sequence) is
                // dropped without touching the run in progress.
            }
            Some(']') => {
                chars.next();
                consume_osc(&mut chars);
            }
            Some(_) => {
                // Two-byte escape (ESC 7, ESC c, ...): consume the second byte too.
                chars.next();
            }
            None => {
                // Stray ESC as the last character. Nothing follows to consume.
            }
        }
    }
    if !buf.is_empty() {
        runs.push(Run { text: buf, style });
    }

    merge_adjacent(runs)
}

/// The same text with every escape sequence removed and nothing else changed.
///
/// Built directly on [`parse`] (rather than a second hand-rolled scanner) so the
/// two can never disagree about what counts as an escape sequence.
pub fn strip(s: &str) -> String {
    parse(s).into_iter().map(|run| run.text).collect()
}

/// Consumes a CSI body (everything after `ESC[`): parameter/intermediate bytes
/// in `0x20..=0x3F`, then a final byte in `0x40..=0x7E`. Returns the raw
/// parameter text and the final byte.
///
/// If the string ends, or a byte outside those ranges shows up, first, the
/// sequence is abandoned: whatever was read is dropped as an incomplete
/// escape, but the offending byte itself is left for the caller to process as
/// ordinary text rather than being eaten too.
fn consume_csi(chars: &mut Peekable<Chars>) -> (String, Option<char>) {
    let mut params = String::new();
    while let Some(&c) = chars.peek() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            chars.next();
            return (params, Some(c));
        }
        if ('\u{20}'..='\u{3f}').contains(&c) {
            params.push(c);
            chars.next();
        } else {
            return (params, None);
        }
    }
    (params, None)
}

/// Consumes an OSC body (everything after `ESC]`) up to and including its
/// terminator, BEL or `ESC \`. An OSC 8 hyperlink is two such sequences
/// wrapping plain link text, so stripping only the sequences themselves
/// (never the text between them) is what makes the link text survive.
///
/// A string that ends before a terminator appears is treated as fully
/// consumed: an unterminated OSC has no well-defined end, so there is nothing
/// left over to preserve.
fn consume_osc(chars: &mut Peekable<Chars>) {
    while let Some(c) = chars.next() {
        match c {
            '\u{7}' => return,
            '\u{1b}' if chars.peek() == Some(&'\\') => {
                chars.next();
                return;
            }
            '\u{1b}' => return,
            _ => {}
        }
    }
}

/// Applies one `CSI ... m` parameter list to `style`, left to right, matching
/// how a real terminal folds successive SGR escapes onto its running state.
fn apply_sgr(style: &mut Style, params: &str) {
    let codes: Vec<u32> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();

    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => *style = Style::default(),
            1 => style.add_modifier.insert(Modifier::BOLD),
            2 => style.add_modifier.insert(Modifier::DIM),
            3 => style.add_modifier.insert(Modifier::ITALIC),
            4 => style.add_modifier.insert(Modifier::UNDERLINED),
            7 => style.add_modifier.insert(Modifier::REVERSED),
            9 => style.add_modifier.insert(Modifier::CROSSED_OUT),
            21 => style.add_modifier.remove(Modifier::BOLD),
            22 => {
                style.add_modifier.remove(Modifier::BOLD);
                style.add_modifier.remove(Modifier::DIM);
            }
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),
            n @ 30..=37 => style.fg = Some(base_color(n - 30)),
            38 => {
                let (color, consumed) = extended_color(&codes[i + 1..]);
                style.fg = color.or(style.fg);
                i += consumed;
            }
            39 => style.fg = None,
            n @ 40..=47 => style.bg = Some(base_color(n - 40)),
            48 => {
                let (color, consumed) = extended_color(&codes[i + 1..]);
                style.bg = color.or(style.bg);
                i += consumed;
            }
            49 => style.bg = None,
            n @ 90..=97 => style.fg = Some(bright_color(n - 90)),
            n @ 100..=107 => style.bg = Some(bright_color(n - 100)),
            _ => {}
        }
        i += 1;
    }
}

/// Maps 0..=7 (as used by both the 30..=37 and 40..=47 SGR ranges) to the
/// standard ANSI palette.
fn base_color(n: u32) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

/// Maps 0..=7 (as used by both the 90..=97 and 100..=107 SGR ranges) to the
/// bright ANSI palette.
fn bright_color(n: u32) -> Color {
    match n {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Parses the `5;N` (indexed) or `2;R;G;B` (truecolor) tail that follows a `38`
/// or `48` code. Returns the resolved colour (`None` if the introducer is
/// unrecognised or its parameters run out before the form is complete) and
/// how many extra parameters (beyond the `38`/`48` itself) it consumed.
///
/// A malformed or truncated sequence still reports every parameter it looked
/// at as consumed, so the caller always skips past them: a `38`/`48`
/// introducer commits its trailing parameters to describing a colour, and
/// they must never fall through to being reinterpreted as unrelated SGR
/// codes (e.g. a truncated `38;2;R;G` misread as bold/dim).
fn extended_color(rest: &[u32]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) => match rest.get(1) {
            Some(&n) => (Some(Color::Indexed(n as u8)), 2),
            None => (None, 1),
        },
        Some(2) if rest.len() >= 4 => (
            Some(Color::Rgb(rest[1] as u8, rest[2] as u8, rest[3] as u8)),
            4,
        ),
        Some(2) => (None, rest.len()),
        Some(_) => (None, 1),
        None => (None, 0),
    }
}

/// Splits `text` the same way [`str::lines`] does, but re-asserts at the
/// start of each line whatever [`Style`] was still active at the end of the
/// previous one.
///
/// A real terminal carries SGR state across newlines; parsing each line of a
/// multi-line block independently (as [`parse`] does, by design, for a single
/// line) would otherwise drop that state at every line boundary. Callers that
/// need per-line styling of a block that may span several lines should split
/// with this instead of `str::lines()`.
///
/// When no style is carried in, nothing is prepended, so a plain line comes
/// back byte-identical to what `str::lines()` yields.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut style = Style::default();
    let mut out = Vec::new();
    for segment in text.split_terminator('\n') {
        let content = segment.strip_suffix('\r').unwrap_or(segment);
        let prefix = sgr_prefix(&style);
        out.push(if prefix.is_empty() {
            content.to_string()
        } else {
            prefix + content
        });
        advance_style(&mut style, segment);
    }
    out
}

/// Updates `style` by applying every SGR sequence found in `text`, the same
/// way [`parse`] would while building runs, but without allocating any.
/// Used by [`split_lines`] to carry style across the line it just consumed.
fn advance_style(style: &mut Style, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                if let (params, Some('m')) = consume_csi(&mut chars) {
                    apply_sgr(style, &params);
                }
            }
            Some(']') => {
                chars.next();
                consume_osc(&mut chars);
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
}

/// Serialises `style` back into the SGR sequence that re-establishes it from
/// scratch: a reset followed by whichever colours and modifiers are set.
/// Emitting the reset first makes this absolute rather than relative, so it
/// is safe to prepend to a line regardless of what preceded it.
///
/// `Style::default()` serialises to an empty string: there is nothing to
/// re-establish, and callers rely on that to leave unstyled lines untouched.
fn sgr_prefix(style: &Style) -> String {
    if *style == Style::default() {
        return String::new();
    }
    let mut codes = vec!["0".to_string()];
    if let Some(fg) = style.fg {
        codes.push(color_code(fg, 30, 90, 38));
    }
    if let Some(bg) = style.bg {
        codes.push(color_code(bg, 40, 100, 48));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".to_string());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".to_string());
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        codes.push("3".to_string());
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push("4".to_string());
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        codes.push("7".to_string());
    }
    if style.add_modifier.contains(Modifier::CROSSED_OUT) {
        codes.push("9".to_string());
    }
    format!("\u{1b}[{}m", codes.join(";"))
}

/// The SGR parameter for `color` in the slot `base`/`bright_base`/`extended`
/// identifies (30/90/38 for foreground, 40/100/48 for background): the
/// inverse of [`base_color`], [`bright_color`] and [`extended_color`] combined.
fn color_code(color: Color, base: u32, bright_base: u32, extended: u32) -> String {
    match color {
        Color::Reset => (base + 9).to_string(), // 39/49: reset that channel to its default
        Color::Black => base.to_string(),
        Color::Red => (base + 1).to_string(),
        Color::Green => (base + 2).to_string(),
        Color::Yellow => (base + 3).to_string(),
        Color::Blue => (base + 4).to_string(),
        Color::Magenta => (base + 5).to_string(),
        Color::Cyan => (base + 6).to_string(),
        Color::Gray => (base + 7).to_string(),
        Color::DarkGray => bright_base.to_string(),
        Color::LightRed => (bright_base + 1).to_string(),
        Color::LightGreen => (bright_base + 2).to_string(),
        Color::LightYellow => (bright_base + 3).to_string(),
        Color::LightBlue => (bright_base + 4).to_string(),
        Color::LightMagenta => (bright_base + 5).to_string(),
        Color::LightCyan => (bright_base + 6).to_string(),
        Color::White => (bright_base + 7).to_string(),
        Color::Indexed(n) => format!("{extended};5;{n}"),
        Color::Rgb(r, g, b) => format!("{extended};2;{r};{g};{b}"),
    }
}

/// Collapses adjacent runs that ended up with the same style and drops any run
/// left with no text, so a line with no escapes yields exactly one run.
fn merge_adjacent(runs: Vec<Run>) -> Vec<Run> {
    let mut merged: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(prev) if prev.style == run.style => prev.text.push_str(&run.text),
            _ => merged.push(run),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(runs: &[Run]) -> Vec<&str> {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn plain_text_is_one_default_run() {
        let runs = parse("hello world");
        assert_eq!(texts(&runs), vec!["hello world"]);
        assert_eq!(runs[0].style, Style::default());
    }

    #[test]
    fn simple_colour_sequence_splits_into_runs() {
        let runs = parse("\u{1b}[32mgreen\u{1b}[0m plain");
        assert_eq!(texts(&runs), vec!["green", " plain"]);
        assert_eq!(runs[0].style.fg, Some(Color::Green));
        assert_eq!(runs[1].style, Style::default());
    }

    #[test]
    fn bold_and_colour_combine_across_separate_escapes() {
        let runs = parse("\u{1b}[1m\u{1b}[32mBoldGreen\u{1b}[0m");
        assert_eq!(texts(&runs), vec!["BoldGreen"]);
        assert!(runs[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(runs[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn bold_and_colour_in_one_sgr_sequence() {
        let runs = parse("\u{1b}[1;32mBoldGreen\u{1b}[0m");
        assert_eq!(texts(&runs), vec!["BoldGreen"]);
        assert!(runs[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(runs[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn indexed_256_colour() {
        let runs = parse("\u{1b}[38;5;208mtext");
        assert_eq!(runs[0].style.fg, Some(Color::Indexed(208)));
    }

    #[test]
    fn indexed_256_colour_background() {
        let runs = parse("\u{1b}[48;5;22mtext");
        assert_eq!(runs[0].style.bg, Some(Color::Indexed(22)));
    }

    #[test]
    fn truecolor_foreground_and_background() {
        let runs = parse("\u{1b}[38;2;10;20;30;48;2;1;2;3mtext");
        assert_eq!(runs[0].style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(runs[0].style.bg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn reset_mid_line_clears_style() {
        let runs = parse("\u{1b}[31mred\u{1b}[0mnormal");
        assert_eq!(texts(&runs), vec!["red", "normal"]);
        assert_eq!(runs[0].style.fg, Some(Color::Red));
        assert_eq!(runs[1].style, Style::default());
    }

    #[test]
    fn default_fg_and_bg_reset_individually() {
        let runs = parse("\u{1b}[31;44mtext\u{1b}[39mfg_reset\u{1b}[49mboth_reset");
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].style.fg, Some(Color::Red));
        assert_eq!(runs[0].style.bg, Some(Color::Blue));
        assert_eq!(runs[1].style.fg, None);
        assert_eq!(runs[1].style.bg, Some(Color::Blue));
        assert_eq!(runs[2].style.fg, None);
        assert_eq!(runs[2].style.bg, None);
    }

    #[test]
    fn bright_foreground_and_background() {
        let runs = parse("\u{1b}[91;104mtext");
        assert_eq!(runs[0].style.fg, Some(Color::LightRed));
        assert_eq!(runs[0].style.bg, Some(Color::LightBlue));
    }

    #[test]
    fn unset_bold_via_22() {
        let runs = parse("\u{1b}[1mbold\u{1b}[22mnormal");
        assert_eq!(texts(&runs), vec!["bold", "normal"]);
        assert!(runs[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!runs[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn empty_param_list_means_reset() {
        let runs = parse("\u{1b}[1mbold\u{1b}[mplain");
        assert_eq!(texts(&runs), vec!["bold", "plain"]);
        assert_eq!(runs[1].style, Style::default());
    }

    #[test]
    fn identical_adjacent_styles_are_merged() {
        // The repeated 1m mid-line changes nothing observable, so it must not split the run.
        let runs = parse("\u{1b}[1mbold\u{1b}[1mstillbold\u{1b}[0m");
        assert_eq!(texts(&runs), vec!["boldstillbold"]);
    }

    #[test]
    fn non_sgr_csi_is_dropped_without_a_run_boundary() {
        let runs = parse("\u{1b}[2Kbefore\u{1b}[10;20Hafter");
        assert_eq!(texts(&runs), vec!["beforeafter"]);
    }

    #[test]
    fn two_char_escape_is_stripped() {
        assert_eq!(strip("save\u{1b}7cursor"), "savecursor");
    }

    #[test]
    fn unterminated_csi_at_end_of_string_is_dropped() {
        let runs = parse("before\u{1b}[123");
        assert_eq!(texts(&runs), vec!["before"]);
    }

    #[test]
    fn malformed_csi_interrupted_by_control_byte_keeps_trailing_text() {
        // 0x01 falls outside both the CSI parameter and final-byte ranges, so it
        // aborts the sequence without being consumed, and the text after it survives.
        assert_eq!(strip("before\u{1b}[123\u{1}after"), "before\u{1}after");
    }

    #[test]
    fn stray_esc_at_end_of_line_does_not_panic_or_eat_text() {
        assert_eq!(strip("trailing text\u{1b}"), "trailing text");
    }

    #[test]
    fn osc_hyperlink_is_stripped_but_link_text_survives() {
        let line = "\u{1b}]8;;https://example.com\u{1b}\\click here\u{1b}]8;;\u{1b}\\";
        assert_eq!(strip(line), "click here");
    }

    #[test]
    fn osc_terminated_by_bel() {
        let line = "\u{1b}]0;window title\u{7}after";
        assert_eq!(strip(line), "after");
    }

    #[test]
    fn unterminated_osc_is_dropped() {
        assert_eq!(strip("before\u{1b}]0;no terminator here"), "before");
    }

    #[test]
    fn strip_matches_parse_concatenated_for_sample_lines() {
        let samples = [
            "plain text",
            "\u{1b}[1;32mBoldGreen\u{1b}[0m tail",
            "\u{1b}[38;5;208m256\u{1b}[0m",
            "\u{1b}[48;2;1;2;3mrgb\u{1b}[0m",
            "\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\",
            "unterminated\u{1b}[9",
            "stray\u{1b}",
            "\u{1b}[1mbold\u{1b}[1mstillbold\u{1b}[0m",
            "\u{1b}[2Kbefore\u{1b}[10;20Hafter",
        ];
        for line in samples {
            let expected: String = parse(line).into_iter().map(|r| r.text).collect();
            assert_eq!(strip(line), expected, "mismatch for {line:?}");
        }
    }

    #[test]
    fn strip_and_parse_never_panic_on_pathological_input() {
        let inputs = [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}]",
            "\u{1b}[;;;m",
            "\u{1b}[999999999999999999m",
            "\u{1b}[38;5m",
            "\u{1b}[38;2;1;2m",
            "\u{1b}[38;9m",
            "\u{1b}[?25h",
            "text\u{1b}[1\u{1b}[2\u{1b}[3mtext",
        ];
        for input in inputs {
            let _ = strip(input);
            let _ = parse(input);
        }
    }

    #[test]
    fn truncated_truecolor_yields_no_colour_and_no_spurious_modifiers() {
        // "38;2;1;2" is a truecolor introducer missing its blue component: the
        // whole malformed sequence must be swallowed, not reinterpreted as
        // DIM(2)/BOLD(1)/DIM(2) once the colour parse gives up.
        let runs = parse("\u{1b}[38;2;1;2mtext");
        assert_eq!(runs[0].style.fg, None);
        assert_eq!(runs[0].style, Style::default());
    }

    #[test]
    fn split_lines_matches_str_lines_byte_identical_for_plain_text() {
        let text = "line one\nline two\nline three";
        assert_eq!(split_lines(text), text.lines().collect::<Vec<_>>());
    }

    #[test]
    fn split_lines_matches_str_lines_with_a_trailing_newline() {
        let text = "line one\nline two\n";
        assert_eq!(split_lines(text), text.lines().collect::<Vec<_>>());
    }

    #[test]
    fn split_lines_matches_str_lines_with_crlf() {
        let text = "line one\r\nline two\r\n";
        assert_eq!(split_lines(text), text.lines().collect::<Vec<_>>());
    }

    #[test]
    fn split_lines_carries_style_across_a_line_boundary_and_releases_it_after_a_reset() {
        let text = "\u{1b}[1;33mWARNING\n  - step one\n  - step two\u{1b}[0m\n  - step three";
        let lines = split_lines(text);
        assert_eq!(lines.len(), 4);

        // The first line is untouched: no style was carried in yet.
        assert_eq!(lines[0], "\u{1b}[1;33mWARNING");

        // The second and third lines re-open bold-yellow before their text, so
        // parsing each one independently still recovers the carried style.
        let bold_yellow = |runs: &[Run]| {
            runs[0].style.fg == Some(Color::Yellow)
                && runs[0].style.add_modifier.contains(Modifier::BOLD)
        };
        assert!(bold_yellow(&parse(&lines[1])));
        assert_eq!(texts(&parse(&lines[1])), vec!["  - step one"]);
        assert!(bold_yellow(&parse(&lines[2])));

        // Line three's trailing reset ends the block, so the style carried
        // into line four is back to default: nothing is prepended.
        assert_eq!(lines[3], "  - step three");
        assert_eq!(parse(&lines[3])[0].style, Style::default());
    }

    #[test]
    fn split_lines_releases_style_the_line_after_a_mid_block_reset() {
        let text = "\u{1b}[31mred\nstill red\n\u{1b}[0mplain\nstill plain";
        let lines = split_lines(text);
        assert_eq!(lines[0], "\u{1b}[31mred");
        assert_eq!(parse(&lines[1])[0].style.fg, Some(Color::Red));
        // The reset lives inside line three's own content, so that line still
        // renders plain even though red style was carried into it.
        assert_eq!(texts(&parse(&lines[2])), vec!["plain"]);
        assert_eq!(parse(&lines[2])[0].style, Style::default());
        // By line four the carried style is default, so nothing is prepended.
        assert_eq!(lines[3], "still plain");
    }
}
