//! The score on screen, as the play screen holds it: its pages as images, and over them, drawn
//! every frame, the measure being played, the cursor and the notes that sound.
//!
//! The pages are rasterized off the main thread, again when the window's width, the zoom or the
//! theme's ink changes; meanwhile the images already there are drawn at the new size, so a
//! resize never waits for them. The play screen passes on `+` and `-` (zoom out and back) and
//! `0` (fit the width).

use std::sync::Arc;
use std::time::Instant;

use gpui_design_system::theme::{radius, target_palette};
use gpui_design_system::{Skeleton, awaiting, palette, space};
use gpui_kit::*;

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

/// What the pages are rasterized for: their width in device pixels and their ink.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    width: u32,
    ink: Rgba,
}

pub struct ScoreView {
    score: Arc<Score>,
    playhead: Box<dyn Playhead>,
    zoom: f32,
    images: Vec<Arc<RenderImage>>,
    /// What `images` were drawn for.
    drawn: Option<Look>,
    /// What is being drawn now, off the main thread.
    drawing: Option<Look>,
    /// Images replaced, given back to the window at the next frame.
    retired: Vec<Arc<RenderImage>>,
    /// Whether the cursor was brought into view since the score was shown.
    shown: bool,
    /// The measure (its number in the order of play) the view was last at: a still cursor that
    /// moves to another one (a seek) is brought into view.
    followed: Option<usize>,
    scroll: ScrollHandle,
}

impl ScoreView {
    pub fn new(score: Arc<Score>, playhead: Box<dyn Playhead>) -> Self {
        Self {
            score,
            playhead,
            zoom: 1.,
            images: Vec::new(),
            drawn: None,
            drawing: None,
            retired: Vec::new(),
            shown: false,
            followed: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Brings the cursor into view at the next frame, as when the score is first shown: the
    /// score comes back from another view, or the cursor jumped while it was still.
    pub fn reveal(&mut self, cx: &mut Context<Self>) {
        self.shown = false;
        cx.notify();
    }

    /// `+` or `=`, `-`, `0`: zooms, and says whether the key was one of them.
    pub fn zoom_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let zoom = match key {
            "+" | "=" => self.zoom * ZOOM_STEP,
            "-" => self.zoom / ZOOM_STEP,
            "0" => 1.,
            _ => return false,
        };
        self.zoom = zoom.clamp(ZOOM_MIN, 1.);
        cx.notify();
        true
    }

    /// The width a page is drawn at, in logical pixels.
    fn page_width(&self, window: &Window) -> f32 {
        let room = f32::from(window.viewport_size().width) - 2. * f32::from(PAGE_GAP);
        room.clamp(1., PAGE_MAX) * self.zoom
    }

    /// Rasterizes the pages for `look` unless they are, or are being: one at a time, the last
    /// look asked for drawn once the one under way is done.
    fn redraw(&mut self, look: Look, cx: &mut Context<Self>) {
        if self.drawn == Some(look) || self.drawing.is_some() {
            return;
        }
        self.drawing = Some(look);
        let score = self.score.clone();
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

    /// Scrolls toward the cursor's system, a third of the way down the view: at once with
    /// `jump`, a step of the way otherwise. Whether the view is there.
    fn follow(&self, cursor: Cursor, page_width: f32, jump: bool) -> bool {
        let score = &self.score;
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

    fn pages(&mut self, page_width: f32, window: &Window, cx: &App) -> Vec<AnyElement> {
        let p = palette(cx);
        let score = self.score.clone();
        let position = self.playhead.position();
        let moment = score.at(position);
        let measure = moment.measure.map(|m| m.number);
        if measure != self.followed {
            self.followed = measure;
            self.shown &= self.playhead.moving();
        }
        if let Some(cursor) = moment.cursor
            && !self.images.is_empty()
            && (self.playhead.moving() || !self.shown)
        {
            // The first jump is tried again until it lands: while the pages fade in, the
            // room they take grows, and an offset beyond it is cut short.
            let there = self.follow(cursor, page_width, !self.shown);
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
        let page_width = self.page_width(window);
        let look = Look {
            width: (page_width * window.scale_factor()).round() as u32,
            // The ink the theme heads to: a theme's fade is not rasterized frame by frame.
            ink: target_palette(cx).text.to_rgb(),
        };
        self.redraw(look, cx);
        let pages = self.pages(page_width, window, cx);
        if self.playhead.moving() && self.playhead.position() <= self.score.duration() {
            window.request_animation_frame();
        }
        let pending = pages.is_empty();
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
        div()
            .id("score-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(awaiting("score-pages", pending, skeleton, content))
    }
}

/// What `tools/capture.sh` prints of a run (`GAMME_TIMING`).
pub fn timing(what: std::fmt::Arguments) {
    if std::env::var_os("GAMME_TIMING").is_some() {
        eprintln!("gamme: {what}");
    }
}
