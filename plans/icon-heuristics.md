# Icon heuristics plan

Replace the three install-specific string predicates in `src/apps.rs`
(`use_direct_package_logo`, `use_icon_background`, `round_icon_background`)
with conditions derived from the icon pixels.

## Problem

Three predicates encode literal identifiers from one machine:

- `"Claude_pzs8sxrjxfjjc!Claude"` — this install's Claude package AUMID.
- `"shell:AppsFolder\\Chrome._crx_cadlkdcgmdikeeg."` — a crx hash that
  differs per Chrome profile and per install.
- `"shell:AppsFolder\\f6cbcda5-b021-4d0e-9fd7-4c5b41ea0aad"` — an Edge PWA
  GUID that exists only here.

They no-op silently for anyone else, so the released binary carries
cosmetic fixes that only work on the author's machine. The tests lock in
the constants, not the underlying conditions. Each of the three real
signals is visible in the pixels, so all three can be detected instead of
enumerated.

## Chosen behavior

### 1. Blurry packaged app (was `use_direct_package_logo`)

The signal is "AppsFolder renders this packaged app's icon visibly soft".
Detect it by rendering both candidates and comparing sharpness.

`icon_source` returns an `IconSource { primary, alternate }` instead of a
`String`. `alternate` is the manifest `Square150x150Logo` path, resolved
whenever the exe has an `AppxManifest.xml` ancestor whose manifest names
the AUMID — no AUMID list. Chrome/Edge PWAs have no manifest, so their
`alternate` is `None` and nothing changes for them.

The loader loads both and keeps the alternate only when it beats the
AppsFolder render by a clear margin (1.15×), so Slack, Teams and other
packaged apps stay on shell artwork on a near-tie and keep their unplated
styling and visual sizing.

`sharpness` = mean absolute neighbour gradient of premultiplied luma,
divided by the image's own luma standard deviation. Normalizing by
contrast makes padding and palette differences between the two renders
largely cancel, leaving softness as the thing measured.

Cost: one extra `GetImage` plus a manifest read per packaged app, once.
Both sit behind caches that a59f6e5 already made process-lifetime:
`icon_bgra` for the pixels and `app_label` for the resolved source. The
startup `WM_WARM` pass calls `apps::warm` for every running app, so the
extra render is paid before the first keystroke rather than on the path
between WIN+TAB and the dialog. The manifest read is no longer gated on
an AUMID, so it now runs for every packaged app rather than for Claude
alone; 7ff51cb hardened `xml_attr` against namespaced and malformed
attributes, which is what makes running it against arbitrary manifests
safe.

### 2. Icon plate (was `use_icon_background`)

The signal is "this mark is a transparent monochrome glyph that needs the
taskbar's white plate". Load the icon plain first, then decide from the
pixels: the icon has a genuine transparent area, and its opaque ink is
near-neutral (low chroma) and dark. If so, reload it with
`SIIGBF_ICONBACKGROUND`.

Scoped to `shell:AppsFolder\` sources, where that flag is meaningful.
That is a category of source, not a machine-specific value.

Theme-independent by decision: the plate matches what the taskbar shows
in both light and dark mode, and the icon cache key stays theme-free.
A theme-aware rule would force the theme into the cache key and make
`apps.rs` depend on the UI palette.

### 3. Rounded corners (was the Edge PWA GUID)

The signal is "this is a full-bleed square with no transparent margin",
which is exactly the plated ChatGPT icon and the square Edge PWA icon.
Detect it as: the border ring of the resulting image is opaque, with no
transparent margin anywhere along it.

Applied only to AppsFolder-sourced images. Direct package PNGs
(the sharpness path above) keep their current unrounded look.

## Changes

`src/apps.rs`

- Remove `use_direct_package_logo`, `use_icon_background`,
  `round_icon_background`, and drop them from the `mod win` `use super::`
  list at `src/apps.rs:243`.
- Add pure, host-testable helpers: `sharpness`, `prefers_alternate_icon`,
  `wants_icon_plate`, `is_full_bleed`.
- Add `IconSource`; `icon_source` and `load_icon_source` return it;
  `packaged_logo_source` is attempted unconditionally rather than gated on
  an AUMID. `app_label`'s memo tuple becomes `(String, IconSource)`.
- Split `load_icon_bgra` into a raw shell load taking a `background: bool`
  flag, plus a chooser that runs load → analyse → optional plate reload →
  optional corner rounding. `icon_bgra` takes `&IconSource`; the cache key
  stays the `primary` string. `warm` passes the `IconSource` through
  unchanged.

`src/ui.rs`

- `show_list` takes `&IconSource` instead of `&str`. `show` resolves its
  own source per group via `icon_source(&g.key)` and needs no change
  beyond the type flowing through `icon_bgra`.

`src/main.rs`

- The `icon` locals at `src/main.rs:621` and `src/main.rs:669` become
  `IconSource`; pass them through to `show_list` at `src/main.rs:624` and
  `src/main.rs:672`.

Tests

- Drop the three tests that assert the constants
  (`rounded_background_is_limited_to_square_pwa_icons`,
  `icon_background_is_limited_to_chatgpt_pwa`,
  `direct_package_logo_is_limited_to_known_blurry_apps`).
- Add tests for the four new predicates: a blurred edge scores below a
  sharp one and a near-tie keeps the primary; a dark monochrome mark with
  transparency wants a plate while a colorful or fully-opaque icon does
  not; full-bleed detection accepts an opaque border and rejects one with
  a transparent margin.
- Keep the `xml_attr` tests from 7ff51cb and
  `rounded_background_mask_clears_only_corner_pixels`; none of them touch
  the removed predicates.

`plans/manual-tests.md`

- Extend the M5 "UWP icons" checks (`plans/manual-tests.md:209`): Claude
  sharp, ChatGPT plated and rounded, Slack / Teams / Settings / Calculator
  unchanged from today.

## Verification

- `make test` on Linux covers the pure logic.
- `make build` (cross-compile to `x86_64-pc-windows-msvc`) proves the
  `cfg(windows)` module still compiles.
- The visual result is only verifiable on Windows by hand; the automated
  runs above do not establish it.

## Risks

The sharpness comparison is a heuristic. The 1.15× margin keeps ties on
AppsFolder, but some packaged app could still cross it and get its
manifest logo where the shell's styling was preferable. The threshold is
a single constant and easy to retune once the real icons are seen on
Windows.

Doing the manifest read and second `GetImage` for every packaged app,
rather than for one AUMID, adds work to the startup warm pass that
a59f6e5 introduced. That pass is off the interactive path, so it costs
startup time rather than dialog latency, but a machine with many packaged
apps running pays it for all of them.
