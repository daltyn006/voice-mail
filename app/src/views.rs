//! Shell: top nav (Input / Output / Models) + page body. Single-window design:
//! processing progress and controls live on the Input page after Start.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, rgba, Animation, AnimationExt, Context, Entity, IntoElement, Render, Window,
};

use crate::store::{Page, Store};
use crate::theme;
use crate::views_input::InputView;
use crate::views_models::ModelsView;
use crate::views_output::OutputView;
use crate::views_record::RecordView;
use crate::views_settings::SettingsView;
use crate::views_wizard::WizardView;
use crate::{GotoInput, GotoOutput, PauseProc, Quit, ToggleTheme};

pub struct RootView {
    store: Entity<Store>,
    input: Entity<InputView>,
    record: Entity<RecordView>,
    output: Entity<OutputView>,
    models: Entity<ModelsView>,
    settings: Entity<SettingsView>,
    wizard: Entity<WizardView>,
}

impl RootView {
    /// Assembled by `main` (which owns the window borrow); see module docs.
    pub fn assemble(
        store: Entity<Store>,
        input: Entity<InputView>,
        record: Entity<RecordView>,
        output: Entity<OutputView>,
        models: Entity<ModelsView>,
        settings: Entity<SettingsView>,
        wizard: Entity<WizardView>,
    ) -> Self {
        RootView {
            store,
            input,
            record,
            output,
            models,
            settings,
            wizard,
        }
    }

    /// Page order shared by tabs, sidebar, and shortcuts.
    fn nav_pages() -> [(Page, &'static str, IconName); 5] {
        [
            (Page::Input, "Input", IconName::FilePlus),
            (Page::Record, "Record", IconName::Mic),
            (Page::Output, "Output", IconName::FileText),
            (Page::Models, "Models", IconName::Layers),
            (Page::Settings, "Settings", IconName::Settings),
        ]
    }

    fn nav_index(page: Page) -> usize {
        Self::nav_pages()
            .iter()
            .position(|(p, _, _)| *p == page)
            .unwrap_or(0)
    }

    /// Browser-style underline tabs (top). Handlers get `&mut App`, so they
    /// go through `update_entity` (notifies observers) instead of `update`.
    fn tabs(&self, page: Page, store: Entity<Store>) -> impl IntoElement {
        let mut bar = TabBar::new("nav")
            .underline()
            .selected_index(Self::nav_index(page));
        for (_, label, _) in Self::nav_pages() {
            bar = bar.child(Tab::new().label(label));
        }
        let pages = Self::nav_pages().map(|(p, _, _)| p);
        bar.on_click(move |i: &usize, _: &mut Window, cx: &mut gpui_kit::App| {
            if let Some(page) = pages.get(*i) {
                let page = *page;
                cx.update_entity(&store, |s, _| {
                    // goto_page auto-stages unsent Record takes to Input.
                    s.goto_page(page);
                });
            }
        })
    }

    /// Left icon menu (fixed 200px). Same behavior contract as tabs.
    fn sidebar(&self, page: Page, store: Entity<Store>) -> impl IntoElement {
        let mut menu = SidebarMenu::new();
        for (p, label, icon) in Self::nav_pages() {
            let active = p == page;
            let store = store.clone();
            menu = menu.child(
                SidebarMenuItem::new(label)
                    .icon(icon)
                    .active(active)
                    .on_click(move |_, _: &mut Window, cx: &mut gpui_kit::App| {
                        cx.update_entity(&store, |s, _| {
                            s.goto_page(p);
                        });
                    }),
            );
        }
        div()
            .flex_shrink_0()
            .h_full()
            .child(Sidebar::new("nav-side").child(menu))
    }

    /// Bounded body slot shared by both layouts: every page gets the
    /// leftover viewport height. Scrolling lives ONLY in the page-level
    /// containers (see [`page_scroll_ids`]): a single scroller
    /// per page, so wheel events always reach the element that can move.
    /// (A scrollable wrapper here would sit under the cursor and eat the
    /// wheel while having nothing to scroll.) `min_w(0)` keeps narrow
    /// windows to a single column; both scroll axes live on the page
    /// scroller itself (never clip here — clipped content with no scroller
    /// above it is unreachable).
    fn body_slot(body: gpui_kit::AnyElement) -> impl IntoElement {
        // Flex all the way down: every level is a flex item with
        // `flex_1() + min_h(0)`, never a `%` height. `%` heights only
        // resolve against flex-determined sizes — a `%` child of a `%`-sized
        // block parent collapses to content height (the window then clips it
        // with no scroller ever engaging). The outer slot is a flex item of
        // the root column AND a flex container, so the inner box gets a
        // definite height to scroll inside of. `flex_1()` alone (basis 0)
        // is right; a trailing `flex_auto()` would reset it to basis `auto`.
        div()
            .flex_1()
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .child(
                div()
                    .max_w(px(1200.0))
                    .mx_auto()
                    .w_full()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
                    .p_4()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(body),
            )
    }

    fn goto(&mut self, page: Page, cx: &mut Context<Self>) {
        self.store.update(cx, |st, cx| {
            st.goto_page(page);
            cx.notify();
        });
    }

    // Window-level shortcuts live on the root element (which outlives every
    // frame). NOTE: Context/Window-level on_action registration outside
    // paint panics (window.rs debug_assert_paint) — never move these there.
    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        // Stop the worker at a safe point first; files stay queued.
        // Mirrors install_quit_hook: single shutdown_prepare covers drafts,
        // prefs, downloads, and the backend abort — always ends in exit(0)
        // to skip DLL teardown while a worker may run inside it.
        crate::shutdown_trace("alt-q quit action fired");
        self.store.update(cx, |st, _| {
            st.shutdown_prepare();
        });
        crate::shutdown_trace("alt-q backend aborted, exiting");
        std::process::exit(0);
    }

    fn on_goto_input(&mut self, _: &GotoInput, _: &mut Window, cx: &mut Context<Self>) {
        self.goto(Page::Input, cx);
    }

    fn on_goto_output(&mut self, _: &GotoOutput, _: &mut Window, cx: &mut Context<Self>) {
        self.goto(Page::Output, cx);
    }

    fn on_pause(&mut self, _: &PauseProc, _: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |st, cx| {
            st.set_paused(!st.paused);
            cx.notify();
        });
    }

    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        let (theme_mode, high_contrast) = self.store.update(cx, |st, _| st.cycle_theme());
        crate::theme::apply_theme(&theme_mode, high_contrast, cx);
        self.store.update(cx, |_, cx| cx.notify());
    }
}

/// Bottom anchor for scroll regression tests (headless + manual): proves a
/// page actually scrolls by moving into view on wheel input. Negligible
/// visual footprint (1px); a single surface renders at a time so the id
/// stays unique across the window (duplicate GPUI element ids panic).
/// Observed for headless queries: a no-op identity wrapper in normal
/// builds, a queryable node under `test-support`.
pub(crate) fn page_bottom_marker() -> impl IntoElement {
    div().id("page-bottom").h(px(1.0)).test_support()
}

/// Scroll-container registry: every full-screen surface owns exactly one
/// page-level scroll box `(page, scroll element id)`. A surface missing
/// here is unreachable on small windows — add the scroller AND extend this
/// list (the test below + `tests/smoke.mjs` enforce both directions: the
/// registry lists exactly the ids present in the view sources).
/// Test-only: production code addresses scrollers by literal id at each
/// build site; this is the checklist, not a lookup.
#[cfg(test)]
pub fn page_scroll_ids() -> [(&'static str, &'static str); 7] {
    [
        ("input", "input-scroll"),
        ("record", "record-scroll"),
        ("output", "output-scroll"),
        ("models", "models-scroll"),
        ("settings", "settings-scroll"),
        ("wizard", "wizard-scroll"),
        ("review", "review-scroll"),
    ]
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // First-run wizard takes the whole window while open (never a trap:
        // every wizard state offers import / retry / continue-without).
        // Absolute fill, NOT a `%` height: the wizard renders directly under
        // the kit root (a block box), where a `%` child collapses to content
        // height and no scroller ever engages (same trap as the page slot —
        // see `body_slot`). `inset_0` against the relatively-positioned kit
        // root gives a definite height; the flex column then bounds the
        // wizard's own scroll box below.
        let wizard_open = self.store.read(cx).wizard_open;
        if wizard_open {
            return div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .min_h(px(0.0))
                .child(self.wizard.clone().into_any_element())
                .into_any_element();
        }
        let (page, theme_id, high_contrast, reduce_motion, nav_seq, sidebar_mode) = {
            let snap = self.store.read(cx);
            (
                snap.page,
                snap.theme_mode.clone(),
                snap.high_contrast,
                snap.reduce_motion,
                snap.nav_seq,
                snap.nav_mode == "sidebar",
            )
        };

        let body: gpui_kit::AnyElement = match page {
            Page::Input => self.input.clone().into_any_element(),
            Page::Record => self.record.clone().into_any_element(),
            Page::Output => self.output.clone().into_any_element(),
            Page::Models => self.models.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        };

        // Page-enter fade: opacity-only by design. A positional offset
        // (`left`/`margin`) inside the animator forces a full relayout of
        // the whole page tree every frame and stack-overflows debug builds
        // (bisected 2026-09-17: `.left()` = instant SO, opacity = clean).
        // The animation id carries the nav generation so every navigation
        // replays it; reduce-motion (app pref or OS) renders the end state
        // instantly.
        let body: gpui_kit::AnyElement = if reduce_motion {
            body
        } else {
            // Sized by flex (see `body_slot`): this wrapper is a flex item
            // of the slot, so `flex_1() + min_h(0)` gives it a definite
            // height; a `%` height here would collapse to content height.
            // `min_h(0)`/`min_w(0)` are load-bearing too: without them
            // this wrapper sizes to its content, the inner scroller never
            // overflows, and no scrollbar appears on small windows.
            div()
                .w_full()
                .flex_1()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .min_h(px(0.0))
                .child(body)
                .with_animation(
                    ("page-swipe", nav_seq),
                    Animation::new(std::time::Duration::from_millis(180))
                        .with_easing(gpui_kit::ease_out_quint()),
                    // Opacity-only: positional offsets force a full relayout
                    // of the whole page tree every frame (see SO bisect).
                    move |el, delta| el.opacity(delta),
                )
                .into_any_element()
        };

        let mut root = div()
            .id("root")
            .flex()
            .size_full()
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_goto_input))
            .on_action(cx.listener(Self::on_goto_output))
            .on_action(cx.listener(Self::on_pause))
            .on_action(cx.listener(Self::on_toggle_theme));
        let store = self.store.clone();
        if sidebar_mode {
            // Left menu + full-height body column. The menu owns no scroll;
            // the page body scrolls exactly as in tabs mode.
            root = root
                .flex_row()
                .gap_2()
                .child(self.sidebar(page, store))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .min_h(px(0.0))
                        .child(Self::body_slot(body)),
                );
        } else {
            root = root
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .p_2()
                        .bg(rgba(theme::banner(&theme_id, high_contrast)))
                        .child(self.tabs(page, store)),
                )
                .child(Self::body_slot(body));
        }
        root.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use std::collections::HashSet;

    /// Serializes the headless scroll tests with each other (the
    /// models-dir override below is process-global) — one static shared
    /// by both tests, never a per-test lock.
    static SCROLL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Every full-screen surface owns exactly one scroll box: no
    /// unreachable screens (the small-window Settings trap), no duplicate
    /// scroller ids (duplicate GPUI element ids panic the a11y tree).
    #[test]
    fn every_surface_owns_exactly_one_scroll_box() {
        let ids = page_scroll_ids();
        assert_eq!(ids.len(), 7, "add the scroller AND extend the registry");
        let pages: HashSet<_> = ids.iter().map(|(p, _)| *p).collect();
        let scrolls: HashSet<_> = ids.iter().map(|(_, s)| *s).collect();
        assert_eq!(pages.len(), ids.len(), "duplicate page in registry");
        assert_eq!(scrolls.len(), ids.len(), "duplicate scroll id in registry");
        for (page, scroll) in ids {
            assert!(!page.is_empty() && !scroll.is_empty());
            assert!(
                scroll.ends_with("-scroll"),
                "{page}: scroll id {scroll} must end in -scroll"
            );
        }
    }

    /// Headless scroll regression (2026-10: vertical scroll dead on every
    /// page — scrollers grew unbounded with their content, the window
    /// clipped them, no bar painted, wheel did nothing). Renders the real
    /// RootView in headless windows down to 200x200 and asserts per page:
    /// (1) the scroller is bounded by the viewport height, (2) when content
    /// overflows, wheel input moves it, (3) repeated wheels reach the
    /// bottom marker (fully renderable, nothing unreachable).
    /// Review needs live editor widgets (built in click handlers, never in
    /// render/tests), so it carries the marker for manual checks but has no
    /// headless case here; every other surface in `page_scroll_ids` is
    /// covered (wizard in its own test below).
    #[gpui_kit::test]
    fn pages_scroll_vertically_at_small_windows(cx: &mut TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{
            point, size, AnyWindowHandle, App, InputEvent, MouseMoveEvent, ScrollDelta,
            ScrollWheelEvent,
        };
        use std::path::PathBuf;

        use crate::store::{FileState, InputFile, InputKind, OutFile};

        // Serial: the models-dir override below is process-global, and
        // Store construction reads the live prefs file — share the store
        // tests' locks so manifest/prefs-guarded tests never interleave.
        // Order (PREFS before MANIFEST) matches the store suite convention.
        let _scroll = SCROLL_LOCK.lock().unwrap();
        let _plock = crate::store::tests::PREFS_LOCK.lock().unwrap();
        let _mlock = crate::store::tests::MANIFEST_LOCK.lock().unwrap();

        // Deterministic models dir (ambient downloads only add rows).
        let dir = std::env::temp_dir().join(format!("vm-scroll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        pv_backend::dirs::set_models_dir_override(dir.clone());

        let pages: [(Page, &str); 5] = [
            (Page::Input, "input-scroll"),
            (Page::Record, "record-scroll"),
            (Page::Output, "output-scroll"),
            (Page::Models, "models-scroll"),
            (Page::Settings, "settings-scroll"),
        ];
        // 200x200 is the extreme small-screen case (below the prod
        // 240px window minimum — headless only); sidebar needs width for
        // its fixed 200px menu, so it is tabs-only there.
        let cases: [(f32, f32, &str); 5] = [
            (200., 200., "tabs"),
            (800., 600., "tabs"),
            (800., 600., "sidebar"),
            (1100., 750., "tabs"),
            (1100., 750., "sidebar"),
        ];

        cx.update(gpui_kit::init);
        cx.update(|cx| crate::theme::apply_theme("dark", false, cx));

        for (page, scroll_id) in pages {
            for (w, h, nav) in cases {
                let handle = cx.open_window(size(px(w), px(h)), |window, cx| {
                    let store = cx.new(|_| Store::new());
                        store.update(cx, |s, _| {
                            s.wizard_open = false;
                            // Direct field write (not set_nav_mode): the
                            // setter persists prefs to disk — racy under
                            // parallel tests and pollutes ambient state.
                            s.nav_mode = crate::store::normalize_nav_mode(nav);
                        // Seed long content so overflow exists to scroll.
                        for i in 0..20u64 {
                            s.input.push(InputFile {
                                id: 1000 + i,
                                path: PathBuf::from(format!("seed-{i}.wav")),
                                state: FileState::Queued,
                                size: 1024,
                                kind: InputKind::Audio,
                            });
                            s.output.push(OutFile {
                                id: 2000 + i,
                                name: format!("Seed {i}"),
                                md_name: format!("Seed {i}.md"),
                                dir: dir.clone(),
                                raw: "seed raw".into(),
                                summary: "seed summary".into(),
                                skipped: false,
                            });
                        }
                        s.goto_page(page);
                    });
                    // Entity construction borrows `cx` sequentially: one
                    // `cx.new` per statement so each borrow is released.
                    let input = cx.new(|_| InputView::assemble(store.clone()));
                    let record = cx.new(|cx| RecordView::assemble(window, cx, store.clone()));
                    let output = OutputView::new(window, cx, store.clone());
                    let models = ModelsView::new(cx, store.clone());
                    let settings = SettingsView::new(window, cx, store.clone());
                    let wizard = WizardView::new(cx, store.clone());
                    let view = cx.new(|_| {
                        RootView::assemble(
                            store.clone(),
                            input,
                            record,
                            output,
                            models,
                            settings,
                            wizard,
                        )
                    });
                    Root::new(view, window, cx)
                });
                // Drive the window WITHOUT leasing the root view entity:
                // `update_window` hands over `(&mut Window, &mut App)` so a
                // frame render inside never re-borrows the view under test
                // (leasing it first panics with "already being updated").
                // Wheel events go straight at viewport center: the scroller
                // wrapper itself is unobservable (plain div id), but its
                // subtree resolves via `within`, and the bottom marker is
                // observed — movement + reachability prove scrolling works.
                let any: AnyWindowHandle = handle.into();
                cx.update(|cx| {
                    cx.update_window(any, |_, window, cx| {
                        window.render_frame(cx);
                        let win_h = px(h);
                        let win_w = px(w);
                        // Horizontal containment, page subtree only: every
                        // painted element under the page scroller must sit
                        // inside the viewport width (the min-width:0 cascade
                        // + wrapping rows make horizontal scrolling
                        // unnecessary — nothing may be clipped sideways).
                        // Nav chrome is out of scope: the tab bar scrolls
                        // internally, the sidebar is fixed-width. Nested
                        // x-scrollers (device names, waveform) are exempt —
                        // their content is reachable by horizontal scroll by
                        // design (proven below for the device strip).
                        let scope_id = gpui_kit::ElementId::from(scroll_id);
                        let x_strips = ["rec-devices-x"];
                        let snaps = gpui_kit::base::test_support::snapshots(window);
                        for s in snaps.iter().filter(|s| s.path().contains(&scope_id)) {
                            if x_strips.iter().any(|x| {
                                s.path().contains(&gpui_kit::ElementId::from(*x))
                            }) {
                                continue;
                            }
                            let b = s.bounds();
                            assert!(
                                b.origin.x >= px(-1.) && b.right() <= win_w + px(1.),
                                "{page:?} {w}x{h} {nav}: element past horizontal edge: {b:?} {:?}",
                                s.path()
                            );
                        }
                        // Closures take the window per call (transient
                        // reborrows): holding `&mut Window` across calls
                        // would collide with the wheel dispatch below.
                        let marker_y = |window: &mut Window| {
                            window
                                .within(scroll_id)
                                .find("page-bottom")
                                .bounds()
                                .origin
                                .y
                        };
                        let wheel = |window: &mut Window, cx: &mut App, dy: f32| {
                            // GPUI hit-tests wheel events at the STORED mouse
                            // position (not the event position): move there
                            // first or no scrollable accepts the event. Aim
                            // inside the page: x=100 is the sidebar menu in
                            // sidebar mode.
                            let at = point(
                                px(if nav == "sidebar" { w - 50. } else { 100. }),
                                px(h / 2.),
                            );
                            window.dispatch_event(
                                MouseMoveEvent {
                                    position: at,
                                    pressed_button: None,
                                    modifiers: Default::default(),
                                }
                                .to_platform_input(),
                                cx,
                            );
                            window.dispatch_event(
                                ScrollWheelEvent {
                                    position: at,
                                    delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                                    ..Default::default()
                                }
                                .to_platform_input(),
                                cx,
                            );
                            window.render_frame(cx);
                        };
                        let bottom0 = marker_y(window);
                        if bottom0 > win_h {
                            // Content overflows: wheel must move it.
                            wheel(window, cx, -300.);
                            let bottom1 = marker_y(window);
                            assert!(
                                bottom1 < bottom0,
                                "{page:?} {w}x{h} {nav}: wheel did not move content ({bottom0:?} -> {bottom1:?})"
                            );
                            // Repeat to the end: the bottom must become reachable.
                            for _ in 0..30 {
                                let b = window
                                    .within(scroll_id)
                                    .find("page-bottom")
                                    .bounds();
                                if b.bottom() <= win_h + px(1.) {
                                    break;
                                }
                                wheel(window, cx, -600.);
                            }
                            let end =
                                window.within(scroll_id).find("page-bottom").bounds();
                            assert!(
                                end.bottom() <= win_h + px(1.),
                                "{page:?} {w}x{h} {nav}: bottom unreachable after full scroll (ends at {end:?}, window {win_h:?})"
                            );
                        }
                        // Record device strip: OS-given names overflow by
                        // design — prove the strip holds scrollable overflow
                        // (content wider than the viewport) instead of
                        // clipping. The Horizontal-axis primitive is covered
                        // by the kit's own suite; what matters here is that
                        // the strip, not the page, owns the excess width.
                        if page == Page::Record {
                            let strip_id = gpui_kit::ElementId::from("rec-devices-x");
                            let mut right = px(0.);
                            let mut count = 0u32;
                            for s in snaps.iter().filter(|s| s.path().contains(&strip_id)) {
                                count += 1;
                                if s.bounds().right() > right {
                                    right = s.bounds().right();
                                }
                            }
                            assert!(count > 0, "record device strip rendered no buttons");
                            // Overflow only exists on narrow windows (at
                            // 800px+ the buttons fit); assert it exactly
                            // where the strip must earn its keep.
                            if w < 400. {
                                assert!(
                                    right > win_w,
                                    "{page:?} {w}x{h} {nav}: device strip has no horizontal overflow to scroll ({right:?} <= {win_w:?})"
                                );
                            }
                        }
                    })
                    .unwrap();
                });
            }
        }
        pv_backend::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Wizard owns its scroll box outside the page slot: same bounded +
    /// wheel-moves-content contract at small windows.
    #[gpui_kit::test]
    fn wizard_scrolls_vertically_at_small_windows(cx: &mut TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{point, size, AnyWindowHandle, InputEvent, MouseMoveEvent, ScrollDelta, ScrollWheelEvent};

        // Shared scroll lock (see pages test) + store suite locks.
        let _scroll = SCROLL_LOCK.lock().unwrap();
        let _plock = crate::store::tests::PREFS_LOCK.lock().unwrap();
        let _mlock = crate::store::tests::MANIFEST_LOCK.lock().unwrap();

        let dir = std::env::temp_dir().join(format!("vm-scroll-wz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        pv_backend::dirs::set_models_dir_override(dir.clone());

        cx.update(gpui_kit::init);
        cx.update(|cx| crate::theme::apply_theme("dark", false, cx));

        for (w, h) in [(200., 200.), (800., 600.)] {
            let handle = cx.open_window(size(px(w), px(h)), |window, cx| {
                let store = cx.new(|_| Store::new());
                store.update(cx, |s, _| {
                    s.wizard_open = true;
                });
                let input = cx.new(|_| InputView::assemble(store.clone()));
                let record = cx.new(|cx| RecordView::assemble(window, cx, store.clone()));
                let output = OutputView::new(window, cx, store.clone());
                let models = ModelsView::new(cx, store.clone());
                let settings = SettingsView::new(window, cx, store.clone());
                let wizard = WizardView::new(cx, store.clone());
                let view = cx.new(|_| {
                    RootView::assemble(
                        store.clone(),
                        input,
                        record,
                        output,
                        models,
                        settings,
                        wizard,
                    )
                });
                Root::new(view, window, cx)
            });
            let any: AnyWindowHandle = handle.into();
            cx.update(|cx| {
                cx.update_window(any, |_, window, cx| {
                    window.render_frame(cx);
                    let win_h = px(h);
                    let win_w = px(w);
                    // Horizontal containment, page subtree only (nav chrome
                    // excluded — same rationale as the pages test).
                    let scope_id = gpui_kit::ElementId::from("wizard-scroll");
                    for s in gpui_kit::base::test_support::snapshots(window)
                        .iter()
                        .filter(|s| s.path().contains(&scope_id))
                    {
                        let b = s.bounds();
                        assert!(
                            b.origin.x >= px(-1.) && b.right() <= win_w + px(1.),
                            "wizard {w}x{h}: element past horizontal edge: {b:?} {:?}",
                            s.path()
                        );
                    }
                    let win_w = px(w);
                    // Horizontal containment: every painted element must sit
                    // inside the viewport width (rows wrap + floors fit, so
                    // nothing may be clipped sideways).
                    for s in gpui_kit::base::test_support::snapshots(window).iter() {
                        let b = s.bounds();
                        assert!(
                            b.origin.x >= px(-1.) && b.right() <= win_w + px(1.),
                            "wizard {w}x{h}: element past horizontal edge: {b:?} {:?}",
                            s.path()
                        );
                    }
                    let marker_y = |window: &mut Window| {
                        window
                            .within("wizard-scroll")
                            .find("page-bottom")
                            .bounds()
                            .origin
                            .y
                    };
                    let bottom0 = marker_y(window);
                    if bottom0 > win_h {
                        window.dispatch_event(
                            MouseMoveEvent {
                                position: point(px(100.), px(100.)),
                                pressed_button: None,
                                modifiers: Default::default(),
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.dispatch_event(
                            ScrollWheelEvent {
                                position: point(px(100.), px(100.)),
                                delta: ScrollDelta::Pixels(point(px(0.), px(-300.))),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        let bottom1 = marker_y(window);
                        assert!(
                        bottom1 < bottom0,
                        "wizard {w}x{h}: wheel did not move content ({bottom0:?} -> {bottom1:?})"
                    );
                    }
                })
                .unwrap();
            });
        }
        pv_backend::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
