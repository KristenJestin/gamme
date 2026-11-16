//! The window's screens and the way between them: the home screen, a piece played, and the
//! settings. A row of the home screen opens its piece, its header's Settings button the settings;
//! the way back of either (or Escape) comes home, whose list is read again (the piece played is
//! now first).
//!
//! The toasts are the window's: one stack above whichever screen shows.

use gpui_design_system::ToastStack;
use gpui_kit::*;

use crate::home::{self, Home};
use crate::play_screen::{self, Opening, PlayScreen};
use crate::settings::{self, SettingsPage};

enum Screen {
    Home,
    Play {
        screen: Entity<PlayScreen>,
        _back: Subscription,
    },
    Settings {
        screen: Entity<SettingsPage>,
        _back: Subscription,
    },
}

pub struct Screens {
    home: Entity<Home>,
    screen: Screen,
    toasts: Entity<ToastStack>,
    _open: Subscription,
    _open_settings: Subscription,
}

impl Screens {
    /// The home screen, or `opening` played over it.
    pub fn new(opening: Option<Opening>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let toasts = cx.new(ToastStack::new);
        let home = cx.new(Home::new);
        let open = cx.subscribe_in(
            &home,
            window,
            |this, _, home::Open(library, piece), window, cx| {
                this.play(Opening::piece(library.clone(), piece.clone()), window, cx);
            },
        );
        let open_settings =
            cx.subscribe_in(&home, window, |this, _, home::OpenSettings, window, cx| {
                this.settings(window, cx)
            });
        let mut screens = Self {
            home,
            screen: Screen::Home,
            toasts,
            _open: open,
            _open_settings: open_settings,
        };
        if let Some(opening) = opening {
            screens.play(opening, window, cx);
        }
        screens
    }

    fn play(&mut self, opening: Opening, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.new(|cx| PlayScreen::new(opening, window, cx));
        let back = cx.subscribe(&screen, |this, _, play_screen::Back, cx| this.home(cx));
        self.screen = Screen::Play {
            screen,
            _back: back,
        };
        cx.notify();
    }

    fn settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.new(|cx| SettingsPage::new(window, cx));
        let back = cx.subscribe(&screen, |this, _, settings::Back, cx| this.home(cx));
        self.screen = Screen::Settings {
            screen,
            _back: back,
        };
        cx.notify();
    }

    fn home(&mut self, cx: &mut Context<Self>) {
        // The play screen goes, and its piano with it.
        self.screen = Screen::Home;
        self.home.update(cx, |home, cx| home.refresh(cx));
        cx.notify();
    }
}

impl Render for Screens {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let screen = match &self.screen {
            Screen::Home => self.home.clone().into_any_element(),
            Screen::Play { screen, .. } => screen.clone().into_any_element(),
            Screen::Settings { screen, .. } => screen.clone().into_any_element(),
        };
        div().size_full().child(screen).child(self.toasts.clone())
    }
}
