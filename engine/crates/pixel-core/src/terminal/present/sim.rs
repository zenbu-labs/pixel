

#![cfg(unix)]

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

pub(super) struct Sim {
    pub(super) cell: (u32, u32),
    pub(super) images: HashMap<u32, Image>,
    pub(super) placements: Vec<Placed>,
    pub(super) cursor: (u32, u32),
    pub(super) buf: Vec<u8>,
    pub(super) partial: Option<(HashMap<String, String>, Vec<u8>)>,
    pub(super) next_seq: u64,
    pub(super) frames: usize,
    /// Graphics commands seen since the last frame marker.
    pub(super) commands_this_frame: usize,
    pub(super) max_commands_per_frame: usize,
    /// Pixels transmitted since the last frame marker, and the finished frame before it.
    pub(super) pixels_this_frame: u64,
    pub(super) pixels_last_frame: u64,
    pub(super) placement_deletes: usize,
    pub(super) transmits: usize,
    pub(super) transient_transmits: usize,
    pub(super) frame_edits: usize,
    /// Placeholder cells by (row, col): the image they show and which of its cells.
    pub(super) cells: HashMap<(u32, u32), PlaceholderCell>,
    /// Images with a virtual placement: the columns and rows it spans.
    virtual_placements: HashMap<u32, (u32, u32)>,
    fg: Option<u32>,
    last_placeholder: Option<(u32, u32)>,
    /// Open synchronized updates; a frame must send exactly one and close it.
    sync_depth: i32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PlaceholderCell {
    image: u32,
    row: Option<u32>,
    col: Option<u32>,
}

pub(super) struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub(super) struct Placed {
    image: u32,
    col: u32,
    row: u32,
    offset: (u32, u32),
    z: i32,
    seq: u64,
}

pub(super) const FRAME_MARK: &str = "framemark";

enum Token {
    Need,
    Skip(usize),
    Text(char, usize),
    Csi(usize),
    Osc(usize),
    Apc(usize, usize),
}

impl Sim {
    pub(super) fn new(cell: (u32, u32)) -> Self {
        Self {
            cell,
            images: HashMap::new(),
            placements: Vec::new(),
            cursor: (1, 1),
            buf: Vec::new(),
            partial: None,
            next_seq: 0,
            frames: 0,
            commands_this_frame: 0,
            max_commands_per_frame: 0,
            pixels_this_frame: 0,
            pixels_last_frame: 0,
            placement_deletes: 0,
            transmits: 0,
            transient_transmits: 0,
            frame_edits: 0,
            cells: HashMap::new(),
            virtual_placements: HashMap::new(),
            fg: None,
            last_placeholder: None,
            sync_depth: 0,
        }
    }

    pub(super) fn image_count(&self) -> usize {
        self.images.len()
    }

    pub(super) fn placement_count(&self) -> usize {
        self.placements.len()
    }

    pub(super) fn feed(&mut self, input: &[u8]) {
        self.buf.extend_from_slice(input);
        unwrap_passthrough(&mut self.buf);
        let mut i = 0;
        while i < self.buf.len() {
            match token(&self.buf[i..]) {
                Token::Need => break,
                Token::Skip(n) => i += n,
                Token::Text(ch, n) => {
                    self.text(ch);
                    i += n;
                }
                Token::Csi(end) => {
                    let body = self.buf[i + 2..i + end + 1].to_vec();
                    self.csi(&body);
                    i += end + 1;
                }
                Token::Osc(end) => {
                    let body = self.buf[i + 2..i + end].to_vec();
                    if body.starts_with(format!("22;{FRAME_MARK}").as_bytes()) {
                        assert_eq!(self.sync_depth, 0, "a frame ended inside a synchronized update");
                        self.frames += 1;
                        self.max_commands_per_frame =
                            self.max_commands_per_frame.max(self.commands_this_frame);
                        self.commands_this_frame = 0;
                        self.pixels_last_frame = self.pixels_this_frame;
                        self.pixels_this_frame = 0;
                    }
                    i += end;
                }
                Token::Apc(sep, end) => {
                    let control = String::from_utf8(self.buf[i + 3..i + sep].to_vec()).unwrap();
                    let payload = if sep < end {
                        self.buf[i + sep + 1..i + end].to_vec()
                    } else {
                        Vec::new()
                    };
                    self.apc(&control, payload);
                    i += end + 2;
                }
            }
        }
        self.buf.drain(..i);
    }

    fn csi(&mut self, body: &[u8]) {
        let Some((&final_byte, params)) = body.split_last() else {
            return;
        };
        let text = String::from_utf8_lossy(params);
        let numbers: Vec<u32> = text.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        match final_byte {
            b'H' => {
                let row = numbers.first().copied().filter(|&n| n > 0).unwrap_or(1);
                let col = numbers.get(1).copied().filter(|&n| n > 0).unwrap_or(1);
                self.cursor = (row, col);
            }
            b'J' if numbers.first() == Some(&2) => self.cells.clear(),
            b'h' if text == "?2026" => {
                self.sync_depth += 1;
                assert_eq!(self.sync_depth, 1, "synchronized updates never nest");
            }
            b'l' if text == "?2026" => {
                self.sync_depth -= 1;
                assert_eq!(self.sync_depth, 0, "a synchronized update closed twice");
            }
            b'm' => match numbers.as_slice() {
                [38, 2, r, g, b] => self.fg = Some(r << 16 | g << 8 | b),
                [39] | [0] | [] => self.fg = None,
                _ => {}
            },
            _ => {}
        }
    }

    /// A printed character: placeholders and their diacritics build placeholder cells, anything
    /// else empties the cell under the cursor.
    fn text(&mut self, ch: char) {
        if ch == crate::kitty::PLACEHOLDER {
            let image = self.fg.expect("a placeholder cell names its image in the foreground color");
            self.cells.insert(self.cursor, PlaceholderCell { image, row: None, col: None });
            self.last_placeholder = Some(self.cursor);
            self.cursor.1 += 1;
            return;
        }
        if let Some(index) = crate::kitty::ROW_COLUMN_DIACRITICS.iter().position(|&d| d == ch) {
            let at = self.last_placeholder.expect("a diacritic follows a placeholder");
            let cell = self.cells.get_mut(&at).unwrap();
            match cell.row {
                None => cell.row = Some(index as u32),
                Some(_) => cell.col = Some(index as u32),
            }
            return;
        }
        self.cells.remove(&self.cursor);
        self.last_placeholder = None;
        self.cursor.1 += 1;
    }

    fn apc(&mut self, control: &str, payload: Vec<u8>) {
        let mut keys: HashMap<String, String> = control
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut payload = payload;
        if let Some((_, stored)) = self.partial.as_mut() {
            stored.extend_from_slice(&payload);
            if keys.get("m").map(String::as_str) == Some("1") {
                return;
            }
            let (k, p) = self.partial.take().unwrap();
            keys = k;
            payload = p;
        } else if keys.get("m").map(String::as_str) == Some("1") {
            self.partial = Some((keys, payload));
            return;
        }
        if keys.get("a").map(String::as_str) != Some("q") {
            self.commands_this_frame += 1;
        }
        let int = |keys: &HashMap<String, String>, name: &str, default: i64| {
            keys.get(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        match keys.get("a").map(String::as_str) {
            Some("q") | None => {}
            Some("T") => {
                let id = int(&keys, "i", 0) as u32;
                let (w, h) = (int(&keys, "s", 0) as u32, int(&keys, "v", 0) as u32);
                let pixels = decode(&keys, &payload);
                assert_eq!(pixels.len(), (w * h * 4) as usize, "a=T size mismatch");
                if keys.get("U").map(String::as_str) == Some("1") {
                    let (c, r) = (int(&keys, "c", 0) as u32, int(&keys, "r", 0) as u32);
                    assert!(c > 0 && r > 0, "a virtual placement says how many cells it spans");
                    self.transmits += 1;
                    self.pixels_this_frame += u64::from(w) * u64::from(h);
                    self.placements.retain(|p| p.image != id);
                    self.images.insert(id, Image { width: w, height: h, pixels });
                    self.virtual_placements.insert(id, (c, r));
                    return;
                }
                assert_eq!(keys.get("C").map(String::as_str), Some("1"), "C=1 keeps the cursor put");
                assert!(keys.contains_key("p"), "placements need an explicit id");
                self.transmits += 1;
                self.transient_transmits += usize::from(keys.get("N").map(String::as_str) == Some("1"));
                self.pixels_this_frame += u64::from(w) * u64::from(h);
                // both kitty and ghostty main drop every placement of a re-transmitted id
                self.placements.retain(|p| p.image != id);
                self.images.insert(id, Image { width: w, height: h, pixels });
                let (row, col) = self.cursor;
                let offset = (int(&keys, "X", 0) as u32, int(&keys, "Y", 0) as u32);
                assert!(offset.0 < self.cell.0 && offset.1 < self.cell.1, "offset must stay inside the cell");
                self.placements.push(Placed {
                    image: id,
                    col,
                    row,
                    offset,
                    z: int(&keys, "z", 0) as i32,
                    seq: self.next_seq,
                });
                self.next_seq += 1;
            }
            Some("t") => {
                let id = int(&keys, "i", 0) as u32;
                let (w, h) = (int(&keys, "s", 0) as u32, int(&keys, "v", 0) as u32);
                let pixels = decode(&keys, &payload);
                assert_eq!(pixels.len(), (w * h * 4) as usize, "a=t size mismatch");
                self.transmits += 1;
                self.images.insert(id, Image { width: w, height: h, pixels });
            }
            Some("f") => {
                let id = int(&keys, "i", 0) as u32;
                assert_eq!(int(&keys, "r", 0), 1, "edits target the root frame");
                assert_eq!(int(&keys, "X", 0), 1, "edits replace pixels rather than blend");
                let (x, y) = (int(&keys, "x", 0) as u32, int(&keys, "y", 0) as u32);
                let (w, h) = (int(&keys, "s", 0) as u32, int(&keys, "v", 0) as u32);
                let pixels = decode(&keys, &payload);
                assert_eq!(pixels.len(), (w * h * 4) as usize, "a=f size mismatch");
                let image = self.images.get_mut(&id).expect("frame edits need a loaded image");
                assert!(x + w <= image.width && y + h <= image.height, "edit stays inside the image");
                for row in 0..h {
                    let dst = (((y + row) * image.width + x) * 4) as usize;
                    let src = (row * w * 4) as usize;
                    image.pixels[dst..dst + (w * 4) as usize].copy_from_slice(&pixels[src..src + (w * 4) as usize]);
                }
                self.frame_edits += 1;
                self.pixels_this_frame += u64::from(w) * u64::from(h);
            }
            Some("d") => match keys.get("d").map(String::as_str) {
                Some("A") => {
                    self.images.clear();
                    self.placements.clear();
                }
                Some("I") => {
                    let id = int(&keys, "i", 0) as u32;
                    self.images.remove(&id);
                    self.virtual_placements.remove(&id);
                    self.placements.retain(|p| p.image != id);
                }
                Some("i") => {
                    let id = int(&keys, "i", 0) as u32;
                    self.placement_deletes += 1;
                    self.placements.retain(|p| p.image != id);
                }
                other => panic!("unsimulated delete kind {other:?}"),
            },
            other => panic!("unsimulated action {other:?}"),
        }
    }

    /// The simulated terminal's background; translucent image pixels blend over it the way
    /// a real terminal blends them over its own background.
    pub(super) const BACKDROP: [u8; 3] = [30, 30, 30];

    pub(super) fn render(&self, width: u32, height: u32) -> Vec<u8> {
        let mut screen = vec![0u8; (width * height * 4) as usize];
        for px in screen.chunks_exact_mut(4) {
            px[..3].copy_from_slice(&Self::BACKDROP);
            px[3] = 255;
        }
        let mut ordered: Vec<&Placed> = self.placements.iter().collect();
        ordered.sort_by_key(|p| (p.z, p.seq));
        for (i, a) in ordered.iter().enumerate() {
            for b in &ordered[i + 1..] {
                if a.z == b.z && a.z != 0 {
                    let (ra, rb) = (self.rect_of(a), self.rect_of(b));
                    assert!(
                        !ra.intersects(rb),
                        "two patches overlap at the same z={}, order would be terminal-defined",
                        a.z
                    );
                }
            }
        }
        for placed in ordered {
            let image = &self.images[&placed.image];
            let x0 = (placed.col - 1) * self.cell.0 + placed.offset.0;
            let y0 = (placed.row - 1) * self.cell.1 + placed.offset.1;
            for row in 0..image.height {
                let y = y0 + row;
                if y >= height {
                    break;
                }
                let w = image.width.min(width.saturating_sub(x0));
                if w == 0 {
                    break;
                }
                let src = (row * image.width * 4) as usize;
                let dst = ((y * width + x0) * 4) as usize;
                let target = &mut screen[dst..dst + (w * 4) as usize];
                for (out, px) in target.chunks_exact_mut(4).zip(image.pixels[src..src + (w * 4) as usize].chunks_exact(4)) {
                    let a = u32::from(px[3]);
                    for c in 0..3 {
                        out[c] = ((u32::from(px[c]) * a + u32::from(out[c]) * (255 - a) + 127) / 255) as u8;
                    }
                    out[3] = 255;
                }
            }
        }
        let (cw, ch) = self.cell;
        for (&(row, col), cell) in &self.cells {
            let Some(image) = self.images.get(&cell.image) else { continue };
            let (cols, rows) = self.virtual_placements[&cell.image];
            let (ri, ci) = (cell.row.expect("placeholder row"), cell.col.expect("placeholder column"));
            assert!(ri < rows && ci < cols, "placeholder cell outside its placement");
            assert_eq!((image.width, image.height), (cols * cw, rows * ch), "the placeholder image would be resampled");
            for y in 0..ch {
                let sy = (row - 1) * ch + y;
                if sy >= height {
                    break;
                }
                for x in 0..cw {
                    let sx = (col - 1) * cw + x;
                    if sx >= width {
                        break;
                    }
                    let src = (((ri * ch + y) * image.width + ci * cw + x) * 4) as usize;
                    let dst = ((sy * width + sx) * 4) as usize;
                    let px = &image.pixels[src..src + 4];
                    let out = &mut screen[dst..dst + 4];
                    let a = u32::from(px[3]);
                    for c in 0..3 {
                        out[c] = ((u32::from(px[c]) * a + u32::from(out[c]) * (255 - a) + 127) / 255) as u8;
                    }
                    out[3] = 255;
                }
            }
        }
        screen
    }

    fn rect_of(&self, placed: &Placed) -> crate::surfaces::Rect {
        let image = &self.images[&placed.image];
        crate::surfaces::Rect {
            x: (placed.col - 1) * self.cell.0 + placed.offset.0,
            y: (placed.row - 1) * self.cell.1 + placed.offset.1,
            w: image.width,
            h: image.height,
        }
    }
}

fn decode(keys: &HashMap<String, String>, payload: &[u8]) -> Vec<u8> {
    let raw = BASE64.decode(payload).expect("payload is base64");
    let len = |k: &str| keys[k].parse::<usize>().unwrap();
    let data = match keys.get("t").map(String::as_str) {
        Some("d") | None => raw,
        Some("f") => std::fs::read(String::from_utf8(raw).unwrap()).expect("payload file exists"),
        Some("t") => {
            let path = String::from_utf8(raw).unwrap();
            assert!(path.contains("tty-graphics-protocol"), "temporary file is named for deletion");
            let data = std::fs::read(&path).expect("payload file exists");
            let _ = std::fs::remove_file(&path);
            data
        }
        Some("s") => read_shm(&String::from_utf8(raw).unwrap(), len("s") * len("v") * 4),
        other => panic!("unsimulated transmission medium {other:?}"),
    };
    match keys.get("o").map(String::as_str) {
        Some("z") => miniz_oxide::inflate::decompress_to_vec_zlib(&data).expect("zlib payload"),
        _ => data,
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn read_shm(name: &str, len: usize) -> Vec<u8> {
    let fd = rustix::shm::open(name, rustix::shm::OFlags::RDONLY, rustix::fs::Mode::empty())
        .expect("shm object exists");
    let data = unsafe {
        let ptr = rustix::mm::mmap(
            std::ptr::null_mut(),
            len,
            rustix::mm::ProtFlags::READ,
            rustix::mm::MapFlags::SHARED,
            &fd,
            0,
        )
        .expect("shm maps");
        let data = std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec();
        rustix::mm::munmap(ptr, len).unwrap();
        data
    };
    // terminals unlink the object once they have read it
    let _ = rustix::shm::unlink(name);
    data
}

/// Replaces every complete tmux passthrough (`ESC P tmux ; ... ESC \`, inner escapes
/// doubled) with what the terminal behind tmux would receive.
fn unwrap_passthrough(buf: &mut Vec<u8>) {
    const START: &[u8] = b"\x1bPtmux;";
    let mut from = 0;
    while let Some(at) = buf[from..].windows(START.len()).position(|w| w == START).map(|p| p + from) {
        let mut inner = Vec::new();
        let mut j = at + START.len();
        let end = loop {
            let Some(&b) = buf.get(j) else { return };
            if b != 0x1b {
                inner.push(b);
                j += 1;
                continue;
            }
            match buf.get(j + 1) {
                None => return,
                Some(0x1b) => {
                    inner.push(0x1b);
                    j += 2;
                }
                Some(b'\\') => break j + 2,
                Some(_) => {
                    inner.push(0x1b);
                    j += 1;
                }
            }
        };
        buf.splice(at..end, inner.iter().copied());
        from = at + inner.len();
    }
}

fn token(buf: &[u8]) -> Token {
    if buf[0] != 0x1b {
        let len = match buf[0] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        if buf.len() < len {
            return Token::Need;
        }
        return match std::str::from_utf8(&buf[..len]) {
            Ok(text) => Token::Text(text.chars().next().unwrap(), len),
            Err(_) => Token::Skip(1),
        };
    }
    let Some(&kind) = buf.get(1) else {
        return Token::Need;
    };
    match kind {
        b'[' => {
            for (i, &b) in buf.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&b) {
                    return Token::Csi(i);
                }
            }
            Token::Need
        }
        b']' | b'P' => {
            let mut i = 2;
            while i + 1 < buf.len() {
                if buf[i] == 0x1b && buf[i + 1] == b'\\' {
                    return Token::Osc(i + 2);
                }
                if buf[i] == 0x07 {
                    return Token::Osc(i + 1);
                }
                i += 1;
            }
            Token::Need
        }
        b'_' => {
            let mut sep = None;
            let mut i = 2;
            while i + 1 < buf.len() {
                if buf[i] == b';' && sep.is_none() {
                    sep = Some(i);
                }
                if buf[i] == 0x1b && buf[i + 1] == b'\\' {
                    return Token::Apc(sep.unwrap_or(i), i);
                }
                i += 1;
            }
            Token::Need
        }
        _ => Token::Skip(2),
    }
}
