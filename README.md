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

#### Which Linux desktops work

| Desktop | Works | One-time setup |
|---|---|---|
| Any **X11** desktop: Linux Mint (Cinnamon, MATE, Xfce), or any "Xorg" session | ✅ | none |
| **GNOME** on Wayland: Ubuntu, Fedora Workstation, Debian | ✅ | **log out and back in once** (see below) |
| **KDE Plasma** on Wayland | ✅ | approve the shortcuts when KDE asks |
| **Hyprland** | ✅ | none |
| Other Wayland desktops (Sway, Cinnamon on Wayland, COSMIC, …) | ❌ not yet | appkey tells you so; use an X11 session for now |

Not sure which one you're using? Run `echo $XDG_SESSION_TYPE $XDG_CURRENT_DESKTOP`.

**GNOME (Ubuntu):** GNOME doesn't let apps switch to other apps' windows or register global shortcuts. appkey therefore comes with a tiny **GNOME Shell extension**, "appkey helper", that does only that, and only when appkey asks. The first time appkey starts, it installs and turns on the helper. GNOME only loads new extensions when you log in, so **log out and back in once**. Until then, the tray menu says so. You can see the helper, or turn it off, in the **Extensions** app.

**KDE Plasma:** the first time appkey sets up your shortcuts, and whenever you add new ones, KDE shows a "Global Shortcuts Requested" window. Click **OK**. The shortcuts also show up in System Settings → Shortcuts, under "appkey".

---

## Set up your shortcuts

Click the appkey icon in the tray / menu bar to open the settings window. Right-click it for the menu (status, **Edit config file…**, **Quit**). The settings window also opens by itself the first time you start appkey.

appkey's windows follow your system's light or dark theme. To choose one yourself, click **Light** or **Dark** at the top right of the settings window (**System** goes back to following the system). The choice is saved straight away.

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

The top of the file may also have `theme = "light"` or `theme = "dark"`, the colours of appkey's own windows. Leave it out to follow the system.

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

- **The Windows / Super key** works as a modifier, e.g. <kbd>Super</kbd>+<kbd>T</kbd>. Your desktop already uses some of these combinations (Windows: <kbd>Win</kbd>+<kbd>E</kbd>, <kbd>Win</kbd>+<kbd>L</kbd>, …; Linux Mint: <kbd>Super</kbd>+<kbd>L</kbd>, <kbd>Super</kbd>+<kbd>D</kbd>, …). If one is taken, the tray menu says it's unavailable. On **Linux Mint (Cinnamon)**, Cinnamon keeps the Super key to itself for its menu, so appkey adds Super shortcuts to Cinnamon's own keyboard shortcuts instead. You'll see them as "appkey shortcut 1, 2, …" in System Settings → Keyboard → Shortcuts → Custom Shortcuts. appkey keeps them up to date and removes them when it quits; change them in appkey's settings, not there.
- **Combinations your desktop already uses (Linux Mint/Cinnamon, GNOME/Ubuntu, KDE Plasma)** are flagged in the settings window, e.g. "Used by GNOME: Screensaver", "Used by KDE: Peek at Desktop" or "Used by Cinnamon: your shortcut "Terminal"". This covers the desktop's built-in shortcuts, the ones you added yourself in its keyboard settings (on KDE also shortcuts other apps registered), and on Ubuntu the dock's <kbd>Super</kbd>+number keys. You can still save, but the tray menu lists the shortcut as unavailable until you pick another combination or free it in the desktop's keyboard settings. If you press such a combination while recording, the desktop acts on it (for example locks the screen) and nothing is recorded, because the desktop handles it before appkey sees it.
- **F1–F24** can be used on their own, without a modifier. On laptops where the top row controls brightness and volume, hold <kbd>Fn</kbd> while recording so the key sends F5 and not "brightness up".
- **The Fn key itself** can't be part of a shortcut. The keyboard handles it internally and never tells the computer it was pressed, so no app can see it.

---

## Low memory and frozen desktops (Ubuntu, Linux Mint)

On Ubuntu, Linux Mint and other Ubuntu-based systems, the settings window has a **Memory** tab with two tools.

**The memory window** shows what's running, grouped by app, so you can tell quickly what is what and close it:

- **One row per app, with everything it started** underneath (rows open expanded; click ▾ to fold one): Chrome with its tabs, GPU and network processes; IntelliJ IDEA (or PyCharm, WebStorm…) with its Kotlin daemon; a terminal tab with what runs in it, e.g. *Terminal: Claude Code* with its dev server.
- **Names you recognise**: apps are labelled with their window titles (Chrome's open pages, IntelliJ's projects), programs without a window with the folder they run in. Hover a row for the full command, folder and process id.
- **Memory, CPU and how long it has been running**, sortable by memory or CPU.
- **Left running in the background**, listed first: programs with no window whose terminal, Claude session or IDE has already closed (old dev servers, Gradle daemons, scripts stuck in a loop). **Close all of these** clears them in one go.
- **Show** brings an app's window to the front, so you can check before closing. **Close** asks a whole app to quit; **End** stops one process (for a Chrome tab: that tab shows "Aw, Snap!" and can be reloaded; which tab is which is only visible in Chrome's own Task Manager, <kbd>Shift</kbd>+<kbd>Esc</kbd>). If something doesn't quit within 5 seconds, **Force quit** appears.
- The list holds still while the mouse pointer is in the window, so a click always hits what you aimed at.

**Uninstall an app you no longer need**, without remembering how you installed it: click **Uninstall…** on its row. appkey works out how it was installed and removes it the matching way:

| Installed as | Removed with |
|---|---|
| Snap | `snap remove` (asks for your password). If snap stops because the app's data folder `~/snap/<name>` wasn't empty, appkey offers to move that folder to the Trash and finish |
| Flatpak (e.g. from Software Manager / Flathub) | `flatpak uninstall` (asks for your password if it was installed for all users) |
| .deb (apt, Software Manager, a downloaded .deb) | `apt-get remove`, after a dry run that shows every package that would go (asks for your password; your settings are kept) |
| AppImage | the .AppImage file and its menu entry are moved to the **Trash**, so you can restore them |
| JetBrains Toolbox IDE | not automatic: appkey tells you to remove it in Toolbox |
| anything else | appkey says it can't tell and shows where the program is |

You always see what will be removed and the exact command first. appkey closes the app, then removes it. It refuses when an apt removal would take part of the desktop or the system with it (for example removing Nemo would also remove Cinnamon), or more than 15 packages: do those in your package manager if you're sure.

Open it with **Show memory use now…**, `appkey memory`, or **its own shortcut**: in Shortcuts, add one and choose app → *appkey* → **Memory window**. appkey only offers your own programs, never parts of the desktop itself.

**Low-memory warning.** Tick *Warn me before the computer runs out of memory* and click **Save**. When free memory stays below the limit you set (10% by default), or programs keep waiting on memory (20% of the time by default), the memory window opens by itself. **Snooze for an hour** keeps the warning away for an hour; **Dismiss** for 5 minutes.

**Busy background programs.** Tick *Also warn when a program left running in the background keeps the CPU busy* and pick how long (10 minutes by default). If such a program uses a whole CPU core for that long, the memory window opens with it at the top.

**Emergency keys (Magic SysRq).** If the computer freezes before you can react, these keys still work because the Linux kernel handles them, not the desktop. Hold <kbd>Alt</kbd> and <kbd>PrtSc</kbd> (SysRq), then press:

| Key | What it does |
|---|---|
| <kbd>F</kbd> | Closes the program using the most memory. Press once and wait a few seconds. |
| <kbd>R</kbd> <kbd>E</kbd> <kbd>I</kbd> <kbd>S</kbd> <kbd>U</kbd> <kbd>B</kbd> | Safe restart when nothing else helps. Press the letters slowly, about 2 seconds apart. |
| <kbd>H</kbd> | Test: only writes help to the kernel log (`sudo dmesg \| tail`). |

Ubuntu and Mint switch <kbd>F</kbd> off by default (setting 176). Choose **Recommended (240)** and click **Apply…**. Your system asks for your password, possibly twice: once to save the setting to `/etc/sysctl.d/90-appkey-sysrq.conf` and once to apply it now. Prefer a terminal? The tab shows the equivalent command to copy. It's off by default because on shared computers someone at a locked screen could use it to close the lock screen, and because <kbd>Alt</kbd>+<kbd>PrtSc</kbd> is also the window-screenshot shortcut. On a computer only you use, 240 is a sensible middle ground.

In the config file, the warning looks like this:

```toml
[memory_watch]
enabled = true
min_available_percent = 10   # warn below this much free memory (2–50)
max_pressure_percent = 20    # …or when programs wait on memory this often (0 = never)
busy_background_minutes = 10 # warn about busy background programs after this long (0 = off)
```

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
| `appkey memory` | Show what's running, grouped by app, and close it (Ubuntu, Mint) |
| `appkey config` | Show where the config file is |

On Windows, tick **Add appkey to PATH** in the installer to use these from a terminal.

---

## Privacy & security

- appkey needs **no special permissions** on any system. It doesn't read your keystrokes: it asks the system to tell it when *your* shortcuts are pressed.
- It never runs commands through a shell. `launch` starts exactly the program you list.
- **Linux:** the memory window can only close your own programs, and it never runs as administrator. Changing the SysRq setting and uninstalling a snap or .deb are the only things that need administrator rights: your system's own password prompt asks for it, and only for writing that one setting.
- Your config decides which programs run, so on macOS and Linux appkey refuses to use it if other users on the computer could edit it.
- **Windows:** an app running *as administrator* can only be brought to the front if appkey also runs as administrator. appkey tells you when this happens rather than working around Windows' protection.

## Troubleshooting

- **A shortcut does nothing:** open the tray menu. The top line says how many shortcuts are active and lists any that failed. Run `appkey check` to validate the config.
- **"Shortcut unavailable":** another app already owns that combination. Pick a different one.
- **The wrong app opens, or a second copy starts:** the `app` name doesn't match. Run `appkey list` while the app is open and copy the name exactly.
- **Linux: "this Wayland desktop isn't supported yet":** see [Which Linux desktops work](#which-linux-desktops-work).
- **GNOME: "appkey's GNOME helper isn't running yet":** log out and back in. If it still says so, open the **Extensions** app and make sure "appkey helper" is on (and that extensions as a whole are turned on).
- **KDE: shortcuts do nothing:** you probably clicked **Cancel** in the "Global Shortcuts Requested" window. KDE remembers that and doesn't ask again, so assign the keys yourself in **System Settings → Shortcuts → appkey**. The tray menu points there too.
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
