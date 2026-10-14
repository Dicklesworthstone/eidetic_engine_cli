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
