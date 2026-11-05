class ClassroomCli < Formula
  desc "Google Classroom from the terminal"
  homepage "https://github.com/SlopProductions/classroom-cli"
  url "https://github.com/SlopProductions/classroom-cli.git", tag: "v0.2.0"
  head "https://github.com/SlopProductions/classroom-cli.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/class --version")
  end
end
