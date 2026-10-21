# Balance ton port

Claude Code sessions start dev servers (Astro, wrangler, Metro, Vite, `serve`...) and leave them behind. The session ends or crashes, the worktree is deleted, and the ports stay taken. Balance ton port cleans up automatically and asks only when it cannot be sure.

It is a macOS menu bar app plus a `btp` CLI, both on one Rust core, and an agent skill.

## What it does

Every 5 seconds it links new listening servers to the Claude Code session that started them, then applies three triggers:

1. **Worktree deleted**: the server is killed at once.
2. **Session dead and port idle for 20 min**: the server is killed.
3. **Session alive but idle for 30 min**: a notification with Kill and Keep buttons (app only).

Kill is SIGTERM on the server and its children (for example wrangler's `workerd`), then SIGKILL after 5 s for survivors. The menu bar lists running servers and the last 5 kills with their reason.

## Install

Homebrew (coming with the first release):

```bash
brew install matthieuh/tap/btp                   # CLI
brew install --cask matthieuh/tap/balance-ton-port   # menu bar app
```

From source today (macOS, Xcode, rustup with the aarch64 and x86_64 darwin targets, XcodeGen):

```bash
cargo install --path crates/btp-cli              # the btp CLI

scripts/build-xcframework.sh                     # Rust core + Swift bindings
(cd app && xcodegen generate)
xcodebuild -project app/BalanceTonPort.xcodeproj -scheme BalanceTonPort build
```

## CLI

```bash
btp list [--json]        # tracked servers; --json has "servers" and "recent_kills"
btp kill <port>          # kill a tracked server by port
btp kill --pid <pid>     # ... or by pid
btp sweep [--dry-run]    # apply triggers 1 and 2 once; --dry-run kills nothing
btp watch                # watch in the foreground until Ctrl-C
btp config get           # print the effective configuration
btp config set <key> <n> # set a numeric key
```

Only servers Balance ton port tracks can be killed; an unknown port or pid exits 1. `btp watch` refuses to start while the app is already watching. Without the app, `btp sweep` applies triggers 1 and 2 only (no alerts).

## Configuration

`~/Library/Application Support/balance-ton-port/config.toml`, shared by the app and the CLI. Edit it, use the app's Settings, or `btp config set`.

| Key | Default | Meaning |
|---|---|---|
| `dead_session_idle_s` | 1200 | Idle time before a dead session's server is killed |
| `alert_after_s` | 1800 | Idle time before the Kill / Keep alert |
| `kill_grace_s` | 5 | Delay between SIGTERM and SIGKILL |
| `scan_interval_s` | 5 | Scan period |
| `claude_config_dirs` | `[]` | Extra Claude config dirs only. `~/.claude`, `$CLAUDE_CONFIG_DIR` and every `~/.claude-*` with a `sessions/` folder are found automatically |
| `exclude_commands` | `mcp`, `argent`, `postgres`, `redis-server`, `mysqld`, `mongod` | Never tracked when the command line contains one of these (case-insensitive) |

An exclusion matches the whole command line, paths included: a project path containing "mcp" excludes its servers. That errs on the safe side.

## Agent skill

Teaches agents to run `btp list --json` before starting a server, reuse or free ports with `btp`, and `btp sweep` at the end of a task, instead of `lsof | xargs kill`.

```bash
cp -r skills/balance-ton-port ~/.claude/skills/
```

If you use a custom `CLAUDE_CONFIG_DIR`, copy it into that directory's `skills/` folder instead.

## Safety rules

- Never kills a process that was not started from a live Claude Code session.
- A server whose worktree is used by another live session is never touched.
- Claude Code processes themselves are never tracked.
- Start times are re-checked before every signal, so a reused pid is never hit.
- `btp sweep --dry-run` prints what would be killed.

## Releasing

CI (`.github/workflows/ci.yml`) runs fmt, clippy, the Rust tests and the Swift tests on every push and pull request. A `v*` tag runs `.github/workflows/release.yml`: universal `btp` and app, signed with the HAPPYSOLVERS SAS Developer ID, notarized, app stapled, then a GitHub release with `btp-<v>-macos-universal.tar.gz`, `BalanceTonPort-<v>.zip`, their `.sha256` files and the filled Homebrew formula and cask.

### Repository secrets (one time)

Settings > Secrets and variables > Actions, or `gh secret set NAME`:

| Secret | What it is | How to get it |
|---|---|---|
| `APPLE_CERT_P12_BASE64` | The "Developer ID Application: HAPPYSOLVERS SAS" certificate and its private key, as base64 | In Keychain Access, My Certificates, select the certificate (expand it to check the private key is there), File > Export Items, format `.p12`, set a password. Then `base64 -i cert.p12 \| pbcopy`. If no such certificate exists yet, create one in the Apple Developer portal (Certificates, +, Developer ID Application; needs the Account Holder role). Delete the `.p12` file afterwards |
| `APPLE_CERT_PASSWORD` | The password set on that `.p12` export | |
| `APPLE_TEAM_ID` | The 10 character team id of HAPPYSOLVERS SAS | developer.apple.com/account, Membership details, or the `(XXXXXXXXXX)` suffix of the certificate name |
| `APPLE_ID` | The Apple ID email of a member of that team, used by `notarytool` | |
| `APPLE_APP_PASSWORD` | An app-specific password for that Apple ID | account.apple.com, Sign-In and Security, App-Specific Passwords, generate one named for example `btp-notarytool` |

### Cutting a release

1. Bump the version in `crates/btp-cli/Cargo.toml` and in `app/project.yml` (`CFBundleShortVersionString`, also bump `CFBundleVersion`), then `(cd app && xcodegen generate)` to refresh `app/Info.plist`, and commit. The release fails if the tag does not match both.
2. Tag and push:

   ```bash
   git tag v0.1.0 && git push --tags
   ```

3. Check the release: download the zip, then `spctl -a -vv "Balance ton port.app"` must say `accepted, source=Notarized Developer ID`.

### Publishing to Homebrew

The tap lives in `matthieuh/homebrew-tap` (a public repo with that exact name, so `brew` finds it as `matthieuh/tap`). The templates are in `packaging/homebrew/`; each release attaches them already filled with its version and sha256 (`scripts/render-homebrew.sh` does it):

```bash
gh release download v0.1.0 -R matthieuh/balance-ton-port -p btp.rb -p balance-ton-port.rb
# in a clone of matthieuh/homebrew-tap
mkdir -p Formula Casks
mv btp.rb Formula/btp.rb
mv balance-ton-port.rb Casks/balance-ton-port.rb
git add Formula Casks && git commit -m "btp, balance-ton-port 0.1.0" && git push
```

Then check from a clean shell:

```bash
brew install matthieuh/tap/btp && brew test matthieuh/tap/btp
brew install --cask matthieuh/tap/balance-ton-port
brew audit --strict --tap matthieuh/tap
```

## License

MIT
