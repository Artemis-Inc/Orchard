# Homebrew formula for Orchard. This is the canonical copy; it is mirrored into
# the tap repository (Artemis-Inc/homebrew-orchard) by the release workflow so
# users can run:
#
#   brew install artemis-inc/orchard/orchard
#
# Do not hand-edit the version or sha256 values: the release workflow regenerates
# this file from the published SHA256SUMS on every tagged release
# (see .github/workflows/release.yml and scripts/update-homebrew.sh).
class Orchard < Formula
  desc "Typed, concurrent language for building LLM agents"
  homepage "https://github.com/Artemis-Inc/Orchard"
  version "3.1.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "68bed8490d9764c4ef8629b6390b7d92b95acd480762ad156ed00b33422977cc"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "15dc6facde8eadf920a2a12b0e53879e084b1e65d18474df04a71e06f909bcca"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a8464c08f323c5210c68e5fcfa9038b67533fd37b650c23419162ac173c311ed"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1aa872f04204b5bacda6686cf5ddee3e7861199ef6e827d4e746e1f79bc1841a"
    end
  end

  def install
    bin.install "orch"
  end

  test do
    assert_match "orch", shell_output("#{bin}/orch --version")
  end
end
