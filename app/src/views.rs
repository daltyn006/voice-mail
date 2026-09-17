//! Shell: top nav (Input / Output / Models) + page body. Single-window design:
//! processing progress and controls live on the Input page after Start.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::{div, rgb, Context, Entity, FontWeight, IntoElement, Render, Window};

use crate::store::{Page, Store};
use crate::theme;
use crate::views_input::InputView;
use crate::views_models::ModelsView;
use crate::views_output::OutputView;
use crate::views_record::RecordView;
use crate::views_settings::SettingsView;
use crate::views_wizard::WizardView;
use crate::{GotoInput, GotoOutput, PauseProc, Quit, ToggleDarkMode};

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

    fn nav_button(
        label: &'static str,
        page: Page,
        active: bool,
        store: Entity<Store>,
    ) -> impl IntoElement {
        let mut b = Button::new(label)
            .label(label)
            .font_weight(FontWeight::SEMIBOLD);
        if active {
            b = b.primary();
        }
        b.on_click(move |_, _, cx| {
            store.update(cx, |s, cx| {
                // goto_page auto-stages unsent Record takes to Input.
                s.goto_page(page);
                cx.notify();
            });
        })
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

    fn on_toggle_dark(&mut self, _: &ToggleDarkMode, _: &mut Window, cx: &mut Context<Self>) {
        let next = !self.store.read(cx).dark_mode;
        crate::theme::apply_theme(next, cx);
        self.store.update(cx, |st, cx| {
            st.set_dark_mode(next);
            cx.notify();
        });
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
        let (page, dark) = {
            let snap = self.store.read(cx);
            (snap.page, snap.dark_mode)
        };

        let body: gpui_kit::AnyElement = match page {
            Page::Input => self.input.clone().into_any_element(),
            Page::Record => self.record.clone().into_any_element(),
            Page::Output => self.output.clone().into_any_element(),
            Page::Models => self.models.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        };

        div()
            .id("root")
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_goto_input))
            .on_action(cx.listener(Self::on_goto_output))
            .on_action(cx.listener(Self::on_pause))
            .on_action(cx.listener(Self::on_toggle_dark))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .flex_wrap()
                    .p_2()
                    .bg(rgb(theme::bg(dark)))
                    .child(Self::nav_button("Input", Page::Input, page == Page::Input, self.store.clone()))
                    .child(Self::nav_button("Record", Page::Record, page == Page::Record, self.store.clone()))
                    .child(Self::nav_button("Output", Page::Output, page == Page::Output, self.store.clone()))
                    .child(Self::nav_button("Models", Page::Models, page == Page::Models, self.store.clone()))
                    .child(Self::nav_button("Settings", Page::Settings, page == Page::Settings, self.store.clone())),
            )
            // Bounded body slot: every page gets the leftover viewport height
            // so inner `flex_1 + overflow_y_scrollbar` containers (Settings,
            // Input, Models, Output tree) scroll instead of growing the
            // window. Collapses cleanly to just the nav at title-bar sizes.
            .child(div().flex_1().overflow_hidden().child(body))
            .into_any_element()
    }
}
