<div align="center">
  <img src="assets/guicons-logo.svg" alt="guicons logo" width="400"/>

  # guicons
</div>

A manifest-driven icon system for native Rust GUI applications: describe
your icon set once in TOML (local files, URLs, iconify.design ids, font
glyphs, mixed freely), get a typed Rust registry, a Slint component, and
a compile-time-checked `icon!` macro that hands your GUI framework its
native image type directly.

## Why guicons?

Most native Rust GUI apps hardcode `include_bytes!` per icon by hand - no
manifest, no shared registry, no compile-time check that a referenced
icon actually exists in the set. guicons gives you that single source of
truth, with codegen and a macro on top so referencing an icon is a
typed, checked operation instead of a bare file path.

## Features

- **One manifest, `icons.gui.toml`**: family/variant/size axes,
  `[link] includes = [...]` to split across files, `[providers.<name>]`
  schemas (10+ built-in providers, with per-field `.override`).
- **Any source, mixed freely**: local file, URL, iconify.design id
  (auto-fetched and cached offline), or a font glyph.
- **Typed build-time codegen** (`guicons-build`): a Rust registry with
  per-family/size builder methods, and a matching Slint `Icon`
  component.
- **`guicons::icon!`**: resolves a selector against your manifest at
  compile time straight into your active GUI framework's native type
  (`slint::Image`, an iced `Handle`, an egui `ImageSource`).
- **Slint integration out of the box**, with a runnable example in
  `crates/guicons/examples/`.
- **CLI** (`guicons-cli`): `icons fetch`/`update` populates the
  offline iconify cache and `icons.lock`; `icons add <iconify-id|file>`
  adds an icon to your manifest in one command.

## Usage

```toml
# icons.gui.toml
[defaults]
root = "assets"

[providers.phosphor]
variants = ["thin", "light", "bold", "fill", "duotone"]

[link]
includes = ["icons/toolbar.gui.toml"]

[settings]
variants.filled = { file = "settings-filled.svg" }
variants.regular = { file = "settings-regular.svg" }

[docker]
file = "docker.svg"

[home]
iconify = "mdi:home"

[spinner]
glyph = "MyIconFont:U+E001"
```

```toml
# icons/toolbar.gui.toml - split out via [link], same [defaults] root
[toolbar.16]
variants.filled = { file = "toolbar-16-filled.svg" }

[toolbar.24]
variants.filled = { file = "toolbar-24-filled.svg" }
variants.regular = { file = "toolbar-24-regular.svg" }
```

```rust
// build.rs
fn main() {
    guicons_build::IconBuild::auto()
        .emit(guicons_build::Emit::Rust)
        .emit(guicons_build::Emit::Slint)
        .build();
}
```

```rust
// lib.rs or main.rs
guicons::include_icons!();
```

Most of the time you just want to hand an icon straight to your GUI
framework - `guicons::icon!` resolves a selector, checked against your
manifest at compile time, directly to whichever framework's native type
matches your enabled feature (`slint`, `iced`, `windows-reactor` or `egui`;
plain `IconData` if none is enabled):

```rust
// with the `slint` feature enabled, this is already a `slint::Image`
my_component.set_icon(guicons::icon!(settings.filled));
```

Use `guicons::icon_data!` instead if you always want the plain `IconData`,
regardless of which GUI feature is enabled.

Use `guicons` normally at runtime, and add `guicons-build` as a
`build-dependencies` entry when generating code from `build.rs`.

### Icon color

`paint` sets the color an SVG's `currentColor` is drawn in, one for both
themes or one per theme. The nearest declaration wins: the entry, its
enclosing tables, its iconify provider, then `[defaults]`. `"none"`
cancels what comes from above.

```toml
[defaults]
paint = { light = "#1a1a1a", dark = "#ffffff" }

[providers.logos]
paint = "none"

[prohibited]
iconify = "fluent:prohibited-24-regular"
paint = "#c42b1c"
```

The app sets the theme once, and again when it changes; icons built after
that use its color. `color = ...` overrides it at a call site. Each
recolored copy lives until the process exits, so pick colors from a fixed
set rather than animating them:

```rust
guicons::set_theme(guicons::Theme::Dark);
guicons::icon!(settings.filled);
guicons::icon!(settings.filled, color = accent);
```

A painted SVG without `currentColor`, or `color = ...` on an icon without
`paint`, is a compile error.

### Downloaded icons

`icons fetch` downloads iconify/url icons into `.cache/guicons/` at the
workspace root and writes their sha256 to `icons.lock` next to the
manifest. Commit both: builds never touch the network unless
`GUICONS_ALLOW_NETWORK=1` is set, and a cached or downloaded icon that
differs from `icons.lock` fails the build. `icons update` re-downloads and
re-locks everything.

### Dynamic/runtime lookups

For the less common case where the family/variant is only known at
runtime (not a literal you can pass to `icon!`), or you need a
runtime-swappable `IconKey` (theming, hot-reload via `IconResolver`),
use the generated registry functions or `guicons::icon_key!` directly:

```rust
let key = icons::key_from_dynamic_family_variant("settings", None, Some("filled"));
let key = guicons::icon_key!(settings.filled); // icons::keys::SETTINGS_FILLED
```

## GUI framework integration

### Slint (`slint` feature)

The generated `icons.slint` (from `Emit::Slint` above) exports an `Icon`
component that switches on a `name` property, plus one typed component
per icon (`settings-filled` → `SettingsFilledIcon`) - `import` either
from your own `.slint` files:

```slint
import { Icon, SettingsFilledIcon } from "icons.slint"; // resolved via an include path pointing at OUT_DIR

// Dynamic - runtime string match against `name`
Icon {
    name: "settings-filled";
}

// Typed - one exported component per icon, checked at compile time
SettingsFilledIcon {}
```

For icon data you don't already have as `icon!` output - e.g. resolved at
runtime through an `IconResolver` - `guicons::slint` converts `IconData`/
`IconSource` to a Slint `Image`, or to a `(font_family, char)` pair for
glyph sources. See [`examples/slint_icon.rs`](crates/guicons/examples/slint_icon.rs)
for a complete, runnable example (`cargo run -p guicons --example slint_icon --features slint`).

### iced (`iced` feature)

`guicons::iced` gives you `iced::widget::svg::Handle`/`image::Handle`
directly from `IconData`/`IconSource`, plus the same glyph pair for
font-based icons. See [`examples/iced_icon.rs`](crates/guicons/examples/iced_icon.rs)
for a complete, runnable example (`cargo run -p guicons --example iced_icon --features iced`).

### egui (`egui` feature)

`icon!` yields an `egui::ImageSource`. Install image loaders
(`egui_extras` with the `svg` feature):

```rust
egui_extras::install_image_loaders(&ctx);
ui.add(egui::Image::new(guicons::icon!("mdi:home")).fit_to_exact_size(egui::vec2(24.0, 24.0)));
```

