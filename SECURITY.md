# Security policy

Balance ton port signals processes (SIGTERM, then SIGKILL), reads the environment of running processes to restart them, and updates itself through Sparkle. A bug in any of these can hurt the machine it runs on, so please report it privately.

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/matthieuh/balance-ton-port/security/advisories/new). Do not open a public issue.

Include the version (`btp --version`, or Settings > About in the app), the macOS version, and the steps to reproduce. You should get an answer within a week.

## In scope

- A process killed that the [safety rules](README.md#safety-rules) say must be spared: one not started from a live agent session, an agent CLI, an excluded command or port, a reused pid.
- A process environment written to disk or logs, or sent anywhere.
- Anything that lets another local user or a web page make the app or `btp` signal, restart or run a process.
- The update path: the Sparkle feed, the EdDSA signature check, the plugin's `btp` install (sha256 and Developer ID checks).

A server killed too early or too late by the normal rules is a bug, not a vulnerability: open an issue with the "Wrong kill" template.

## Supported versions

Only the latest release gets fixes. The app updates itself; the CLI updates with `brew upgrade btp`.
