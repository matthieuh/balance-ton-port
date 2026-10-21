# Template. The release workflow renders it with scripts/render-homebrew.sh
# (real version in the url, real sha256) and attaches the result to the
# GitHub release; copy that file to matthieuh/homebrew-tap as Formula/btp.rb.
class Btp < Formula
  desc "Kill dev servers left behind by Claude Code sessions"
  homepage "https://github.com/matthieuh/balance-ton-port"
  url "https://github.com/matthieuh/balance-ton-port/releases/download/v0.0.0/btp-0.0.0-macos-universal.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"

  depends_on :macos

  def install
    bin.install "btp"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/btp --version")
    ENV["BTP_DATA_DIR"] = testpath.to_s
    assert_match "dead_session_idle_s", shell_output("#{bin}/btp config get")
  end
end
