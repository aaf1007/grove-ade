#!/bin/sh
# PreToolUse hook: only the owner merges (CLAUDE.local.md). Blocks merging a PR and turning on
# auto-merge, from Bash and from the desktop app's PR tool. Exit 2 blocks the call and shows
# stderr to Claude.
#
# It guards against mistakes, not against an agent set on getting around it. It errs toward
# blocking: text that only mentions a merge command is blocked too.

hook_input=$(cat)

refuse() {
  echo "Blocked: $1 Only the owner merges or turns on auto-merge in grove (CLAUDE.local.md). Ask the owner. If the command only mentions merging in text (a PR body or a comment), put that text in a file and pass it with --body-file." >&2
  exit 2
}

# Fail closed: without jq, with input jq cannot read, or with no tool name, the hook cannot
# tell what the call does, so it blocks it.
if ! tool_name=$(printf '%s' "$hook_input" | jq -r '.tool_name // empty' 2>/dev/null) ||
  ! command=$(printf '%s' "$hook_input" | jq -r '.tool_input.command // empty' 2>/dev/null) ||
  [ -z "$tool_name" ]; then
  echo "Blocked: the no-merge hook (.claude/hooks/block-merge.sh) could not read this call. Install jq (brew install jq), or ask the owner." >&2
  exit 2
fi

case "$tool_name" in
  mcp__ccd_pr__set_auto_merge) refuse "auto-merge through the app." ;;
  Bash) ;;
  *) exit 0 ;;
esac

# Join backslash-continued lines, then drop quotes and backslashes, so gh pr 'merge',
# gh pr "merge" and a call split over lines are all caught.
continued_command=$(printf '%s\n' "$command" | awk '{ if (sub(/\\$/, "")) printf "%s ", $0; else print }')
joined_command=$(printf '%s' "$continued_command" | tr -d "\"'\\\\")

# The command split into separate calls, one per line: on ; & | and newlines outside quotes,
# so a quoted & (say in a header) does not split a call. Quotes and backslashes are dropped
# afterwards.
separate_calls=$(printf '%s' "$continued_command" | awk 'BEGIN { RS = "\001" } {
  quote = ""; call = ""
  for (i = 1; i <= length($0); i++) {
    c = substr($0, i, 1)
    # A backslash escapes the next character, except inside single quotes.
    if (c == "\\" && quote != "\047") { call = call c substr($0, i + 1, 1); i++; continue }
    if (quote == "" && (c == "\"" || c == "\047")) quote = c
    else if (quote != "" && c == quote) quote = ""
    else if (quote == "" && (c == ";" || c == "&" || c == "|" || c == "\n")) { print call; call = ""; continue }
    call = call c
  }
  print call
}' | tr -d "\"'\\\\")

# gh pr merge, checked one command at a time so `gh pr view; git merge` is not a match.
if printf '%s\n' "$joined_command" | tr ';&|' '\n' |
  grep -Eq '(^|[^[:alnum:]_-])gh([[:space:]].*)?[[:space:]]pr[[:space:]]+(.*[[:space:]])?merge([[:space:]]|$)'; then
  refuse "gh pr merge."
fi
# gh api checks look at the whole command at once, because a GraphQL query can span lines.
one_line_command=$(printf '%s' "$joined_command" | tr '\n\t' '  ')

# The GraphQL merge and auto-merge mutations.
if printf '%s' "$one_line_command" |
  grep -Eq '(^|[^[:alnum:]_-])gh[[:space:]].*api.*(mergePullRequest|enablePullRequestAutoMerge)'; then
  refuse "merging through gh api."
fi
# The REST merge endpoint. A GET on it only checks whether the PR is merged, so allow that:
# no method flag and no fields (gh api's default GET), or an explicit GET. Anything else is
# blocked. Each call is checked on its own, so flags of another call in the same line do
# not count.
printf '%s\n' "$separate_calls" | {
  while IFS= read -r api_call; do
    printf '%s' "$api_call" |
      grep -Eq '(^|[^[:alnum:]_-])gh[[:space:]].*api.*/merge([^[:alnum:]_]|$)' || continue
    # Each method flag with its value, one per line, upper-cased.
    methods=$(printf '%s' "$api_call" |
      grep -Eo '(^|[[:space:]])(-X[[:space:]]*|--method[[:space:]=]+)[^[:space:]]*' |
      sed -E 's/^[[:space:]]*(-X[[:space:]]*|--method[[:space:]=]+)//' | tr '[:lower:]' '[:upper:]')
    if printf '%s\n' "$methods" | grep -Eqv '^(GET)?$'; then
      exit 2
    fi
    # Fields with no method flag make gh api send a POST. Short fields may be attached, as in
    # -fkey=value.
    if [ -z "$methods" ] &&
      printf '%s' "$api_call" | grep -Eq '(^|[[:space:]])(-[fF]|--field([[:space:]=]|$)|--raw-field([[:space:]=]|$)|--input([[:space:]=]|$))'; then
      exit 2
    fi
  done
  exit 0
} || refuse "merging through gh api."
exit 0
