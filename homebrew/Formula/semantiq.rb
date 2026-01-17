class Semantiq < Formula
  desc "Semantic code understanding for AI tools - One MCP Server for all AI coding assistants"
  homepage "https://github.com/so-keyldzn/semantiq"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/so-keyldzn/semantiq/releases/download/v#{version}/semantiq-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_ARM64_MACOS"
    end
    on_intel do
      url "https://github.com/so-keyldzn/semantiq/releases/download/v#{version}/semantiq-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_X64_MACOS"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/so-keyldzn/semantiq/releases/download/v#{version}/semantiq-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_SHA256_ARM64_LINUX"
    end
    on_intel do
      url "https://github.com/so-keyldzn/semantiq/releases/download/v#{version}/semantiq-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_SHA256_X64_LINUX"
    end
  end

  def install
    bin.install "semantiq"
  end

  test do
    assert_match "semantiq", shell_output("#{bin}/semantiq --version")
  end
end
