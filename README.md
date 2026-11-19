# Glide

A fluid island at the top of your Windows screen. It rests as a thin lip on
the screen edge, pours out into a strip when something is worth a glance, and
opens into a panel when you click it. Written in Rust on Direct2D; it does
nothing at all while nothing is happening.

## What it shows

| | |
| --- | --- |
| **Now playing** | Whatever any app reports to Windows (Spotify, browsers, players): artwork, title, timeline, previous / play / next. |
| **Calendar** | Today, the month, and your region's public holidays. No accounts, no events. |
| **Weather** | Today's forecast from Open-Meteo, for your Windows location or a city you set. |
| **Timer, stopwatch, alarm** | Focus presets (5 / 15 / 25 / 45 min), a stopwatch, one daily alarm. |
| **Shelf** | Drop files or text on the island, or copy something; drag it back out or click to copy. Kept in memory only, forgotten after 20 minutes. |
| **Battery** | Level, charging, time left; announces plugging in and low battery. |
| **Notifications** | The app's icon, who it is from, and the start of the message. A WhatsApp call gets an Open button. |
| **Signals** | Volume changes, Caps Lock, a dot while the microphone or camera is in use, a Join button when you copy a Teams / Zoom / Meet link. |

## Using it

- **Point** at the lip at the top centre of the screen to see the strip.
- **Click** the pill to open the page. **Scroll** sideways (or with the wheel, or click the dots) to change mode: one swipe, one mode. It works on the pill too, and the last mode you used is the one that comes back.
- While music plays and no window is under the island, the pill stays out with a live waveform; under a window it tucks back to the lip.
- **Move away** and it folds back.
- **Tray icon** (right-click): light / dark / match Windows, seven accent colours (each with its own shade for light and dark), start with Windows, settings, quit.

Weather city and holiday region: tray icon, *Edit Settings*, set `"city": "Chennai"`
(and, if you want a different country's holidays, `"region": "US"`), save, then
*Apply Settings and Refresh Weather*.

## Install

Run `Glide-Setup-<version>-x64.exe`, or unzip the portable build anywhere and
run `glide.exe`. Windows 10 (2004) or later, 64-bit. Nothing else to install.

Glide is not signed with a paid certificate, so the first run shows the Windows
SmartScreen "unknown publisher" prompt: choose **More info**, then **Run anyway**.

To remove it: *Apps > Installed apps > Glide > Uninstall* (or delete the
portable folder). Settings live in `%APPDATA%\Glide`.

## Privacy

- Glide talks to the internet for two things only: the forecast
  (`api.open-meteo.com`, sending your coordinates or city) and public holidays
  (`date.nager.at`, or Google's public holiday calendar for regions it lacks).
- Copied text on the shelf stays in memory. Anything a password manager marks
  as private is never recorded.
- To notice Caps Lock, Glide asks Windows to tell it when a key is pressed. It
  looks only at the Caps Lock state and never reads which keys were typed.

## Building

```
cargo build --release
"%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" packaging\glide.iss
```

Rust stable (1.82 or later) with the MSVC toolchain. The only dependencies are
the `windows` crates and `serde`; everything is open source.

`GLIDE_TRACE=<file>` appends a line per state change, for diagnosing behaviour.

## Instant notifications

Windows only tells an app about a notification the moment it arrives if the app has a package identity. Glide ships one (`identity\Glide.msix`, signed with `identity\Glide.cer`). Because the certificate is Glide's own and not from a public authority, each PC has to be told to trust it, which takes one administrator approval:

- **Installer:** tick *Show notifications the moment they arrive*.
- **Later, or portable:** run `identity\register-identity.ps1`.

Without it everything else works, and notifications still appear: Glide notices a toast window and otherwise checks the list every eight seconds, so one can be a few seconds late. `"notifications": false` in settings turns the feature off.

Trusting the certificate means this PC will accept packages signed with it. Only install Glide from someone you trust.

## How it stays light

- Sources are event-driven. Two exceptions, both slow: the notification list (every eight seconds, see above) and the playback position (every three seconds, only while something plays).
- No frame is drawn unless something is moving. At rest the process sleeps in
  `GetMessage`. Measured on the development PC: about 0.5% of one core idle
  (all of it the notification check; 0% with notifications off), the same with
  music playing under a window, and about 5% of one core while the live
  waveform is on screen. About 24 MB of private memory; the exe is 0.6 MB.
- The window is only as large as the shape and never takes keyboard focus.

## Layout

| Path | Role |
| --- | --- |
| `src/shell/window.rs` | Host window and message loop |
| `src/shell/render.rs` | Direct2D on a DirectComposition surface |
| `src/shell/shape.rs`, `spring.rs` | The island's outline and the springs that move it |
| `src/shell/ui.rs` | Small immediate-mode toolkit with hover/press springs |
| `src/shell/ole.rs` | Clipboard and drag-and-drop |
| `src/island.rs` | States, activity priority, events |
| `src/modules/` | One file per source |

## Credits

Interface type is Segoe UI Variable and icons are Segoe Fluent Icons, both part
of Windows. Weather data by [Open-Meteo](https://open-meteo.com/) (CC BY 4.0).
Holiday data by [Nager.Date](https://date.nager.at/). The shape is inspired by
Apple's Dynamic Island and by the edge player in
[Noir](https://github.com/MaximkaCoder/Noir).
