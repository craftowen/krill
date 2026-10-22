#!/usr/bin/env python3
"""Generate a local personal tap from already built release artifacts; never upload."""

import argparse
import hashlib
import json
from pathlib import Path
import re
from urllib.parse import urlsplit


def ruby_string(value):
    # JSON string syntax also works in Ruby, except Ruby interpolates these forms.
    return json.dumps(str(value), ensure_ascii=False).replace("#{", r"\#{").replace("#@", r"\#@").replace("#$", r"\#$")


def artifact_url(value):
    url = urlsplit(value)
    if url.scheme not in ("https", "file") or url.username or url.password or url.query or url.fragment:
        raise ValueError("artifact URL must be HTTPS or a local file URL, without credentials, query or fragment")
    if url.scheme == "https" and not url.hostname:
        raise ValueError("HTTPS URL needs a host")
    if url.scheme == "file" and url.netloc not in ("", "localhost"):
        raise ValueError("file URL must be local")
    return value


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cask(version, url, digest, homepage):
    return f'''# Generated locally by Krill packaging/homebrew/generate.py.
cask "krill" do
  version {ruby_string(version)}
  sha256 {ruby_string(digest)}

  url {ruby_string(url)}
  name "Krill"
  desc "Persistent parallel coding agent sessions"
  homepage {ruby_string(homepage)}

  depends_on arch: :arm64
  auto_updates true

  preflight_steps do
    run "/bin/sh", args: ["-c",
      'if [ -e "$1" ] || [ -L "$1" ]; then ' \\
      '[ -L "$1" ] && [ "$(readlink "$1")" = "$2" ] && exit 0; ' \\
      'echo "krill already exists at $1. Resolve the conflict before installing Krill." >&2; exit 1; fi',
      "krill-conflict", "{{{{HOMEBREW_PREFIX}}}}/bin/krill", "{{{{appdir}}}}/Krill.app/Contents/MacOS/krill"]
  end

  app "Krill.app"
  binary "#{{appdir}}/Krill.app/Contents/MacOS/krill"

  zap trash: [
    "~/Library/Application Support/dev.krill.app",
    "~/Library/Caches/dev.krill.app",
    "~/Library/Logs/Krill",
    "~/Library/Preferences/dev.krill.app.plist",
    "~/Library/Saved Application State/dev.krill.app.savedState",
  ]

  caveats <<~EOS
    This personal cask is ad-hoc signed and is not notarized.
    Gatekeeper can block downloaded builds; this cask does not bypass it.
    Prefer the source formula until Developer ID signing is available.
    If krill already exists, resolve the conflict before installing; do not overwrite it.
    Before uninstalling with --zap, close sessions and run krill hook uninstall for
    each agent you enabled. User agent settings and project worktrees are preserved.
  EOS
end
'''


def formula(version, url, digest, homepage):
    return f'''# Generated locally by Krill packaging/homebrew/generate.py.
class Krill < Formula
  desc "Persistent parallel coding agent sessions"
  homepage {ruby_string(homepage)}
  url {ruby_string(url)}
  version {ruby_string(version)}
  sha256 {ruby_string(digest)}
  license "Apache-2.0"

  depends_on "rust" => :build
  depends_on :macos
  depends_on arch: :arm64
  conflicts_with "krill", because: "both install an krill executable"

  def install
    system "perl", "-e", "alarm shift; exec @ARGV", "300",
           "cargo", "build", "--release", "--locked",
           "-p", "krill", "-p", "krill-holder", "-p", "krilld", "-p", "krill-app"
    system "bash", "packaging/bundle.sh"
    libexec.install "packaging/dist/Krill.app"
    bin.install_symlink libexec/"Krill.app/Contents/MacOS/krill"
    bin.install_symlink libexec/"Krill.app/Contents/MacOS/krill-app" => "krill"
  end

  def caveats
    <<~EOS
      Launch the source-built UI with krill. The CLI is krill.
      The app bundle is in #{{libexec}}/Krill.app and is built locally, not notarized.
      Do not install the krill cask alongside this formula.
      Uninstall keeps sessions, history and agent settings; see the release guide
      before removing user data.
    EOS
  end

  test do
    ENV["KRILL_STATE_DIR"] = (testpath/"state").to_s
    ENV["KRILL_LOG_DIR"] = (testpath/"logs").to_s
    assert_match "usage: krill", shell_output("#{{bin}}/krill 2>&1", 2)
    assert_predicate libexec/"Krill.app/Contents/Helpers/krill-holder", :executable?
    assert_predicate libexec/"Krill.app/Contents/Helpers/krilld", :executable?
  end
end
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--source-url", required=True)
    parser.add_argument("--dmg", type=Path, required=True)
    parser.add_argument("--dmg-url", required=True)
    parser.add_argument("--homepage", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?", args.version):
            raise ValueError("version must be a semantic version without a v prefix")
        source_url = artifact_url(args.source_url)
        dmg_url = artifact_url(args.dmg_url)
        homepage = artifact_url(args.homepage)
        source_hash, dmg_hash = sha256(args.source), sha256(args.dmg)
        # Refuse accidental overwrite of a previously prepared tap.
        args.output.mkdir(parents=True, exist_ok=False)
        (args.output / "Formula").mkdir()
        (args.output / "Casks").mkdir()
        (args.output / "Formula/krill.rb").write_text(formula(args.version, source_url, source_hash, homepage))
        (args.output / "Casks/krill.rb").write_text(cask(args.version, dmg_url, dmg_hash, homepage))
        (args.output / "SHA256SUMS").write_text(f"{source_hash}  {args.source.name}\n{dmg_hash}  {args.dmg.name}\n")
    except (ValueError, OSError) as error:
        parser.exit(1, f"release metadata: {error}\n")


if __name__ == "__main__":
    main()
