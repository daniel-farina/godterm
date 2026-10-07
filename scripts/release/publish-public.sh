#!/usr/bin/env bash
# Publish the current tree of a local branch (default: master, whose full
# history stays in the private repo) to the public repo as ONE new commit on
# top of the public history. The private history never leaves this machine.
#
#   scripts/release/publish-public.sh "Summary of what changed"
#   SRC=master PUBLIC_REMOTE=origin scripts/release/publish-public.sh "..."
#
# Before pushing it scans what changed for secrets, personal emails, personal
# paths and private hosts (the patterns in ~/.config/godterm/public-deny.txt,
# one extended regex per line, plus built in token formats) and refuses on a
# hit. Commits use the GitHub noreply address.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
msg="${1:?usage: publish-public.sh \"commit message\"}"
SRC="${SRC:-master}"
REMOTE="${PUBLIC_REMOTE:-origin}"
BRANCH="${PUBLIC_BRANCH:-main}"

git fetch -q "$REMOTE" "$BRANCH"
base="$(git rev-parse "$REMOTE/$BRANCH")"
tree="$(git rev-parse "$SRC^{tree}")"
if [[ "$(git rev-parse "$base^{tree}")" == "$tree" ]]; then
  echo "public $BRANCH already has this tree"; exit 0
fi

deny='sk-ant-[a-z]+[0-9]+-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{20}|github_pat_|xai-[A-Za-z0-9]{20}|AKIA[0-9A-Z]{16}|BEGIN [A-Z ]*PRIVATE KEY|dop_v1_'
list="${PUBLIC_DENY:-$HOME/.config/godterm/public-deny.txt}"
if [[ -f "$list" ]]; then
  while IFS= read -r line; do
    [[ -z "$line" || "$line" == \#* ]] || deny+="|$line"
  done < "$list"
fi
hits="$(git diff "$base" "$tree" -- . ':!scripts/release/publish-public.sh' | grep -E '^\+' | grep -niE "$deny" || true)"
if [[ -n "$hits" ]]; then
  echo "refusing to publish: the change contains denied patterns:" >&2
  printf '%s\n' "$hits" | cut -c1-160 >&2
  exit 1
fi

name="$(gh api user --jq '.name // .login')"
email="$(gh api user --jq '"\(.id)+\(.login)@users.noreply.github.com"')"
commit="$(GIT_AUTHOR_NAME="$name" GIT_AUTHOR_EMAIL="$email" GIT_COMMITTER_NAME="$name" GIT_COMMITTER_EMAIL="$email" \
  git commit-tree "$tree" -p "$base" -m "$msg")"
git update-ref refs/heads/public-main "$commit"
git push -q "$REMOTE" "$commit:refs/heads/$BRANCH"
echo "published $(git rev-parse --short "$commit") to $REMOTE/$BRANCH"
