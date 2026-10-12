#!/usr/bin/env bash
# CI-only source admission. Signed-tag and trusted-publisher retries retain
# their independent signature/archive checks in Simit's publisher engine.
set -eu
test "${RUNNER_ENVIRONMENT:-}" = github-hosted
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] || { [ "${GITHUB_EVENT_NAME:-}" = push ] && [ "${GITHUB_REF_TYPE:-}" != tag ]; }; then
  test "${GITHUB_RUN_ATTEMPT:-}" = 1
fi
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ]; then
  revision="$(jq -r '.pull_request.head.sha' "$GITHUB_EVENT_PATH")"
  git fetch origin "$revision"
  git checkout --detach "$revision"
  test "$(git rev-parse HEAD)" = "$revision"
fi
