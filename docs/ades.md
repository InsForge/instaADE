# How InstaADE works with each ADE

The README's [Supported ADEs](../README.md#supported-ades) says what you need and what to click. This page says what InstaADE does on the sandbox and on your computer for each ADE. To add an ADE, see [Adding an ADE](../DEVELOPMENT.md#adding-an-ade).

## T3 Code

InstaADE installs T3 at the same version as your T3 Code desktop app, then pairs them. On the sandbox it adds:

- T3 under `/opt/t3`, linked as `/usr/local/bin/t3`
- A `t3-instabox` service that runs the T3 server on port 3774, which InstaADE publishes for pairing
- An activity probe in `/usr/local/lib/instabox/activity.d/t3`, so the sandbox sleeps only when T3 is idle
- A preload, `~/.local/share/t3-instabox/forwarded-host.cjs`, that the service loads into `t3 serve` through `NODE_OPTIONS`. The instabox port proxy sends `Host: 127.0.0.1:3774` and the published address in `X-Forwarded-Host`. T3 checks DPoP proofs (T3 Code 0.1.12 and later) against a URL built from `Host`, so for a request that came through the proxy the preload puts the published address back in `Host`. It acts only when `Host` is `127.0.0.1:3774` and `X-Forwarded-Host` is `3774-<id>.<domain>`, where `<id>` is this sandbox's id from `/etc/instabox/identity` without its `m_` prefix (any id on a sandbox without that file). T3's own child processes do not load it.

Every **Open in T3 Code** pairs again. It wakes the sandbox, updates T3 to the app's version when it is behind and idle, and pairs them with a new code. The sandbox keeps the same address, so T3 Code updates the environment it saved, or adds it again when it lost it. On a sandbox that has T3 but was never paired from this computer, Open first runs the same setup as **Install T3 Code**.

InstaADE never moves T3 on a sandbox back to an older version. When the sandbox runs a newer T3 than the app, InstaADE only pairs.

The setup writes `~/.local/share/t3-instabox/setup` with the id of the setup the running server was started with. A sync (Open, resume, a new T3 Code version) whose sandbox T3 is behind the app runs the whole setup at the app's version. One whose sandbox T3 is at or ahead of the app's version, but has an older id, keeps that version and brings only the probe, the preload and the unit up to date. Either way T3 restarts only while it is idle. When it is busy, nothing T3 uses changes and the next sync tries again.

An older setup refuses a DPoP pairing. T3 Code pairs with DPoP only when both the desktop app and the T3 on the sandbox are 0.1.11-nightly.20261118.4903 or later. Only then does a pairing wait for the setup. When the setup is older, **Open**, **Pair again** and **Install** first bring it up to date. When T3 is busy or that fails, they do not pair, and show "T3 is busy on this sandbox. Try again when the agent is idle." or the error. When either side is older, the pairing uses a bearer token, which the older setup takes, and they pair as they always did. **Open** still syncs first, but a busy or failed refresh does not stop its pairing. **Pair again**, and **Install** on a sandbox with a newer T3, run no refresh. **Install** on a sandbox whose T3 is not newer runs the whole setup as always, and stops when it fails.

## cmux (macOS)

cmux connects over SSH by itself and uploads its own remote part, at the version of the cmux app, so InstaADE installs nothing and no versions must match. On the sandbox it runs one command, `mkdir -p ~/.local/state && chmod go-w ~/.local ~/.local/state`, because cmux refuses a group-writable state directory. **Open in cmux** opens a `cmux://ssh` link with an SSH token of the sandbox.

The link holds the token, so InstaADE opens it only in the real cmux app in `/Applications` or `~/Applications`. Its bundle identifier must be `com.cmuxterm.app`, and `codesign` must find it signed with the Developer ID of Manaflow, Inc. (team `7WLXT3NR37`). InstaADE checks this when it looks for cmux, again before it wakes, creates or marks a sandbox for cmux, and again right before it makes or loads the token. It never opens the link in another handler of `cmux://` links or through cmux.com. Without that app, nothing opens and no token is made.

cmux has no idle probe yet, so the sandbox sleeps by its terminals and CPU.

## Orca

Orca connects over SSH by itself and installs its own server on the sandbox under `~/.orca-remote`, so InstaADE installs nothing and no versions must match. Orca has no link that adds a host, but it imports the hosts in `~/.ssh/config`. So **Install Orca** and **Open in Orca** wake the sandbox, write it as a host to `~/.ssh/instabox/config` and start Orca. Then connect the host from Orca's SSH menu and open `/data/home`.

```
# instabox-sandbox <sandbox id>
Host instabox-dev-1
  HostName ssh.instabox.sh
  Port 2222
  User <SSH token>
  StrictHostKeyChecking accept-new
  ForwardAgent no
```

- The alias is `instabox-` and the sandbox name. When two sandboxes have the same name, the second one also gets the end of its id. A host keeps its alias, because Orca knows the host by it.
- `~/.ssh/config` gets one line at the top, `Include ~/.ssh/instabox/config`, so these options come before any `Host *`. InstaADE never changes another line there, and keeps a copy of the file in `~/.ssh/config.instabox.bak` before its first change. On Windows these files are in `%USERPROFILE%\.ssh`.
- `ForwardAgent no` is there because the InstaBox SSH gateway refuses agent forwarding, and Orca fails the whole connection when that request is refused.
- The hosts file holds SSH tokens. On macOS only you can read it and its folder. When InstaADE gets a new token for the sandbox, it rewrites `User`.
- Tokens last 30 days. At startup, at sign-in and every few minutes, InstaADE renews the token of each sandbox with a host once it has fewer than 7 days left. That is an API call only and never wakes the sandbox. When the host cannot take the new token, the stored token stays behind, so the next pass tries again.
- Deleting the sandbox in InstaADE removes its host. A full refresh removes the host of any sandbox no longer in your sandbox list, and signing out removes them all. InstaADE changes only the blocks it marked. Orca keeps a host it imported until you remove it in Orca.
- On macOS, InstaADE starts only the real Orca app in `/Applications` or `~/Applications`. Its bundle identifier must be `com.stablyai.orca`, and `codesign` must find it signed with the Developer ID of team `6CX3WHS9HZ`. InstaADE checks this when it looks for Orca, again before it wakes, creates or marks a sandbox for Orca, and again after the wake, right before it makes or loads the token.
- On Windows, InstaADE starts `Orca.exe` where the installer puts it (`%LOCALAPPDATA%\Programs\Orca` or `%ProgramFiles%\Orca`). It starts it only when the file's version resource names the product Orca and holds a version, read from the file each time. There is no signature check on Windows yet.

Orca has no idle probe, so the sandbox sleeps by its terminals and CPU.

## Paradev (macOS)

Paradev (formerly Superset) runs a sandbox as one of its hosts, and sets the host up by itself over SSH: it installs its CLI and a `superset-host` user service on the sandbox, which then connects out to Paradev's relay. Paradev has no link that adds a host without you, so InstaADE installs nothing and hands the setup to Paradev:

1. **Install Paradev** wakes the sandbox, copies its SSH address, `ssh://<SSH token>@<gateway>:<port>`, and opens Paradev's host settings. A minute later InstaADE clears the address from the clipboard, unless you copied something else since. When InstaADE quits before then, it clears the address on the way out by the same rule.
2. In Paradev, click **Set up a host over SSH** and paste the address.
3. Click **Install Paradev** again. InstaADE checks with one command on the sandbox that `superset-host` is active. Only then does the sandbox get `paradev` in its mark, and a new workspace on it opens in Paradev.

**Open in Paradev** wakes the sandbox, since Paradev does not wake a sleeping one, reads its machine id, and opens `superset://new-workspace?host=<host id>`, with the host id Paradev derives from that machine id. **Reconnect** checks the host again.

The address holds an SSH token, so InstaADE copies it, and opens Paradev's links, only for the real Paradev app in `/Applications` or `~/Applications`. Its bundle identifier must be `com.superset.desktop` (the Canary build is not supported), and `codesign` must find it signed with Paradev's Developer ID (team `2HVBK29S3C`). InstaADE checks this when it looks for Paradev, again before it wakes, creates or marks a sandbox for Paradev, and again right before it makes or loads the token or opens a link. It never opens a `superset://` link in another handler.

Paradev has no idle probe yet, so the sandbox sleeps by its terminals and CPU.

## Paseo

Paseo runs a daemon on every machine it works on, and its desktop app connects to that daemon. Over SSH, the app runs `ssh -W 127.0.0.1:6767 <host>` with your OpenSSH config, and it installs and starts nothing on the host. Paseo has no link that adds a host. So InstaADE runs the daemon on the sandbox and writes its host, and you add the host in Paseo once:

1. **Install Paseo** wakes the sandbox. When the sandbox has no `paseo`, InstaADE installs the Paseo CLI at your app's version, with `npm install -g @getpaseo/cli@<version>`. The standard sandbox has Node.js and npm, and its global npm folder needs no sudo.
2. It runs the daemon as the `paseo-instabox` service. The service runs `paseo daemon run` in a login shell, so the daemon finds Claude Code, Codex and the other agent CLIs on your PATH. The daemon listens on `127.0.0.1:6767` only, starts at boot and restarts when it fails. Once it answers, the sandbox gets `paseo` in its mark.
3. InstaADE writes the sandbox as a host in `~/.ssh/instabox/config`, the same host as for [Orca](#orca), with the same `Include` line, token renewal and cleanup. Then it copies `ssh://instabox-<name>` and starts Paseo. The address holds no token: ssh reads the login from the host.
4. In Paseo, open **Settings**, click **Add host**, then **Remote SSH**, paste the address and click **Connect**. Paseo keeps the host.

**Open in Paseo** wakes the sandbox, writes the host, copies the address and starts Paseo. It runs nothing on the sandbox. **Reconnect** runs the setup again, which starts a stopped daemon.

InstaADE never updates or restarts a Paseo the sandbox already has: the daemon may be running agents, and Paseo's app and daemon work across versions. To update it, run `npm install -g @getpaseo/cli` on the sandbox, then `sudo systemctl restart paseo-instabox`.

On Windows, InstaADE does not copy the address yet. Type it from the message InstaADE shows.

Paseo reads the host, which holds an SSH token, so InstaADE starts only the real Paseo app:

- On macOS, Paseo in `/Applications` or `~/Applications`. Its bundle identifier must be `sh.paseo.desktop`, and `codesign` must find it signed with Paseo's Developer ID (team `99ZMJMKU9Y`). InstaADE checks this when it looks for Paseo, again before it wakes, creates or marks a sandbox for Paseo, and again after the wake, right before it makes or loads the token.
- On Windows, `Paseo.exe` where the installer puts it (`%LOCALAPPDATA%\Programs\Paseo` or `%ProgramFiles%\Paseo`). InstaADE starts it only when the file's version resource names the product Paseo and holds a version, read from the file each time. Paseo's Windows app is not signed, so there is no signature to check.

Paseo has no idle probe yet, so the sandbox sleeps by its terminals and CPU.

## Several ADEs on one sandbox

A sandbox can have several ADEs, and its details list each one with **Open**. The sandbox's metadata records which ADEs it has, in a form older releases (named Instabox) still read. [ADEs and the sandbox mark](../DEVELOPMENT.md#ades-and-the-sandbox-mark) has the rules.
