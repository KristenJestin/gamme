//! The home screen: the pieces of the library, the last played first, with the means to import
//! one (a file picker, or a file dropped on the window) and to remove one.
//!
//! The library's work (reading, copying, listing) is blocking and runs off the main thread; the
//! screen only puts its results in place and says them in toasts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_design_system::{
    Button, Confirm, EmptyLine, Icon, ListCard, ListRow, Listing, Menu, PageHeader, Section, Toast,
    ToastStack, Type as _, icon, palette, space, toast,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::icons;
use crate::library::{ImportError, Library, Piece};

/// A row of the list: a piece and the menu of its "⋯" button, which keeps its state while the
/// row stays.
#[derive(Clone)]
struct Row {
    piece: Piece,
    menu: Entity<Menu>,
}

pub struct Home {
    /// `None` when there is no home folder to keep a library in.
    library: Option<Library>,
    pieces: Vec<Piece>,
    /// Whether the library has been read once: until then the list is not empty, only unknown.
    loaded: bool,
    listing: Listing<Row>,
    menus: HashMap<String, Entity<Menu>>,
    /// The piece whose removal is asked, and whether the question is still open (it fades away
    /// as it closes, so the piece is kept to say its name until then).
    asking: Option<(Piece, bool)>,
    /// How many imports are under way.
    importing: usize,
    toasts: Entity<ToastStack>,
}

impl Home {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let library = Library::open();
        if let Some(library) = library.clone() {
            cx.spawn(async move |this, cx| {
                let pieces = cx
                    .background_executor()
                    .spawn(async move { library.list() })
                    .await;
                this.update(cx, |this, cx| this.set_pieces(pieces, cx)).ok();
            })
            .detach();
        }
        Self {
            // With no library there is nothing to read: the list is empty, and importing says why
            // it cannot.
            loaded: library.is_none(),
            library,
            pieces: Vec::new(),
            listing: Listing::default(),
            menus: HashMap::new(),
            asking: None,
            importing: 0,
            toasts: cx.new(ToastStack::new),
        }
    }

    /// Takes the library as it is now, and gives each piece the menu of its row.
    fn set_pieces(&mut self, pieces: Vec<Piece>, cx: &mut Context<Self>) {
        let home = cx.entity().downgrade();
        for piece in &pieces {
            self.menus.entry(piece.id.clone()).or_insert_with(|| {
                let (id, home) = (piece.id.clone(), home.clone());
                let more = ElementId::Name(format!("piece-{id}-more").into());
                let remove = ElementId::Name(format!("piece-{id}-remove").into());
                cx.new(|cx| {
                    Menu::new(more, "Actions of this piece", cx).danger(
                        remove,
                        "Remove…",
                        move |_, cx| {
                            home.update(cx, |home, cx| home.ask_removal(&id, cx)).ok();
                        },
                    )
                })
            });
        }
        // The rows that are leaving keep their own handle on their menu.
        self.menus
            .retain(|id, _| pieces.iter().any(|piece| &piece.id == id));
        self.pieces = pieces;
        self.loaded = true;
        cx.notify();
    }

    /// The file picker; what it returns is imported.
    fn pick(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| match chosen.await {
            Ok(Ok(Some(paths))) => {
                this.update(cx, |this, cx| this.import(paths, cx)).ok();
            }
            Ok(Err(error)) => {
                this.update(cx, |_, cx| {
                    toast(
                        Toast::failure("The file picker did not open").detail(error.to_string()),
                        cx,
                    );
                })
                .ok();
            }
            // Cancelled.
            _ => {}
        })
        .detach();
    }

    /// Imports files, one after the other, off the main thread, then lists the library again.
    fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let Some(library) = self.library.clone() else {
            toast(
                Toast::failure("Nowhere to keep the pieces")
                    .detail("Gamme found no home folder to keep its library in."),
                cx,
            );
            return;
        };
        self.importing += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (results, pieces) = cx
                .background_executor()
                .spawn(async move {
                    let results: Vec<_> = paths.iter().map(|path| library.import(path)).collect();
                    (results, library.list())
                })
                .await;
            this.update(cx, |this, cx| {
                this.importing -= 1;
                this.set_pieces(pieces, cx);
                announce(results, cx);
            })
            .ok();
        })
        .detach();
    }

    fn ask_removal(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(piece) = self.pieces.iter().find(|piece| piece.id == id) {
            self.asking = Some((piece.clone(), true));
            cx.notify();
        }
    }

    fn cancel_removal(&mut self, cx: &mut Context<Self>) {
        if let Some((_, open)) = &mut self.asking {
            *open = false;
            cx.notify();
        }
    }

    fn remove_asked(&mut self, cx: &mut Context<Self>) {
        let Some((piece, true)) = self.asking.clone() else {
            return;
        };
        self.cancel_removal(cx);
        let Some(library) = self.library.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (removed, pieces) = cx
                .background_executor()
                .spawn(async move {
                    let removed = library.remove(&piece.id).map(|()| piece);
                    (removed, library.list())
                })
                .await;
            this.update(cx, |this, cx| {
                this.set_pieces(pieces, cx);
                match removed {
                    Ok(piece) => toast(Toast::success("Removed ").quoted_title(&piece.title), cx),
                    Err(error) => toast(
                        Toast::failure("The piece was not removed").detail(error.to_string()),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
    }

    fn row(row: &Row, ink: Hsla, now: u64) -> ListRow {
        let piece = &row.piece;
        let played = match piece.last_played {
            Some(at) => format!("played {}", ago(now, at)),
            None => "not played yet".to_owned(),
        };
        // A piece with no composer says what its kind gives, which MIDI has less of.
        let line = match (&piece.composer, piece.kind.engraved()) {
            (Some(composer), _) => Some(composer.clone()),
            (None, false) => Some("Falling notes only, no score".to_owned()),
            (None, true) => None,
        };
        ListRow::new(
            ElementId::Name(format!("piece-{}", piece.id).into()),
            piece.title.clone(),
        )
        .leading(icon(icons::MUSIC).text_color(ink))
        .tag(piece.kind.label())
        .when_some(line, |row, line| row.line(line))
        .meta(played)
        .more(row.menu.clone())
    }
}

/// What the imports said: one line for the pieces that came in, one toast for each that did not.
fn announce(results: Vec<Result<Piece, ImportError>>, cx: &mut App) {
    let mut imported = Vec::new();
    for result in results {
        match result {
            Ok(piece) => imported.push(piece),
            Err(error) => toast(Toast::failure("Not imported").detail(error.to_string()), cx),
        }
    }
    match imported.as_slice() {
        [] => {}
        [piece] => toast(Toast::success("Imported ").quoted_title(&piece.title), cx),
        several => toast(
            Toast::success(format!("Imported {} pieces", several.len())),
            cx,
        ),
    }
}

/// When a piece was played, in the words of a list: "just now", "5 min ago", "3 h ago", "2 d ago".
fn ago(now: u64, then: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

impl Render for Home {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let reduce = cx.reduce_motion();
        self.listing.sync(
            self.pieces.iter().filter_map(|piece| {
                let menu = self.menus.get(&piece.id)?.clone();
                Some((
                    piece.id.clone(),
                    Row {
                        piece: piece.clone(),
                        menu,
                    },
                ))
            }),
            cx.background_executor().now(),
            reduce,
        );
        let home = cx.entity().downgrade();
        let confirm_home = home.clone();

        let mut list = ListCard::new("pieces-list");
        for slot in self.listing.rows() {
            list = list.item(
                ElementId::Name(format!("piece-{}-item", slot.key).into()),
                slot.placement,
                Self::row(slot.value, p.faint, now),
            );
        }
        let empty = self.loaded && self.listing.rows().next().is_none();
        let importing = self.importing > 0;
        let import = Button::secondary("import", if importing { "Importing…" } else { "Import" })
            .leading(Icon::Plus)
            .tooltip("Import a MusicXML, MEI or MIDI file, or drop it on the window")
            .disabled(importing)
            .on_click(cx.listener(|this, _, _, cx| this.pick(cx)));

        let mut section = Section::new("pieces", "Pieces")
            .sentence("What you play, the last played first.")
            .action(import);
        if !self.pieces.is_empty() {
            section = section.count(self.pieces.len().to_string());
        }
        let section = if empty {
            section.child(EmptyLine::new(
                "pieces-empty",
                icons::MUSIC,
                "No piece yet. Import a MusicXML, MEI or MIDI file, or drop it on this window.",
            ))
        } else if self.loaded {
            section.child(list)
        } else {
            section
        };

        let (question, open) = match &self.asking {
            Some((piece, open)) => (format!("Remove “{}”?", piece.title), *open),
            None => (String::new(), false),
        };

        div()
            .id("home")
            .size_full()
            .bg(p.bg)
            .overflow_y_scroll()
            .flex()
            .justify_center()
            // A file dragged over the window says where it would go.
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(palette(cx).hover))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import(paths.paths().to_vec(), cx);
            }))
            .child(
                div()
                    .w(space::COLUMN)
                    .max_w_full()
                    .min_h_full()
                    .px(space::XL)
                    .pt(space::XXXL)
                    .flex()
                    .flex_col()
                    .gap(space::XL)
                    .child(PageHeader::new("home-header", "Gamme").below(
                        div().ui_text().text_color(p.mute).child(
                            "Learn the piano: the score first, the falling notes as a help.",
                        ),
                    ))
                    .child(section)
                    .child(div().flex_1()),
            )
            .child(
                Confirm::new("remove-piece", question)
                    .sentence(
                        "The piece leaves your library, with its copy of the file. The file you \
                         imported it from is not touched.",
                    )
                    .action("Remove piece")
                    .open(open)
                    .on_confirm(move |_, cx| {
                        confirm_home
                            .update(cx, |home, cx| home.remove_asked(cx))
                            .ok();
                    })
                    .on_cancel(move |_, cx| {
                        home.update(cx, |home, cx| home.cancel_removal(cx)).ok();
                    }),
            )
            .child(self.toasts.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn a_time_is_said_in_the_biggest_unit_it_fills() {
        assert_eq!(ago(1_000, 1_000), "just now");
        assert_eq!(ago(1_059, 1_000), "just now");
        assert_eq!(ago(1_060, 1_000), "1 min ago");
        assert_eq!(ago(1_000 + 3_599, 1_000), "59 min ago");
        assert_eq!(ago(1_000 + 3_600, 1_000), "1 h ago");
        assert_eq!(ago(1_000 + 86_400 * 3, 1_000), "3 d ago");
        // A clock set back is not a time to come.
        assert_eq!(ago(10, 1_000), "just now");
    }
}
