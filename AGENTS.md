# Agent assignment: NIRULOGIN 0.2.0, a one-shot repository, security and UX update

## 0. Quick start

1. **Work on the `main` branch.** The audit baseline is `ecd3a16` (0.1.12, pinned by
   NIRUX 0.12.199). The GitHub default branch is currently `master`, which holds only a
   stale initial commit (0.1.0). Item L1 fixes that first.
2. Read this file, `README.md`, `src/main.rs`, `src/greetd.rs`, `nirulogin-session`,
   `nirulogin-power.rules` and `install.sh`. Revalidate each item against current HEAD and
   skip items that are already fixed, noting that you did.
3. Implement **L1–L9, then U1–U5 in order**. Commit per item. Keep `cargo fmt --check`,
   `clippy -D warnings` and tests green.
4. Pass the gates in §4, release in §5 and report in §6.

This is an **execution** assignment. Do not stop after a plan or a branch. Make sensible
decisions, document them, and continue without routine questions. Only ask if you hit a
genuine external blocker or a real risk of making a host unable to log in.

## 1. Invariants

- **greetd owns PAM and the session.** NIRULOGIN never authenticates by itself, never
  stores passwords, and never starts a session without greetd's `Success`.
- A host must **always be able to log in**. Before switching any host to a new build, keep
  a working fallback: the previous `/usr/local/bin/nirulogin`, SDDM/LightDM if installed,
  or a TTY login. Test each host with recovery SSH open.
- No network access and no autologin. The session stays NIRUWM.
- Keep `rust-version = 1.85`. Locked builds must work on Debian 13's rustc 1.85.
- Power actions stay behind a confirmation dialog.

## 2. Bugs and repository problems

### L1: The default branch is stale (high, verified)
`gh repo view` reports `defaultBranchRef = master`. `master` has 1 commit (0.1.0, from
2026-10-30). All development and releases up to v0.1.12 are on `main`. Visitors, the
README shown on GitHub and plain `git clone` therefore get outdated code. That code also
fails clippy (`too_many_arguments`).
**Fix:**
1. `gh repo edit nirucon/nirulogin --default-branch main`.
2. Tag the old tip as `archive/master-0.1.0` and push the tag.
3. Delete the `master` branch on the remote.
4. Verify that `git clone` checks out `main`, and that nothing in `nirucon/nirux`
   (installer, release workflow, docs) references `master` for this repo. Fix any
   references you find.

### L2: Generated files are committed
`SHA256SUMS` in the repo root refers to `target/release/nirulogin`. **Fix:** remove it,
ignore it in `.gitignore`, and confirm that the release workflow generates its own checksums.

### L3: The username is hardcoded to `niru`
`Ui::new` prefills `NIRULOGIN_USER` or `"niru"`. NIRUX is public, so other users get a
wrong name. Also, `configured_session` falls back to `/home/{username}/…`, which is wrong
for home directories outside `/home`.
**Fix:**
- Prefill the **last successfully logged-in user**. Store it in
  `/var/lib/nirulogin/last-user`, written only after greetd `Success`. Have `install.sh`
  create the directory, owned by the detected greeter user with mode 0700.
- Otherwise prefill the only human account (uid ≥ 1000, a valid login shell, not
  `nobody`) when there is exactly one.
- Otherwise leave the field empty with focus on Username.
- `NIRULOGIN_USER` still overrides all of this.
- Resolve the session path from `getpwnam(user).pw_dir` instead of formatting `/home/…`.

### L4: Esc does not cancel an extra PAM prompt
During an extra prompt (OTP, a second factor, a password change), Esc only clears the
field. The greetd session stays open, and the UI stays in "signing" until it times out.
**Fix:** Esc during `pam_prompt` drops the reply channel, and `greetd::login` must then
send `CancelSession` before returning. Today, when `prompt()` returns an error, the
function returns without cancelling. Show "Sign-in cancelled".

### L5: Password copies are not wiped
`ui.password.clone()`, the `password` moved into the thread, `password.to_string()` in
`greetd::login`, and the serialized JSON payload `Vec<u8>` all keep the secret.
**Fix:** use `Zeroizing<String>` for every owned copy, and zeroize the serialization buffer
after `write_all`. Add a short comment wherever a copy cannot be avoided.

### L6: Power actions freeze the greeter
`run_power` runs `dbus-send --reply-timeout=120000`, then `systemctl`/`loginctl`, all on
the UI thread. **Fix:** run the action on a worker with a 10 s overall deadline, show
"Powering off…", and keep the UI responsive.

### L7: The polkit rule allows `*-multiple-sessions` without authentication
`nirulogin-power.rules` lets anyone at the greeter power off or reboot even while other
users are logged in, for example over SSH.
**Fix:**
- Remove `power-off-multiple-sessions` and `reboot-multiple-sessions` from the
  unconditional `YES`.
- When login1 refuses, show "Other users are logged in. Power off from a session
  instead."
- The single-session actions stay allowed.
- Document the change in the README.

### L8: The README contradicts the code about PAM prompts
The README says extra interactive PAM prompts are rejected, but `main` implements them
(`34e47a8`). **Fix:** describe the real behavior: secret and visible prompts, info and
error messages, and Esc cancels (L4).

### L9: Only one monitor is covered
The window is sized to `current_monitor()` or `primary_monitor()`. On a multi-monitor
greeter, the other outputs show the bare X root. That has to be confirmed on othala, which
has 2×2560x1440.
**Fix:**
- Cover every monitor with the background color, either with one borderless window per
  extra monitor or one window spanning the root.
- Draw the login form on the primary monitor, or on the monitor with the pointer when
  there is no primary.
- Re-layout on RandR changes.

## 3. UX

### U1: Caps Lock warning
Show "Caps Lock is on" under the password field while it is active (winit modifiers or
the XKB state). Users notice nothing today.

### U2: Keyboard access to the layout switch
The layout indicator can only be clicked. Add Tab focus to it, and a shortcut (Ctrl+Space
or F2), as long as it cannot be typed into the password. Show the layout in the password
field placeholder as well, for example "Password · SE".

### U3: Feedback after a failed sign-in
After a failed sign-in:
- clear the password, keep the username, and focus the password,
- show greetd/PAM's message, including faillock text when present,
- count the failed attempts during this greeter run.

Disable the button while signing and show "Signing in…".

### U4: The theme
Optionally read `/etc/nirulogin/theme.toml`, with the palette and an optional wallpaper
path that the greeter user can read. When present, use those colors, and the wallpaper
scaled and darkened. Use a decoder only if it fits the 1.85 MSRV without a heavy
dependency; otherwise colors only. When absent, nothing changes.
Add a small, separate change in `nirucon/nirux`: during `nirux update`, which already runs
privileged steps for NIRULOGIN, write that file from the owner's active theme. Keep the
change small and reviewable, and document it.

### U5: Power controls
Show Power off/Restart as icon + label, focusable with Tab, with the confirmation dialog
focused on Cancel by default.

Out of scope: a session picker, user avatars, autologin, network/Wi-Fi, accessibility
tools beyond keyboard access, and Wayland.

## 4. Gates

Automated:
- `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked`, a release build, and `--version`, in CI on `main`.
- An MSRV job in CI (`dtolnay/rust-toolchain@1.85`, `cargo build --locked`).
- **A fake greetd socket test:** a test server on a temp `GREETD_SOCK` that speaks the
  greetd IPC framing. Cover:
  - success with one secret prompt,
  - an auth error,
  - an extra secret and visible prompt,
  - info/error messages,
  - Esc cancel (asserting `CancelSession` is sent),
  - an oversized message,
  - and that `StartSession` carries the expected cmd/env.
- Unit tests:
  - last-user selection and the single-human fallback against a passwd fixture,
  - the `getpwnam` home lookup,
  - the polkit rule content (no `-multiple-sessions` in the allow list),
  - the multi-monitor layout math,
  - Caps Lock state.
- An Xvfb `--preview` screenshot smoke test.

Native, on hosts the owner has authorized for full testing, with reboots and re-login as
needed: othala (Debian, `_greetd`, 2 monitors), algiz (Arch, `greeter`) and void (Void,
`_greeter`, runit). On each host, one at a time:
1. Inventory greetd and polkit, back up the configs, and keep recovery SSH open.
2. Install through the normal NIRUX path.
3. Reboot to the greeter.
4. Test a wrong password, then the right password, the last-user prefill on the next
   boot, a layout switch and that the layout reaches the session, Caps Lock, Esc cancel,
   and power-dialog cancel.
5. On othala, the coverage of both monitors.
6. Leave each host on the new greeter, logged in.

**niruxwork** (the owner's work laptop): Xvfb `--preview` tests only, unless the owner
explicitly agrees to a reboot.

## 5. Release

Bump to **0.2.0** and update the README. Tag `v0.2.0` from `main` through the normal
workflow and verify the assets and checksums. Then pin it in `nirucon/nirux` `manifest`,
together with the U4 nirux change:
- If `nirux/tools/suite-pin` exists, use it.
- Otherwise bump the manifest the usual way. Change only what this assignment owns, and
  rebase if other pins land at the same time.

Do not move or replace published tags or assets.

## 6. Final report

For each item L1–L9 and U1–U5: the change, the tests and the evidence. Include the branch
fix (new default, archive tag), release URLs, and per-host installed and running versions
plus greeter user and monitor count. List remaining limits. Mark anything unfinished as
**PARTIAL** and give the exact remaining steps, including how to roll a host back to the
previous greeter.
