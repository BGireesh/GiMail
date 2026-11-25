#!/bin/zsh
# Sets up a fresh Revyl cloud Mac for working on Exo: toolchains, GitHub and Claude Code sign-in,
# the repository, a warm build, and a few shortcuts. Revyl Macs start empty and are deleted when
# they stop (two hours at most), so run this once at the start of every session, from Terminal on
# the Mac's desktop:
#
#   zsh -c "$(curl -fsSL https://raw.githubusercontent.com/BGireesh/GiMail/gireesh/revyl-dev-mac/scripts/revyl-dev-mac.sh)"
#
# Optional, to skip the interactive sign-ins: export GH_TOKEN and/or CLAUDE_CODE_OAUTH_TOKEN in the
# same Terminal first. Nothing here stores a token beyond the Mac itself, which Revyl deletes.
# Every step is safe to run again.

set -e
setopt pipefail 2>/dev/null || true

REPO=BGireesh/GiMail
UPSTREAM=https://github.com/m1guelpf/Exo.git
DIR="$HOME/Exo"
RC="$HOME/.zshrc"
REVYL_ADMIN_PASSWORD=revyl   # Revyl's documented password for the Mac's `computer` account

# Interactive sign-ins need a terminal; without one (a remote exec), they are skipped.
has_tty() { { : </dev/tty; } 2>/dev/null; }

step() { print -P "\n%F{cyan}==> $1%f"; }
note() { print -P "%F{yellow}    $1%f"; }

export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8   # fastlane wants a UTF-8 locale

# Exo's tests open many files at once; the Mac's default limit of 256 fails hundreds of them.
ulimit -n 65536

step "Homebrew: take ownership so installs work without sudo"
if [[ ! -w /opt/homebrew/Cellar ]]; then
  print "$REVYL_ADMIN_PASSWORD" | sudo -S -p '' chown -R "$(whoami)" /opt/homebrew
fi

step "Rust"
if [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt >/dev/null
fi
source "$HOME/.cargo/env"
rustc --version

step "Ruby 4 for fastlane (builds Exo.app)"
if [[ ! -x /opt/homebrew/opt/ruby/bin/ruby ]]; then
  HOMEBREW_NO_AUTO_UPDATE=1 brew install ruby >/dev/null
fi
export PATH="/opt/homebrew/opt/ruby/bin:$PATH"
ruby -v

step "GitHub sign-in"
if ! gh auth status >/dev/null 2>&1; then
  if [[ -n "$GH_TOKEN" ]]; then
    note "Using GH_TOKEN from this Terminal."
  elif has_tty; then
    note "Approve the code below at https://github.com/login/device (your iPad or phone works)."
    gh auth login --hostname github.com --git-protocol https --web --scopes repo,workflow </dev/tty
  else
    note "No terminal, so skipping GitHub sign-in; run 'gh auth login' later to push."
  fi
fi
if gh auth status >/dev/null 2>&1; then
gh auth setup-git
# Commit as the signed-in GitHub account, with its private noreply address.
if [[ -z "$(git config --global user.email)" ]]; then
  login=$(gh api user --jq .login)
  id=$(gh api user --jq .id)
  name=$(gh api user --jq '.name // .login')
  git config --global user.name "$name"
  git config --global user.email "${id}+${login}@users.noreply.github.com"
fi
fi
git config --global pull.ff only
[[ -n "$(git config --global user.email)" ]] && print "Committing as $(git config --global user.name) <$(git config --global user.email)>"

step "Repository"
if [[ ! -d "$DIR/.git" ]]; then
  git clone -q "https://github.com/$REPO.git" "$DIR"
  git -C "$DIR" remote add upstream "$UPSTREAM"
fi
git -C "$DIR" fetch -q --all --prune
# fastlane's gems go in the checkout (as on Gireesh's Mac), not Homebrew Ruby's shared folder.
grep -qx '/vendor/' "$DIR/.git/info/exclude" 2>/dev/null || print -l '/vendor/' '/.bundle/' >> "$DIR/.git/info/exclude"
(cd "$DIR" && bundle config set --local path vendor/bundle >/dev/null)
print "$(git -C "$DIR" branch --show-current) at $(git -C "$DIR" log --oneline -1)"

step "Claude Code"
mkdir -p "$HOME/.claude"
cat > "$HOME/.claude/CLAUDE.md" <<'EOF'
# Personal instructions (Gireesh)

- Never add `Co-Authored-By: Claude`, "Generated with Claude Code", or any other Claude/Anthropic
  attribution to commit messages or PR bodies. Tell subagents that commit this too.
- Work on branches named `gireesh/<topic>`. Never commit to `main`, and merge or push to `main`
  only with Gireesh's explicit approval.
- This is a Revyl cloud Mac: it is deleted when it stops, two hours after it started. Push work to
  `origin` (BGireesh/GiMail) before then, and say plainly when something is not pushed yet.
- Exo is a macOS app. Raise the open-file limit before tests (`ulimit -n 65536`); `exo-test` does.
- To see the app with fake mail, run `exo-mock` (a local gmail-mock), never a real Google account.
EOF
if [[ -n "$CLAUDE_CODE_OAUTH_TOKEN" ]]; then
  note "Using CLAUDE_CODE_OAUTH_TOKEN from this Terminal."
else
  note "Claude Code signs in the first time you run 'claude' (it shows a link and asks for a code)."
fi
claude --version

step "Shortcuts in ~/.zshrc"
if ! grep -q '# >>> exo-dev >>>' "$RC" 2>/dev/null; then
  cat >> "$RC" <<'EOF'
# >>> exo-dev >>>
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
ulimit -n 65536
source "$HOME/.cargo/env"
export PATH="/opt/homebrew/opt/ruby/bin:$PATH"
alias exo='cd ~/Exo'
# Full test suite.
exo-test() { (cd ~/Exo && ulimit -n 65536 && cargo test --workspace "$@"); }
# Build Exo.app the way the README does, then open it.
exo-app() { (cd ~/Exo && bundle install --quiet && bundle exec fastlane build && open target/release/Exo.app); }
# Run Exo against a local gmail-mock with fake mail (sign in with the button; it never reaches Google).
exo-mock() {
  (cd ~/Exo && cargo build -q -p gmail-mock --bin gmail-mock) || return
  pkill -f 'target/debug/gmail-mock' 2>/dev/null
  (~/Exo/target/debug/gmail-mock --port 8787 >/tmp/gmail-mock.log 2>&1 &)
  sleep 2
  [[ -d ~/Exo/target/release/Exo.app ]] || exo-app
  pkill -x Exo 2>/dev/null; sleep 1
  open --env EXO_GMAIL_MOCK=http://127.0.0.1:8787/ ~/Exo/target/release/Exo.app
}
# What is on this Mac and not yet on GitHub.
exo-unpushed() { (cd ~/Exo && git status --short && git log --branches --not --remotes --oneline); }
# <<< exo-dev <<<
EOF
fi

step "Warm build (so the first test run and app build are quick)"
(cd "$DIR" && cargo test --workspace --no-run -q 2>&1 | tail -3)
(cd "$DIR" && bundle install --quiet)

print -P "\n%F{green}Ready.%f Open a new Terminal tab (for the shortcuts), then:"
print "  exo            # cd ~/Exo"
print "  git switch -c gireesh/<topic>"
print "  claude         # Claude Code in the repo"
print "  exo-test       # full test suite"
print "  exo-mock       # build Exo.app and run it against fake mail"
print "  exo-unpushed   # anything not on GitHub yet; push before this Mac's two hours are up"
