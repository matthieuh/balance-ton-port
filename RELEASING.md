# Releasing

CI (`.github/workflows/ci.yml`) runs fmt, clippy, the Rust tests and the Swift tests on every push and pull request. A `v*` tag runs `.github/workflows/release.yml`: universal `btp` and app, signed with the HAPPYSOLVERS Developer ID, notarized, app stapled, app packed by `scripts/make-dmg.sh` into `BalanceTonPort-<v>.dmg` (app plus an Applications link), the dmg signed, notarized and stapled too, then a GitHub release with `btp-<v>-macos-universal.tar.gz`, `BalanceTonPort-<v>.dmg`, their `.sha256` files, the filled Homebrew formula and cask, and notes generated from the merged pull requests. A tag with a hyphen (`v0.2.0-beta.1`) is published as a prerelease. Nothing is published unless every step passed.

### Repository secrets (one time)

Settings > Secrets and variables > Actions, or `gh secret set NAME`:

| Secret | What it is | How to get it |
|---|---|---|
| `APPLE_CERT_P12_BASE64` | The "Developer ID Application: HAPPYSOLVERS" certificate and its private key, as base64 | In Keychain Access, My Certificates, select the certificate (expand it to check the private key is there), File > Export Items, format `.p12`, set a password. Then `base64 -i cert.p12 \| pbcopy`. If no such certificate exists yet, create one in the Apple Developer portal (Certificates, +, Developer ID Application; needs the Account Holder role). Delete the `.p12` file afterwards |
| `APPLE_CERT_PASSWORD` | The password set on that `.p12` export | |
| `APPLE_TEAM_ID` | The 10 character team id of HAPPYSOLVERS SAS | developer.apple.com/account, Membership details, or the `(XXXXXXXXXX)` suffix of the certificate name |
| `APPLE_ID` | The Apple ID email of a member of that team, used by `notarytool` | |
| `APPLE_APP_PASSWORD` | An app-specific password for that Apple ID | account.apple.com, Sign-In and Security, App-Specific Passwords, generate one named for example `btp-notarytool` |
| `HOMEBREW_TAP_TOKEN` | A fine-grained personal access token that lets the release push to `matthieuh/homebrew-tap` | github.com/settings/personal-access-tokens, repository access: only `matthieuh/homebrew-tap`, permission Contents: read and write. Then `gh secret set HOMEBREW_TAP_TOKEN -R matthieuh/balance-ton-port`. It expires on 2027-10-24: before then, create a new token the same way and set the secret again |

### Cutting a release

1. Bump the version in `crates/btp-cli/Cargo.toml` and in `app/project.yml` (`CFBundleShortVersionString`, also bump `CFBundleVersion`), then `(cd app && xcodegen generate)` to refresh `app/Info.plist`, and commit. The release fails if the tag does not match both.
2. Tag and push:

   ```bash
   git tag v0.1.0 && git push --tags
   ```

3. Check the release: download the dmg, `spctl -a -t open --context context:primary-signature -vv BalanceTonPort-0.1.0.dmg` must say `accepted, source=Notarized Developer ID`; open it, drag the app to Applications, then `spctl -a -vv "/Applications/Balance ton port.app"` must say the same.

### The Claude Code plugin

The plugin (`plugin/`) is installed from the repo's default branch, not from a release. Claude Code updates an installed plugin only when its version changes, so bump `version` in `plugin/.claude-plugin/plugin.json` in every change to `plugin/` that should reach users.

`BTP_MIN_VERSION` in `plugin/scripts/lib.sh` is the oldest `btp` the hooks accept. Raise it when the hooks start calling a command or flag that only a newer `btp` has, and never above the version being released (the release workflow refuses a tag below it; `scripts/test-plugin.sh` notes when `btp-cli` is still below it, which is expected between raising it and the release bump). A `btp` below it makes the plugin offer an install, which refuses until a release at or above it is published.

### Publishing to Homebrew

The tap lives in `matthieuh/homebrew-tap` (a public repo with that exact name, so `brew` finds it as `matthieuh/tap`). The templates are in `packaging/homebrew/`; `scripts/render-homebrew.sh` fills them with the release's version and sha256, and each release attaches the result.

For a final release (no hyphen in the tag) the `tap` job of the release workflow (after the `release` job) commits `Formula/btp.rb` and `Casks/balance-ton-port.rb` to the tap's default branch with the `HOMEBREW_TAP_TOKEN` secret. A prerelease tag leaves the tap untouched. If that step fails after the release is published (an expired token, for instance), fix the cause and rerun only the `tap` job from the Actions page, or do it by hand, which stays the fallback:

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
