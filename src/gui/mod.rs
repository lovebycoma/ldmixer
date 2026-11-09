//! The lD Mixer window (`ld-mixer gui`).
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

use ld_mixer::config::{self, Settings};
use ld_mixer::engine::{MonitorState, SystemState};
use ld_mixer::ipc::{self, MonitorCmd, Notice, Request};
use ld_mixer::vdev;
use ld_mixer::mixer::{MixerState, Send, Strip};
use ld_mixer::protocol::{self, monitor, Bus, NodeKind, Profile, RouteSource};

use widgets::{hex, Fader, Knob, Rgb};

pub use ld_mixer::APP_ID;

fn accent(bus: Bus) -> Rgb {
    match bus {
        Bus::Main => hex(0x2dd4bf),
        Bus::CueA => hex(0xf59e0b),
        Bus::CueB => hex(0xa78bfa),
    }
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

const CSS: &str = r#"
.mixer-body { background-color: #12151b; }
.strips { }
.strip {
  background-color: #1a1e26;
  border-right: 1px solid #2a303b;
  border-radius: 0;
}
.strip.stereo { background-color: #1c2029; }
.strip-name { font-weight: 700; font-size: 0.85em; }
.strip-sub { font-size: 0.72em; color: #8b93a3; }
.value { font-size: 0.75em; font-feature-settings: "tnum"; color: #c9d1dc; }
.chip { min-height: 4px; border-radius: 0; }
.kind-mic .chip { background-color: #f87171; }
.kind-digital .chip { background-color: #60a5fa; }
.kind-daw .chip { background-color: #34d399; }
button.tiny { min-height: 0; min-width: 0; padding: 0; font-size: 0.75em; font-weight: 700; }
.mix-dot { min-width: 10px; min-height: 10px; border-radius: 5px; }
.mix-dot.main { background-color: #2dd4bf; }
.mix-dot.cuea { background-color: #f59e0b; }
.mix-dot.cueb { background-color: #a78bfa; }
.mix-heard { font-size: 0.8em; color: #8b93a3; }
editablelabel.mix-value { font-size: 1.5em; font-weight: 800; }
button.mute:checked { background-color: #dc2626; color: white; }
button.solo:checked { background-color: #eab308; color: #111; }
button.phase:checked { background-color: #3b82f6; color: white; }
button.link-toggle:checked { background-color: #64748b; color: white; }
button.vdev:checked { background-color: #0ea5e9; color: white; }
editablelabel.value { min-width: 0; }
editablelabel.value text { font-size: 0.75em; }
editablelabel.monitor-volume text { font-size: 1em; }
button.monitor-toggle { min-height: 34px; font-weight: 800; font-size: 0.85em; }
button.monitor-toggle:checked { background-color: #e5e7eb; color: #111; }
button.monitor-toggle.cut:checked { background-color: #dc2626; color: white; }
button.monitor-toggle.dim:checked { background-color: #f59e0b; color: #111; }
button.monitor-toggle.talk:checked { background-color: #22c55e; color: #111; }
.monitor-volume { font-size: 1.6em; font-weight: 800; font-feature-settings: "tnum"; }
.side-panel { background-color: #161a21; }
window.bus-main .bus-accent { color: #2dd4bf; }
window.bus-cuea .bus-accent { color: #f59e0b; }
window.bus-cueb .bus-accent { color: #a78bfa; }
"#;

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
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
    gapp.connect_activate(move |gapp| {
        // A second `ld-mixer gui` just raises the running window.
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

/// Size of every strip button (Ø, link, device, mute, solo), fixed so they
/// line up whatever the theme does to icon or label buttons.
const TINY_W: i32 = 28;
const TINY_H: i32 = 24;

/// Width of a mono strip; a linked strip is exactly two of them.
const STRIP_W: i32 = 92;

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
            .default_width(1700)
            .default_height(780)
            .build();
        window.add_css_class("bus-main");

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
            .vscrollbar_policy(gtk::PolicyType::Never)
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
            let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
            dot.add_css_class("mix-dot");
            dot.add_css_class(bus_name(bus));
            row.add_prefix(&dot);
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
                .spacing(8)
                .margin_top(14)
                .margin_bottom(14)
                .margin_start(12)
                .margin_end(12)
                .build();
            let knob = Knob::new(120, false, 1.0, accent(bus));
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
            .spacing(18)
            .margin_start(14)
            .margin_end(14)
            .margin_top(14)
            .margin_bottom(14)
            .build();
        left_panel.append(&mix_group);
        let left_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(340)
            .hexpand(false)
            .child(&adw::Clamp::builder().maximum_size(360).tightening_threshold(360).child(&left_panel).build())
            .build();
        left_scroll.add_css_class("side-panel");

        let mixer_area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        mixer_area.add_css_class("mixer-body");
        mixer_area.set_hexpand(true);
        mixer_area.append(&scroller);

        // Side panel.
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_start(14)
            .margin_end(14)
            .margin_top(14)
            .margin_bottom(14)
            .build();

        // Monitor section.
        let mon_group = adw::PreferencesGroup::builder().title("Monitor").build();
        let mon_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(6)
            .build();
        let mon_knob = Knob::new(128, false, protocol::db_to_fader(Some(-20.0)), hex(0xe5e7eb));
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

        let grid = gtk::Grid::builder()
            .column_spacing(6)
            .row_spacing(6)
            .column_homogeneous(true)
            .build();
        let mut mon_buttons = Vec::new();
        let toggles: Vec<(u8, &str, &str, &str, bool)> = vec![
            (monitor::DIM, "DIM", "dim", "Dim the monitors by the dim level", true),
            (monitor::CUT, "CUT", "cut", "Mute the monitors", true),
            (monitor::ALT, "ALT", "alt", "Switch to the alternate speakers", self.profile.has_alt),
            (monitor::TALKBACK, "TALK", "talk", "Talkback into the cue mixes", self.profile.has_talkback),
            (monitor::MONO, "MONO", "mono", "Sum the monitors to mono", true),
            (monitor::POLARITY, "Ø", "polarity", "Invert the polarity of one monitor side", true),
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
            grid.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
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
        // Clamp keeps the panel narrow; the strips get the remaining width.
        let clamp = adw::Clamp::builder()
            .maximum_size(360)
            .tightening_threshold(360)
            .child(&panel)
            .build();
        let panel_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(340)
            .hexpand(false)
            .child(&clamp)
            .build();
        panel_scroll.add_css_class("side-panel");

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&mixer_area);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        body.append(&left_scroll);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        body.append(&panel_scroll);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
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
            .width_request(if strip.stereo { 2 * STRIP_W } else { STRIP_W })
            .build();
        b.add_css_class("strip");
        b.add_css_class(match kind {
            NodeKind::Mic => "kind-mic",
            NodeKind::Digital => "kind-digital",
            NodeKind::Daw => "kind-daw",
        });
        if strip.stereo {
            b.add_css_class("stereo");
        }

        let name = gtk::Label::builder()
            .label(self.state.borrow().strip_name(self.profile, strip))
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(9)
            .tooltip_text("Double-click to rename")
            .build();
        name.add_css_class("strip-name");
        name.set_margin_top(9);
        name.set_margin_bottom(7);
        name.set_margin_start(4);
        name.set_margin_end(4);
        let click = gtk::GestureClick::new();
        let a = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                a.rename_dialog(node);
            }
        });
        name.add_controller(click);
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        chip.add_css_class("chip");
        let sub = gtk::Label::new(Some(match (kind, strip.stereo) {
            (NodeKind::Mic, false) => "Mic / Line",
            (NodeKind::Mic, true) => "Mic / Line St",
            (NodeKind::Digital, false) => "Optical",
            (NodeKind::Digital, true) => "Optical St",
            (NodeKind::Daw, false) => "DAW",
            (NodeKind::Daw, true) => "DAW Stereo",
        }));
        sub.add_css_class("strip-sub");

        // Phase and link buttons.
        let tools = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Center)
            .build();
        let phase_inputs = self.profile.phase_inputs as usize;
        if node < phase_inputs {
            let phase = gtk::ToggleButton::with_label("Ø");
            phase.add_css_class("tiny");
            phase.set_size_request(TINY_W, TINY_H);
            phase.add_css_class("phase");
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
        if node % 2 == 0 && node + 1 < self.profile.nodes.len() {
            let link = gtk::ToggleButton::builder().child(&link_icon()).build();
            link.add_css_class("tiny");
            link.set_size_request(TINY_W, TINY_H);
            link.add_css_class("link-toggle");
            link.set_tooltip_text(Some("Link with the next channel as a stereo pair"));
            link.set_active(strip.stereo);
            let a = self.clone();
            link.connect_toggled(move |l| {
                let on = l.is_active();
                a.state.borrow_mut().set_linked(node, on);
                a.commit();
                // Rebuild after this handler returns; it destroys this button.
                let a = a.clone();
                glib::idle_add_local_once(move || a.rebuild_strips());
            });
            tools.append(&link);
        }
        // Expose this strip to other apps as its own PipeWire device.
        let playback = kind.is_playback();
        let vdev_btn = gtk::ToggleButton::builder()
            .icon_name(if playback { "audio-speakers-symbolic" } else { "audio-input-microphone-symbolic" })
            .active(self.state.borrow().channels[node].virtual_device)
            .build();
        vdev_btn.add_css_class("tiny");
        vdev_btn.set_size_request(TINY_W, TINY_H);
        vdev_btn.add_css_class("vdev");
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
            .build();
        let mut pans = Vec::new();
        let channels = if strip.stereo { 2 } else { 1 };
        for (k, ch) in (node..node + channels).enumerate() {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let knob = Knob::new(38, true, if strip.stereo { k as f64 } else { 0.5 }, accent(bus));
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
        let fader = Fader::new(if strip.stereo { 2 } else { 1 }, accent(bus));
        let db_label = value_entry("-inf");
        db_label.set_tooltip_text(Some("Click to type a level in dB (e.g. -12, -inf)"));

        let ms = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Center)
            .build();
        let mute = gtk::ToggleButton::with_label("M");
        mute.add_css_class("tiny");
        mute.set_size_request(TINY_W, TINY_H);
        mute.add_css_class("mute");
        mute.set_tooltip_text(Some("Mute in this mix"));
        let solo = gtk::ToggleButton::with_label("S");
        solo.add_css_class("tiny");
        solo.set_size_request(TINY_W, TINY_H);
        solo.add_css_class("solo");
        solo.set_tooltip_text(Some("Solo in this mix"));
        ms.append(&mute);
        ms.append(&solo);

        // The level readout sits right above the fader's slot.
        db_label.set_halign(gtk::Align::Start);
        db_label.set_width_request(DB_LABEL_W);
        let f = fader.clone();
        let label = db_label.clone();
        fader.area.connect_resize(move |w, _| {
            let margin = (f.slot_center(w as f64) - DB_LABEL_W as f64 / 2.0).max(0.0) as i32;
            if label.margin_start() != margin {
                // Defer: changing layout from inside an allocation is not allowed.
                let label = label.clone();
                glib::idle_add_local_once(move || label.set_margin_start(margin));
            }
        });

        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(8)
            .margin_start(4)
            .margin_end(4)
            .vexpand(true)
            .build();
        inner.append(&sub);
        inner.append(&tools);
        inner.append(&ms);
        inner.append(&pan_row);
        inner.append(&db_label);
        inner.append(&fader.area);

        b.append(&name);
        b.append(&chip);
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
            s.fader.set_accent(accent(bus));
            s.fader.set_value(send.fader);
            for (ch, knob, label) in &s.pans {
                let pan = st.send(*ch, bus).pan;
                knob.set_accent(accent(bus));
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
            &format!("New name for {}. Leave empty to restore the default.", ld_mixer::mixer::default_mix_name(bus)),
            &current,
            "Rename",
            move |a, text| {
                a.state.borrow_mut().mix_names[bus.index()] = text.trim().to_string();
                a.commit();
                a.sync_names();
            },
        );
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
const DB_LABEL_W: i32 = 46;

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

/// A chain-link glyph drawn in the widget's text color (Adwaita's
/// `insert-link-symbolic` no longer looks like a link).
fn link_icon() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(14).content_height(14).build();
    area.set_draw_func(|area, cr, w, h| {
        let c = area.color();
        cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
        cr.set_line_width(1.6);
        cr.translate(w as f64 / 2.0, h as f64 / 2.0);
        cr.rotate(-std::f64::consts::FRAC_PI_4);
        // Two interlocking rounded links.
        for dx in [-2.6, 2.6] {
            let (x, y, lw, r) = (dx - 3.6, -2.2, 7.2, 2.2);
            cr.new_sub_path();
            cr.arc(x + lw - r, y + r, r, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
            cr.arc(x + r, y + r, r, std::f64::consts::FRAC_PI_2, 3.0 * std::f64::consts::FRAC_PI_2);
            cr.close_path();
            let _ = cr.stroke();
        }
    });
    area
}
