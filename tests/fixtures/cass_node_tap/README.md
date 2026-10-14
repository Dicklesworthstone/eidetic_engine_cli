# Native Node TAP fixtures

Captured by executing Node.js v22.16.0 with:

```sh
node --test --test-reporter=tap /mnt/data/node-tap-probe/basic.test.cjs
```

The authored input was:

```js
const test = require('node:test');
const assert = require('node:assert/strict');
test('cache returns the saved revision', () => { assert.equal(1, 2); });
test('empty lookup returns no result', () => { assert.equal(null, null); });
```

`flat_failed.tap` is the exit-1 run. Changing only the first assertion's expected
value from `2` to `1`, then running the identical command, produced the exit-0
`flat_passed.tap`. Only whitespace on otherwise blank diagnostic lines was
removed from the captured output. Durations and sandbox source locations are
retained as observations, not canonical identities or diagnostic fingerprints.

These producer fixtures do not imply that the Rust consumer tests were run.

## Nested scopes and cancellation

`nested_failed.tap` and `nested_passed.tap` were captured with the same Node
version and `node --test --test-reporter=tap` command, using this input at
`/mnt/data/node-tap-probe/nested.test.cjs`:

```js
const { test, describe, it } = require('node:test');
const assert = require('node:assert/strict');
describe('ledger', () => {
  it('saves the revision', () => { assert.equal(1, 2); });
  it.skip('excluded test', () => {});
  it.todo('future behavior', () => {});
});
test('transaction', async t => {
  await t.test('retries', () => {});
});
```

Again only the assertion's expected `2` was changed to `1` for the green run.
The observed summaries contain five tests and one suite: a test with a nested
test counts separately from its child; a describe suite does not count as a
test. The failed run exits 1 and the repaired run exits 0. Blank-line diagnostic
whitespace is normalized as above.

`cancelled.tap` is an exit-1 native run of:

```js
const { test } = require('node:test');
test('pending', async () => { await new Promise(() => {}); });
```

Its non-ok result increments `cancelled`, not `fail`. It supplies a diagnostic,
but not a complete selected-test inventory that can establish a verified fix.

`timeout.tap` uses a test with `{ timeout: 5 }` that awaits a 20 ms timer;
`aborted.tap` uses an already-aborted `AbortController` signal. These were also
captured from exit-1 native Node runs. Their failure types are respectively
`testTimeoutFailure` and `testAborted`; both increment the cancelled counter
and must never certify an executed-test repair.
