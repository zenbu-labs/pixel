use std::io::Write as _;

use unicode_width::UnicodeWidthChar;

use crate::canvas::Canvas;
use crate::style::Color;
use crate::tree::{PxRect, RNode, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Attrs {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Empty,
    Glyph {
        text: String,
        color: Color,
        attrs: Attrs,
        wide: bool,
    },
    WideTail,
}

struct Glyph {
    text: String,
    width: usize,
    byte: usize,
}

/// Text the terminal draws itself, one entry per terminal cell, laid over the
/// frame image that sits beneath terminal text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextGrid {
    cols: u32,
    rows: u32,
    cell: (f32, f32),
    cells: Vec<Cell>,
}

pub(crate) fn line_cells(line: &str) -> usize {
    glyphs(line, 0).iter().map(|g| g.width).sum()
}

fn glyphs(line: &str, base: usize) -> Vec<Glyph> {
    let mut out: Vec<Glyph> = Vec::new();
    for (i, c) in line.char_indices() {
        let c = if c == '\t' { ' ' } else { c };
        match c.width() {
            None => {}
            Some(0) => {
                if let Some(last) = out.last_mut() {
                    last.text.push(c);
                }
            }
            Some(width) => out.push(Glyph {
                text: c.to_string(),
                width,
                byte: base + i,
            }),
        }
    }
    out
}

fn over(top: Color, bottom: [u8; 3]) -> [u8; 3] {
    let a = u32::from(top[3]);
    std::array::from_fn(|i| ((u32::from(top[i]) * a + u32::from(bottom[i]) * (255 - a) + 127) / 255) as u8)
}

impl TextGrid {
    pub(crate) fn new(cols: u32, rows: u32, cell: (u32, u32)) -> Self {
        Self {
            cols,
            rows,
            cell: (cell.0.max(1) as f32, cell.1.max(1) as f32),
            cells: vec![Cell::Empty; (cols * rows) as usize],
        }
    }

    pub(crate) fn blank(&self) -> Self {
        Self::new(self.cols, self.rows, (self.cell.0 as u32, self.cell.1 as u32))
    }

    pub(crate) fn same_shape(&self, other: &TextGrid) -> bool {
        (self.cols, self.rows, self.cell) == (other.cols, other.rows, other.cell)
    }

    /// Walks the tree in paint order: text nodes drawn by the terminal fill cells,
    /// and anything painted later on top of them hides the cells it covers.
    pub(crate) fn place_tree(
        &mut self,
        tree: &Tree,
        origin_x: f32,
        cursor: Option<(f32, f32)>,
        frame: &Canvas,
        backdrop: [u8; 3],
    ) {
        for &id in tree.paint_order() {
            let Some(node) = tree.get(id) else {
                continue;
            };
            let hovered = cursor.is_some_and(|(x, y)| node.visible.contains(x, y));
            let terminal_text = tree.draws_terminal_text(node);
            if paints_over(node, hovered, terminal_text) {
                self.clear_rect(shift(node.visible, origin_x));
            }
            if terminal_text && let Some(text) = &node.text {
                self.place_text(tree, node, text, origin_x, hovered, frame, backdrop);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn place_text(
        &mut self,
        tree: &Tree,
        node: &RNode,
        text: &str,
        origin_x: f32,
        hovered: bool,
        frame: &Canvas,
        backdrop: [u8; 3],
    ) {
        let (cw, ch) = self.cell;
        let (pad_left, pad_top, pad_right) = tree
            .taffy
            .layout(node.taffy)
            .map(|l| (l.padding.left, l.padding.top, l.padding.right))
            .unwrap_or((0.0, 0.0, 0.0));
        let rect = shift(node.abs, origin_x);
        let visible = shift(node.visible, origin_x);
        let color = match (hovered, node.style.hover_color) {
            (true, Some(c)) => c,
            _ => node.resolved.color,
        };
        let first_col = ((rect.x + pad_left) / cw).round() as i64;
        let last_fitting = ((rect.x + rect.w - pad_right) / cw - 0.5).floor() as i64;
        let min_col = ((visible.x / cw - 0.5).ceil() as i64).max(0);
        let max_col = (((visible.x + visible.w) / cw - 0.5).floor() as i64).min(self.cols as i64 - 1);
        let top_row = ((rect.y + pad_top) / ch).round() as i64;
        let mut byte = 0;
        for (i, line) in text.split('\n').enumerate() {
            let row = top_row + i as i64;
            let center = (row as f32 + 0.5) * ch;
            let shown = row >= 0
                && row < self.rows as i64
                && center >= visible.y
                && center <= visible.y + visible.h;
            if shown {
                let mut line_glyphs = glyphs(line, byte);
                let room = (last_fitting - first_col + 1).max(0) as usize;
                if node.style.ellipsis && line_glyphs.iter().map(|g| g.width).sum::<usize>() > room {
                    let mut used = 0;
                    let keep = line_glyphs
                        .iter()
                        .take_while(|g| {
                            used += g.width;
                            used < room
                        })
                        .count();
                    let at = line_glyphs.get(keep).map_or(byte, |g| g.byte);
                    line_glyphs.truncate(keep);
                    if room > 0 {
                        line_glyphs.push(Glyph {
                            text: "…".into(),
                            width: 1,
                            byte: at,
                        });
                    }
                }
                let mut col = first_col;
                for glyph in line_glyphs {
                    let end = col + glyph.width as i64 - 1;
                    if end > max_col {
                        break;
                    }
                    if col >= min_col {
                        let (fg, attrs) = styled(node, glyph.byte, color);
                        let [r, g, b] = over(fg, self.backdrop_at(frame, col, row, backdrop));
                        self.put(col as u32, row as u32, glyph, [r, g, b, 255], attrs);
                    }
                    col = end + 1;
                }
            }
            byte += line.len() + 1;
        }
    }

    fn backdrop_at(&self, frame: &Canvas, col: i64, row: i64, backdrop: [u8; 3]) -> [u8; 3] {
        let x = (((col as f32 + 0.5) * self.cell.0) as u32).min(frame.width.saturating_sub(1));
        let y = (((row as f32 + 0.5) * self.cell.1) as u32).min(frame.height.saturating_sub(1));
        let at = ((y * frame.width + x) * 4) as usize;
        match frame.pixels.get(at..at + 4) {
            Some(&[r, g, b, a]) => over([r, g, b, a], backdrop),
            _ => backdrop,
        }
    }

    fn row(&self, row: u32) -> &[Cell] {
        let start = self.index(0, row);
        &self.cells[start..start + self.cols as usize]
    }

    fn index(&self, col: u32, row: u32) -> usize {
        (row * self.cols + col) as usize
    }

    fn put(&mut self, col: u32, row: u32, glyph: Glyph, color: Color, attrs: Attrs) {
        let wide = glyph.width > 1;
        self.clear_cell(col, row);
        if wide {
            self.clear_cell(col + 1, row);
        }
        let at = self.index(col, row);
        self.cells[at] = Cell::Glyph {
            text: glyph.text,
            color,
            attrs,
            wide,
        };
        if wide {
            self.cells[at + 1] = Cell::WideTail;
        }
    }

    fn clear_cell(&mut self, col: u32, row: u32) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let at = self.index(col, row);
        match self.cells[at] {
            Cell::Glyph { wide: true, .. } if col + 1 < self.cols => self.cells[at + 1] = Cell::Empty,
            Cell::WideTail if col > 0 => self.cells[at - 1] = Cell::Empty,
            _ => {}
        }
        self.cells[at] = Cell::Empty;
    }

    fn clear_rect(&mut self, rect: PxRect) {
        let (cw, ch) = self.cell;
        let first_col = ((rect.x / cw - 0.5).ceil() as i64).max(0);
        let last_col = (((rect.x + rect.w) / cw - 0.5).floor() as i64).min(self.cols as i64 - 1);
        let first_row = ((rect.y / ch - 0.5).ceil() as i64).max(0);
        let last_row = (((rect.y + rect.h) / ch - 0.5).floor() as i64).min(self.rows as i64 - 1);
        for row in first_row..=last_row {
            for col in first_col..=last_col {
                self.clear_cell(col as u32, row as u32);
            }
        }
    }

    /// Escape sequences that turn what `previous` put on screen into this grid.
    pub(crate) fn write_changes(&self, previous: &TextGrid, out: &mut Vec<u8>) {
        debug_assert!(self.same_shape(previous));
        let mut wrote = false;
        for row in 0..self.rows {
            let (now, before) = (self.row(row), previous.row(row));
            let Some(first) = (0..now.len()).find(|&i| now[i] != before[i]) else {
                continue;
            };
            let last = (0..now.len()).rfind(|&i| now[i] != before[i]).unwrap_or(first);
            let start = if first > 0 && (now[first] == Cell::WideTail || before[first] == Cell::WideTail) {
                first - 1
            } else {
                first
            };
            if !wrote {
                out.extend_from_slice(b"\x1b7");
                wrote = true;
            }
            let _ = write!(out, "\x1b[{};{}H\x1b[0m", row + 1, start + 1);
            let mut style: Option<(Color, Attrs)> = None;
            for cell in &now[start..=last] {
                match cell {
                    Cell::Empty => {
                        if style.take().is_some() {
                            out.extend_from_slice(b"\x1b[0m");
                        }
                        out.push(b' ');
                    }
                    Cell::Glyph {
                        text, color, attrs, ..
                    } => {
                        if style != Some((*color, *attrs)) {
                            let _ = write!(out, "\x1b[0;38;2;{};{};{}", color[0], color[1], color[2]);
                            for (on, code) in [
                                (attrs.bold, ";1"),
                                (attrs.italic, ";3"),
                                (attrs.underline, ";4"),
                                (attrs.strikethrough, ";9"),
                            ] {
                                if on {
                                    out.extend_from_slice(code.as_bytes());
                                }
                            }
                            out.push(b'm');
                            style = Some((*color, *attrs));
                        }
                        out.extend_from_slice(text.as_bytes());
                    }
                    Cell::WideTail => {}
                }
            }
        }
        if wrote {
            out.extend_from_slice(b"\x1b[0m\x1b8");
        }
    }
}

fn shift(rect: PxRect, dx: f32) -> PxRect {
    PxRect {
        x: rect.x + dx,
        ..rect
    }
}

fn paints_over(node: &RNode, hovered: bool, terminal_text: bool) -> bool {
    node.style.background.is_some()
        || node.style.border.is_some()
        || (hovered && node.style.hover_background.is_some())
        || node.image.is_some()
        || node.surface.is_some()
        || node.shape.is_some()
        || node.input.is_some()
        || (node.text.is_some() && !terminal_text)
}

fn styled(node: &RNode, byte: usize, color: Color) -> (Color, Attrs) {
    match node.spans.iter().find(|s| s.start <= byte && byte < s.end) {
        Some(span) => (
            span.color,
            Attrs {
                bold: span.bold,
                italic: span.italic,
                underline: span.underline,
                strikethrough: span.strikethrough,
            },
        ),
        None => (color, Attrs::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, width: usize) -> Glyph {
        Glyph {
            text: text.into(),
            width,
            byte: 0,
        }
    }

    #[test]
    fn widths_follow_unicode_cells() {
        assert_eq!(line_cells("abc"), 3);
        assert_eq!(line_cells("日本"), 4);
        assert_eq!(line_cells("e\u{301}"), 1);
        assert_eq!(line_cells("a\u{7}b"), 2);
    }

    #[test]
    fn first_write_positions_and_colors_each_run() {
        let mut grid = TextGrid::new(10, 2, (8, 16));
        grid.put(2, 1, glyph("h", 1), [1, 2, 3, 255], Attrs::default());
        grid.put(3, 1, glyph("i", 1), [1, 2, 3, 255], Attrs::default());
        let mut out = Vec::new();
        grid.write_changes(&grid.blank(), &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b7\x1b[2;3H\x1b[0m\x1b[0;38;2;1;2;3mhi\x1b[0m\x1b8"
        );
    }

    #[test]
    fn removed_text_is_blanked() {
        let mut before = TextGrid::new(4, 1, (8, 16));
        before.put(0, 0, glyph("日", 2), [9, 9, 9, 255], Attrs::default());
        let now = before.blank();
        let mut out = Vec::new();
        now.write_changes(&before, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b7\x1b[1;1H\x1b[0m  \x1b[0m\x1b8");
    }

    #[test]
    fn unchanged_grid_writes_nothing() {
        let mut grid = TextGrid::new(4, 1, (8, 16));
        grid.put(0, 0, glyph("x", 1), [9, 9, 9, 255], Attrs::default());
        let mut out = Vec::new();
        grid.write_changes(&grid.clone(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn covering_half_of_a_wide_glyph_removes_all_of_it() {
        let mut grid = TextGrid::new(4, 1, (8, 16));
        grid.put(0, 0, glyph("日", 2), [9, 9, 9, 255], Attrs::default());
        grid.clear_rect(PxRect {
            x: 8.0,
            y: 0.0,
            w: 8.0,
            h: 16.0,
        });
        assert_eq!(grid, grid.blank());
    }
}
