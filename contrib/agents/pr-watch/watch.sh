#!/bin/sh
# Print the open PRs you authored, or nothing. Any source works as long as
# the output is stable for an unchanged state (the poll trigger dedupes on
# content). GitHub example:
gh pr list --author @me --state open \
  --json number,title,reviewDecision,mergeable,statusCheckRollup,updatedAt 2>/dev/null
