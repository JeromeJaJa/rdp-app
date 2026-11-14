# Changelog

What changed in each release, newest first. From 0.3.1 on, each release's list is generated from its
commits (`bun scripts/release-notes.mjs`); versions follow [Semantic Versioning](https://semver.org/)
(pre-1.0, so a minor version can change behavior).

## [0.5.0] - 2026-11-14

### Features

- **frames:** pages pull frames where there are no shared buffers [`c23ce4c`](https://github.com/JeromeJaJa/rdp-app/commit/c23ce4c)
- **mac:** saved passwords are encrypted with a key in the login Keychain [`96aad9e`](https://github.com/JeromeJaJa/rdp-app/commit/96aad9e)
- **mac:** Mac windows keep their traffic lights, and shortcuts show ⌘ [`c48a2dc`](https://github.com/JeromeJaJa/rdp-app/commit/c48a2dc)
- **mac:** Cmd works as Ctrl in a session, without stuck keys [`aedb7eb`](https://github.com/JeromeJaJa/rdp-app/commit/aedb7eb)
- **mac:** copy and paste text between the Mac and sessions [`e5126cc`](https://github.com/JeromeJaJa/rdp-app/commit/e5126cc)
- **mac:** drop files from Finder onto a session [`b3c6a6a`](https://github.com/JeromeJaJa/rdp-app/commit/b3c6a6a)
- **mac:** the acrylic window material works on a Mac [`1a71e97`](https://github.com/JeromeJaJa/rdp-app/commit/1a71e97)

### Fixes

- **mac:** links open in the browser [`546412c`](https://github.com/JeromeJaJa/rdp-app/commit/546412c)
- **mac:** installing the companion says it needs Windows, instead of a missing file [`69c3b7c`](https://github.com/JeromeJaJa/rdp-app/commit/69c3b7c)
- **mac:** the password field says it's kept in the Keychain [`32f734e`](https://github.com/JeromeJaJa/rdp-app/commit/32f734e)
- **mac:** no Keychain prompt at startup or after updates [`ab84a43`](https://github.com/JeromeJaJa/rdp-app/commit/ab84a43)
- **ui:** switching views while the window is hidden no longer throws [`995ec79`](https://github.com/JeromeJaJa/rdp-app/commit/995ec79)
- **ui:** the eye button shows a connection's saved password [`d8cd939`](https://github.com/JeromeJaJa/rdp-app/commit/d8cd939)

### Other changes

- **mac:** no warnings outside Windows [`72e8193`](https://github.com/JeromeJaJa/rdp-app/commit/72e8193)
- check the Mac build too [`988d457`](https://github.com/JeromeJaJa/rdp-app/commit/988d457)
- **api:** dev builds accept a token from RDPAPP_DEV_API_TOKEN [`62ff8c2`](https://github.com/JeromeJaJa/rdp-app/commit/62ff8c2)
- **mac:** bundle the app and a disk image, ad-hoc signed [`0359630`](https://github.com/JeromeJaJa/rdp-app/commit/0359630)
- **release:** build for Mac too, and update Mac apps from the same feed [`7a01ba5`](https://github.com/JeromeJaJa/rdp-app/commit/7a01ba5)
- **engine:** the drive-sharing tests run on Windows only [`10a5109`](https://github.com/JeromeJaJa/rdp-app/commit/10a5109)
- **mac:** the Windows-only crates take no dependencies elsewhere [`557d63f`](https://github.com/JeromeJaJa/rdp-app/commit/557d63f)
- **engine:** the drive-sharing tests build on Windows only [`38092a3`](https://github.com/JeromeJaJa/rdp-app/commit/38092a3)
- the Mac app, what it does, and how it differs [`c17157c`](https://github.com/JeromeJaJa/rdp-app/commit/c17157c)
- **mac:** tests never touch the Keychain [`7ac58fa`](https://github.com/JeromeJaJa/rdp-app/commit/7ac58fa)
- **companion:** the streaming speed test passes on slower CI runners [`ab05fa2`](https://github.com/JeromeJaJa/rdp-app/commit/ab05fa2)
- **companion:** the streaming speed test measures against the machine's own speed [`db036cc`](https://github.com/JeromeJaJa/rdp-app/commit/db036cc)
- releases build and check the Mac app too [`b768266`](https://github.com/JeromeJaJa/rdp-app/commit/b768266)

## [0.4.0] - 2026-11-06

### Features

- **grid:** fold groups, and full buttons for a group's controls [`fa08044`](https://github.com/JeromeJaJa/rdp-app/commit/fa08044)
- **ui:** Ctrl+click skips confirmations [`bbe044c`](https://github.com/JeromeJaJa/rdp-app/commit/bbe044c)
- **updates:** the app updates itself, and shows its version and what's new [`3d8856f`](https://github.com/JeromeJaJa/rdp-app/commit/3d8856f)
- **updates:** download only when updating, so it's always the newest [`0861ceb`](https://github.com/JeromeJaJa/rdp-app/commit/0861ceb)
- **updates:** hovering the Update pill shows what changed [`8b42a17`](https://github.com/JeromeJaJa/rdp-app/commit/8b42a17)

### Fixes

- **deps:** update IronRDP, so drive redirection survives replies over 1600 bytes [`c4d379c`](https://github.com/JeromeJaJa/rdp-app/commit/c4d379c)
- **clipboard:** many sessions no longer freeze the app [`97665d9`](https://github.com/JeromeJaJa/rdp-app/commit/97665d9)
- **grid:** a connecting tile keeps its status on hover [`1d811c2`](https://github.com/JeromeJaJa/rdp-app/commit/1d811c2)
- **companion:** program controls and sends give up at once when the install failed [`fa3da51`](https://github.com/JeromeJaJa/rdp-app/commit/fa3da51)

### Other changes

- **changelog:** an absolute link, so it works in release notes too [`0224c24`](https://github.com/JeromeJaJa/rdp-app/commit/0224c24)
- the clipboard's threading and on-screen offers, and an Unreleased changelog [`c3747b3`](https://github.com/JeromeJaJa/rdp-app/commit/c3747b3)
- check on releases and pull requests, not every push; notes from commits [`3a0d88d`](https://github.com/JeromeJaJa/rdp-app/commit/3a0d88d)
- **release:** sign the installer and publish the update feed [`dedf821`](https://github.com/JeromeJaJa/rdp-app/commit/dedf821)
- self-updating, and the release signing key [`c4587dd`](https://github.com/JeromeJaJa/rdp-app/commit/c4587dd)
- **release:** each change in the notes ends with its commit, not in parentheses [`77fbf80`](https://github.com/JeromeJaJa/rdp-app/commit/77fbf80)

## [0.3.0] - 2026-10-27

### Added

- **Group controls in the grid.** Each group's heading in the grid has Connect all, Disconnect all and the group menu,
  like the sidebar.
- **Close tabs to the right.** Right-click a tab in a window's tab strip to close every tab after it.
- **Remote wallpaper setting** (Settings → Graphics, overridable per group and connection). Turn it off to give
  sessions a plain background, which is less for each host to send. It's on by default, and the host's own wallpaper
  setting is never changed. Applies from the next connect.
- A README with a screenshot, a [how it works](https://github.com/JeromeJaJa/rdp-app/blob/main/docs/ARCHITECTURE.md) guide, and an MIT license.

### Changed

- **Releases ship only the installer.** The bare exe and the portable zip are gone: the installer is the one download
  that always puts the companion next to the app. A bare exe ran, but could never install the companion.

## [0.2.0] - 2026-10-21

### Added

- **The companion** (experimental, off by default): a small helper in each remote session, installed over WinRM.
  - Fast copies: copy files, text or anything else on one side and paste on the other, streamed and compressed.
  - A direct, encrypted link beside the session for its traffic (set up like Tailscale, no open ports), since the
    remote desktop connection caps uploads near 1 MB/s on a distant host. Falls back to RDP if it drops.
  - Drag and drop files between this PC and a session, both ways.
  - Send files to one connection, a group or a selection.
  - Host stats: one labelled stat on sidebar rows and grid tiles, and a card with recent history when you hover a
    tab.
  - Program controls: launch, restart or close a program on one or many hosts.
  - Commands across many hosts show as one row that fills in, not a toast per host.
- Ctrl/Shift+click to pick several connections in the sidebar, then right-click to act on all of them.
- The connection form puts its settings beside the fields, with the group first.
- The window that files are dropped on comes to the front.

### Fixed

- A drag out of a session always ends, so other apps can drag again without restarting the app.
- An ended session's clipboard no longer freezes the window.
- Sessions resize when a window drag is let go, not mid-drag.
- Submenus stay open while the pointer moves to them.
- Tab names, the address and the stat line up at every size.
- The Windows key stays local outside fullscreen.
- A session moved back to the main window keeps painting.

## [0.1.0] - 2026-10-21

The first release.

- Many sessions at once: each on its own thread, with sessions you aren't looking at paused on the server.
- H.264 (AVC420 and AVC444) decoded on the GPU or in software, plus RemoteFX, ClearCodec and Planar.
- Tabs on top or on the left, a live grid of every session, and tabs torn off into windows of their own.
- Groups, bulk add from a pasted list, and import / export.
- Clipboard (text, images and files), audio, microphone, drive / printer / smart card redirection, dynamic
  resolution and auto-reconnect.
- Server certificates pinned on first use.
- A local REST API for scripts and agents.
- Passwords encrypted with Windows DPAPI.

[0.5.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/JeromeJaJa/rdp-app/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JeromeJaJa/rdp-app/releases/tag/v0.1.0
