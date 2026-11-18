#!/bin/sh
# Builds the tree for the public repository in an empty directory.
# Usage, from a checkout of grove: export_public_snapshot.sh <output_directory> [<git_ref>]
#
# The tree comes from `git archive` of the ref (default main), so an untracked or ignored file can
# never be in it. The private repository's slug is then rewritten to the public one, and
# check_banned_names.sh runs on the result. Prints the commit it exported, and the output path
# when the check passes. The default ref is the local main, which may be behind the remote.
# It makes no commit and pushes nothing.
# Exits 0 when the snapshot is clean, 1 when the name check fails, 2 when it cannot run.

set -u

# The slugs are built from parts, so the rewrite below leaves this file alone in the snapshot.
github_owner=aaf1007
private_repository_name=grove
public_repository_name=grove-ade

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
  echo "usage: export_public_snapshot.sh <output_directory> [<git_ref>]" >&2
  exit 2
fi
output_directory=$1
git_ref=${2:-main}
script_folder=$(cd "$(dirname "$0")" && pwd)

repository_root=$(git rev-parse --show-toplevel) || exit 2
commit=$(git rev-parse --verify --quiet "$git_ref^{commit}") || {
  echo "export_public_snapshot: unknown ref $git_ref" >&2
  exit 2
}

if [ -e "$output_directory" ]; then
  [ -d "$output_directory" ] || { echo "export_public_snapshot: $output_directory is not a directory" >&2; exit 2; }
  [ -z "$(ls -A "$output_directory")" ] || { echo "export_public_snapshot: $output_directory is not empty" >&2; exit 2; }
fi
mkdir -p "$output_directory" || exit 2
output_directory=$(cd "$output_directory" && pwd)

work_folder=$(mktemp -d "${TMPDIR:-/tmp}/grove-public-snapshot.XXXXXX") || exit 2
trap 'rm -rf "$work_folder"' EXIT

git -C "$repository_root" archive --format=tar --output="$work_folder/tree.tar" "$commit" || exit 2
tar -x -f "$work_folder/tree.tar" -C "$output_directory" || exit 2
echo "export_public_snapshot: exported $git_ref at commit $commit"

# The list below holds one path per line, so a newline in a name would split it into two paths,
# and the second could be a file outside the snapshot.
paths_with_a_newline=$(find "$output_directory" -name '*
*') || exit 2
[ -z "$paths_with_a_newline" ] || {
  echo "export_public_snapshot: a file name in $git_ref holds a newline: $paths_with_a_newline" >&2
  exit 2
}

# The slug starts after a character that cannot be part of an owner name and ends before one that
# cannot be part of a repository name. So another owner, a longer repository name and the public
# slug itself are left alone. The rule runs twice because a match takes the character after it,
# which hides a second slug that starts right there.
slug_rewrite="s#(^|[^A-Za-z0-9-])$github_owner/$private_repository_name([^A-Za-z0-9_-]|\$)#\\1$github_owner/$public_repository_name\\2#g"
LC_ALL=C grep -r -l -I -F -e "$github_owner/$private_repository_name" "$output_directory" > "$work_folder/files_with_the_slug"
[ $? -le 1 ] || { echo "export_public_snapshot: could not search $output_directory for the slug" >&2; exit 2; }
while IFS= read -r path; do
  LC_ALL=C sed -E -e "$slug_rewrite" -e "$slug_rewrite" "$path" > "$work_folder/rewritten" && cat "$work_folder/rewritten" > "$path" || {
    echo "export_public_snapshot: could not rewrite $path" >&2
    exit 2
  }
done < "$work_folder/files_with_the_slug"

sh "$script_folder/check_banned_names.sh" "$output_directory" || {
  check_exit=$?
  echo "export_public_snapshot: the name check failed. The tree is left in $output_directory" >&2
  exit "$check_exit"
}
echo "Public snapshot of $git_ref: $output_directory"
