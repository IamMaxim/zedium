use editor::{Editor, EditorElement, EditorStyle};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, TextStyle, WeakEntity, Window, div, relative,
};
use language::Buffer;
use settings::Settings as _;
use theme::ActiveTheme;
use theme_settings::ThemeSettings;
use ui::prelude::*;
use ui::{IconButtonShape, Tooltip};
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

/// A read-only tab showing an object's reconstructed / server-generated DDL.
/// Not runnable and not persisted (see the plan's Task 4 note).
pub struct DdlView {
    editor: Entity<Editor>,
    title: SharedString,
    ddl: SharedString,
    #[allow(dead_code)]
    workspace: WeakEntity<Workspace>,
}

/// `DdlView` never mutates in a way that affects its tab, so it emits nothing.
pub enum DdlViewEvent {}

impl DdlView {
    pub fn new(
        title: SharedString,
        ddl: SharedString,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| Buffer::local(ddl.to_string(), cx));
        let editor = cx.new(|cx| {
            let mut editor = Editor::for_buffer(buffer.clone(), None, window, cx);
            editor.set_read_only(true);
            editor
        });

        // Mirror `QueryView`: resolve the SQL language off-thread, then attach it
        // so the read-only buffer gets syntax highlighting.
        cx.spawn({
            let workspace = workspace.clone();
            let buffer = buffer.clone();
            async move |_this, cx| {
                let Ok(language_future) = workspace.read_with(cx, |workspace, cx| {
                    workspace
                        .project()
                        .read(cx)
                        .languages()
                        .language_for_name("SQL")
                }) else {
                    return;
                };
                let language = language_future.await.ok();
                buffer.update(cx, |buffer, cx| {
                    buffer.set_language(language, cx);
                });
            }
        })
        .detach();

        DdlView {
            editor,
            title,
            ddl,
            workspace,
        }
    }

    pub fn open(
        workspace: &mut Workspace,
        title: SharedString,
        ddl: SharedString,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.entity().downgrade();
        let view = cx.new(|cx| DdlView::new(title, ddl, weak, window, cx));
        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
    }

    fn editor_style(cx: &App) -> EditorStyle {
        let settings = ThemeSettings::get_global(cx);
        let theme = cx.theme();
        let text_style = TextStyle {
            color: theme.colors().text,
            font_family: settings.buffer_font.family.clone(),
            font_features: settings.buffer_font.features.clone(),
            font_fallbacks: settings.buffer_font.fallbacks.clone(),
            font_size: settings.buffer_font_size(cx).into(),
            font_weight: settings.buffer_font.weight,
            line_height: relative(settings.buffer_line_height.value()),
            ..Default::default()
        };
        EditorStyle {
            background: theme.colors().editor_background,
            local_player: theme.players().local(),
            text: text_style,
            syntax: theme.syntax().clone(),
            ..Default::default()
        }
    }
}

impl Focusable for DdlView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<DdlViewEvent> for DdlView {}

impl Item for DdlView {
    type Event = DdlViewEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn to_item_events(event: &DdlViewEvent, _f: &mut dyn FnMut(ItemEvent)) {
        match *event {}
    }
}

impl Render for DdlView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor_style = Self::editor_style(cx);
        let copy_button = IconButton::new("copy-ddl", IconName::Copy)
            .shape(IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .tooltip(Tooltip::text("Copy DDL"))
            .on_click(cx.listener(|this, _, _window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(this.ddl.to_string()));
            }));

        let toolbar = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(copy_button)
            .child(
                Label::new(self.title.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );

        div().size_full().flex().flex_col().child(toolbar).child(
            div()
                .flex_1()
                .w_full()
                .min_h_0()
                .overflow_hidden()
                .child(EditorElement::new(&self.editor, editor_style)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    #[gpui::test]
    async fn ddl_view_holds_readonly_sql(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::Workspace::test_new(project.clone(), window, cx)
        });

        workspace.update_in(cx, |workspace, window, cx| {
            DdlView::open(
                workspace,
                "users DDL".into(),
                "CREATE TABLE \"public\".\"users\" ();".into(),
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<DdlView>(cx)
                .next()
                .expect("DdlView tab should exist")
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.ddl.as_ref(), "CREATE TABLE \"public\".\"users\" ();");
            assert!(
                view.editor.read(cx).read_only(cx),
                "DDL editor must be read-only"
            );
            assert_eq!(
                view.editor.read(cx).text(cx),
                "CREATE TABLE \"public\".\"users\" ();"
            );
        });
    }
}
