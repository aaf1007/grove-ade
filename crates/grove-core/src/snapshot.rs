//! A session's terminal as bytes for a reset xterm.js, so a panel that attaches late shows what
//! one that saw every byte would.

use std::io::Write;

use alacritty_terminal::Term;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor};

/// A session's terminal at the moment of an attach, as bytes for a reset xterm.js to write
/// before the first live frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The last frame the snapshot covers. Live frames start at `seq + 1`. 0 if no frame has
    /// gone out yet.
    pub seq: u64,
    /// The size the bytes were written for, in cells. The panel takes this size before writing
    /// them.
    pub cols: u16,
    pub rows: u16,
    /// Scrollback, screen, cursor and modes as escape sequences, ending with any unfinished
    /// escape sequence or UTF-8 character.
    pub bytes: Vec<u8>,
}

/// Writes the parser's scrollback, screen, cursor and modes as escape sequences, then
/// `unfinished`, the bytes of a sequence the parser has read but not finished.
pub(crate) fn serialize<T>(term: &mut Term<T>, unfinished: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    if term.mode().contains(TermMode::ALT_SCREEN) {
        with_main_screen(term, |main| {
            w.lines(main, main.history_size());
            w.cursor(main);
        });
        // Saves the main screen's cursor, as entering the alt screen did in the parser.
        w.set_style(&Style::default());
        w.out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        w.lines(term.grid(), 0);
    } else {
        w.lines(term.grid(), term.grid().history_size());
    }
    w.saved_cursor(&term.grid().saved_cursor);
    w.modes(term);
    w.cursor(term.grid());
    w.out.extend_from_slice(unfinished);
    w.out
}

/// The bytes of an escape sequence the parser has read but not finished, to end a snapshot with
/// so the next frame finishes it. It follows vte 0.15's states. It keeps each byte that shapes
/// the sequence and none the parser has acted on already, such as a line feed inside a CSI, or
/// an OSC that an ESC ended.
#[derive(Default)]
pub(crate) struct Unfinished {
    state: State,
    bytes: Vec<u8>,
}

/// The most of a sequence `Unfinished` keeps, so output that never ends one stays small. Its
/// start is enough for a panel's parser to finish the sequence; the panel misses the bytes past it.
const MAX_UNFINISHED: usize = 4096;

/// vte's parser states. The four CSI states end on the same bytes, so they are one here.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    Osc,
    SosPmApc,
}

impl Unfinished {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Follows `bytes`, which the parser has just read.
    pub(crate) fn advance(&mut self, bytes: &[u8]) {
        // Every state starts over at ESC, so only the bytes from the last one on matter. Text
        // with none leaves the ground state as it is.
        let last_esc = bytes.iter().rposition(|&byte| byte == 0x1b);
        if last_esc.is_none() && self.state == State::Ground {
            return;
        }
        for &byte in &bytes[last_esc.unwrap_or(0)..] {
            self.step(byte);
        }
    }

    fn step(&mut self, byte: u8) {
        use State::*;
        // vte executes or ignores these inside a sequence, but for DCS data.
        let control = matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f);
        let (state, keep) = match (self.state, byte) {
            (_, 0x1b) => {
                // Any state starts a new sequence here.
                self.bytes.clear();
                (Escape, true)
            }
            (Ground, _) | (_, 0x18 | 0x1a) | (DcsPassthrough, 0x9c) | (Osc, 0x07) => {
                (Ground, false)
            }
            (DcsPassthrough, _) => (DcsPassthrough, true),
            (state, _) if control => (state, false),
            (Osc, _) => (Osc, true),
            (SosPmApc | DcsIgnore, _) => (self.state, false),
            (Csi, 0x40..=0x7e) => (Ground, false),
            (Csi, _) => (Csi, true),
            (Escape | EscapeIntermediate, 0x20..=0x2f) => (EscapeIntermediate, true),
            (Escape, b'[') => (Csi, true),
            (Escape, b']') => (Osc, true),
            (Escape, b'P') => (DcsEntry, true),
            (Escape, b'X' | b'^' | b'_') => (SosPmApc, true),
            (Escape | EscapeIntermediate, 0x30..=0x7e) => (Ground, false),
            (DcsEntry | DcsParam | DcsIntermediate, 0x20..=0x2f) => (DcsIntermediate, true),
            (DcsEntry | DcsParam, 0x30..=0x3b) | (DcsEntry, 0x3c..=0x3f) => (DcsParam, true),
            (DcsParam, 0x3c..=0x3f) | (DcsIntermediate, 0x30..=0x3f) => (DcsIgnore, true),
            (DcsEntry | DcsParam | DcsIntermediate, 0x40..=0x7e) => (DcsPassthrough, true),
            // DEL and bytes from 0x80 up, which vte ignores here.
            (state, _) => (state, false),
        };
        self.state = state;
        if state == Ground {
            self.bytes.clear();
        } else if keep && self.bytes.len() < MAX_UNFINISHED {
            self.bytes.push(byte);
        }
    }
}

/// Runs `read` on the main screen while the alt screen is on. alacritty_terminal 0.26 has no
/// getter for it, so this swaps to it and back. Swapping back clears the alt screen, so it is
/// put back from a copy. It also sets the main screen's saved cursor to its cursor, which it
/// already is: entering the alt screen set it, and only a resize can move one and not the other.
fn with_main_screen<T>(term: &mut Term<T>, read: impl FnOnce(&Grid<Cell>)) {
    let alt = term.grid().clone();
    term.swap_alt();
    read(term.grid());
    term.swap_alt();
    *term.grid_mut() = alt;
}

/// The kitty protocol's keyboard flags, lowest bit first.
const KEYBOARD_FLAGS: [TermMode; 5] = [
    TermMode::DISAMBIGUATE_ESC_CODES,
    TermMode::REPORT_EVENT_TYPES,
    TermMode::REPORT_ALTERNATE_KEYS,
    TermMode::REPORT_ALL_KEYS_AS_ESC,
    TermMode::REPORT_ASSOCIATED_TEXT,
];

/// Each mode a program sets with `CSI h` and resets with `CSI l`, but the alt screen.
const MODES: [(TermMode, &str); 15] = [
    (TermMode::APP_CURSOR, "?1"),
    (TermMode::ORIGIN, "?6"),
    (TermMode::LINE_WRAP, "?7"),
    (TermMode::SHOW_CURSOR, "?25"),
    (TermMode::MOUSE_REPORT_CLICK, "?1000"),
    (TermMode::MOUSE_DRAG, "?1002"),
    (TermMode::MOUSE_MOTION, "?1003"),
    (TermMode::FOCUS_IN_OUT, "?1004"),
    (TermMode::UTF8_MOUSE, "?1005"),
    (TermMode::SGR_MOUSE, "?1006"),
    (TermMode::ALTERNATE_SCROLL, "?1007"),
    (TermMode::URGENCY_HINTS, "?1042"),
    (TermMode::BRACKETED_PASTE, "?2004"),
    (TermMode::INSERT, "4"),
    (TermMode::LINE_FEED_NEW_LINE, "20"),
];

/// Escape sequences for a fresh terminal, with the colors and attributes they leave set.
#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    style: Style,
}

impl Writer {
    /// Writes the last `history` scrollback lines and the screen, top to bottom.
    fn lines(&mut self, grid: &Grid<Cell>, history: usize) {
        let cols = grid.columns();
        let rows = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX);
        let history = i32::try_from(history).unwrap_or(i32::MAX);
        let mut continued = false;
        for line in -history..rows {
            let row = &grid[Line(line)];
            let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            let mut end = if wrapped {
                cols
            } else {
                (0..cols)
                    .rev()
                    .find(|&col| !is_blank(&row[Column(col)]))
                    .map_or(0, |col| col + 1)
            };
            // Only a character after the last column wraps the line before it.
            if continued {
                end = end.max(1);
            }
            for cell in (0..end).map(|col| &row[Column(col)]) {
                // A wide character writes its spacers itself.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                self.set_style(&Style::of(cell));
                self.char(cell);
            }
            if line + 1 < rows && !wrapped {
                // A line feed that scrolls fills the new line with the current background.
                self.set_style(&Style::default());
                self.out.extend_from_slice(b"\r\n");
            }
            continued = wrapped;
        }
    }

    /// Sets or resets each mode where it differs from a fresh terminal's, then the keypad,
    /// the keyboard flags and the cursor style.
    fn modes<T>(&mut self, term: &Term<T>) {
        let mode = *term.mode();
        let fresh = TermMode::default();
        for (flag, code) in MODES {
            if mode.contains(flag) != fresh.contains(flag) {
                let set = if mode.contains(flag) { 'h' } else { 'l' };
                let _ = write!(self.out, "\x1b[{code}{set}");
            }
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            self.out.extend_from_slice(b"\x1b=");
        }
        // Always none while the parser has the kitty protocol off.
        let keyboard = KEYBOARD_FLAGS
            .iter()
            .enumerate()
            .filter(|(_, flag)| mode.contains(**flag))
            .fold(0, |flags, (bit, _)| flags | 1 << bit);
        if keyboard != 0 {
            let _ = write!(self.out, "\x1b[>{keyboard}u");
        }
        let style = term.cursor_style();
        if style != CursorStyle::default() {
            let steady = match style.shape {
                CursorShape::Underline => 4,
                CursorShape::Beam => 6,
                _ => 2,
            };
            let _ = write!(self.out, "\x1b[{} q", steady - u8::from(style.blinking));
        }
    }

    /// Saves `saved` as the cursor that DECRC restores.
    fn saved_cursor(&mut self, saved: &Cursor<Cell>) {
        self.move_to(saved.point);
        self.set_style(&Style::of(&saved.template));
        self.out.extend_from_slice(b"\x1b7");
    }

    /// Puts the cursor where the grid has it, waiting to wrap if it is, with its colors.
    fn cursor(&mut self, grid: &Grid<Cell>) {
        let cursor = &grid.cursor;
        let mut point = cursor.point;
        if cursor.input_needs_wrap {
            // A wide character ends in a spacer, so it is written from its first cell.
            if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
                point.column -= 1;
            }
            // Writing the last character again leaves the terminal waiting to wrap too.
            self.move_to(point);
            self.set_style(&Style::of(&grid[point]));
            self.char(&grid[point]);
        } else {
            self.move_to(point);
        }
        self.set_style(&Style::of(&cursor.template));
    }

    fn move_to(&mut self, point: Point) {
        let _ = write!(
            self.out,
            "\x1b[{};{}H",
            point.line.0 + 1,
            point.column.0 + 1
        );
    }

    fn char(&mut self, cell: &Cell) {
        let mut buf = [0; 4];
        // The parser keeps a tab in the cell it jumped from, and xterm.js draws it as a space.
        let c = if cell.c == '\t' { ' ' } else { cell.c };
        self.out
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        for &c in cell.zerowidth().unwrap_or_default() {
            self.out
                .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }

    /// Writes what it takes to go from the current style to `style`.
    fn set_style(&mut self, style: &Style) {
        if style.link != self.style.link {
            match &style.link {
                Some(link) => {
                    let _ = write!(self.out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
                }
                None => self.out.extend_from_slice(b"\x1b]8;;\x1b\\"),
            }
        }
        if style.sgr != self.style.sgr {
            style.sgr.write(&mut self.out);
        }
        self.style = style.clone();
    }
}

/// A cell with nothing to draw, as a cell nothing was ever written to.
fn is_blank(cell: &Cell) -> bool {
    matches!(cell.c, ' ' | '\t')
        && (cell.flags - Flags::WRAPLINE).is_empty()
        && Style::of(cell) == Style::default()
}

/// How a cell is drawn.
#[derive(Clone, Default, PartialEq)]
struct Style {
    sgr: Sgr,
    link: Option<Hyperlink>,
}

impl Style {
    fn of(cell: &Cell) -> Self {
        Self {
            sgr: Sgr {
                fg: (cell.fg != Color::Named(NamedColor::Foreground)).then_some(cell.fg),
                bg: (cell.bg != Color::Named(NamedColor::Background)).then_some(cell.bg),
                flags: cell.flags & SGR_FLAGS,
                underline: cell.underline_color(),
            },
            link: cell.hyperlink(),
        }
    }
}

/// The attributes SGR sets. None is the default color.
#[derive(Clone, Copy, PartialEq)]
struct Sgr {
    fg: Option<Color>,
    bg: Option<Color>,
    flags: Flags,
    underline: Option<Color>,
}

impl Default for Sgr {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            flags: Flags::empty(),
            underline: None,
        }
    }
}

const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

impl Sgr {
    /// Writes one SGR that resets everything, then sets these attributes.
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        let attrs = [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::UNDERLINE, ";4"),
            (Flags::DOUBLE_UNDERLINE, ";4:2"),
            (Flags::UNDERCURL, ";4:3"),
            (Flags::DOTTED_UNDERLINE, ";4:4"),
            (Flags::DASHED_UNDERLINE, ";4:5"),
            (Flags::INVERSE, ";7"),
            (Flags::HIDDEN, ";8"),
            (Flags::STRIKEOUT, ";9"),
        ];
        for (flag, code) in attrs {
            if self.flags.contains(flag) {
                out.extend_from_slice(code.as_bytes());
            }
        }
        if let Some(color) = self.fg {
            sgr_color(out, color, Some((30, 90)), 38);
        }
        if let Some(color) = self.bg {
            sgr_color(out, color, Some((40, 100)), 48);
        }
        if let Some(color) = self.underline {
            sgr_color(out, color, None, 58);
        }
        out.push(b'm');
    }
}

/// `short` has the codes for the 8 colors and their bright versions, where SGR has them.
/// `extended` is 38, 48 or 58.
fn sgr_color(out: &mut Vec<u8>, color: Color, short: Option<(u8, u8)>, extended: u8) {
    let _ = match color {
        Color::Named(name) => {
            let index = name as usize;
            match short {
                Some((normal, _)) if index < 8 => write!(out, ";{}", usize::from(normal) + index),
                Some((_, bright)) if index < 16 => {
                    write!(out, ";{}", usize::from(bright) + index - 8)
                }
                _ if index < 16 => write!(out, ";{extended};5;{index}"),
                // SGR names no other color: the foreground and background are the defaults.
                _ => Ok(()),
            }
        }
        Color::Indexed(index) => write!(out, ";{extended};5;{index}"),
        Color::Spec(rgb) => write!(out, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b),
    };
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;
    use crate::terminal::Terminal;

    fn terminal() -> Terminal {
        let (tx, _rx) = mpsc::channel();
        Terminal::new(80, 24, tx)
    }

    /// A fresh terminal fed `t`'s snapshot, as a panel is, and the replies it sent.
    fn restore(t: &mut Terminal) -> (Terminal, Receiver<Vec<u8>>) {
        let (cols, rows) = t.size();
        let (tx, rx) = mpsc::channel();
        let mut fresh = Terminal::new(cols, rows, tx);
        fresh.advance(&t.snapshot());
        (fresh, rx)
    }

    #[test]
    fn a_snapshot_after_2000_lines_has_the_screen_and_the_last_1000_lines() {
        let mut t = terminal();
        for i in 1..=2000 {
            t.advance(format!("line {i}\r\n").as_bytes());
        }

        let (restored, _) = restore(&mut t);

        assert_eq!(restored.scrollback_len(), 1000);
        assert_eq!(restored.line_text(-1000), "line 978");
        assert_eq!(restored.line_text(-1), "line 1977");
        assert_eq!(restored.line_text(0), "line 1978");
        assert_eq!(restored.line_text(22), "line 2000");
        assert_eq!(t.differences(&restored), Vec::<String>::new());
    }

    #[test]
    fn a_snapshot_keeps_colors_links_wide_characters_and_the_cursor() {
        let mut t = terminal();
        t.advance(
            "\x1b[1;3;4:3;58;5;196m\x1b[38;2;1;2;3;48;5;17mstyled\x1b[0m \x1b[91;44mbright\x1b[0m\r\n\
             \x1b]8;id=a;https://example.com\x1b\\link\x1b]8;;\x1b\\ 漢字 e\u{301}\ta\x1b[41m\x1b[K\r\n\
             \x1b[5;10H\x1b[32m\x1b7\x1b[0m\x1b[3;1H\x1b[7m"
                .as_bytes(),
        );
        // Ends waiting to wrap after a linked wide character in the last two columns, with the
        // link closed after it.
        t.advance("\x1b[3;79H\x1b]8;;https://wrap\x1b\\界\x1b]8;;\x1b\\".as_bytes());

        let (restored, _) = restore(&mut t);

        assert_eq!(t.differences(&restored), Vec::<String>::new());
    }

    #[test]
    fn a_snapshot_in_the_alt_screen_keeps_its_modes_and_the_main_screen_behind_it() {
        let mut t = terminal();
        t.advance(b"\x1b[33mbefore vim\x1b[0m\r\n$ ");
        t.advance(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[Hvim screen\x1b[5;3H");

        let (mut restored, _) = restore(&mut t);

        assert_eq!(restored.line_text(0), "vim screen");
        assert!(restored.bracketed_paste());
        assert_eq!(t.differences(&restored), Vec::<String>::new());
        // The main screen comes back, with its cursor, when the program leaves the alt screen.
        t.advance(b"\x1b[?1049l");
        restored.advance(b"\x1b[?1049l");
        assert_eq!(restored.line_text(0), "before vim");
        assert_eq!(t.differences(&restored), Vec::<String>::new());
    }

    #[test]
    fn a_snapshot_keeps_every_mode_on_or_off_and_the_cursor_style() {
        let mut t = terminal();
        // On: cursor keys, keypad, all mouse motion, UTF-8 mouse, focus, bracketed paste,
        // insert, new line, origin. Off: alternate scroll, urgency hints, line wrap, the cursor.
        t.advance(
            b"\x1b[?1h\x1b=\x1b[?1003h\x1b[?1005h\x1b[?1004h\x1b[?2004h\x1b[4h\x1b[20h\x1b[?6h\
              \x1b[?1007l\x1b[?1042l\x1b[?7l\x1b[?25l\x1b[5 q\x1b[3;4H",
        );

        let (restored, _) = restore(&mut t);

        assert_eq!(t.differences(&restored), Vec::<String>::new());
    }

    // Reading the main screen swaps alacritty_terminal's grids. If an update changes what
    // swap_alt does, this fails.
    #[test]
    fn a_snapshot_in_the_alt_screen_leaves_the_parser_as_it_was() {
        let before: &[u8] =
            b"main\x1b[2;5H\x1b[31m\x1b7\x1b[4;9H\x1b[?1049h\x1b[44malt\x1b[3;3H\x1b7\x1b[6;6H";
        let after: &[u8] = b"more\x1b8x\x1b[?1049lback\x1b8y";
        let mut snapshotted = terminal();
        let mut untouched = terminal();
        snapshotted.advance(before);
        untouched.advance(before);

        snapshotted.snapshot();

        assert_eq!(untouched.differences(&snapshotted), Vec::<String>::new());
        snapshotted.advance(after);
        untouched.advance(after);
        assert_eq!(untouched.differences(&snapshotted), Vec::<String>::new());
    }

    #[test]
    fn output_after_a_snapshot_cut_mid_sequence_finishes_the_sequence() {
        // Output up to the snapshot, and the rest of it.
        let cuts: [(&str, &[u8], &[u8]); 11] = [
            ("CSI", b"\x1b[3", b"1mred\x1b[0m"),
            (
                "OSC",
                b"\x1b]8;;https://ex",
                b"ample.com\x1b\\link\x1b]8;;\x1b\\",
            ),
            (
                "OSC before its backslash",
                b"\x1b]8;;https://x\x1b",
                b"\\link",
            ),
            ("UTF-8", b"caf\xc3", "\u{a9} 漢".as_bytes()),
            (
                "UTF-8 in an OSC",
                b"\x1b]8;;https://\xe6\xbc",
                b"\xa2\x1b\\link",
            ),
            ("a line feed in a CSI", b"ab\x1b[1\n", b"2mbold"),
            ("ESC", b"ab\x1b", b"[32mgreen"),
            ("a charset", b"\x1b(", b"0qqq\x1b(B"),
            ("DCS", b"\x1bP$q", b"m\x1b\\after"),
            ("APC", b"\x1b_Gabc", b"def\x1b\\after"),
            ("a CSI cancelled", b"\x1b[3\x18", b"1m"),
        ];
        for (name, before, after) in cuts {
            let mut t = terminal();
            t.advance(before);

            let (mut restored, _) = restore(&mut t);
            t.advance(after);
            restored.advance(after);

            assert_eq!(
                t.differences(&restored),
                Vec::<String>::new(),
                "cut in {name}"
            );
        }
    }

    #[test]
    fn a_snapshot_cut_in_an_endless_sequence_stays_small_and_finishes_it() {
        // The start of a sequence, a byte that repeats for 1 MiB, and the rest of it.
        let cuts: [(&str, &[u8], u8, &[u8]); 3] = [
            ("CSI", b"\x1b[1m\x1b[", b'0', b"00mafter"),
            ("OSC", b"\x1b]999;", b'a', b"aaa\x07after"),
            ("DCS", b"\x1bPq", b'#', b"###\x1b\\after"),
        ];
        for (name, start, repeated, end) in cuts {
            let mut t = terminal();
            t.advance(start);
            for chunk in vec![repeated; 1 << 20].chunks(4096) {
                t.advance(chunk);
            }

            assert!(t.snapshot().len() < 64 * 1024, "snapshot cut in {name}");
            let (mut restored, _) = restore(&mut t);
            t.advance(end);
            restored.advance(end);

            assert_eq!(
                t.differences(&restored),
                Vec::<String>::new(),
                "cut in {name}"
            );
        }
    }

    #[test]
    fn a_snapshot_holds_no_terminal_queries() {
        let (tx, live_replies) = mpsc::channel();
        let mut t = Terminal::new(80, 24, tx);
        // Every query the parser answers, then an OSC color query cut between the ESC and the
        // backslash that end it. The parser answers that one at the ESC.
        t.advance(
            b"\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?2004$p\x1b[18t\x1b]10;?\x07\x1b]4;1;?\x07\x1b]11;?\x1b",
        );
        assert_eq!(live_replies.try_iter().count(), 9);

        let (_, replies) = restore(&mut t);

        assert_eq!(
            replies.try_iter().collect::<Vec<_>>(),
            Vec::<Vec<u8>>::new()
        );
    }

    #[test]
    fn a_snapshot_keeps_the_keyboard_flags() {
        let kitty_term = || {
            let config = Config {
                kitty_keyboard: true,
                ..Config::default()
            };
            Term::new(config, &TermSize::new(80, 24), VoidListener)
        };
        let mut live = kitty_term();
        Processor::<StdSyncHandler>::new().advance(&mut live, b"\x1b[>5u");

        let mut restored = kitty_term();
        Processor::<StdSyncHandler>::new().advance(&mut restored, &serialize(&mut live, &[]));

        assert_eq!(
            *restored.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL,
            TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALTERNATE_KEYS
        );
    }
}
