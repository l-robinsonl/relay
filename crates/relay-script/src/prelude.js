// Relay's test framework, injected before every script.
//
// Written in JavaScript rather than bound from Rust: the matchers are
// ordinary comparisons, and expressing them here keeps the Rust boundary
// down to data in, results out. A smaller boundary is a smaller sandbox
// escape surface.
//
// Everything it defines is captured into __relay at the end, which is the
// only thing the host reads back.

globalThis.__relay = {
  tests: [],
  // Bodies waiting to run, drained by __run.
  queue: [],
  finished: false,
  // Assertions made outside any test() block still have to be reported,
  // so they collect under a synthetic group.
  loose: [],
  currentTest: null,
};

function __record(passed, message) {
  const entry = { passed, message };
  if (globalThis.__relay.currentTest) {
    globalThis.__relay.currentTest.assertions.push(entry);
  } else {
    globalThis.__relay.loose.push(entry);
  }
  return entry;
}

function __describe(value) {
  if (value === undefined) return 'undefined';
  if (value === null) return 'null';
  if (typeof value === 'string') return JSON.stringify(value);
  if (typeof value === 'object') {
    try {
      const text = JSON.stringify(value);
      return text.length > 200 ? text.slice(0, 200) + '…' : text;
    } catch (e) {
      return String(value);
    }
  }
  return String(value);
}

function __deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return false;
  if (typeof a !== 'object') return false;

  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => __deepEqual(a[k], b[k]));
}

/** Starts an assertion chain. */
globalThis.expect = function expect(actual) {
  const assert = (passed, message) => {
    const entry = __record(passed, message);
    if (!passed) {
      // Throwing stops the enclosing test() at its first failure, which
      // is almost always what the author meant. test() catches it.
      const error = new Error(message);
      error.__relayAssertion = entry;
      throw error;
    }
  };

  const chain = {
    toBe(expected) {
      assert(
        actual === expected,
        `expected ${__describe(expected)} but got ${__describe(actual)}`,
      );
    },
    toEqual(expected) {
      assert(
        __deepEqual(actual, expected),
        `expected ${__describe(expected)} but got ${__describe(actual)}`,
      );
    },
    toBeDefined() {
      assert(actual !== undefined && actual !== null, 'expected a value, got nothing');
    },
    toBeUndefined() {
      assert(actual === undefined, `expected nothing, got ${__describe(actual)}`);
    },
    toBeNull() {
      assert(actual === null, `expected null, got ${__describe(actual)}`);
    },
    toBeTruthy() {
      assert(!!actual, `expected something truthy, got ${__describe(actual)}`);
    },
    toBeFalsy() {
      assert(!actual, `expected something falsy, got ${__describe(actual)}`);
    },
    toContain(needle) {
      const ok =
        typeof actual === 'string'
          ? actual.indexOf(needle) !== -1
          : Array.isArray(actual) && actual.indexOf(needle) !== -1;
      assert(ok, `expected ${__describe(actual)} to contain ${__describe(needle)}`);
    },
    toHaveLength(length) {
      const actualLength = actual == null ? undefined : actual.length;
      assert(
        actualLength === length,
        `expected length ${length} but got ${__describe(actualLength)}`,
      );
    },
    toBeGreaterThan(limit) {
      assert(actual > limit, `expected ${__describe(actual)} to be above ${limit}`);
    },
    toBeLessThan(limit) {
      assert(actual < limit, `expected ${__describe(actual)} to be below ${limit}`);
    },
    toMatch(pattern) {
      const regex = pattern instanceof RegExp ? pattern : new RegExp(pattern);
      assert(
        typeof actual === 'string' && regex.test(actual),
        `expected ${__describe(actual)} to match ${pattern}`,
      );
    },
    toHaveProperty(name) {
      assert(
        actual != null && Object.prototype.hasOwnProperty.call(actual, name),
        `expected an object with property ${__describe(name)}`,
      );
    },
  };

  // `.not` inverts every matcher without writing each one twice.
  chain.not = new Proxy(
    {},
    {
      get(_target, name) {
        return (...args) => {
          let threw = false;
          const before = globalThis.__relay.currentTest
            ? globalThis.__relay.currentTest.assertions.length
            : globalThis.__relay.loose.length;
          try {
            chain[name](...args);
          } catch (e) {
            threw = true;
          }
          // Discard whatever the inner matcher recorded; only the
          // inverted result belongs in the report.
          const list = globalThis.__relay.currentTest
            ? globalThis.__relay.currentTest.assertions
            : globalThis.__relay.loose;
          list.length = before;
          assert(threw, `expected not.${String(name)} to hold, but it did not`);
        };
      },
    },
  );

  return chain;
};

/**
 * Registers a group of assertions.
 *
 * The body is queued rather than run immediately, so that __run can
 * await it. Calling it inline would break any async body: the await
 * would return after test() had already restored currentTest, and every
 * assertion after the first await would be filed under no test at all —
 * reporting a pass for a test that had not finished running.
 */
globalThis.test = function test(name, body) {
  const entry = { name: String(name), assertions: [], error: null };
  globalThis.__relay.tests.push(entry);
  globalThis.__relay.queue.push({ entry, body });
};

globalThis.it = globalThis.test;

/**
 * Runs the queued tests, one after another.
 *
 * Awaiting each body keeps currentTest set for its whole duration,
 * including across awaits inside it. The host calls this and then drains
 * the job queue.
 */
globalThis.__relay.run = async function run() {
  while (globalThis.__relay.queue.length > 0) {
    const { entry, body } = globalThis.__relay.queue.shift();
    globalThis.__relay.currentTest = entry;
    try {
      await body();
    } catch (e) {
      // An assertion failure has already been recorded; anything else is
      // a fault in the script and belongs on the test.
      if (!e || !e.__relayAssertion) {
        entry.error = e && e.message ? String(e.message) : String(e);
      }
    } finally {
      globalThis.__relay.currentTest = null;
    }
  }
  globalThis.__relay.finished = true;
};
