//! The lD Mixer window (`ld-mixer gui`).
//!
//! Layout follows the official iD Mixer: a mix selector (Main / Cue A /
//! Cue B) in the header, channel strips on the left, the selected mix's
//! master fader, and a side panel with the monitor section, output routing
//! (including loopback) and system settings.
//!
//! All device I/O happens on the engine thread; this module only edits the
//! [`MixerState`], sends the resulting [`Desired`] state, and reflects the
//! engine's events.

mod widgets;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use ld_mixer::config;
use ld_mixer::engine::{self, Command, Desired, EngineHandle, Event, MonitorState, SystemState};
use ld_mixer::mixer::{MixerState, Send, Strip};
use ld_mixer::protocol::{self, monitor, Bus, NodeKind, Profile};

use widgets::{hex, Fader, Knob, Rgb};

pub const APP_ID: &str = "io.github.lovebycoma.LdMixer";

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
.strip {
  background-color: #1b1f27;
  border: 1px solid #262b35;
  border-radius: 12px;
  padding: 8px 5px 8px 5px;
}
.strip.stereo { background-color: #1d222b; }
.strip.master { background-color: #1f2430; border-color: #313847; }
.strip-name { font-weight: 700; font-size: 0.85em; }
.strip-sub { font-size: 0.72em; color: #8b93a3; }
.value { font-size: 0.75em; font-feature-settings: "tnum"; color: #c9d1dc; }
.chip { min-height: 3px; border-radius: 2px; }
.kind-mic .chip { background-color: #f87171; }
.kind-digital .chip { background-color: #60a5fa; }
.kind-daw .chip { background-color: #34d399; }
.master .chip { background-color: currentColor; }
button.tiny { min-height: 22px; min-width: 22px; padding: 0 4px; font-size: 0.75em; font-weight: 700; }
button.mute:checked { background-color: #dc2626; color: white; }
button.solo:checked { background-color: #eab308; color: #111; }
button.phase:checked { background-color: #3b82f6; color: white; }
button.link-toggle:checked { background-color: #64748b; color: white; }
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
window.bus-main toggle-group > toggle:checked { background-color: #2dd4bf; color: #0b0d12; }
window.bus-cuea toggle-group > toggle:checked { background-color: #f59e0b; color: #0b0d12; }
window.bus-cueb toggle-group > toggle:checked { background-color: #a78bfa; color: #0b0d12; }
toggle-group > toggle:checked label { color: #0b0d12; }
toggle-group > toggle { font-weight: 800; padding-left: 14px; padding-right: 14px; }
"#;

/// Start the GUI. Blocks until the window closes.
pub fn run(mock: bool) -> glib::ExitCode {
    let gapp = adw::Application::builder().application_id(APP_ID).build();
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
    pan: Knob,
    pan_label: gtk::Label,
    db_label: gtk::Label,
    mute: gtk::ToggleButton,
    solo: gtk::ToggleButton,
}

/// Widgets that live for the whole window.
struct Ui {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    mixes: adw::ToggleGroup,
    menu_button: gtk::MenuButton,
    strips_box: gtk::Box,
    master_name: gtk::Label,
    master: Fader,
    master_db: gtk::Label,
    mon_knob: Knob,
    mon_label: gtk::Label,
    mon_buttons: Vec<(u8, gtk::ToggleButton)>,
    mono_mode: adw::ComboRow,
    talk_source: adw::ComboRow,
    dim_level: adw::SpinRow,
    routes: Vec<adw::ComboRow>,
    rate_row: adw::ActionRow,
    device_row: adw::ActionRow,
    optical_in: adw::ComboRow,
    optical_out: adw::ComboRow,
}

struct App {
    gapp: adw::Application,
    profile: &'static Profile,
    state: RefCell<MixerState>,
    bus: Cell<Bus>,
    engine: RefCell<Option<EngineHandle>>,
    ui: OnceCell<Ui>,
    strips: RefCell<Vec<StripUi>>,
    /// Set while code (not the user) updates widgets, to suppress callbacks.
    syncing: Cell<bool>,
    connected: Cell<bool>,
    /// No saved state yet: read the device before writing anything.
    first_run: Cell<bool>,
    awaiting_readback: Cell<bool>,
    save_pending: Cell<bool>,
    /// When the user last moved the monitor knob; device echoes are ignored
    /// for a moment so the knob does not jump back while dragging.
    monitor_touched: Cell<Option<Instant>>,
}

impl App {
    fn start(gapp: &adw::Application, mock: bool) {
        let profile = &protocol::PROFILES[0];
        let saved = config::load_state(profile);
        let first_run = saved.is_none();
        let state = saved.unwrap_or_else(|| MixerState::default_for(profile));

        let (tx, rx) = async_channel::unbounded();
        let engine = engine::spawn(mock, tx);

        let app = Rc::new(App {
            gapp: gapp.clone(),
            profile,
            state: RefCell::new(state),
            bus: Cell::new(Bus::Main),
            engine: RefCell::new(Some(engine)),
            ui: OnceCell::new(),
            strips: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            connected: Cell::new(false),
            first_run: Cell::new(first_run),
            awaiting_readback: Cell::new(false),
            save_pending: Cell::new(false),
            monitor_touched: Cell::new(None),
        });
        app.build_ui();
        app.install_actions();
        app.rebuild_menu();
        app.rebuild_strips();
        app.sync_panel();

        let a = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                a.on_event(ev);
            }
        });

        // Debounced autosave.
        let a = app.clone();
        glib::timeout_add_local(Duration::from_millis(800), move || {
            a.flush_save();
            glib::ControlFlow::Continue
        });

        if let Some(path) = std::env::var_os("LDMIXER_SCREENSHOT") {
            if let Some(bus) = std::env::var("LDMIXER_SCREENSHOT_BUS").ok().as_deref().and_then(bus_from_name) {
                app.ui().mixes.set_active_name(Some(bus_name(bus)));
            }
            let a = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                a.screenshot(std::path::Path::new(&path));
                a.shutdown();
                a.gapp.quit();
            });
        }

        app.ui().window.present();
    }

    fn ui(&self) -> &Ui {
        self.ui.get().expect("UI built")
    }

    fn send(&self, cmd: Command) {
        if let Some(e) = self.engine.borrow().as_ref() {
            e.send(cmd);
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
            .default_width(1380)
            .default_height(780)
            .build();
        window.add_css_class("bus-main");

        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("lD Mixer", "Connecting…");
        header.set_title_widget(Some(&title));

        let mixes = adw::ToggleGroup::new();
        for bus in Bus::ALL {
            mixes.add(
                adw::Toggle::builder()
                    .label(bus.label().to_uppercase())
                    .name(bus_name(bus))
                    .tooltip(format!("Show the {} mix", bus.label()))
                    .build(),
            );
        }
        mixes.set_active_name(Some("main"));
        let a = self.clone();
        mixes.connect_active_name_notify(move |g| {
            if let Some(bus) = g.active_name().as_deref().and_then(bus_from_name) {
                a.select_bus(bus);
            }
        });
        header.pack_start(&mixes);

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

        // Channel strips.
        let strips_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(10)
            .margin_end(10)
            .margin_top(10)
            .margin_bottom(10)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&strips_box)
            .build();

        // Master strip of the selected mix.
        let master_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(92)
            .margin_top(10)
            .margin_bottom(10)
            .margin_end(10)
            .build();
        master_box.add_css_class("strip");
        master_box.add_css_class("master");
        let master_name = gtk::Label::new(Some("MAIN MIX"));
        master_name.add_css_class("strip-name");
        master_name.add_css_class("bus-accent");
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        chip.add_css_class("chip");
        chip.add_css_class("bus-accent");
        let master_sub = gtk::Label::new(Some("Master"));
        master_sub.add_css_class("strip-sub");
        let master = Fader::new(0, accent(Bus::Main));
        let master_db = gtk::Label::new(Some("0.0 dB"));
        master_db.add_css_class("value");
        master_box.append(&master_name);
        master_box.append(&chip);
        master_box.append(&master_sub);
        master_box.append(&master.area);
        master_box.append(&master_db);
        let a = self.clone();
        master.connect_changed(move |v| {
            if a.syncing.get() {
                return;
            }
            a.state.borrow_mut().masters[a.bus.get().index()] = v;
            a.ui().master_db.set_label(&protocol::format_db(protocol::fader_to_db(v)));
            a.commit();
        });

        let mixer_area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        mixer_area.add_css_class("mixer-body");
        mixer_area.set_hexpand(true);
        mixer_area.append(&scroller);
        mixer_area.append(&master_box);

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
        let mon_label = gtk::Label::new(Some("–"));
        mon_label.add_css_class("monitor-volume");
        mon_box.append(&mon_knob.area);
        mon_box.append(&mon_label);
        let a = self.clone();
        mon_knob.connect_changed(move |v| {
            if a.syncing.get() {
                return;
            }
            let db = protocol::fader_to_db(v);
            a.monitor_touched.set(Some(Instant::now()));
            a.ui().mon_label.set_label(&protocol::format_db(db));
            a.send(Command::MonitorLevel(monitor::VOLUME, db));
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
                    a.send(Command::MonitorBool(sel, b.is_active()));
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
                a.send(Command::MonitorByte(monitor::MONO_MODE, r.selected() as u8));
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
                a.send(Command::MonitorByte(monitor::TALKBACK_SOURCE, 0x10 + r.selected() as u8));
            }
        });
        let dim_level = adw::SpinRow::with_range(-60.0, 0.0, 1.0);
        dim_level.set_title("Dim Level");
        dim_level.set_subtitle("dB");
        let a = self.clone();
        dim_level.connect_value_notify(move |r| {
            if !a.syncing.get() {
                a.send(Command::MonitorLevel(monitor::DIM_LEVEL, Some(r.value() as f32)));
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
                row.set_subtitle("Recorded back as inputs 11+12 (experimental)");
            }
            let a = self.clone();
            row.connect_selected_notify(move |r| {
                if a.syncing.get() {
                    return;
                }
                let pair = &a.profile.outputs[i];
                if let Some(src) = pair.sources.get(r.selected() as usize) {
                    a.state.borrow_mut().routes[i] = *src;
                    a.commit();
                }
            });
            out_group.add(&row);
            routes.push(row);
        }

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
                    a.send(Command::Optical { output, mode: r.selected() as i32 });
                }
            });
        }
        sys_group.add(&device_row);
        sys_group.add(&rate_row);
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
        body.append(&panel_scroll);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
        toolbar.set_content(Some(&toasts));
        window.set_content(Some(&toolbar));

        let a = self.clone();
        window.connect_close_request(move |_| {
            a.shutdown();
            glib::Propagation::Proceed
        });

        let _ = self.ui.set(Ui {
            window,
            title,
            banner,
            toasts,
            mixes,
            menu_button,
            strips_box,
            master_name,
            master,
            master_db,
            mon_knob,
            mon_label,
            mon_buttons,
            mono_mode,
            talk_source,
            dim_level,
            routes,
            rate_row,
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
        let strips = self.state.borrow().strips();
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
            .spacing(6)
            .width_request(if strip.stereo { 96 } else { 78 })
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
            let link = gtk::ToggleButton::with_label("LINK");
            link.add_css_class("tiny");
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

        let pan = Knob::new(40, true, 0.5, accent(bus));
        pan.area.set_tooltip_text(Some(if strip.stereo { "Balance" } else { "Pan" }));
        let pan_label = gtk::Label::new(Some("C"));
        pan_label.add_css_class("value");
        let fader = Fader::new(if strip.stereo { 2 } else { 1 }, accent(bus));
        let db_label = gtk::Label::new(Some("-inf"));
        db_label.add_css_class("value");

        let ms = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Center)
            .build();
        let mute = gtk::ToggleButton::with_label("M");
        mute.add_css_class("tiny");
        mute.add_css_class("mute");
        mute.set_tooltip_text(Some("Mute in this mix"));
        let solo = gtk::ToggleButton::with_label("S");
        solo.add_css_class("tiny");
        solo.add_css_class("solo");
        solo.set_tooltip_text(Some("Solo in this mix"));
        ms.append(&mute);
        ms.append(&solo);

        b.append(&name);
        b.append(&chip);
        b.append(&sub);
        b.append(&tools);
        b.append(&pan.area);
        b.append(&pan_label);
        b.append(&fader.area);
        b.append(&db_label);
        b.append(&ms);

        let a = self.clone();
        let label = db_label.clone();
        fader.connect_changed(move |v| {
            label.set_label(&protocol::format_db(protocol::fader_to_db(v)));
            a.update_send(node, |s| s.fader = v);
        });
        let a = self.clone();
        let label = pan_label.clone();
        pan.connect_changed(move |v| {
            let p = v * 2.0 - 1.0;
            label.set_label(&pan_text(p));
            a.update_send(node, |s| s.pan = p);
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

        (b, StripUi { strip, fader, pan, pan_label, db_label, mute, solo })
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, param: Option<&glib::VariantTy>, f: Box<dyn Fn(Option<String>)>| {
            let action = gio::SimpleAction::new(name, param);
            action.connect_activate(move |_, p| f(p.and_then(|v| v.get::<String>())));
            self.gapp.add_action(&action);
        };
        let a = self.clone();
        add("mix", Some(glib::VariantTy::STRING), Box::new(move |p| {
            if let Some(name) = p {
                a.ui().mixes.set_active_name(Some(&name));
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
                a.awaiting_readback.set(true);
                a.send(Command::ReadBack);
            } else {
                a.toast("No device connected");
            }
        }));
        let a = self.clone();
        add("reset", None, Box::new(move |_| a.reset_dialog()));
        let a = self.clone();
        add("about", None, Box::new(move |_| a.about()));

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
        let device = gio::Menu::new();
        device.append(Some("Load Mix from Device"), Some("app.readback"));
        device.append(Some("Reset Mixer…"), Some("app.reset"));
        menu.append_section(None, &device);
        let about = gio::Menu::new();
        about.append(Some("About lD Mixer"), Some("app.about"));
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

    /// Push the current state to the device and schedule a save.
    fn commit(&self) {
        self.save_pending.set(true);
        if !self.connected.get() || self.awaiting_readback.get() {
            return;
        }
        let st = self.state.borrow();
        self.send(Command::Apply(Desired {
            matrix: st.matrix(),
            routing: st.routing(self.profile),
            phases: st.phases(self.profile),
        }));
    }

    fn flush_save(&self) {
        if self.save_pending.replace(false) && !self.first_run.get() {
            if let Err(e) = config::save_state(self.profile, &self.state.borrow()) {
                log::warn!("saving state failed: {e}");
            }
        }
    }

    fn shutdown(&self) {
        if !self.first_run.get() {
            self.save_pending.set(true);
        }
        self.flush_save();
        // Dropping the handle stops and joins the engine thread.
        self.engine.borrow_mut().take();
    }

    fn select_bus(&self, bus: Bus) {
        self.bus.set(bus);
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
            s.pan.set_accent(accent(bus));
            s.fader.set_value(send.fader);
            s.pan.set_value((send.pan + 1.0) / 2.0);
            s.pan_label.set_label(&pan_text(send.pan));
            s.db_label.set_label(&protocol::format_db(protocol::fader_to_db(send.fader)));
            s.mute.set_active(send.mute);
            s.solo.set_active(send.solo);
        }
        let ui = self.ui();
        let m = st.masters[bus.index()];
        ui.master.set_accent(accent(bus));
        ui.master.set_value(m);
        ui.master_db.set_label(&protocol::format_db(protocol::fader_to_db(m)));
        ui.master_name.set_label(&format!("{} MIX", bus.label().to_uppercase()));
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
    }

    // -- engine events -----------------------------------------------------------

    fn on_event(self: &Rc<Self>, ev: Event) {
        let ui = self.ui();
        match ev {
            Event::Connected(info) => {
                self.connected.set(true);
                let who = if info.mock {
                    "mock device".to_string()
                } else {
                    info.serial.clone().unwrap_or_else(|| "connected".into())
                };
                ui.title.set_subtitle(&format!("{} · {}", info.profile.name, who));
                ui.device_row.set_subtitle(&format!(
                    "{} · firmware {:x}.{:02x}{}",
                    info.profile.name,
                    info.firmware >> 8,
                    info.firmware & 0xff,
                    if info.mock { " · mock" } else { "" }
                ));
                if info.mock {
                    ui.banner.set_title("Mock device: no hardware is being controlled");
                    ui.banner.set_revealed(true);
                } else {
                    ui.banner.set_revealed(false);
                }
                if self.first_run.get() {
                    // Never overwrite an unknown device state blindly.
                    self.awaiting_readback.set(true);
                    self.send(Command::ReadBack);
                } else {
                    self.commit();
                }
            }
            Event::Disconnected(reason) => {
                self.connected.set(false);
                ui.title.set_subtitle("Not connected");
                ui.device_row.set_subtitle("Not connected");
                ui.banner.set_title(&reason);
                ui.banner.set_revealed(true);
                for s in self.strips.borrow().iter() {
                    s.fader.set_meters(&[0.0, 0.0]);
                }
            }
            Event::ReadBack { matrix, routing } => self.on_readback(matrix, routing),
            Event::Meters(levels) => {
                for s in self.strips.borrow().iter() {
                    let n = if s.strip.stereo { 2 } else { 1 };
                    let end = (s.strip.node + n).min(levels.len());
                    s.fader.set_meters(levels.get(s.strip.node..end).unwrap_or(&[]));
                }
            }
            Event::Monitor(m) => self.on_monitor(&m),
            Event::System(s) => self.on_system(&s),
            Event::Error(e) => self.toast(&format!("Device error: {e}")),
        }
    }

    fn on_readback(self: &Rc<Self>, matrix: Option<Vec<i16>>, routing: Vec<Option<u8>>) {
        self.awaiting_readback.set(false);
        let was_first = self.first_run.replace(false);
        // Reads that alias (every row identical) are not trustworthy.
        let usable = matrix.filter(|m| {
            let rows: Vec<&[i16]> = m.chunks(protocol::MIXER_STRIDE).collect();
            let aliased = rows.windows(2).all(|w| w[0] == w[1])
                && rows[0].iter().any(|&c| c != protocol::RAW_MUTE);
            !aliased
        });
        match usable {
            Some(m) => {
                let mut st = self.state.borrow_mut();
                st.import_matrix(&m);
                for (i, pair) in self.profile.outputs.iter().enumerate() {
                    let code = routing.get(pair.first as usize).copied().flatten();
                    if let Some(src) = code.and_then(|c| protocol::route_source_from_code(self.profile.route_scheme, c)) {
                        if pair.sources.contains(&src) {
                            st.routes[i] = src;
                        }
                    }
                }
                drop(st);
                self.toast("Loaded the current mix from the device");
            }
            None if was_first => {
                self.toast("Could not read the device's mix; applied lD Mixer defaults");
            }
            None => {
                self.toast("The device did not report its mix");
            }
        }
        self.rebuild_strips();
        self.sync_panel();
        self.commit();
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
            ui.mon_label.set_label(&protocol::format_db(m.volume));
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
        ui.rate_row.set_subtitle(&match s.sample_rate {
            Some(r) => format!("{:.1} kHz{}", r as f64 / 1000.0, if s.streaming { "" } else { " (idle)" }),
            None => "No stream running".into(),
        });
        if let Some(m) = s.optical_in {
            ui.optical_in.set_selected(m.clamp(0, 1) as u32);
        }
        if let Some(m) = s.optical_out {
            ui.optical_out.set_selected(m.clamp(0, 1) as u32);
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

    fn rename_dialog(self: &Rc<Self>, node: usize) {
        let current = self.state.borrow().channels[node].label.clone();
        self.text_dialog(
            "Rename Channel",
            "Leave empty to restore the default name.",
            &current,
            "Rename",
            move |a, text| {
                a.state.borrow_mut().channels[node].label = text.trim().to_string();
                a.save_pending.set(true);
                a.rebuild_strips();
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
                a.first_run.set(false);
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
            .website("https://github.com/lovebycoma/ld-mixer")
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
