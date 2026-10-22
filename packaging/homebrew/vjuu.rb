# Template for Formula/vjuu.rb in github.com/kimvais/homebrew-tap; the release workflow fills in @...@.
class Vjuu < Formula
  desc "Fast review window for AI-agent handoffs, with inline comments"
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

  # Linux builds link the system's WebKitGTK 4.1 and GTK 3 (Debian/Ubuntu: libwebkit2gtk-4.1-0).
  on_linux do
    on_arm do
      url "https://github.com/kimvais/vjuu/releases/download/v@VERSION@/vjuu-v@VERSION@-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_LINUX_ARM@"
    end
    on_intel do
      url "https://github.com/kimvais/vjuu/releases/download/v@VERSION@/vjuu-v@VERSION@-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_LINUX_INTEL@"
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
