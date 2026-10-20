# M1 UI design

This is the visual design of the M1 iPhone app: identity, design tokens, components, screens and the launch-time unlock behaviour. It was set by the maintainer on 2026-10-17, after a simulator walkthrough showed that the M1 UI had shipped with no visual design. The screens ran letterboxed (fixed by F18), the keyboard hid navigation, and settings rendered acceptance criteria as paragraphs.

The maintainer's design canvas and design-system artifact are the visual reference. They are private, so this document is self-contained: every value a task needs is here. Where this document and a task's older wording disagree on appearance, this document wins. Behaviour and acceptance criteria are unchanged unless a section below says otherwise.

## Identity

- **Mark:** an italic Instrument Serif ampersand (&). It is white on brand indigo for the icon and launch screen, `brand` in light theme, and `accent` in dark theme.
- **Wordmark:** "Oh And" in Instrument Serif regular, next to the mark.
- **Typeface licence:** Instrument Serif is SIL Open Font License 1.1. The app bundles the TTF files (regular and italic) with `OFL.txt` and never loads fonts from the network.
- **Baseline:** the iOS 26 Liquid Glass design. Navigation and actions float on glass over the content. Content sits on solid cards over a cool neutral ground. Glass is never put on glass.

## Tokens

Colours have a light and a dark value. Code defines them once, as dynamic colours that follow the system appearance, and never hard-codes a hex value at a call site.

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `brand` | `#1E2761` | `#2E3A94` | Identity only: icon ground, the mark, launch, empty states. Never a button. |
| `onBrand` | `#FFFFFF` | `#FFFFFF` | Mark and text on `brand`. |
| `accent` | `#3D4ED6` | `#8E9BFF` | The one interactive colour: primary buttons, links, toggles, checkmarks, caret. |
| `accentPressed` | `#2F3DB0` | `#A9B3FF` | Pressed state of accent fills. |
| `onAccent` | `#FFFFFF` | `#0B0C14` | Text and icons on an accent fill. |
| `tint` | `#E7E9FF` | `#23264A` | Relief fill: secondary buttons, selected chips, the Saved pill, icon tiles. `ink` on it. |
| `surface0` | `#F5F6FA` | `#0A0B12` | Page ground. |
| `surface1` | `#FFFFFF` | `#151726` | Cards and grouped sections. |
| `surface2` | `#ECEEF5` | `#1D2033` | Fields and tracks inside cards; unselected chips. |
| `glass` | white at 64% | `#1C1F32` at 58% | Fallback fill for floating controls where the system glass material is unavailable. |
| `glassStroke` | white at 90% | white at 12% | 1 pt edge on glass fallbacks. |
| `ink` | `#11132A` | `#F1F2FA` | Primary text and icons. |
| `inkMuted` | `#565B78` | `#A2A6C4` | Secondary text: footers, timestamps, row values. |
| `inkFaint` | `#8A8EA8` | `#6E7290` | Placeholders only (about 3.3:1). Never content. |
| `hairline` | `#E1E3EE` | `#2A2D44` | Rare separators inside a card. |
| `signalRecord` | `#E5484D` | `#FF6369` | Recording fills only (live dot, stop button). Never text. |
| `destructive` | `#C9373C` | `#FF6369` | Text and icons for Discard, Delete, Erase everything. |
| `signalSuccess` | `#1F8A5B` | `#4CC38A` | Done states. |
| `signalWarning` | `#A86400` | `#F0B34A` | Needs attention. |

Type uses the system sans for everything you read or tap, mapped to SwiftUI text styles so it follows Dynamic Type. Instrument Serif is used only for:

| Style | Size / line height | Use |
| --- | --- | --- |
| `displayXL` | 44 / 46 | First-level screen titles (Search, Settings, Today on the unfolded home). |
| `capture` | 26–28 / 32–34 | Text in the composer; the italic placeholder "What's on your mind?". |
| `numeral` | 76 / 76, tabular | The recording timer. |
| `wordmark` | 22 | "Oh And" in the header. |

Serif sizes scale with Dynamic Type through `Font.custom(_:size:relativeTo:)`.

**Spacing** is a 4-point grid: 4, 8, 12, 16, 24, 32, 48. Side margins are 16 on compact width and 24 on regular width.

**Radius:**
- chips and icon tiles 10;
- fields and inner controls 16;
- cards 24;
- the composer and sheets 32;
- buttons and floating controls are pills.

Corners are concentric: a control inset 8 inside a 24 card uses 16.

**Elevation:** floating controls use `0 8 32` at 14% ink in light and 50% black in dark. Cards use `0 1 2` at 6% ink in light and no shadow in dark, where surface steps do the work. Touch targets are at least 44 × 44 pt.

## Components

Each is one reusable SwiftUI view or style in `app/design-system/`.

- **PillButton**, in three styles:
  - primary: `accent` fill, `onAccent` text, height 48–50;
  - secondary: `tint` fill, `ink` text, height 44;
  - plain: text only, in `accent` or `destructive`.

  Disabled primary buttons use `surface2` with `inkFaint`.
- **GlassButton**: a 44 pt circle on glass, with an SF Symbol in `ink`. Used for search, settings, back and info.
- **GlassPill**: a glass capsule holding the header wordmark or the search field.
- **Card**: `surface1`, radius 24, card shadow, padding 14–18.
- **GroupedSection**: a card of rows. A row is at least 52 pt high and holds:
  - an optional 32 pt icon tile (`tint` fill, `accent` glyph);
  - a label;
  - an optional value in `inkMuted`;
  - a chevron in `inkFaint`.

  There are no full-width dividers; the section title (15 semibold) sits above the card and one footer line (13, `inkMuted`) below it.
- **ChoiceRow**: a row with an `accent` checkmark when selected, exposed as a radio button to accessibility.
- **ToggleRow**: the system toggle tinted `accent`.
- **Chip**: a 34 pt capsule. Selected is `accent` fill with `onAccent` text; unselected is `surface2` with `ink` text.
- **SavedPill**: a `tint` capsule holding an `accent` check disc and "Saved", with the floating shadow. It shows for about 2 s, then fades.
- **Composer**: a glass capsule, radius 32. At rest it holds the italic serif placeholder and a 48 pt `accent` mic button. While text is entered it grows with its content (up to about 40% of the visible height) and shows a `tint` mic button and a primary Save. While recording it becomes the recording panel:
  - the live dot and "Recording";
  - the serif timer;
  - level bars in `accent`;
  - Discard (`destructive`, plain);
  - a 72 pt `signalRecord` stop button.
- **Mark**: the italic & glyph rendered from the bundled font, sized by a parameter.

## Screens

### Home (the capture surface)

There's one screen, compact and regular.

- **Header:** three floating glass controls.
  - Search, a GlassButton. On regular width it's a wide GlassPill instead.
  - The wordmark, a GlassPill holding the Mark and "Oh And".
  - Settings, a GlassButton. While the keyboard is up, this becomes Done, which dismisses the keyboard.
- **Body:** recent captures as Cards, newest first, grouped under "Today" and "Earlier" (13 semibold, `inkMuted`). Each card shows a small type dot, the type and a relative date, then the item's text. Tapping a card opens that item's source detail (added 2026-10-20, F32). On regular width the cards form two columns under a `displayXL` "Today".
- **Composer** at the bottom, above the safe area or the keyboard. It is never covered.
- **Saving:** Save clears the composer, keeps the keyboard up and shows the SavedPill. The new item appears at the top of the list as "Just now · sorting…" until it's interpreted.

**Recents are a private read.** The capture surface must work in `capture_only`, so the body has three states:

1. **Unlocked:** the recent captures, as above.
2. **Unlocking:** placeholder cards while a quiet Face ID attempt is in flight. The composer is already live.
3. **Locked:** one card with a lock tile, "Your captures are locked", "You can still save new ones. Unlock to see what you saved before." and a primary "Unlock" button. Unlock runs the normal authentication (biometrics, passcode fallback).

Nothing derived from stored history appears in the locked or unlocking states: no counts, no snippets, no "3 new" badges.

### Launch-time unlock

When the app launches, and each time it returns to the foreground, the app makes one **quiet** authentication attempt:
- it uses the biometrics-only policy, with no passcode fallback button (`localizedFallbackTitle = ""`);
- it never blocks the composer;
- it never prompts when biometrics are unavailable, not enrolled or locked out.

Success moves the session to `authenticated` exactly as the existing flow does, and the home shows recents. Failure, cancellation or unavailability leaves `capture_only` and shows the locked card. The read gate, relock policy and every gated read are unchanged; this only changes when authentication is attempted.

### Search

- **Header:** a glass back button and a GlassPill search field, focused on entry.
- **Filters:** a row of Chips (type, time, topic). Each opens a menu, and the active filters show as selected chips. The existing filter capabilities are kept; they're shown as chips instead of a form.
- **Results:** "N results" (13 semibold, `inkMuted`), then result Cards with the type dot, type and date, and the text with the matched words highlighted in `tint`.
- **Empty states:** use the empty-state pattern (the Mark at large size in a faint tint, a serif line, one muted line, an optional secondary button).
- **Regular width:** results on the left, about 274 pt wide, with the selected one outlined in `accent`. The item detail is on the right as a card: a type chip and date, the item text in serif 30, "What you wrote" with its source in a `surface2` panel, then Mark done (primary), Edit (secondary) and Delete (`destructive` plain).

### Settings

- **Layout:** a glass back button, then a `displayXL` "Settings".
- **First GroupedSection:** Daily prompt (value On/Off) and Unfinished recordings (value: a count or None).
- **Second GroupedSection:** Processing (value e.g. "On this phone"), Privacy (the per-route grants of what may leave the device; its value is set by U06b), Audio (value e.g. "7 days") and Export.
- **Erase everything:** its own card, centred, `destructive`.
- **Version:** the version line at the bottom.

Each settings screen has a glass back button and an info GlassButton (top right) that opens a sheet with the full explanation. The screen itself carries a `displayXL` title, one GroupedSection per decision and one footer line.

- **Audio:** "Keep recordings after they become text" with ChoiceRows. Footer: "The text is always kept. A recording with no text yet is never removed." Then "Waiting for text", with rows or None.
- **Processing:** "Sort my captures" with ChoiceRows, each with a one-line detail:
  - "On this phone" (Nothing leaves the device);
  - "With a cloud provider";
  - "With my own server".

  Footer: "Applies to new captures. What you saved is never changed." Below it, a secondary "Add provider" button and a plain "Advanced" link to the profile, model, timeout and routing controls.
- **Daily prompt:** a ToggleRow and a time row. Footer: "A gentle nudge to look at one thing you saved. Never shown on the lock screen." Then a preview of the notification.

### Assets

- **App icon:**
  - default: the white mark on `brand`, with a soft top light;
  - dark variant: the mark in `accent` dark on `#0A0B12`;
  - tinted variant: one luminance shape.

  The mark is centred and about 60% of the icon height. It's generated from the bundled font's glyph outline, not drawn by hand.
- **Launch screen:** `UILaunchScreen` with the background colour `brand` (light) or `#0A0B12` (dark) and the mark image centred. There's no text.
- **Notifications:** use the app icon; prompts and reminders show no item content on the lock screen.

## Copy

- Write from the user's side, plainly and briefly. "Saved", not "Capture persisted". "Nothing leaves the device", not "local-only processing route".
- Use at most one footer line per section. Guarantees the user doesn't need to act on go in the info sheet.
- Never use status paragraphs.

## Sizes and layouts

Use size classes; never check the device model. The maintainer uses an iPhone Duo:
- the outer 5.4″ display is compact width, about 466 × 678 pt;
- the inner 7.6″ display unfolded is regular width, about 626 × 890 pt.

These point sizes assume 3× scale and must be confirmed in the Xcode simulator. Every screen must also work on a standard iPhone at compact width, at the largest Dynamic Type sizes, and in both appearances.

## Review checklist for UI tasks

A UI task built after DS01 lands is reviewed against this list as well as its own criteria:

1. It uses `app/design-system` tokens and components. A UI file contains no literal colour, font size outside the type styles, or radius.
2. Both appearances are correct, and text meets 4.5:1, or 3:1 for 24 pt and larger.
3. It works at compact and regular width and at the largest accessibility text size, with nothing clipped or overlapping.
4. Copy follows the Copy rules; there's at most one footer line per section.
5. Nothing derived from stored history appears in `capture_only`.
6. Once DS08 has landed, the exact-head CI run's walkthrough screenshots of the changed screens are linked in the PR.
