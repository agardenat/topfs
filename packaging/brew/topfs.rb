class Topfs < Formula
  desc "Live top-N biggest filesystem entries with tree display"
  homepage "https://github.com/agardenat/topfs"
  url "https://github.com/agardenat/topfs/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "459d55385bca5c096405fc6ef35991778fff0f769988eb5fadd9f84164962d8d"
  license "Apache-2.0"
  head "https://github.com/agardenat/topfs.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "topfs", shell_output("#{bin}/topfs --version")
  end
end
