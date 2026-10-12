# appkey

**One key to jump to any app.**

Press a shortcut, and the app you assigned to it comes to the front. If it isn't open yet, appkey starts it.
If it's already open, appkey never starts a second copy; it brings up the window you were using last.

> Example: press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>T</kbd> and your terminal appears, wherever you are.

appkey lives quietly in your menu bar / system tray, is about 3 MB, and works on **Windows**, **macOS** and **Linux**.

---

## Install

Download the file for your system from the [latest release](https://github.com/carevski/appkey/releases/latest).

### Windows

1. Download **`appkey-…-windows-x64-setup.exe`** and run it. No administrator rights are needed.
2. If Windows shows *"Windows protected your PC"*, click **More info → Run anyway**. (This appears because the app isn't code-signed yet.)
3. Tick **Start appkey when I sign in** if you want your shortcuts available all the time.

Prefer no installer? Download the `.zip`, unpack it anywhere and run `appkey.exe`.

### macOS

1. Download **`appkey-…-macos-universal.dmg`**, open it and drag **appkey** into **Applications**.
2. The first time you open it, macOS will say it can't verify the developer (the app isn't notarized yet).
   Go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway**.
3. A small key icon appears in the menu bar. To start appkey automatically, add it under
   **System Settings → General → Login Items**.

### Linux

| Your system | Download | Install with |
|---|---|---|
| Ubuntu 22.04+, Linux Mint 21+, Debian 12+, Pop!_OS | `.deb` | `sudo apt install ./appkey-*-linux-x86_64.deb` |
| Fedora, openSUSE, RHEL | `.rpm` | `sudo dnf install ./appkey-*-linux-x86_64.rpm` |
| Arch, or anything else | `.tar.gz` | unpack it and copy `appkey` into `~/.local/bin` |

ARM computers (e.g. Raspberry Pi) use the `-aarch64` files instead.

Then start **appkey** from your applications menu. To start it automatically, add it to your desktop's
*Startup Applications* (Mint: **Menu → Startup Applications → Add**).

> **Important:** on Linux, appkey currently works with **X11** desktops. That includes Linux Mint (Cinnamon, MATE, Xfce) and any desktop you start with an "Xorg" session.
> **Wayland** desktops are not supported yet. That includes recent Ubuntu, Fedora and KDE by default. On those, appkey tells you so instead of silently failing. Not sure which one you're using? Run `echo $XDG_SESSION_TYPE`.

---

## Set up your shortcuts

Click the appkey icon in the tray / menu bar and choose **Settings…**. The settings window also opens by itself the first time you start appkey.

1. Click **+ Add shortcut**, then **press the keys** you want, for example <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>T</kbd>. (Esc cancels.)
2. Click **Choose app…** and pick the application:
   - **Open now**: apps that are running at the moment
   - **Installed**: apps on your computer (type in the search box to filter)
   - **Pick a window…**: click it, then switch to the app you want within 3 seconds. appkey notes which app you switched to.
3. Click **Save**. The new shortcut works straight away; there's nothing to restart.

To change a shortcut, click its keys or its app. To delete one, click **Remove**. If something's wrong, the row says so (for example "Same shortcut as row 2") and **Save** stays disabled until it's fixed.

The tray menu shows how many shortcuts are active. If one couldn't be set up, it tells you why. Usually another app already uses that key combination.

### The config file

The settings window saves everything to a small text file. You don't need to touch it, but you can edit it by hand (tray menu → **Edit config file…**). appkey notices changes as soon as you save.

```toml
[[shortcut]]
keys = "ctrl+alt+t"
name = "Terminal"                # shown in the settings window
app = "com.apple.Terminal"

[[shortcut]]
keys = "ctrl+alt+b"
name = "Firefox"
app = "firefox"
launch = ["firefox", "--new-window"]
os = ["linux"]
```

| Setting | Required | What it means |
|---|---|---|
| `keys` | yes | Modifiers joined with `+`, then a key: `ctrl`, `alt` (or `option`), `shift`, `super` (also `cmd` / the Windows key); keys `a`–`z`, `0`–`9`, `F1`–`F24`, `space`, `enter`, … |
| `app` | yes | How appkey recognises the app when it's already open (see below). |
| `name` | no | A friendly name for the settings window. |
| `launch` | no | How to start the app if it isn't open: `["program", "argument", …]`. If you leave it out, appkey uses `app`. |
| `os` | no | Only use this shortcut on some systems, e.g. `["linux"]`. Shortcuts made in the settings window are limited to the system you made them on, so one config can be shared between computers. |

Saving from the settings window rewrites the file, so any comments you added by hand are lost.

### Finding the right `app` name

The settings window fills this in for you. For hand edits, open the app and run `appkey list`.

| System | `app` is… | Examples |
|---|---|---|
| macOS | the bundle id, or the app's name | `com.apple.Terminal`, `Safari` |
| Windows | the program file name | `WindowsTerminal.exe`, `chrome.exe` |
| Linux | the window class | `gnome-terminal`, `firefox`, `code` |

Upper/lower case doesn't matter, and on Windows the `.exe` is optional.

### Choosing good key combinations

A global shortcut works everywhere, so it **takes the combination away from every other app**.
Using <kbd>Ctrl</kbd>+<kbd>T</kbd>, for example, would stop "new tab" from working in your browser.
Combinations with two modifiers are the safest: <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+*letter*, or <kbd>Super</kbd>/<kbd>Cmd</kbd>+<kbd>Shift</kbd>+*letter*.

---

## Command line

appkey also has a few terminal commands. They're optional, but useful for setup and troubleshooting:

| Command | What it does |
|---|---|
| `appkey` | Start appkey in the tray (same as opening it from the menu) |
| `appkey settings` | Open the settings window |
| `appkey list` | Show open apps and the names to use for `app` |
| `appkey check` | Check your config for mistakes and show notes for your system |
| `appkey open firefox` | Do exactly what a shortcut for `firefox` would do, once |
| `appkey config` | Show where the config file is |

On Windows, tick **Add appkey to PATH** in the installer to use these from a terminal.

---

## Privacy & security

- appkey needs **no special permissions** on any system. It doesn't read your keystrokes: it asks the system to tell it when *your* shortcuts are pressed.
- It never runs commands through a shell. `launch` starts exactly the program you list.
- Your config decides which programs run, so on macOS and Linux appkey refuses to use it if other users on the computer could edit it.
- **Windows:** an app running *as administrator* can only be brought to the front if appkey also runs as administrator. appkey tells you when this happens rather than working around Windows' protection.

## Troubleshooting

- **A shortcut does nothing:** open the tray menu. The top line says how many shortcuts are active and lists any that failed. Run `appkey check` to validate the config.
- **"Shortcut unavailable":** another app already owns that combination. Pick a different one.
- **The wrong app opens, or a second copy starts:** the `app` name doesn't match. Run `appkey list` while the app is open and copy the name exactly.
- **Linux: "this is a Wayland session":** see the note under [Linux](#linux).
- **Linux: no tray icon on GNOME:** install the *AppIndicator and KStatusNotifierItem Support* extension (Ubuntu has it already). Shortcuts work without the icon, and you can open settings with `appkey settings`.
- **Linux: the settings window doesn't open:** it needs a graphics driver with OpenGL, which every normal desktop has. Run `appkey settings` in a terminal to see the error.

---

## For developers

```sh
cargo build --release   # → target/release/appkey (no system libraries needed)
cargo test
```

- How it works and why: [docs/DESIGN.md](docs/DESIGN.md)
- Conventions (also used by Claude Code): [CLAUDE.md](CLAUDE.md)
- Every push to `main` builds all installers and publishes a release automatically
  ([`.github/workflows/release.yml`](.github/workflows/release.yml)). The version is `MAJOR.MINOR` from `Cargo.toml` plus a build number. Bump `MAJOR.MINOR` in `Cargo.toml` by hand.

## License

[MIT](LICENSE)
