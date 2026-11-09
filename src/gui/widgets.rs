//! Custom-drawn console widgets: a fader with built-in meters, and a knob.
//!
//! The fader redraws at meter rate, so it renders with GTK's GPU render
//! nodes ([`SnapArea`]): rectangles, gradients and rounded clips, plus a
//! texture of the static scale that is rasterized once per size. The knob
//! changes rarely and stays a Cairo `gtk::DrawingArea`. Both use gesture
//! controllers. Values are positions in `0.0..=1.0`; the owner maps them to
//! dB. `set_value` never fires the change callback, so the UI can follow the
//! device without feedback loops.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo;
use gtk::glib;
use gtk::graphene::{Point, Rect};
use gtk::gsk::{ColorStop, RoundedRect};
use gtk::prelude::*;
use gtk::gdk;

use ldmixer::protocol;

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

fn rgba(c: Rgb, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.0 as f32, c.1 as f32, c.2 as f32, a)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

fn rounded(x: f64, y: f64, w: f64, h: f64, r: f64) -> RoundedRect {
    RoundedRect::from_rect(rect(x, y, w, h), r as f32)
}

// ---------------------------------------------------------------------------
// SnapArea: a DrawingArea-like widget that draws with render nodes
// ---------------------------------------------------------------------------

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    type DrawFn = Box<dyn Fn(&gtk::Snapshot, f64, f64)>;
    type ResizeFn = Box<dyn Fn(i32, i32)>;

    #[derive(Default)]
    pub struct SnapArea {
        pub draw: RefCell<Option<DrawFn>>,
        pub resized: RefCell<Vec<ResizeFn>>,
        pub content: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SnapArea {
        const NAME: &'static str = "LdSnapArea";
        type Type = super::SnapArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SnapArea {}

    impl WidgetImpl for SnapArea {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.content.get();
            let v = if orientation == gtk::Orientation::Horizontal { w } else { h };
            (v, v, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            for cb in self.resized.borrow().iter() {
                cb(width, height);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if let Some(draw) = self.draw.borrow().as_ref() {
                draw(snapshot, obj.width() as f64, obj.height() as f64);
            }
        }
    }
}

glib::wrapper! {
    /// A widget that renders through a snapshot callback (GPU render nodes)
    /// instead of Cairo, for content that redraws often.
    pub struct SnapArea(ObjectSubclass<imp::SnapArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SnapArea {
    pub fn new(content_width: i32, content_height: i32) -> SnapArea {
        let a: SnapArea = glib::Object::new();
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        a.imp().content.set((content_width, content_height));
        a
    }

    pub fn set_draw_func(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        *self.imp().draw.borrow_mut() = Some(Box::new(f));
    }

    /// Called with the new width and height after every allocation.
    pub fn connect_resize(&self, f: impl Fn(i32, i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().resized.borrow_mut().push(Box::new(f));
    }
}

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
    /// The parts that only change with the widget's size (meter wells,
    /// scale, slot), rasterized once per size into a GPU texture.
    static_layer: RefCell<Option<(i32, i32, i32, gdk::Texture)>>,
}

/// A vertical fader with a dB scale and one meter per channel.
#[derive(Clone)]
pub struct Fader {
    pub area: SnapArea,
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
        let area = SnapArea::new(group_width(channels).ceil() as i32 + 4, 240);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.add_css_class("fader");
        let inner = Rc::new(FaderInner {
            value: Cell::new(0.0),
            default: Cell::new(1.0),
            accent: Cell::new(accent),
            meters: RefCell::new(vec![0.0; channels]),
            peaks: RefCell::new(vec![(0.0, Instant::now()); channels]),
            drag_origin: Cell::new(0.0),
            on_change: RefCell::new(None),
            static_layer: RefCell::new(None),
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
        let mut changed = false;
        for (i, slot) in m.iter_mut().enumerate() {
            let v = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            // Fast attack, gentle release, so the bar does not flicker.
            let mut next = if v > *slot { v } else { (*slot * 0.82).max(v) };
            if next < 0.002 {
                next = 0.0;
            }
            changed |= (next - *slot).abs() > 0.001;
            *slot = next;
            let p = &mut peaks[i];
            if v >= p.0 || now.duration_since(p.1) > Duration::from_millis(1500) {
                changed |= (p.0 - v).abs() > 0.001;
                *p = (v, now);
            }
        }
        drop((m, peaks));
        // Silent or steady meters cost nothing.
        if changed {
            self.area.queue_draw();
        }
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
        self.area.set_draw_func(move |snap, w, h| me.draw(snap, w, h));

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

    /// Draw the size-dependent background (meter wells, scale, slot).
    fn draw_static(&self, cr: &cairo::Context, w: f64, h: f64) {
        let len = (h - FADER_TOP - FADER_BOTTOM).max(1.0);
        let ypos = |p: f64| FADER_TOP + (1.0 - p) * len;
        let channels = self.inner.meters.borrow().len();
        let (meter_x0, labels_right, slot_x) = layout(w, channels);
        for i in 0..channels {
            let x = meter_x0 + i as f64 * (METER_W + METER_GAP);
            set(cr, METER_BG);
            rounded_rect(cr, x, FADER_TOP, METER_W, len, 1.5);
            let _ = cr.fill();
        }
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
        set(cr, TRACK);
        rounded_rect(cr, slot_x - 2.0, FADER_TOP - 4.0, 4.0, len + 8.0, 2.0);
        let _ = cr.fill();
    }

    /// The static layer as a texture, re-rasterized only when the size or
    /// scale changes; the renderer keeps it on the GPU between frames.
    fn static_texture(&self, w: f64, h: f64) -> Option<gdk::Texture> {
        let scale = self.area.scale_factor().max(1);
        let (wi, hi) = (w.ceil() as i32, h.ceil() as i32);
        let mut cache = self.inner.static_layer.borrow_mut();
        let fresh = !matches!(&*cache, Some((cw, ch, cs, _)) if *cw == wi && *ch == hi && *cs == scale);
        if fresh {
            *cache = (|| {
                let mut surf = cairo::ImageSurface::create(cairo::Format::ARgb32, wi * scale, hi * scale).ok()?;
                surf.set_device_scale(scale as f64, scale as f64);
                {
                    let c = cairo::Context::new(&surf).ok()?;
                    self.draw_static(&c, w, h);
                }
                surf.flush();
                let stride = surf.stride() as usize;
                let data = surf.data().ok()?.to_vec();
                // Cairo ARGB32 is premultiplied BGRA in memory on little endian.
                let tex = gdk::MemoryTexture::new(
                    wi * scale,
                    hi * scale,
                    gdk::MemoryFormat::B8g8r8a8Premultiplied,
                    &glib::Bytes::from_owned(data),
                    stride,
                );
                Some((wi, hi, scale, tex.upcast::<gdk::Texture>()))
            })();
        }
        cache.as_ref().map(|(_, _, _, t)| t.clone())
    }

    fn draw(&self, snap: &gtk::Snapshot, w: f64, h: f64) {
        let len = (h - FADER_TOP - FADER_BOTTOM).max(1.0);
        let ypos = |p: f64| FADER_TOP + (1.0 - p) * len;
        let channels = self.inner.meters.borrow().len();
        let (meter_x0, _, slot_x) = layout(w, channels);
        let acc = self.inner.accent.get();
        let v = self.value();

        if let Some(tex) = self.static_texture(w, h) {
            snap.append_texture(&tex, &rect(0.0, 0.0, w.ceil(), h.ceil()));
        }

        // Meter levels: one gradient spanning the full height, clipped to
        // the lit part so the colors stay tied to the dB scale.
        let stops = [
            ColorStop::new(0.0, rgba(METER_LOW, 1.0)),
            ColorStop::new(0.7, rgba(METER_LOW, 1.0)),
            ColorStop::new(0.86, rgba(METER_MID, 1.0)),
            ColorStop::new(1.0, rgba(METER_HIGH, 1.0)),
        ];
        let meters = self.inner.meters.borrow();
        let peaks = self.inner.peaks.borrow();
        for (i, &m) in meters.iter().enumerate() {
            let x = meter_x0 + i as f64 * (METER_W + METER_GAP);
            if m > 0.002 {
                let top = FADER_TOP + (1.0 - m as f64) * len;
                snap.push_rounded_clip(&rounded(x, top, METER_W, FADER_TOP + len - top, 1.5));
                snap.append_linear_gradient(
                    &rect(x, top, METER_W, FADER_TOP + len - top),
                    &Point::new(x as f32, (FADER_TOP + len) as f32),
                    &Point::new(x as f32, FADER_TOP as f32),
                    &stops,
                );
                snap.pop();
            }
            let p = peaks[i].0;
            if p > 0.01 {
                let y = (FADER_TOP + (1.0 - p as f64) * len).round();
                snap.append_color(&rgba(if p > 0.95 { METER_HIGH } else { CAP }, 1.0), &rect(x, y, METER_W, 1.0));
            }
        }
        drop((meters, peaks));

        // Slot fill below the cap, in the mix's accent color.
        snap.push_rounded_clip(&rounded(slot_x - 2.0, ypos(v), 4.0, FADER_TOP + len - ypos(v) + 4.0, 2.0));
        snap.append_color(&rgba(acc, 0.85), &rect(slot_x - 2.0, ypos(v), 4.0, FADER_TOP + len - ypos(v) + 4.0));
        snap.pop();

        // Cap: shadow, body, accent line.
        let cy = ypos(v);
        let (cx, top) = (slot_x - CAP_W / 2.0, cy - CAP_H / 2.0);
        snap.push_rounded_clip(&rounded(cx, top + 2.0, CAP_W, CAP_H, 4.0));
        snap.append_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.45), &rect(cx, top + 2.0, CAP_W, CAP_H));
        snap.pop();
        snap.push_rounded_clip(&rounded(cx, top, CAP_W, CAP_H, 4.0));
        snap.append_linear_gradient(
            &rect(cx, top, CAP_W, CAP_H),
            &Point::new(cx as f32, top as f32),
            &Point::new(cx as f32, (top + CAP_H) as f32),
            &[
                ColorStop::new(0.0, rgba(CAP, 1.0)),
                ColorStop::new(1.0, rgba((CAP.0 * 0.72, CAP.1 * 0.72, CAP.2 * 0.75), 1.0)),
            ],
        );
        snap.pop();
        snap.append_color(&rgba(acc, 1.0), &rect(cx + 4.0, cy - 1.0, CAP_W - 8.0, 2.0));

        if self.area.has_focus() {
            let ring = rounded(cx - 1.5, top - 1.5, CAP_W + 3.0, CAP_H + 3.0, 5.0);
            let c = rgba(acc, 0.6);
            snap.append_border(&ring, &[1.0; 4], &[c, c, c, c]);
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
