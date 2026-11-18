#!/bin/sh
# Tests for check_banned_names.sh and export_public_snapshot.sh.
# Run: sh scripts/public_snapshot.test.sh
# The banned names here are made up. No real product name belongs in this file.

script_folder=$(cd "$(dirname "$0")" && pwd)
check_banned_names="$script_folder/check_banned_names.sh"
export_public_snapshot="$script_folder/export_public_snapshot.sh"
temp_root=${TMPDIR:-/tmp}
test_root=$(mktemp -d "${temp_root%/}/grove-public-snapshot-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# Runs a command, keeps its output in $output and its exit code in $exit_code.
run() {
  output=$("$@" 2>&1)
  exit_code=$?
}

expect_exit() {
  [ "$exit_code" = "$1" ] || fail "$2: wanted exit $1, got $exit_code. Output: $output"
}

expect_output_has() {
  case "$output" in
    *"$1"*) ;;
    *) fail "$2: wanted \"$1\" in the output. Output: $output" ;;
  esac
}

expect_output_lacks() {
  case "$output" in
    *"$1"*) fail "$2: did not want \"$1\" in the output. Output: $output" ;;
  esac
}

write_file() {
  mkdir -p "$(dirname "$1")"
  printf '%s\n' "$2" > "$1"
}

names_file="$test_root/names.txt"
cat > "$names_file" <<'NAMES'
# A comment, then a blank line. Neither is a pattern.

examplerival
\bfoe\b
allow: --examplerival-[a-z-]+
NAMES

# --- check_banned_names.sh ---

tree="$test_root/banned"
write_file "$tree/docs/notes.md" "first line
We are faster than ExampleRival."
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a banned name"
expect_output_has "docs/notes.md:2:" "a banned name"

tree="$test_root/clean"
write_file "$tree/docs/notes.md" "Nothing to see. A whole-word pattern skips longer words: foes, foetus."
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 0 "a clean tree"

tree="$test_root/whole_word"
write_file "$tree/notes.md" "friend or foe"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a whole-word name"

tree="$test_root/allowed_path"
write_file "$tree/crates/grove-core/src/editor.rs" "Editor::ExampleRival"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 0 "an allow-listed path"

tree="$test_root/same_name_elsewhere"
write_file "$tree/other/crates/grove-core/src/editor.rs" "Editor::ExampleRival"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "an allow-listed path under another folder"

tree="$test_root/allowed_pattern"
write_file "$tree/app/style.css" "color: var(--examplerival-editor-background);"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 0 "an allowed pattern"

tree="$test_root/allowed_pattern_and_name"
write_file "$tree/app/style.css" "color: var(--examplerival-editor-background); /* like examplerival */"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a banned name beside an allowed pattern"
expect_output_has "app/style.css:1:" "a banned name beside an allowed pattern"

tree="$test_root/skipped_files"
write_file "$tree/Cargo.lock" "examplerival"
write_file "$tree/app/package-lock.json" "examplerival"
write_file "$tree/sidecar/bun.lock" "examplerival"
mkdir -p "$tree/icons"
printf 'examplerival\000\001\002' > "$tree/icons/icon.png"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 0 "lock files and binary files"

tree="$test_root/banned_path"
write_file "$tree/docs/examplerival-notes.md" "nothing"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a banned name in a path"
expect_output_has "docs/examplerival-notes.md" "a banned name in a path"

# Only package lock files are skipped. Another file that ends in -lock.json is checked.
tree="$test_root/other_lock_file"
write_file "$tree/skills-lock.json" "examplerival"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a file that only looks like a lock file"
expect_output_has "skills-lock.json:1:" "a file that only looks like a lock file"

tree="$test_root/banned_link_name"
write_file "$tree/real/a.md" "nothing"
ln -s real "$tree/examplerival-docs"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a banned name in a symlink's name"
expect_output_has "examplerival-docs" "a banned name in a symlink's name"

tree="$test_root/banned_link_target"
mkdir -p "$tree"
ln -s ../examplerival/notes "$tree/link"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "a banned name in a symlink's target"
expect_output_has "link: " "a banned name in a symlink's target"

# awk would read the first path as a variable assignment and the second as standard input.
tree="$test_root/odd_file_names"
write_file "$tree/notes=v2.md" "We beat ExampleRival."
write_file "$tree/-" "Also ExampleRival."
write_file "$tree/zz/last.md" "And ExampleRival."
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 1 "odd file names"
expect_output_has "notes=v2.md:1:" "a path shaped like an assignment"
expect_output_has "-:1:" "a file named -"
expect_output_has "zz/last.md:1:" "a file after one named -"

printf 'examplerival  \r\n\\bfoe\\b\t\r\nallow: --examplerival-[a-z-]+ \r\n' > "$test_root/crlf-names.txt"
run sh "$check_banned_names" "$test_root/banned" "$test_root/crlf-names.txt"
expect_exit 1 "a names file with CRLF line ends"
run sh "$check_banned_names" "$test_root/whole_word" "$test_root/crlf-names.txt"
expect_exit 1 "a whole-word name from a names file with CRLF line ends"
run sh "$check_banned_names" "$test_root/allowed_pattern" "$test_root/crlf-names.txt"
expect_exit 0 "an allowed pattern from a names file with CRLF line ends"

# Cutting allowed text can join the two halves of a banned name. The joined name still fails.
printf 'examplerival\nallow: XX\n' > "$test_root/joining-names.txt"
tree="$test_root/joined_name"
write_file "$tree/notes.md" "exampleXXrival"
run sh "$check_banned_names" "$tree" "$test_root/joining-names.txt"
expect_exit 1 "a banned name that appears once allowed text is cut"
expect_output_has "notes.md:1:" "a banned name that appears once allowed text is cut"

# A newline in a file name would split it into two paths, so the check refuses to run.
tree="$test_root/newline_in_name"
write_file "$tree/one" "nothing"
write_file "$tree/two" "nothing"
write_file "$tree/one
two" "examplerival"
run sh "$check_banned_names" "$tree" "$names_file"
expect_exit 2 "a file name with a newline"
expect_output_has "newline" "a file name with a newline"

# A pattern that is not a valid regex must stop the check, not pass it.
printf 'examplerival\nfoo(bar\n' > "$test_root/bad-pattern-names.txt"
run sh "$check_banned_names" "$test_root/banned" "$test_root/bad-pattern-names.txt"
expect_exit 2 "a names file with an invalid regex"
expect_output_has "not a valid regex" "a names file with an invalid regex"

printf 'examplerival\nallow: --x-(\n' > "$test_root/bad-allow-names.txt"
run sh "$check_banned_names" "$test_root/banned" "$test_root/bad-allow-names.txt"
expect_exit 2 "a names file with an invalid allow regex"
expect_output_has "not a valid regex" "a names file with an invalid allow regex"

run sh "$check_banned_names" "$test_root/clean" "$test_root/no-such-names.txt"
expect_exit 2 "a missing names file"
expect_output_has "names file" "a missing names file"

write_file "$test_root/empty-names.txt" "# only a comment"
run sh "$check_banned_names" "$test_root/clean" "$test_root/empty-names.txt"
expect_exit 2 "a names file with no pattern"

# With no names file argument, the default is research/banned-names.txt in the repository the
# command runs in. Outside a repository there is none.
run sh -c 'cd "$1" && sh "$2" "$3"' _ "$test_root" "$check_banned_names" "$test_root/clean"
expect_exit 2 "no names file outside a repository"
expect_output_has "names file" "no names file outside a repository"

# --- export_public_snapshot.sh ---

# The slug is built from parts so the snapshot's own rewrite leaves this file alone.
github_owner=aaf1007
private_slug="$github_owner/grove"
public_slug="$private_slug-ade"

new_repo() {
  repo=$1
  mkdir -p "$repo"
  git -C "$repo" init --quiet --initial-branch=main
  write_file "$repo/.gitignore" "ignored.txt
research/"
  write_file "$repo/README.md" "Clone https://github.com/$private_slug.git
Issues: $private_slug#12
Already public: $public_slug
Another repo: $private_slug-hooks
Another owner: not$private_slug.git
Twice: $private_slug,$private_slug"
  write_file "$repo/research/banned-names.txt" "examplerival"
  write_file "$repo/ignored.txt" "ignored"
}

commit_all() {
  git -C "$1" add --all
  git -C "$1" -c user.name=test -c user.email=test@example.com commit --quiet --message "test"
}

snapshot() {
  run sh -c 'cd "$1" && shift && sh "$@"' _ "$@"
}

repo="$test_root/repo"
new_repo "$repo"
commit_all "$repo"
write_file "$repo/untracked.txt" "untracked"
out="$test_root/out"
snapshot "$repo" "$export_public_snapshot" "$out"
expect_exit 0 "a snapshot"
expect_output_has "$out" "a snapshot prints its output path"
expect_output_has "$(git -C "$repo" rev-parse main)" "a snapshot prints the commit it exported"
[ -f "$out/README.md" ] || fail "a snapshot holds the tracked file"
[ ! -e "$out/untracked.txt" ] || fail "a snapshot holds no untracked file"
[ ! -e "$out/ignored.txt" ] || fail "a snapshot holds no ignored file"
[ ! -e "$out/research" ] || fail "a snapshot holds no ignored folder"
[ ! -e "$out/.git" ] || fail "a snapshot holds no git folder"
[ -z "$(git -C "$repo" status --porcelain --untracked-files=no)" ] || fail "a snapshot changes nothing tracked"
[ "$(git -C "$repo" rev-list --count main)" = 1 ] || fail "a snapshot makes no commit"

wanted_readme="Clone https://github.com/$public_slug.git
Issues: $public_slug#12
Already public: $public_slug
Another repo: $private_slug-hooks
Another owner: not$private_slug.git
Twice: $public_slug,$public_slug"
[ "$(cat "$out/README.md")" = "$wanted_readme" ] || fail "the slug is rewritten once: $(cat "$out/README.md")"

snapshot "$repo" "$export_public_snapshot" "$out"
expect_exit 2 "a non-empty output directory"
expect_output_has "not empty" "a non-empty output directory"

# A second commit on another branch holds a banned name. The default ref, main, does not.
git -C "$repo" checkout --quiet -b named
write_file "$repo/docs/notes.md" "Faster than ExampleRival."
commit_all "$repo"
snapshot "$repo" "$export_public_snapshot" "$test_root/out_named" named
expect_exit 1 "a snapshot with a banned name"
expect_output_has "docs/notes.md:1:" "a snapshot with a banned name"
expect_output_has "$(git -C "$repo" rev-parse named)" "a failed snapshot prints the commit it exported"
snapshot "$repo" "$export_public_snapshot" "$test_root/out_main"
expect_exit 0 "a snapshot of the default ref from another branch"
[ ! -e "$test_root/out_main/docs/notes.md" ] || fail "the default ref is main"

snapshot "$repo" "$export_public_snapshot" "$test_root/out_unknown" no-such-ref
expect_exit 2 "an unknown ref"
[ ! -e "$test_root/out_unknown" ] || fail "an unknown ref makes no output directory"

# From a linked worktree, the default names file is the one in the main checkout.
git -C "$repo" worktree add --quiet --detach "$test_root/linked_worktree" main
run sh -c 'cd "$1" && sh "$2" "$3"' _ "$test_root/linked_worktree" "$check_banned_names" "$test_root/banned"
expect_exit 1 "the default names file from a linked worktree"
expect_output_has "docs/notes.md:2:" "the default names file from a linked worktree"

# A tracked file name with a newline must not send the rewrite to a path outside the snapshot.
repo="$test_root/newline_repo"
new_repo "$repo"
write_file "$repo/x" "nothing"
write_file "$repo/x
README.md" "$private_slug"
commit_all "$repo"
readme_before=$(cat "$repo/README.md")
snapshot "$repo" "$export_public_snapshot" "$test_root/out_newline"
expect_exit 2 "a snapshot with a newline in a file name"
expect_output_has "newline" "a snapshot with a newline in a file name"
[ "$(cat "$repo/README.md")" = "$readme_before" ] || fail "a snapshot never writes to the source checkout"

if [ "$failures" -eq 0 ]; then
  echo "public_snapshot.test.sh: all passed"
else
  echo "public_snapshot.test.sh: $failures failed"
  exit 1
fi
