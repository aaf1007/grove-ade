#!/bin/sh
# Fails when a banned name appears in a file, or in a file's path, under a directory.
# Usage: check_banned_names.sh <directory> [<names_file>]
#
# The names file holds one case-insensitive extended regex per line. Blank lines and lines that
# start with # are skipped. A line "allow: <regex>" names text that is cut from every line before
# the check. Use it for an exact pattern that has to stay, such as a dependency's own API. That
# regex is case-sensitive.
#
# The default names file is research/banned-names.txt in the main checkout of the repository the
# command runs in. It is never committed, so the names themselves stay out of the tree.
#
# Package lock files and binary files are skipped. A symlink is checked by its name and its target.
# Prints path:line:text for every hit.
# Exits 0 with no hit, 1 with a hit, 2 when it cannot run.

set -u
LC_ALL=C
export LC_ALL

# The Open-in-editor feature names the editors it opens. Only these files may hold a banned name.
allowed_paths='crates/grove-core/src/editor.rs
crates/grove-core/tests/editor.rs
app/src-tauri/src/main.rs
app/src/bridge.ts
app/src/DetailsCard.tsx
app/src/DetailsCard.test.tsx'

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
  echo "usage: check_banned_names.sh <directory> [<names_file>]" >&2
  exit 2
fi
directory=$1
[ -d "$directory" ] || { echo "check_banned_names: $directory is not a directory" >&2; exit 2; }

if [ $# -eq 2 ]; then
  names_file=$2
else
  git_common_folder=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || {
    echo "check_banned_names: not in a git repository, so there is no default names file. Pass one: check_banned_names.sh <directory> <names_file>" >&2
    exit 2
  }
  names_file="$(dirname "$git_common_folder")/research/banned-names.txt"
fi
[ -f "$names_file" ] || {
  echo "check_banned_names: names file $names_file not found. It is not committed. Write one regex per line there, or pass another names file." >&2
  exit 2
}

work_folder=$(mktemp -d "${TMPDIR:-/tmp}/grove-banned-names.XXXXXX") || exit 2
trap 'rm -rf "$work_folder"' EXIT
banned_patterns="$work_folder/banned_patterns"
allowed_patterns="$work_folder/allowed_patterns"
names="$work_folder/names"
checked_paths="$work_folder/checked_paths"
text_files="$work_folder/text_files"
allowed_text_removed="$work_folder/allowed_text_removed"
hits="$work_folder/hits"

cannot_run() {
  echo "check_banned_names: $1" >&2
  exit 2
}

# grep exits 0 on a match and 1 on none. Anything above that is an error, which must never read
# as "no hits".
grep_failed() {
  [ "$1" -gt 1 ]
}

# Trailing spaces, tabs and carriage returns are cut, so a names file with CRLF line ends works.
sed -E 's/[[:space:]]+$//' "$names_file" > "$names" || cannot_run "could not read $names_file"
grep -v -E '^[[:space:]]*(#|$)|^allow:' "$names" > "$banned_patterns"
grep_failed $? && cannot_run "could not read the patterns in $names_file"
sed -n -E 's/^allow:[[:space:]]*//p' "$names" > "$allowed_patterns"
[ -s "$banned_patterns" ] || cannot_run "names file $names_file holds no pattern"

# One invalid regex would make every grep and awk below fail, so both sets are tried once here.
# awk compiles a regex only when gsub runs, so it gets one empty line to work on.
remove_allowed_text='
  FILENAME == allowed_patterns_file { allowed[allowed_count++] = $0; next }
  { for (i = 0; i < allowed_count; i++) gsub(allowed[i], ""); print }
'
grep -i -E -f "$banned_patterns" /dev/null
grep_failed $? && cannot_run "a pattern in $names_file is not a valid regex"
echo | awk -v allowed_patterns_file="$allowed_patterns" "$remove_allowed_text" "$allowed_patterns" - > /dev/null \
  || cannot_run "an allow: pattern in $names_file is not a valid regex"

cd "$directory" || exit 2
: > "$hits"

# The lists below hold one path per line, so a newline in a name would split it into two paths.
paths_with_a_newline=$(find . -name '*
*' -not -path './.git/*') || cannot_run "could not list $directory"
[ -z "$paths_with_a_newline" ] || cannot_run "a file name under $directory holds a newline: $paths_with_a_newline"

find . \( -type f -o -type l \) -not -path './.git/*' > "$checked_paths" || cannot_run "could not list $directory"
sed 's|^\./||' "$checked_paths" > "$work_folder/relative_paths"
grep -i -E -f "$banned_patterns" "$work_folder/relative_paths" > "$work_folder/path_hits"
grep_failed $? && cannot_run "could not check the paths under $directory"
sed 's|$|: banned name in the path|' "$work_folder/path_hits" >> "$hits"

find . -type l -not -path './.git/*' > "$checked_paths" || cannot_run "could not list $directory"
while IFS= read -r link; do
  link_target=$(readlink "$link") || cannot_run "could not read the link $link"
  printf '%s\n' "$link_target" | grep -q -i -E -f "$banned_patterns"
  grep_status=$?
  grep_failed "$grep_status" && cannot_run "could not check the link $link"
  [ "$grep_status" -eq 0 ] && printf '%s: banned name in the link target: %s\n' "${link#./}" "$link_target" >> "$hits"
done < "$checked_paths"

# Every text file with a line in it. The patterns run after the allowed text is cut, because
# cutting it can join the two halves of a banned name.
grep -r -l -I -e '' --exclude-dir=.git \
  --exclude=Cargo.lock --exclude=package-lock.json --exclude='bun.lock*' . > "$text_files"
grep_failed $? && cannot_run "could not read every file under $directory"

# The path keeps its ./ so awk never reads it as an option, an assignment or standard input.
while IFS= read -r path; do
  printf '%s\n' "$allowed_paths" | grep -q -x -F -e "${path#./}" && continue
  awk -v allowed_patterns_file="$allowed_patterns" "$remove_allowed_text" "$allowed_patterns" "$path" > "$allowed_text_removed" \
    || cannot_run "could not read $path"
  grep -n -i -E -f "$banned_patterns" "$allowed_text_removed" > "$work_folder/file_hits"
  grep_failed $? && cannot_run "could not check $path"
  while IFS= read -r hit; do printf '%s:%s\n' "${path#./}" "$hit"; done < "$work_folder/file_hits" >> "$hits"
done < "$text_files"

if [ -s "$hits" ]; then
  cat "$hits"
  echo "check_banned_names: $(wc -l < "$hits" | tr -d ' ') banned name hits under $directory" >&2
  exit 1
fi
echo "check_banned_names: no banned name under $directory"
