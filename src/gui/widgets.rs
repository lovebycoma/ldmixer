//! Custom-drawn console widgets: a fader with built-in meters, and a knob.
//!
//! Both are thin wrappers around `gtk::DrawingArea` with gesture
//! controllers. Values are positions in `0.0..=1.0`; the owner maps them to
//! dB. `set_value` never fires the change callback, so the UI can follow the
//! device without feedback loops.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;

use ld_mixer::protocol;

/// An RGB color in 0..1.
pub type Rgb = (f64, f64, f64);

pub const fn hex(c: u32) -> Rgb {
    (
        ((c >> 16) & 0xff) as f64 / 255.0,
        ((c >> 8) & 0xff) as f64 / 255.0,
        (c & 0xff) as f64 / 255.0,
    )
}

const TRACK: Rgb = hex(0x0f1218);
const TICK: Rgb = hex(0x5b6474);
const TEXT: Rgb = hex(0x8b93a3);
const CAP: Rgb = hex(0xdfe4ec);
const METER_BG: Rgb = hex(0x161a22);
const METER_LOW: Rgb = hex(0x22c55e);
const METER_MID: Rgb = hex(0xeab308);
const METER_HIGH: Rgb = hex(0xef4444);

fn set(cr: &cairo::Context, c: Rgb) {
    cr.set_source_rgb(c.0, c.1, c.2);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

type Callback = RefCell<Option<Box<dyn Fn(f64)>>>;

// ---------------------------------------------------------------------------
// Fader
// ---------------------------------------------------------------------------

struct FaderInner {
    value: Cell<f64>,
    default: Cell<f64>,
    accent: Cell<Rgb>,
    meters: RefCell<Vec<f32>>,
    peaks: RefCell<Vec<(f32, Instant)>>,
    drag_origin: Cell<f64>,
    on_change: Callback,
}

/// A vertical fader with a dB scale and one meter per channel.
#[derive(Clone)]
pub struct Fader {
    pub area: gtk::DrawingArea,
    inner: Rc<FaderInner>,
}

const FADER_TOP: f64 = 12.0;
const FADER_BOTTOM: f64 = 12.0;
/// Fader geometry, left to right: meters, scale labels, slot with cap.
const METER_W: f64 = 9.0;
const METER_GAP: f64 = 3.0;
const LABEL_W: f64 = 15.0;
const CAP_W: f64 = 30.0;
const CAP_H: f64 = 16.0;
const SCALE_DB: [f32; 8] = [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0];

impl Fader {
    /// `channels` meters are drawn left of the slot (0 for none).
    pub fn new(channels: usize, accent: Rgb) -> Fader {
        let area = gtk::DrawingArea::builder()
            .content_width(group_width(channels).ceil() as i32 + 4)
            .content_height(240)
            .vexpand(true)
            .focusable(true)
            .build();
        area.add_css_class("fader");
        let inner = Rc::new(FaderInner {
            value: Cell::new(0.0),
            default: Cell::new(1.0),
            accent: Cell::new(accent),
            meters: RefCell::new(vec![0.0; channels]),
            peaks: RefCell::new(vec![(0.0, Instant::now()); channels]),
            drag_origin: Cell::new(0.0),
            on_change: RefCell::new(None),
        });
        let f = Fader { area, inner };
        f.install();
        f
    }

    pub fn connect_changed(&self, cb: impl Fn(f64) + 'static) {
        *self.inner.on_change.borrow_mut() = Some(Box::new(cb));
    }

    pub fn value(&self) -> f64 {
        self.inner.value.get()
    }

    /// Set without notifying.
    pub fn set_value(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() > f64::EPSILON {
            self.inner.value.set(v);
            self.area.queue_draw();
        }
    }

    pub fn set_accent(&self, c: Rgb) {
        self.inner.accent.set(c);
        self.area.queue_draw();
    }

    /// Meter levels, 0..1 per channel.
    pub fn set_meters(&self, levels: &[f32]) {
        let mut m = self.inner.meters.borrow_mut();
        let mut peaks = self.inner.peaks.borrow_mut();
        let now = Instant::now();
        for (i, slot) in m.iter_mut().enumerate() {
            let v = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            // Fast attack, gentle release, so the bar does not flicker.
            *slot = if v > *slot { v } else { (*slot * 0.82).max(v) };
            let p = &mut peaks[i];
            if v >= p.0 || now.duration_since(p.1) > Duration::from_millis(1500) {
                *p = (v, now);
            }
        }
        drop((m, peaks));
        self.area.queue_draw();
    }

    fn change(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() <= f64::EPSILON {
            return;
        }
        self.inner.value.set(v);
        self.area.queue_draw();
        if let Some(cb) = self.inner.on_change.borrow().as_ref() {
            cb(v);
        }
    }

    /// Horizontal center of the slot for a widget `width` wide, so labels
    /// above the fader can line up with it.
    pub fn slot_center(&self, width: f64) -> f64 {
        layout(width, self.inner.meters.borrow().len()).2
    }

    fn track_len(&self) -> f64 {
        (self.area.height() as f64 - FADER_TOP - FADER_BOTTOM).max(1.0)
    }

    fn install(&self) {
        let me = self.clone();
        self.area.set_draw_func(move |_, cr, w, h| me.draw(cr, w as f64, h as f64));

        let drag = gtk::GestureDrag::new();
        let me = self.clone();
        drag.connect_drag_begin(move |_, _, y| {
            me.area.grab_focus();
            // Clicking the slot away from the cap jumps there; grabbing the
            // cap keeps its position, so it never leaps under the pointer.
            let cap_y = FADER_TOP + (1.0 - me.value()) * me.track_len();
            if (y - cap_y).abs() > 12.0 {
                me.change(1.0 - (y - FADER_TOP) / me.track_len());
            }
            me.inner.drag_origin.set(me.value());
        });
        let me = self.clone();
        drag.connect_drag_update(move |g, _, dy| {
            let fine = g
                .current_event_state()
                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let scale = if fine { 0.2 } else { 1.0 };
            me.change(me.inner.drag_origin.get() - dy * scale / me.track_len());
        });
        self.area.add_controller(drag);

        let click = gtk::GestureClick::new();
        let me = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                me.change(me.inner.default.get());
            }
        });
        self.area.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let me = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            me.change(me.value() - dy * 0.015);
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let me = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            let step = match key {
                Key::Up => 0.01,
                Key::Down => -0.01,
                Key::Page_Up => 0.1,
                Key::Page_Down => -0.1,
                Key::Home => 1.0,
                Key::End => -1.0,
                _ => return glib::Propagation::Proceed,
            };
            me.change(me.value() + step);
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);
    }

    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        let len = (h - FADER_TOP - FADER_BOTTOM).max(1.0);
        let ypos = |p: f64| FADER_TOP + (1.0 - p) * len;
        let channels = self.inner.meters.borrow().len();
        let (meter_x0, labels_right, slot_x) = layout(w, channels);
        let acc = self.inner.accent.get();
        let v = self.value();

        // Meters on the left, sharing the slot's dB scale.
        let meters = self.inner.meters.borrow();
        let peaks = self.inner.peaks.borrow();
        for (i, &m) in meters.iter().enumerate() {
            let x = meter_x0 + i as f64 * (METER_W + METER_GAP);
            set(cr, METER_BG);
            rounded_rect(cr, x, FADER_TOP, METER_W, len, 1.5);
            let _ = cr.fill();
            if m > 0.002 {
                let top = FADER_TOP + (1.0 - m as f64) * len;
                let grad = cairo::LinearGradient::new(0.0, FADER_TOP + len, 0.0, FADER_TOP);
                grad.add_color_stop_rgb(0.0, METER_LOW.0, METER_LOW.1, METER_LOW.2);
                grad.add_color_stop_rgb(0.7, METER_LOW.0, METER_LOW.1, METER_LOW.2);
                grad.add_color_stop_rgb(0.86, METER_MID.0, METER_MID.1, METER_MID.2);
                grad.add_color_stop_rgb(1.0, METER_HIGH.0, METER_HIGH.1, METER_HIGH.2);
                let _ = cr.set_source(&grad);
                rounded_rect(cr, x, top, METER_W, FADER_TOP + len - top, 1.5);
                let _ = cr.fill();
            }
            let p = peaks[i].0;
            if p > 0.01 {
                let y = (FADER_TOP + (1.0 - p as f64) * len).round() + 0.5;
                set(cr, if p > 0.95 { METER_HIGH } else { CAP });
                cr.set_line_width(1.0);
                cr.move_to(x, y);
                cr.line_to(x + METER_W, y);
                let _ = cr.stroke();
            }
        }
        drop((meters, peaks));

        // Scale between the meters and the slot.
        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(8.5);
        for db in SCALE_DB {
            let y = ypos(protocol::db_to_fader(Some(db))).round() + 0.5;
            set(cr, TICK);
            cr.set_line_width(1.0);
            cr.move_to(labels_right + 2.0, y);
            cr.line_to(labels_right + 5.0, y);
            let _ = cr.stroke();
            let text = format!("{}", db.abs() as i32);
            if let Ok(ext) = cr.text_extents(&text) {
                set(cr, TEXT);
                cr.move_to(labels_right - ext.width() - ext.x_bearing(), y + ext.height() / 2.0);
                let _ = cr.show_text(&text);
            }
        }

        // Slot, filled below the cap in the mix's accent color.
        set(cr, TRACK);
        rounded_rect(cr, slot_x - 2.0, FADER_TOP - 4.0, 4.0, len + 8.0, 2.0);
        let _ = cr.fill();
        cr.set_source_rgba(acc.0, acc.1, acc.2, 0.85);
        rounded_rect(cr, slot_x - 2.0, ypos(v), 4.0, FADER_TOP + len - ypos(v) + 4.0, 2.0);
        let _ = cr.fill();

        // Cap.
        let cy = ypos(v);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        rounded_rect(cr, slot_x - CAP_W / 2.0, cy - CAP_H / 2.0 + 2.0, CAP_W, CAP_H, 4.0);
        let _ = cr.fill();
        let grad = cairo::LinearGradient::new(0.0, cy - CAP_H / 2.0, 0.0, cy + CAP_H / 2.0);
        grad.add_color_stop_rgb(0.0, CAP.0, CAP.1, CAP.2);
        grad.add_color_stop_rgb(1.0, CAP.0 * 0.72, CAP.1 * 0.72, CAP.2 * 0.75);
        let _ = cr.set_source(&grad);
        rounded_rect(cr, slot_x - CAP_W / 2.0, cy - CAP_H / 2.0, CAP_W, CAP_H, 4.0);
        let _ = cr.fill();
        set(cr, acc);
        cr.rectangle(slot_x - CAP_W / 2.0 + 4.0, cy - 1.0, CAP_W - 8.0, 2.0);
        let _ = cr.fill();

        if self.area.has_focus() {
            cr.set_source_rgba(acc.0, acc.1, acc.2, 0.6);
            cr.set_line_width(1.0);
            rounded_rect(cr, slot_x - CAP_W / 2.0 - 1.5, cy - CAP_H / 2.0 - 1.5, CAP_W + 3.0, CAP_H + 3.0, 5.0);
            let _ = cr.stroke();
        }
    }
}

/// Width of the meters + scale + slot group.
fn group_width(channels: usize) -> f64 {
    let n = channels as f64;
    let meters = if channels > 0 { n * METER_W + (n - 1.0) * METER_GAP + 6.0 } else { 0.0 };
    meters + LABEL_W + 6.0 + CAP_W
}

/// `(first meter x, right edge of the scale labels, slot center)`, with
/// the group centered in the widget.
fn layout(w: f64, channels: usize) -> (f64, f64, f64) {
    let x0 = ((w - group_width(channels)) / 2.0).max(2.0).round();
    let n = channels as f64;
    let meters = if channels > 0 { n * METER_W + (n - 1.0) * METER_GAP + 6.0 } else { 0.0 };
    let labels_right = x0 + meters + LABEL_W;
    let slot_x = (labels_right + 6.0 + CAP_W / 2.0).round();
    (x0, labels_right, slot_x)
}

// ---------------------------------------------------------------------------
// Knob
// ---------------------------------------------------------------------------

struct KnobInner {
    value: Cell<f64>,
    default: f64,
    bipolar: bool,
    accent: Cell<Rgb>,
    drag_origin: Cell<f64>,
    on_change: Callback,
}

/// A rotary control. Bipolar knobs (pan) fill from the center.
#[derive(Clone)]
pub struct Knob {
    pub area: gtk::DrawingArea,
    inner: Rc<KnobInner>,
}

const ARC_START: f64 = 0.75 * PI;
const ARC_SWEEP: f64 = 1.5 * PI;

impl Knob {
    pub fn new(size: i32, bipolar: bool, default: f64, accent: Rgb) -> Knob {
        let area = gtk::DrawingArea::builder()
            .content_width(size)
            .content_height(size)
            .halign(gtk::Align::Center)
            .focusable(true)
            .build();
        area.add_css_class("knob");
        let inner = Rc::new(KnobInner {
            value: Cell::new(default),
            default,
            bipolar,
            accent: Cell::new(accent),
            drag_origin: Cell::new(0.0),
            on_change: RefCell::new(None),
        });
        let k = Knob { area, inner };
        k.install();
        k
    }

    pub fn connect_changed(&self, cb: impl Fn(f64) + 'static) {
        *self.inner.on_change.borrow_mut() = Some(Box::new(cb));
    }

    pub fn value(&self) -> f64 {
        self.inner.value.get()
    }

    pub fn set_value(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() > f64::EPSILON {
            self.inner.value.set(v);
            self.area.queue_draw();
        }
    }

    pub fn set_accent(&self, c: Rgb) {
        self.inner.accent.set(c);
        self.area.queue_draw();
    }

    fn change(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() <= f64::EPSILON {
            return;
        }
        self.inner.value.set(v);
        self.area.queue_draw();
        if let Some(cb) = self.inner.on_change.borrow().as_ref() {
            cb(v);
        }
    }

    fn install(&self) {
        let me = self.clone();
        self.area.set_draw_func(move |_, cr, w, h| me.draw(cr, w as f64, h as f64));

        let drag = gtk::GestureDrag::new();
        let me = self.clone();
        drag.connect_drag_begin(move |_, _, _| {
            me.area.grab_focus();
            me.inner.drag_origin.set(me.value());
        });
        let me = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let fine = g
                .current_event_state()
                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let travel = if fine { 900.0 } else { 180.0 };
            me.change(me.inner.drag_origin.get() + (dx - dy) / travel);
        });
        self.area.add_controller(drag);

        let click = gtk::GestureClick::new();
        let me = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                me.change(me.inner.default);
            }
        });
        self.area.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let me = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            me.change(me.value() - dy * 0.02);
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let me = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            let step = match key {
                Key::Up | Key::Right => 0.01,
                Key::Down | Key::Left => -0.01,
                _ => return glib::Propagation::Proceed,
            };
            me.change(me.value() + step);
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);
    }

    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        let (cx, cy) = (w / 2.0, h / 2.0);
        let r = w.min(h) / 2.0 - 3.0;
        let ring = (r * 0.16).clamp(2.5, 7.0);
        let acc = self.inner.accent.get();
        let v = self.value();
        let angle = |p: f64| ARC_START + p * ARC_SWEEP;

        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_width(ring);
        set(cr, TRACK);
        cr.arc(cx, cy, r - ring / 2.0, angle(0.0), angle(1.0));
        let _ = cr.stroke();

        let (a0, a1) = if self.inner.bipolar {
            let (a, b) = (angle(0.5), angle(v));
            if a < b { (a, b) } else { (b, a) }
        } else {
            (angle(0.0), angle(v))
        };
        if a1 - a0 > 0.001 {
            set(cr, acc);
            cr.arc(cx, cy, r - ring / 2.0, a0, a1);
            let _ = cr.stroke();
        }

        // Body.
        let body = r - ring - 3.0;
        let grad = cairo::LinearGradient::new(0.0, cy - body, 0.0, cy + body);
        grad.add_color_stop_rgb(0.0, 0.25, 0.27, 0.32);
        grad.add_color_stop_rgb(1.0, 0.13, 0.14, 0.17);
        let _ = cr.set_source(&grad);
        cr.arc(cx, cy, body, 0.0, 2.0 * PI);
        let _ = cr.fill();

        // Pointer.
        let a = angle(v);
        set(cr, CAP);
        cr.set_line_width((body * 0.12).clamp(1.5, 3.5));
        cr.move_to(cx + a.cos() * body * 0.35, cy + a.sin() * body * 0.35);
        cr.line_to(cx + a.cos() * body * 0.85, cy + a.sin() * body * 0.85);
        let _ = cr.stroke();

        if self.area.has_focus() {
            cr.set_source_rgba(acc.0, acc.1, acc.2, 0.5);
            cr.set_line_width(1.0);
            cr.arc(cx, cy, r + 1.5, 0.0, 2.0 * PI);
            let _ = cr.stroke();
        }
    }
}
