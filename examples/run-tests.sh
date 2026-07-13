#!/usr/bin/env bash
set -euo pipefail

# Run Playwright E2E tests for impresspress examples.
# Each example uses .env for configuration.
#
# Usage:
#   ./run-tests.sh              # test all three examples
#   ./run-tests.sh dropship     # test only dropship
#   ./run-tests.sh saas blog    # test saas and blog

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PORT="${TEST_PORT:-8091}"
WAFER_RUN__AUTH__JWT_SECRET="${WAFER_RUN__AUTH__JWT_SECRET:-examples-test-secret-$(date +%s)}"

# Build impresspress (release, includes impresspress-web wasm bundling).
echo "==> Building impresspress..."
cd "$REPO_ROOT"
just build 2>&1 | tail -1
BINARY="$REPO_ROOT/target/release/impresspress"

if [ ! -f "$BINARY" ]; then
  echo "ERROR: Binary not found at $BINARY"
  exit 1
fi

# Install playwright if needed
cd "$SCRIPT_DIR"
if [ ! -d "node_modules" ]; then
  echo "==> Installing dependencies..."
  npm install --silent
fi

# Determine which examples to test
if [ $# -gt 0 ]; then
  EXAMPLES=("$@")
else
  EXAMPLES=(dropship saas blog)
fi

FAILED=0

for example in "${EXAMPLES[@]}"; do
  EXAMPLE_DIR="$SCRIPT_DIR/$example"
  if [ ! -d "$EXAMPLE_DIR" ]; then
    echo "ERROR: Example '$example' not found at $EXAMPLE_DIR"
    FAILED=1
    continue
  fi

  echo ""
  echo "============================================"
  echo "  Testing: $example"
  echo "============================================"

  # Clean up any previous data
  rm -rf "$EXAMPLE_DIR/data"

  # Copy frontend files to the web block's storage path
  if [ -d "$EXAMPLE_DIR/frontend/build" ]; then
    mkdir -p "$EXAMPLE_DIR/data/storage/wafer-run/web/site"
    cp -r "$EXAMPLE_DIR/frontend/build/"* "$EXAMPLE_DIR/data/storage/wafer-run/web/site/"
  fi

  # Start impresspress in the example directory. The example ships a static
  # landing page (copied into storage above), so declare it via
  # WAFER_RUN_SHARED__HAS_LANDING_PAGE so routing serves it at `/`.
  cd "$EXAMPLE_DIR"
  WAFER_RUN__AUTH__JWT_SECRET="$WAFER_RUN__AUTH__JWT_SECRET" IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET="test-webhook-secret" WAFER_RUN_SHARED__HAS_LANDING_PAGE=true "$BINARY" serve --run-migrations &
  SERVER_PID=$!

  # Wait for server to be ready
  echo "==> Waiting for server on port $PORT..."
  for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:$PORT/health" > /dev/null 2>&1; then
      echo "==> Server ready"
      break
    fi
    if [ $i -eq 30 ]; then
      echo "ERROR: Server failed to start for $example"
      kill $SERVER_PID 2>/dev/null || true
      FAILED=1
      continue 2
    fi
    sleep 1
  done

  # Run tests for this example
  cd "$SCRIPT_DIR"
  if TEST_PORT="$PORT" npx playwright test "tests/${example}.spec.ts" --reporter=list; then
    echo "==> $example: PASSED"
  else
    echo "==> $example: FAILED"
    FAILED=1
  fi

  # Stop server and clean up
  kill $SERVER_PID 2>/dev/null || true
  wait $SERVER_PID 2>/dev/null || true
  rm -rf "$EXAMPLE_DIR/data"
  sleep 1  # ensure port is released before next example
done

# After the native pass for each example, also exercise the sealed × web
# build for `blog` as a proof-of-concept of the unified CLI's --target web
# path. The web build emits a static `dist/` that the bundled impresspress-web
# wasm renders client-side. Other examples can opt in by adding a
# tests/<name>-web.spec.ts in this directory.
if [[ " ${EXAMPLES[*]} " == *" blog "* ]] && [ -d "$SCRIPT_DIR/blog" ]; then
  echo ""
  echo "============================================"
  echo "  Testing: blog (web target)"
  echo "============================================"
  cd "$SCRIPT_DIR/blog"
  "$BINARY" build --target web

  WEB_PORT="${TEST_WEB_PORT:-8090}"
  "$BINARY" serve --target web --port "$WEB_PORT" &
  WEB_PID=$!

  for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:$WEB_PORT/index.html" > /dev/null 2>&1; then
      break
    fi
    sleep 1
  done

  cd "$SCRIPT_DIR"
  if TEST_WEB_PORT="$WEB_PORT" npx playwright test tests/blog-web.spec.ts --reporter=list; then
    echo "==> blog (web): PASSED"
  else
    echo "==> blog (web): FAILED"
    FAILED=1
  fi

  kill $WEB_PID 2>/dev/null || true
  wait $WEB_PID 2>/dev/null || true
fi

echo ""
if [ $FAILED -eq 0 ]; then
  echo "All examples passed!"
else
  echo "Some examples failed."
  exit 1
fi
