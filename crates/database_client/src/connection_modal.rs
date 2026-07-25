use crate::{
    connection::profile::{ConnectionProfile, SslMode},
    connection::store,
    panel::DatabasePanel,
};
use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, TaskExt as _, Window,
};
use menu::{Cancel, Confirm};
use ui::{Checkbox, ToggleState, prelude::*};
use util::ResultExt as _;
use workspace::ModalView;

pub struct ConnectionModal {
    panel: Entity<DatabasePanel>,
    name_input: Entity<Editor>,
    host_input: Entity<Editor>,
    port_input: Entity<Editor>,
    database_input: Entity<Editor>,
    user_input: Entity<Editor>,
    password_input: Entity<Editor>,
    ssl_mode: SslMode,
    /// When `Some`, we are editing an existing profile rather than creating a new one.
    edit_profile: Option<ConnectionProfile>,
    read_only: bool,
    test_status: Option<(String, bool)>,
}

/// Formats the result of a connection probe into a status message and a success flag.
pub(crate) fn test_status_message(result: Result<(), String>) -> (String, bool) {
    match result {
        Ok(()) => ("Connection succeeded".to_string(), true),
        Err(error) => (format!("Connection failed: {error}"), false),
    }
}

/// A trivial toggle used by the checkbox click.
pub(crate) fn toggled(current: bool) -> bool {
    !current
}

impl ConnectionModal {
    pub fn new(panel: Entity<DatabasePanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Connection name", window, cx);
            editor
        });
        let host_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("localhost", window, cx);
            editor
        });
        let port_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("5432", window, cx);
            editor
        });
        let database_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("postgres", window, cx);
            editor
        });
        let user_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("user", window, cx);
            editor
        });
        let password_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("password", window, cx);
            editor.set_masked(true, cx);
            editor
        });
        Self {
            panel,
            name_input,
            host_input,
            port_input,
            database_input,
            user_input,
            password_input,
            ssl_mode: SslMode::default(),
            edit_profile: None,
            read_only: false,
            test_status: None,
        }
    }

    /// Open the modal pre-filled with an existing profile for editing.
    pub fn edit(
        panel: Entity<DatabasePanel>,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Connection name", window, cx);
            editor.set_text(profile.name.as_str(), window, cx);
            editor
        });
        let host_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("localhost", window, cx);
            editor.set_text(profile.host.as_str(), window, cx);
            editor
        });
        let port_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("5432", window, cx);
            editor.set_text(profile.port.to_string().as_str(), window, cx);
            editor
        });
        let database_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("postgres", window, cx);
            editor.set_text(profile.database.as_str(), window, cx);
            editor
        });
        let user_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("user", window, cx);
            editor.set_text(profile.user.as_str(), window, cx);
            editor
        });
        let password_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("password (leave blank to keep current)", window, cx);
            editor.set_masked(true, cx);
            editor
        });
        let ssl_mode = profile.ssl_mode;
        Self {
            panel,
            name_input,
            host_input,
            port_input,
            database_input,
            user_input,
            password_input,
            ssl_mode,
            read_only: profile.read_only,
            edit_profile: Some(profile),
            test_status: None,
        }
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).text(cx);
        let host = self.host_input.read(cx).text(cx);
        let port_str = self.port_input.read(cx).text(cx);
        let database = self.database_input.read(cx).text(cx);
        let user = self.user_input.read(cx).text(cx);
        let password = self.password_input.read(cx).text(cx);

        let port = port_str.parse::<u16>().unwrap_or(5432);

        if let Some(ref existing) = self.edit_profile {
            // Edit mode: replace the profile in JSON; update keychain only when password is non-blank.
            let profile = ConnectionProfile {
                id: existing.id.clone(),
                name,
                host,
                port,
                database,
                user,
                ssl_mode: self.ssl_mode,
                read_only: self.read_only,
            };
            let mut profiles = store::load_profiles();
            store::replace_profile_by_id(&mut profiles, profile.clone());
            match store::save_profiles(&profiles) {
                Ok(()) => {
                    if !password.is_empty() {
                        store::store_password(cx, &profile, &password).detach_and_log_err(cx);
                    }
                }
                Err(e) => {
                    log::error!("failed to save connection profiles: {e}");
                }
            }
        } else {
            // Add mode: append a new profile.
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let profile = ConnectionProfile {
                id: format!("conn-{millis}"),
                name,
                host,
                port,
                database,
                user,
                ssl_mode: self.ssl_mode,
                read_only: self.read_only,
            };

            let mut profiles = store::load_profiles();
            profiles.push(profile.clone());
            match store::save_profiles(&profiles) {
                Ok(()) => {
                    store::store_password(cx, &profile, &password).detach_and_log_err(cx);
                }
                Err(e) => {
                    log::error!("failed to save connection profile: {e}");
                }
            }
        }

        self.panel.update(cx, |panel, cx| {
            panel.reload_profiles(cx);
        });

        cx.emit(DismissEvent);
    }

    fn cycle_ssl_mode(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ssl_mode = match self.ssl_mode {
            SslMode::Disable => SslMode::Prefer,
            SslMode::Prefer => SslMode::Require,
            SslMode::Require => SslMode::Disable,
        };
        cx.notify();
    }

    fn toggle_read_only(&mut self, cx: &mut Context<Self>) {
        self.read_only = toggled(self.read_only);
        cx.notify();
    }

    fn profile_from_inputs(&self, cx: &App) -> (ConnectionProfile, String) {
        let port = self
            .port_input
            .read(cx)
            .text(cx)
            .parse::<u16>()
            .unwrap_or(5432);
        let profile = ConnectionProfile {
            id: self
                .edit_profile
                .as_ref()
                .map(|existing| existing.id.clone())
                .unwrap_or_default(),
            name: self.name_input.read(cx).text(cx),
            host: self.host_input.read(cx).text(cx),
            port,
            database: self.database_input.read(cx).text(cx),
            user: self.user_input.read(cx).text(cx),
            ssl_mode: self.ssl_mode,
            read_only: self.read_only,
        };
        let password = self.password_input.read(cx).text(cx);
        (profile, password)
    }

    fn test_connection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let (profile, password) = self.profile_from_inputs(cx);
        let password = (!password.is_empty()).then_some(password);
        self.test_status = Some(("Testing…".to_string(), true));
        cx.notify();
        let connect = crate::connection::client::Connection::connect(profile, password, cx);
        cx.spawn(async move |this, cx| {
            let result = connect.await;
            this.update(cx, |this, cx| {
                // Drop the Connection on success; only report status.
                this.test_status = Some(test_status_message(
                    result
                        .map(|_connection| ())
                        .map_err(|error| error.to_string()),
                ));
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

impl ModalView for ConnectionModal {}
impl EventEmitter<DismissEvent> for ConnectionModal {}

impl Focusable for ConnectionModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_input.focus_handle(cx)
    }
}

impl Render for ConnectionModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ssl_label = match self.ssl_mode {
            SslMode::Disable => "SSL: Disable",
            SslMode::Prefer => "SSL: Prefer",
            SslMode::Require => "SSL: Require",
        };
        let title = if self.edit_profile.is_some() {
            "Edit Connection"
        } else {
            "New Connection"
        };

        v_flex()
            .key_context("ConnectionModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .w_full()
                    .gap_1p5()
                    .child(Icon::new(IconName::DatabaseZap).size(IconSize::XSmall))
                    .child(Headline::new(title).size(HeadlineSize::XSmall)),
            )
            .child(
                v_flex()
                    .px_3()
                    .pb_3()
                    .w_full()
                    .gap_1()
                    .child(Label::new("Name"))
                    .child(self.name_input.clone())
                    .child(Label::new("Host"))
                    .child(self.host_input.clone())
                    .child(Label::new("Port"))
                    .child(self.port_input.clone())
                    .child(Label::new("Database"))
                    .child(self.database_input.clone())
                    .child(Label::new("User"))
                    .child(self.user_input.clone())
                    .child(Label::new("Password"))
                    .child(self.password_input.clone())
                    .child(Button::new("ssl-mode", ssl_label).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.cycle_ssl_mode(window, cx);
                        },
                    )))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Checkbox::new("read-only", ToggleState::from(self.read_only))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.toggle_read_only(cx)
                                    })),
                            )
                            .child(Label::new("Read-only (disable all edits)")),
                    )
                    .child(Button::new("test-connection", "Test connection").on_click(
                        cx.listener(|this, _, window, cx| this.test_connection(window, cx)),
                    ))
                    .when_some(self.test_status.clone(), |this, (message, ok)| {
                        this.child(Label::new(message).color(if ok {
                            Color::Success
                        } else {
                            Color::Error
                        }))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::toggled;

    #[test]
    fn read_only_toggles() {
        assert!(toggled(false));
        assert!(!toggled(true));
    }
}

#[cfg(test)]
mod test_connection_tests {
    use super::test_status_message;

    #[test]
    fn formats_success_and_failure() {
        assert_eq!(
            test_status_message(Ok(())),
            ("Connection succeeded".into(), true)
        );
        let (message, ok) = test_status_message(Err("timeout".into()));
        assert!(!ok);
        assert!(message.contains("timeout"));
    }
}
