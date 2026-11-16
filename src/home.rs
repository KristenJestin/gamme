//! The home screen: the pieces to play. Empty until the library (the next issue) fills it.

use gpui_design_system::{EmptyLine, PageHeader, Section, Type as _, palette, space};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{engraver, icons};

pub struct Home {
    /// Verovio's version, asked off the main thread: loading its resources is not free.
    engraver: Option<SharedString>,
}

impl Home {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let version = cx
                .background_executor()
                .spawn(async { engraver::version() })
                .await;
            this.update(cx, |this, cx| {
                this.engraver = Some(version.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self { engraver: None }
    }
}

impl Render for Home {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div().size_full().bg(p.bg).flex().justify_center().child(
            div()
                .w(space::COLUMN)
                .max_w_full()
                .px(space::XL)
                .pt(space::XXXL)
                .flex()
                .flex_col()
                .gap(space::XL)
                .child(
                    PageHeader::new("home-header", "Gamme").below(
                        div().ui_text().text_color(p.mute).child(
                            "Learn the piano: the score first, the falling notes as a help.",
                        ),
                    ),
                )
                .child(
                    Section::new("pieces", "Pieces")
                        .sentence("What you play, the latest first.")
                        .child(EmptyLine::new(
                            "pieces-empty",
                            icons::MUSIC,
                            "No piece yet. The ones you import will be listed here.",
                        )),
                )
                .child(div().flex_1())
                .when_some(self.engraver.clone(), |d, version| {
                    d.child(
                        div()
                            .pb(space::LG)
                            .mono(11.)
                            .text_color(p.faint)
                            .child(format!("Engraver: Verovio {version}")),
                    )
                }),
        )
    }
}
