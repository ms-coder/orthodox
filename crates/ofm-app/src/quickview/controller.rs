//! The quick-view controller: switching, following the source panel's cursor (debounced),
//! cancelling stale work and turning worker results into pane content.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::rc::{Rc, Weak};
use std::time::Instant;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CancelToken, Location};
use ofm_jobs::event::SizeTotals;

use super::DEBOUNCE;
use super::debounce::{Debouncer, Tick};
use super::pane::{Body, Content, PreviewPane};
use super::worker::{self, ImageData, Msg, Preview, Target, TargetKey, Update, ViewBox};
use crate::chrome::statusline::{SizeFormat, StatusFormat, cursor_info, format_size};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::panel::{LoadState, PanelController, PanelHost, Side};
use crate::signal::SignalId;
use crate::window::TextHold;
use crate::wiring;

/// Signal connections made while quick view is on (all removed by [`QuickView::off`]).
#[derive(Default)]
struct Conns {
    source: Option<(Weak<PanelController>, SignalId)>,
    host: Option<(Rc<PanelHost>, SignalId)>,
    active: Option<SignalId>,
}

/// What is currently shown or being produced.
struct Current {
    key: Option<TargetKey>,
    cancel: CancelToken,
}

/// Quick-view state of the window (stored in `ctx.ext`).
pub struct QuickView {
    ctx: Weak<AppContext>,
    panes: RefCell<[Option<PreviewPane>; 2]>,
    /// Path header / status line of each side (shown as "Quick view" + the item's info).
    holds: RefCell<[Option<TextHold>; 2]>,
    shown_on: Cell<Option<Side>>,
    conns: RefCell<Conns>,
    debounce: RefCell<Debouncer<()>>,
    timer_armed: Cell<bool>,
    generation: Cell<u64>,
    current: RefCell<Option<Current>>,
}

impl QuickView {
    /// Controller for `ctx` (no panes yet: [`register_pane`](Self::register_pane)).
    pub fn new(ctx: &Rc<AppContext>) -> Rc<QuickView> {
        Rc::new(QuickView {
            ctx: Rc::downgrade(ctx),
            panes: RefCell::new([None, None]),
            holds: RefCell::new([None, None]),
            shown_on: Cell::new(None),
            conns: RefCell::new(Conns::default()),
            debounce: RefCell::new(Debouncer::new(DEBOUNCE)),
            timer_armed: Cell::new(false),
            generation: Cell::new(0),
            current: RefCell::new(None),
        })
    }

    /// Remember the pane laid over `side`'s file list.
    pub fn register_pane(&self, side: Side, pane: PreviewPane) {
        self.panes.borrow_mut()[side.min(1)] = Some(pane);
    }

    /// Remember the text hold of `side`'s path header and status line.
    pub fn register_hold(&self, side: Side, hold: TextHold) {
        self.holds.borrow_mut()[side.min(1)] = Some(hold);
    }

    fn hold(&self, side: Side) -> Option<TextHold> {
        self.holds.borrow()[side.min(1)].clone()
    }

    /// Whether quick view is on.
    pub fn is_on(&self) -> bool {
        self.shown_on.get().is_some()
    }

    /// The side showing the preview, if on.
    pub fn shown_side(&self) -> Option<Side> {
        self.shown_on.get()
    }

    /// What the pane of `side` currently shows (tests, diagnostics).
    pub fn pane_content(&self, side: Side) -> Option<Content> {
        self.pane(side).map(|p| p.content())
    }

    fn pane(&self, side: Side) -> Option<PreviewPane> {
        self.panes.borrow()[side.min(1)].clone()
    }

    /// `panel.toggle_quickview`.
    pub fn toggle(self: &Rc<Self>, ctx: &Rc<AppContext>) {
        if self.is_on() {
            self.off();
        } else {
            self.on(ctx);
        }
    }

    fn on(self: &Rc<Self>, ctx: &Rc<AppContext>) {
        let side = ctx.other_side();
        let Some(pane) = self.pane(side) else {
            tracing::debug!("quickview: no pane installed");
            return;
        };
        self.shown_on.set(Some(side));
        if let Some(hold) = self.hold(side) {
            hold.hold(tr("Quick view"), "");
        }
        pane.set_visible(true);
        let source = 1 - side;
        let host = Rc::clone(ctx.host(source));
        self.bind_source(host.active());
        let me = Rc::downgrade(self);
        let host_conn = host.active_changed.connect(move |ctrl| {
            if let Some(me) = me.upgrade() {
                me.bind_source(Rc::clone(ctrl));
                me.schedule();
            }
        });
        // The preview side is not a place to put the cursor: bounce back to the file panel.
        let (me, weak_ctx) = (Rc::downgrade(self), Rc::downgrade(ctx));
        let active_conn = ctx.active_changed.connect(move |s| {
            if let (Some(me), Some(ctx)) = (me.upgrade(), weak_ctx.upgrade())
                && me.shown_on.get() == Some(*s)
            {
                ctx.set_active_side(1 - *s);
            }
        });
        {
            let mut c = self.conns.borrow_mut();
            c.host = Some((host, host_conn));
            c.active = Some(active_conn);
        }
        self.refresh();
    }

    fn off(&self) {
        let Some(side) = self.shown_on.take() else {
            return;
        };
        self.cancel_current();
        self.generation.set(self.generation.get() + 1);
        self.debounce.borrow_mut().cancel();
        let conns = std::mem::take(&mut *self.conns.borrow_mut());
        if let Some((weak, id)) = conns.source
            && let Some(ctrl) = weak.upgrade()
        {
            ctrl.changed.disconnect(id);
        }
        if let Some((host, id)) = conns.host {
            host.active_changed.disconnect(id);
        }
        if let (Some(id), Some(ctx)) = (conns.active, self.ctx.upgrade()) {
            ctx.active_changed.disconnect(id);
        }
        if let Some(pane) = self.pane(side) {
            pane.set_visible(false);
            pane.set_content(Content::default());
        }
        if let Some(hold) = self.hold(side) {
            hold.release();
        }
    }

    /// Follow `ctrl` (the source panel's active tab) instead of the previous tab.
    fn bind_source(self: &Rc<Self>, ctrl: Rc<PanelController>) {
        let old = self.conns.borrow_mut().source.take();
        if let Some((weak, id)) = old
            && let Some(prev) = weak.upgrade()
        {
            prev.changed.disconnect(id);
        }
        let me = Rc::downgrade(self);
        let id = ctrl.changed.connect(move |_| {
            if let Some(me) = me.upgrade() {
                me.schedule();
            }
        });
        self.conns.borrow_mut().source = Some((Rc::downgrade(&ctrl), id));
    }

    // ---- debounce -----------------------------------------------------------------------------

    /// The source panel changed: re-target after the quiet period.
    fn schedule(self: &Rc<Self>) {
        if !self.is_on() {
            return;
        }
        self.debounce.borrow_mut().push(Instant::now(), ());
        if !self.timer_armed.replace(true) {
            self.arm(DEBOUNCE);
        }
    }

    fn arm(self: &Rc<Self>, after: std::time::Duration) {
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(after, move || {
            if let Some(me) = me.upgrade() {
                me.tick();
            }
        });
    }

    fn tick(self: &Rc<Self>) {
        let tick = self.debounce.borrow_mut().tick(Instant::now());
        match tick {
            Tick::Fire(()) => {
                self.timer_armed.set(false);
                self.refresh();
            }
            Tick::Wait(d) => self.arm(d),
            Tick::Idle => self.timer_armed.set(false),
        }
    }

    // ---- starting a preview ---------------------------------------------------------------------

    fn cancel_current(&self) {
        if let Some(cur) = self.current.borrow_mut().take() {
            cur.cancel.cancel();
        }
    }

    fn show_message(&self, pane: &PreviewPane, text: &str) {
        self.cancel_current();
        self.generation.set(self.generation.get() + 1);
        pane.set_content(Content {
            title: String::new(),
            subtitle: String::new(),
            body: Body::Message(text.to_owned()),
        });
    }

    /// Look at the cursor of the source panel and start the matching preview (unless it is
    /// already shown or running).
    fn refresh(self: &Rc<Self>) {
        let (Some(ctx), Some(side)) = (self.ctx.upgrade(), self.shown_on.get()) else {
            return;
        };
        let Some(pane) = self.pane(side) else {
            return;
        };
        let source = ctx.host(1 - side).active();
        let (entry, dir, state) = {
            let m = source.model();
            (m.cursor_entry().cloned(), source.location(), source.state())
        };
        // The preview side's status line describes the previewed item (as the source panel's
        // status line does).
        if let Some(hold) = self.hold(side) {
            let fmt = {
                let cfg = ctx.config.borrow();
                StatusFormat {
                    size: SizeFormat::from_config(&cfg.panels.size_format),
                    time: cfg.panels.time_format.clone(),
                }
            };
            hold.set_status(&entry.as_ref().map_or_else(String::new, |e| cursor_info(e, &fmt)));
        }
        let Some(entry) = entry else {
            let text = match state {
                LoadState::Loading => tr("Loading..."),
                _ => tr("No file under the cursor"),
            };
            self.show_message(&pane, text);
            *self.current.borrow_mut() = Some(Current {
                key: None,
                cancel: CancelToken::new(),
            });
            return;
        };
        let path = match (&entry.real_path, dir.join(&entry.name)) {
            (Some(p), _) => p.clone(),
            (None, Location::Local(p)) => p,
            (None, _) => {
                self.show_message(&pane, tr("No preview for the contents of an archive"));
                return;
            }
        };
        let target = Target {
            path,
            name: entry.name.clone(),
            kind: entry.kind,
            size: entry.size,
            mtime_ns: entry.mtime_ns,
            mode: entry.mode,
        };
        let key = target.key();
        if self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|c| c.key.as_ref() == Some(&key))
        {
            return; // same item (selection / watcher churn): nothing to do
        }
        self.cancel_current();
        let cancel = CancelToken::new();
        *self.current.borrow_mut() = Some(Current {
            key: Some(key),
            cancel: cancel.clone(),
        });
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        let fmt = {
            let cfg = ctx.config.borrow();
            StatusFormat {
                size: SizeFormat::from_config(&cfg.panels.size_format),
                time: cfg.panels.time_format.clone(),
            }
        };
        let (width, height, rows) = pane.view_box();
        let view = ViewBox {
            width,
            height,
            rows,
        };
        let title = target.name.to_string_lossy().into_owned();
        let job = Rc::new(Job {
            target: target.clone(),
            fmt: fmt.clone(),
            title,
        });

        if target.is_dir() && target.name != ".." {
            pane.set_content(dir_content(
                &job.target,
                &job.fmt,
                &job.title,
                DirState::Counting { files: 0, bytes: 0 },
            ));
            let (tx, rx) = async_channel::unbounded();
            let path = target.path.clone();
            let spawned = std::thread::Builder::new()
                .name("ofm-quickview-size".into())
                .spawn(move || worker::dir_size(&path, &cancel, generation, &tx));
            if let Err(e) = spawned {
                tracing::error!("quickview: cannot start the size walk: {e}");
                return;
            }
            self.attach(rx, job);
        } else {
            let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
                let preview = worker::build(&target, view, &fmt, &cancel);
                if cancel.is_cancelled() {
                    return;
                }
                let _ = tx.send_blocking(Msg {
                    generation,
                    update: Update::Ready(preview),
                });
            });
            self.attach(rx, job);
        }
    }

    /// Consume worker messages on the main loop; stale generations end the loop (which drops
    /// the receiver and so stops a still-running worker at its next send).
    fn attach(self: &Rc<Self>, rx: async_channel::Receiver<Msg>, job: Rc<Job>) {
        let me = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |msg| {
                let Some(me) = me.upgrade() else {
                    return ControlFlow::Break(());
                };
                if msg.generation != me.generation.get() {
                    return ControlFlow::Break(());
                }
                let Some(pane) = me.shown_on.get().and_then(|s| me.pane(s)) else {
                    return ControlFlow::Break(());
                };
                let last = !matches!(msg.update, Update::DirProgress { .. });
                pane.set_content(content_for(msg.update, &job));
                if last {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            || {},
        );
    }
}

/// Everything the receiving side needs to know about one request.
struct Job {
    target: Target,
    fmt: StatusFormat,
    title: String,
}

// ---- content construction (pure apart from texture creation) --------------------------------------

/// Progress of a directory summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirState {
    /// Still walking: running totals.
    Counting {
        /// Files seen.
        files: u64,
        /// Their bytes.
        bytes: u64,
    },
    /// Finished.
    Done(SizeTotals),
    /// The walk failed.
    Failed(String),
}

/// The information table of a directory in `state`.
pub fn dir_content(t: &Target, fmt: &StatusFormat, title: &str, state: DirState) -> Content {
    let mut rows = worker::info_rows(t, tr("Folder").to_owned(), fmt);
    let size = |b: u64| format_size(b, fmt.size);
    let mut note = None;
    let subtitle;
    match state {
        DirState::Counting { files, bytes } => {
            subtitle = tr("counting...").to_owned();
            rows.push(worker::Row {
                label: tr("Files").to_owned(),
                value: files.to_string(),
            });
            rows.push(worker::Row {
                label: tr("Size").to_owned(),
                value: size(bytes),
            });
        }
        DirState::Done(totals) => {
            subtitle = String::new();
            rows.push(worker::Row {
                label: tr("Files").to_owned(),
                value: totals.files.to_string(),
            });
            rows.push(worker::Row {
                label: tr("Folders").to_owned(),
                value: totals.dirs.saturating_sub(1).to_string(),
            });
            rows.push(worker::Row {
                label: tr("Size").to_owned(),
                value: worker::size_exact(totals.bytes, fmt.size),
            });
            rows.push(worker::Row {
                label: tr("On disk").to_owned(),
                value: size(totals.disk_bytes),
            });
        }
        DirState::Failed(msg) => {
            subtitle = String::new();
            note = Some(msg);
        }
    }
    Content {
        title: title.to_owned(),
        subtitle,
        body: Body::Info { rows, note },
    }
}

/// The pane content of a text preview.
pub fn text_content(title: &str, lines: Vec<String>, encoding: &str, complete: bool) -> Content {
    let subtitle = if complete {
        encoding.to_owned()
    } else {
        format!("{encoding} \u{b7} {}", tr("first lines"))
    };
    Content {
        title: title.to_owned(),
        subtitle,
        body: Body::Text(lines),
    }
}

fn texture(img: &ImageData) -> gdk::Texture {
    let format = if img.alpha {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    gdk::MemoryTexture::new(img.width, img.height, format, &img.bytes, img.stride).upcast()
}

fn content_for(update: Update, job: &Job) -> Content {
    let title = job.title.as_str();
    match update {
        Update::Ready(Preview::Text {
            lines,
            encoding,
            complete,
        }) => text_content(title, lines, encoding, complete),
        Update::Ready(Preview::Image(img)) => Content {
            title: title.to_owned(),
            subtitle: String::new(),
            body: Body::Image {
                texture: texture(&img),
                caption: img.caption,
            },
        },
        Update::Ready(Preview::Info { rows, note }) => Content {
            title: title.to_owned(),
            subtitle: String::new(),
            body: Body::Info { rows, note },
        },
        Update::Ready(Preview::Message(text)) => Content {
            title: title.to_owned(),
            subtitle: String::new(),
            body: Body::Message(text),
        },
        Update::DirProgress { files, bytes } => dir_content(
            &job.target,
            &job.fmt,
            title,
            DirState::Counting { files, bytes },
        ),
        Update::DirDone(totals) => {
            dir_content(&job.target, &job.fmt, title, DirState::Done(totals))
        }
        Update::DirFailed(msg) => dir_content(&job.target, &job.fmt, title, DirState::Failed(msg)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_core::EntryKind;

    use super::*;

    fn dir_target() -> Target {
        Target {
            path: "/d".into(),
            name: "d".into(),
            kind: EntryKind::Dir,
            size: 0,
            mtime_ns: 0,
            mode: 0o040_755,
        }
    }

    fn rows(c: &Content) -> Vec<(String, String)> {
        match &c.body {
            Body::Info { rows, .. } => rows
                .iter()
                .map(|r| (r.label.clone(), r.value.clone()))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn counting_shows_running_totals() {
        let c = dir_content(
            &dir_target(),
            &StatusFormat::default(),
            "d",
            DirState::Counting {
                files: 12,
                bytes: 2048,
            },
        );
        assert_eq!(c.subtitle, "counting...");
        let r = rows(&c);
        assert!(r.contains(&("Files".into(), "12".into())), "{r:?}");
        assert!(r.contains(&("Size".into(), "2.0 KiB".into())), "{r:?}");
    }

    #[test]
    fn done_excludes_the_root_from_the_folder_count() {
        let c = dir_content(
            &dir_target(),
            &StatusFormat::default(),
            "d",
            DirState::Done(SizeTotals {
                bytes: 1024,
                disk_bytes: 8192,
                files: 2,
                dirs: 3,
            }),
        );
        assert!(c.subtitle.is_empty());
        let r = rows(&c);
        assert!(r.contains(&("Folders".into(), "2".into())), "{r:?}");
        assert!(r.contains(&("On disk".into(), "8.0 KiB".into())), "{r:?}");
    }

    #[test]
    fn failed_walk_shows_the_message() {
        let c = dir_content(
            &dir_target(),
            &StatusFormat::default(),
            "d",
            DirState::Failed("boom".into()),
        );
        assert!(matches!(&c.body, Body::Info { note: Some(n), .. } if n == "boom"));
    }

    #[test]
    fn text_subtitle_mentions_truncation() {
        let c = text_content("a.txt", vec![], "utf-8", true);
        assert_eq!(c.subtitle, "utf-8");
        let c = text_content("a.txt", vec![], "utf-8", false);
        assert_eq!(c.subtitle, "utf-8 \u{b7} first lines");
    }
}
