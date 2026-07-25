# database_client

A Postgres database browser panel for Zed/Zedium.

## Configuration

Connection profiles are stored as JSON at:

```
$config_dir/database_client/connections.json
```

where `$config_dir` is the platform config directory (e.g. `~/.config/zed` on Linux, `~/Library/Application Support/Zed` on macOS).

Passwords are **not** stored on disk. They are kept in the OS keychain (Keychain on macOS, Secret Service on Linux) and looked up by a stable URL key derived from the connection parameters.

## Actions

| Action | Description |
|--------|-------------|
| `database_client::ToggleFocus` | Show or hide the Database panel |
| `database_client::NewConnection` | Open the New Connection modal |
| `database_client::RunQuery` | Execute the SQL in the active Query View |

Default key bindings are registered inside the crate:

- `cmd-enter` / `ctrl-enter` in the `QueryView` context runs `RunQuery`.

To add custom global bindings, add them to your `keymap.json`:

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "ctrl-shift-d": "database_client::ToggleFocus"
    }
  }
]
```

## SQL syntax highlighting

SQL syntax highlighting in the Query View requires the **SQL** language extension to be installed. Without it, queries are treated as plain text.

## Connection context menu

Right-clicking a connection node in the panel reveals three actions:

- **Disconnect** — drops the live connection; the next expand reconnects.
- **Edit** — opens the modal pre-filled with the existing settings; leaving the password field blank keeps the current keychain password.
- **Delete** — removes the profile from `connections.json` and deletes the keychain entry.

## Notes / limitations

- **TLS certificate validation:** When `sslmode` is `require` or `prefer`, the server certificate is validated against the OS trust store. A Postgres server using a self-signed certificate will fail to connect. Either configure the server with a properly-issued certificate, or set `sslmode` to `disable` for trusted local networks.

- **Changing connection host/port/user/database invalidates the saved password:** The keychain entry is keyed on those four fields. If you edit any of them, re-enter the password in the Edit dialog — leaving it blank will cause the saved password to be looked up under the old key and not found.
