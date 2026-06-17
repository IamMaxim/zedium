# Project Tabs

Zedium shows your open projects as tabs centered in the title bar, on every
platform (Linux, macOS, and Windows). Each tab is one project; switching tabs
swaps the entire workspace for that project.

## Enabling and disabling

Project tabs are on by default. To turn them off, set the `project_tabs`
boolean in your `settings.json`. Like other extension-style settings in Zedium,
this is a top-level key, not nested under another section:

```json
{
  "project_tabs": false
}
```

## Using the tabs

- Click a tab to switch to that project.
- The `×` on a tab closes that project. Closing the last remaining tab leaves
  an empty workspace.
- The `+` button opens the Recent Projects picker so you can add another
  project as a new tab.
- Drag tabs to reorder them. The order is saved and restored across restarts.

## Keyboard shortcuts

- Next project tab: `ctrl-shift-]` (Linux/Windows), `cmd-shift-]` (macOS)
- Previous project tab: `ctrl-shift-[` (Linux/Windows), `cmd-shift-[` (macOS)
- `multi_workspace::NewProjectTab` opens a new project tab. It is unbound by
  default — bind it in your keymap, or use the `+` button or the command
  palette.

## Separate windows

`workspace: new window` still opens a separate OS window, exactly as before.
Project tabs live within a single window and do not change this behavior.
