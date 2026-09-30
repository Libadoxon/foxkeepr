# foxkeepr

Remembers which Hyprland workspace each Firefox window sits on, and puts the
windows back where they were after Firefox crashes or the machine reboots.

Moving windows around by hand keeps working: foxkeepr only ever moves a window
at the moment it recognizes it as one restored from a previous session. Every
move you make after that is simply recorded.

## How it decides where a window belongs

Window addresses do not survive a restart, so a restored window is recognised
by its **title**. Firefox's session restore brings each window back with the
tab it had, so the title usually matches. Windows whose titles have changed are
matched by **position** instead: the leftover window that opened earliest takes
the leftover record that was created earliest, and so on. Pass
`--no-order-fallback` to match only on exact titles.

Restoring is armed only when the browser is *absent*. Opening an extra window
during normal use therefore never triggers a move.

The layout is only written to disk once the set of windows has held still for a
few seconds, and never while no windows are open.

## Usage

```
foxkeepr run        # watch Hyprland and restore windows as they reappear
foxkeepr status     # print the layout currently on record
foxkeepr restore    # match the windows open right now and move them
```

The layout lives at `$XDG_STATE_HOME/foxkeepr/session.json`

Useful flags — see `foxkeepr --help` for the rest:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--class CLASS` | firefox, floorp, librewolf, waterfox, zen, … | Window class to treat as the browser. Repeatable; replaces the defaults. |
| `--grace SECS` | 60 | How long after the first restored window matching stays open. |
| `--settle SECS` | 8 | How long to let a title settle before matching by position. |
| `--stability SECS` | 5 | How long the window set must hold still before the layout is saved. |
| `--no-order-fallback` | off | Only ever match by exact title. |
| `--verbose` | off | Log every event and decision. |

Run it with a throwaway class to watch it work without touching your browser:

```sh
foxkeepr run --class kitty --state /tmp/probe.json --verbose
```

## Logging
`--verbose` is shorthand for `debug`. `RUST_LOG` overrides it and takes the
usual filter syntax:

```sh
RUST_LOG=foxkeepr=trace foxkeepr run
```

## Running it as a service

### NixOs (home-manager)

```nix
systemd.user.services.foxkeepr = {
  Unit = {
    Description = "Keep Firefox windows on their Hyprland workspaces";
    PartOf = [ config.wayland.systemd.target ];
    After = [ config.wayland.systemd.target ];
  };
  Service = {
    ExecStart = "${pkgs.foxkeepr}/bin/foxkeepr run";
    Restart = "always";
    RestartSec = 2;
  };
  Install.WantedBy = [ config.wayland.systemd.target ];
};
```

### Other Distros (systemd based)

Copy `systemd/foxkeepr.service` to `~/.config/systemd/user/`, set
`ExecStart` to the installed binary, and enable it:

```sh
systemctl --user enable --now foxkeepr
```

The daemon exits when Hyprland closes the event socket, so `Restart=always`
pairs it with the compositor's lifetime.

## Note on Hyprland 0.56+

Dispatches go over the control socket in the Lua form Hyprland 0.56 expects:

```lua
hl.dsp.window.move({ workspace = 4, window = 'address:0x…', follow = false })
```

Older Hyprland versions aren't supported

## Disclaimer on AI usage
This project has been largely written by Claude Code. Every line of code was audited and approved by a human.

## License
Everything under GPL-3.0-or-later, see [LICENSE](./LICENSE) for details
