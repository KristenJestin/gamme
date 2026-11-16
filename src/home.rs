//! The home screen: the pieces to play. Empty until the library (the next issue) fills it.

use gpui_design_system::{EmptyLine, PageHeader, Section, Type as _, palette, space};
use gpui_kit::*;

use crate::icons;

pub struct Home;

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
                ),
        )
    }
}
