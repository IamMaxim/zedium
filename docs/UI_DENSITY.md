# Dense UI: per-bar height, font, and icon settings

Zedium adds fine-grained, per-bar density controls on top of upstream Zed's two
global knobs (`ui_font_size` and the 3-step `unstable.ui_density`). You can
independently tune the **status bar**, the **editor tab bar**, and the
**breadcrumbs / editor toolbar** row.

Each bar exposes three optional keys:

| Key | Meaning |
|-----|---------|
| `font_size` | The bar's base font size in px — it works like `ui_font_size`, but scoped to this one bar. Text, icons, and spacing in the bar scale relative to it, so lowering it makes the whole bar proportionally smaller. Unset = the global `ui_font_size`. |
| `icon_size` | Absolute icon size in px for that bar. Overrides the proportional icon scaling from `font_size`. Unset = default. |
| `padding` | Vertical padding in px. The bar's height is its content plus this padding, top and bottom — smaller = denser. Unset = default density. |

**All keys are optional.** Any key you leave unset behaves exactly like stock
Zed — these settings are purely additive and change nothing until you set them.

## How it works (and why nothing clips)

`font_size` sets the bar's *rem size* (the same mechanism `ui_font_size` uses
globally), applied to the entire bar subtree. Because virtually every size in
the UI — text, icons, gaps, line height — is expressed relative to the rem
size, the whole bar scales together as one unit. That's why text never clips
when you shrink a bar: the line height shrinks with the text, and the bar's
content-driven height follows.

Just like `ui_font_size: 16` renders ~14px label text (not literally 16px), a
bar's `font_size` is a base size, not the exact pixel height of every glyph —
different labels in a bar (e.g. the cursor position vs. a mode badge) keep their
relative sizes and all scale together.

`icon_size` is the one knob that breaks out of the proportional scaling: set it
to pin icons to an absolute pixel size regardless of `font_size` (useful if you
want a small font but slightly larger, easier-to-hit icons).

Values are clamped to sane ranges: `padding` 0–40 px, `font_size` 6–100 px,
`icon_size` 6–48 px.

## Settings

```jsonc
{
  // Bottom status bar
  "status_bar": {
    "padding": 2,
    "font_size": 11,
    "icon_size": 12
  },

  // Editor tab bar (the row of file tabs)
  "tab_bar": {
    "padding": 1,
    "font_size": 12,
    "icon_size": 13
  },

  // Breadcrumbs / editor toolbar row.
  // `padding` applies to the whole toolbar row (breadcrumbs, quick-action
  // buttons, and — best-effort — the buffer-search bar). When only breadcrumbs
  // are shown the row gets dense; when the search bar is open that row keeps the
  // height its input needs, so nothing is clipped.
  // `font_size` scales the breadcrumb text + the "›" separators (and toolbar
  // icons, unless `icon_size` overrides them). `icon_size` sizes the
  // quick-action icons.
  "toolbar": {
    "padding": 2,
    "font_size": 12,
    "icon_size": 14
  }
}
```

## A compact preset

A reasonable "dense" starting point:

```jsonc
{
  "status_bar": { "padding": 1, "font_size": 11, "icon_size": 12 },
  "tab_bar":    { "padding": 1, "font_size": 12, "icon_size": 13 },
  "toolbar":    { "padding": 1, "font_size": 12, "icon_size": 13 }
}
```

Combine with the global `ui_font_size` and `unstable.ui_density: "compact"` for
an even tighter chrome.

## Scope / notes

- These keys live next to the existing visibility toggles for each bar
  (`tab_bar.show`, `status_bar.*`, `toolbar.breadcrumbs`, …).
- The toolbar is shared by the breadcrumbs and the editor controls (search,
  quick actions), so `toolbar.*` necessarily affects that whole row — by design.
- Because `font_size` scales a bar uniformly, items contributed by any
  crate (e.g. the Vim mode indicator in the status bar, file-type icons on
  tabs) scale too — there is no per-widget allowlist to keep in sync.
- This is a UI-only feature; it does not affect Zedium's air-gap guarantees.
- There is currently no entry for these in the graphical settings editor; set
  them in `settings.json`.
