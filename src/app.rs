//! The GTK app: the wheel window, the status pop-up, trim/resize dialogs.
//!
//! Runs as a D-Bus activated service. The GNOME Shell extension calls its
//! actions (show-wheel, set-tools, drag-ended) over org.gtk.Actions; the wheel
//! is a plain drop target, so the dropped file list comes straight from GTK.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use shiftdrop::convert::{available_targets, which};
use shiftdrop::formats::{Category, Fmt, detect, fallback_dir, name};
use shiftdrop::jobs::{plan_convert, run_batch, sweep_leftovers};
use shiftdrop::media::{gpu_h264, probe};
use shiftdrop::tools::{Asks, Params, Tool, available_tools, plan_tool};

use crate::wheel;

const APP_ID: &str = "dev.jayhemnani.ShiftDrop";
const WHEEL_TITLE: &str = "ShiftDrop Wheel"; // the extension places windows by these titles
const STATUS_TITLE: &str = "ShiftDrop Status";
const SUCCESS_LINGER: Duration = Duration::from_millis(4000);
const RESIZE_EDGES: [u32; 7] = [3840, 2560, 1920, 1280, 1080, 720, 512];

/// One line on stderr (the user journal, when D-Bus started us) while the flag file exists.
/// Same switch as the extension: `touch ~/.cache/shiftdrop-debug`.
fn debug(message: &str) {
    if glib::user_cache_dir().join("shiftdrop-debug").exists() {
        let now = glib::DateTime::now_local().and_then(|t| t.format("%H:%M:%S")).unwrap_or_default();
        eprintln!("{now} shiftdrop: {message}");
    }
}

const CSS: &str = "
window.ot-wheel, window.ot-wheel > * { background: transparent; box-shadow: none; }
window.ot-status { background: rgba(12, 12, 14, 0.96); border-radius: 18px; color: white; }
window.ot-status .title { font-weight: bold; }
window.ot-status .dim { opacity: 0.6; }
window.ot-status .error { color: #ff7b72; }
window.ot-status progressbar progress { background: #ff8a1f; }
window.ot-status button { background: rgba(255, 255, 255, 0.10); color: white; }
";

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

/// (paths on disk, names of the rest). A phone, a network share or the
/// trash can hand over files with no local path; those can't be converted.
fn local_paths(files: &[gio::File]) -> (Vec<PathBuf>, Vec<String>) {
    let mut paths = vec![];
    let mut other = vec![];
    for f in files {
        match f.path() {
            Some(p) => paths.push(p),
            None => other.push(f.basename().map_or_else(|| f.uri().to_string(), |b| b.display().to_string())),
        }
    }
    (paths, other)
}

fn unreadable_line(names: &[String]) -> String {
    let more = if names.len() > 3 { " …" } else { "" };
    format!("Skipped {}{more}: not a file on this computer (phone, network or trash). Copy it to a folder first.",
            names[..names.len().min(3)].join(", "))
}

/// What a wheel segment does.
#[derive(Clone, Copy)]
enum Choice {
    Convert(Fmt),
    Tool(&'static Tool),
}

impl Choice {
    fn title(self) -> String {
        match self {
            Choice::Convert(fmt) => format!("Converting to {}", fmt.label()),
            Choice::Tool(tool) => tool.title.into(),
        }
    }
}

/// From the work thread to the window.
enum Update {
    Progress { done: usize, total: usize, fraction: f64 },
    Done { outputs: Vec<PathBuf>, errors: Vec<String> },
}

/// Ring of formats (or tools). Pick one by dropping files on it, or by clicking.
#[derive(Default)]
struct WheelState {
    files: Vec<PathBuf>,
    unreadable: Vec<String>, // dropped, but not on this computer's disk
    tools: bool,
    items: Vec<(String, Choice)>,
    hover: Option<usize>,
    picking: bool, // opened from Files with a known selection: click instead of drop
}

/// Progress, then the result with Open / Show in Files. Errors stay until closed.
struct Status {
    window: gtk::Window,
    title: gtk::Label,
    detail: gtk::Label,
    bar: gtk::ProgressBar,
    cancel_btn: gtk::Button,
    open_btn: gtk::Button,
    show_btn: gtk::Button,
    buttons: gtk::Box,
    outputs: RefCell<Vec<PathBuf>>,
    hovered: Cell<bool>,
    linger: RefCell<Option<glib::SourceId>>,
}

struct App {
    gapp: gtk::Application,
    wheel_window: gtk::Window,
    area: gtk::DrawingArea,
    wheel: RefCell<WheelState>,
    status: Status,
    busy: Cell<bool>,
    cancel: RefCell<Arc<AtomicBool>>,
    dropping: Cell<bool>,
}

pub fn main(args: &[String]) -> glib::ExitCode {
    let gapp = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::HANDLES_OPEN);
    let app: Rc<RefCell<Option<Rc<App>>>> = Rc::default();
    let hold = RefCell::new(None);

    gapp.connect_startup({
        let app = app.clone();
        move |gapp| {
            let css = gtk::CssProvider::new();
            css.load_from_string(CSS);
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            for leftover in sweep_leftovers() {
                debug(&format!("removed half-written {} from a job that was killed", leftover.display()));
            }
            let built = App::new(gapp);
            built.add_actions();
            *app.borrow_mut() = Some(built);
            *hold.borrow_mut() = Some(gapp.hold()); // a service: stay alive between drags
        }
    });
    gapp.connect_activate(|_| println!("ShiftDrop is running. Drag a file, hold Shift, and drop it on a format."));
    gapp.connect_open({
        let app = app.clone();
        // `shiftdrop FILE...`, used by the Files right-click script: pick by clicking
        move |_, files, _hint| {
            let Some(app) = app.borrow().clone() else { return };
            let (paths, unreadable) = local_paths(files);
            app.reset_wheel(paths, false, true, unreadable);
            app.wheel_window.present();
        }
    });
    gapp.run_with_args(args)
}

impl App {
    fn new(gapp: &gtk::Application) -> Rc<App> {
        let wheel_window = gtk::Window::builder().application(gapp).title(WHEEL_TITLE)
            .decorated(false).resizable(false).build();
        wheel_window.add_css_class("ot-wheel");
        let area = gtk::DrawingArea::builder().content_width(wheel::SIZE).content_height(wheel::SIZE).build();
        wheel_window.set_child(Some(&area));

        let app = Rc::new(App {
            gapp: gapp.clone(),
            wheel_window,
            area,
            wheel: RefCell::default(),
            status: Status::new(gapp),
            busy: Cell::new(false),
            cancel: RefCell::default(),
            dropping: Cell::new(false),
        });
        app.connect_wheel();
        app.connect_status();
        app
    }

    fn add_actions(self: &Rc<Self>) {
        let show = gio::SimpleAction::new("show-wheel", Some(glib::VariantTy::BOOLEAN));
        let app = self.clone();
        show.connect_activate(move |_, p| app.show_wheel(p.and_then(|v| v.get()).unwrap_or(false)));
        let set_tools = gio::SimpleAction::new("set-tools", Some(glib::VariantTy::BOOLEAN));
        let app = self.clone();
        set_tools.connect_activate(move |_, p| app.set_tools(p.and_then(|v| v.get()).unwrap_or(false)));
        let ended = gio::SimpleAction::new("drag-ended", None);
        let app = self.clone();
        ended.connect_activate(move |_, _| app.drag_ended());
        for action in [show, set_tools, ended] {
            self.gapp.add_action(&action);
        }
    }

    // --- the wheel: state ----------------------------------------------------

    fn connect_wheel(self: &Rc<Self>) {
        let app = Rc::downgrade(self);
        self.area.set_draw_func(move |_, cr, w, _h| {
            let Some(app) = app.upgrade() else { return };
            let (center, subtitle) = app.center();
            let wheel = app.wheel.borrow();
            let labels: Vec<String> = wheel.items.iter().map(|(l, _)| l.clone()).collect();
            wheel::draw(cr, &labels, wheel.hover, &center, &subtitle, w);
        });

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY); // never MOVE: the source keeps its file
        drop.set_preload(true);
        let app = self.clone();
        drop.connect_value_notify(move |target| app.on_preload(target));
        let app = self.clone();
        drop.connect_motion(move |_, x, y| {
            let count = app.wheel.borrow().items.len();
            app.set_hover(wheel::hit(count, x, y, wheel::SIZE));
            gdk::DragAction::COPY
        });
        let app = self.clone();
        drop.connect_leave(move |_| app.set_hover(None));
        let app = self.clone();
        drop.connect_drop(move |target, value, x, y| app.on_drop(target, value, x, y));
        self.area.add_controller(drop);

        let motion = gtk::EventControllerMotion::new();
        let app = self.clone();
        motion.connect_motion(move |_, x, y| {
            let (picking, count) = {
                let wheel = app.wheel.borrow();
                (wheel.picking, wheel.items.len())
            };
            if picking {
                app.set_hover(wheel::hit(count, x, y, wheel::SIZE));
            }
        });
        self.area.add_controller(motion);
        let click = gtk::GestureClick::builder().button(0).build();
        let app = self.clone();
        click.connect_released(move |gesture, _, x, y| app.on_click(gesture, x, y));
        self.area.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let window = self.wheel_window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.set_visible(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.wheel_window.add_controller(keys);
    }

    fn reset_wheel(&self, files: Vec<PathBuf>, tools: bool, picking: bool, unreadable: Vec<String>) {
        *self.wheel.borrow_mut() = WheelState { files, unreadable, tools, picking, ..Default::default() };
        self.refresh();
    }

    fn set_tools(&self, tools: bool) {
        let mut wheel = self.wheel.borrow_mut();
        wheel.tools = tools;
        wheel.hover = None;
        drop(wheel);
        self.refresh();
    }

    fn refresh(&self) {
        let mut wheel = self.wheel.borrow_mut();
        let formats: Option<Vec<Fmt>> = wheel.files.iter().map(detect).collect();
        wheel.items = match formats {
            Some(formats) if !formats.is_empty() && wheel.tools => available_tools(&formats, which)
                .into_iter().map(|t| (t.label.to_string(), Choice::Tool(t))).collect(),
            Some(formats) if !formats.is_empty() => available_targets(&formats, which)
                .into_iter().map(|f| (f.label(), Choice::Convert(f))).collect(),
            _ => vec![],
        };
        drop(wheel);
        self.area.queue_draw();
    }

    fn center(&self) -> (String, String) {
        let wheel = self.wheel.borrow();
        let mode = if wheel.tools { "TOOLS" } else { "CONVERT" };
        if let Some(i) = wheel.hover {
            return (wheel.items[i].0.clone(), plural(wheel.files.len(), "file"));
        }
        let (center, subtitle) = if wheel.files.is_empty() && !wheel.unreadable.is_empty() {
            ("NOTHING", "not a local file")
        } else if wheel.files.is_empty() {
            (mode, "move onto the wheel")
        } else if wheel.items.is_empty() {
            ("NOTHING", "can't handle this file")
        } else if wheel.picking {
            (mode, "right-click: switch")
        } else if !wheel.tools {
            (mode, "Ctrl: tools")
        } else {
            (mode, "release Ctrl: formats")
        };
        (center.into(), subtitle.into())
    }

    fn set_hover(&self, index: Option<usize>) {
        let mut wheel = self.wheel.borrow_mut();
        if index != wheel.hover {
            wheel.hover = index;
            drop(wheel);
            self.area.queue_draw();
        }
    }

    // --- the wheel: drag and drop --------------------------------------------

    /// Take the dropped file list; true when it differs from what the wheel showed.
    fn take_files(&self, value: &glib::Value) -> bool {
        let files = value.get::<gdk::FileList>().map(|list| list.files()).unwrap_or_default();
        let (files, unreadable) = local_paths(&files);
        let mut wheel = self.wheel.borrow_mut();
        wheel.unreadable = unreadable;
        if files == wheel.files {
            return false;
        }
        wheel.files = files;
        drop(wheel);
        self.refresh();
        true
    }

    fn on_preload(&self, target: &gtk::DropTarget) {
        let Some(value) = target.value() else { return };
        if self.take_files(&value) {
            let wheel = self.wheel.borrow();
            let names: Vec<String> = wheel.files.iter().take(5).map(|f| name(f)).collect();
            let not_local = if wheel.unreadable.is_empty() {
                String::new()
            } else {
                format!(", {} not local", wheel.unreadable.len())
            };
            debug(&format!("drag carries {} file(s): {names:?} -> {} choices{not_local}",
                           wheel.files.len(), wheel.items.len()));
        }
    }

    fn on_drop(self: &Rc<Self>, target: &gtk::DropTarget, value: &glib::Value, x: f64, y: f64) -> bool {
        // Shift means "move" to Files. Only ever finish a drop as a copy, so the
        // source app has no reason to delete its file afterwards.
        let actions = target.current_drop().map_or(gdk::DragAction::empty(), |d| d.actions());
        debug(&format!("drop at {x:.0},{y:.0} actions={actions:?}"));
        if !actions.contains(gdk::DragAction::COPY) {
            self.dropping.set(true);
            self.wheel_window.set_visible(false);
            self.finished(vec![], vec!["That drag only allowed moving the file, so nothing was done.".into()]);
            return false;
        }
        self.take_files(value);
        let (files, unreadable, choice) = {
            let wheel = self.wheel.borrow();
            let index = wheel::hit(wheel.items.len(), x, y, wheel::SIZE);
            (wheel.files.clone(), wheel.unreadable.clone(), index.map(|i| wheel.items[i].1))
        };
        self.dropping.set(true);
        self.wheel_window.set_visible(false);
        if files.is_empty() && !unreadable.is_empty() {
            self.finished(vec![], vec![unreadable_line(&unreadable)]);
            return false;
        }
        let Some(choice) = choice else { return false }; // dropped on the centre: cancel
        self.start(files, choice);
        true
    }

    // --- the wheel: click mode -----------------------------------------------

    fn on_click(self: &Rc<Self>, gesture: &gtk::GestureClick, x: f64, y: f64) {
        if gesture.current_button() == gdk::BUTTON_SECONDARY {
            let tools = self.wheel.borrow().tools;
            self.set_tools(!tools);
            return;
        }
        let (files, choice) = {
            let wheel = self.wheel.borrow();
            let index = wheel::hit(wheel.items.len(), x, y, wheel::SIZE);
            (wheel.files.clone(), index.map(|i| wheel.items[i].1))
        };
        self.wheel_window.set_visible(false);
        if let Some(choice) = choice.filter(|_| !files.is_empty()) {
            self.start(files, choice);
        }
    }

    // --- actions from the extension ------------------------------------------

    fn show_wheel(&self, tools: bool) {
        debug(&format!("show-wheel tools={tools}"));
        self.dropping.set(false);
        self.reset_wheel(vec![], tools, false, vec![]);
        self.wheel_window.present();
    }

    fn drag_ended(self: &Rc<Self>) {
        // the shell's drag-end can arrive just before our drop handler runs
        debug(&format!("drag-ended (dropped on wheel: {})", self.dropping.get()));
        let app = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            if !app.dropping.get() && !app.wheel.borrow().picking {
                app.wheel_window.set_visible(false);
            }
        });
    }

    // --- running work --------------------------------------------------------

    fn start(self: &Rc<Self>, files: Vec<PathBuf>, choice: Choice) {
        match choice {
            Choice::Tool(tool) if tool.asks.is_some() => self.ask(tool, files),
            _ => self.run_in_background(files, choice, Params::default()),
        }
    }

    fn run_in_background(self: &Rc<Self>, files: Vec<PathBuf>, choice: Choice, params: Params) {
        if self.busy.get() {
            self.finished(vec![], vec!["Still working on the last batch. Try again when it's done.".into()]);
            return;
        }
        self.busy.set(true);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = cancel.clone();
        let unreadable = self.wheel.borrow().unreadable.clone();
        let title = choice.title();
        self.working(&title, 0, files.len(), 0.0);

        let (send, updates) = async_channel::unbounded();
        thread::spawn(move || {
            let (outputs, errors) = work(files, choice, params, &cancel, unreadable, &send);
            let _ = send.send_blocking(Update::Done { outputs, errors });
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(update) = updates.recv().await {
                match update {
                    Update::Progress { done, total, fraction } => app.working(&title, done, total, fraction),
                    Update::Done { outputs, errors } => {
                        app.busy.set(false);
                        app.finished(outputs, errors);
                    }
                }
            }
        });
    }

    // --- dialogs for tools that need a value ---------------------------------

    fn ask(self: &Rc<Self>, tool: &'static Tool, files: Vec<PathBuf>) {
        let trim = tool.asks == Some(Asks::Trim);
        let win = gtk::Window::builder().application(&self.gapp)
            .title(if trim { "Trim" } else { "Resize images" }).modal(true).resizable(false).build();
        let grid = gtk::Grid::builder().row_spacing(10).column_spacing(12)
            .margin_top(16).margin_bottom(16).margin_start(16).margin_end(16).build();
        let go = gtk::Button::builder().label("Start").css_classes(["suggested-action"]).build();
        let cancel = gtk::Button::with_label("Cancel");
        let w = win.clone();
        cancel.connect_clicked(move |_| w.destroy());
        let label = |text: &str| gtk::Label::builder().label(text).xalign(0.0).build();

        let params: Box<dyn Fn() -> Params> = if trim {
            let length = probe(&files[0]).duration.unwrap_or(0.0);
            if length == 0.0 {
                self.finished(vec![], vec![format!("{}: can't read its length", name(&files[0]))]);
                return;
            }
            let start = gtk::SpinButton::with_range(0.0, length, 0.1);
            let end = gtk::SpinButton::with_range(0.0, length, 0.1);
            start.set_digits(1);
            end.set_digits(1);
            end.set_value(length);
            grid.attach(&label(&format!("{} · {length:.1}s", name(&files[0]))), 0, 0, 2, 1);
            grid.attach(&label("Start (s)"), 0, 1, 1, 1);
            grid.attach(&start, 1, 1, 1, 1);
            grid.attach(&label("End (s)"), 0, 2, 1, 1);
            grid.attach(&end, 1, 2, 1, 1);
            Box::new(move || Params { start: start.value(), end: end.value(), ..Default::default() })
        } else {
            let names: Vec<String> = RESIZE_EDGES.iter().map(|e| format!("{e} px")).collect();
            let edges = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
            edges.set_selected(2);
            grid.attach(&label("Longest side"), 0, 0, 1, 1);
            grid.attach(&edges, 1, 0, 1, 1);
            let hint = label("Smaller images stay as they are.");
            hint.add_css_class("dim-label");
            grid.attach(&hint, 0, 1, 2, 1);
            Box::new(move || Params { long_edge: RESIZE_EDGES[edges.selected() as usize], ..Default::default() })
        };

        let (app, w) = (self.clone(), win.clone());
        go.connect_clicked(move |_| {
            let values = params();
            w.destroy();
            app.run_in_background(files.clone(), Choice::Tool(tool), values);
        });
        let row = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        row.append(&cancel);
        row.append(&go);
        grid.attach(&row, 0, 3, 2, 1);
        win.set_child(Some(&grid));
        win.set_default_widget(Some(&go));
        win.present();
    }

    // --- the status pop-up ---------------------------------------------------

    fn connect_status(self: &Rc<Self>) {
        let s = &self.status;
        let app = Rc::downgrade(self);
        s.cancel_btn.connect_clicked(move |_| {
            if let Some(app) = app.upgrade() {
                app.cancel.borrow().store(true, Ordering::Relaxed);
                app.status.cancel_btn.set_sensitive(false);
                app.status.title.set_text("Cancelling…");
            }
        });
        let app = Rc::downgrade(self);
        s.open_btn.connect_clicked(move |_| {
            if let Some(app) = app.upgrade() {
                app.open();
            }
        });
        let app = Rc::downgrade(self);
        s.show_btn.connect_clicked(move |_| {
            if let Some(app) = app.upgrade() {
                app.show_in_files();
            }
        });
        let hover = gtk::EventControllerMotion::new();
        let app = Rc::downgrade(self);
        hover.connect_enter(move |_, _, _| {
            if let Some(app) = app.upgrade() {
                app.status.hovered.set(true);
            }
        });
        let app = Rc::downgrade(self);
        hover.connect_leave(move |_| {
            if let Some(app) = app.upgrade() {
                app.on_status_leave();
            }
        });
        s.window.add_controller(hover);
    }

    fn working(&self, title: &str, done: usize, total: usize, fraction: f64) {
        let s = &self.status;
        let cancelling = self.cancel.borrow().load(Ordering::Relaxed);
        self.cancel_linger();
        s.title.set_text(if cancelling { "Cancelling…" } else { title });
        s.title.remove_css_class("error");
        s.detail.set_text(&if total > 1 { format!("{} of {total}", done + 1) } else { String::new() });
        s.detail.set_visible(total > 1);
        s.bar.set_visible(true);
        s.bar.set_fraction(fraction);
        s.cancel_btn.set_visible(true);
        s.cancel_btn.set_sensitive(!cancelling);
        s.buttons.set_visible(false);
        s.window.present();
    }

    fn finished(self: &Rc<Self>, outputs: Vec<PathBuf>, errors: Vec<String>) {
        let s = &self.status;
        s.bar.set_visible(false);
        s.cancel_btn.set_visible(false);
        s.buttons.set_visible(true);
        s.open_btn.set_visible(outputs.len() == 1);
        s.show_btn.set_visible(!outputs.is_empty());
        if !errors.is_empty() {
            let done = if outputs.is_empty() { String::new() } else { format!("{} done, ", plural(outputs.len(), "file")) };
            s.title.set_text(&format!("{done}{}", plural(errors.len(), "problem")));
            s.title.add_css_class("error");
            s.detail.set_text(&errors[..errors.len().min(4)].join("\n"));
            s.detail.set_visible(true);
        } else {
            s.title.set_text(&format!("✓ {} ready", plural(outputs.len(), "file")));
            let fallback = fallback_dir();
            let where_ = if outputs.iter().any(|p| p.parent() == Some(fallback.as_path())) {
                format!(" (in ~/{}: the original's folder is read-only)", name(&fallback))
            } else {
                String::new()
            };
            let names: Vec<String> = outputs.iter().take(3).map(|p| name(p)).collect();
            let more = if outputs.len() > 3 { " …" } else { "" };
            s.detail.set_text(&format!("{}{more}{where_}", names.join(", ")));
            s.detail.set_visible(true);
            self.linger(SUCCESS_LINGER);
        }
        *s.outputs.borrow_mut() = outputs;
        s.window.present();
    }

    /// Hide the pop-up after `delay`, unless the pointer is on it by then.
    fn linger(self: &Rc<Self>, delay: Duration) {
        let app = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(delay, move || {
            let Some(app) = app.upgrade() else { return };
            app.status.linger.borrow_mut().take();
            if !app.status.hovered.get() {
                app.status.window.set_visible(false);
            }
        });
        *self.status.linger.borrow_mut() = Some(id);
    }

    fn on_status_leave(self: &Rc<Self>) {
        let s = &self.status;
        s.hovered.set(false);
        if s.linger.borrow().is_none() && !s.bar.is_visible() && !s.title.has_css_class("error") {
            self.linger(Duration::from_millis(1500));
        }
    }

    fn cancel_linger(&self) {
        if let Some(id) = self.status.linger.borrow_mut().take() {
            id.remove();
        }
    }

    fn open(&self) {
        let outputs = self.status.outputs.borrow();
        if let Some(first) = outputs.first() {
            let uri = gio::File::for_path(first).uri();
            if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
                debug(&format!("open {uri} failed: {e}"));
            }
            self.status.window.set_visible(false);
        }
    }

    fn show_in_files(&self) {
        let uris: Vec<String> = self.status.outputs.borrow().iter().map(|p| gio::File::for_path(p).uri().into()).collect();
        let proxy = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session, gio::DBusProxyFlags::NONE, None, "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1", None::<&gio::Cancellable>,
        );
        match proxy {
            Ok(proxy) => proxy.call("ShowItems", Some(&(uris, "").to_variant()), gio::DBusCallFlags::NONE, -1,
                                    None::<&gio::Cancellable>, |_| {}),
            Err(e) => debug(&format!("Show in Files failed: {e}")),
        }
        self.status.window.set_visible(false);
    }
}

impl Status {
    fn new(gapp: &gtk::Application) -> Status {
        let window = gtk::Window::builder().application(gapp).title(STATUS_TITLE)
            .decorated(false).resizable(false).build();
        window.add_css_class("ot-status");
        window.set_default_size(380, -1);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8)
            .margin_top(14).margin_bottom(14).margin_start(18).margin_end(18).build();
        let title = gtk::Label::builder().xalign(0.0).css_classes(["title"]).build();
        let detail = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(48).css_classes(["dim"]).build();
        let bar = gtk::ProgressBar::new();
        let cancel_btn = gtk::Button::builder().label("Cancel").halign(gtk::Align::End).build();
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let open_btn = gtk::Button::with_label("Open");
        let show_btn = gtk::Button::with_label("Show in Files");
        let close = gtk::Button::with_label("Close");
        let w = window.clone();
        close.connect_clicked(move |_| w.set_visible(false));
        for b in [&open_btn, &show_btn, &close] {
            buttons.append(b);
        }
        content.append(&title);
        content.append(&detail);
        content.append(&bar);
        content.append(&cancel_btn);
        content.append(&buttons);
        window.set_child(Some(&content));
        Status { window, title, detail, bar, cancel_btn, open_btn, show_btn, buttons,
                 outputs: RefCell::default(), hovered: Cell::new(false), linger: RefCell::default() }
    }
}

/// Plan and run a batch on the work thread. Returns (outputs, error lines) for the pop-up.
fn work(files: Vec<PathBuf>, choice: Choice, params: Params, cancel: &AtomicBool, unreadable: Vec<String>,
        send: &async_channel::Sender<Update>) -> (Vec<PathBuf>, Vec<String>) {
    let planned = match choice {
        Choice::Convert(fmt) => plan_convert(&files, fmt),
        Choice::Tool(tool) => plan_tool_for(tool, &files, &params),
    };
    let jobs = match planned {
        Ok(jobs) => jobs,
        Err(e) => return (vec![], vec![e]),
    };
    let total = jobs.len();
    let progress = |done: usize, fraction: f64| {
        let _ = send.send_blocking(Update::Progress { done: done.min(total.saturating_sub(1)), total, fraction });
    };
    let finished = |job: &shiftdrop::convert::Job, error: Option<&str>| {
        let how = if job.used_fallback { " (GPU failed, used CPU)" } else { "" };
        let result = match error {
            Some(e) => format!("failed: {e}"),
            None => format!("ok{how} -> {:?}", job.outputs.iter().map(|p| name(p)).collect::<Vec<_>>()),
        };
        debug(&format!("{} {}: {result}", job.title, name(&job.sources[0])));
    };

    let started = Instant::now();
    let (outputs, mut errors) = run_batch(jobs, &progress, &finished, cancel);
    debug(&format!("batch of {total} took {:.2}s", started.elapsed().as_secs_f64()));
    // the drop was a copy, so every original must still be there
    errors.extend(files.iter().filter(|f| !f.exists())
        .map(|f| format!("{}: the original is gone. Check the folder you dragged from.", name(f))));
    if cancel.load(Ordering::Relaxed) {
        errors.retain(|e| !e.ends_with(": Cancelled"));
        errors.push("Cancelled".into());
    }
    if !unreadable.is_empty() {
        errors.push(unreadable_line(&unreadable));
    }
    (outputs, errors)
}

fn plan_tool_for(tool: &Tool, files: &[PathBuf], params: &Params) -> Result<Vec<shiftdrop::convert::Job>, String> {
    let pairs: Vec<(PathBuf, Fmt)> = files.iter()
        .map(|f| detect(f).map(|fmt| (f.clone(), fmt)).ok_or_else(|| format!("{}: unknown file type", name(f))))
        .collect::<Result<_, _>>()?;
    let is = |fmt: Fmt, category| fmt.category() == category;
    let media: HashMap<PathBuf, _> = pairs.iter()
        .filter(|(_, fmt)| is(*fmt, Category::Video) || is(*fmt, Category::Audio))
        .map(|(f, _)| (f.clone(), probe(f)))
        .collect();
    let gpu = tool.id == "trim" && pairs.iter().any(|(_, fmt)| is(*fmt, Category::Video)) && gpu_h264();
    plan_tool(tool.id, &pairs, params, &media, gpu)
}
