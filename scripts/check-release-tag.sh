#!/usr/bin/env bash
# Refuse to attach assets to a tag naming a different commit.
set -euo pipefail
expected=${1:?Expected full commit SHA}
tag=${2:?Expected version tag}
remote=${3:-origin}
[[ "$expected" =~ ^[0-9a-f]{40}$ && "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
if refs=$(git ls-remote --exit-code --tags "$remote" "refs/tags/$tag" "refs/tags/$tag^{}"); then
    actual=$(printf '%s\n' "$refs" | awk '$2 ~ /\^\{\}$/ { annotated=$1 } $2 !~ /\^\{\}$/ { lightweight=$1 } END { print annotated ? annotated : lightweight }')
    test "$actual" = "$expected" || { echo 'Release tag points to a different commit' >&2; exit 1; }
else
    status=$?
    # git ls-remote reserves status 2 for a successful lookup with no matching ref.
    test "$status" -eq 2 || exit "$status"
fi
