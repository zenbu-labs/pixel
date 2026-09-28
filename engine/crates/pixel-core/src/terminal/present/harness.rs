use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::transmit_strategy::POOL;
use super::sim::{FRAME_MARK, Sim};
use super::{Identity, Presenter};
use super::super::{FrameTransport, SessionEnv, Terminal};
use crate::canvas::Canvas;
use crate::surfaces::Rect;
use crate::wrapper::Wrapper;

const COLS: u32 = 64;
pub(super) const ROWS: u32 = 30;
pub(super) const CELL: (u32, u32) = (10, 20);
pub(super) const GHOSTTY: Identity = Identity::Ghostty { version: (1, 3, 1) };

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn open_pty() -> (std::fs::File, std::fs::File, String) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let mut name = [0u8; 128];
    let ok = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            name.as_mut_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(ok, 0, "openpty failed");
    let end = name.iter().position(|&b| b == 0).unwrap();
    let path = String::from_utf8(name[..end].to_vec()).unwrap();
    let (master, slave) = unsafe {
        use std::os::unix::io::FromRawFd as _;
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    (master, slave, path)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_winsize(slave: &std::fs::File) {
    use std::os::unix::io::AsRawFd as _;
    let ws = libc::winsize {
        ws_row: ROWS as u16,
        ws_col: COLS as u16,
        ws_xpixel: (COLS * CELL.0) as u16,
        ws_ypixel: (ROWS * CELL.1) as u16,
    };
    let ok = unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    assert_eq!(ok, 0, "TIOCSWINSZ failed");
}

pub(super) struct Harness {
    pub(super) term: Terminal,
    captured: Arc<Mutex<Vec<u8>>>,
    consumed: usize,
    pub(super) sim: Sim,
    frames: usize,
    /// The simulated terminal only reads its input every this many frames, the way a
    /// real one falls behind a fast producer.
    pub(super) feed_every: usize,
    /// Frame areas the harness declares as belonging to an opaque embedded surface.
    pub(super) opaque: Vec<crate::surfaces::OpaqueArea>,
    /// Where the harness says UI was painted over a surface this frame.
    pub(super) ui_over_surfaces: Vec<Rect>,
    /// Hand canvases over as premultiplied, the way the engine does for a lone view.
    pub(super) premultiplied: bool,
}

impl Harness {
    pub(super) fn new(present: Presenter, transport: FrameTransport, identity: Identity) -> Self {
        Self::behind(Wrapper::None, present, transport, identity)
    }

    /// A terminal reached through a relay: every graphics command arrives wrapped and the
    /// simulated terminal unwraps it, as tmux would.
    pub(super) fn behind(wrapper: Wrapper, present: Presenter, transport: FrameTransport, identity: Identity) -> Self {
        let (master, slave, path) = open_pty();
        set_winsize(&slave);
        std::mem::forget(slave);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let mut reader = master;
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let mut term = Terminal::open(&path, wrapper, SessionEnv::of_session(HashMap::new())).unwrap();
        term.present = present;
        term.transport = transport;
        term.identity = identity;
        term.cell = Some(CELL);
        // Opening probed the transports with real graphics commands; the simulated terminal
        // reads everything up to a marker now so they do not count against the first frame.
        term.set_pointer_shape(FRAME_MARK).unwrap();
        let mut sim = Sim::new(CELL);
        let mut consumed = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while sim.frames < 1 {
            let fresh: Vec<u8> = captured.lock().unwrap()[consumed..].to_vec();
            consumed += fresh.len();
            sim.feed(&fresh);
            assert!(Instant::now() < deadline, "open probes never arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
        sim.frames = 0;
        sim.commands_this_frame = 0;
        sim.transmits = 0;
        sim.frame_edits = 0;
        Self {
            term,
            captured,
            consumed,
            sim,
            frames: 0,
            feed_every: 1,
            // opaque everywhere, but not a surface: pixels may change without damage rects
            opaque: vec![crate::surfaces::OpaqueArea { surface: None, rect: Rect::sized(W, H) }],
            ui_over_surfaces: Vec::new(),
            premultiplied: false,
        }
    }

    /// The pane has been quiet: lets the presenter fold or flatten its patches, then checks
    /// the screen the way a frame does.
    pub(super) fn idle(&mut self, canvas: &Canvas) {
        std::thread::sleep(IDLE_WAIT);
        let frame = crate::canvas::Frame { canvas, premultiplied: self.premultiplied, changed: &[], repainted: &[], opaque: &self.opaque, ui_over_surfaces: &self.ui_over_surfaces };
        self.term.flatten_if_idle(frame).unwrap();
        self.settle(canvas);
    }

    /// Image ids the simulated terminal holds at or above `from`.
    pub(super) fn image_ids_from(&self, from: u32) -> usize {
        self.sim.images.keys().filter(|id| **id >= from).count()
    }

    pub(super) fn frame(&mut self, canvas: &Canvas, damage: Option<&[Rect]>) {
        // No damage list means the whole frame may have changed, as the engine reports it.
        let whole = [Rect::sized(canvas.width, canvas.height)];
        match damage {
            Some(changed) => self.frame_repaint(canvas, &[], changed),
            None => self.frame_repaint(canvas, &whole, &[]),
        }
    }

    /// A frame where a view repainted whole (`repainted`) while `changed` rects hold pixels
    /// known to be new, the way the engine reports a chrome re-render.
    pub(super) fn frame_repaint(&mut self, canvas: &Canvas, repainted: &[Rect], changed: &[Rect]) {
        let frame = crate::canvas::Frame { canvas, premultiplied: self.premultiplied, changed, repainted, opaque: &self.opaque, ui_over_surfaces: &self.ui_over_surfaces };
        self.term.draw(frame).unwrap();
        self.settle(canvas);
    }

    fn settle(&mut self, canvas: &Canvas) {
        self.term.set_pointer_shape(FRAME_MARK).unwrap();
        self.frames += 1;
        if !self.frames.is_multiple_of(self.feed_every) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.sim.frames < self.frames {
            let fresh: Vec<u8> = {
                let captured = self.captured.lock().unwrap();
                captured[self.consumed..].to_vec()
            };
            self.consumed += fresh.len();
            self.sim.feed(&fresh);
            if self.sim.frames >= self.frames {
                break;
            }
            assert!(Instant::now() < deadline, "frame marker never arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
        let screen = self.sim.render(canvas.width, canvas.height);
        let expected = if self.premultiplied { composited(&straight(canvas)) } else { composited(canvas) };
        if screen != expected {
            let bad = screen
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .position(|(a, b)| a != b)
                .unwrap();
            let (x, y) = (bad as u32 % canvas.width, bad as u32 / canvas.width);
            panic!(
                "frame {}: screen diverged at ({x},{y}): screen {:?} vs canvas {:?}, {} images, {} placements",
                self.frames,
                &screen[bad * 4..bad * 4 + 4],
                &expected[bad * 4..bad * 4 + 4],
                self.sim.image_count(),
                self.sim.placement_count(),
            );
        }
        let tiles = self.term.patches.tiles.len();
        let live = &self.term.patches.live;
        assert!(
            live.iter().all(|a| live.iter().filter(|b| b.id == a.id).count() == 1),
            "frame {}: a patch id is live twice: {:?}",
            self.frames,
            live
        );
        assert!(self.sim.image_count() <= POOL as usize + 1 + tiles, "image pool overflowed");
        assert!(self.sim.placement_count() <= POOL as usize + 1 + tiles, "placements overflowed");
    }
}

/// Longer than the presenter's idle threshold.
const IDLE_WAIT: Duration = Duration::from_millis(450);

/// What a terminal shows for this canvas: its pixels blended over the simulated background.
fn composited(canvas: &Canvas) -> Vec<u8> {
    let mut out = canvas.pixels.clone();
    for px in out.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        for (c, back) in px[..3].iter_mut().zip(Sim::BACKDROP) {
            *c = ((u32::from(*c) * a + u32::from(back) * (255 - a) + 127) / 255) as u8;
        }
        px[3] = 255;
    }
    out
}

/// The canvas with colors scaled back up by alpha, what a premultiplied canvas means.
fn straight(canvas: &Canvas) -> Canvas {
    let mut out = Canvas::new(canvas.width, canvas.height);
    out.pixels.copy_from_slice(&canvas.pixels);
    super::straighten(&mut out.pixels, true);
    out
}

/// Scales colors down by alpha, producing what a premultiplied canvas holds for `color`.
pub(super) fn premultiply(color: [u8; 4]) -> [u8; 4] {
    let a = u32::from(color[3]);
    let scale = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
    [scale(color[0]), scale(color[1]), scale(color[2]), color[3]]
}

pub(super) struct Lcg(pub(super) u32);

impl Lcg {
    pub(super) fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        self.0 >> 8
    }

    pub(super) fn range(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

pub(super) fn fill(canvas: &mut Canvas, rect: Rect, color: [u8; 4]) {
    let rect = rect.clamped(canvas.width, canvas.height);
    for row in rect.y..rect.y + rect.h {
        let start = (row * canvas.width + rect.x) as usize * 4;
        for px in canvas.pixels[start..start + rect.w as usize * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&color);
        }
    }
}

pub(super) fn base_canvas(width: u32, height: u32, shade: u8) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    fill(&mut canvas, Rect::sized(width, height), [shade, shade / 2, 40, 255]);
    canvas
}

pub(super) const W: u32 = COLS * CELL.0;
pub(super) const H: u32 = (ROWS - 1) * CELL.1;
