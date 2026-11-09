//! The lD Mixer window (`ldmixer gui`).
//!
//! Layout follows the official iD Mixer: a mix selector (Main / Cue A /
//! Cue B) in the header and as an accordion in the left column (the open
//! mix is the one being edited, with its master as a knob), channel strips
//! in the middle, and a right column with the monitor section, output
//! routing (including loopback) and system settings.
//!
//! The window is a client of the lD Mixer daemon (started on demand): it
//! edits a copy of the [`MixerState`], sends it to the daemon, and reflects
//! the daemon's notices. Closing the window leaves the daemon, and with it
//! the mix and the virtual devices, running.

mod widgets;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use ldmixer::config::{self, Settings};
use ldmixer::engine::{MonitorState, SystemState};
use ldmixer::ipc::{self, MonitorCmd, Notice, Request};
use ldmixer::vdev;
use ldmixer::mixer::{MixerState, Send, Strip};
use ldmixer::protocol::{self, monitor, Bus, NodeKind, Profile, RouteSource};

use widgets::{emi, hex, Fader, Knob, Rgb};

pub use ldmixer::APP_ID;

fn accent(st: &MixerState, bus: Bus) -> Rgb {
    hex(st.mix_color(bus))
}

fn bus_name(bus: Bus) -> &'static str {
    match bus {
        Bus::Main => "main",
        Bus::CueA => "cuea",
        Bus::CueB => "cueb",
    }
}

fn bus_from_name(name: &str) -> Option<Bus> {
    Bus::ALL.into_iter().find(|b| bus_name(*b) == name)
}

// Sizes are in em so the layout follows the user's font and text scale;
// only hairlines stay in px. Mix colors come from `mix_color_css`.
const CSS: &str = r#"
/* Colors come from the theme (libadwaita named colors), so lD Mixer looks
   like a stock GTK app; only state colors (mute, solo, ...) are fixed. */
.mixer-body { background-color: @view_bg_color; }
.strip {
  background-color: @view_bg_color;
  border-right: 1px solid alpha(@view_fg_color, 0.1);
}
.strip-head {
  background-color: alpha(@view_fg_color, 0.05);
  border-top: 0.2em solid transparent;
  border-bottom: 1px solid alpha(@view_fg_color, 0.1);
  padding: 0.75em 0.4em 0.65em 0.4em;
}
.strip-name { font-weight: 700; font-size: 1.05em; }
.strip-sub { font-size: 0.72em; color: alpha(@view_fg_color, 0.55); }
.value { font-size: 0.75em; font-feature-settings: "tnum"; color: alpha(@view_fg_color, 0.8); }
button.tiny { min-height: 0; min-width: 0; padding: 0; margin: 0; font-size: 0.72em; font-weight: 700; letter-spacing: 0.04em; }
button.mode-badge {
  min-height: 0; min-width: 0; padding: 0.15em 0.55em; margin: 0;
  font-size: 0.62em; font-weight: 700; letter-spacing: 0.08em;
  color: alpha(@view_fg_color, 0.6);
  background: none;
  border: 1px solid alpha(@view_fg_color, 0.2);
  border-radius: 0.35em;
}
button.mode-badge:hover { background-color: alpha(@view_fg_color, 0.07); color: @view_fg_color; }
.mix-dot { min-width: 0.75em; min-height: 0.75em; border-radius: 50%; }
button.mix-color { min-width: 0; min-height: 0; padding: 0.4em; border-radius: 50%; }
.mix-heard { font-size: 0.8em; color: alpha(@window_fg_color, 0.55); }
editablelabel.mix-value { font-size: 1.5em; font-weight: 800; }
/* Strip buttons (Ø, device, S, M) light up white when on, like the monitor buttons. */
button.tiny:checked { background-color: alpha(@window_fg_color, 0.85); color: @window_bg_color; }
editablelabel.value { min-width: 0; }
/* The edit field keeps the label's font and drops theme padding, which clipped the text's top. */
editablelabel > stack > text { font-size: 1em; padding: 0 0.15em; margin: 0; min-height: 0; }
button.monitor-toggle { min-height: 2.3em; font-weight: 800; font-size: 0.85em; letter-spacing: 0.04em; }
button.monitor-toggle:checked { background-color: alpha(@window_fg_color, 0.85); color: @window_bg_color; }
button.monitor-toggle.cut:checked { background-color: #dc2626; color: white; }
button.monitor-toggle.dim:checked { background-color: #f59e0b; color: #111; }
button.monitor-toggle.talk:checked { background-color: #22c55e; color: #111; }
.monitor-volume { font-size: 1.6em; font-weight: 800; font-feature-settings: "tnum"; }
"#;

/// CSS for the mixes' colors: list dots, the open mix's title, strip headers.
fn mix_color_css(st: &MixerState) -> String {
    Bus::ALL
        .iter()
        .map(|&b| {
            let (n, c) = (bus_name(b), st.mix_color(b));
            format!(
                ".mix-dot.{n} {{ background-color: #{c:06x}; }}\n\
                 .mix-row.active.{n} label.title {{ color: #{c:06x}; }}\n\
                 window.bus-{n} .strip-head {{ border-top-color: #{c:06x}; }}\n"
            )
        })
        .collect()
}

/// Start the GUI. Blocks until the window closes.
pub fn run(mock: bool) -> glib::ExitCode {
    let mut flags = gio::ApplicationFlags::empty();
    if std::env::var_os("LDMIXER_SCREENSHOT").is_some() {
        // Development screenshots must not hand off to a running window.
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let gapp = adw::Application::builder().application_id(APP_ID).flags(flags).build();
    gapp.connect_startup(|_| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        widgets::init_em();
        // GTK's automatic font rendering cut the top pixel row off glyphs
        // at fractional scales (seen with GTK 4.24 at 1.25). Manual mode
        // renders with the user's own font settings instead. This changes
        // only lD Mixer's in-process GtkSettings, nothing on the system.
        if let Some(st) = gtk::Settings::default() {
            st.set_gtk_font_rendering(gtk::FontRendering::Manual);
        }
        if let Some(display) = gdk::Display::default() {
            // Above user priority: themes such as adw-gtk3 install a full
            // stylesheet as the user's gtk.css, which would otherwise win over
            // these rules (e.g. giving text buttons wide padding). The rules
            // only target lD Mixer's own classes.
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_USER + 100,
            );
        }
    });
    gapp.connect_activate(move |gapp| {
        // A second `ldmixer gui` just raises the running window.
        if let Some(w) = gapp.active_window() {
            w.present();
            return;
        }
        App::start(gapp, mock);
    });
    // GTK must not see our own subcommand arguments.
    gapp.run_with_args::<&str>(&[])
}

/// Widgets of one channel strip.
struct StripUi {
    strip: Strip,
    fader: Fader,
    /// One pan per channel: `(node, knob, readout)`.
    pans: Vec<(usize, Knob, gtk::EditableLabel)>,
    db_label: gtk::EditableLabel,
    mute: gtk::ToggleButton,
    solo: gtk::ToggleButton,
}

/// One mix in the accordion: its master knob and where it is heard.
struct MixRow {
    bus: Bus,
    row: adw::ExpanderRow,
    knob: Knob,
    value: gtk::EditableLabel,
    heard: gtk::Label,
}

/// Minimum size of every strip button (Ø, link, device, mute, solo), fixed
/// so they line up whatever the theme does to icon or label buttons; the
/// rows stretch them to the strip's width.
fn tiny_w() -> i32 {
    emi(1.9)
}
fn tiny_h() -> i32 {
    emi(1.9)
}

/// Width of a mono strip; a linked strip is exactly two of them.
fn strip_w() -> i32 {
    emi(7.5)
}

/// Width of the side columns (Mixes, Monitor).
fn column_w() -> i32 {
    emi(23.0)
}

/// Widgets that live for the whole window.
struct Ui {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    menu_button: gtk::MenuButton,
    strips_box: gtk::Box,
    mix_rows: Vec<MixRow>,
    mon_knob: Knob,
    mon_label: gtk::EditableLabel,
    mon_buttons: Vec<(u8, gtk::ToggleButton)>,
    mono_mode: adw::ComboRow,
    talk_source: adw::ComboRow,
    dim_level: adw::SpinRow,
    routes: Vec<adw::ComboRow>,
    loopback_switch: adw::SwitchRow,
    rate_row: adw::ActionRow,
    quantum_row: adw::ActionRow,
    device_row: adw::ActionRow,
    optical_in: adw::ComboRow,
    optical_out: adw::ComboRow,
}

struct App {
    gapp: adw::Application,
    profile: &'static Profile,
    state: RefCell<MixerState>,
    bus: Cell<Bus>,
    /// Writer half of the daemon connection.
    conn: RefCell<Option<std::os::unix::net::UnixStream>>,
    ui: OnceCell<Ui>,
    strips: RefCell<Vec<StripUi>>,
    /// Set while code (not the user) updates widgets, to suppress callbacks.
    syncing: Cell<bool>,
    connected: Cell<bool>,
    /// Set when the daemon announced it is quitting on request.
    quitting: Cell<bool>,
    /// When the user last moved the monitor knob; device echoes are ignored
    /// for a moment so the knob does not jump back while dragging.
    monitor_touched: Cell<Option<Instant>>,
    mock: bool,
    settings: RefCell<Settings>,
    vdevs: RefCell<Vec<vdev::Status>>,
    /// Weak self-reference, for scheduling work from `&self` methods.
    me: OnceCell<std::rc::Weak<App>>,
    /// A state update is queued (sends are coalesced to ~60 per second).
    commit_pending: Cell<bool>,
    /// The mixes' colors as CSS (see [`mix_color_css`]).
    color_css: gtk::CssProvider,
}

impl App {
    fn start(gapp: &adw::Application, mock: bool) {
        let profile = &protocol::PROFILES[0];
        let app = Rc::new(App {
            gapp: gapp.clone(),
            profile,
            state: RefCell::new(MixerState::default_for(profile)),
            bus: Cell::new(Bus::Main),
            conn: RefCell::new(None),
            ui: OnceCell::new(),
            strips: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            connected: Cell::new(false),
            quitting: Cell::new(false),
            monitor_touched: Cell::new(None),
            mock,
            settings: RefCell::new(Settings::default()),
            vdevs: RefCell::new(Vec::new()),
            me: OnceCell::new(),
            commit_pending: Cell::new(false),
            color_css: gtk::CssProvider::new(),
        });
        let _ = app.me.set(Rc::downgrade(&app));
        app.build_ui();
        app.install_actions();
        app.rebuild_menu();
        app.rebuild_strips();
        app.sync_panel();
        app.select_bus(Bus::Main);
        app.connect_daemon();

        if let Some(path) = std::env::var_os("LDMIXER_SCREENSHOT") {
            if let Some(bus) = std::env::var("LDMIXER_SCREENSHOT_BUS").ok().as_deref().and_then(bus_from_name) {
                app.select_bus(bus);
            }
            let a = app.clone();
            let delay = std::env::var("LDMIXER_SCREENSHOT_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800);
            glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                a.screenshot(std::path::Path::new(&path));
                a.gapp.quit();
            });
        }

        app.ui().window.present();
    }

    /// Connect to the daemon (starting it if needed) and pump its notices
    /// into the main loop. Reconnects if the daemon goes away unexpectedly.
    fn connect_daemon(self: &Rc<Self>) {
        let conn = match ipc::connect_or_spawn(true, self.mock) {
            Ok(c) => c,
            Err(e) => {
                self.set_offline(&format!("{e:#}"));
                let a = self.clone();
                glib::timeout_add_local_once(Duration::from_secs(2), move || a.connect_daemon());
                return;
            }
        };
        let ipc::Connection { writer, mut reader } = conn;
        *self.conn.borrow_mut() = Some(writer);
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            while let Ok(Some(n)) = ipc::read_line::<Notice>(&mut reader) {
                if tx.send_blocking(n).is_err() {
                    return;
                }
            }
        });
        let a = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(n) = rx.recv().await {
                a.on_notice(n);
            }
            // The daemon went away.
            a.conn.borrow_mut().take();
            a.connected.set(false);
            if a.quitting.get() {
                a.gapp.quit();
            } else {
                a.set_offline("The lD Mixer background service stopped; restarting it…");
                let a2 = a.clone();
                glib::timeout_add_local_once(Duration::from_secs(1), move || a2.connect_daemon());
            }
        });
    }

    fn set_offline(&self, reason: &str) {
        let ui = self.ui();
        ui.title.set_subtitle("Not connected");
        ui.device_row.set_subtitle("Not connected");
        ui.banner.set_title(reason);
        ui.banner.set_revealed(true);
        for s in self.strips.borrow().iter() {
            s.fader.set_meters(&[0.0, 0.0]);
        }
    }

    fn ui(&self) -> &Ui {
        self.ui.get().expect("UI built")
    }

    fn send(&self, req: Request) {
        let mut conn = self.conn.borrow_mut();
        if let Some(w) = conn.as_mut() {
            if ipc::write_line(w, &req).is_err() {
                *conn = None;
            }
        }
    }

    fn toast(&self, msg: &str) {
        self.ui().toasts.add_toast(adw::Toast::new(msg));
    }

    // -- building ------------------------------------------------------------

    fn build_ui(self: &Rc<Self>) {
        let window = adw::ApplicationWindow::builder()
            .application(&self.gapp)
            .title("lD Mixer")
            .default_width(emi(116.0))
            .default_height(emi(53.0))
            .build();
        window.add_css_class("bus-main");
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&window),
            &self.color_css,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 100,
        );

        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("lD Mixer", "Connecting…");
        header.set_title_widget(Some(&title));


        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .primary(true)
            .build();
        header.pack_end(&menu_button);

        let banner = adw::Banner::new("");
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);

        // Channel strips, edge to edge, separated by divider lines.
        let strips_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .halign(gtk::Align::Start)
            .build();
        strips_box.add_css_class("strips");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .child(&strips_box)
            .build();

        // Mixes column (beside the monitor column): the three mixes as an accordion. Opening one makes it
        // the mix the strips show; its master is a knob.
        let mix_group = adw::PreferencesGroup::builder()
            .title("Mixes")
            .description("Open a mix to edit it on the strips")
            .build();
        let mut mix_rows = Vec::new();
        for bus in Bus::ALL {
            let row = adw::ExpanderRow::builder()
                .title(format!("{} Mix", bus.label()))
                .subtitle("Master 0.0 dB")
                .build();
            // The color dot doubles as the color picker.
            let dot = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).build();
            dot.add_css_class("mix-dot");
            dot.add_css_class(bus_name(bus));
            let color = gtk::Button::builder()
                .child(&dot)
                .valign(gtk::Align::Center)
                .tooltip_text("Change the color of this mix")
                .build();
            color.add_css_class("flat");
            color.add_css_class("mix-color");
            let a = self.clone();
            color.connect_clicked(move |_| a.mix_color_dialog(bus));
            row.add_prefix(&color);
            row.add_css_class("mix-row");
            row.add_css_class(bus_name(bus));
            let rename = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Rename this mix")
                .valign(gtk::Align::Center)
                .build();
            rename.add_css_class("flat");
            let a = self.clone();
            rename.connect_clicked(move |_| a.rename_mix_dialog(bus));
            row.add_suffix(&rename);

            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(emi(0.55))
                .margin_top(emi(1.0))
                .margin_bottom(emi(1.0))
                .margin_start(emi(0.8))
                .margin_end(emi(0.8))
                .build();
            let knob = Knob::new(emi(8.2), false, 1.0, accent(&self.state.borrow(), bus));
            knob.area.set_tooltip_text(Some("Mix master level"));
            let value = value_entry("0.0 dB");
            value.add_css_class("mix-value");
            value.set_tooltip_text(Some("Click to type a level in dB"));
            let heard = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
            heard.add_css_class("mix-heard");
            content.append(&knob.area);
            content.append(&value);
            content.append(&heard);
            row.add_row(&content);

            let a = self.clone();
            row.connect_expanded_notify(move |r| {
                if r.is_expanded() && !a.syncing.get() {
                    a.select_bus(bus);
                }
            });
            let a = self.clone();
            let label = value.clone();
            knob.connect_changed(move |v| {
                if a.syncing.get() {
                    return;
                }
                a.state.borrow_mut().masters[bus.index()] = v;
                label.set_text(&protocol::format_db(protocol::fader_to_db(v)));
                a.sync_mix_rows();
                a.commit();
            });
            let a = self.clone();
            let k = knob.clone();
            on_value_edited(&value, move |l, text| {
                if let Some(db) = parse_db(text) {
                    k.set_value(protocol::db_to_fader(db));
                    a.state.borrow_mut().masters[bus.index()] = k.value();
                    a.sync_mix_rows();
                    a.commit();
                }
                l.set_text(&protocol::format_db(protocol::fader_to_db(k.value())));
            });
            mix_group.add(&row);
            mix_rows.push(MixRow { bus, row, knob, value, heard });
        }
        let left_panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(emi(1.25))
            .margin_start(emi(1.0))
            .margin_end(emi(1.0))
            .margin_top(emi(1.0))
            .margin_bottom(emi(1.0))
            .build();
        left_panel.append(&mix_group);
        // The column is `column_w()` wide, margins included.
        left_panel.set_width_request(column_w() - 2 * emi(1.0));

        let mixer_area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        mixer_area.add_css_class("mixer-body");
        mixer_area.set_hexpand(true);
        mixer_area.append(&scroller);

        // Side panel.
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(emi(1.25))
            .margin_start(emi(1.0))
            .margin_end(emi(1.0))
            .margin_top(emi(1.0))
            .margin_bottom(emi(1.0))
            .build();

        // Monitor section.
        let mon_group = adw::PreferencesGroup::builder().title("Monitor").build();
        let mon_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(emi(0.7))
            .margin_top(emi(0.4))
            .build();
        let mon_knob = Knob::new(emi(8.75), false, protocol::db_to_fader(Some(-20.0)), hex(0xe5e7eb));
        mon_knob.area.set_tooltip_text(Some("Monitor volume (follows the front-panel knob)"));
        let mon_label = value_entry("–");
        mon_label.add_css_class("monitor-volume");
        mon_label.set_tooltip_text(Some("Click to type a level in dB"));
        let a = self.clone();
        on_value_edited(&mon_label, move |l, text| {
            let knob = &a.ui().mon_knob;
            match parse_db(text) {
                Some(db) => {
                    knob.set_value(protocol::db_to_fader(db));
                    a.monitor_touched.set(Some(Instant::now()));
                    a.send(Request::Monitor(MonitorCmd::Level(monitor::VOLUME, db)));
                    l.set_text(&protocol::format_db(db));
                }
                None => l.set_text(&protocol::format_db(protocol::fader_to_db(knob.value()))),
            }
        });
        mon_box.append(&mon_knob.area);
        mon_box.append(&mon_label);
        let a = self.clone();
        mon_knob.connect_changed(move |v| {
            if a.syncing.get() {
                return;
            }
            let db = protocol::fader_to_db(v);
            a.monitor_touched.set(Some(Instant::now()));
            a.ui().mon_label.set_text(&protocol::format_db(db));
            a.send(Request::Monitor(MonitorCmd::Level(monitor::VOLUME, db)));
        });

        // Two joined rows, laid out like the official mixer's monitor buttons.
        let grid = gtk::Box::new(gtk::Orientation::Vertical, emi(0.4));
        let rows = [linked_row(), linked_row()];
        for r in &rows {
            grid.append(r);
        }
        let mut mon_buttons = Vec::new();
        let toggles: Vec<(u8, &str, &str, &str, bool)> = vec![
            (monitor::TALKBACK, "TALK", "talk", "Talkback into the cue mixes", self.profile.has_talkback),
            (monitor::POLARITY, "Ø", "polarity", "Invert the polarity of one monitor side", true),
            (monitor::MONO, "MONO", "mono", "Sum the monitors to mono", true),
            (monitor::ALT, "ALT", "alt", "Switch to the alternate speakers", self.profile.has_alt),
            (monitor::DIM, "DIM", "dim", "Dim the monitors by the dim level", true),
            (monitor::CUT, "CUT", "cut", "Mute the monitors", true),
        ];
        for (i, (sel, label, class, tip, available)) in toggles.into_iter().enumerate() {
            let b = gtk::ToggleButton::with_label(label);
            b.add_css_class("monitor-toggle");
            b.add_css_class(class);
            b.set_tooltip_text(Some(tip));
            b.set_sensitive(available);
            let a = self.clone();
            b.connect_toggled(move |b| {
                if !a.syncing.get() {
                    a.send(Request::Monitor(MonitorCmd::Bool(sel, b.is_active())));
                }
            });
            rows[i / 3].append(&b);
            mon_buttons.push((sel, b));
        }
        mon_box.append(&grid);
        mon_group.add(&mon_box);

        let mon_opts = adw::PreferencesGroup::new();
        let mono_mode = adw::ComboRow::builder()
            .title("Mono Mode")
            .model(&gtk::StringList::new(&["L + R", "Left Only", "Right Only"]))
            .build();
        let a = self.clone();
        mono_mode.connect_selected_notify(move |r| {
            if !a.syncing.get() {
                a.send(Request::Monitor(MonitorCmd::Byte(monitor::MONO_MODE, r.selected() as u8)));
            }
        });
        let talk_source = adw::ComboRow::builder()
            .title("Talkback Source")
            .model(&gtk::StringList::new(&["Mic 1", "Mic 2", "Digi 1"]))
            .build();
        talk_source.set_sensitive(self.profile.has_talkback);
        let a = self.clone();
        talk_source.connect_selected_notify(move |r| {
            if !a.syncing.get() {
                a.send(Request::Monitor(MonitorCmd::Byte(monitor::TALKBACK_SOURCE, 0x10 + r.selected() as u8)));
            }
        });
        let dim_level = adw::SpinRow::with_range(-60.0, 0.0, 1.0);
        dim_level.set_title("Dim Level");
        dim_level.set_subtitle("dB");
        let a = self.clone();
        dim_level.connect_value_notify(move |r| {
            if !a.syncing.get() {
                a.send(Request::Monitor(MonitorCmd::Level(monitor::DIM_LEVEL, Some(r.value() as f32))));
            }
        });
        mon_opts.add(&mono_mode);
        mon_opts.add(&talk_source);
        mon_opts.add(&dim_level);

        // Output routing, loopback included.
        let out_group = adw::PreferencesGroup::builder()
            .title("Outputs")
            .description("What each output plays")
            .build();
        let mut routes = Vec::new();
        for (i, pair) in self.profile.outputs.iter().enumerate() {
            let labels: Vec<String> = pair.sources.iter().map(|s| s.label()).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let row = adw::ComboRow::builder()
                .title(pair.name)
                .model(&gtk::StringList::new(&labels))
                .build();
            if pair.name == "Loopback" && self.profile.loopback_capture.is_some() {
                row.set_subtitle("Recorded back to the computer as inputs 11+12");
            }
            let a = self.clone();
            row.connect_selected_notify(move |r| {
                if a.syncing.get() {
                    return;
                }
                let pair = &a.profile.outputs[i];
                if let Some(src) = pair.sources.get(r.selected() as usize) {
                    a.state.borrow_mut().routes[i] = *src;
                    a.sync_mix_rows();
                    a.commit();
                }
            });
            out_group.add(&row);
            routes.push(row);
        }

        let loopback_switch = adw::SwitchRow::builder()
            .title("Loopback as App Input")
            .subtitle("Adds “iD24 Loopback” as an input for other apps while lD Mixer is open")
            .active(self.settings.borrow().loopback_source)
            .build();
        loopback_switch.set_sensitive(self.profile.loopback_capture.is_some());
        let a = self.clone();
        loopback_switch.connect_active_notify(move |r| {
            if a.syncing.get() {
                return;
            }
            a.settings.borrow_mut().loopback_source = r.is_active();
            let s = a.settings.borrow().clone();
            a.send(Request::SetSettings(s));
        });
        out_group.add(&loopback_switch);

        // System.
        let sys_group = adw::PreferencesGroup::builder().title("System").build();
        let device_row = adw::ActionRow::builder().title("Device").subtitle("Not connected").build();
        let rate_row = adw::ActionRow::builder().title("Sample Rate").subtitle("–").build();
        rate_row.set_tooltip_text(Some("Set by the sound server; lD Mixer does not change it"));
        let optical_in = adw::ComboRow::builder()
            .title("Optical In")
            .model(&gtk::StringList::new(&["ADAT", "S/PDIF"]))
            .build();
        let optical_out = adw::ComboRow::builder()
            .title("Optical Out")
            .model(&gtk::StringList::new(&["ADAT", "S/PDIF"]))
            .build();
        for (row, output) in [(&optical_in, false), (&optical_out, true)] {
            row.set_sensitive(self.profile.has_optical);
            let a = self.clone();
            row.connect_selected_notify(move |r| {
                if !a.syncing.get() {
                    a.send(Request::Optical { output, mode: r.selected() as i32 });
                }
            });
        }
        sys_group.add(&device_row);
        sys_group.add(&rate_row);
        let quantum_row = adw::ActionRow::builder().title("Quantum").subtitle("–").build();
        quantum_row.set_tooltip_text(Some(
            "PipeWire's buffer size, set by the sound server; lD Mixer does not change it",
        ));
        sys_group.add(&quantum_row);
        sys_group.add(&optical_in);
        sys_group.add(&optical_out);

        panel.append(&mon_group);
        panel.append(&mon_opts);
        panel.append(&out_group);
        panel.append(&sys_group);
        panel.set_width_request(column_w() - 2 * emi(1.0));

        // The two side columns: side by side on wide windows, stacked on
        // narrower ones (see the breakpoints below).
        let side_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let column_sep = gtk::Separator::new(gtk::Orientation::Vertical);
        side_box.append(&left_panel);
        side_box.append(&column_sep);
        side_box.append(&panel);
        let side_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&side_box)
            .build();
        let sidebar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sidebar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        sidebar.append(&side_scroll);

        let two_columns = (2 * column_w() + 2) as f64;
        let split = adw::OverlaySplitView::builder()
            .content(&mixer_area)
            .sidebar(&sidebar)
            .sidebar_position(gtk::PackType::End)
            .sidebar_width_fraction(1.0)
            .min_sidebar_width(two_columns)
            .max_sidebar_width(two_columns)
            .build();
        // Shows the side panel when it is tucked away (narrow windows).
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Mixes and Monitor")
            .visible(false)
            .build();
        split
            .bind_property("show-sidebar", &sidebar_button, "active")
            .bidirectional()
            .sync_create()
            .build();
        header.pack_end(&sidebar_button);

        // Breakpoints, in em so they follow the font size like the layout.
        let one_column = (column_w() + 1) as f64;
        let max_width = |w: f64| {
            adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, emi(w) as f64, adw::LengthUnit::Px)
        };
        let stacked = |bp: &adw::Breakpoint| {
            bp.add_setter(&side_box, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
            bp.add_setter(&column_sep, "orientation", Some(&gtk::Orientation::Horizontal.to_value()));
            bp.add_setter(&split, "min-sidebar-width", Some(&one_column.to_value()));
            bp.add_setter(&split, "max-sidebar-width", Some(&one_column.to_value()));
        };
        // Medium: one side column, Mixes above Monitor.
        let medium = adw::Breakpoint::new(max_width(90.0));
        stacked(&medium);
        window.add_breakpoint(medium);
        // Narrow: the strips get the window; the side column slides over them.
        let narrow = adw::Breakpoint::new(max_width(55.0));
        stacked(&narrow);
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&split, "show-sidebar", Some(&false.to_value()));
        narrow.add_setter(&sidebar_button, "visible", Some(&true.to_value()));
        window.add_breakpoint(narrow);
        // Breakpoints need a minimum size; smaller content scrolls.
        window.set_size_request(emi(24.0), emi(26.0));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        toolbar.set_content(Some(&toasts));
        window.set_content(Some(&toolbar));

        let a = self.clone();
        window.connect_close_request(move |_| {
            // The daemon keeps the mixer running; only the window closes.
            a.flush_commit();
            a.conn.borrow_mut().take();
            glib::Propagation::Proceed
        });

        let _ = self.ui.set(Ui {
            window,
            title,
            banner,
            toasts,
            menu_button,
            strips_box,
            mix_rows,
            mon_knob,
            mon_label,
            mon_buttons,
            mono_mode,
            talk_source,
            dim_level,
            routes,
            loopback_switch,
            rate_row,
            quantum_row,
            device_row,
            optical_in,
            optical_out,
        });
    }

    fn rebuild_strips(self: &Rc<Self>) {
        let ui = self.ui();
        while let Some(child) = ui.strips_box.first_child() {
            ui.strips_box.remove(&child);
        }
        let strips = self.state.borrow().visible_strips();
        let mut built = Vec::new();
        for strip in strips {
            let (widget, s) = self.build_strip(strip);
            ui.strips_box.append(&widget);
            built.push(s);
        }
        *self.strips.borrow_mut() = built;
        self.sync_strips();
    }

    fn build_strip(self: &Rc<Self>, strip: Strip) -> (gtk::Box, StripUi) {
        let node = strip.node;
        let kind = self.profile.nodes[node].kind;
        let bus = self.bus.get();
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .width_request(if strip.stereo { 2 * strip_w() } else { strip_w() })
            .build();
        b.add_css_class("strip");
        if strip.stereo {
            b.add_css_class("stereo");
        }

        // Header: name and input type, under a line in the open mix's color.
        let name = gtk::Label::builder()
            .label(self.state.borrow().strip_name(self.profile, strip))
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(9)
            .tooltip_text("Double-click to rename")
            .build();
        name.add_css_class("strip-name");
        let click = gtk::GestureClick::new();
        let a = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                a.rename_dialog(node);
            }
        });
        name.add_controller(click);
        let sub = gtk::Label::new(Some(match (kind, strip.stereo) {
            (NodeKind::Mic, false) => "Mic / Line",
            (NodeKind::Mic, true) => "Mic / Line St",
            (NodeKind::Digital, false) => "Optical",
            (NodeKind::Digital, true) => "Optical St",
            (NodeKind::Daw, false) => "DAW",
            (NodeKind::Daw, true) => "DAW Stereo",
        }));
        sub.add_css_class("strip-sub");
        let head = gtk::Box::new(gtk::Orientation::Vertical, emi(0.1));
        head.add_css_class("strip-head");
        head.append(&name);
        head.append(&sub);

        // Polarity and device buttons, joined and filling the strip.
        let tools = linked_row();
        let phase_inputs = self.profile.phase_inputs as usize;
        if node < phase_inputs {
            let phase = tiny_toggle("Ø", "phase");
            phase.set_tooltip_text(Some("Polarity invert"));
            phase.set_active(self.state.borrow().channels[node].phase);
            let a = self.clone();
            phase.connect_toggled(move |p| {
                {
                    let mut st = a.state.borrow_mut();
                    st.channels[node].phase = p.is_active();
                    if strip.stereo && node + 1 < phase_inputs {
                        st.channels[node + 1].phase = p.is_active();
                    }
                }
                a.commit();
            });
            tools.append(&phase);
        }
        // Expose this strip to other apps as its own PipeWire device.
        let playback = kind.is_playback();
        let vdev_btn = gtk::ToggleButton::builder()
            .icon_name(if playback { "audio-speakers-symbolic" } else { "audio-input-microphone-symbolic" })
            .active(self.state.borrow().channels[node].virtual_device)
            .build();
        make_tiny(vdev_btn.upcast_ref(), "vdev");
        vdev_btn.set_tooltip_text(Some(if playback {
            "Show as its own output (speaker) in other apps while lD Mixer runs"
        } else {
            "Show as its own input (microphone) in other apps while lD Mixer runs"
        }));
        let a = self.clone();
        vdev_btn.connect_toggled(move |b| {
            if a.syncing.get() {
                return;
            }
            a.state.borrow_mut().channels[node].virtual_device = b.is_active();
            a.commit();
        });
        tools.append(&vdev_btn);

        // One pan per channel; a linked pair shows its left and right side by side.
        let pan_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .homogeneous(true)
            .margin_top(emi(0.2))
            .build();
        let mut pans = Vec::new();
        let channels = if strip.stereo { 2 } else { 1 };
        for (k, ch) in (node..node + channels).enumerate() {
            let col = gtk::Box::new(gtk::Orientation::Vertical, emi(0.3));
            let knob = Knob::new(emi(3.8), true, if strip.stereo { k as f64 } else { 0.5 }, accent(&self.state.borrow(), bus));
            knob.area.set_tooltip_text(Some(match (strip.stereo, k) {
                (false, _) => "Pan",
                (true, 0) => "Pan of the left channel",
                _ => "Pan of the right channel",
            }));
            let label = value_entry("C");
            label.set_tooltip_text(Some("Click to type: C, L50, R30 or -100..100"));
            col.append(&knob.area);
            col.append(&label);
            pan_row.append(&col);
            let a = self.clone();
            let l = label.clone();
            knob.connect_changed(move |v| {
                let p = v * 2.0 - 1.0;
                l.set_text(&pan_text(p));
                a.update_send(ch, |s| s.pan = p);
            });
            let a = self.clone();
            let kn = knob.clone();
            on_value_edited(&label, move |l, text| {
                if let Some(p) = parse_pan(text) {
                    kn.set_value((p + 1.0) / 2.0);
                    a.update_send(ch, |s| s.pan = p);
                }
                l.set_text(&pan_text(kn.value() * 2.0 - 1.0));
            });
            pans.push((ch, knob, label));
        }

        // MONO / STEREO badge between two rules; clicking it links or
        // unlinks the pair (an odd channel links with the one before it).
        let mode = gtk::Box::new(gtk::Orientation::Horizontal, emi(0.4));
        let rule = || {
            let s = gtk::Separator::new(gtk::Orientation::Horizontal);
            s.set_hexpand(true);
            s.set_valign(gtk::Align::Center);
            s
        };
        let badge = gtk::Button::with_label(if strip.stereo { "STEREO" } else { "MONO" });
        badge.add_css_class("mode-badge");
        badge.remove_css_class("text-button");
        let pair_start = node & !1;
        if pair_start + 1 < self.profile.nodes.len() {
            badge.set_tooltip_text(Some(if strip.stereo { "Unlink into two mono channels" } else { "Link with its neighbor as a stereo pair" }));
            let a = self.clone();
            badge.connect_clicked(move |_| {
                a.state.borrow_mut().set_linked(pair_start, !strip.stereo);
                a.commit();
                // Rebuild after this handler returns; it destroys this button.
                let a = a.clone();
                glib::idle_add_local_once(move || a.rebuild_strips());
            });
        } else {
            badge.set_sensitive(false);
        }
        mode.append(&rule());
        mode.append(&badge);
        mode.append(&rule());

        let fader = Fader::new(if strip.stereo { 2 } else { 1 }, accent(&self.state.borrow(), bus));
        let db_label = value_entry("-inf");
        db_label.set_tooltip_text(Some("Click to type a level in dB (e.g. -12, -inf)"));

        // Solo and mute, joined like the official mixer's S | M.
        let ms = linked_row();
        let solo = tiny_toggle(if strip.stereo { "SOLO" } else { "S" }, "solo");
        solo.set_tooltip_text(Some("Solo in this mix"));
        let mute = tiny_toggle(if strip.stereo { "MUTE" } else { "M" }, "mute");
        mute.set_tooltip_text(Some("Mute in this mix"));
        ms.append(&solo);
        ms.append(&mute);

        // The level readout sits right above the fader's slot.
        db_label.set_halign(gtk::Align::Start);
        db_label.set_width_request(db_label_w());
        let f = fader.clone();
        let label = db_label.clone();
        fader.area.connect_resize(move |w, _| {
            let margin = (f.slot_center(w as f64) - db_label_w() as f64 / 2.0).max(0.0) as i32;
            if label.margin_start() != margin {
                // Defer: changing layout from inside an allocation is not allowed.
                let label = label.clone();
                glib::idle_add_local_once(move || label.set_margin_start(margin));
            }
        });

        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(emi(0.85))
            .margin_top(emi(0.85))
            .margin_bottom(emi(1.25))
            .margin_start(emi(0.6))
            .margin_end(emi(0.6))
            .vexpand(true)
            .build();
        inner.append(&tools);
        inner.append(&pan_row);
        inner.append(&mode);
        inner.append(&ms);
        inner.append(&db_label);
        inner.append(&fader.area);

        b.append(&head);
        b.append(&inner);

        let a = self.clone();
        let label = db_label.clone();
        fader.connect_changed(move |v| {
            label.set_text(&protocol::format_db(protocol::fader_to_db(v)));
            a.update_send(node, |s| s.fader = v);
        });
        // Typed level.
        let a = self.clone();
        let f = fader.clone();
        on_value_edited(&db_label, move |l, text| {
            if let Some(db) = parse_db(text) {
                f.set_value(protocol::db_to_fader(db));
                let v = f.value();
                a.update_send(node, |s| s.fader = v);
            }
            l.set_text(&protocol::format_db(protocol::fader_to_db(f.value())));
        });
        let a = self.clone();
        mute.connect_toggled(move |m| {
            let on = m.is_active();
            a.update_send(node, |s| s.mute = on);
        });
        let a = self.clone();
        solo.connect_toggled(move |m| {
            let on = m.is_active();
            a.update_send(node, |s| s.solo = on);
        });

        // Right click: rename or hide this line.
        let menu = gio::Menu::new();
        let item = gio::MenuItem::new(Some("Rename…"), None);
        item.set_action_and_target_value(Some("app.rename-strip"), Some(&(node as i32).to_variant()));
        menu.append_item(&item);
        let item = gio::MenuItem::new(Some("Hide"), None);
        item.set_action_and_target_value(Some("app.hide-strip"), Some(&(node as i32).to_variant()));
        menu.append_item(&item);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&b);
        popover.set_has_arrow(false);
        let right = gtk::GestureClick::builder().button(3).build();
        let pop = popover.clone();
        right.connect_pressed(move |_, _, x, y| {
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.popup();
        });
        b.add_controller(right);

        (b, StripUi { strip, fader, pans, db_label, mute, solo })
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, param: Option<&glib::VariantTy>, f: Box<dyn Fn(Option<String>)>| {
            let action = gio::SimpleAction::new(name, param);
            action.connect_activate(move |_, p| f(p.and_then(|v| v.get::<String>())));
            self.gapp.add_action(&action);
        };
        let a = self.clone();
        add("mix", Some(glib::VariantTy::STRING), Box::new(move |p| {
            if let Some(bus) = p.as_deref().and_then(bus_from_name) {
                a.select_bus(bus);
            }
        }));
        let a = self.clone();
        add("save-preset", None, Box::new(move |_| a.save_preset_dialog()));
        let a = self.clone();
        add("load-preset", Some(glib::VariantTy::STRING), Box::new(move |p| {
            let Some(name) = p else { return };
            match config::load_preset(a.profile, &name) {
                Ok(st) => {
                    *a.state.borrow_mut() = st;
                    a.rebuild_strips();
                    a.sync_panel();
                    a.rebuild_menu();
                    a.commit();
                    a.toast(&format!("Recalled preset “{name}”"));
                }
                Err(e) => a.toast(&format!("Could not load preset: {e}")),
            }
        }));
        let a = self.clone();
        add("delete-preset", Some(glib::VariantTy::STRING), Box::new(move |p| {
            if let Some(name) = p {
                a.delete_preset_dialog(name);
            }
        }));
        let a = self.clone();
        add("readback", None, Box::new(move |_| {
            if a.connected.get() {
                a.send(Request::ReadBack);
            } else {
                a.toast("No device connected");
            }
        }));
        let a = self.clone();
        add("reset", None, Box::new(move |_| a.reset_dialog()));
        let a = self.clone();
        add("about", None, Box::new(move |_| a.about()));
        let a = self.clone();
        add("quit-all", None, Box::new(move |_| {
            // Stops the background service too; the window closes when the
            // daemon confirms.
            a.quitting.set(true);
            a.send(Request::Shutdown);
            if a.conn.borrow().is_none() {
                a.gapp.quit();
            }
        }));

        // Per-line actions (target: the strip's first node).
        let add_int = |name: &str, f: Box<dyn Fn(usize)>| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::INT32));
            action.connect_activate(move |_, p| {
                if let Some(n) = p.and_then(|v| v.get::<i32>()) {
                    f(n.max(0) as usize);
                }
            });
            self.gapp.add_action(&action);
        };
        let a = self.clone();
        add_int("rename-strip", Box::new(move |n| a.rename_dialog(n)));
        let a = self.clone();
        add_int("hide-strip", Box::new(move |n| a.set_hidden(n, true)));
        let a = self.clone();
        add_int("show-strip", Box::new(move |n| a.set_hidden(n, false)));
        let a = self.clone();
        add("show-all-strips", None, Box::new(move |_| {
            let hidden: Vec<usize> = {
                let st = a.state.borrow();
                (0..st.channels.len()).filter(|&i| st.channels[i].hidden).collect()
            };
            for n in hidden {
                a.state.borrow_mut().channels[n].hidden = false;
            }
            a.after_layout_change();
        }));

        self.gapp.set_accels_for_action("app.mix::main", &["<Ctrl>1"]);
        self.gapp.set_accels_for_action("app.mix::cuea", &["<Ctrl>2"]);
        self.gapp.set_accels_for_action("app.mix::cueb", &["<Ctrl>3"]);
        self.gapp.set_accels_for_action("app.save-preset", &["<Ctrl>s"]);
    }

    fn rebuild_menu(&self) {
        let menu = gio::Menu::new();
        let presets = gio::Menu::new();
        presets.append(Some("Save Preset…"), Some("app.save-preset"));
        let names = config::list_presets(self.profile);
        if !names.is_empty() {
            let recall = gio::Menu::new();
            let delete = gio::Menu::new();
            for n in &names {
                let item = gio::MenuItem::new(Some(n), None);
                item.set_action_and_target_value(Some("app.load-preset"), Some(&n.to_variant()));
                recall.append_item(&item);
                let item = gio::MenuItem::new(Some(n), None);
                item.set_action_and_target_value(Some("app.delete-preset"), Some(&n.to_variant()));
                delete.append_item(&item);
            }
            presets.append_submenu(Some("Recall Preset"), &recall);
            presets.append_submenu(Some("Delete Preset"), &delete);
        }
        menu.append_section(None, &presets);
        // Hidden lines, to bring them back.
        let hidden: Vec<(usize, String)> = {
            let st = self.state.borrow();
            st.strips()
                .into_iter()
                .filter(|s| st.channels[s.node].hidden)
                .map(|s| (s.node, st.strip_name(self.profile, s)))
                .collect()
        };
        if !hidden.is_empty() {
            let lines = gio::Menu::new();
            let sub = gio::Menu::new();
            for (node, name) in &hidden {
                let item = gio::MenuItem::new(Some(name), None);
                item.set_action_and_target_value(Some("app.show-strip"), Some(&(*node as i32).to_variant()));
                sub.append_item(&item);
            }
            sub.append(Some("Show All"), Some("app.show-all-strips"));
            lines.append_submenu(Some(&format!("Hidden Channels ({})", hidden.len())), &sub);
            menu.append_section(None, &lines);
        }
        let device = gio::Menu::new();
        device.append(Some("Load Mix from Device"), Some("app.readback"));
        device.append(Some("Reset Mixer…"), Some("app.reset"));
        menu.append_section(None, &device);
        let about = gio::Menu::new();
        about.append(Some("About lD Mixer"), Some("app.about"));
        about.append(Some("Quit lD Mixer Completely"), Some("app.quit-all"));
        menu.append_section(None, &about);
        self.ui().menu_button.set_menu_model(Some(&menu));
    }

    // -- state flow ------------------------------------------------------------

    fn update_send(&self, node: usize, f: impl FnOnce(&mut Send)) {
        if self.syncing.get() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            let bus = self.bus.get();
            f(st.send_mut(node, bus));
        }
        self.commit();
    }

    /// Send the current state to the daemon, which applies and saves it.
    /// A fader drag produces an event per pointer motion, so sends are
    /// coalesced: at most one every 16 ms, always with the latest state.
    fn commit(&self) {
        if self.commit_pending.replace(true) {
            return;
        }
        match self.me.get().and_then(|w| w.upgrade()) {
            Some(me) => {
                glib::timeout_add_local_once(Duration::from_millis(16), move || me.flush_commit());
            }
            None => self.flush_commit(),
        }
    }

    fn flush_commit(&self) {
        if self.commit_pending.replace(false) {
            let st = self.state.borrow().clone();
            self.send(Request::SetState(Box::new(st)));
        }
    }

    fn select_bus(&self, bus: Bus) {
        self.bus.set(bus);
        // Accordion: only the active mix is open.
        self.syncing.set(true);
        for m in &self.ui().mix_rows {
            m.row.set_expanded(m.bus == bus);
            if m.bus == bus {
                m.row.add_css_class("active");
            } else {
                m.row.remove_css_class("active");
            }
        }
        self.syncing.set(false);
        let w = &self.ui().window;
        for b in Bus::ALL {
            w.remove_css_class(&format!("bus-{}", bus_name(b)));
        }
        w.add_css_class(&format!("bus-{}", bus_name(bus)));
        self.sync_strips();
    }

    /// Show the selected mix's values on the strips and master.
    fn sync_strips(&self) {
        let bus = self.bus.get();
        let st = self.state.borrow();
        self.syncing.set(true);
        for s in self.strips.borrow().iter() {
            let send = st.send(s.strip.node, bus);
            s.fader.set_accent(accent(&st, bus));
            s.fader.set_value(send.fader);
            for (ch, knob, label) in &s.pans {
                let pan = st.send(*ch, bus).pan;
                knob.set_accent(accent(&st, bus));
                knob.set_value((pan + 1.0) / 2.0);
                if !label.is_editing() {
                    label.set_text(&pan_text(pan));
                }
            }
            if !s.db_label.is_editing() {
                s.db_label.set_text(&protocol::format_db(protocol::fader_to_db(send.fader)));
            }
            s.mute.set_active(send.mute);
            s.solo.set_active(send.solo);
        }
        self.syncing.set(false);
        drop(st);
        self.sync_mix_rows();
    }

    /// Mix knobs, their readouts and where each mix is heard.
    fn sync_mix_rows(&self) {
        let st = self.state.borrow();
        self.syncing.set(true);
        for m in &self.ui().mix_rows {
            let v = st.masters[m.bus.index()];
            let db = protocol::fader_to_db(v);
            m.knob.set_value(v);
            if !m.value.is_editing() {
                m.value.set_text(&protocol::format_db(db));
            }
            m.row.set_subtitle(&format!("Master {}", protocol::format_db(db)));
            let wanted = match m.bus {
                Bus::Main => &[RouteSource::Main, RouteSource::Alt][..],
                Bus::CueA => &[RouteSource::CueA][..],
                Bus::CueB => &[RouteSource::CueB][..],
            };
            let outs: Vec<&str> = self
                .profile
                .outputs
                .iter()
                .zip(&st.routes)
                .filter(|(_, r)| wanted.contains(r))
                .map(|(o, _)| o.name)
                .collect();
            m.heard.set_label(&if outs.is_empty() {
                "Not routed to any output".to_string()
            } else {
                format!("Heard on: {}", outs.join(", "))
            });
        }
        self.syncing.set(false);
    }

    /// Show routing from the state in the side panel.
    fn sync_panel(&self) {
        let st = self.state.borrow();
        self.syncing.set(true);
        for ((row, pair), src) in self.ui().routes.iter().zip(self.profile.outputs).zip(&st.routes) {
            if let Some(i) = pair.sources.iter().position(|s| s == src) {
                row.set_selected(i as u32);
            }
        }
        self.syncing.set(false);
        drop(st);
        self.sync_mix_rows();
        self.sync_names();
        self.sync_colors();
    }

    // -- daemon notices -----------------------------------------------------------

    fn on_notice(self: &Rc<Self>, n: Notice) {
        let ui = self.ui();
        match n {
            Notice::Welcome { .. } => {}
            Notice::Device(Some(d), _) => {
                self.connected.set(true);
                let who = if d.mock {
                    "mock device".to_string()
                } else {
                    d.serial.clone().unwrap_or_else(|| "connected".into())
                };
                ui.title.set_subtitle(&format!("{} · {}", d.model, who));
                ui.device_row.set_subtitle(&format!(
                    "{} · firmware {:x}.{:02x}{}",
                    d.model,
                    d.firmware >> 8,
                    d.firmware & 0xff,
                    if d.mock { " · mock" } else { "" }
                ));
                if d.mock {
                    ui.banner.set_title("Mock device: no hardware is being controlled");
                    ui.banner.set_revealed(true);
                } else {
                    ui.banner.set_revealed(false);
                }
            }
            Notice::Device(None, reason) => {
                self.connected.set(false);
                self.set_offline(&reason.unwrap_or_else(|| "No interface connected".into()));
            }
            Notice::State(st) => {
                *self.state.borrow_mut() = *st;
                self.rebuild_strips();
                self.sync_panel();
                self.rebuild_menu();
            }
            Notice::Settings(st) => {
                self.syncing.set(true);
                ui.loopback_switch.set_active(st.loopback_source);
                self.syncing.set(false);
                *self.settings.borrow_mut() = st;
            }
            Notice::Meters(levels) => {
                // Draw meters on the fader's own dB scale.
                let levels: Vec<f32> =
                    levels.iter().map(|db| protocol::db_to_fader(*db) as f32).collect();
                for s in self.strips.borrow().iter() {
                    let n = if s.strip.stereo { 2 } else { 1 };
                    let end = (s.strip.node + n).min(levels.len());
                    s.fader.set_meters(levels.get(s.strip.node..end).unwrap_or(&[]));
                }
            }
            Notice::Monitor(m) => self.on_monitor(&m),
            Notice::System(s) => self.on_system(&s),
            Notice::VirtualDevices(list) => {
                let old = self.vdevs.replace(list.clone());
                for st in &list {
                    let was_error = old
                        .iter()
                        .find(|o| o.spec.node_name == st.spec.node_name)
                        .and_then(|o| o.error.clone());
                    if let Some(e) = &st.error {
                        if was_error.as_ref() != Some(e) {
                            self.toast(&format!("{}: {e}", st.spec.description));
                        }
                    }
                }
            }
            Notice::Toast(t) => self.toast(&t),
            Notice::RawReply(_) => {}
            Notice::Quitting => self.quitting.set(true),
        }
    }

    fn on_monitor(&self, m: &MonitorState) {
        let ui = self.ui();
        self.syncing.set(true);
        let touched = self
            .monitor_touched
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(600));
        if !touched {
            ui.mon_knob.set_value(protocol::db_to_fader(m.volume));
            if !ui.mon_label.is_editing() {
                ui.mon_label.set_text(&protocol::format_db(m.volume));
            }
        }
        for (sel, b) in &ui.mon_buttons {
            b.set_active(match *sel {
                monitor::DIM => m.dim,
                monitor::CUT => m.cut,
                monitor::ALT => m.alt,
                monitor::TALKBACK => m.talkback,
                monitor::MONO => m.mono,
                monitor::POLARITY => m.polarity,
                _ => false,
            });
        }
        ui.mono_mode.set_selected(m.mono_mode.min(2) as u32);
        ui.talk_source.set_selected(m.talkback_source.saturating_sub(0x10).min(2) as u32);
        if let Some(d) = m.dim_level {
            ui.dim_level.set_value(d as f64);
        }
        self.syncing.set(false);
    }

    fn on_system(&self, s: &SystemState) {
        let ui = self.ui();
        self.syncing.set(true);
        ui.quantum_row.set_subtitle(&match s.quantum {
            Some(q) => {
                let rate = s.sample_rate.unwrap_or(48000).max(1) as f64;
                format!(
                    "{q} samples · {:.1} ms{}",
                    q as f64 * 1000.0 / rate,
                    if s.quantum_forced { " (forced)" } else { " (default; apps may change it)" }
                )
            }
            None => "Unknown".into(),
        });
        ui.rate_row.set_subtitle(&match s.sample_rate {
            Some(r) => format!("{:.1} kHz{}", r as f64 / 1000.0, if s.streaming { "" } else { " (idle)" }),
            None => "No stream running".into(),
        });
        // Only 0 (ADAT) and 1 (S/PDIF) are known; other values (the iD24's
        // output port has been seen to answer 255) are left unshown.
        if let Some(m @ 0..=1) = s.optical_in {
            ui.optical_in.set_selected(m as u32);
        }
        if let Some(m @ 0..=1) = s.optical_out {
            ui.optical_out.set_selected(m as u32);
        }
        self.syncing.set(false);
    }

    // -- dialogs -------------------------------------------------------------------

    fn text_dialog(
        self: &Rc<Self>,
        heading: &str,
        body: &str,
        initial: &str,
        accept: &str,
        done: impl Fn(&Rc<App>, String) + 'static,
    ) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("ok", accept);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("ok"));
        let a = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                done(&a, entry.text().to_string());
            }
        });
        dialog.present(Some(&self.ui().window));
    }

    /// Hide or show a line (strip), then refresh the window and menu.
    fn set_hidden(self: &Rc<Self>, node: usize, hidden: bool) {
        if let Some(ch) = self.state.borrow_mut().channels.get_mut(node) {
            ch.hidden = hidden;
        }
        self.after_layout_change();
        if hidden {
            self.toast("Channel hidden; show it again from the main menu");
        }
    }

    fn after_layout_change(self: &Rc<Self>) {
        self.commit();
        let a = self.clone();
        // Rebuild outside the handler that triggered this (it may be a
        // widget of the strip being removed).
        glib::idle_add_local_once(move || {
            a.rebuild_strips();
            a.rebuild_menu();
        });
    }

    fn rename_mix_dialog(self: &Rc<Self>, bus: Bus) {
        let current = self.state.borrow().mix_names[bus.index()].clone();
        self.text_dialog(
            "Rename Mix",
            &format!("New name for {}. Leave empty to restore the default.", ldmixer::mixer::default_mix_name(bus)),
            &current,
            "Rename",
            move |a, text| {
                a.state.borrow_mut().mix_names[bus.index()] = text.trim().to_string();
                a.commit();
                a.sync_names();
            },
        );
    }

    /// Mix colors on the dots, mix knobs and strips.
    fn sync_colors(&self) {
        {
            let st = self.state.borrow();
            self.color_css.load_from_string(&mix_color_css(&st));
            for m in &self.ui().mix_rows {
                m.knob.set_accent(accent(&st, m.bus));
            }
        }
        self.sync_strips();
    }

    /// Pick a mix's color (from its dot in the Mixes list).
    fn mix_color_dialog(self: &Rc<Self>, bus: Bus) {
        let (r, g, b) = hex(self.state.borrow().mix_color(bus));
        let initial = gdk::RGBA::new(r as f32, g as f32, b as f32, 1.0);
        let dialog = gtk::ColorDialog::builder()
            .title(format!("{} Color", self.state.borrow().mix_name(bus)))
            .with_alpha(false)
            .build();
        let a = self.clone();
        dialog.choose_rgba(Some(&self.ui().window), Some(&initial), gio::Cancellable::NONE, move |res| {
            // Cancelling the dialog is an error result; keep the color.
            let Ok(c) = res else { return };
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            let rgb = byte(c.red()) << 16 | byte(c.green()) << 8 | byte(c.blue());
            a.state.borrow_mut().set_mix_color(bus, rgb);
            a.commit();
            a.sync_colors();
        });
    }

    /// Mix names in the header, the accordion and the output menus.
    fn sync_names(&self) {
        let ui = self.ui();
        let st = self.state.borrow();
        for m in &ui.mix_rows {
            m.row.set_title(&st.mix_name(m.bus));
        }
        self.syncing.set(true);
        for ((row, pair), src) in ui.routes.iter().zip(self.profile.outputs).zip(&st.routes) {
            let labels: Vec<String> = pair.sources.iter().map(|s| st.source_label(*s)).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            row.set_model(Some(&gtk::StringList::new(&labels)));
            if let Some(i) = pair.sources.iter().position(|s| s == src) {
                row.set_selected(i as u32);
            }
        }
        self.syncing.set(false);
    }

    fn rename_dialog(self: &Rc<Self>, node: usize) {
        let current = self.state.borrow().channels[node].label.clone();
        self.text_dialog(
            "Rename Channel",
            "Leave empty to restore the default name.",
            &current,
            "Rename",
            move |a, text| {
                a.state.borrow_mut().channels[node].label = text.trim().to_string();
                a.after_layout_change();
            },
        );
    }

    fn save_preset_dialog(self: &Rc<Self>) {
        self.text_dialog(
            "Save Preset",
            "Stores every mix, link, name and output route.",
            "",
            "Save",
            |a, text| match config::save_preset(a.profile, &text, &a.state.borrow()) {
                Ok(name) => {
                    a.rebuild_menu();
                    a.toast(&format!("Saved preset “{name}”"));
                }
                Err(e) => a.toast(&format!("Could not save preset: {e}")),
            },
        );
    }

    fn delete_preset_dialog(self: &Rc<Self>, name: String) {
        let dialog = adw::AlertDialog::new(
            Some("Delete Preset?"),
            Some(&format!("“{name}” will be removed permanently.")),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let a = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                match config::delete_preset(a.profile, &name) {
                    Ok(()) => a.rebuild_menu(),
                    Err(e) => a.toast(&format!("Could not delete preset: {e}")),
                }
            }
        });
        dialog.present(Some(&self.ui().window));
    }

    fn reset_dialog(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Reset Mixer?"),
            Some("All mixes return to the defaults: only DAW 1+2 plays, everything else is down."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("reset", "Reset");
        dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
        let a = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "reset" {
                *a.state.borrow_mut() = MixerState::default_for(a.profile);
                a.rebuild_strips();
                a.sync_panel();
                a.commit();
            }
        });
        dialog.present(Some(&self.ui().window));
    }

    fn about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("lD Mixer")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("lovebycoma")
            .website("https://github.com/lovebycoma/ldmixer")
            .license_type(gtk::License::MitX11)
            .comments(
                "Unofficial mixer for Audient iD interfaces. lD = Linux iD.\n\
                 Not affiliated with Audient.",
            )
            .build();
        about.add_credit_section(
            Some("Protocol research"),
            &["MixiD https://github.com/TheOnlyJoey/MixiD", "BiD https://github.com/baakhoff/BiD", "Monix https://github.com/sKuhLight/monix"],
        );
        about.present(Some(&self.ui().window));
    }

    /// Render the window to a PNG (development aid, `LDMIXER_SCREENSHOT`).
    fn screenshot(&self, path: &std::path::Path) {
        let window = &self.ui().window;
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let (w, h) = (window.width() as f64, window.height() as f64);
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, w, h);
        let Some(node) = snapshot.to_node() else { return };
        let Some(renderer) = window.renderer() else { return };
        let texture = renderer.render_texture(&node, None);
        if let Err(e) = texture.save_to_png(path) {
            eprintln!("screenshot failed: {e}");
        }
    }
}

fn pan_text(p: f64) -> String {
    let v = (p * 100.0).round() as i32;
    match v {
        0 => "C".into(),
        v if v < 0 => format!("L{}", -v),
        v => format!("R{v}"),
    }
}

/// Width of a strip's level readout, centered over the fader slot.
fn db_label_w() -> i32 {
    emi(3.1)
}

/// A small click-to-edit value readout.
fn value_entry(text: &str) -> gtk::EditableLabel {
    let l = gtk::EditableLabel::new(text);
    l.add_css_class("value");
    l.set_halign(gtk::Align::Center);
    l.set_alignment(0.5);
    l.set_width_chars(6);
    l.set_max_width_chars(7);
    l
}

/// Call `f(label, typed_text)` when the user finishes editing. Escape
/// restores the old text before this fires, so `f` can always reformat.
fn on_value_edited(l: &gtk::EditableLabel, f: impl Fn(&gtk::EditableLabel, &str) + 'static) {
    l.connect_editing_notify(move |l| {
        if !l.is_editing() {
            let text = l.text().to_string();
            f(l, &text);
        }
    });
}

/// Parse a typed level: "-12", "-12 dB", "−6.5", "-inf", "mute", "off".
/// Values above 0 dB are capped at 0 dB. `None` = not understood;
/// `Some(None)` = silence.
fn parse_db(text: &str) -> Option<Option<f32>> {
    let t = text.trim().to_lowercase().replace('−', "-");
    let t = t.trim_end_matches("db").trim();
    if matches!(t, "-inf" | "inf" | "-∞" | "mute" | "off") {
        return Some(None);
    }
    let v: f32 = t.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(if v <= protocol::FLOOR_DB { None } else { Some(v.min(0.0)) })
}

/// Parse a typed pan: "C", "L", "R", "L30", "R 45", "-100".."100".
fn parse_pan(text: &str) -> Option<f64> {
    let t = text.trim().to_uppercase().replace('−', "-");
    let num = |s: &str| -> Option<f64> {
        let s = s.trim();
        if s.is_empty() { Some(100.0) } else { s.parse::<f64>().ok() }
    };
    let v = if t == "C" || t == "0" {
        0.0
    } else if let Some(rest) = t.strip_prefix('L') {
        -num(rest)?
    } else if let Some(rest) = t.strip_prefix('R') {
        num(rest)?
    } else {
        t.parse::<f64>().ok()?
    };
    Some((v / 100.0).clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values() {
        assert_eq!(parse_db("-12"), Some(Some(-12.0)));
        assert_eq!(parse_db(" -6.5 dB "), Some(Some(-6.5)));
        assert_eq!(parse_db("+3"), Some(Some(0.0)));
        assert_eq!(parse_db("-inf"), Some(None));
        assert_eq!(parse_db("-200"), Some(None));
        assert_eq!(parse_db("loud"), None);
        assert_eq!(parse_pan("C"), Some(0.0));
        assert_eq!(parse_pan("L30"), Some(-0.3));
        assert_eq!(parse_pan("r"), Some(1.0));
        assert_eq!(parse_pan("-50"), Some(-0.5));
        assert_eq!(parse_pan("x"), None);
    }
}

/// A row of joined buttons (GTK's `.linked`) sharing the strip's width.
fn linked_row() -> gtk::Box {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).homogeneous(true).build();
    b.add_css_class("linked");
    b
}

/// Style a strip button: one fixed minimum size whatever the theme does to
/// text or icon buttons.
fn make_tiny(b: &gtk::Widget, class: &str) {
    b.add_css_class("tiny");
    b.add_css_class(class);
    b.set_size_request(tiny_w(), tiny_h());
    b.remove_css_class("text-button");
    b.remove_css_class("image-button");
}

fn tiny_toggle(label: &str, class: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::with_label(label);
    make_tiny(b.upcast_ref(), class);
    b
}
