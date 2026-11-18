#!/bin/sh
# Regression checks for block-merge.sh. Run: sh .claude/hooks/block-merge.test.sh
# Each case feeds the hook a tool call and checks its exit code: 2 blocks, 0 allows.

hook="$(dirname "$0")/block-merge.sh"
failures=0

expect() {
  wanted_exit=$1
  hook_input=$2
  printf '%s' "$hook_input" | sh "$hook" >/dev/null 2>&1
  actual_exit=$?
  if [ "$actual_exit" != "$wanted_exit" ]; then
    echo "FAIL: wanted $wanted_exit, got $actual_exit: $hook_input"
    failures=$((failures + 1))
  fi
}

bash_call() {
  jq -n --arg command "$1" '{tool_name: "Bash", tool_input: {command: $command}}'
}

blocks() { expect 2 "$(bash_call "$1")"; }
allows() { expect 0 "$(bash_call "$1")"; }

# gh pr merge, in every spelling.
blocks 'gh pr merge 12 --squash'
blocks 'cd x && gh  pr merge --auto 12'
blocks "gh pr 'merge' 12"
blocks 'gh pr "merge" 12'
blocks 'gh pr me"rge" 12'
blocks 'gh pr -R a/b merge 12'
blocks 'gh pr \
merge 12'

# The REST merge endpoint with a method other than GET, or with fields (a POST).
blocks 'gh api -X PUT repos/a/b/pulls/12/merge'
blocks 'gh api -XPUT repos/a/b/pulls/12/merge'
blocks 'gh api -X put repos/a/b/pulls/12/merge'
blocks 'gh api --method=PUT repos/a/b/pulls/12/merge'
blocks 'gh api "repos/a/b/pulls/12/"merge -X PUT'
blocks 'gh api repos/a/b/pulls/12/merge -f merge_method=squash'
blocks 'gh api repos/a/b/pulls/12/merge -fmerge_method=squash'
blocks 'gh api repos/a/b/pulls/12/merge -Fmerge_method=squash'
blocks 'gh api repos/a/b/pulls/12/merge --field=merge_method=squash'
blocks 'gh api repos/a/b/pulls/12/merge --raw-field merge_method=squash'
blocks 'gh api repos/a/b/pulls/12/merge --input body.json'
blocks 'gh api \
  -X PUT repos/a/b/pulls/12/merge'
blocks 'gh api -X GET repos/a/b/pulls/12/merge; gh api -X PUT repos/a/b/pulls/12/merge'

# Separators inside quotes, or escaped, do not split a call.
blocks "gh api repos/a/b/pulls/220/merge \\
  -H 'X-Audit-Note: QA & DevOps' -X PUT"
blocks 'gh api repos/a/b/pulls/220/merge -H "a; b" -X PUT'
blocks 'gh api repos/a/b/pulls/220/merge -H "a | b" -fmerge_method=squash'
blocks 'gh api repos/a/b/pulls/220/merge \
  -H "X-Audit-Note: QA \" & DevOps" -X PUT'
blocks 'gh api repos/a/b/pulls/220/merge -H "a \" ; b" -X PUT'
blocks "gh api repos/a/b/pulls/220/merge -H 'a \\' -X PUT"

# The GraphQL merge and auto-merge mutations, on one line or several.
blocks 'gh api graphql \
 -f query="mutation{enablePullRequestAutoMerge(input:{})}"'
blocks "gh api graphql -f query='mutation {
  mergePullRequest(input:{}) { clientMutationId }
}'"

# The app's auto-merge tool, and input the hook cannot read.
expect 2 '{"tool_name": "mcp__ccd_pr__set_auto_merge", "tool_input": {}}'
expect 2 'not json'
expect 2 ''
expect 2 '{}'
expect 2 '{"tool_name": "", "tool_input": {"command": "ls"}}'

# Allowed: a GET on the merge endpoint, separate calls, and other commands.
allows 'gh api repos/a/b/pulls/12/merge'
allows 'gh api -X GET repos/a/b/pulls/12/merge'
allows 'gh api -XGET repos/a/b/pulls/12/merge'
allows 'gh api --method=get repos/a/b/pulls/12/merge'
allows 'gh api --method GET repos/a/b/pulls/12/merge --jq .'
allows 'gh api -X GET -fx=1 repos/a/b/pulls/12/merge'
allows 'gh api repos/a/b/pulls/220/merge -H "a & b"'
allows 'gh api repos/a/b/pulls/220/merge
gh api -X POST repos/a/b/issues/220/comments -f body=Checked'
allows 'gh api repos/a/b/pulls/220/merge; gh api repos/a/b/issues/220/comments -f body=ok'
allows 'gh pr view 12; gh pr checks 12; git merge origin/main'
allows 'gh pr view 12 && git merge origin/main'
allows 'gh pr create --body-file body.md'
allows 'gh api repos/a/b/pulls/12/comments'
allows 'gh api repos/a/b/merges'
allows 'ls -la'
expect 0 '{"tool_name": "Read", "tool_input": {"file_path": "merge.md"}}'

# Without jq the hook cannot read any call, so it blocks every one.
no_jq_path=$(mktemp -d)
for tool in cat grep tr awk sed; do ln -s "$(command -v "$tool")" "$no_jq_path/$tool"; done
printf '%s' '{"tool_name": "Bash", "tool_input": {"command": "ls"}}' |
  PATH=$no_jq_path /bin/sh "$hook" >/dev/null 2>&1
if [ $? != 2 ]; then
  echo "FAIL: without jq, ls was not blocked"
  failures=$((failures + 1))
fi
rm -rf "$no_jq_path"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "all passed"
