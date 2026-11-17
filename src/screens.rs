//! The window's screens and the way between them: the home screen, a piece played, and the
//! settings. A row of the home screen opens its piece, its header's Settings button the settings;
//! the way back of either (or Escape) comes home, whose list is read again (the piece played is
//! now first).
//!
//! The toasts are the window's: one stack above whichever screen shows; MIDI in (`midi_in.rs`)
//! too, which listens to the keyboards for as long as the window is open, whichever screen shows.

use std::sync::Arc;

use gpui_design_system::ToastStack;
use gpui_kit::*;

use crate::config;
use crate::home::{self, Home};
use crate::midi_in::MidiIn;
use crate::midi_out::{Alsa, Ports};
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
    /// The machine's MIDI ports, one client for the screens that list or use them.
    ports: Arc<dyn Ports>,
    /// The player's keyboards, to the play screen; the settings page chooses which.
    midi_in: MidiIn,
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
        let ports: Arc<dyn Ports> = Arc::new(Alsa::default());
        // The settings are a file read: on MIDI in's own thread.
        let midi_in = MidiIn::start(ports.clone(), || config::load().0.midi_in);
        let mut screens = Self {
            home,
            screen: Screen::Home,
            ports,
            midi_in,
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
        let (ports, midi_in) = (self.ports.clone(), self.midi_in.clone());
        let screen = cx.new(|cx| PlayScreen::new(opening, ports, &midi_in, window, cx));
        let back = cx.subscribe(&screen, |this, _, play_screen::Back, cx| this.home(cx));
        self.screen = Screen::Play {
            screen,
            _back: back,
        };
        cx.notify();
    }

    fn settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (ports, midi_in) = (self.ports.clone(), self.midi_in.clone());
        let screen = cx.new(|cx| SettingsPage::new(ports, midi_in, window, cx));
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
