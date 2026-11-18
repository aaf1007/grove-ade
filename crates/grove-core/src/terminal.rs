//! The headless parser that every byte of a session's output goes through. It is the only thing
//! that answers the session's terminal queries, attached or not, and it keeps the screen and
//! modes the core reads.

use std::borrow::Cow;
use std::sync::mpsc::Sender;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
#[cfg(test)]
use alacritty_terminal::index::Column;
use alacritty_terminal::index::Line;
#[cfg(test)]
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{NamedColor, Processor, Rgb};

use crate::snapshot::{self, Unfinished};

/// The smallest size the parser takes, as alacritty_terminal's `MIN_COLUMNS` and
/// `MIN_SCREEN_LINES`. A wide character needs two columns, and with one it panics.
pub(crate) const MIN_COLS: u16 = 2;
pub(crate) const MIN_ROWS: u16 = 1;

/// Lines of scrollback the parser keeps.
const SCROLLBACK: usize = 1000;

// The replies xterm.js gives to DA1 and DA2.
const DA1: &str = "\x1b[?1;2c";
const DA2: &str = "\x1b[>0;276;0c";

pub(crate) struct Terminal {
    term: Term<Replies>,
    parser: Processor,
    // The start of a UTF-8 character cut off at the end of the last chunk.
    held: Vec<u8>,
    // The escape sequence the parser is in the middle of.
    unfinished: Unfinished,
}

impl Terminal {
    /// Each reply to a terminal query goes to `replies`, to be written to the PTY.
    pub(crate) fn new(cols: u16, rows: u16, replies: Sender<Vec<u8>>) -> Self {
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &Size::new(cols, rows), Replies(replies)),
            parser: Processor::new(),
            held: Vec::new(),
            unfinished: Unfinished::default(),
        }
    }

    /// Parses one chunk of output and sends the replies to any queries in it.
    pub(crate) fn advance(&mut self, chunk: &[u8]) {
        // vte 0.15.0 drops a byte when it joins a UTF-8 character split across two calls, so
        // the end of a cut character waits for the next chunk.
        let bytes = if self.held.is_empty() {
            Cow::Borrowed(chunk)
        } else {
            self.held.extend_from_slice(chunk);
            Cow::Owned(std::mem::take(&mut self.held))
        };
        let whole = bytes.len() - partial_utf8(&bytes);
        self.parser.advance(&mut self.term, &bytes[..whole]);
        self.unfinished.advance(&bytes[..whole]);
        self.held = bytes[whole..].to_vec();
        // vte buffers a synchronized update (mode 2026) to draw it at once. Nothing draws here,
        // so buffering would only delay replies.
        if self.parser.sync_bytes_count() > 0 {
            self.parser.stop_sync(&mut self.term);
        }
    }

    /// Sets the size in cells. The caller resizes the PTY to match.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(Size::new(cols, rows));
    }

    /// The screen, one line per row, without trailing spaces.
    pub(crate) fn screen_text(&self) -> String {
        let grid = self.term.grid();
        let rows: Vec<String> = (0..grid.screen_lines())
            .map(|line| {
                let mut row = String::new();
                for cell in &grid[Line::from(line)][..] {
                    // The second half of a wide character.
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }
                    row.push(cell.c);
                    row.extend(cell.zerowidth().into_iter().flatten());
                }
                row.trim_end().to_owned()
            })
            .collect();
        rows.join("\n")
    }

    /// The last `n` lines of the scrollback and screen, oldest first, without trailing spaces. A
    /// line the terminal wrapped across rows comes back whole. The blank rows below the last
    /// output are left out.
    pub(crate) fn last_lines(&self, n: usize) -> Vec<String> {
        let grid = self.term.grid();
        let top = -i32::try_from(grid.history_size()).unwrap_or(i32::MAX);
        let bottom = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX);
        let mut lines = Vec::new();
        let mut line = String::new();
        for row in top..bottom {
            let row = &grid[Line(row)][..];
            for cell in row {
                // The second half of a wide character, or the empty last column a wide
                // character that did not fit left before it wrapped.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line.push(cell.c);
                line.extend(cell.zerowidth().into_iter().flatten());
            }
            let wrapped = row
                .last()
                .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE));
            if !wrapped {
                lines.push(std::mem::take(&mut line).trim_end().to_owned());
            }
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.split_off(lines.len().saturating_sub(n))
    }

    /// Whether the program has bracketed paste on.
    pub(crate) fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// The size in cells.
    pub(crate) fn size(&self) -> (u16, u16) {
        // Both came from a u16.
        let cols = u16::try_from(self.term.columns()).unwrap_or(u16::MAX);
        let rows = u16::try_from(self.term.screen_lines()).unwrap_or(u16::MAX);
        (cols, rows)
    }

    /// The scrollback, screen, cursor and modes as bytes for a fresh terminal of the same size,
    /// ending with the bytes the parser has read but not acted on yet.
    pub(crate) fn snapshot(&mut self) -> Vec<u8> {
        let unfinished = [self.unfinished.bytes(), &self.held].concat();
        snapshot::serialize(&mut self.term, &unfinished)
    }
}

#[cfg(test)]
impl Terminal {
    /// Line `line` as text: 0 is the top row of the screen and -1 the newest scrollback line.
    pub(crate) fn line_text(&self, line: i32) -> String {
        let row = &self.term.grid()[Line(line)];
        let text: String = row[..].iter().map(|cell| cell.c).collect();
        text.trim_end().to_owned()
    }

    /// Whether the parser has read part of an escape sequence or UTF-8 character.
    pub(crate) fn mid_sequence(&self) -> bool {
        !self.unfinished.bytes().is_empty() || !self.held.is_empty()
    }

    pub(crate) fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// How `other` differs from this terminal: every cell of the scrollback and the screen, the
    /// cursor, the saved cursor and the modes.
    pub(crate) fn differences(&self, other: &Terminal) -> Vec<String> {
        let mut diffs = Vec::new();
        let (want, got) = (&self.term, &other.term);
        if want.mode() != got.mode() {
            diffs.push(format!("modes {:?}, want {:?}", got.mode(), want.mode()));
        }
        if want.cursor_style() != got.cursor_style() {
            diffs.push(format!(
                "cursor style {:?}, want {:?}",
                got.cursor_style(),
                want.cursor_style()
            ));
        }
        let (want, got) = (want.grid(), got.grid());
        for (name, w, g) in [
            ("cursor", &want.cursor, &got.cursor),
            ("saved cursor", &want.saved_cursor, &got.saved_cursor),
        ] {
            if (w.point, w.input_needs_wrap) != (g.point, g.input_needs_wrap) {
                diffs.push(format!(
                    "{name} {:?} wrap {}, want {:?} wrap {}",
                    g.point, g.input_needs_wrap, w.point, w.input_needs_wrap
                ));
            }
            if !same_cell(&w.template, &g.template) {
                diffs.push(format!(
                    "{name} pen {:?}, want {:?}",
                    g.template, w.template
                ));
            }
        }
        if want.history_size() != got.history_size() {
            diffs.push(format!(
                "{} scrollback lines, want {}",
                got.history_size(),
                want.history_size()
            ));
        }
        let history = want.history_size().min(got.history_size());
        let history = i32::try_from(history).unwrap_or(i32::MAX);
        let rows = i32::try_from(want.screen_lines()).unwrap_or(i32::MAX);
        for line in -history..rows {
            let (w, g) = (&want[Line(line)], &got[Line(line)]);
            if let Some(col) =
                (0..want.columns()).find(|&c| !same_cell(&w[Column(c)], &g[Column(c)]))
            {
                diffs.push(format!(
                    "line {line} col {col}: {:?}, want {:?}",
                    g[Column(col)],
                    w[Column(col)]
                ));
            }
        }
        diffs
    }
}

/// Whether two cells draw the same. The parser keeps a tab in the cell it jumped from, which
/// draws as a space. It numbers each link that came without an id from one counter for the
/// whole process, so two parsers never give such a link the same id.
#[cfg(test)]
fn same_cell(a: &Cell, b: &Cell) -> bool {
    let c = |cell: &Cell| if cell.c == '\t' { ' ' } else { cell.c };
    let link = |cell: &Cell| {
        cell.hyperlink().map(|link| {
            let id = Some(link.id().to_owned()).filter(|id| !id.ends_with("_alacritty"));
            (id, link.uri().to_owned())
        })
    };
    (c(a), a.fg, a.bg, a.flags) == (c(b), b.fg, b.bg, b.flags)
        && a.zerowidth() == b.zerowidth()
        && a.underline_color() == b.underline_color()
        && link(a) == link(b)
}

/// The length of a UTF-8 character cut off at the end of `bytes`, or 0.
fn partial_utf8(bytes: &[u8]) -> usize {
    // A character is at most 4 bytes, so a cut one starts in the last 3.
    for (back, &b) in bytes.iter().rev().take(3).enumerate() {
        let len = match b {
            0x80..=0xbf => continue,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        return if back + 1 < len { back + 1 } else { 0 };
    }
    0
}

/// A terminal size in cells, as alacritty_terminal takes it.
struct Size {
    cols: usize,
    rows: usize,
}

impl Size {
    /// A size below the parser's smallest becomes the smallest.
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(MIN_COLS).into(),
            rows: rows.max(MIN_ROWS).into(),
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Turns the parser's events into replies for the PTY.
struct Replies(Sender<Vec<u8>>);

impl EventListener for Replies {
    /// Sends the reply for a query. Every other event is for a UI, so it is dropped.
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::PtyWrite(text) => as_xterm_js(text),
            Event::ColorRequest(index, format) => format(theme_color(index)),
            // The rest are for a UI. The size in pixels gets no reply, as in xterm.js.
            _ => return,
        };
        // Fails only once the writer thread is gone, and then nobody reads replies.
        let _ = self.0.send(reply.into_bytes());
    }
}

/// alacritty_terminal names itself in DA1 and DA2. A program should see the same terminal
/// whether or not a panel is attached, so answer as xterm.js does.
fn as_xterm_js(text: String) -> String {
    if text == "\x1b[?6c" {
        DA1.into()
    } else if text.starts_with("\x1b[>") && text.ends_with('c') {
        DA2.into()
    } else {
        text
    }
}

/// A color from xterm.js's default theme, until the UI's theme reaches the core.
fn theme_color(index: usize) -> Rgb {
    const ANSI: [u32; 16] = [
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
        0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ];
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    const BACKGROUND: usize = NamedColor::Background as usize;
    let hex = |v: u32| {
        let [_, r, g, b] = v.to_be_bytes();
        Rgb { r, g, b }
    };
    match index {
        0..=15 => hex(ANSI[index]),
        16..=231 => {
            let i = index - 16;
            Rgb {
                r: CUBE[i / 36],
                g: CUBE[i / 6 % 6],
                b: CUBE[i % 6],
            }
        }
        232..=255 => {
            let v = u8::try_from(8 + 10 * (index - 232)).unwrap_or(u8::MAX);
            Rgb { r: v, g: v, b: v }
        }
        BACKGROUND => hex(0x000000),
        // The foreground and the cursor.
        _ => hex(0xffffff),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use super::*;

    fn terminal() -> (Terminal, Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        (Terminal::new(80, 24, tx), rx)
    }

    fn replies(rx: &Receiver<Vec<u8>>) -> String {
        String::from_utf8(rx.try_iter().flatten().collect()).unwrap()
    }

    #[test]
    fn device_attributes_get_xterm_js_replies() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[c\x1b[0c\x1b[>c\x1b[>0c");

        assert_eq!(replies(&rx), [DA1, DA1, DA2, DA2].concat());
    }

    #[test]
    fn device_status_and_cursor_position_get_replies() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[5n\x1b[3;7H\x1b[6n");

        assert_eq!(replies(&rx), "\x1b[0n\x1b[3;7R");
    }

    #[test]
    fn a_mode_report_says_whether_the_mode_is_on() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[?2004$p\x1b[?2004h\x1b[?2004$p\x1b[4$p");

        assert_eq!(replies(&rx), "\x1b[?2004;2$y\x1b[?2004;1$y\x1b[4;2$y");
    }

    #[test]
    fn color_queries_get_the_theme_colors() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b]10;?\x1b\\\x1b]11;?\x07\x1b]4;1;?\x07");

        assert_eq!(
            replies(&rx),
            "\x1b]10;rgb:ffff/ffff/ffff\x1b\\\
             \x1b]11;rgb:0000/0000/0000\x07\
             \x1b]4;1;rgb:cccc/0000/0000\x07"
        );
    }

    #[test]
    fn the_size_in_cells_gets_a_reply_and_the_size_in_pixels_none() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[14t\x1b[18t");

        assert_eq!(replies(&rx), "\x1b[8;24;80t");
    }

    #[test]
    fn a_kitty_keyboard_query_gets_no_reply() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[?u");

        assert_eq!(replies(&rx), "");
    }

    #[test]
    fn resize_changes_the_size_the_parser_reports() {
        let (mut t, rx) = terminal();

        t.resize(100, 40);
        t.advance(b"\x1b[18t");

        assert_eq!(replies(&rx), "\x1b[8;40;100t");
    }

    #[test]
    fn a_query_in_a_synchronized_update_gets_its_reply_at_once() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[?2026h\x1b[c");

        assert_eq!(replies(&rx), DA1);
    }

    #[test]
    fn a_query_split_across_chunks_gets_one_reply() {
        let (mut t, rx) = terminal();

        t.advance(b"\x1b[");
        t.advance(b"6n");

        assert_eq!(replies(&rx), "\x1b[1;1R");
    }

    #[test]
    fn screen_text_has_one_line_per_row() {
        let (mut t, _rx) = terminal();

        t.advance("hi  \r\n\x1b[2Cwide 漢 e\u{301}".as_bytes());

        let text = t.screen_text();
        assert_eq!(text.split('\n').count(), 24);
        assert_eq!(
            &text[..text.find("\n\n").unwrap()],
            "hi\n  wide 漢 e\u{301}"
        );
    }

    #[test]
    fn a_utf8_character_split_across_chunks_comes_out_whole() {
        let (mut t, _rx) = terminal();

        t.advance(b"caf\xc3");
        t.advance("\u{e9} 漢".as_bytes().split_first().unwrap().1);

        assert!(t.screen_text().starts_with("café 漢\n"));
    }

    #[test]
    fn a_zero_size_terminal_still_fits_a_wide_character() {
        let (tx, rx) = mpsc::channel();
        let mut t = Terminal::new(0, 0, tx);

        t.advance("漢\x1b[18t".as_bytes());

        assert_eq!(replies(&rx), "\x1b[8;1;2t");
        assert_eq!(t.screen_text(), "漢");
    }

    #[test]
    fn last_lines_reach_into_the_scrollback_and_drop_the_blank_rows_below() {
        let (mut t, _rx) = terminal();
        let output: String = (1..=40).map(|n| format!("{n}\r\n")).collect();
        t.advance(format!("{output}\r\n").as_bytes());

        assert_eq!(t.last_lines(3), ["38", "39", "40"]);
        assert_eq!(t.last_lines(1000).len(), 40);
        assert_eq!(t.last_lines(0), Vec::<String>::new());
    }

    #[test]
    fn a_line_wrapped_across_rows_comes_back_as_one_line() {
        let (mut t, _rx) = terminal();
        let long = "x".repeat(100) + " end";

        t.advance(format!("{long}\r\nnext  \r\n").as_bytes());

        assert_eq!(t.last_lines(2), [long, "next".to_owned()]);
    }

    #[test]
    fn a_wide_character_that_wraps_to_the_next_row_leaves_no_space_behind() {
        let (mut t, _rx) = terminal();
        // One column is left on the first row, too narrow for the wide character.
        let line = "x".repeat(79) + "漢";

        t.advance(line.as_bytes());

        assert_eq!(t.last_lines(1), [line]);
    }

    #[test]
    fn a_blank_line_in_the_output_stays() {
        let (mut t, _rx) = terminal();

        t.advance(b"a\r\n\r\nb");

        assert_eq!(t.last_lines(5), ["a", "", "b"]);
    }

    #[test]
    fn bracketed_paste_follows_the_program() {
        let (mut t, _rx) = terminal();
        assert!(!t.bracketed_paste());

        t.advance(b"\x1b[?2004h");
        assert!(t.bracketed_paste());

        t.advance(b"\x1b[?2004l");
        assert!(!t.bracketed_paste());
    }
}
