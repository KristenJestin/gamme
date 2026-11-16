//! The score on screen: its pages as images, and over them, drawn every frame, the measure being
//! played, the cursor and the notes that sound.
//!
//! The pages are rasterized off the main thread, again when the window's width, the zoom or the
//! theme's ink changes; meanwhile the images already there are drawn at the new size, so a
//! resize never waits for them. Keys: `+` and `-` zoom out and back, `0` fits the width.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_design_system::theme::{radius, target_palette};
use gpui_design_system::{EmptyLine, PageHeader, Skeleton, Type as _, awaiting, palette, space};
use gpui_kit::*;

use crate::icons;
use crate::raster;
use crate::score::{Cursor, Score};

/// Around the pages and between them.
const PAGE_GAP: Pixels = space::XL;
/// The widest a page is drawn, at full zoom.
const PAGE_MAX: f32 = 1600.;
const ZOOM_STEP: f32 = 1.15;
const ZOOM_MIN: f32 = 0.5;
/// How far the view goes toward the cursor at each frame while it follows it.
const FOLLOW: f32 = 0.12;

/// Where the play is, read by the score at every frame. The clock is one.
pub trait Playhead: 'static {
    /// The position in the piece, in ms at the piece's own tempo, as Verovio's timemap counts.
    fn position(&self) -> f64;
    /// Whether it moves: the score is drawn every frame, the view following the cursor, while
    /// it does.
    fn moving(&self) -> bool;
}

/// A position that does not move.
pub struct Still(pub f64);

impl Playhead for Still {
    fn position(&self) -> f64 {
        self.0
    }

    fn moving(&self) -> bool {
        false
    }
}

/// The piece played through at its own tempo from a position: a stand-in until the clock, to see
/// the cursor move.
pub struct Preview {
    from: f64,
    started: Instant,
}

impl Preview {
    pub fn new(from: f64) -> Self {
        Self {
            from,
            started: Instant::now(),
        }
    }
}

impl Playhead for Preview {
    fn position(&self) -> f64 {
        self.from + self.started.elapsed().as_secs_f64() * 1e3
    }

    fn moving(&self) -> bool {
        true
    }
}

/// A piece to open from the command line, until the library opens them:
/// `gamme <piece> [--at <seconds>] [--play]`.
#[derive(Debug, PartialEq)]
pub struct Opening {
    pub piece: PathBuf,
    /// Where the cursor starts, in ms.
    pub at: f64,
    /// Whether the cursor moves from there at the piece's tempo.
    pub play: bool,
}

impl Opening {
    pub fn parse(piece: String, mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut opening = Self {
            piece: PathBuf::from(piece),
            at: 0.,
            play: false,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--play" => opening.play = true,
                "--at" => {
                    let seconds = args
                        .next()
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|s| s.is_finite() && *s >= 0.)
                        .ok_or("--at takes a number of seconds")?;
                    opening.at = seconds * 1e3;
                }
                other => return Err(format!("unknown argument {other} (see gamme --help)")),
            }
        }
        Ok(opening)
    }

    fn playhead(&self) -> Box<dyn Playhead> {
        match self.play {
            true => Box::new(Preview::new(self.at)),
            false => Box::new(Still(self.at)),
        }
    }
}

enum Piece {
    Engraving,
    Failed(SharedString),
    Ready(Arc<Score>),
}

/// What the pages are rasterized for: their width in device pixels and their ink.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    width: u32,
    ink: Rgba,
}

pub struct ScoreView {
    title: SharedString,
    file: SharedString,
    piece: Piece,
    playhead: Box<dyn Playhead>,
    zoom: f32,
    images: Vec<Arc<RenderImage>>,
    /// What `images` were drawn for.
    drawn: Option<Look>,
    /// What is being drawn now, off the main thread.
    drawing: Option<Look>,
    /// Images replaced, given back to the window at the next frame.
    retired: Vec<Arc<RenderImage>>,
    /// Whether the cursor was brought into view once.
    shown: bool,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl ScoreView {
    pub fn new(opening: Opening, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let title = opening.piece.file_stem().map_or_else(
            || "Piece".into(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let file = opening
            .piece
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let piece = opening.piece.clone();
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let opened = cx
                .background_executor()
                .spawn(async move { Score::open(&piece) })
                .await;
            this.update(cx, |this, cx| {
                this.piece = match opened {
                    Ok(score) => {
                        timing(format_args!(
                            "engraved {} pages in {} ms",
                            score.pages().len(),
                            started.elapsed().as_millis()
                        ));
                        Piece::Ready(Arc::new(score))
                    }
                    Err(error) => Piece::Failed(error.into()),
                };
                cx.notify();
            })
        })
        .detach();
        Self {
            title: title.into(),
            file: file.into(),
            piece: Piece::Engraving,
            playhead: opening.playhead(),
            zoom: 1.,
            images: Vec::new(),
            drawn: None,
            drawing: None,
            retired: Vec::new(),
            shown: false,
            scroll: ScrollHandle::new(),
            focus,
        }
    }

    /// Hands the score another playhead: the clock, when a piece is played.
    #[expect(dead_code, reason = "the clock is not there yet")]
    pub fn set_playhead(&mut self, playhead: Box<dyn Playhead>, cx: &mut Context<Self>) {
        self.playhead = playhead;
        cx.notify();
    }

    /// The width a page is drawn at, in logical pixels.
    fn page_width(&self, window: &Window) -> f32 {
        let room = f32::from(window.viewport_size().width) - 2. * f32::from(PAGE_GAP);
        room.clamp(1., PAGE_MAX) * self.zoom
    }

    /// Rasterizes the pages for `look` unless they are, or are being: one at a time, the last
    /// look asked for drawn once the one under way is done.
    fn redraw(&mut self, score: &Arc<Score>, look: Look, cx: &mut Context<Self>) {
        if self.drawn == Some(look) || self.drawing.is_some() {
            return;
        }
        self.drawing = Some(look);
        let score = score.clone();
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let images = cx
                .background_executor()
                .spawn(async move { raster::pages(&score, look.width, look.ink) })
                .await;
            this.update(cx, |this, cx| {
                timing(format_args!(
                    "rasterized {} px wide in {} ms",
                    look.width,
                    started.elapsed().as_millis()
                ));
                let old = std::mem::replace(&mut this.images, images);
                this.retired.extend(old);
                this.drawn = Some(look);
                this.drawing = None;
                cx.notify();
            })
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let zoom = match event.keystroke.key.as_str() {
            "+" | "=" => self.zoom * ZOOM_STEP,
            "-" => self.zoom / ZOOM_STEP,
            "0" => 1.,
            _ => return,
        };
        self.zoom = zoom.clamp(ZOOM_MIN, 1.);
        cx.notify();
    }

    /// Scrolls toward the cursor's system, a third of the way down the view: at once with
    /// `jump`, a step of the way otherwise. Whether the view is there.
    fn follow(&self, score: &Score, cursor: Cursor, page_width: f32, jump: bool) -> bool {
        let gap = f32::from(PAGE_GAP);
        let height = |page: usize| {
            let (width, height) = score.page_size(page);
            height * page_width / width
        };
        let top: f32 = gap + (0..cursor.page).map(|page| height(page) + gap).sum::<f32>();
        let bottom: f32 = gap
            + (0..score.pages().len())
                .map(|page| height(page) + gap)
                .sum::<f32>();
        let (width, _) = score.page_size(cursor.page);
        let y = top + cursor.top * page_width / width;
        let view = f32::from(self.scroll.bounds().size.height);
        let target = (y - view / 3.).min(bottom - view).max(0.);
        let current = -f32::from(self.scroll.offset().y);
        if (target - current).abs() < 1. {
            return true;
        }
        let next = match jump {
            true => target,
            false => current + (target - current) * FOLLOW,
        };
        self.scroll.set_offset(point(px(0.), px(-next)));
        false
    }

    fn pages(
        &mut self,
        score: &Score,
        page_width: f32,
        window: &Window,
        cx: &App,
    ) -> Vec<AnyElement> {
        let p = palette(cx);
        let position = self.playhead.position();
        let moment = score.at(position);
        if let Some(cursor) = moment.cursor
            && !self.images.is_empty()
            && (self.playhead.moving() || !self.shown)
        {
            // The first jump is tried again until it lands: while the pages fade in, the
            // room they take grows, and an offset beyond it is cut short.
            let there = self.follow(score, cursor, page_width, !self.shown);
            self.shown |= there;
            if !self.shown {
                window.request_animation_frame();
            }
        }
        self.images
            .iter()
            .enumerate()
            .map(|(page, image)| {
                let (width, height) = score.page_size(page);
                let k = page_width / width;
                let place = |left: f32, top: f32, w: f32, h: f32| {
                    div()
                        .absolute()
                        .left(px(left * k))
                        .top(px(top * k))
                        .w(px(w * k))
                        .h(px(h * k))
                };
                let band = moment.measure.filter(|m| m.page == page).map(|m| {
                    place(m.area.left, m.area.top, m.area.width(), m.area.height())
                        .rounded(px(4.))
                        .bg(p.run.opacity(0.12))
                });
                let heads = moment
                    .sounding
                    .iter()
                    .filter(|head| head.page == page)
                    .map(|head| {
                        let a = head.area;
                        div()
                            .absolute()
                            .left(px(a.left * k - 2.))
                            .top(px(a.top * k - 2.))
                            .w(px(a.width() * k + 4.))
                            .h(px(a.height() * k + 4.))
                            .rounded(px(3.))
                            .bg(p.run.opacity(0.45))
                    });
                let line = moment.cursor.filter(|c| c.page == page).map(|c| {
                    div()
                        .absolute()
                        .left(px(c.x * k - 1.))
                        .top(px(c.top * k - 6.))
                        .w(px(2.))
                        .h(px((c.bottom - c.top) * k + 12.))
                        .rounded(px(1.))
                        .bg(p.run)
                });
                div()
                    .id(("score-page", page))
                    .relative()
                    .flex_none()
                    .w(px(page_width))
                    .h(px(height * k))
                    .rounded(radius::CARD)
                    .bg(p.card)
                    .overflow_hidden()
                    .child(img(image.clone()).size_full())
                    .children(band)
                    .children(heads)
                    .children(line)
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for ScoreView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for old in self.retired.drain(..) {
            cx.drop_image(old, Some(window));
        }
        let p = palette(cx);
        let page_width = self.page_width(window);
        let mut status = "Engraving…".to_owned();
        let mut pages = Vec::new();
        let mut failed = None;
        match &self.piece {
            Piece::Engraving => {}
            Piece::Failed(error) => {
                failed = Some(error.clone());
                status = "Not engraved".into();
            }
            Piece::Ready(score) => {
                let score = score.clone();
                let look = Look {
                    width: (page_width * window.scale_factor()).round() as u32,
                    // The ink the theme heads to: a theme's fade is not rasterized frame by frame.
                    ink: target_palette(cx).text.to_rgb(),
                };
                self.redraw(&score, look, cx);
                pages = self.pages(&score, page_width, window, cx);
                let position = self.playhead.position().clamp(0., score.duration());
                let measure = score.at(position).measure.map(|m| m.name.clone());
                status = match measure {
                    Some(name) => format!(
                        "Measure {name}  ·  {} / {}",
                        clock(position),
                        clock(score.duration())
                    ),
                    None => clock(score.duration()),
                };
                if self.playhead.moving() && self.playhead.position() <= score.duration() {
                    window.request_animation_frame();
                }
            }
        }
        let pending = failed.is_none() && pages.is_empty();
        let header = PageHeader::new("score-header", self.title.clone())
            .path(self.file.clone())
            .below(div().ui_text().text_color(p.mute).child(status));
        let skeleton = div().flex().flex_col().items_center().py(PAGE_GAP).child(
            Skeleton::new("score-skeleton")
                .w(px(page_width))
                .h(px(page_width * 1.3)),
        );
        let content = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(PAGE_GAP)
            .py(PAGE_GAP)
            .children(pages);
        let body = match failed {
            Some(error) => div()
                .px(PAGE_GAP)
                .py(PAGE_GAP)
                .child(EmptyLine::new("score-failed", icons::MUSIC, error))
                .into_any_element(),
            None => awaiting("score-pages", pending, skeleton, content).into_any_element(),
        };
        div()
            .size_full()
            .bg(p.bg)
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex_none()
                    .px(space::XXL)
                    .pt(space::XL)
                    .pb(space::LG)
                    .border_b_1()
                    .border_color(p.line)
                    .child(header),
            )
            .child(
                div()
                    .id("score-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }
}

/// `m:ss`.
fn clock(ms: f64) -> String {
    let seconds = (ms / 1e3).max(0.).floor() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// What `tools/capture.sh` prints of a run (`GAMME_TIMING`).
fn timing(what: std::fmt::Arguments) {
    if std::env::var_os("GAMME_TIMING").is_some() {
        eprintln!("gamme: {what}");
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` attribute would come with it.
    use super::{Opening, Playhead, Preview, Still, clock};

    fn parse(line: &str) -> Result<Opening, String> {
        let mut words = line.split_whitespace().map(str::to_owned);
        Opening::parse(words.next().unwrap(), words)
    }

    #[test]
    fn a_piece_opens_at_a_position_still_or_playing() {
        assert_eq!(
            parse("piece.musicxml").unwrap(),
            Opening {
                piece: "piece.musicxml".into(),
                at: 0.,
                play: false
            }
        );
        let playing = parse("piece.mxl --at 12.5 --play").unwrap();
        assert_eq!((playing.at, playing.play), (12_500., true));
        for bad in ["p --at", "p --at -1", "p --at soon", "p --loud"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_still_playhead_stays_and_a_preview_moves_on() {
        let still = Still(1_000.);
        assert_eq!((still.position(), still.moving()), (1_000., false));
        let preview = Preview::new(2_000.);
        assert!(preview.moving() && preview.position() >= 2_000.);
    }

    #[test]
    fn the_clock_reads_minutes_and_seconds() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(65_400.), "1:05");
    }
}
