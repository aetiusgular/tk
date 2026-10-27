# macOS-only parts

Two programs that run only on macOS, each its own Cargo workspace with its
own `Cargo.lock`, kept out of the app's workspace:

- `activity-collector/`: a root launchd daemon that runs `/usr/bin/eslogger`
  and records what the processes MonoCode starts did. Detection only.
- `computer-use-helper/`: a separate app that holds Accessibility and Screen
  Recording and embeds Cua Driver for computer use.

Neither installs or runs itself. The app asks before either is installed.
