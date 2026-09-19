//! Shell: top nav (Input / Output / Models) + page body. Single-window design:
//! processing progress and controls live on the Input page after Start.

use gpui_kit::assets::IconName;
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, rgba, Animation, AnimationExt, Context, Div, Entity, IntoElement, Render, Window,
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
    /// containers (`input-scroll`, `settings-scroll`, …): a single scroller
    /// per page, so wheel events always reach the element that can move.
    /// (A scrollable wrapper here would sit under the cursor and eat the
    /// wheel while having nothing to scroll.) `min_w(0)` + clipped
    /// horizontal overflow keep narrow windows to a single column instead
    /// of clipping content.
    fn body_slot(body: gpui_kit::AnyElement) -> Div {
        div().flex_1().flex_auto().min_w(px(0.0)).child(
            div()
                .max_w(px(1200.0))
                .mx_auto()
                .w_full()
                .min_w(px(0.0))
                .p_4()
                .h_full()
                .overflow_x_hidden()
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

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // First-run wizard takes the whole window while open (never a trap:
        // every wizard state offers import / retry / continue-without).
        let wizard_open = self.store.read(cx).wizard_open;
        if wizard_open {
            return self.wizard.clone().into_any_element();
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
            // Sized to fill the content slot: the page-level `h_full`
            // scrollers resolve their height through this wrapper. An
            // unsized wrapper breaks that chain (inner scrollers go
            // unbounded, swallow wheel events, and the page never scrolls).
            div()
                .w_full()
                .h_full()
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
