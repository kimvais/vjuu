# Template for Formula/vjuu.rb in github.com/kimvais/homebrew-tap; the release workflow fills in @...@.
class Vjuu < Formula
  desc "Fast review window for AI-agent handoffs: markdown, diffs and code with inline comments"
  homepage "https://github.com/kimvais/vjuu"
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/kimvais/vjuu/releases/download/v@VERSION@/vjuu-v@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA_ARM@"
    end
    on_intel do
      url "https://github.com/kimvais/vjuu/releases/download/v@VERSION@/vjuu-v@VERSION@-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA_INTEL@"
    end
  end

  def install
    bin.install "vjuu"
  end

  test do
    # No arguments: prints usage and exits 2, without opening a window.
    assert_match "usage: vjuu", shell_output("#{bin}/vjuu 2>&1", 2)
  end
end
